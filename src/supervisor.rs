//! The supervising entrypoint: the app spawned as 999 with nothing kept, signals forwarded,
//! zombies reaped, and every `WG_POLL_SECS` the tunnel assessed (spec 5.3), repaired (5.6) or
//! re-applied (5.5), its heartbeat written, every heartbeat file adjudicated and the ping sent.
//! Fetches and consent asks run on worker threads; the loop only ever waits on its waker.

use std::path::PathBuf;

use crate::bundle::Effective;
use crate::logfile::Log;
use crate::ping::{Kind, Ping};

/// When to ping (spec 7): `/start` once when every file is first fresh, plain on every fresh
/// poll after, `/fail` once on the transition to stale, plain again on recovery.
#[derive(Debug, Default)]
pub struct Pinger {
  started: bool,
  last_fresh: Option<bool>,
}

impl Pinger {
  pub fn decide(&mut self, all_fresh: bool) -> Option<Kind> {
    let was = self.last_fresh;
    self.last_fresh = Some(all_fresh);
    if all_fresh {
      if !self.started {
        self.started = true;
        return Some(Kind::Start);
      }
      return Some(Kind::Plain);
    }
    (was == Some(true)).then_some(Kind::Fail)
  }
}

/// The applied configuration and the tag it came from (`None` in environment mode).
#[derive(Debug, Clone)]
pub struct Applied {
  pub tag: Option<String>,
  pub effective: Effective,
}

pub struct Plan {
  pub exe: PathBuf,
  pub app_root: PathBuf,
  pub poll_secs: u64,
  pub ping: Option<Ping>,
  /// `[tool.docker].scrub_env`, on top of the built-in secrets.
  pub scrub: Vec<String>,
  pub consent_socket: PathBuf,
  pub log: Log,
  #[cfg(unix)]
  pub tunnel: Option<TunnelPlan>,
}

/// The tunnel as the boot left it (spec 5.2), handed to the loop.
#[cfg(unix)]
pub struct TunnelPlan {
  pub interface: crate::wireguard::Interface,
  pub settings: crate::wireguard::Settings,
  pub fetcher: Option<crate::fetch::Fetcher>,
  pub applied: Option<Applied>,
  /// The reason while `applied` is `None`, which only the tolerate switch allows.
  pub unconfigured: crate::health::Reason,
  pub endpoint_ok: bool,
}

#[cfg(unix)]
pub use unix::run;

#[cfg(unix)]
pub mod unix {
  use std::io::Read as _;
  use std::os::fd::AsFd as _;
  use std::os::unix::net::UnixStream;
  use std::os::unix::process::CommandExt as _;
  use std::path::Path;
  use std::process::Command;
  use std::sync::Arc;
  use std::sync::atomic::{AtomicBool, Ordering};
  use std::thread::JoinHandle;
  use std::time::{Duration, Instant};

  use anyhow::{Context as _, Result};
  use nix::sys::signal::{Signal, kill};
  use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
  use nix::unistd::{Gid, Pid, Uid, getuid, setgid, setgroups, setuid};

  use super::{Applied, Pinger, Plan, TunnelPlan};
  use crate::fetch::{self, Outcome};
  use crate::health::{Health, Interface, Reason, Repair, Reply, State};
  use crate::logfile::Log;
  use crate::ping::{self, Kind};
  use crate::prepare::NONROOT;
  use crate::wireguard::{ApplyError, SECRET_VARS};
  use crate::{cache, consent, heartbeat};

  /// The loop's wake-up (spec 5.2 step 7): a socket pair the signal handlers write a byte to,
  /// so a signal or the child's exit (SIGCHLD) ends the wait at once instead of at the next
  /// 250 ms tick. The flags and `waitpid` still carry the facts; this only ends the sleep.
  pub struct Waker {
    rx: UnixStream,
    /// The handlers hold their own clones; this end serves the test's `poke`.
    #[cfg(test)]
    tx: UnixStream,
  }

