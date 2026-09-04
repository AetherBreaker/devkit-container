//! `mkdir -p` + recursive chown of every required persisted dir. The chown is injected
//! so the loop is testable on any platform and by any user.

use std::path::Path;

use anyhow::{Context as _, Result};

/// The uid and gid the Dockerfile creates for `nonroot`.
pub const NONROOT: u32 = 999;

/// Create each `entry` under `app_root` (no-op where the mount already provides it), then
/// hand every path in its tree — and every directory created on the way to it — to `chown`.
pub fn prepare(app_root: &Path, entries: &[String], chown: &mut dyn FnMut(&Path) -> Result<()>) -> Result<()> {
  for entry in entries {
    let dir = app_root.join(entry);
    // Remember which ancestors did not exist yet: they are created by `create_dir_all`
    // and must be chowned too, or nonroot could not descend into its own directory.
    let mut created: Vec<std::path::PathBuf> = Vec::new();
    let mut probe = dir.as_path();
    while probe != app_root && !probe.exists() {
      created.push(probe.to_path_buf());
      let Some(parent) = probe.parent() else { break };
      probe = parent;
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    // Deepest first in `created`; chown the shallow ones, then the whole target tree.
    for p in created.iter().rev().filter(|p| *p != &dir) {
      chown(p)?;
    }
    walk(&dir, chown)?;
  }
  Ok(())
}

fn walk(path: &Path, chown: &mut dyn FnMut(&Path) -> Result<()>) -> Result<()> {
  chown(path)?;
  // `symlink_metadata`, not `is_dir()`, which follows links: a symlink to a directory is a
  // leaf here, so a link planted inside a mounted volume cannot redirect the walk (and the
  // chowns with it) outside that volume.
  let meta = std::fs::symlink_metadata(path).with_context(|| format!("reading {}", path.display()))?;
  if meta.is_dir() {
    for entry in std::fs::read_dir(path).with_context(|| format!("listing {}", path.display()))? {
      walk(&entry?.path(), chown)?;
    }
  }
  Ok(())
}

/// Give `path` to uid/gid 999 without following symlinks (a symlink inside a mounted
/// volume must not redirect the chown elsewhere).
#[cfg(unix)]
pub fn chown_nonroot(path: &Path) -> Result<()> {
  std::os::unix::fs::lchown(path, Some(NONROOT), Some(NONROOT)).with_context(|| format!("chown {}", path.display()))
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::path::PathBuf;

  #[test]
  fn creates_missing_dirs_and_chowns_every_path_in_each_tree() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("persisted_data/logs")).unwrap();
    std::fs::write(root.join("persisted_data/logs/a.txt"), "x").unwrap();
    let mut seen: Vec<PathBuf> = Vec::new();
    prepare(root, &["persisted_data".into(), "new/deep".into()], &mut |p| {
      seen.push(p.to_path_buf());
      Ok(())
    })
    .unwrap();
    assert!(root.join("new/deep").is_dir());
    for rel in ["persisted_data", "persisted_data/logs", "persisted_data/logs/a.txt", "new/deep"] {
      assert!(seen.contains(&root.join(rel)), "{rel} not chowned: {seen:?}");
    }
    // `new` itself was created on the way to `new/deep` and must be chowned too, or nonroot
    // cannot traverse into its own directory.
    assert!(seen.contains(&root.join("new")), "{seen:?}");
  }

  #[cfg(unix)]
  #[test]
  fn a_symlinked_directory_is_a_leaf() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("outside")).unwrap();
    std::fs::write(root.join("outside/secret.txt"), "x").unwrap();
    std::fs::create_dir_all(root.join("persisted_data")).unwrap();
    std::os::unix::fs::symlink(root.join("outside"), root.join("persisted_data/link")).unwrap();
    let mut seen: Vec<PathBuf> = Vec::new();
    prepare(root, &["persisted_data".into()], &mut |p| {
      seen.push(p.to_path_buf());
      Ok(())
    })
    .unwrap();
    assert!(seen.contains(&root.join("persisted_data/link")), "{seen:?}");
    assert!(
      !seen.contains(&root.join("persisted_data/link/secret.txt")),
      "walked through a symlink: {seen:?}"
    );
  }

  #[test]
  fn a_chown_failure_stops_with_the_path() {
    let dir = tempfile::tempdir().unwrap();
    let err = prepare(dir.path(), &["x".into()], &mut |p| anyhow::bail!("nope {}", p.display())).unwrap_err();
    assert!(err.to_string().contains("nope"), "{err}");
  }
}
