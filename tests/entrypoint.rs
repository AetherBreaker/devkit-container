//! The binary end to end. `run` is Linux-only and needs root for the chown and the
//! privilege drop, so that part is `#[ignore]`; the query subcommands run everywhere.

use std::path::Path;
use std::process::Command;

fn bin() -> Command {
  Command::new(env!("CARGO_BIN_EXE_devkit-container"))
}

fn write(root: &Path, rel: &str, content: &str) {
  let p = root.join(rel);
  std::fs::create_dir_all(p.parent().unwrap()).unwrap();
  std::fs::write(p, content).unwrap();
}

#[test]
fn query_subcommands_print_without_trailing_newline() {
  let dir = tempfile::tempdir().unwrap();
  write(
    dir.path(),
    "pyproject.toml",
    "[project]\nreadme = \"README.md\"\n[project.optional-dependencies]\napp = []\n",
  );
  let py = dir.path().join("pyproject.toml");
  let out = bin().args(["app-extra", "--pyproject"]).arg(&py).output().unwrap();
  assert_eq!(String::from_utf8_lossy(&out.stdout), "--extra app");
  let out = bin().args(["readme", "--pyproject"]).arg(&py).output().unwrap();
  assert_eq!(String::from_utf8_lossy(&out.stdout), "README.md");
}

#[cfg(not(unix))]
#[test]
fn run_is_unsupported_off_unix() {
  let out = bin().arg("run").output().unwrap();
  assert_eq!(out.status.code(), Some(1));
  assert!(String::from_utf8_lossy(&out.stderr).contains("unsupported platform"));
}

#[cfg(unix)]
fn is_root() -> bool {
  std::fs::metadata("/proc/self")
    .map(|m| std::os::unix::fs::MetadataExt::uid(&m) == 0)
    .unwrap_or(false)
}

#[cfg(unix)]
#[test]
fn run_refuses_a_non_root_caller() {
  // Only meaningful when *not* root; CI and dev shells are not.
  if is_root() {
    return;
  }
  let out = bin().arg("run").output().unwrap();
  assert_eq!(out.status.code(), Some(1));
  assert!(String::from_utf8_lossy(&out.stderr).contains("must run as root"));
}

/// Needs root: `sudo -E cargo test -p aeth-devkit-container -- --ignored`.
#[cfg(unix)]
#[test]
#[ignore]
fn as_root_checks_mounts_prepares_dirs_drops_privileges_and_execs() {
  use std::os::unix::fs::PermissionsExt as _;
  let dir = tempfile::tempdir().unwrap();
  let root = dir.path();
  write(
    root,
    "pyproject.toml",
    "[project.scripts]\nrun-app-demo = \"m:main\"\n[tool.docker]\nrequired_persisted_dirs = [\"persisted_data\"]\n",
  );
  // The script writes its uid where the (root-created, root-owned) tempdir allows: into the
  // chowned persisted dir, which is exactly what the app is expected to be able to write.
  write(
    root,
    ".venv/bin/run-app-demo",
    "#!/bin/sh\nid -u > \"$(dirname \"$0\")/../../persisted_data/uid.txt\"\n",
  );
  std::fs::set_permissions(root.join(".venv/bin/run-app-demo"), std::fs::Permissions::from_mode(0o755)).unwrap();
  let no_mount = root.join("mountinfo-none");
  std::fs::write(&no_mount, "28 0 8:1 / / rw - ext4 /dev/sda1 rw\n").unwrap();
  let out = bin()
    .args(["run", "--pyproject"])
    .arg(root.join("pyproject.toml"))
    .arg("--app-root")
    .arg(root)
    .arg("--mountinfo")
    .arg(&no_mount)
    .output()
    .unwrap();
  assert_eq!(out.status.code(), Some(1));
  assert!(String::from_utf8_lossy(&out.stderr).contains("persisted_data"));
  assert!(
    !root.join("persisted_data").exists(),
    "nothing created before the mount check passes"
  );

  let mounted = root.join("mountinfo-ok");
  std::fs::write(
    &mounted,
    format!("99 28 8:2 /x {} rw - ext4 /dev/sda2 rw\n", root.join("persisted_data").display()),
  )
  .unwrap();
  let out = bin()
    .args(["run", "--pyproject"])
    .arg(root.join("pyproject.toml"))
    .arg("--app-root")
    .arg(root)
    .arg("--mountinfo")
    .arg(&mounted)
    .output()
    .unwrap();
  assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
  assert_eq!(std::fs::read_to_string(root.join("persisted_data/uid.txt")).unwrap().trim(), "999");
  let meta = std::fs::metadata(root.join("persisted_data")).unwrap();
  assert_eq!(std::os::unix::fs::MetadataExt::uid(&meta), 999);
}
