//! The supervising entrypoint (spec 5): the app spawned as 999 with nothing kept, signals
//! forwarded, zombies reaped, and every `WG_POLL_SECS` the tunnel checked, the tunnel's
//! heartbeat written, every heartbeat file adjudicated and the ping sent. Root only when a
//! tunnel needs re-upping; otherwise it drops to 999 before spawning.

use std::path::PathBuf;

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

pub struct Plan {
  pub exe: PathBuf,
  pub app_root: PathBuf,
  /// The tunnel and its stale threshold, when the mode is on.
  #[cfg(unix)]
  pub tunnel: Option<(crate::wireguard::Tunnel, u64)>,
  pub poll_secs: u64,
  pub ping: Option<Ping>,
}

#[cfg(unix)]
pub use unix::run;

#[cfg(unix)]
mod unix {
  use std::io::Read as _;
  use std::os::fd::AsFd as _;
  use std::os::unix::net::UnixStream;
  use std::os::unix::process::CommandExt as _;
  use std::process::Command;
  use std::sync::Arc;
  use std::sync::atomic::{AtomicBool, Ordering};
  use std::thread::JoinHandle;
  use std::time::{Duration, Instant};

  use anyhow::{Context as _, Result};
  use nix::sys::signal::{Signal, kill};
  use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
  use nix::unistd::{Gid, Pid, Uid, getuid, setgid, setgroups, setuid};

  use super::{Pinger, Plan};
  use crate::heartbeat;
  use crate::ping::{self, Kind};
  use crate::prepare::NONROOT;
  use crate::wireguard::{Assessor, SECRET_VARS};

  fn drop_privileges() -> Result<()> {
    setgroups(&[]).context("setgroups")?;
    setgid(Gid::from_raw(NONROOT)).context("setgid")?;
    setuid(Uid::from_raw(NONROOT)).context("setuid")?;
    Ok(())
  }

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

  /// Supervise `plan.exe` until it exits; returns the code to exit with (signal death as
  /// 128+n). Never returns while the child runs.
  pub fn run(plan: Plan) -> Result<u8> {
    let root = getuid().is_root();
    let (tunnel, stale_secs) = match plan.tunnel {
      Some((t, s)) => (Some(t), s),
      None => (None, heartbeat::DEFAULT_MAX_AGE_SECS),
    };
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

    let mut cmd = Command::new(&plan.exe);
    for var in SECRET_VARS {
      cmd.env_remove(var);
    }
    if plan.ping.is_some() {
      cmd.env("DEVKIT_SUPERVISED_PING", "1");
    }
    if spawner_is_root {
      // SAFETY: only async-signal-safe syscalls between fork and exec.
      unsafe {
        cmd.pre_exec(|| drop_privileges().map_err(|e| std::io::Error::other(e.to_string())));
      }
    }
    let child = cmd.spawn().with_context(|| format!("spawning {}", plan.exe.display()))?;
    let child_pid = Pid::from_raw(child.id() as i32);
    eprintln!("devkit-container: supervising pid {child_pid}");

    let logs = heartbeat::logs_dir(&plan.app_root);
    let app_beat = logs.join(heartbeat::APP_FILE);
    let tunnel_beat = logs.join(heartbeat::TUNNEL_FILE);
    let agent = ping::agent();
    let mut assessor = Assessor::new(stale_secs);
    let mut pinger = Pinger::default();
    let mut inflight: Option<JoinHandle<()>> = None;
    let mut next_poll = Instant::now();
    let exit_code: u8;

    // Off the loop's thread: a slow host must not delay signal forwarding or reaping. One in
    // flight at a time, bounded by the agent's 10 s timeout; a skipped plain ping is one
    // missed beat, well inside the check's grace.
    let send = |inflight: &mut Option<JoinHandle<()>>, kind: Kind, body: &str| {
      let Some(p) = &plan.ping else { return };
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

    loop {
      // Forwarded signals.
      for (flag, sig) in [(&term, Signal::SIGTERM), (&int, Signal::SIGINT), (&hup, Signal::SIGHUP)] {
        if flag.swap(false, Ordering::SeqCst) {
          let _ = kill(child_pid, sig);
        }
      }
      // Reap: the app child ends the loop; anything else is an orphan adopted as PID 1.
      let mut app_exit: Option<u8> = None;
      loop {
        match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
          Ok(WaitStatus::Exited(pid, code)) if pid == child_pid => app_exit = Some(code as u8),
          Ok(WaitStatus::Signaled(pid, sig, _)) if pid == child_pid => app_exit = Some(128u8.wrapping_add(sig as i32 as u8)),
          Ok(WaitStatus::StillAlive) | Err(_) => break,
          Ok(_) => {}
        }
      }
      if let Some(code) = app_exit {
        exit_code = code;
        break;
      }
      if Instant::now() >= next_poll {
        next_poll = Instant::now() + Duration::from_secs(plan.poll_secs);
        let now = jiff::Timestamp::now();
        let mut reasons: Vec<String> = Vec::new();
        if let Some(t) = &tunnel {
          let handshake = match t.latest_handshake() {
            Ok(h) => h,
            Err(e) => {
              eprintln!("devkit-container: {e:#}");
              None
            }
          };
          let (fresh, action) = assessor.assess(now.as_second() as u64, handshake);
          if fresh {
            if let Err(e) = heartbeat::write(&tunnel_beat, now) {
              eprintln!("devkit-container: {e:#}");
            }
          } else if let Err(e) = t.act(action) {
            eprintln!("devkit-container: re-up failed: {e:#}");
          }
          if let Err(r) = heartbeat::check(&tunnel_beat, stale_secs, now) {
            reasons.push(r);
          }
        }
        if let Err(r) = heartbeat::check(&app_beat, heartbeat::DEFAULT_MAX_AGE_SECS, now) {
          reasons.push(r);
        }
        match pinger.decide(reasons.is_empty()) {
          Some(Kind::Fail) => {
            eprintln!("devkit-container: unhealthy: {}", reasons.join("; "));
            send(&mut inflight, Kind::Fail, &reasons.join("\n"));
          }
          Some(kind) => send(&mut inflight, kind, ""),
          None => {}
        }
      }
      let until_poll = next_poll.saturating_duration_since(Instant::now());
      waker.wait(until_poll.min(Duration::from_millis(250)));
    }

    if let Some(t) = &tunnel {
      t.down();
    }
    if exit_code != 0 {
      eprintln!("devkit-container: app exited with {exit_code}");
      send(&mut inflight, Kind::Fail, &format!("exit code {exit_code}"));
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
