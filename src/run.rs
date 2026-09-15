//! The container entrypoint (Linux only): the shell script's job in the order of spec 7, the
//! tunnel's boot (5.2 steps 1 to 5) before any file is touched, then one branch: exec the app
//! (the default) or spawn and supervise it (`supervise` / `wireguard`).

use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use nix::unistd::{Gid, Uid, getuid, setgid, setgroups, setuid};

use crate::bundle::{self, Effective};
use crate::fetch::{self, Fetcher, Hosts, Outcome};
use crate::health::Reason;
use crate::logfile::Log;
use crate::ping::{self, Kind, Ping};
use crate::supervisor::{Applied, Plan, TunnelPlan};
use crate::wireguard::{self, ApplyError, Interface, Mode, Settings};
use crate::{cache, consent, heartbeat, mounts, prepare, pyproject, supervisor};

pub struct RunArgs {
  pub pyproject: PathBuf,
  pub app_root: PathBuf,
  pub mountinfo: PathBuf,
}

/// Returns the exit code when supervising; the exec path only ever returns an error.
pub fn run(args: &RunArgs) -> Result<u8> {
  if !getuid().is_root() {
    bail!("entrypoint must run as root (uid 0); got uid {}", getuid());
  }
  let doc = pyproject::load(&args.pyproject)?;
  // Resolved before any directory is created, so a misconfigured pyproject fails first.
  let script = pyproject::launch_script(&doc)?;
  let startup = pyproject::startup_scripts(&doc)?;
  let scrub = pyproject::scrub_env(&doc)?;
  let entries = pyproject::required_persisted_dirs(&doc)?;
  let wireguard = pyproject::wireguard(&doc)?;
  let supervise = wireguard || pyproject::supervise(&doc)?;
  let mountinfo = std::fs::read_to_string(&args.mountinfo).with_context(|| format!("reading {}", args.mountinfo.display()))?;
  let missing = mounts::unbacked(&mounts::parse_mountinfo(&mountinfo), &args.app_root, &entries);
  if !missing.is_empty() {
    bail!(
      "required_persisted_dirs not backed by a bind mount (start the container with its volume): {}",
      missing.join(", ")
    );
  }
  let env = |k: &str| std::env::var(k).ok();
  // The ping before the tunnel, so a refused start can send /fail (5.2).
  let ping = if supervise {
    let services = pyproject::services(&doc);
    let ping = Ping::configure(
      env("ALERTS_HEALTHCHECK_PING_URL").as_deref(),
      env("PINGKEY").as_deref(),
      ping::slug(env("HEARTBEAT_SLUG").as_deref(), &services).as_deref(),
    );
    match (&ping, wireguard) {
      (None, true) => eprintln!(
        "devkit-container: no ping configured (PINGKEY and HEARTBEAT_SLUG, or ALERTS_HEALTHCHECK_PING_URL): the tunnel is visible to Docker but not to healthchecks.io"
      ),
      (None, false) => eprintln!("devkit-container: no ping configured; the app pings for itself"),
      _ => {}
    }
    ping
  } else {
    None
  };
  let log = Log::new(&args.app_root);
  // The tunnel, before `prepare`: a refused start touches no file.
  let tunnel = if wireguard {
    Some(boot_tunnel(&args.app_root, &log, ping.as_ref())?)
  } else {
    None
  };
  let down = |t: &Option<BootTunnel>| {
    if let Some(t) = t {
      t.plan.interface.down();
    }
  };
  // `prepare`, with the implicit folders of 4.4 and 5.7.
  let mut to_prepare = entries.clone();
  if supervise {
    to_prepare.push("persisted_data/logs".to_string());
  }
  if wireguard {
    to_prepare.push(cache::DIR.to_string());
  }
  if let Err(e) = prepare::prepare(&args.app_root, &to_prepare, &mut prepare::chown_nonroot) {
    down(&tunnel);
    return Err(e);
  }
  if supervise && let Err(e) = consent_dir() {
    down(&tunnel);
    return Err(e);
  }
  if let Some(text) = tunnel.as_ref().and_then(|t| t.cache_text.as_deref())
    && let Err(e) = cache::write(&args.app_root, text)
  {
    log.line(&format!("cache write failed: {e:#}"));
  }
  // A tunnel heartbeat left by an earlier run must not read as a stale tunnel.
  if !wireguard {
    let _ = std::fs::remove_file(heartbeat::logs_dir(&args.app_root).join(heartbeat::TUNNEL_FILE));
  }
  if let Err(e) = run_startup_scripts(&args.app_root, &startup) {
    down(&tunnel);
    return Err(e);
  }
  let exe = args.app_root.join(".venv").join("bin").join(&script);
  if supervise {
    let poll_secs = match &tunnel {
      Some(t) => t.plan.settings.poll_secs,
      None => env("WG_POLL_SECS").and_then(|v| v.parse().ok()).unwrap_or(30),
    };
    return supervisor::run(Plan {
      exe,
      app_root: args.app_root.clone(),
      poll_secs,
      ping,
      scrub,
      consent_socket: PathBuf::from(consent::SOCKET),
      log,
      tunnel: tunnel.map(|t| t.plan),
    });
  }
  // Drop privileges, then replace this process with the app. Order matters: once the uid is
  // 999 the process may no longer change its groups, so groups and gid go first.
  setgroups(&[]).context("setgroups")?;
  setgid(Gid::from_raw(prepare::NONROOT)).context("setgid")?;
  setuid(Uid::from_raw(prepare::NONROOT)).context("setuid")?;
  use std::os::unix::process::CommandExt as _;
  let mut cmd = std::process::Command::new(&exe);
  for var in wireguard::SECRET_VARS.iter().copied().chain(scrub.iter().map(String::as_str)) {
    cmd.env_remove(var);
  }
  // `exec` never returns on success; the only thing it can hand back is the error that stopped it.
  let err = cmd.exec();
  Err(anyhow!(err)).with_context(|| format!("exec {}", exe.display()))
}

