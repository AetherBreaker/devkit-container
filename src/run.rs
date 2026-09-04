//! The container entrypoint (Linux only): the shell script's job, in order.

use std::path::PathBuf;

use anyhow::{Context as _, Result, anyhow, bail};
use nix::unistd::{Gid, Uid, getuid, setgid, setgroups, setuid};

use crate::{mounts, prepare, pyproject};

pub struct RunArgs {
  pub pyproject: PathBuf,
  pub app_root: PathBuf,
  pub mountinfo: PathBuf,
}

/// Steps 1–5 of the spec. Every check happens before the filesystem is touched.
pub fn run(args: &RunArgs) -> Result<()> {
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
  // 4. mkdir -p + recursive chown.
  prepare::prepare(&args.app_root, &entries, &mut prepare::chown_nonroot)?;
  // 5. Drop privileges, then replace this process with the app. Order matters: once the
  //    uid is 999 the process may no longer change its groups, so groups and gid go first.
  let exe = args.app_root.join(".venv").join("bin").join(&script);
  setgroups(&[]).context("setgroups")?;
  setgid(Gid::from_raw(prepare::NONROOT)).context("setgid")?;
  setuid(Uid::from_raw(prepare::NONROOT)).context("setuid")?;
  // `exec` never returns on success: the current process image is replaced. The only
  // thing it can hand back is the error that stopped it.
  use std::os::unix::process::CommandExt as _;
  let err = std::process::Command::new(&exe).exec();
  Err(anyhow!(err)).with_context(|| format!("exec {}", exe.display()))
}
