//! The container entrypoint (Linux only): the shell script's job, in order, then one branch:
//! exec the app (the default) or spawn and supervise it (`supervise` / `wireguard`, spec 4).

use std::path::PathBuf;

use anyhow::{Context as _, Result, anyhow, bail};
use nix::unistd::{Gid, Uid, getuid, setgid, setgroups, setuid};

use crate::{heartbeat, mounts, ping, prepare, pyproject, supervisor, wireguard};

pub struct RunArgs {
  pub pyproject: PathBuf,
  pub app_root: PathBuf,
  pub mountinfo: PathBuf,
}

/// Steps 1–5 of the spec. Every check happens before the filesystem is touched. Returns the
/// exit code when supervising; the exec path only ever returns an error.
pub fn run(args: &RunArgs) -> Result<u8> {
  // 1. Root only: chown and the privilege drop need it (same rule as the old script).
  if !getuid().is_root() {
    bail!("entrypoint must run as root (uid 0); got uid {}", getuid());
  }
  let doc = pyproject::load(&args.pyproject)?;
  // 2. The launch command — resolved first so a misconfigured pyproject fails before any
  //    directory is created.
  let script = pyproject::launch_script(&doc)?;
  let entries = pyproject::required_persisted_dirs(&doc)?;
  // 3. Mount check.
  let mountinfo = std::fs::read_to_string(&args.mountinfo).with_context(|| format!("reading {}", args.mountinfo.display()))?;
  let mounts = mounts::parse_mountinfo(&mountinfo);
  let missing = mounts::unbacked(&mounts, &args.app_root, &entries);
  if !missing.is_empty() {
    bail!(
      "required_persisted_dirs not backed by a bind mount (start the container with its volume): {}",
      missing.join(", ")
    );
  }
  let wireguard = pyproject::wireguard(&doc)?;
  let supervise = wireguard || pyproject::supervise(&doc)?;
  // 3b. The tunnel, after the mount check (a missing volume must not wait out a handshake)
  //     and before `prepare` (every check before the filesystem is touched).
  let tunnel = if wireguard {
    let cfg = wireguard::Config::from_env(&|k| std::env::var(k).ok())?;
    let (poll, stale) = (cfg.poll_secs, cfg.stale_secs);
    Some((wireguard::Tunnel::start(cfg)?, poll, stale))
  } else {
    None
  };
  // 4. mkdir -p + recursive chown. A failure here brings a tunnel down again.
  if let Err(e) = prepare::prepare(&args.app_root, &entries, &mut prepare::chown_nonroot) {
    if let Some((t, _, _)) = &tunnel {
      t.down();
    }
    return Err(e);
  }
  // A tunnel heartbeat left by an earlier run must not read as a stale tunnel (spec 7).
  if !wireguard {
    let _ = std::fs::remove_file(heartbeat::logs_dir(&args.app_root).join(heartbeat::TUNNEL_FILE));
  }
  let exe = args.app_root.join(".venv").join("bin").join(&script);
  if supervise {
    let services = pyproject::services(&doc);
    let env = |k: &str| std::env::var(k).ok();
    let ping = ping::Ping::configure(
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
    let (tunnel, poll_secs) = match tunnel {
      Some((t, poll, stale)) => (Some((t, stale)), poll),
      None => (None, env("WG_POLL_SECS").and_then(|v| v.parse().ok()).unwrap_or(30)),
    };
    return supervisor::run(supervisor::Plan {
      exe,
      app_root: args.app_root.clone(),
      tunnel,
      poll_secs,
      ping,
    });
  }
  // 5. Drop privileges, then replace this process with the app. Order matters: once the
  //    uid is 999 the process may no longer change its groups, so groups and gid go first.
  setgroups(&[]).context("setgroups")?;
  setgid(Gid::from_raw(prepare::NONROOT)).context("setgid")?;
  setuid(Uid::from_raw(prepare::NONROOT)).context("setuid")?;
  // `exec` never returns on success: the current process image is replaced. The only
  // thing it can hand back is the error that stopped it.
  use std::os::unix::process::CommandExt as _;
  let err = std::process::Command::new(&exe).exec();
  Err(anyhow!(err)).with_context(|| format!("exec {}", exe.display()))
}
