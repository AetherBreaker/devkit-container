# App-declared health checks (devkit-container) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** devkit-container reads the health-check declarations and pings an aeth-ext app writes
under `persisted_data/logs/health/`, judges them for Docker health, and has the supervisor act on
their flags (withhold the ping, send stall `/fail`s, stop the app) and on the startup deadline.

**Architecture:** One new cross-platform module, `src/checks.rs`, holds everything testable: the
file contract (parse/read), the judging rule, the probe used by `healthcheck`, and `Watch`, a
pure per-poll state machine the supervisor drives (like `health.rs` for the tunnel). `run`,
`healthcheck`, `Pinger` and the supervisor loop get thin glue around it.

**Tech Stack:** Rust 2024, `jiff` timestamps, `serde_json::Value` (no serde derive), `toml_edit`,
`tempfile` in tests.

**Spec:** `docs/superpowers/specs/2026-11-23-app-health-checks-design.md`

## Global Constraints

- Work on branch `feat/app-health-checks`. Never merge; the user reviews the PR.
- Commits follow Conventional Commits with the module as scope (`feat(checks): …`), ending with
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- `rustfmt.toml`: `tab_spaces = 2`, `max_width = 135`. CI runs `cargo fmt --all --check`,
  `cargo clippy --all-targets -- -D warnings` and `cargo test` on windows-latest and ubuntu-latest.
- `healthcheck` builds on Windows: nothing in `checks.rs` may be `#[cfg(unix)]`.
- No new dependencies. `serde_json` moves from `[target.'cfg(unix)'.dependencies]` to
  `[dependencies]` (keep `1.0.151`); read JSON through `serde_json::Value`, no serde derive.
- Timestamps are read with `heartbeat::parse` and written with `heartbeat::write`.
- Every `*_SECS` value is a whole number from 1 to `wireguard::MAX_SECS` (31536000).
- The version stays 2.3.1 in this PR. The user's `devkit release minor` makes it 2.4.0.
- The compose and Dockerfile templates are unchanged.
- While iterating, run only the targeted test; run fmt, clippy and the full `cargo test` once at
  the end (AGENTS.md "Testing Workflow").
- Comments carry reasoning, densely; don't extract small single-use helpers (AGENTS.md).

## Review Focus

1. A ping written in the same second as `checks.json` (`at == written_at`) counts as current, not
   as older than the declarations.
2. A declared name that isn't `[a-z0-9][a-z0-9_-]{0,63}` (e.g. `../x`) makes the declarations
   unreadable; no path outside `health/state/` is ever read.
3. A ping `at` in the future (clock skew) is not stalled, matching `heartbeat::check`.
4. `checks.json` with an empty `checks` table: healthy, no effects, and the deadline is satisfied.
5. A check that stalls, recovers and stalls again sends a stall `/fail` each time; a failure ping
   with a new `at` sends again, while the same failure seen on the next poll does not.

---

### Task 1: The file contract and the judging rule

**Files:**

- Create: `src/checks.rs`
- Modify: `src/main.rs` (add `mod checks;`, no attribute), `Cargo.toml` (move `serde_json`)

**Interfaces:**

- Produces (all `pub` in `crate::checks`):
  - `const DIR: &str = "health"`, `const DECLARATIONS: &str = "checks.json"`,
    `const STATE_DIR: &str = "state"`, `const DEADLINE: &str = "declare-by.txt"`,
    `const VERSION: u64 = 1`
  - `fn dir(app_root: &Path) -> PathBuf`: `heartbeat::logs_dir(app_root).join(DIR)`
  - `struct Check { name: String, period_secs: u64, grace_secs: u64, stall_message: Option<String>, affects_ping: bool, fail_ping: bool, shutdown: bool }`
    (`Debug, Clone, PartialEq, Eq`)
  - `struct Declarations { written_at: jiff::Timestamp, checks: Vec<Check> }`, with checks sorted by name
  - `struct Report { at: jiff::Timestamp, failure: bool, message: Option<String> }`
  - `enum Judgment { Healthy, Failing(String), Stalled(String) }`
  - `fn parse_declarations(text: &str) -> Result<Declarations, String>`
  - `fn read_declarations(dir: &Path) -> Result<Option<Declarations>, String>`: `Ok(None)` when absent
  - `fn parse_report(text: &str) -> Result<Report, String>`
  - `fn read_report(dir: &Path, name: &str) -> Result<Option<Report>, String>`: reads `dir/state/<name>.json`
  - `fn judge(check: &Check, report: Option<&Report>, written_at: jiff::Timestamp, now: jiff::Timestamp) -> Judgment`

