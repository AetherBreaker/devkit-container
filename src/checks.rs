//! The app-declared health checks' file contract (an `aeth_ext` app writes it; the supervisor and
//! the Docker probe read it): `health/checks.json` declares the checks, `health/state/<name>.json`
//! holds each one's last ping. `judge` is the one rule deciding whether a check is healthy.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::{heartbeat, wireguard::MAX_SECS};

pub const DIR: &str = "health";
pub const DECLARATIONS: &str = "checks.json";
pub const STATE_DIR: &str = "state";
pub const DEADLINE: &str = "declare-by.txt";
pub const VERSION: u64 = 1;

pub fn dir(app_root: &Path) -> PathBuf {
  heartbeat::logs_dir(app_root).join(DIR)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
  pub name: String,
  pub period_secs: u64,
  pub grace_secs: u64,
  pub stall_message: Option<String>,
  pub affects_ping: bool,
  pub fail_ping: bool,
  pub shutdown: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declarations {
  pub written_at: jiff::Timestamp,
  /// Sorted by name, so effects and logs come out in a stable order.
  pub checks: Vec<Check>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
  pub at: jiff::Timestamp,
  pub failure: bool,
  pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Judgment {
  Healthy,
  Failing(String),
  Stalled(String),
}

/// Reasons carry no path; `read_*` prefixes it.
fn field<'a>(v: &'a Value, key: &str, kind: &str, get: impl Fn(&'a Value) -> bool) -> Result<&'a Value, String> {
  v.get(key)
    .filter(|x| get(x))
    .ok_or_else(|| format!("{key} missing or not a {kind}"))
}

/// `null` or `""` means none.
fn optional_message(v: &Value, key: &str) -> Result<Option<String>, String> {
  match v.get(key) {
    None | Some(Value::Null) => Ok(None),
    Some(Value::String(s)) => Ok(Some(s.clone()).filter(|s| !s.is_empty())),
    Some(_) => Err(format!("{key} missing or not a string or null")),
  }
}

fn timestamp(v: &Value, key: &str) -> Result<jiff::Timestamp, String> {
  let text = field(v, key, "string", Value::is_string)?.as_str().unwrap_or_default();
  heartbeat::parse(text).map_err(|e| format!("{key}: {e}"))
}

pub fn parse_declarations(text: &str) -> Result<Declarations, String> {
  let root: Value = serde_json::from_str(text).map_err(|_| "not JSON".to_string())?;
  let version = field(&root, "version", "number", Value::is_u64)?.as_u64().unwrap_or_default();
  if version != VERSION {
    return Err(format!("unsupported version {version}"));
  }
  let written_at = timestamp(&root, "written_at")?;
  let table = field(&root, "checks", "object", Value::is_object)?
    .as_object()
    .into_iter()
    .flatten();
  let mut checks = Vec::new();
  for (name, c) in table {
    // The name becomes a file name under `state/`: nothing outside this pattern may reach a path.
    let mut chars = name.chars();
    let allowed = |ch: char| ch.is_ascii_lowercase() || ch.is_ascii_digit();
    if !chars.next().is_some_and(allowed) || !chars.all(|ch| allowed(ch) || ch == '_' || ch == '-') || name.len() > 64 {
      return Err(format!("invalid check name {name:?}"));
    }
    let secs = |key: &str| -> Result<u64, String> {
      let n = field(c, key, "whole number", Value::is_u64)?.as_u64().unwrap_or_default();
      if (1..=MAX_SECS).contains(&n) {
        Ok(n)
      } else {
        Err(format!("{name}: {key} must be 1 to {MAX_SECS}"))
      }
    };
    let flag = |key: &str| Ok::<_, String>(field(c, key, "boolean", Value::is_boolean)?.as_bool().unwrap_or_default());
    checks.push(Check {
      name: name.clone(),
      period_secs: secs("period_secs")?,
      grace_secs: secs("grace_secs")?,
      stall_message: optional_message(c, "stall_message")?,
      affects_ping: flag("affects_ping")?,
      fail_ping: flag("fail_ping")?,
      shutdown: flag("shutdown")?,
    });
  }
  checks.sort_by(|a, b| a.name.cmp(&b.name));
  Ok(Declarations { written_at, checks })
}

pub fn parse_report(text: &str) -> Result<Report, String> {
  let root: Value = serde_json::from_str(text).map_err(|_| "not JSON".to_string())?;
  Ok(Report {
    at: timestamp(&root, "at")?,
    failure: field(&root, "failure", "boolean", Value::is_boolean)?.as_bool().unwrap_or_default(),
    message: optional_message(&root, "message")?,
  })
}

/// `Ok(None)` when the file is absent; every other problem is `Err("<path>: <reason>")`.
fn read_with<T>(path: &Path, parse: impl Fn(&str) -> Result<T, String>) -> Result<Option<T>, String> {
  match std::fs::read_to_string(path) {
    Ok(text) => parse(&text).map(Some).map_err(|e| format!("{}: {e}", path.display())),
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
    Err(e) => Err(format!("{}: unreadable ({e})", path.display())),
  }
}

pub fn read_declarations(dir: &Path) -> Result<Option<Declarations>, String> {
  read_with(&dir.join(DECLARATIONS), parse_declarations)
}

/// `name` must come from `parse_declarations`, which has already vetted it as a file name.
pub fn read_report(dir: &Path, name: &str) -> Result<Option<Report>, String> {
  read_with(&dir.join(STATE_DIR).join(format!("{name}.json")), parse_report)
}

/// A ping older than the declarations belongs to a previous run and is ignored, so the clock
/// restarts at `written_at`. Age is whole seconds and stalls at `period + grace`; a negative age
/// (clock skew) is fresh, as in `heartbeat::check`. A stall outranks an older failure ping.
pub fn judge(check: &Check, report: Option<&Report>, written_at: jiff::Timestamp, now: jiff::Timestamp) -> Judgment {
  let report = report.filter(|r| r.at >= written_at);
  let clock = report.map_or(written_at, |r| r.at);
  let age = now.as_second() - clock.as_second();
  let limit = check.period_secs + check.grace_secs;
  let name = &check.name;
  if age >= 0 && age as u64 >= limit {
    return Judgment::Stalled(match &check.stall_message {
      Some(m) => format!("{name}: {m} (no ping for {age} s)"),
      None => format!(
        "{name}: no ping for {age} s (period {} s + grace {} s)",
        check.period_secs, check.grace_secs
      ),
    });
  }
  match report {
    Some(r) if r.failure => Judgment::Failing(format!("{name}: {}", r.message.as_deref().unwrap_or("failure ping"))),
    _ => Judgment::Healthy,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn at(s: &str) -> jiff::Timestamp {
    s.parse().unwrap()
  }
  const DECL: &str = r#"{"version": 1, "written_at": "2026-11-23T14:00:00+00:00", "checks": {
  "imap": {"period_secs": 600, "grace_secs": 120, "stall_message": "IMAP server unresponsive",
           "affects_ping": false, "fail_ping": false, "shutdown": false},
  "heartbeat": {"period_secs": 60, "grace_secs": 120, "stall_message": null,
                "affects_ping": true, "fail_ping": true, "shutdown": false}}}"#;

  #[test]
  fn declarations_parse_sorted_by_name() {
    let d = parse_declarations(DECL).unwrap();
    assert_eq!(d.written_at, at("2026-11-23T14:00:00Z"));
    assert_eq!(d.checks.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["heartbeat", "imap"]);
    assert_eq!(d.checks[1].stall_message.as_deref(), Some("IMAP server unresponsive"));
    assert!(d.checks[0].affects_ping && d.checks[0].fail_ping && !d.checks[0].shutdown);
  }

  #[test]
  fn bad_declarations_are_unreadable_with_a_reason() {
    assert!(parse_declarations("{").unwrap_err().contains("not JSON"));
    assert!(
      parse_declarations(&DECL.replace("\"version\": 1", "\"version\": 2"))
        .unwrap_err()
        .contains("unsupported version 2")
    );
    assert!(
      parse_declarations(&DECL.replace("\"period_secs\": 600, ", ""))
        .unwrap_err()
        .contains("period_secs")
    );
    // Review Focus 2: a name becomes a file name, so anything outside the pattern is refused.
    for bad in ["../x", "Imap", "", "a/b", &"a".repeat(65)] {
      let text = DECL.replace("\"imap\"", &format!("{bad:?}"));
      assert!(parse_declarations(&text).is_err(), "{bad:?} must be refused");
    }
    // Review Focus 4.
    let empty = parse_declarations(r#"{"version": 1, "written_at": "2026-11-23T14:00:00Z", "checks": {}}"#).unwrap();
    assert!(empty.checks.is_empty());
  }

  #[test]
  fn reports_parse_and_read() {
    let r = parse_report(r#"{"at": "2026-11-23T14:05:00+00:00", "failure": true, "message": "mailbox unreadable"}"#).unwrap();
    assert_eq!(
      (r.at, r.failure, r.message.as_deref()),
      (at("2026-11-23T14:05:00Z"), true, Some("mailbox unreadable"))
    );
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(read_report(dir.path(), "imap"), Ok(None));
    assert_eq!(read_declarations(dir.path()), Ok(None));
  }

  #[test]
  fn judging_follows_the_spec_rule() {
    let d = parse_declarations(DECL).unwrap();
    let (hb, imap) = (&d.checks[0], &d.checks[1]);
    let w = d.written_at;
    let ok = |s| Report {
      at: at(s),
      failure: false,
      message: None,
    };
    // Not pinged yet: the clock starts at written_at; 720 s = period 600 + grace 120.
    assert_eq!(judge(imap, None, w, at("2026-11-23T14:11:59Z")), Judgment::Healthy);
    assert_eq!(
      judge(imap, None, w, at("2026-11-23T14:12:00Z")),
      Judgment::Stalled("imap: IMAP server unresponsive (no ping for 720 s)".into())
    );
    assert_eq!(
      judge(hb, None, w, at("2026-11-23T14:03:00Z")),
      Judgment::Stalled("heartbeat: no ping for 180 s (period 60 s + grace 120 s)".into())
    );
    // Review Focus 1: same second as written_at counts.
    assert_eq!(
      judge(imap, Some(&ok("2026-11-23T14:00:00Z")), w, at("2026-11-23T14:11:59Z")),
      Judgment::Healthy
    );
    // Older than the declarations: ignored, the clock stays at written_at.
    assert_eq!(
      judge(imap, Some(&ok("2026-11-23T13:59:59Z")), w, at("2026-11-23T14:12:00Z")),
      Judgment::Stalled("imap: IMAP server unresponsive (no ping for 720 s)".into())
    );
    let fail = |m: Option<&str>| Report {
      at: at("2026-11-23T14:01:00Z"),
      failure: true,
      message: m.map(Into::into),
    };
    assert_eq!(
      judge(imap, Some(&fail(Some("mailbox unreadable"))), w, at("2026-11-23T14:02:00Z")),
      Judgment::Failing("imap: mailbox unreadable".into())
    );
    assert_eq!(
      judge(imap, Some(&fail(None)), w, at("2026-11-23T14:02:00Z")),
      Judgment::Failing("imap: failure ping".into())
    );
    // A stall wins over an older failure ping.
    assert!(matches!(
      judge(imap, Some(&fail(None)), w, at("2026-11-23T14:13:00Z")),
      Judgment::Stalled(_)
    ));
    // Review Focus 3: a ping from the future is not stalled.
    assert_eq!(
      judge(imap, Some(&ok("2026-11-23T15:00:00Z")), w, at("2026-11-23T14:30:00Z")),
      Judgment::Healthy
    );
  }
}