  impl Waker {
    pub fn new() -> Result<Waker> {
      let (rx, tx) = UnixStream::pair().context("creating the wake-up socket pair")?;
      rx.set_nonblocking(true).context("wake-up socket")?;
      tx.set_nonblocking(true).context("wake-up socket")?;
      for sig in [
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGHUP,
        signal_hook::consts::SIGCHLD,
      ] {
        signal_hook::low_level::pipe::register(sig, tx.try_clone().context("wake-up socket")?)
          .context("installing the wake-up handler")?;
      }
      #[cfg(not(test))]
      drop(tx);
      Ok(Waker {
        rx,
        #[cfg(test)]
        tx,
      })
    }

    /// Wait until a byte arrives or `max` passes, then drain every byte that arrived.
    pub fn wait(&self, max: Duration) {
      use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
      let mut fds = [PollFd::new(self.rx.as_fd(), PollFlags::POLLIN)];
      let timeout = PollTimeout::try_from(max).unwrap_or(PollTimeout::MAX);
      let _ = poll(&mut fds, timeout);
      let mut buf = [0u8; 64];
      while (&self.rx).read(&mut buf).is_ok_and(|n| n > 0) {}
    }

    /// What a signal handler does, for the test.
    #[cfg(test)]
    pub fn poke(&self) {
      use std::io::Write as _;
      let _ = (&self.tx).write_all(&[1]);
    }
  }

  fn drop_privileges() -> Result<()> {
    setgroups(&[]).context("setgroups")?;
    setgid(Gid::from_raw(NONROOT)).context("setgid")?;
    setuid(Uid::from_raw(NONROOT)).context("setuid")?;
    Ok(())
  }

  /// Why the loop ends the run before the app did, and how the app is stopped first.
  enum Exit {
    /// A local command failed (5.3): SIGTERM, then down, /fail, exit 1.
    Broken(anyhow::Error),
    /// The consent path ran out (6.3): SIGINT, then down, /fail, exit 75.
    GiveUp { elapsed: u64, reason: Reason, hold_limit: bool },
    /// The hub's newest bundle dropped this spoke (5.5): /fail, SIGINT, down, exit 75.
    Removed(String),
  }

  /// The tunnel side of the loop: the health machine, the worker threads and their results.
  struct TunnelState {
    plan: TunnelPlan,
    health: Health,
    started: Instant,
    fetch_job: Option<JoinHandle<Outcome>>,
    ask_job: Option<JoinHandle<Reply>>,
    reply: Option<Reply>,
    next_version_check: Instant,
    version_check_failing: bool,
    last_obtain_failure: Option<String>,
  }

  impl TunnelState {
    fn new(plan: TunnelPlan, started: Instant) -> TunnelState {
      let s = &plan.settings;
      TunnelState {
        health: Health::new(s.tolerate, plan.fetcher.is_some(), s.limit_secs, s.hold_limit_secs),
        next_version_check: started + Duration::from_secs(s.version_poll_secs),
        plan,
        started,
        fetch_job: None,
        ask_job: None,
        reply: None,
        version_check_failing: false,
        last_obtain_failure: None,
      }
    }

    fn observation(&self) -> Interface {
      match self.plan.applied {
        None => Interface::Unconfigured(self.plan.unconfigured),
        Some(_) => Interface::Configured {
          endpoint_ok: self.plan.endpoint_ok,
        },
      }
    }

    /// One version → bundle → entry attempt on a thread, if none is in flight.
    fn start_fetch(&mut self) {
      let (Some(f), None) = (&self.plan.fetcher, &self.fetch_job) else {
        return;
      };
      let f = f.clone();
      let public_key = self.plan.interface.public_key.clone();
      let tag = self.plan.applied.as_ref().and_then(|a| a.tag.clone());
      self.fetch_job = Some(std::thread::spawn(move || fetch::obtain(&f, &public_key, tag.as_deref())));
    }

