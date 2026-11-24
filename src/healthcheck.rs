//! `devkit-container healthcheck`: every named heartbeat file fresh, or exit 1 with one reason
//! per problem on stderr (what `docker inspect` shows). Reads files only: no root, no
//! capabilities, no `wg`, no pyproject parse, no dependence on the supervisor (spec 7). Under
//! `WG_TOLERATE_DISCONNECTED=1` the tunnel's file is skipped and the app's judged alone
//! (timeclock design §11.2). The app's declared health checks (`checks::probe` over the health
//! folder) are judged after the files, each failing check adding its own line; an app that
//! declares nothing is judged by its heartbeat file alone, as before.

use std::path::{Path, PathBuf};

use crate::{checks, heartbeat};

pub fn run(files: &[PathBuf], max_age: u64, tolerate: bool, health_dir: &Path) -> u8 {
  let now = jiff::Timestamp::now();
  let mut failed = false;
  for file in files {
    // Matched by name so the compose template's exact-list healthcheck stays as it is; a stale
    // tunnel still reaches healthchecks.io through the supervisor's `/fail` (timeclock design §11.2).
    if tolerate && file.file_name().is_some_and(|n| n == heartbeat::TUNNEL_FILE) {
      continue;
    }
    if let Err(reason) = heartbeat::check(file, max_age, now) {
      eprintln!("{reason}");
      failed = true;
    }
  }
  for line in checks::probe(health_dir, now) {
    eprintln!("{line}");
    failed = true;
  }
  u8::from(failed)
}