/// `/run/devkit`, mode 0700, owned by nonroot: where a participating app opens the consent
/// socket (spec 6.2).
fn consent_dir() -> Result<()> {
  use std::os::unix::fs::PermissionsExt as _;
  std::fs::create_dir_all(consent::DIR).with_context(|| format!("creating {}", consent::DIR))?;
  std::fs::set_permissions(consent::DIR, std::fs::Permissions::from_mode(0o700)).with_context(|| format!("chmod {}", consent::DIR))?;
  prepare::chown_nonroot(Path::new(consent::DIR))
}

/// What the boot hands the supervisor, plus the bundle text to cache once `prepare` has made
/// the folder.
pub struct BootTunnel {
  pub plan: TunnelPlan,
  pub cache_text: Option<String>,
}

/// Spec 5.2 steps 1 to 5. `Err` is Broken or a refused start, `/fail` sent and the interface
/// down; `Ok` is Connected, or Disconnected under the tolerate switch.
fn boot_tunnel(app_root: &Path, log: &Log, ping: Option<&Ping>) -> Result<BootTunnel> {
  let settings = Settings::from_env(&|k| std::env::var(k).ok())?;
  let (preshared_key, fetcher) = match &settings.mode {
    Mode::Env { preshared_key, .. } => (preshared_key.clone(), None),
    Mode::Fetched { hub_url, repo, token } => (
      None,
      Some(Fetcher::new(hub_url.clone(), repo.clone(), token.clone(), Hosts::default())),
    ),
  };
  let alert = |body: &str| {
    if let Some(p) = ping
      && let Err(e) = ping::send(&ping::agent(), &p.url(Kind::Fail), body)
    {
      eprintln!("devkit-container: ping Fail failed: {e}");
    }
  };
  // 1. The interface and the key. Broken: exit 1, nothing left behind.
  let iface = match Interface::create(&settings.private_key, preshared_key) {
    Ok(i) => i,
    Err(e) => {
      alert(&format!("{e:#}"));
      return Err(e);
    }
  };
  log.line(&format!("wireguard public key {}", iface.public_key));
  // 2. The configuration.
  let mut cache_text = None;
  let mut failure: Option<(Reason, String)> = None;
  let obtained: Option<(Option<String>, Effective)> = match &settings.mode {
    Mode::Env { effective, .. } => Some((None, effective.clone())),
    Mode::Fetched { .. } => match fetch::obtain(fetcher.as_ref().expect("fetched mode"), &iface.public_key, None) {
      Outcome::New { tag, text, effective } => {
        log.line(&format!("fetched bundle {tag}"));
        cache_text = Some(text);
        Some((Some(tag), effective))
      }
      Outcome::NotEnrolled { tag, text } => {
        cache_text = Some(text);
        failure = Some((
          Reason::NotEnrolled,
          format!(
            "not enrolled in {tag}: no entry for {}; turn [tool.docker].wireguard off or enrol the key, then redeploy",
            iface.public_key
          ),
        ));
        None
      }
      Outcome::Unchanged(tag) => {
        failure = Some((Reason::ConfigUnavailable, format!("config unavailable: unexpected unchanged {tag}")));
        None
      }
      Outcome::Unavailable(msg) => {
        log.line(&format!("config unavailable: {msg}"));
        // The cache is the fallback (4.4); one without this spoke's entry counts as none.
        let cached = cache::read(app_root)
          .ok()
          .and_then(|text| bundle::parse(&text).ok())
          .and_then(|b| Some((b.hub_version.clone()?, bundle::select(&b, &iface.public_key)?)));
        match cached {
          Some((tag, effective)) => {
            log.line(&format!("using cached bundle {tag}"));
            Some((Some(tag), effective))
          }
          None => {
            failure = Some((
              Reason::ConfigUnavailable,
              "config unavailable: no bundle and no usable cache".to_string(),
            ));
            None
          }
        }
      }
    },
  };
  // 3 and 4. Apply, then the first handshake.
  let mut applied = None;
  let mut endpoint_ok = true;
  if let Some((tag, effective)) = obtained {
    match iface.apply(&effective) {
      Err(ApplyError::Local(e)) => {
        iface.down();
        alert(&format!("{e:#}"));
        return Err(e);
      }
      Err(ApplyError::Endpoint(e)) => {
        endpoint_ok = false;
        failure = Some((Reason::EndpointUnresolvable, format!("endpoint unresolvable: {e:#}")));
      }
      Ok(()) => {}
    }
    log.line(&format!(
      "applied {}: {}",
      tag.as_deref().unwrap_or("the environment"),
      effective.describe()
    ));
    if failure.is_none() {
      match iface.wait_handshake(&effective.hub_public_key, Duration::from_secs(settings.handshake_timeout_secs)) {
        Err(e) => {
          iface.down();
          alert(&format!("{e:#}"));
          return Err(e);
        }
        Ok(true) => log.line(&format!("wireguard handshake with {}", effective.endpoint)),
        Ok(false) => {
          failure = Some((
            Reason::NoHandshake,
            format!(
              "no wireguard handshake with {} within {} s",
              effective.endpoint, settings.handshake_timeout_secs
            ),
          ))
        }
      }
    }
    applied = Some(Applied { tag, effective });
  }
  // 5. The gate.
  let unconfigured = match (&failure, &applied) {
    (Some((reason, _)), None) => *reason,
    _ => Reason::ConfigUnavailable,
  };
  if let Some((_, msg)) = &failure {
    if !settings.tolerate {
      iface.down();
      log.line(&format!("refusing to start: {msg}"));
      alert(msg);
      bail!("{msg}");
    }
    log.line(&format!("WG_TOLERATE_DISCONNECTED=1: starting Disconnected: {msg}"));
    alert(msg);
  }
  Ok(BootTunnel {
    plan: TunnelPlan {
      interface: iface,
      settings,
      fetcher,
      applied,
      unconfigured,
      endpoint_ok,
    },
    cache_text,
  })
}