    /// Take the results of finished threads. Runs before every poll, so a result is acted on at
    /// the first poll after it arrived (5.2 step 7).
    fn collect(&mut self, log: &Log, app_root: &Path) -> Result<(), Exit> {
      if self.fetch_job.as_ref().is_some_and(|h| h.is_finished())
        && let Some(h) = self.fetch_job.take()
      {
        match h.join() {
          Ok(outcome) => self.take_outcome(outcome, log, app_root)?,
          Err(_) => log.line("the fetch thread panicked; the next poll retries"),
        }
      }
      if self.ask_job.as_ref().is_some_and(|h| h.is_finished())
        && let Some(h) = self.ask_job.take()
      {
        let reply = h.join().unwrap_or(Reply::Ok);
        log.line(match reply {
          Reply::Hold => "the app replied hold; postponing",
          Reply::Ok => "the app replied ok, or did not reply",
        });
        self.reply = Some(reply);
      }
      Ok(())
    }

    fn take_outcome(&mut self, outcome: Outcome, log: &Log, app_root: &Path) -> Result<(), Exit> {
      match outcome {
        Outcome::Unchanged(_) => self.version_check_ok(log),
        Outcome::Unavailable(msg) => {
          if self.plan.applied.is_none() {
            if self.last_obtain_failure.as_deref() != Some(msg.as_str()) {
              log.line(&format!("config unavailable: {msg}"));
              self.last_obtain_failure = Some(msg);
            }
            self.plan.unconfigured = Reason::ConfigUnavailable;
          } else if !self.version_check_failing {
            // Once per change of outcome (5.5), never per attempt.
            self.version_check_failing = true;
            log.line(&format!("version check failing: {msg}"));
          }
        }
        Outcome::New { tag, text, effective } => {
          self.version_check_ok(log);
          if let Err(e) = cache::write(app_root, &text) {
            log.line(&format!("cache write failed: {e:#}"));
          }
          match self.plan.applied.clone() {
            None => {
              let r = self.plan.interface.apply(&effective);
              self.classify(r, log)?;
              log.line(&format!("applied {tag}: {}", effective.describe()));
              self.plan.applied = Some(Applied { tag: Some(tag), effective });
              self.health.config_applied();
            }
            Some(old) if old.effective == effective => {
              log.line(&format!("applied {tag}: no change for this spoke"));
              self.plan.applied = Some(Applied { tag: Some(tag), effective });
            }
            Some(old) => {
              let r = self.plan.interface.reapply(&old.effective, &effective, self.plan.endpoint_ok);
              self.classify(r, log)?;
              log.line(&format!("re-applied {tag} in place: {}", old.effective.diff(&effective)));
              self.plan.applied = Some(Applied { tag: Some(tag), effective });
              self.health.config_applied();
            }
          }
        }
        Outcome::NotEnrolled { tag, text } => {
          self.version_check_ok(log);
          if let Err(e) = cache::write(app_root, &text) {
            log.line(&format!("cache write failed: {e:#}"));
          }
          if self.plan.applied.is_some() {
            return Err(Exit::Removed(tag));
          }
          let msg = format!("not enrolled in {tag}: no entry for {}", self.plan.interface.public_key);
          if self.last_obtain_failure.as_deref() != Some(msg.as_str()) {
            log.line(&msg);
            self.last_obtain_failure = Some(msg);
          }
          self.plan.unconfigured = Reason::NotEnrolled;
        }
      }
      Ok(())
    }

    fn version_check_ok(&mut self, log: &Log) {
      if self.version_check_failing {
        self.version_check_failing = false;
        log.line("version check succeeding again");
      }
    }

    /// Spec 5.3: a local failure is Broken; the endpoint failing is Disconnected.
    fn classify(&mut self, r: Result<(), ApplyError>, log: &Log) -> Result<(), Exit> {
      match r {
        Ok(()) => {
          self.plan.endpoint_ok = true;
          Ok(())
        }
        Err(ApplyError::Endpoint(e)) => {
          self.plan.endpoint_ok = false;
          log.line(&format!("endpoint unresolvable: {e:#}"));
          Ok(())
        }
        Err(ApplyError::Local(e)) => Err(Exit::Broken(e)),
      }
    }

