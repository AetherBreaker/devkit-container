//! The placeholder log file (spec 5.8): every line the binary writes for itself goes to stderr
//! as before and is appended, with a timestamp, to `persisted_data/logs/devkit-container.log`.
//! Dumb on purpose: open, append one line, close; nothing buffered, rotated or capped; a failed
//! write changes nothing. Hooking the binary into aeth_ext's logging is later work (todo.md).
#![allow(dead_code)] // until run and the supervisor use it (task 11)

use std::io::Write as _;
use std::path::{Path, PathBuf};

pub const FILE: &str = "devkit-container.log";

#[derive(Debug, Clone)]
pub struct Log {
  path: PathBuf,
}

impl Log {
  pub fn new(app_root: &Path) -> Log {
    Log {
      path: crate::heartbeat::logs_dir(app_root).join(FILE),
    }
  }

  pub fn path(&self) -> &Path {
    &self.path
  }

  /// `devkit-container: <msg>` to stderr, then `<timestamp> <msg>` appended to the file. The
  /// file write is best effort: before `prepare` the folder may not exist yet, and nothing here
  /// may ever fail the run.
  pub fn line(&self, msg: &str) {
    eprintln!("devkit-container: {msg}");
    let _ = self.append(msg);
  }

  fn append(&self, msg: &str) -> std::io::Result<()> {
    let existed = self.path.exists();
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&self.path)?;
    writeln!(file, "{} {msg}", jiff::Timestamp::now())?;
    #[cfg(unix)]
    if !existed {
      use std::os::unix::fs::PermissionsExt as _;
      let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o644));
      let _ = std::os::unix::fs::lchown(&self.path, Some(crate::prepare::NONROOT), Some(crate::prepare::NONROOT));
    }
    #[cfg(not(unix))]
    let _ = existed;
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn each_line_is_appended_with_a_timestamp_and_the_file_is_world_readable() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(crate::heartbeat::logs_dir(dir.path())).unwrap();
    let log = Log::new(dir.path());
    log.line("first");
    log.line("second: with details");
    let text = std::fs::read_to_string(log.path()).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    for (line, msg) in lines.iter().zip(["first", "second: with details"]) {
      let (stamp, rest) = line.split_once(' ').unwrap();
      assert!(stamp.parse::<jiff::Timestamp>().is_ok(), "{stamp} is not a timestamp");
      assert_eq!(rest, msg);
    }
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt as _;
      assert_eq!(std::fs::metadata(log.path()).unwrap().permissions().mode() & 0o777, 0o644);
    }
  }

  #[test]
  fn a_missing_folder_loses_the_line_quietly() {
    let dir = tempfile::tempdir().unwrap();
    let log = Log::new(dir.path());
    log.line("nowhere to go");
    assert!(!log.path().exists());
  }
}