- [ ] **Step 1: Write the failing tests** in `#[cfg(test)] mod tests` of `src/checks.rs`

```rust
fn at(s: &str) -> jiff::Timestamp { s.parse().unwrap() }
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
  assert!(parse_declarations(&DECL.replace("\"version\": 1", "\"version\": 2")).unwrap_err().contains("unsupported version 2"));
  assert!(parse_declarations(&DECL.replace("\"period_secs\": 600, ", "")).unwrap_err().contains("period_secs"));
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
  assert_eq!((r.at, r.failure, r.message.as_deref()), (at("2026-11-23T14:05:00Z"), true, Some("mailbox unreadable")));
  let dir = tempfile::tempdir().unwrap();
  assert_eq!(read_report(dir.path(), "imap"), Ok(None));
  assert_eq!(read_declarations(dir.path()), Ok(None));
}

#[test]
fn judging_follows_the_spec_rule() {
  let d = parse_declarations(DECL).unwrap();
  let (hb, imap) = (&d.checks[0], &d.checks[1]);
  let w = d.written_at;
  let ok = |s| Report { at: at(s), failure: false, message: None };
  // Not pinged yet: the clock starts at written_at; 720 s = period 600 + grace 120.
  assert_eq!(judge(imap, None, w, at("2026-11-23T14:11:59Z")), Judgment::Healthy);
  assert_eq!(judge(imap, None, w, at("2026-11-23T14:12:00Z")),
             Judgment::Stalled("imap: IMAP server unresponsive (no ping for 720 s)".into()));
  assert_eq!(judge(hb, None, w, at("2026-11-23T14:03:00Z")),
             Judgment::Stalled("heartbeat: no ping for 180 s (period 60 s + grace 120 s)".into()));
  // Review Focus 1: same second as written_at counts.
  assert_eq!(judge(imap, Some(&ok("2026-11-23T14:00:00Z")), w, at("2026-11-23T14:11:59Z")), Judgment::Healthy);
  // Older than the declarations: ignored, the clock stays at written_at.
  assert_eq!(judge(imap, Some(&ok("2026-11-23T13:59:59Z")), w, at("2026-11-23T14:12:00Z")),
             Judgment::Stalled("imap: IMAP server unresponsive (no ping for 720 s)".into()));
  let fail = |m: Option<&str>| Report { at: at("2026-11-23T14:01:00Z"), failure: true, message: m.map(Into::into) };
  assert_eq!(judge(imap, Some(&fail(Some("mailbox unreadable"))), w, at("2026-11-23T14:02:00Z")),
             Judgment::Failing("imap: mailbox unreadable".into()));
  assert_eq!(judge(imap, Some(&fail(None)), w, at("2026-11-23T14:02:00Z")), Judgment::Failing("imap: failure ping".into()));
  // A stall wins over an older failure ping.
  assert!(matches!(judge(imap, Some(&fail(None)), w, at("2026-11-23T14:13:00Z")), Judgment::Stalled(_)));
  // Review Focus 3: a ping from the future is not stalled.
  assert_eq!(judge(imap, Some(&ok("2026-11-23T15:00:00Z")), w, at("2026-11-23T14:30:00Z")), Judgment::Healthy);
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test checks::`
Expected: FAIL to compile (module or items missing).

- [ ] **Step 3: Implement the Task 1 interfaces in `src/checks.rs`**

Parse with `serde_json::Value` and `.get()` chains, as `fetch.rs` does. Unreadable reasons name
the file: `"<path>: not JSON"`, `"<path>: unsupported version N"`,
`"<path>: <field> missing or not a <type>"`, `"<path>: invalid check name <name:?>"`. A
`read_*` that hits `NotFound` returns `Ok(None)`; any other I/O error is `Err("<path>: unreadable (<e>)")`.
`judge` takes `report` only when `report.at >= written_at`; age is `now - clock` in whole
seconds, and a negative age is fresh. `stall_message` empty or `null` means none.

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test checks::`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/checks.rs src/main.rs Cargo.toml Cargo.lock
git commit -m "feat(checks): read and judge app-declared health checks"
```

### Task 2: Docker's probe judges the health folder

**Files:**

- Modify: `src/checks.rs` (add `probe`), `src/healthcheck.rs`, `src/main.rs:92-101`
- Test: `src/checks.rs`, `tests/entrypoint.rs`

**Interfaces:**