    /// The tunnel's share of one poll: assess, write the heartbeat, repair, check the version,
    /// ask, or give up.
    fn poll(&mut self, log: &Log, socket: &Path, tunnel_beat: &Path) -> Result<(), Exit> {
      let now = jiff::Timestamp::now();
      let handshake = match &self.plan.applied {
        None => None,
        Some(a) => self
          .plan
          .interface
          .latest_handshake(&a.effective.hub_public_key)
          .map_err(Exit::Broken)?,
      };
      let fresh = handshake.is_some_and(|h| (now.as_second() as u64).saturating_sub(h) < self.plan.settings.stale_secs);
      let d = self
        .health
        .poll(self.started.elapsed().as_secs(), self.observation(), fresh, self.reply.take());
      if d.changed {
        log.line(&format!("wireguard {}", d.state.describe()));
      }
      if fresh && let Err(e) = heartbeat::write(tunnel_beat, now) {
        log.line(&format!("{e:#}"));
      }
      match (d.repair, self.plan.applied.clone()) {
        (Repair::Obtain, _) => self.start_fetch(),
        (Repair::ResetEndpoint, Some(a)) => {
          log.line(&format!("wireguard re-up: resetting the endpoint {}", a.effective.endpoint));
          let r = self.plan.interface.set_endpoint(&a.effective);
          self.classify(r, log)?;
        }
        (Repair::DownUp, Some(a)) => {
          log.line("wireguard re-up: wg0 down and up");
          let r = self.plan.interface.down_up(&a.effective);
          self.classify(r, log)?;
        }
        _ => {}
      }
      if d.check_version {
        self.start_fetch();
      }
      if d.state == State::Connected && self.plan.fetcher.is_some() && Instant::now() >= self.next_version_check {
        self.next_version_check = Instant::now() + Duration::from_secs(self.plan.settings.version_poll_secs);
        self.start_fetch();
      }
      if let Some(elapsed) = d.ask
        && self.ask_job.is_none()
      {
        let path = socket.to_path_buf();
        let reason = format!("wireguard-disconnected {elapsed}s");
        log.line(&format!("shutdown pending: asking the app ({reason})"));
        self.ask_job = Some(std::thread::spawn(move || consent::ask(&path, &reason, consent::TIMEOUT)));
      }
      if let Some(g) = d.give_up {
        let State::Disconnected(reason) = d.state else {
          unreachable!("a give-up is Disconnected")
        };
        return Err(Exit::GiveUp {
          elapsed: g.elapsed,
          reason,
          hold_limit: g.hold_limit,
        });
      }
      Ok(())
    }
  }