/// Run each startup script as root, in list order, one at a time, with the full environment,
/// working directory `app_root`, inherited stdio, no arguments, no timeout (spec 7). The first
/// nonzero exit is the error, naming the script and its code or signal.
pub fn run_startup_scripts(app_root: &Path, names: &[String]) -> Result<()> {
  for name in names {
    let exe = app_root.join(".venv").join("bin").join(name);
    let status = std::process::Command::new(&exe)
      .current_dir(app_root)
      .status()
      .with_context(|| format!("startup script {name}: running {}", exe.display()))?;
    if !status.success() {
      match status.code() {
        Some(code) => bail!("startup script {name} exited {code}"),
        None => bail!("startup script {name} was killed by signal {}", status.signal().unwrap_or(0)),
      }
    }
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use std::os::unix::fs::PermissionsExt as _;

  use super::*;

  fn script(root: &std::path::Path, name: &str, body: &str) {
    let dir = root.join(".venv").join("bin");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
  }

  #[test]
  fn startup_scripts_run_in_order_in_the_app_root_with_the_environment_and_stop_at_the_first_failure() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    script(
      &root,
      "first",
      "pwd >> log.txt; echo \"$CARGO_MANIFEST_DIR\" >> log.txt; echo first $# >> log.txt",
    );
    script(&root, "second", "echo second >> log.txt");
    run_startup_scripts(&root, &["first".into(), "second".into()]).unwrap();
    let log = std::fs::read_to_string(root.join("log.txt")).unwrap();
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines[0], root.to_string_lossy(), "working directory is the app root");
    assert_eq!(lines[1], env!("CARGO_MANIFEST_DIR"), "the full environment is inherited");
    assert_eq!(&lines[2..], ["first 0", "second"], "in order, no arguments");
    script(&root, "third", "exit 3");
    script(&root, "fourth", "echo fourth >> log.txt");
    let err = run_startup_scripts(&root, &["third".into(), "fourth".into()])
      .unwrap_err()
      .to_string();
    assert_eq!(err, "startup script third exited 3");
    assert!(
      !std::fs::read_to_string(root.join("log.txt")).unwrap().contains("fourth"),
      "stopped at the failure"
    );
    let err = run_startup_scripts(&root, &["missing".into()]).unwrap_err().to_string();
    assert!(err.contains("startup script missing"), "{err}");
  }
}
