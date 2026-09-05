//! Reading `/proc/self/mountinfo` and deciding whether a required directory is backed by
//! a mount — the "container started without its volume" check.

use std::path::{Path, PathBuf};

/// Mount points (field 5 of each mountinfo line). Paths with spaces are written with
/// octal escapes (`\040`), decoded here.
pub fn parse_mountinfo(text: &str) -> Vec<PathBuf> {
  text
    .lines()
    // `nth(4)` is the fifth whitespace-separated field: the mount point.
    .filter_map(|l| l.split_whitespace().nth(4))
    .map(|p| PathBuf::from(unescape(p)))
    .collect()
}

fn unescape(s: &str) -> String {
  // Decoded as bytes, not chars: the kernel escapes only space, tab, newline and backslash,
  // so a non-ASCII path arrives as raw UTF-8, and `byte as char` would turn each byte into
  // a Latin-1 code point (`é` → `Ã©`). The bytes are reassembled into UTF-8 at the end.
  let mut out: Vec<u8> = Vec::with_capacity(s.len());
  let bytes = s.as_bytes();
  let mut i = 0;
  while i < bytes.len() {
    // `\` followed by three octal digits is one byte; anything else is literal. The bound
    // guarantees indices `i+1..=i+3` exist before the slice is taken.
    if bytes[i] == b'\\' && i + 3 < bytes.len() && bytes[i + 1..i + 4].iter().all(|b| (b'0'..=b'7').contains(b)) {
      out.push(u8::from_str_radix(&s[i + 1..i + 4], 8).unwrap_or(b'?'));
      i += 4;
    } else {
      out.push(bytes[i]);
      i += 1;
    }
  }
  String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// Walk from `app_root/entry` upwards, stopping *before* `app_root`; `true` if any path on
/// the way is a mount point. The root filesystem is always a mount, which is why the walk
/// must exclude `/app` and above.
pub fn is_backed(mounts: &[PathBuf], app_root: &Path, entry: &str) -> bool {
  let mut p = app_root.join(entry);
  while p != app_root {
    if mounts.iter().any(|m| m == &p) {
      return true;
    }
    // `parent()` is `None` only at the filesystem root, which we never reach from under
    // `app_root`; treat it as "not backed" rather than looping forever.
    let Some(parent) = p.parent() else { return false };
    p = parent.to_path_buf();
  }
  false
}

/// The entries with no mount behind them, in input order.
pub fn unbacked<'a>(mounts: &[PathBuf], app_root: &Path, entries: &'a [String]) -> Vec<&'a str> {
  entries
    .iter()
    .map(String::as_str)
    .filter(|e| !is_backed(mounts, app_root, e))
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  const MOUNTINFO: &str = "\
22 28 0:21 / /proc rw,nosuid - proc proc rw
28 0 8:1 / / rw,relatime - ext4 /dev/sda1 rw
99 28 8:2 /data/x_files /app/persisted_data rw,relatime - ext4 /dev/sda2 rw
100 28 8:2 /d /app/with\\040space rw - ext4 /dev/sda2 rw
101 28 8:2 /d /app/données rw - ext4 /dev/sda2 rw
";

  #[test]
  fn mount_points_come_from_field_five_with_octal_escapes_decoded() {
    let m = parse_mountinfo(MOUNTINFO);
    assert!(m.contains(&PathBuf::from("/app/persisted_data")));
    assert!(m.contains(&PathBuf::from("/app/with space")));
    assert!(m.contains(&PathBuf::from("/app/données")), "raw UTF-8 survives: {m:?}");
    assert!(m.contains(&PathBuf::from("/")));
  }

  #[test]
  fn an_entry_is_backed_by_itself_or_an_ancestor_below_app() {
    let m = parse_mountinfo(MOUNTINFO);
    let app = Path::new("/app");
    assert!(is_backed(&m, app, "persisted_data"));
    assert!(is_backed(&m, app, "persisted_data/logs/deeper"));
    // `/` is a mount point, but the walk stops before `/app`, so root does not count.
    assert!(!is_backed(&m, app, "scratch"));
    let entries = vec!["persisted_data".to_string(), "scratch".to_string(), "other/x".to_string()];
    assert_eq!(unbacked(&m, app, &entries), ["scratch", "other/x"]);
  }
}
