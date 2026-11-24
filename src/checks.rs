//! The app-declared health checks' file contract (an `aeth_ext` app writes it; the supervisor and
//! the Docker probe read it): `health/checks.json` declares the checks, `health/state/<name>.json`
//! holds each one's last ping and the run's last failure ping. `judge` is the one rule deciding
//! whether a check is healthy.

use std::{
  collections::BTreeMap,
  path::{Path, PathBuf},
};

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
  /// The run's most recent failure ping, kept across success pings (the current one when it is a
  /// failure), so a failure the next ping overwrote before a poll still reaches the supervisor.
  pub last_failure: Option<Failure>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
  pub at: jiff::Timestamp,
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
    let secs = |key: &str, min: u64| -> Result<u64, String> {
      let n = field(c, key, "whole number", Value::is_u64)?.as_u64().unwrap_or_default();
      if (min..=MAX_SECS).contains(&n) {
        Ok(n)
      } else {
        Err(format!("{name}: {key} must be {min} to {MAX_SECS}"))
      }
    };
    let flag = |key: &str| Ok::<_, String>(field(c, key, "boolean", Value::is_boolean)?.as_bool().unwrap_or_default());
    checks.push(Check {
      name: name.clone(),
      period_secs: secs("period_secs", 1)?,
      grace_secs: secs("grace_secs", 0)?,
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
  let last_failure = match root.get("last_failure") {
    None | Some(Value::Null) => None,
    Some(f) => Some(Failure {
      at: timestamp(f, "at").map_err(|e| format!("last_failure.{e}"))?,
      message: optional_message(f, "message").map_err(|e| format!("last_failure.{e}"))?,
    }),
  };
  Ok(Report {
    at: timestamp(&root, "at")?,
    failure: field(&root, "failure", "boolean", Value::is_boolean)?.as_bool().unwrap_or_default(),
    message: optional_message(&root, "message")?,
    last_failure,
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

/// What the Docker probe prints: one line per problem, none when healthy. Without declarations the
/// app is judged as before, unless it has missed the deadline the supervisor set at boot. An
/// unreadable report fails whatever the clock says, as the supervisor's `Watch` treats it.
pub fn probe(dir: &Path, now: jiff::Timestamp) -> Vec<String> {
  let declarations = match read_declarations(dir) {
    Ok(Some(d)) => d,
    Ok(None) => {
      return match read_with(&dir.join(DEADLINE), |t| heartbeat::parse(t.trim()).map_err(|e| e.to_string())) {
        Ok(Some(by)) if now >= by => vec![format!("health checks not declared by {by}")],
        Ok(_) => vec![],
        Err(e) => vec![e],
      };
    }
    Err(e) => return vec![e],
  };
  declarations
    .checks
    .iter()
    .filter_map(|check| match read_report(dir, &check.name) {
      Ok(report) => match judge(check, report.as_ref(), declarations.written_at, now) {
        Judgment::Healthy => None,
        Judgment::Failing(line) | Judgment::Stalled(line) => Some(line),
      },
      Err(reason) => Some(format!("{}: state unreadable ({reason})", check.name)),
    })
    .collect()
}

/// What one supervisor poll found; the caller logs `log`, sends one `/fail` joining `alerts`,
/// skips the plain ping when `withheld`, and stops the app when `stop` is set.
#[derive(Debug, Default, PartialEq, Eq)]
#[cfg_attr(not(unix), allow(dead_code))] // only the Unix supervisor polls
pub struct Verdict {
  pub withheld: bool,
  pub alerts: Vec<String>,
  pub stop: Option<String>,
  pub log: Vec<String>,
}

/// The supervisor's memory across polls: the cached declarations plus what already fired, so each
/// failure ping and each stall episode alerts once.
#[cfg_attr(not(unix), allow(dead_code))]
pub struct Watch {
  timeout_secs: Option<u64>,
  started: jiff::Timestamp,
  declarations: Option<Declarations>,
  /// Last reason declarations were unreadable, so a repeat isn't logged every poll.
  declarations_error: Option<String>,
  /// Per check: the last unreadable-state reason, cleared when it reads again.
  state_errors: BTreeMap<String, String>,
  /// Per check: stalled as of the last judgment; absent before the first, which counts as not stalled.
  stalled: BTreeMap<String, bool>,
  /// Per check: the `at` of the last failure ping alerted on.
  alerted: BTreeMap<String, jiff::Timestamp>,
}

#[cfg_attr(not(unix), allow(dead_code))]
impl Watch {
  pub fn new(timeout_secs: Option<u64>, started: jiff::Timestamp) -> Watch {
    Watch {
      timeout_secs,
      started,
      declarations: None,
      declarations_error: None,
      state_errors: BTreeMap::new(),
      stalled: BTreeMap::new(),
      alerted: BTreeMap::new(),
    }
  }

  /// Declarations are read until one parses, then never again (the app writes them once per boot).
  pub fn poll(&mut self, dir: &Path, now: jiff::Timestamp) -> Verdict {
    let mut verdict = Verdict::default();
    if self.declarations.is_none() {
      match read_declarations(dir) {
        Ok(found) => {
          self.declarations = found;
          self.declarations_error = None;
        }
        Err(reason) => {
          if self.declarations_error.as_ref() != Some(&reason) {
            verdict.log.push(reason.clone());
          }
          self.declarations_error = Some(reason);
        }
      }
    }
    // Unreadable declarations count as not declared, so a broken file can't dodge the deadline.
    let Some(declarations) = &self.declarations else {
      if let Some(secs) = self
        .timeout_secs
        .filter(|&s| now.as_second() - self.started.as_second() >= s as i64)
      {
        let message = format!("health checks not declared within {secs} s");
        verdict.alerts.push(message.clone());
        verdict.stop = Some(message);
      }
      return verdict;
    };
    for check in &declarations.checks {
      let name = &check.name;
      let report = match read_report(dir, name) {
        Ok(report) => {
          self.state_errors.remove(name);
          report
        }
        // Failing but never stalled, as in `probe`; the stalled flag stays as it was so an
        // unreadable poll in the middle of an episode doesn't start a new one.
        Err(reason) => {
          if self.state_errors.insert(name.clone(), reason.clone()).as_ref() != Some(&reason) {
            verdict.log.push(format!("{name}: state unreadable ({reason})"));
          }
          verdict.withheld |= check.affects_ping;
          continue;
        }
      };
      // From `last_failure`, not the latest ping, which a success may have overwritten since the
      // last poll; and whatever `judge` says, since a stall outranks a failure ping there.
      if let Some(f) = report
        .as_ref()
        .and_then(|r| r.last_failure.as_ref())
        .filter(|f| f.at >= declarations.written_at)
        && self.alerted.get(name) != Some(&f.at)
      {
        self.alerted.insert(name.clone(), f.at);
        verdict
          .alerts
          .push(format!("{name}: {}", f.message.as_deref().unwrap_or("failure ping")));
      }
      let judgment = judge(check, report.as_ref(), declarations.written_at, now);
      let stalled = matches!(judgment, Judgment::Stalled(_));
      verdict.withheld |= check.affects_ping && judgment != Judgment::Healthy;
      if self.stalled.insert(name.clone(), stalled) != Some(true)
        && let Judgment::Stalled(reason) = judgment
      {
        if check.fail_ping {
          verdict.alerts.push(reason.clone());
        }
        if check.shutdown {
          verdict.stop.get_or_insert(reason);
        }
      }
    }
    verdict
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
    // The spec allows no grace, but not a zero period; both are capped at MAX_SECS.
    assert!(parse_declarations(&DECL.replace("\"grace_secs\": 120", "\"grace_secs\": 0")).is_ok());
    for (from, to) in [
      ("\"period_secs\": 600", "\"period_secs\": 0"),
      ("\"period_secs\": 600", "\"period_secs\": 31536001"),
      ("\"grace_secs\": 120, \"stall", "\"grace_secs\": 31536001, \"stall"),
    ] {
      assert!(parse_declarations(&DECL.replace(from, to)).is_err(), "{to} must be refused");
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
    assert_eq!(r.last_failure, None, "a missing last_failure is null");
    let kept = parse_report(
      r#"{"at": "2026-11-23T14:06:00Z", "failure": false, "message": null,
          "last_failure": {"at": "2026-11-23T14:05:00Z", "message": "mailbox unreadable"}}"#,
    )
    .unwrap();
    assert_eq!(
      kept.last_failure,
      Some(Failure {
        at: at("2026-11-23T14:05:00Z"),
        message: Some("mailbox unreadable".into())
      })
    );
    for (bad, field) in [
      (r#"{"message": null}"#, "last_failure.at"),
      (r#"{"at": "yesterday", "message": null}"#, "last_failure.at"),
      (r#"{"at": "2026-11-23T14:05:00Z", "message": 3}"#, "last_failure.message"),
      ("true", "last_failure.at"),
    ] {
      let text = format!(r#"{{"at": "2026-11-23T14:06:00Z", "failure": false, "message": null, "last_failure": {bad}}}"#);
      let err = parse_report(&text).unwrap_err();
      assert!(err.starts_with(field), "{bad}: {err}");
    }
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
      last_failure: None,
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
      last_failure: None,
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

  #[test]
  fn the_probe_reports_one_line_per_problem() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let now = at("2026-11-23T14:30:00Z");
    assert!(probe(d, now).is_empty(), "no folder content: healthy, as today");
    std::fs::write(d.join(DEADLINE), "2026-11-23T14:35:00Z").unwrap();
    assert!(probe(d, now).is_empty(), "deadline not reached");
    std::fs::write(d.join(DEADLINE), "2026-11-23T14:29:00Z").unwrap();
    assert_eq!(probe(d, now), ["health checks not declared by 2026-11-23T14:29:00Z"]);
    std::fs::write(d.join(DECLARATIONS), DECL).unwrap();
    std::fs::create_dir(d.join(STATE_DIR)).unwrap();
    std::fs::write(
      d.join(STATE_DIR).join("heartbeat.json"),
      r#"{"at": "2026-11-23T14:29:30Z", "failure": false, "message": null}"#,
    )
    .unwrap();
    std::fs::write(d.join(STATE_DIR).join("imap.json"), "{").unwrap();
    let lines = probe(d, now);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].starts_with("imap: ") && lines[0].contains("unreadable"), "{lines:?}");
    std::fs::write(d.join(DECLARATIONS), "{").unwrap();
    assert!(probe(d, now)[0].contains("not JSON"));
  }

  fn write(dir: &Path, rel: &str, text: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
  }
  /// As the writer has it: a failure ping is its own `last_failure`.
  fn state(at: &str, failure: bool, message: Option<&str>) -> String {
    let last = failure.then(|| serde_json::json!({"at": at, "message": message}));
    serde_json::json!({"at": at, "failure": failure, "message": message, "last_failure": last}).to_string()
  }
  /// A success ping that keeps an earlier failure as `last_failure`.
  fn after_failure(at: &str, failed_at: &str, message: &str) -> String {
    serde_json::json!({"at": at, "failure": false, "message": null,
                       "last_failure": {"at": failed_at, "message": message}})
    .to_string()
  }
  const STALL_IMAP: &str = "imap: IMAP server unresponsive (no ping for 720 s)";
  const STALL_HB: &str = "heartbeat: no ping for 180 s (period 60 s + grace 120 s)";

  #[test]
  fn declarations_are_cached_once_valid() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let mut watch = Watch::new(None, at("2026-11-23T14:00:00Z"));
    assert_eq!(watch.poll(d, at("2026-11-23T14:00:30Z")), Verdict::default());
    write(d, DECLARATIONS, "{");
    let reason = read_declarations(d).unwrap_err();
    let verdict = watch.poll(d, at("2026-11-23T14:01:00Z"));
    assert_eq!(
      verdict,
      Verdict {
        log: vec![reason],
        ..Verdict::default()
      }
    );
    assert_eq!(
      watch.poll(d, at("2026-11-23T14:01:30Z")),
      Verdict::default(),
      "same reason is not logged again"
    );
    write(d, DECLARATIONS, DECL);
    assert_eq!(watch.poll(d, at("2026-11-23T14:02:00Z")), Verdict::default());
    write(d, DECLARATIONS, "{");
    assert_eq!(
      watch.poll(d, at("2026-11-23T14:02:30Z")),
      Verdict::default(),
      "cached: never read again"
    );
  }

  #[test]
  fn the_deadline_alerts_and_stops_only_when_nothing_was_declared() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let message = "health checks not declared within 300 s".to_string();
    let missed = Verdict {
      alerts: vec![message.clone()],
      stop: Some(message),
      ..Verdict::default()
    };
    let mut watch = Watch::new(Some(300), at("2026-11-23T14:00:00Z"));
    assert_eq!(watch.poll(d, at("2026-11-23T14:04:59Z")), Verdict::default());
    assert_eq!(watch.poll(d, at("2026-11-23T14:05:00Z")), missed);
    // Unreadable declarations count as not declared, and still log their reason once.
    write(d, DECLARATIONS, "{");
    let reason = read_declarations(d).unwrap_err();
    let verdict = watch.poll(d, at("2026-11-23T14:05:30Z"));
    assert_eq!(
      verdict,
      Verdict {
        log: vec![reason],
        ..missed
      }
    );
    // Review Focus 4: even an empty table satisfies the deadline.
    write(
      d,
      DECLARATIONS,
      r#"{"version": 1, "written_at": "2026-11-23T14:00:00Z", "checks": {}}"#,
    );
    assert_eq!(watch.poll(d, at("2026-11-23T14:06:00Z")), Verdict::default());
    assert_eq!(watch.poll(d, at("2026-11-23T15:00:00Z")), Verdict::default());
    let mut no_deadline = Watch::new(None, at("2026-11-23T14:00:00Z"));
    let empty = tempfile::tempdir().unwrap();
    assert_eq!(no_deadline.poll(empty.path(), at("2026-11-24T14:00:00Z")), Verdict::default());
  }

  #[test]
  fn affects_ping_withholds_while_failing_or_stalled() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(d, DECLARATIONS, DECL);
    let mut watch = Watch::new(None, at("2026-11-23T14:00:00Z"));
    assert_eq!(watch.poll(d, at("2026-11-23T14:01:00Z")), Verdict::default());
    let verdict = watch.poll(d, at("2026-11-23T14:03:00Z"));
    assert_eq!(
      verdict,
      Verdict {
        withheld: true,
        alerts: vec![STALL_HB.into()],
        ..Verdict::default()
      }
    );
    write(d, "state/heartbeat.json", &state("2026-11-23T14:03:10Z", false, None));
    assert_eq!(watch.poll(d, at("2026-11-23T14:03:20Z")), Verdict::default());
    write(d, "state/heartbeat.json", &state("2026-11-23T14:03:30Z", true, Some("db down")));
    let verdict = watch.poll(d, at("2026-11-23T14:03:40Z"));
    assert_eq!(
      verdict,
      Verdict {
        withheld: true,
        alerts: vec!["heartbeat: db down".into()],
        ..Verdict::default()
      }
    );
    // imap does not affect the ping: its stall leaves it alone.
    write(d, "state/heartbeat.json", &state("2026-11-23T14:11:50Z", false, None));
    assert_eq!(watch.poll(d, at("2026-11-23T14:12:00Z")), Verdict::default());
  }

  #[test]
  fn failure_pings_alert_once_per_ping_whatever_the_flags() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(d, DECLARATIONS, DECL);
    let mut watch = Watch::new(None, at("2026-11-23T14:00:00Z"));
    // Review Focus 5. imap has no flags at all; a ping older than the declarations is not alerted.
    write(d, "state/imap.json", &state("2026-11-23T13:59:00Z", true, Some("old run")));
    write(d, "state/heartbeat.json", &state("2026-11-23T14:01:00Z", true, Some("db down")));
    let first = Verdict {
      withheld: true,
      alerts: vec!["heartbeat: db down".into()],
      ..Verdict::default()
    };
    assert_eq!(watch.poll(d, at("2026-11-23T14:01:10Z")), first);
    write(d, "state/imap.json", &state("2026-11-23T14:01:15Z", true, None));
    let verdict = watch.poll(d, at("2026-11-23T14:01:20Z"));
    assert_eq!(
      verdict,
      Verdict {
        withheld: true,
        alerts: vec!["imap: failure ping".into()],
        ..Verdict::default()
      }
    );
    let same = Verdict {
      withheld: true,
      ..Verdict::default()
    };
    assert_eq!(watch.poll(d, at("2026-11-23T14:01:30Z")), same, "same pings, no new alert");
    write(d, "state/heartbeat.json", &state("2026-11-23T14:01:40Z", true, Some("db down")));
    assert_eq!(
      watch.poll(d, at("2026-11-23T14:01:50Z")),
      Verdict {
        alerts: first.alerts,
        ..same
      }
    );
  }

  #[test]
  fn a_failure_ping_a_success_overwrote_before_the_poll_still_alerts_once() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(d, DECLARATIONS, DECL);
    let mut watch = Watch::new(None, at("2026-11-23T14:00:00Z"));
    assert_eq!(watch.poll(d, at("2026-11-23T14:01:00Z")), Verdict::default());
    // imap has no flags; its latest ping is a success, so it is judged healthy all along.
    write(
      d,
      "state/imap.json",
      &after_failure("2026-11-23T14:01:20Z", "2026-11-23T14:01:10Z", "mailbox unreadable"),
    );
    let alert = Verdict {
      alerts: vec!["imap: mailbox unreadable".into()],
      ..Verdict::default()
    };
    assert_eq!(watch.poll(d, at("2026-11-23T14:01:30Z")), alert);
    assert_eq!(watch.poll(d, at("2026-11-23T14:01:40Z")), Verdict::default(), "alerted once");
    write(
      d,
      "state/imap.json",
      &after_failure("2026-11-23T14:01:55Z", "2026-11-23T14:01:50Z", "mailbox unreadable"),
    );
    assert_eq!(watch.poll(d, at("2026-11-23T14:02:00Z")), alert, "a newer failure alerts again");
    // A last failure older than the declarations is a previous run's.
    write(
      d,
      "state/heartbeat.json",
      &after_failure("2026-11-23T14:02:05Z", "2026-11-23T13:59:00Z", "old run"),
    );
    assert_eq!(watch.poll(d, at("2026-11-23T14:02:10Z")), Verdict::default());
  }

  #[test]
  fn fail_ping_and_shutdown_react_to_each_stall_episode() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    // quiet stalls with no flags; imap (shutdown) and heartbeat (fail_ping) react.
    let quiet = r#""checks": {"quiet": {"period_secs": 10, "grace_secs": 0, "stall_message": null,
                "affects_ping": false, "fail_ping": false, "shutdown": false},"#;
    let text = DECL
      .replace(
        "\"fail_ping\": false, \"shutdown\": false}",
        "\"fail_ping\": false, \"shutdown\": true}",
      )
      .replace("\"checks\": {", quiet);
    write(d, DECLARATIONS, &text);
    let mut watch = Watch::new(None, at("2026-11-23T14:00:00Z"));
    assert_eq!(watch.poll(d, at("2026-11-23T14:00:05Z")), Verdict::default());
    let hb_stall = Verdict {
      withheld: true,
      alerts: vec![STALL_HB.into()],
      ..Verdict::default()
    };
    assert_eq!(watch.poll(d, at("2026-11-23T14:03:00Z")), hb_stall, "quiet stalls silently");
    let still = Verdict {
      withheld: true,
      ..Verdict::default()
    };
    assert_eq!(watch.poll(d, at("2026-11-23T14:03:10Z")), still, "still stalled: nothing new");
    write(d, "state/heartbeat.json", &state("2026-11-23T14:03:15Z", false, None));
    assert_eq!(watch.poll(d, at("2026-11-23T14:03:20Z")), Verdict::default());
    let again = watch.poll(d, at("2026-11-23T14:06:15Z"));
    assert_eq!(
      again,
      Verdict {
        alerts: vec![STALL_HB.into()],
        ..hb_stall
      }
    );
    write(d, "state/heartbeat.json", &state("2026-11-23T14:11:50Z", false, None));
    let stop = Verdict {
      stop: Some(STALL_IMAP.into()),
      ..Verdict::default()
    };
    assert_eq!(watch.poll(d, at("2026-11-23T14:12:00Z")), stop);
    assert_eq!(watch.poll(d, at("2026-11-23T14:12:10Z")), Verdict::default());
    write(d, "state/imap.json", &state("2026-11-23T14:12:20Z", false, None));
    assert_eq!(watch.poll(d, at("2026-11-23T14:12:30Z")), Verdict::default());
    write(d, "state/heartbeat.json", &state("2026-11-23T14:24:10Z", false, None));
    assert_eq!(watch.poll(d, at("2026-11-23T14:24:20Z")), stop, "a new episode stops again");
  }

  #[test]
  fn an_unreadable_state_file_fails_without_stalling() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(d, DECLARATIONS, DECL);
    let mut watch = Watch::new(None, at("2026-11-23T14:00:00Z"));
    write(d, "state/heartbeat.json", "{");
    let reason = read_report(d, "heartbeat").unwrap_err();
    let line = format!("heartbeat: state unreadable ({reason})");
    assert_eq!(
      watch.poll(d, at("2026-11-23T14:01:00Z")),
      Verdict {
        withheld: true,
        log: vec![line],
        ..Verdict::default()
      }
    );
    // Well past period + grace, yet no stall alert and no repeated log line.
    assert_eq!(
      watch.poll(d, at("2026-11-23T14:30:00Z")),
      Verdict {
        withheld: true,
        ..Verdict::default()
      }
    );
    write(d, "state/heartbeat.json", &state("2026-11-23T14:30:05Z", false, None));
    write(d, "state/imap.json", "{");
    let reason = read_report(d, "imap").unwrap_err();
    let line = format!("imap: state unreadable ({reason})");
    assert_eq!(
      watch.poll(d, at("2026-11-23T14:30:10Z")),
      Verdict {
        log: vec![line],
        ..Verdict::default()
      }
    );
  }
}
