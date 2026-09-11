//! Heartbeat files (spec 7): one timestamp, written by a process while it is healthy, fresh
//! while younger than the max age. The app's is written by `aeth_ext`, the tunnel's by the
//! supervisor; `healthcheck` and the supervisor's ping both read them the same way.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow};

pub const DEFAULT_MAX_AGE_SECS: u64 = 180;
pub const APP_FILE: &str = "heartbeat.txt";
pub const TUNNEL_FILE: &str = "wireguard-heartbeat.txt";

/// Where every heartbeat file lives: the app's log directory, a host bind mount, so another
/// container can mount it read-only and check the same files (spec 7).
pub fn logs_dir(app_root: &Path) -> PathBuf {
  app_root.join("persisted_data").join("logs")
}

/// What `datetime.isoformat()` produces: with an offset it is an instant; bare, it is
/// container-local time (`TZ`, else UTC), which is how `date -d` read it before.
pub fn parse(text: &str) -> Result<jiff::Timestamp> {
  let t = text.trim();
  if let Ok(ts) = t.parse::<jiff::Timestamp>() {
    return Ok(ts);
  }
  let civil: jiff::civil::DateTime = t.parse().map_err(|e| anyhow!("not an ISO 8601 timestamp: {e}"))?;
  civil
    .to_zoned(jiff::tz::TimeZone::system())
    .map(|z| z.timestamp())
    .map_err(|e| anyhow!("not a valid local time: {e}"))
}

/// `Ok` when `path` holds a timestamp younger than `max_age` seconds at `now`; else the
/// one-line reason `healthcheck` prints and the supervisor logs.
pub fn check(path: &Path, max_age: u64, now: jiff::Timestamp) -> Result<(), String> {
  let name = path.display();
  let text = match std::fs::read_to_string(path) {
    Ok(t) => t,
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(format!("{name}: missing")),
    Err(e) => return Err(format!("{name}: unreadable ({e})")),
  };
  if text.trim().is_empty() {
    return Err(format!("{name}: empty"));
  }
  let ts = parse(&text).map_err(|e| format!("{name}: {e}"))?;
  let age = now.as_second() - ts.as_second();
  if age >= 0 && age as u64 >= max_age {
    return Err(format!("{name}: stale by {age} s (max {max_age})"));
  }
  Ok(())
}

/// Write `now` to `path` atomically (a sibling temp file renamed into place) and
/// world-readable, so a reader never sees a half-written timestamp.
pub fn write(path: &Path, now: jiff::Timestamp) -> Result<()> {
  let tmp = path.with_extension("txt.tmp");
  std::fs::write(&tmp, now.to_string()).with_context(|| format!("writing {}", tmp.display()))?;
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644)).with_context(|| format!("chmod {}", tmp.display()))?;
  }
  std::fs::rename(&tmp, path).with_context(|| format!("renaming {} to {}", tmp.display(), path.display()))
}

#[cfg(test)]
mod tests {
  use super::*;

  fn at(s: &str) -> jiff::Timestamp {
    s.parse().unwrap()
  }

  #[test]
  fn offset_and_bare_timestamps_parse_as_aeth_ext_writes_them() {
    // `datetime.now(UTC).isoformat()`, `datetime.now(ZoneInfo("America/Chicago")).isoformat()`,
    // and `datetime.now().isoformat()` (bare: container-local time, UTC in the image).
    assert_eq!(
      parse("2026-09-10T12:00:00.123456+00:00").unwrap(),
      at("2026-09-10T12:00:00.123456Z")
    );
    assert_eq!(parse("2026-09-10T07:00:00-05:00").unwrap(), at("2026-09-10T12:00:00Z"));
    let bare = parse("2026-09-10T12:00:00.5").unwrap();
    // Read in the system zone: equal to the civil time zoned there, whatever the machine's zone.
    let expect = "2026-09-10T12:00:00.5"
      .parse::<jiff::civil::DateTime>()
      .unwrap()
      .to_zoned(jiff::tz::TimeZone::system())
      .unwrap()
      .timestamp();
    assert_eq!(bare, expect);
    assert_eq!(
      parse("  2026-09-10T12:00:00Z\n").unwrap(),
      at("2026-09-10T12:00:00Z"),
      "surrounding whitespace"
    );
    for bad in ["", "yesterday", "2026-13-01T00:00:00Z", "1757505600"] {
      assert!(parse(bad).is_err(), "{bad:?}");
    }
  }

  #[test]
  fn check_names_each_way_a_file_can_be_wrong() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("heartbeat.txt");
    let now = at("2026-09-10T12:03:00Z");
    let missing = check(&p, 180, now).unwrap_err();
    assert!(missing.contains("heartbeat.txt") && missing.contains("missing"), "{missing}");
    std::fs::write(&p, "").unwrap();
    assert!(check(&p, 180, now).unwrap_err().contains("empty"));
    std::fs::write(&p, "nope").unwrap();
    assert!(check(&p, 180, now).unwrap_err().contains("not an ISO 8601"));
    std::fs::write(&p, "2026-09-10T12:00:00Z").unwrap();
    assert!(check(&p, 180, now).unwrap_err().contains("stale by 180 s"));
  }

  #[test]
  fn fresh_means_younger_than_max_age() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("heartbeat.txt");
    std::fs::write(&p, "2026-09-10T12:00:00Z").unwrap();
    assert_eq!(check(&p, 180, at("2026-09-10T12:02:59Z")), Ok(()));
    let stale = check(&p, 180, at("2026-09-10T12:03:00Z")).unwrap_err();
    assert!(stale.contains("stale by 180 s"), "{stale}");
    // A timestamp from the future is fresh: clocks skew, and the file is not lying about age.
    assert_eq!(check(&p, 180, at("2026-09-10T11:00:00Z")), Ok(()));
  }

  #[test]
  fn write_is_atomic_and_world_readable() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("wireguard-heartbeat.txt");
    write(&p, at("2026-09-10T12:00:00Z")).unwrap();
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "2026-09-10T12:00:00Z");
    assert!(!dir.path().join("wireguard-heartbeat.txt.tmp").exists(), "renamed into place");
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt as _;
      assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o644);
    }
    write(&p, at("2026-09-10T12:01:00Z")).unwrap();
    assert_eq!(check(&p, 180, at("2026-09-10T12:02:00Z")), Ok(()));
  }
}
