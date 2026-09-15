//! The cached bundle (spec 4.4): the last bundle a fetch validated, in the entrypoint's own
//! folder under `persisted_data`, so a boot can reach Connected with GitHub down. Best effort at
//! every call site and never a health signal; the folder is `prepare`'s to create.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

pub const DIR: &str = "persisted_data/wireguard";
pub const FILE: &str = "peers.toml";

pub fn dir(app_root: &Path) -> PathBuf {
  app_root.join("persisted_data").join("wireguard")
}

pub fn path(app_root: &Path) -> PathBuf {
  dir(app_root).join(FILE)
}

/// Atomic (a sibling temp file renamed into place), world-readable, handed to nonroot on Linux
/// so the folder stays uniformly owned. The chown is best effort: a non-root test cannot do it,
/// and `prepare` re-chowns the whole tree at the next boot anyway.
pub fn write(app_root: &Path, text: &str) -> Result<()> {
  let path = path(app_root);
  let tmp = path.with_extension("toml.tmp");
  std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644)).with_context(|| format!("chmod {}", tmp.display()))?;
    let _ = std::os::unix::fs::lchown(&tmp, Some(crate::prepare::NONROOT), Some(crate::prepare::NONROOT));
  }
  std::fs::rename(&tmp, &path).with_context(|| format!("renaming {} to {}", tmp.display(), path.display()))
}

pub fn read(app_root: &Path) -> Result<String> {
  let path = path(app_root);
  std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn write_is_atomic_world_readable_and_read_back() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("persisted_data").join("wireguard")).unwrap();
    write(dir.path(), "schema = 1\n").unwrap();
    assert_eq!(read(dir.path()).unwrap(), "schema = 1\n");
    assert_eq!(
      path(dir.path()),
      dir.path().join("persisted_data").join("wireguard").join("peers.toml")
    );
    assert!(!dir.path().join("persisted_data").join("wireguard").join("peers.toml.tmp").exists());
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt as _;
      assert_eq!(std::fs::metadata(path(dir.path())).unwrap().permissions().mode() & 0o777, 0o644);
    }
    write(dir.path(), "schema = 1\n# second\n").unwrap();
    assert!(read(dir.path()).unwrap().contains("second"));
  }

  #[test]
  fn a_missing_folder_or_file_is_an_error_naming_the_path() {
    let dir = tempfile::tempdir().unwrap();
    let err = write(dir.path(), "x").unwrap_err().to_string();
    assert!(err.contains("peers.toml.tmp"), "{err}");
    let err = read(dir.path()).unwrap_err().to_string();
    assert!(err.contains("peers.toml"), "{err}");
  }
}
