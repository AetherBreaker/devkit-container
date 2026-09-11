//! `devkit-container healthcheck`: every named heartbeat file fresh, or exit 1 with one reason
//! per problem on stderr (what `docker inspect` shows). Reads files only: no root, no
//! capabilities, no `wg`, no pyproject parse, no dependence on the supervisor (spec 7).

use std::path::PathBuf;

use crate::heartbeat;

pub fn run(files: &[PathBuf], max_age: u64) -> u8 {
  let now = jiff::Timestamp::now();
  let mut failed = false;
  for file in files {
    if let Err(reason) = heartbeat::check(file, max_age, now) {
      eprintln!("{reason}");
      failed = true;
    }
  }
  u8::from(failed)
}