- Consumes: Task 1.
- Produces: `pub fn probe(dir: &Path, now: jiff::Timestamp) -> Vec<String>` in `crate::checks`;
  `healthcheck::run(files: &[PathBuf], max_age: u64, tolerate: bool, health_dir: &Path) -> u8`.

- [ ] **Step 1: Write the failing tests**

In `src/checks.rs` tests (write files with `std::fs::write` under a `tempfile::tempdir()`):

```rust
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
  std::fs::write(d.join(STATE_DIR).join("heartbeat.json"), r#"{"at": "2026-11-23T14:29:30Z", "failure": false, "message": null}"#).unwrap();
  std::fs::write(d.join(STATE_DIR).join("imap.json"), "{").unwrap();
  let lines = probe(d, now);
  assert_eq!(lines.len(), 1, "{lines:?}");
  assert!(lines[0].starts_with("imap: ") && lines[0].contains("unreadable"), "{lines:?}");
  std::fs::write(d.join(DECLARATIONS), "{").unwrap();
  assert!(probe(d, now)[0].contains("not JSON"));
}
```

In `tests/entrypoint.rs`, next to the existing healthcheck tests (runs on every platform):
`healthcheck_fails_with_the_apps_stall_message`: build `<root>/persisted_data/logs/` with a fresh
`heartbeat.txt`, `health/checks.json` declaring `imap` (period 1, grace 0,
`stall_message` "IMAP server unresponsive") with `written_at` an hour ago, no state file; run the
binary `healthcheck --app-root <root>`; assert exit code 1 and stderr containing
`imap: IMAP server unresponsive (no ping for`.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test checks::tests::the_probe && cargo test --test entrypoint healthcheck_fails_with_the_apps_stall_message`
Expected: FAIL (`probe` missing; the binary exits 0).

- [ ] **Step 3: Implement**

`probe`: declarations absent → the deadline line if `DEADLINE` exists and `now >=` its time (an
unparsable deadline file is its own line), else nothing; declarations unreadable → that one line;
otherwise one line per check that is not `Healthy`. An unreadable report is
`"<name>: state unreadable (<reason>)"` whatever the clock says, and `Watch` (Task 6) treats it
the same way. `healthcheck::run` prints `probe(health_dir, now)` lines after the `--file` checks and
fails if any. `main.rs` passes `checks::dir(&app_root)`.

- [ ] **Step 4: Run to verify they pass** (same commands). Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/checks.rs src/healthcheck.rs src/main.rs tests/entrypoint.rs
git commit -m "feat(healthcheck): judge the app's declared health checks"
```

### Task 3: The `health_checks_timeout_secs` key

**Files:**

- Modify: `src/pyproject.rs`

**Interfaces:**

- Produces: `pub fn health_checks_timeout_secs(doc: &DocumentMut) -> Result<Option<u64>>`
  (with `#[cfg_attr(not(unix), allow(dead_code))]` like the other getters).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn the_health_checks_timeout_is_an_optional_bounded_whole_number() {
  assert_eq!(health_checks_timeout_secs(&doc("[project]\n")).unwrap(), None);
  assert_eq!(health_checks_timeout_secs(&doc("[tool.docker]\nhealth_checks_timeout_secs = 300\n")).unwrap(), Some(300));
  assert_eq!(health_checks_timeout_secs(&doc("[tool.docker]\nhealth_checks_timeout_secs = 31536000\n")).unwrap(), Some(31_536_000));
  for bad in ["0", "31536001", "-5", "1.5", "\"300\"", "true"] {
    let e = health_checks_timeout_secs(&doc(&format!("[tool.docker]\nhealth_checks_timeout_secs = {bad}\n"))).unwrap_err().to_string();
    assert!(e.contains("[tool.docker].health_checks_timeout_secs") && e.contains("1 to 31536000"), "{bad}: {e}");
  }
}
```

- [ ] **Step 2: Run to verify it fails.** Run: `cargo test pyproject::tests::the_health_checks_timeout`. Expected: FAIL to compile.

- [ ] **Step 3: Implement** with `item.as_integer()`; the error is
`"[tool.docker].health_checks_timeout_secs must be a whole number of seconds, 1 to 31536000, got {value}"`,
using `wireguard::MAX_SECS`.

- [ ] **Step 4: Run to verify it passes.** Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/pyproject.rs
git commit -m "feat(pyproject): read [tool.docker].health_checks_timeout_secs"
```

### Task 4: `run` resets the health folder and writes the deadline

**Files:**

- Modify: `src/run.rs:32-40` (read the key), `:87-110` (folder handling), `:127-137` (pass it on), `src/supervisor.rs:42-54` (`Plan`)
- Test: `tests/entrypoint.rs` (`as_root_checks_mounts_prepares_dirs_drops_privileges_and_execs`, `#[ignore]`, root)