  /// Reap every finished child (orphans adopted as PID 1 included); the app's exit code once
  /// it has ended (signal death as 128+n).
  fn reap(child: Pid) -> Option<u8> {
    let mut app_exit = None;
    loop {
      match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
        Ok(WaitStatus::Exited(pid, code)) if pid == child => app_exit = Some(code as u8),
        Ok(WaitStatus::Signaled(pid, sig, _)) if pid == child => app_exit = Some(128u8.wrapping_add(sig as i32 as u8)),
        Ok(WaitStatus::StillAlive) | Err(_) => break,
        Ok(_) => {}
      }
    }
    app_exit
  }

  /// Signal the app and give it 30 s to exit before SIGKILL (5.3, 5.5, 6.3); reaped either way.
  fn stop_app(child: Pid, sig: Signal, waker: &Waker) {
    let _ = kill(child, sig);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
      if reap(child).is_some() {
        return;
      }
      if Instant::now() >= deadline {
        let _ = kill(child, Signal::SIGKILL);
        let _ = waitpid(child, None);
        return;
      }
      waker.wait(Duration::from_millis(250).min(deadline.saturating_duration_since(Instant::now())));
    }
  }

  /// Supervise `plan.exe` until it exits; returns the code to exit with. Never returns while
  /// the child runs, except through the three shutdown paths, which stop it first.
  pub fn run(plan: Plan) -> Result<u8> {
    let Plan {
      exe,
      app_root,
      poll_secs,
      ping,
      scrub,
      consent_socket,
      log,
      tunnel,
    } = plan;
    let root = getuid().is_root();
    // Without a tunnel nothing later needs root: drop now, and the child inherits 999.
    if tunnel.is_none() && root {
      drop_privileges()?;
    }
    let spawner_is_root = tunnel.is_some() && root;

    let term = Arc::new(AtomicBool::new(false));
    let int = Arc::new(AtomicBool::new(false));
    let hup = Arc::new(AtomicBool::new(false));
    for (sig, flag) in [
      (signal_hook::consts::SIGTERM, &term),
      (signal_hook::consts::SIGINT, &int),
      (signal_hook::consts::SIGHUP, &hup),
    ] {
      signal_hook::flag::register(sig, Arc::clone(flag)).context("installing signal handler")?;
    }
    let waker = Waker::new()?;

    let mut cmd = Command::new(&exe);
    for var in SECRET_VARS.iter().copied().chain(scrub.iter().map(String::as_str)) {
      cmd.env_remove(var);
    }
    if ping.is_some() {
      cmd.env("DEVKIT_SUPERVISED_PING", "1");
    }
    cmd.env("DEVKIT_CONSENT_SOCKET", &consent_socket);
    if spawner_is_root {
      // SAFETY: only async-signal-safe syscalls between fork and exec.
      unsafe {
        cmd.pre_exec(|| drop_privileges().map_err(|e| std::io::Error::other(e.to_string())));
      }
    }
    let child = cmd.spawn().with_context(|| format!("spawning {}", exe.display()))?;
    let child_pid = Pid::from_raw(child.id() as i32);
    log.line(&format!("supervising pid {child_pid}"));

    let logs = heartbeat::logs_dir(&app_root);
    let app_beat = logs.join(heartbeat::APP_FILE);
    let tunnel_beat = logs.join(heartbeat::TUNNEL_FILE);
    let agent = ping::agent();
    let mut pinger = Pinger::default();
    let mut inflight: Option<JoinHandle<()>> = None;
    let mut next_poll = Instant::now();
    let mut tunnel = tunnel.map(|t| TunnelState::new(t, Instant::now()));
    let exit_code: u8;

    // Off the loop's thread: a slow host must not delay signal forwarding or reaping. One in
    // flight at a time, bounded by the agent's 10 s timeout.
    let send = |inflight: &mut Option<JoinHandle<()>>, kind: Kind, body: &str| {
      let Some(p) = &ping else { return };
      if inflight.as_ref().is_some_and(|h| !h.is_finished()) {
        eprintln!("devkit-container: ping {kind:?} skipped: the previous one is still in flight");
        return;
      }
      let (agent, url, body) = (agent.clone(), p.url(kind), body.to_string());
      *inflight = Some(std::thread::spawn(move || {
        if let Err(e) = ping::send(&agent, &url, &body) {
          eprintln!("devkit-container: ping {kind:?} failed: {e}");
        }
      }));
    };
    // The exit paths wait for the in-flight ping, then send theirs synchronously.
    let send_now = |inflight: &mut Option<JoinHandle<()>>, body: &str| {
      if let Some(h) = inflight.take() {
        let _ = h.join();
      }
      if let Some(p) = &ping
        && let Err(e) = ping::send(&agent, &p.url(Kind::Fail), body)
      {
        eprintln!("devkit-container: ping Fail failed: {e}");
      }
    };

    loop {
      for (flag, sig) in [(&term, Signal::SIGTERM), (&int, Signal::SIGINT), (&hup, Signal::SIGHUP)] {
        if flag.swap(false, Ordering::SeqCst) {
          let _ = kill(child_pid, sig);
        }
      }
      if let Some(code) = reap(child_pid) {
        exit_code = code;
        break;
      }
      let mut exit: Option<Exit> = None;
      if let Some(ts) = &mut tunnel
        && let Err(e) = ts.collect(&log, &app_root)
      {
        exit = Some(e);
      }
      if exit.is_none() && Instant::now() >= next_poll {
        next_poll = Instant::now() + Duration::from_secs(poll_secs);
        let now = jiff::Timestamp::now();
        let mut reasons: Vec<String> = Vec::new();
        if let Some(ts) = &mut tunnel {
          if let Err(e) = ts.poll(&log, &consent_socket, &tunnel_beat) {
            exit = Some(e);
          }
          if let Err(r) = heartbeat::check(&tunnel_beat, ts.plan.settings.stale_secs, now) {
            reasons.push(r);
          }
        }
        if exit.is_none() {
          if let Err(r) = heartbeat::check(&app_beat, heartbeat::DEFAULT_MAX_AGE_SECS, now) {
            reasons.push(r);
          }
          match pinger.decide(reasons.is_empty()) {
            Some(Kind::Fail) => {
              log.line(&format!("unhealthy: {}", reasons.join("; ")));
              send(&mut inflight, Kind::Fail, &reasons.join("\n"));
            }
            Some(kind) => send(&mut inflight, kind, ""),
            None => {}
          }
        }
      }
      if let Some(exit) = exit {
        let ts = tunnel.as_ref().expect("an exit comes from the tunnel");
        match exit {
          Exit::Broken(err) => {
            log.line(&format!("broken: {err:#}; stopping the app"));
            stop_app(child_pid, Signal::SIGTERM, &waker);
            ts.plan.interface.down();
            send_now(&mut inflight, &format!("{err:#}"));
            return Err(err);
          }
          Exit::GiveUp {
            elapsed,
            reason,
            hold_limit,
          } => {
            let msg = format!("gave up after {elapsed} s: {}", reason.as_str());
            if hold_limit {
              log.line("WG_HOLD_LIMIT_SECS reached; proceeding without asking again");
            }
            log.line(&format!("{msg}; SIGINT to the app"));
            stop_app(child_pid, Signal::SIGINT, &waker);
            ts.plan.interface.down();
            send_now(&mut inflight, &msg);
            return Ok(75);
          }
          Exit::Removed(tag) => {
            let msg = format!("removed from the hub's peer table in {tag}");
            log.line(&format!("{msg}; SIGINT to the app"));
            send_now(&mut inflight, &msg);
            stop_app(child_pid, Signal::SIGINT, &waker);
            ts.plan.interface.down();
            return Ok(75);
          }
        }
      }
      let until_poll = next_poll.saturating_duration_since(Instant::now());
      waker.wait(until_poll.min(Duration::from_millis(250)));
    }

    if let Some(ts) = &tunnel {
      ts.plan.interface.down();
    }
    if exit_code != 0 {
      log.line(&format!("app exited with {exit_code}"));
      // Synchronous like the other exit paths: `send` would skip it behind an in-flight ping.
      send_now(&mut inflight, &format!("exit code {exit_code}"));
    }
    // The container ends with this process; give the last ping its chance to leave.
    if let Some(h) = inflight {
      let _ = h.join();
    }
    Ok(exit_code)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::ping::Kind;

  #[test]
  fn start_once_plain_while_fresh_fail_on_the_transition_then_plain_again() {
    let mut p = Pinger::default();
    assert_eq!(p.decide(false), None, "silent until first fresh");
    assert_eq!(p.decide(false), None);
    assert_eq!(p.decide(true), Some(Kind::Start));
    assert_eq!(p.decide(true), Some(Kind::Plain));
    assert_eq!(p.decide(false), Some(Kind::Fail));
    assert_eq!(p.decide(false), None, "one /fail per outage");
    assert_eq!(p.decide(true), Some(Kind::Plain), "no second /start");
  }

  #[cfg(unix)]
  #[test]
  fn the_waker_returns_at_once_on_a_write_and_after_the_timeout_otherwise() {
    use std::time::{Duration, Instant};
    let waker = super::unix::Waker::new().unwrap();
    // Other tests spawn processes, and their SIGCHLD wakes every waker in this process, so a
    // full timeout is asserted as "one of a few waits lasted it", not "the first did".
    let full_timeout = |waker: &super::unix::Waker| {
      (0..20).any(|_| {
        let start = Instant::now();
        waker.wait(Duration::from_millis(200));
        start.elapsed() >= Duration::from_millis(150)
      })
    };
    assert!(full_timeout(&waker), "a wait without a wake-up lasts the timeout");
    waker.poke();
    let start = Instant::now();
    waker.wait(Duration::from_secs(5));
    assert!(start.elapsed() < Duration::from_secs(1), "woke at once");
    assert!(full_timeout(&waker), "the poke was drained");
  }
}