**Interfaces:**

- Consumes: Task 1 (`checks::dir`, `checks::DEADLINE`), Task 3.
- Produces: `Plan` gains `pub health_timeout_secs: Option<u64>`.

- [ ] **Step 1: Extend the root test** so that, with `health_checks_timeout_secs = 300` in its
pyproject and a leftover `persisted_data/logs/health/state/old.json`, after `run` the leftover is
gone, `persisted_data/logs/health/` is owned by 999:999, and `declare-by.txt` parses to roughly now + 300 s.

- [ ] **Step 2: Run to verify it fails.** Run: `sudo -E cargo test --test entrypoint -- --ignored as_root`. Expected: FAIL.

- [ ] **Step 3: Implement**

Before `prepare`, `remove_dir_all(checks::dir(&args.app_root))`, ignoring `NotFound` like the
tunnel file removal and propagating anything else through `down`. With the key set, push
`persisted_data/logs/health` onto `to_prepare` (so `prepare` creates and chowns it and any missing
ancestor), and after `prepare` write `declare-by.txt` with `heartbeat::write(…, now + timeout)`.
Pass the key into `Plan`.

- [ ] **Step 4: Run to verify it passes** (same command). Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/run.rs src/supervisor.rs tests/entrypoint.rs
git commit -m "feat(run): reset the health folder at start and write the declarations deadline"
```

### Task 5: `Pinger` learns to withhold

**Files:**

- Modify: `src/supervisor.rs:12-33` and its test module

**Interfaces:**

- Produces: `pub fn decide(&mut self, core_fresh: bool, withheld: bool) -> Option<Kind>`.

- [ ] **Step 1: Rewrite the existing Pinger test for the new signature and add**

```rust
#[test]
fn withheld_skips_plain_and_start_but_never_masks_a_core_fail() {
  let mut p = Pinger::default();
  assert_eq!(p.decide(true, true), None, "no /start while withheld");
  assert_eq!(p.decide(true, false), Some(Kind::Start));
  assert_eq!(p.decide(true, true), None, "withheld: nothing, not /fail");
  assert_eq!(p.decide(false, true), Some(Kind::Fail), "a core stale transition still fails");
  assert_eq!(p.decide(false, false), None, "one /fail per outage");
  assert_eq!(p.decide(true, false), Some(Kind::Plain));
}
```

- [ ] **Step 2: Run to verify it fails.** Run: `cargo test supervisor::tests`. Expected: FAIL to compile.

- [ ] **Step 3: Implement** per the spec's "Regular ping": the `/fail` transition tracks
`core_fresh` alone; while withheld, return `None`; `/start` is sent the first time
`core_fresh && !withheld`.

- [ ] **Step 4: Run to verify it passes.** Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/supervisor.rs
git commit -m "feat(supervisor): let failing health checks withhold the regular ping"
```

### Task 6: `Watch`, the supervisor's per-poll health state

**Files:**

- Modify: `src/checks.rs`

**Interfaces:**

- Consumes: Task 1.
- Produces (in `crate::checks`):
  - `#[derive(Debug, Default, PartialEq, Eq)] pub struct Verdict { pub withheld: bool, pub alerts: Vec<String>, pub stop: Option<String>, pub log: Vec<String> }`
  - `pub struct Watch` with `pub fn new(timeout_secs: Option<u64>, started: jiff::Timestamp) -> Watch`
    and `pub fn poll(&mut self, dir: &Path, now: jiff::Timestamp) -> Verdict`

Semantics, per the spec's Supervisor section:

- Declarations are read until valid, then cached and never read again. Unreadable: one `log` line
  per distinct reason; retry next poll.
- Absent at or past `started + timeout`: `alerts` and `stop` both carry
  `"health checks not declared within N s"`.
- `withheld`: any `affects_ping` check is not `Healthy`.
- `alerts` gets `"<name>: <message>"` for each new failure ping (`at` differs from the last one
  this `Watch` alerted on for that check), whatever the flags. On a check's transition into
  `Stalled`, it also gets the stall reason if `fail_ping` is set; with `shutdown`, `stop` is set
  to the stall reason. Before its first judgment a check counts as not stalled.

- [ ] **Step 1: Write the failing tests** (helpers `write(dir, rel, text)` and `state(at, failure, msg)`)

```rust
#[test]
fn declarations_are_cached_once_valid() {
  // Unreadable: logged once, retried. Valid: cached; replacing the file afterwards changes nothing.
}
#[test]
fn the_deadline_alerts_and_stops_only_when_nothing_was_declared() {
  // timeout 300, started 14:00: poll at 14:04:59 → Verdict::default(); 14:05:00 →
  // alerts == stop == ["health checks not declared within 300 s"]. With checks.json present
  // (even with an empty table, Review Focus 4) at 14:06 → no alert, no stop.
}
#[test]
fn affects_ping_withholds_while_failing_or_stalled() {}
#[test]
fn failure_pings_alert_once_per_ping_whatever_the_flags() {
  // Review Focus 5: same `at` on the next poll → no alert; new `at` → alert again.
}
#[test]
fn fail_ping_and_shutdown_react_to_each_stall_episode() {
  // Stall → alert (fail_ping) / stop (shutdown); still stalled next poll → nothing;
  // recover, stall again → alert/stop again (Review Focus 5). A check without either flag stalls silently.
}
```

Each body asserts exact `Verdict` values with the strings from Task 1's judging test.

- [ ] **Step 2: Run to verify they fail.** Run: `cargo test checks::tests`. Expected: FAIL.

- [ ] **Step 3: Implement `Watch`** with per-check maps (`BTreeMap<String, bool>` for stalled,
`BTreeMap<String, jiff::Timestamp>` for the last alerted failure) next to the cached
`Option<Declarations>`.

- [ ] **Step 4: Run to verify they pass.** Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/checks.rs
git commit -m "feat(checks): track declared checks across supervisor polls"
```

### Task 7: The supervisor acts on the verdict; README

**Files:**

- Modify: `src/supervisor.rs` (`Exit`, the poll block `:640-665`, the exit arms `:666-711`), `README.md`

**Interfaces:**

- Consumes: Task 4 (`Plan::health_timeout_secs`), Task 5, Task 6.
- Produces: `Exit::Health { reason: String, alerts: Vec<String> }`.

- [ ] **Step 1: Wire it**

At `run`'s start, build `checks::Watch::new(health_timeout_secs, jiff::Timestamp::now())`. In the
poll block, while `!stopping`, take `watch.poll(&checks::dir(&app_root), now)` (else
`Verdict::default()`, so a stopping app's checks change nothing); log each `verdict.log` line.

- A `stop`: `exit = Some(Exit::Health { reason, alerts })`.
- Otherwise: `kind = pinger.decide(reasons.is_empty(), verdict.withheld)`. The `/fail` body is
  the core `reasons` when `kind` is `Fail`, plus `verdict.alerts`. If that body is non-empty, send
  one `Kind::Fail` with the lines joined by `\n`; it replaces the plain ping. Otherwise send
  `kind`, as today.

`Exit::Health` arm: `log.line("health: {reason}; stopping the app")`; `send_now` the alerts when
non-empty, else join any in-flight ping; `stop_all(&mut live, Signal::SIGTERM)`; `down()`;
`return Ok(1)`. That returns before the generic exit-code ping.

- [ ] **Step 2: README.** Document the changes at these lines:
  - the `run` bullet (`:31-33`): the health folder reset, the deadline file;
  - the poll and ping rules (`:104-111`): withheld, alerts, the two stops;
  - `healthcheck` (`:112-119`);
  - a `[tool.docker]` row for `health_checks_timeout_secs` (`:123-133`);
  - "Heartbeat files" (`:170-184`): `health/checks.json`, `health/state/<name>.json`, `health/declare-by.txt`.

- [ ] **Step 3: Verify the whole crate**

Run: `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.
Expected: all clean. If Docker is available, also
`cargo test --test docker_supervisor -- --ignored --nocapture` (it needs the wireguard module;
report rather than fake it if it can't run).

- [ ] **Step 4: Commit**

```bash
git add src/supervisor.rs README.md
git commit -m "feat(supervisor): act on declared health checks and the declarations deadline"
```

### Task 8: PR and two Copilot review rounds

- [ ] **Step 1:** `git push -u origin feat/app-health-checks`, then `gh pr create` with a body
  covering what, why (link the spec) and how it was tested, ending with
  `🤖 Generated with [Claude Code](https://claude.com/claude-code)`.
- [ ] **Step 2:** Request a Copilot review (GitHub MCP `request_copilot_review`). When it lands,
  verify each comment against the code (superpowers:receiving-code-review), fix what's real,
  answer what isn't, and push.
- [ ] **Step 3:** Repeat Step 2 once more (round two).
- [ ] **Step 4:** Stop. The PR stays unmerged for the user.
