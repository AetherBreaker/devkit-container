# Container Supervisor, Wireguard Mode and Heartbeat Healthcheck Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give `devkit-container` the `supervise` and `wireguard` switches, a supervising entrypoint, the WireGuard tunnel, the `healthcheck` subcommand over heartbeat files, the healthchecks.io ping, the compose template as package data, and the gated Dockerfile block; then release it.

**Architecture:** `run` keeps its checks and grows one branch: exec (today) or supervise. Four new modules: `heartbeat` (timestamp files: parse, freshness, atomic write), `healthcheck` (the subcommand), `ping` (URL building; the request goes through the venv's Python as uid 999), `wireguard` (shell-outs to `ip`/`wg`, a pure stale-and-re-up state machine), `supervisor` (spawn as 999, reap, forward signals, the poll loop that ties the other three together). The compose template joins `template.Dockerfile` in the `devkit_container` package; both carry `# !` gates on `keys("tool.docker.wireguard")` and the compose one carries `# !rule` annotations.

**Tech Stack:** Rust 2024, `clap`, `anyhow`, `toml_edit`, `nix` 0.31 (`user`, `signal`, `process`), `signal-hook` 0.4, `jiff` 0.2 (`tzdb-bundle-always`); Docker for the smoke tests; `wireguard-tools` and `iproute2` inside the image.

**Spec:** `docs/superpowers/specs/2026-09-08-container-wireguard-mode-design.md`, sections 3 (the package-data half), 4 to 9 (this repo's parts), 10 (step 2), 11, 12 (this repo's paragraphs), 13, 14. Depends on the template-language plan's releases (aeth-devkit 15.0.0, devkit-templates 1.2.0) for the render check in Task 9 only; every other task builds and tests without them.

## Global Constraints

- Run Python and tooling under `uv run`; `uv add`/`uv remove`/`uv lock` are refused by the project hook (use `uv sync`, `poe lock`).
- `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` run on Windows and Linux in CI: every Unix-only item is behind `#[cfg(unix)]` and the Windows build stays warning-free (see the `cfg_attr(not(unix), allow(dead_code))` pattern in `main.rs`).
- The smoke wheel is a static `x86_64-unknown-linux-musl` build cross-compiled from Windows with `rust-lld`: no crate that needs a C compiler (no `ring`, no `aws-lc`, no `openssl`). `jiff`, `signal-hook` and `nix` are pure Rust.
- Secrets never appear in argv, logs or error messages: `WG_PRIVATE_KEY`, `WG_PEER_PRESHARED_KEY`, `PINGKEY`, and any ping URL. Errors name the variable, never the value (spec 5).
- Defaults, exactly (spec 6, 7): keepalive 25, handshake timeout 60 s, poll 30 s, stale 180 s, heartbeat max age 180 s; heartbeat files `/app/persisted_data/logs/heartbeat.txt` and `/app/persisted_data/logs/wireguard-heartbeat.txt` (relative to `--app-root` in tests).
- Environment names, exactly: `WG_PRIVATE_KEY`, `WG_ADDRESS`, `WG_PEER_PUBLIC_KEY`, `WG_PEER_ENDPOINT`, `WG_PEER_ALLOWED_IPS`, `WG_PEER_PRESHARED_KEY`, `WG_PERSISTENT_KEEPALIVE`, `WG_HANDSHAKE_TIMEOUT_SECS`, `WG_POLL_SECS`, `WG_STALE_SECS`, `HEARTBEAT_SLUG`, `PINGKEY`, `ALERTS_HEALTHCHECK_PING_URL`, `DEVKIT_SUPERVISED_PING`. Empty is unset.
- Commit messages: Conventional Commits with `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`.
- Targeted tests while iterating; the full suite (`cargo test`, then the ignored smoke tests) once at the end (AGENTS.md).

---

### Task 1: Branch, dependencies, the switches in `pyproject.rs`

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/pyproject.rs`

**Interfaces:**
- Produces: `pyproject::supervise(&DocumentMut) -> Result<bool>`, `pyproject::wireguard(&DocumentMut) -> Result<bool>`, `pyproject::services(&DocumentMut) -> Vec<String>`.

- [ ] **Step 1: Branch and add the dependencies**

```bash
git switch -c supervisor-wireguard
```

In `Cargo.toml`:

```toml
[dependencies]
  anyhow    = "1.0.104"
  clap      = { version = "4", features = ["derive"] }
  jiff      = { version = "0.2", features = ["tzdb-bundle-always"] }
  toml_edit = "0.25.13"

# `nix` and `signal-hook` have no Windows build at all, so they must not even be resolved there.
[target.'cfg(unix)'.dependencies]
  nix         = { version = "0.31.3", features = ["user", "signal", "process"] }
  signal-hook = "0.4"
```

Run: `cargo build`
Expected: `Finished` (on Windows `nix`/`signal-hook` are skipped).

- [ ] **Step 2: Write the failing tests**

In `src/pyproject.rs`'s test module add:

```rust
  #[test]
  fn the_switches_are_booleans_off_by_default() {
    let d = doc("[tool.docker]\nservices = [\"a\", \"b\"]\n");
    assert!(!supervise(&d).unwrap() && !wireguard(&d).unwrap());
    assert_eq!(services(&d), ["a", "b"]);
    let d = doc("[tool.docker]\nsupervise = true\nwireguard = true\n");
    assert!(supervise(&d).unwrap() && wireguard(&d).unwrap());
    assert!(services(&d).is_empty());
    assert!(supervise(&doc("[project]\n")).unwrap() == false);
    for bad in ["supervise = \"yes\"", "wireguard = 1"] {
      let d = doc(&format!("[tool.docker]\n{bad}\n"));
      let err = supervise(&d).and(wireguard(&d)).unwrap_err().to_string();
      assert!(err.contains("must be true or false"), "{bad}: {err}");
    }
  }
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test pyproject::tests::the_switches`
Expected: compile error, functions not found.

- [ ] **Step 4: Write the implementation**

Add to `src/pyproject.rs`:

```rust
/// `[tool.docker].<key>` as a boolean, `false` when absent; anything else is an error naming
/// the key, so a misspelt value cannot read as "off".
fn docker_flag(doc: &DocumentMut, key: &str) -> Result<bool> {
  match doc.get("tool").and_then(|t| t.get("docker")).and_then(|d| d.get(key)) {
    None => Ok(false),
    Some(item) => item
      .as_bool()
      .with_context(|| format!("[tool.docker].{key} must be true or false, got {}", item.to_string().trim())),
  }
}

/// `[tool.docker].supervise`: spawn and supervise the app instead of exec'ing it (spec 4).
pub fn supervise(doc: &DocumentMut) -> Result<bool> {
  docker_flag(doc, "supervise")
}

/// `[tool.docker].wireguard`: bring up the tunnel before the app; implies `supervise`.
pub fn wireguard(doc: &DocumentMut) -> Result<bool> {
  docker_flag(doc, "wireguard")
}

/// `[tool.docker].services` as strings (the slug fallback in `ping`); empty when absent or
/// malformed, since the binary does not own that key's validation.
pub fn services(doc: &DocumentMut) -> Vec<String> {
  doc
    .get("tool")
    .and_then(|t| t.get("docker"))
    .and_then(|d| d.get("services"))
    .and_then(|s| s.as_array())
    .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
    .unwrap_or_default()
}
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test pyproject`
Expected: all `pyproject` tests pass.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/pyproject.rs
git commit -m "feat(pyproject): read the supervise and wireguard switches"
```

---

### Task 2: Heartbeat files (`heartbeat.rs`)

**Files:**
- Create: `src/heartbeat.rs`
- Modify: `src/main.rs` (add `mod heartbeat;`)

**Interfaces:**
- Produces:
  - `pub const DEFAULT_MAX_AGE_SECS: u64 = 180`
  - `pub const APP_FILE: &str = "heartbeat.txt"`, `pub const TUNNEL_FILE: &str = "wireguard-heartbeat.txt"`, `pub fn logs_dir(app_root: &Path) -> PathBuf` (`app_root/persisted_data/logs`)
  - `pub fn parse(text: &str) -> Result<jiff::Timestamp>`
  - `pub fn check(path: &Path, max_age: u64, now: jiff::Timestamp) -> Result<(), String>` (`Err` carries the one-line reason)
  - `pub fn write(path: &Path, now: jiff::Timestamp) -> Result<()>`

- [ ] **Step 1: Write the failing tests**

Create `src/heartbeat.rs`:

```rust
//! Heartbeat files (spec 7): one timestamp, written by a process while it is healthy, fresh
//! while younger than the max age. The app's is written by `aeth_ext`, the tunnel's by the
//! supervisor; `healthcheck` and the supervisor's ping both read them the same way.

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
    assert_eq!(parse("2026-09-10T12:00:00.123456+00:00").unwrap(), at("2026-09-10T12:00:00.123456Z"));
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
    assert_eq!(parse("  2026-09-10T12:00:00Z\n").unwrap(), at("2026-09-10T12:00:00Z"), "surrounding whitespace");
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
    assert_eq!(check(&p, 180, now), Ok(()), "exactly 180 s is fresh? no: 180 is not younger than 180");
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
```

Fix the first `check` test's last assertion to what the contract says (the comment is a reminder, the assertion must be right): replace it with

```rust
    std::fs::write(&p, "2026-09-10T12:00:00Z").unwrap();
    assert!(check(&p, 180, now).unwrap_err().contains("stale by 180 s"));
```

Add `mod heartbeat;` to `main.rs`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test heartbeat`
Expected: compile errors, functions not found.

- [ ] **Step 3: Write the implementation**

Above the tests:

```rust
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
  let civil: jiff::civil::DateTime = t
    .parse()
    .map_err(|e| anyhow!("not an ISO 8601 timestamp: {e}"))?;
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
```

(`jiff::Timestamp`'s `Display` is RFC 3339 with `Z`; if the assertion on the written text fails because of sub-second digits, format with `now.strftime("%Y-%m-%dT%H:%M:%SZ")` instead: the file's contract is "an ISO 8601 timestamp", not a byte-exact form.)

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test heartbeat`
Expected: 4 passed.

- [ ] **Step 5: Commit**

```bash
git add src/heartbeat.rs src/main.rs
git commit -m "feat(heartbeat): parse, check and write heartbeat timestamp files"
```

---

### Task 3: The `healthcheck` subcommand

**Files:**
- Create: `src/healthcheck.rs`
- Modify: `src/main.rs`
- Modify: `tests/entrypoint.rs`

**Interfaces:**
- Consumes: `heartbeat::{check, logs_dir, APP_FILE, DEFAULT_MAX_AGE_SECS}`.
- Produces: `healthcheck::run(files: &[PathBuf], max_age: u64) -> u8` (exit code; reasons on stderr); the CLI `devkit-container healthcheck [--file PATH]... [--max-age SECS] [--app-root DIR]`.

- [ ] **Step 1: Write the failing binary test**

In `tests/entrypoint.rs` add:

```rust
#[test]
fn healthcheck_reads_files_only_and_says_why_it_fails() {
  let dir = tempfile::tempdir().unwrap();
  let root = dir.path();
  let logs = root.join("persisted_data").join("logs");
  std::fs::create_dir_all(&logs).unwrap();
  // Default: the app's file under --app-root, max age 180.
  let out = bin().args(["healthcheck", "--app-root"]).arg(root).output().unwrap();
  assert_eq!(out.status.code(), Some(1));
  let err = String::from_utf8_lossy(&out.stderr);
  assert!(err.contains("heartbeat.txt") && err.contains("missing"), "{err}");
  let now = jiff::Timestamp::now();
  std::fs::write(logs.join("heartbeat.txt"), now.to_string()).unwrap();
  let out = bin().args(["healthcheck", "--app-root"]).arg(root).output().unwrap();
  assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
  // Two files, one stale: exit 1, one reason line, naming the stale one only.
  let old = now.checked_sub(jiff::Span::new().seconds(400)).unwrap();
  std::fs::write(logs.join("wireguard-heartbeat.txt"), old.to_string()).unwrap();
  let out = bin()
    .args(["healthcheck", "--file"])
    .arg(logs.join("heartbeat.txt"))
    .arg("--file")
    .arg(logs.join("wireguard-heartbeat.txt"))
    .output()
    .unwrap();
  assert_eq!(out.status.code(), Some(1));
  let err = String::from_utf8_lossy(&out.stderr);
  assert_eq!(err.lines().count(), 1, "{err}");
  assert!(err.contains("wireguard-heartbeat.txt") && err.contains("stale by"), "{err}");
  // --max-age widens it.
  let out = bin()
    .args(["healthcheck", "--max-age", "1000", "--file"])
    .arg(logs.join("wireguard-heartbeat.txt"))
    .output()
    .unwrap();
  assert!(out.status.success());
}
```

Add `jiff` to `[dev-dependencies]` in `Cargo.toml` (`jiff = "0.2"`; it is already a dependency, so this only makes it visible to the test crate). Add `use std::path::PathBuf;` where the test file needs it (it already imports `Path`).

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --test entrypoint healthcheck`
Expected: FAIL, clap reports an unrecognised subcommand (exit 2).

- [ ] **Step 3: Write the implementation**

Create `src/healthcheck.rs`:

```rust
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
```

In `main.rs` add `mod healthcheck;` and the subcommand:

```rust
  /// Exit 0 when every heartbeat file is fresh, else 1 with a reason per file on stderr.
  Healthcheck {
    /// A heartbeat file to check; repeatable. Default: the app's, under --app-root.
    #[arg(long = "file")]
    files: Vec<PathBuf>,
    /// Seconds a timestamp may be old before it counts as stale.
    #[arg(long, default_value_t = heartbeat::DEFAULT_MAX_AGE_SECS)]
    max_age: u64,
    #[arg(long, default_value = "/app")]
    app_root: PathBuf,
  },
```

and in `main`'s match, before the `Run` arms:

```rust
    Command::Healthcheck { files, max_age, app_root } => {
      let files = if files.is_empty() {
        vec![heartbeat::logs_dir(&app_root).join(heartbeat::APP_FILE)]
      } else {
        files
      };
      return ExitCode::from(healthcheck::run(&files, max_age));
    }
```

(The `match` currently binds `result`; an early `return` from inside the match arm is the least churn. Keep the `Run` arms as they are for now; Task 8 changes them.)

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --test entrypoint healthcheck`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/healthcheck.rs src/main.rs tests/entrypoint.rs Cargo.toml Cargo.lock
git commit -m "feat(healthcheck): the subcommand that replaces the compose shell one-liner"
```

---

### Task 4: The ping (`ping.rs`)

**Files:**
- Create: `src/ping.rs`
- Modify: `src/main.rs` (add `mod ping;`)

**Interfaces:**
- Produces:
  - `pub enum Kind { Start, Plain, Fail }`
  - `pub struct Ping { … }` with `pub fn configure(url: Option<&str>, pingkey: Option<&str>, slug: Option<&str>) -> Option<Ping>` and `pub fn url(&self, kind: Kind) -> String`
  - `pub fn slug(env_slug: Option<&str>, services: &[String]) -> Option<String>`
  - `#[cfg(unix)] pub fn send(python: &Path, url: &str, body: &str, as_uid: Option<u32>) -> std::io::Result<std::process::Child>` (spawns; the caller reaps)
  - `pub const PYTHON_SNIPPET: &str` (the `-c` program)

- [ ] **Step 1: Write the failing tests**

Create `src/ping.rs`:

```rust
//! The healthchecks.io ping, in `aeth_ext.monitoring.ping`'s exact shape (spec 7): a fixed
//! URL, else `https://hc-ping.com/<PINGKEY>/<HEARTBEAT_SLUG>` with `?create=1`; `/start`
//! once, plain while healthy, `/fail` with a body on a stale transition or a bad exit. The
//! request itself goes through the venv's Python as uid 999 with the URL on stdin, so no TLS
//! stack enters the root process and the URL's secret never reaches argv.

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_fixed_url_wins_and_never_autoprovisions() {
    let p = configure(Some("https://hc-ping.com/fixed-uuid"), Some("key"), Some("app")).unwrap();
    assert_eq!(p.url(Kind::Plain), "https://hc-ping.com/fixed-uuid");
    assert_eq!(p.url(Kind::Start), "https://hc-ping.com/fixed-uuid/start");
    assert_eq!(p.url(Kind::Fail), "https://hc-ping.com/fixed-uuid/fail");
  }

  #[test]
  fn key_and_slug_build_the_autoprovisioning_url() {
    let p = configure(None, Some("my-key"), Some("my-app")).unwrap();
    assert_eq!(p.url(Kind::Plain), "https://hc-ping.com/my-key/my-app?create=1");
    assert_eq!(p.url(Kind::Start), "https://hc-ping.com/my-key/my-app/start?create=1");
    assert_eq!(p.url(Kind::Fail), "https://hc-ping.com/my-key/my-app/fail?create=1");
  }

  #[test]
  fn nothing_pings_without_a_url_or_a_key_with_a_slug_and_empty_is_unset() {
    assert!(configure(None, None, Some("app")).is_none());
    assert!(configure(None, Some("key"), None).is_none());
    assert!(configure(Some(""), Some(""), Some("app")).is_none());
    assert!(configure(None, Some("key"), Some("")).is_none());
    assert!(configure(Some("  "), None, None).is_none());
  }

  #[test]
  fn the_slug_is_the_environment_else_the_single_service() {
    assert_eq!(slug(Some("svc"), &["a".into(), "b".into()]).as_deref(), Some("svc"));
    assert_eq!(slug(None, &["only".into()]).as_deref(), Some("only"));
    assert_eq!(slug(Some(""), &["only".into()]).as_deref(), Some("only"));
    assert_eq!(slug(None, &["a".into(), "b".into()]), None, "two services: no guessing");
    assert_eq!(slug(None, &[]), None);
  }

  #[test]
  fn the_python_snippet_reads_the_url_and_body_from_stdin() {
    assert!(PYTHON_SNIPPET.contains("sys.stdin.readline()") && PYTHON_SNIPPET.contains("timeout=10"));
    assert!(!PYTHON_SNIPPET.contains("argv"), "the URL must never be an argument");
  }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test ping`
Expected: compile errors.

- [ ] **Step 3: Write the implementation**

Above the tests:

```rust
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
  Start,
  Plain,
  Fail,
}

#[derive(Debug, Clone)]
pub struct Ping {
  base: String,
  autoprovision: bool,
}

fn set(v: Option<&str>) -> Option<&str> {
  v.map(str::trim).filter(|s| !s.is_empty())
}

impl Ping {
  /// A fixed URL, else key and slug; `None` when neither is configured (empty is unset).
  pub fn configure(url: Option<&str>, pingkey: Option<&str>, slug: Option<&str>) -> Option<Ping> {
    if let Some(url) = set(url) {
      return Some(Ping {
        base: url.to_string(),
        autoprovision: false,
      });
    }
    match (set(pingkey), set(slug)) {
      (Some(key), Some(slug)) => Some(Ping {
        base: format!("https://hc-ping.com/{key}/{slug}"),
        autoprovision: true,
      }),
      _ => None,
    }
  }

  pub fn url(&self, kind: Kind) -> String {
    let suffix = match kind {
      Kind::Start => "/start",
      Kind::Plain => "",
      Kind::Fail => "/fail",
    };
    let query = if self.autoprovision { "?create=1" } else { "" };
    format!("{}{suffix}{query}", self.base)
  }
}

/// `HEARTBEAT_SLUG`, else the single `[tool.docker].services` entry; two services would be
/// two containers pinging one check, so that case pings nothing (spec 7).
pub fn slug(env_slug: Option<&str>, services: &[String]) -> Option<String> {
  if let Some(s) = set(env_slug) {
    return Some(s.to_string());
  }
  match services {
    [one] => Some(one.clone()),
    _ => None,
  }
}

/// The request, as `aeth_ext` makes it: `urlopen` with a 10 s timeout, GET without a body,
/// POST with one. The URL is the first stdin line; the rest of stdin is the body.
pub const PYTHON_SNIPPET: &str = "import sys, urllib.request\n\
url = sys.stdin.readline().strip()\n\
body = sys.stdin.read().encode()\n\
req = urllib.request.Request(url, data=body or None, method='POST' if body else 'GET')\n\
urllib.request.urlopen(req, timeout=10).close()\n";

/// Spawn the request through `python` (the venv's), as `as_uid` when the caller is root.
/// The caller reaps the child; a nonzero exit is a failed ping, logged and never fatal.
#[cfg(unix)]
pub fn send(python: &Path, url: &str, body: &str, as_uid: Option<u32>) -> std::io::Result<std::process::Child> {
  use std::io::Write as _;
  use std::os::unix::process::CommandExt as _;
  use std::process::{Command, Stdio};
  let mut cmd = Command::new(python);
  cmd.arg("-c").arg(PYTHON_SNIPPET).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null());
  if let Some(uid) = as_uid {
    cmd.uid(uid).gid(uid);
  }
  let mut child = cmd.spawn()?;
  if let Some(mut stdin) = child.stdin.take() {
    // A short write; a closed pipe (python died at once) is reported by the exit status.
    let _ = writeln!(stdin, "{url}");
    let _ = stdin.write_all(body.as_bytes());
  }
  Ok(child)
}
```

(`Command::uid`/`gid` set the child's ids before exec and clear supplementary groups the way `setgroups([])` does when the caller is root; the smoke test's `no_extra_groups` check covers the app child, which the supervisor spawns the same way.)

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test ping`
Expected: 5 passed (on Windows too: `send` is `cfg(unix)`; add `#[cfg_attr(not(unix), allow(dead_code))]` on the module declaration in `main.rs` if the Windows build warns about unused items).

- [ ] **Step 5: Commit**

```bash
git add src/ping.rs src/main.rs
git commit -m "feat(ping): healthchecks.io URLs in aeth_ext's shape, sent through the venv's Python as 999"
```

---

### Task 5: The tunnel (`wireguard.rs`)

**Files:**
- Create: `src/wireguard.rs`
- Modify: `src/main.rs` (add `#[cfg(unix)] mod wireguard;` … the pure parts are cross-platform; see below)

**Interfaces:**
- Produces:
  - `pub struct Config { pub private_key: String, pub address: String, pub peer_public_key: String, pub peer_endpoint: String, pub peer_allowed_ips: Vec<String>, pub peer_preshared_key: Option<String>, pub keepalive: u32, pub handshake_timeout_secs: u64, pub poll_secs: u64, pub stale_secs: u64 }` with `pub fn from_env(get: &dyn Fn(&str) -> Option<String>) -> Result<Config>`
  - `pub const SECRET_VARS: [&str; 2] = ["WG_PRIVATE_KEY", "WG_PEER_PRESHARED_KEY"]`
  - `pub struct Assessor { … }` with `pub fn new(stale_secs: u64) -> Assessor` and `pub fn assess(&mut self, now: u64, latest_handshake: Option<u64>) -> (bool, Action)`; `pub enum Action { Nothing, ResetEndpoint, DownUp }`
  - `pub fn parse_latest_handshake(wg_show: &str, peer_public_key: &str) -> Option<u64>`
  - `#[cfg(unix)] pub struct Tunnel { cfg: Config }` with `pub fn start(cfg: Config) -> Result<Tunnel>` (preflight, bring-up, public key logged, first handshake), `pub fn latest_handshake(&self) -> Result<Option<u64>>`, `pub fn act(&self, action: Action) -> Result<()>`, `pub fn down(&self)`

- [ ] **Step 1: Write the failing tests (pure parts)**

Create `src/wireguard.rs`:

```rust
//! Wireguard mode (spec 6): the environment contract, `ip` + `wg set` bring-up with the keys
//! over stdin, the first-handshake wait, and the stale-and-re-up rule. The rule is a pure
//! state machine (`Assessor`) so it is tested with an injected clock; the shell-outs are
//! `Tunnel`, exercised by the smoke test.

#[cfg(test)]
mod tests {
  use super::*;
  use std::collections::HashMap;

  fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
  }

  const REQUIRED: [(&str, &str); 5] = [
    ("WG_PRIVATE_KEY", "priv"),
    ("WG_ADDRESS", "10.8.0.20/32"),
    ("WG_PEER_PUBLIC_KEY", "pub"),
    ("WG_PEER_ENDPOINT", "hub:51820"),
    ("WG_PEER_ALLOWED_IPS", "10.8.0.0/24, 192.168.1.0/24"),
  ];

  #[test]
  fn the_environment_contract_with_its_defaults() {
    let e = env(&REQUIRED);
    let c = Config::from_env(&|k| e.get(k).cloned()).unwrap();
    assert_eq!(c.peer_allowed_ips, ["10.8.0.0/24", "192.168.1.0/24"]);
    assert_eq!((c.keepalive, c.handshake_timeout_secs, c.poll_secs, c.stale_secs), (25, 60, 30, 180));
    assert!(c.peer_preshared_key.is_none());
    let mut e = env(&REQUIRED);
    e.insert("WG_PEER_PRESHARED_KEY".into(), "psk".into());
    e.insert("WG_PERSISTENT_KEEPALIVE".into(), "10".into());
    e.insert("WG_POLL_SECS".into(), "1".into());
    e.insert("WG_STALE_SECS".into(), "".into()); // empty is unset
    let c = Config::from_env(&|k| e.get(k).cloned()).unwrap();
    assert_eq!(c.peer_preshared_key.as_deref(), Some("psk"));
    assert_eq!((c.keepalive, c.poll_secs, c.stale_secs), (10, 1, 180));
  }

  #[test]
  fn a_missing_or_bad_variable_is_named_never_its_value() {
    for (missing, _) in REQUIRED {
      let e: HashMap<String, String> = REQUIRED.iter().filter(|(k, _)| *k != missing).map(|(k, v)| (k.to_string(), v.to_string())).collect();
      let err = Config::from_env(&|k| e.get(k).cloned()).unwrap_err().to_string();
      assert!(err.contains(missing), "{missing}: {err}");
      assert!(!err.contains("priv"), "{err}");
    }
    let mut e = env(&REQUIRED);
    e.insert("WG_PERSISTENT_KEEPALIVE".into(), "0".into());
    let err = Config::from_env(&|k| e.get(k).cloned()).unwrap_err().to_string();
    assert!(err.contains("WG_PERSISTENT_KEEPALIVE") && err.contains("nonzero"), "{err}");
    e.insert("WG_PERSISTENT_KEEPALIVE".into(), "soon".into());
    let err = Config::from_env(&|k| e.get(k).cloned()).unwrap_err().to_string();
    assert!(err.contains("WG_PERSISTENT_KEEPALIVE") && !err.contains("soon"), "{err}");
  }

  #[test]
  fn stale_resets_the_endpoint_first_then_cycles_the_interface() {
    let mut a = Assessor::new(180);
    assert_eq!(a.assess(1000, Some(900)), (true, Action::Nothing));
    assert_eq!(a.assess(1179, Some(1000)), (true, Action::Nothing), "179 s old is fresh");
    assert_eq!(a.assess(1180, Some(1000)), (false, Action::ResetEndpoint), "180 s old is stale");
    assert_eq!(a.assess(1210, Some(1000)), (false, Action::DownUp));
    assert_eq!(a.assess(1240, Some(1000)), (false, Action::DownUp));
    assert_eq!(a.assess(1270, Some(1265)), (true, Action::Nothing), "recovered");
    assert_eq!(a.assess(1500, Some(1265)), (false, Action::ResetEndpoint), "the next outage starts over");
    assert_eq!(Assessor::new(180).assess(5, None), (false, Action::ResetEndpoint), "never handshaken is stale");
  }

  #[test]
  fn the_latest_handshake_is_read_from_wg_show() {
    let out = "otherpub=\t1757505000\npubkey=\t1757505600\n";
    assert_eq!(parse_latest_handshake(out, "pubkey="), Some(1757505600));
    assert_eq!(parse_latest_handshake("pubkey=\t0\n", "pubkey="), None, "0 means never");
    assert_eq!(parse_latest_handshake(out, "missing"), None);
  }
}
```

Add `mod wireguard;` to `main.rs` with `#[cfg_attr(not(unix), allow(dead_code))]`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test wireguard`
Expected: compile errors.

- [ ] **Step 3: Write the pure implementation**

Above the tests:

```rust
use anyhow::{Context as _, Result, bail};

pub const SECRET_VARS: [&str; 2] = ["WG_PRIVATE_KEY", "WG_PEER_PRESHARED_KEY"];

#[derive(Debug, Clone)]
pub struct Config {
  pub private_key: String,
  pub address: String,
  pub peer_public_key: String,
  pub peer_endpoint: String,
  pub peer_allowed_ips: Vec<String>,
  pub peer_preshared_key: Option<String>,
  pub keepalive: u32,
  pub handshake_timeout_secs: u64,
  pub poll_secs: u64,
  pub stale_secs: u64,
}

impl Config {
  /// The `WG_*` contract (spec 6). `get` is the environment, injected for tests. Empty is
  /// unset. A failure names the variable and never echoes a value.
  pub fn from_env(get: &dyn Fn(&str) -> Option<String>) -> Result<Config> {
    let var = |k: &str| get(k).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    let required = |k: &str| var(k).with_context(|| format!("{k} is not set; the wireguard mode needs it"));
    let number = |k: &str, default: u64| -> Result<u64> {
      match var(k) {
        None => Ok(default),
        Some(v) => v.parse().map_err(|_| anyhow::anyhow!("{k} must be a whole number of seconds")),
      }
    };
    let keepalive: u32 = match var("WG_PERSISTENT_KEEPALIVE") {
      None => 25,
      Some(v) => v.parse().map_err(|_| anyhow::anyhow!("WG_PERSISTENT_KEEPALIVE must be a whole number of seconds"))?,
    };
    if keepalive == 0 {
      bail!("WG_PERSISTENT_KEEPALIVE must be nonzero: without a keepalive an idle tunnel never re-handshakes and would read as stale");
    }
    Ok(Config {
      private_key: required("WG_PRIVATE_KEY")?,
      address: required("WG_ADDRESS")?,
      peer_public_key: required("WG_PEER_PUBLIC_KEY")?,
      peer_endpoint: required("WG_PEER_ENDPOINT")?,
      peer_allowed_ips: required("WG_PEER_ALLOWED_IPS")?
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect(),
      peer_preshared_key: var("WG_PEER_PRESHARED_KEY"),
      keepalive,
      handshake_timeout_secs: number("WG_HANDSHAKE_TIMEOUT_SECS", 60)?,
      poll_secs: number("WG_POLL_SECS", 30)?,
      stale_secs: number("WG_STALE_SECS", 180)?,
    })
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
  Nothing,
  /// Re-set the peer endpoint so its name is resolved again (the hub may have moved).
  ResetEndpoint,
  /// The interface down and up again: the second and every later response to a stale poll.
  DownUp,
}

/// The stale rule: a handshake older than `stale_secs` (or none) is stale; the first stale
/// poll resets the endpoint, every following one cycles the interface, and a fresh poll
/// starts the sequence over.
#[derive(Debug)]
pub struct Assessor {
  stale_secs: u64,
  stale_polls: u32,
}

impl Assessor {
  pub fn new(stale_secs: u64) -> Assessor {
    Assessor { stale_secs, stale_polls: 0 }
  }

  /// `(fresh, what to do)`.
  pub fn assess(&mut self, now: u64, latest_handshake: Option<u64>) -> (bool, Action) {
    let fresh = latest_handshake.is_some_and(|h| now.saturating_sub(h) < self.stale_secs);
    if fresh {
      self.stale_polls = 0;
      return (true, Action::Nothing);
    }
    self.stale_polls += 1;
    (false, if self.stale_polls == 1 { Action::ResetEndpoint } else { Action::DownUp })
  }
}

/// `wg show wg0 latest-handshakes` prints `<peer public key>\t<epoch seconds>`; 0 is never.
pub fn parse_latest_handshake(wg_show: &str, peer_public_key: &str) -> Option<u64> {
  wg_show
    .lines()
    .filter_map(|l| l.split_once('\t'))
    .find(|(k, _)| k.trim() == peer_public_key)
    .and_then(|(_, v)| v.trim().parse::<u64>().ok())
    .filter(|&epoch| epoch != 0)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test wireguard`
Expected: 4 passed.

- [ ] **Step 5: Write the shell-out half**

Append, below the pure parts:

```rust
#[cfg(unix)]
pub use unix::Tunnel;

#[cfg(unix)]
mod unix {
  use std::io::Write as _;
  use std::process::{Command, Stdio};

  use anyhow::{Context as _, Result, bail};

  use super::{Action, Config, parse_latest_handshake};

  pub const IFACE: &str = "wg0";

  /// The interface, up. Dropping it does not tear it down: the supervisor decides when.
  pub struct Tunnel {
    cfg: Config,
  }

  /// Run a command, feeding `stdin` if given; an error names the command and its stderr.
  fn run(program: &str, args: &[&str], stdin: Option<&str>) -> Result<String> {
    let mut cmd = Command::new(program);
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() });
    let mut child = cmd
      .spawn()
      .with_context(|| format!("running {program} {}", args.join(" ")))?;
    if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
      // Written and closed before waiting, or `wg` blocks reading a key that never ends.
      pipe.write_all(text.as_bytes()).context("writing to stdin")?;
      pipe.write_all(b"\n").context("writing to stdin")?;
    }
    let out = child.wait_with_output().with_context(|| format!("waiting for {program}"))?;
    if !out.status.success() {
      bail!(
        "{program} {} failed ({}): {}",
        args.join(" "),
        out.status,
        String::from_utf8_lossy(&out.stderr).trim()
      );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
  }

  impl Tunnel {
    /// Preflight, bring-up, the public key logged, the first handshake within the timeout
    /// (spec 6, steps 1 to 3). A refused start leaves no interface behind.
    pub fn start(cfg: Config) -> Result<Tunnel> {
      for tool in ["wg", "ip"] {
        if Command::new(tool).arg("--version").output().is_err() && Command::new(tool).arg("-V").output().is_err() {
          bail!("{tool} is not in the image: it was built without the wireguard block; rerun setup-project with a devkit that knows the `wireguard` switch and rebuild");
        }
      }
      let public = run("wg", &["pubkey"], Some(&cfg.private_key))?;
      eprintln!("devkit-container: wireguard public key {}", public.trim());
      let tunnel = Tunnel { cfg };
      if let Err(e) = tunnel.up() {
        tunnel.down();
        return Err(e);
      }
      let deadline = std::time::Instant::now() + std::time::Duration::from_secs(tunnel.cfg.handshake_timeout_secs);
      loop {
        if tunnel.latest_handshake()?.is_some() {
          eprintln!("devkit-container: wireguard handshake with {}", tunnel.cfg.peer_endpoint);
          return Ok(tunnel);
        }
        if std::time::Instant::now() >= deadline {
          tunnel.down();
          bail!(
            "no wireguard handshake with {} within {} s (WG_HANDSHAKE_TIMEOUT_SECS)",
            tunnel.cfg.peer_endpoint,
            tunnel.cfg.handshake_timeout_secs
          );
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
      }
    }

    fn up(&self) -> Result<()> {
      let c = &self.cfg;
      run("ip", &["link", "add", "dev", IFACE, "type", "wireguard"], None)?;
      run("wg", &["set", IFACE, "private-key", "/dev/stdin"], Some(&c.private_key))?;
      let keepalive = c.keepalive.to_string();
      let allowed = c.peer_allowed_ips.join(",");
      run(
        "wg",
        &[
          "set", IFACE, "peer", &c.peer_public_key, "endpoint", &c.peer_endpoint, "allowed-ips", &allowed, "persistent-keepalive", &keepalive,
        ],
        None,
      )?;
      if let Some(psk) = &c.peer_preshared_key {
        run("wg", &["set", IFACE, "peer", &c.peer_public_key, "preshared-key", "/dev/stdin"], Some(psk))?;
      }
      run("ip", &["address", "add", &c.address, "dev", IFACE], None)?;
      run("ip", &["link", "set", "up", "dev", IFACE], None)?;
      for cidr in &c.peer_allowed_ips {
        // `replace`, not `add`: the kernel may already have added the interface's own subnet.
        run("ip", &["route", "replace", cidr, "dev", IFACE], None)?;
      }
      Ok(())
    }

    pub fn latest_handshake(&self) -> Result<Option<u64>> {
      let out = run("wg", &["show", IFACE, "latest-handshakes"], None)?;
      Ok(parse_latest_handshake(&out, &self.cfg.peer_public_key))
    }

    pub fn act(&self, action: Action) -> Result<()> {
      match action {
        Action::Nothing => Ok(()),
        Action::ResetEndpoint => {
          eprintln!("devkit-container: wireguard stale; re-up: resetting the endpoint {}", self.cfg.peer_endpoint);
          run("wg", &["set", IFACE, "peer", &self.cfg.peer_public_key, "endpoint", &self.cfg.peer_endpoint], None).map(|_| ())
        }
        Action::DownUp => {
          eprintln!("devkit-container: wireguard still stale; re-up: {IFACE} down and up");
          self.down();
          self.up()
        }
      }
    }

    /// Best effort: an interface that is already gone is not an error.
    pub fn down(&self) {
      let _ = Command::new("ip").args(["link", "del", "dev", IFACE]).output();
    }
  }
}
```

Run: `cargo build && cargo clippy --all-targets -- -D warnings`
Expected: clean on this platform (on Windows the `unix` module is compiled out; the pure parts carry the `allow(dead_code)` from the module declaration).

- [ ] **Step 6: Commit**

```bash
git add src/wireguard.rs src/main.rs
git commit -m "feat(wireguard): the WG_* contract, ip + wg bring-up over stdin, and the stale-and-re-up rule"
```

---

### Task 6: The supervisor (`supervisor.rs`)

**Files:**
- Create: `src/supervisor.rs`
- Modify: `src/main.rs` (add `#[cfg(unix)] mod supervisor;`)

**Interfaces:**
- Consumes: `heartbeat::{check, write, logs_dir, APP_FILE, TUNNEL_FILE, DEFAULT_MAX_AGE_SECS}`, `ping::{Ping, Kind, send}`, `wireguard::{Tunnel, Assessor, SECRET_VARS}`, `prepare::NONROOT`.
- Produces: `pub struct Plan { pub exe: PathBuf, pub app_root: PathBuf, pub tunnel: Option<(wireguard::Tunnel, u64 /*stale_secs*/)>, pub poll_secs: u64, pub ping: Option<Ping> }` and `pub fn run(plan: Plan) -> Result<u8>` (the exit code to end the process with).

- [ ] **Step 1: Write the failing unit test for the adjudication**

The supervisor's loop is verified by the smoke test; its one pure piece, the ping decision, gets a unit test. Create `src/supervisor.rs`:

```rust
//! The supervising entrypoint (spec 5): the app spawned as 999 with nothing kept, signals
//! forwarded, zombies reaped, and every `WG_POLL_SECS` the tunnel checked, the tunnel's
//! heartbeat written, every heartbeat file adjudicated and the ping sent. Root only when a
//! tunnel needs re-upping; otherwise it drops to 999 before spawning.

#[cfg(test)]
mod tests {
  use super::*;
  use crate::ping::Kind;

  #[test]
  fn start_once_plain_while_fresh_fail_on_the_transition_then_plain_again() {
    let mut p = Pinger::default();
    assert_eq!(p.decide(false), None, "silent until first fresh");
    assert_eq!(p.decide(false), None);
    assert_eq!(p.decide(true), Some(Kind::Start));
    assert_eq!(p.decide(true), Some(Kind::Plain));
    assert_eq!(p.decide(false), Some(Kind::Fail));
    assert_eq!(p.decide(false), None, "one /fail per outage");
    assert_eq!(p.decide(true), Some(Kind::Plain), "no second /start");
  }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test supervisor`
Expected: compile error, `Pinger` not found. (`Pinger` is pure and lives above the `#[cfg(unix)]` inner module, so this test runs on Windows too; only the loop is Unix-only.)

- [ ] **Step 3: Write the implementation**

```rust
use std::path::PathBuf;

use anyhow::Result;

use crate::ping::{Kind, Ping};

/// When to ping (spec 7): `/start` once when every file is first fresh, plain on every fresh
/// poll after, `/fail` once on the transition to stale, plain again on recovery.
#[derive(Debug, Default)]
pub struct Pinger {
  started: bool,
  last_fresh: Option<bool>,
}

impl Pinger {
  pub fn decide(&mut self, all_fresh: bool) -> Option<Kind> {
    let was = self.last_fresh;
    self.last_fresh = Some(all_fresh);
    if all_fresh {
      if !self.started {
        self.started = true;
        return Some(Kind::Start);
      }
      return Some(Kind::Plain);
    }
    (was == Some(true)).then_some(Kind::Fail)
  }
}

pub struct Plan {
  pub exe: PathBuf,
  pub app_root: PathBuf,
  /// The tunnel and its stale threshold, when the mode is on.
  pub tunnel: Option<(crate::wireguard::Tunnel, u64)>,
  pub poll_secs: u64,
  pub ping: Option<Ping>,
}

#[cfg(unix)]
pub use unix::run;

#[cfg(unix)]
mod unix {
  use std::os::unix::process::CommandExt as _;
  use std::process::{Child, Command};
  use std::sync::Arc;
  use std::sync::atomic::{AtomicBool, Ordering};
  use std::time::{Duration, Instant};

  use anyhow::{Context as _, Result};
  use nix::sys::signal::{Signal, kill};
  use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
  use nix::unistd::{Gid, Pid, Uid, getuid, setgid, setgroups, setuid};

  use super::{Pinger, Plan};
  use crate::heartbeat;
  use crate::ping::{self, Kind};
  use crate::prepare::NONROOT;
  use crate::wireguard::{Assessor, SECRET_VARS};

  fn drop_privileges() -> Result<()> {
    setgroups(&[]).context("setgroups")?;
    setgid(Gid::from_raw(NONROOT)).context("setgid")?;
    setuid(Uid::from_raw(NONROOT)).context("setuid")?;
    Ok(())
  }

  /// Supervise `plan.exe` until it exits; returns the code to exit with (signal death as
  /// 128+n). Never returns while the child runs.
  pub fn run(plan: Plan) -> Result<u8> {
    let root = getuid().is_root();
    let (tunnel, stale_secs) = match plan.tunnel {
      Some((t, s)) => (Some(t), s),
      None => (None, heartbeat::DEFAULT_MAX_AGE_SECS),
    };
    // Without a tunnel nothing later needs root: drop now, and the child inherits 999.
    if tunnel.is_none() && root {
      drop_privileges()?;
    }
    let spawner_is_root = tunnel.is_some() && root;

    let term = Arc::new(AtomicBool::new(false));
    let int = Arc::new(AtomicBool::new(false));
    let hup = Arc::new(AtomicBool::new(false));
    for (sig, flag) in [
      (signal_hook::consts::SIGTERM, &term),
      (signal_hook::consts::SIGINT, &int),
      (signal_hook::consts::SIGHUP, &hup),
    ] {
      signal_hook::flag::register(sig, Arc::clone(flag)).context("installing signal handler")?;
    }

    let mut cmd = Command::new(&plan.exe);
    for var in SECRET_VARS {
      cmd.env_remove(var);
    }
    if plan.ping.is_some() {
      cmd.env("DEVKIT_SUPERVISED_PING", "1");
    }
    if spawner_is_root {
      // SAFETY: only async-signal-safe syscalls between fork and exec.
      unsafe {
        cmd.pre_exec(|| drop_privileges().map_err(|e| std::io::Error::other(e.to_string())));
      }
    }
    let child = cmd.spawn().with_context(|| format!("spawning {}", plan.exe.display()))?;
    let child_pid = Pid::from_raw(child.id() as i32);
    eprintln!("devkit-container: supervising pid {child_pid}");

    let logs = heartbeat::logs_dir(&plan.app_root);
    let app_beat = logs.join(heartbeat::APP_FILE);
    let tunnel_beat = logs.join(heartbeat::TUNNEL_FILE);
    let python = plan.app_root.join(".venv").join("bin").join("python");
    let mut assessor = Assessor::new(stale_secs);
    let mut pinger = Pinger::default();
    let mut pings: Vec<Child> = Vec::new();
    let mut next_poll = Instant::now();
    let exit_code: u8;

    let send = |pings: &mut Vec<Child>, kind: Kind, body: &str| {
      let Some(p) = &plan.ping else { return };
      match ping::send(&python, &p.url(kind), body, spawner_is_root.then_some(NONROOT)) {
        Ok(c) => pings.push(c),
        Err(e) => eprintln!("devkit-container: ping {kind:?} could not start: {e}"),
      }
    };

    loop {
      // Forwarded signals.
      for (flag, sig) in [(&term, Signal::SIGTERM), (&int, Signal::SIGINT), (&hup, Signal::SIGHUP)] {
        if flag.swap(false, Ordering::SeqCst) {
          let _ = kill(child_pid, sig);
        }
      }
      // Reap: the app child ends the loop; anything else (a ping) is a zombie to collect.
      let mut app_exit: Option<u8> = None;
      loop {
        match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
          Ok(WaitStatus::Exited(pid, code)) if pid == child_pid => app_exit = Some(code as u8),
          Ok(WaitStatus::Signaled(pid, sig, _)) if pid == child_pid => app_exit = Some(128u8.wrapping_add(sig as i32 as u8)),
          Ok(WaitStatus::Exited(_, code)) if code != 0 => eprintln!("devkit-container: ping exited with {code}"),
          Ok(WaitStatus::StillAlive) | Err(_) => break,
          Ok(_) => {}
        }
      }
      pings.retain_mut(|c| matches!(c.try_wait(), Ok(None)));
      if let Some(code) = app_exit {
        exit_code = code;
        break;
      }
      if Instant::now() >= next_poll {
        next_poll = Instant::now() + Duration::from_secs(plan.poll_secs);
        let now = jiff::Timestamp::now();
        let mut reasons: Vec<String> = Vec::new();
        if let Some(t) = &tunnel {
          let handshake = match t.latest_handshake() {
            Ok(h) => h,
            Err(e) => {
              eprintln!("devkit-container: {e:#}");
              None
            }
          };
          let (fresh, action) = assessor.assess(now.as_second() as u64, handshake);
          if fresh {
            if let Err(e) = heartbeat::write(&tunnel_beat, now) {
              eprintln!("devkit-container: {e:#}");
            }
          } else if let Err(e) = t.act(action) {
            eprintln!("devkit-container: re-up failed: {e:#}");
          }
          if let Err(r) = heartbeat::check(&tunnel_beat, stale_secs, now) {
            reasons.push(r);
          }
        }
        if let Err(r) = heartbeat::check(&app_beat, heartbeat::DEFAULT_MAX_AGE_SECS, now) {
          reasons.push(r);
        }
        match pinger.decide(reasons.is_empty()) {
          Some(Kind::Fail) => {
            eprintln!("devkit-container: unhealthy: {}", reasons.join("; "));
            send(&mut pings, Kind::Fail, &reasons.join("\n"));
          }
          Some(kind) => send(&mut pings, kind, ""),
          None => {}
        }
      }
      std::thread::sleep(Duration::from_millis(250));
    }

    if let Some(t) = &tunnel {
      t.down();
    }
    if exit_code != 0 {
      eprintln!("devkit-container: app exited with {exit_code}");
      send(&mut pings, Kind::Fail, &format!("exit code {exit_code}"));
    }
    // The container ends with this process; give the last ping its chance to leave.
    for mut c in pings {
      let _ = c.wait();
    }
    Ok(exit_code)
  }
}
```

Add `#[cfg_attr(not(unix), allow(dead_code))] mod supervisor;` to `main.rs`. Run `cargo build` on Linux (or WSL); on Windows `cargo build` must still pass with the `unix` submodule compiled out.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test supervisor`
Expected: 1 passed.

- [ ] **Step 5: Commit**

```bash
git add src/supervisor.rs src/main.rs
git commit -m "feat(supervisor): spawn as 999, forward signals, reap, poll the tunnel and heartbeats, ping"
```

---

### Task 7: Wire `run` to the branch

**Files:**
- Modify: `src/run.rs`
- Modify: `src/main.rs`
- Modify: `tests/entrypoint.rs` (the root-only test gains the leftover-removal check)

**Interfaces:**
- Produces: `run::run(args: &RunArgs) -> Result<u8>` (exit code; the exec path never returns `Ok`).

- [ ] **Step 1: Restructure `run`**

Replace the body of `run::run` after the mount check with:

```rust
  let wireguard = pyproject::wireguard(&doc)?;
  let supervise = wireguard || pyproject::supervise(&doc)?;
  // 3b. The tunnel, after the mount check (a missing volume must not wait out a handshake)
  //     and before `prepare` (every check before the filesystem is touched).
  let tunnel = if wireguard {
    let cfg = wireguard::Config::from_env(&|k| std::env::var(k).ok())?;
    let (poll, stale) = (cfg.poll_secs, cfg.stale_secs);
    Some((wireguard::Tunnel::start(cfg)?, poll, stale))
  } else {
    None
  };
  // 4. mkdir -p + recursive chown. A failure here brings a tunnel down again.
  if let Err(e) = prepare::prepare(&args.app_root, &entries, &mut prepare::chown_nonroot) {
    if let Some((t, _, _)) = &tunnel {
      t.down();
    }
    return Err(e);
  }
  // A tunnel heartbeat left by an earlier run must not read as a stale tunnel (spec 7).
  if !wireguard {
    let _ = std::fs::remove_file(heartbeat::logs_dir(&args.app_root).join(heartbeat::TUNNEL_FILE));
  }
  let exe = args.app_root.join(".venv").join("bin").join(&script);
  if supervise {
    let services = pyproject::services(&doc);
    let env = |k: &str| std::env::var(k).ok();
    let ping = ping::Ping::configure(
      env("ALERTS_HEALTHCHECK_PING_URL").as_deref(),
      env("PINGKEY").as_deref(),
      ping::slug(env("HEARTBEAT_SLUG").as_deref(), &services).as_deref(),
    );
    match (&ping, wireguard) {
      (None, true) => eprintln!("devkit-container: no ping configured (PINGKEY and HEARTBEAT_SLUG, or ALERTS_HEALTHCHECK_PING_URL): the tunnel is visible to Docker but not to healthchecks.io"),
      (None, false) => eprintln!("devkit-container: no ping configured; the app pings for itself"),
      _ => {}
    }
    let (tunnel, poll_secs) = match tunnel {
      Some((t, poll, stale)) => (Some((t, stale)), poll),
      None => (None, env("WG_POLL_SECS").and_then(|v| v.parse().ok()).unwrap_or(30)),
    };
    return supervisor::run(supervisor::Plan {
      exe,
      app_root: args.app_root.clone(),
      tunnel,
      poll_secs,
      ping,
    });
  }
  // 5. Drop privileges, then replace this process with the app. (unchanged)
  setgroups(&[]).context("setgroups")?;
  setgid(Gid::from_raw(prepare::NONROOT)).context("setgid")?;
  setuid(Uid::from_raw(prepare::NONROOT)).context("setuid")?;
  use std::os::unix::process::CommandExt as _;
  let err = std::process::Command::new(&exe).exec();
  Err(anyhow!(err)).with_context(|| format!("exec {}", exe.display()))
```

Change the signature to `pub fn run(args: &RunArgs) -> Result<u8>`, add `use crate::{heartbeat, mounts, ping, prepare, pyproject, supervisor, wireguard};`, and update the module doc. In `main.rs`, make the `Run` arm on Unix `return match run::run(&…) { Ok(code) => ExitCode::from(code), Err(e) => { eprintln!("error: {e:#}"); ExitCode::from(1) } }` (the exec path only ever returns `Err`).

- [ ] **Step 2: Extend the root-only binary test**

In `tests/entrypoint.rs`'s `as_root_checks_mounts_prepares_dirs_drops_privileges_and_execs`, before the successful run, plant a leftover and assert it is gone afterwards:

```rust
  std::fs::create_dir_all(root.join("persisted_data/logs")).unwrap();
  std::fs::write(root.join("persisted_data/logs/wireguard-heartbeat.txt"), "2020-01-01T00:00:00Z").unwrap();
  // … the successful run …
  assert!(!root.join("persisted_data/logs/wireguard-heartbeat.txt").exists(), "a leftover tunnel beat is removed with the mode off");
```

Run: `cargo build && cargo clippy --all-targets -- -D warnings && cargo test --test entrypoint`
Expected: clean; the root-only test stays `#[ignore]` (runs under `sudo` on Linux: `sudo -E cargo test --test entrypoint -- --ignored`), the others pass.

- [ ] **Step 3: Commit**

```bash
git add src/run.rs src/main.rs tests/entrypoint.rs
git commit -m "feat(run): branch to the supervisor when supervise or wireguard is on; exec otherwise"
```

---

### Task 8: The templates, the schema doc, the README

**Files:**
- Create: `python/devkit_container/compose.template.yaml`
- Modify: `python/devkit_container/template.Dockerfile`
- Modify: `python/devkit_container/__init__.py` (docstring mentions both templates)
- Modify: `README.md`
- Modify: `.github/workflows/ci.yml` (the wheel job's assertion checks both files ship)

- [ ] **Step 1: The compose template**

Create `python/devkit_container/compose.template.yaml`:

```yaml
services:
# !service-block:
  {service}:
    # !rule exact
    container_name: {service}
    build:
      # !rule exact
      context: .
      # !rule exact
      dockerfile: docker/Dockerfile
      args:
        # !rule repo
        GIT_REPO: {git_repo}
        # !rule presence
        GIT_TAG: {git_tag}
    # !rule presence
    restart: no
    # !rule volume-target
    volumes:
      - type: bind
        source: /data/{package}_files
        target: /app/persisted_data
    # !if dep("aeth-ext") or keys("tool.docker.supervise") or keys("tool.docker.wireguard") as heartbeat:
    # !rule env-keys
    environment:
      - HEARTBEAT_SLUG={service}
    # !end heartbeat
    # !if dep("aeth-ext"):
      - ALERTS_EMAIL=info@sweetfiretobacco.com
      - ALERTS_EMAIL_PWD=${ALERTS_EMAIL_PWD:?}
      - ALERTS_RECIPIENTS=["jacob.ogden@sweetfiretobacco.com"]
    # !end
    # !if keys("tool.docker.wireguard"):
      - WG_PRIVATE_KEY=${WG_PRIVATE_KEY:?}
      - WG_ADDRESS=${WG_ADDRESS:?}
      - WG_PEER_PUBLIC_KEY=${WG_PEER_PUBLIC_KEY:?}
      - WG_PEER_ENDPOINT=${WG_PEER_ENDPOINT:?}
      - WG_PEER_ALLOWED_IPS=${WG_PEER_ALLOWED_IPS:?}
      - WG_PEER_PRESHARED_KEY=${WG_PEER_PRESHARED_KEY:-}
      - WG_PERSISTENT_KEEPALIVE=${WG_PERSISTENT_KEEPALIVE:-}
      - WG_HANDSHAKE_TIMEOUT_SECS=${WG_HANDSHAKE_TIMEOUT_SECS:-}
      - WG_POLL_SECS=${WG_POLL_SECS:-}
      - WG_STALE_SECS=${WG_STALE_SECS:-}
    # !end
    # !if keys("tool.docker.wireguard"):
    # !rule presence
    cap_add:
      - NET_ADMIN
    # !end
    # !rule presence
    networks:
      - coolify
    healthcheck:
      # !if keys("tool.docker.wireguard"):
      # !rule exact-list
      test:
        - CMD
        - /app/.venv/bin/devkit-container
        - healthcheck
        - --file
        - /app/persisted_data/logs/heartbeat.txt
        - --file
        - /app/persisted_data/logs/wireguard-heartbeat.txt
      # !rule exact
      start_period: 90s
      # !end
      # !if not keys("tool.docker.wireguard"):
      # !rule exact-list
      test:
        - CMD
        - /app/.venv/bin/devkit-container
        - healthcheck
      # !rule exact
      start_period: 15s
      # !end
      # !rule exact
      interval: 30s
      # !rule exact
      timeout: 5s
      # !rule exact
      retries: 3
# !end service-block

networks:
  coolify:
    external: true
```

- [ ] **Step 2: The Dockerfile block**

In `template.Dockerfile`, after the `useradd` `RUN` in the final stage add:

```dockerfile
# Wireguard mode: the tools the entrypoint shells out to, so they version with the binary.
# !if keys("tool.docker.wireguard"):
RUN apt-get update && apt-get install -y --no-install-recommends wireguard-tools iproute2 \
  && rm -rf /var/lib/apt/lists/*
# !end
```

- [ ] **Step 3: The README and the wheel assertion**

In `README.md`:
- **Subcommands**: add `healthcheck` (the paragraph from spec 7: files only, `--file` repeatable, `--max-age`, exit codes, reasons on stderr, bare timestamps as container-local time) and extend `run` with the branch: `supervise`/`wireguard` spawn and supervise (signals forwarded, zombies reaped, exit code passed through, `DEVKIT_SUPERVISED_PING`, the secrets scrubbed), the tunnel steps, the poll, the tunnel heartbeat, the ping (URL rules, `/start`/`/fail`, the Python subprocess), the no-ping log line.
- **`[tool.docker]` schema** table: rows for `supervise` and `wireguard`.
- New section **Environment contract**: the `WG_*` table from spec 6 plus `HEARTBEAT_SLUG`, `PINGKEY`, `ALERTS_HEALTHCHECK_PING_URL`, `DEVKIT_SUPERVISED_PING`.
- New section **Heartbeat files**: the two paths, the format, freshness, who writes what (spec 7's first two paragraphs).
- **How a project uses it**: mention `compose.template.yaml` beside the Dockerfile template.
- **Tests**: the new smoke tests and the render check (Task 9).

In `.github/workflows/ci.yml`, extend the wheel job's Python assertion to `assert {'devkit_container/template.Dockerfile', 'devkit_container/compose.template.yaml'} <= set(names), names` and its step name to mention both files. In `__init__.py` add the compose template to the docstring.

- [ ] **Step 4: Commit**

```bash
git add python/devkit_container README.md .github/workflows/ci.yml
git commit -m "feat(templates): the compose template with rule annotations and the wireguard block in both templates"
```

---

### Task 9: The render check and the CI jobs

**Files:**
- Create: `ci/render.sh`
- Modify: `.github/workflows/ci.yml`

- [ ] **Step 1: The render script**

Create `ci/render.sh`:

```bash
#!/usr/bin/env bash
# Render this checkout's two templates into a scratch Docker project through the released
# devkit, with the mode off and on, and fail on a render error or a marker left behind. The
# scratch project's lock points devkit-container at this checkout's wheel, so setup-project
# reads these templates and cannot advance the package past them (spec 12).
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
uv run maturin build --release --out "$here/dist"
wheel="$(ls "$here"/dist/devkit_container-*.whl | head -1)"
for mode in off on; do
  root="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/render-$mode"
  rm -rf "$root" && mkdir -p "$root/src/scratch_app" && : > "$root/src/scratch_app/__init__.py"
  {
    printf '[project]\nname = "scratch-app"\nversion = "0.1.0"\nrequires-python = ">=3.14"\ndependencies = ["devkit-container"]\n\n'
    printf '[project.scripts]\nrun-app-scratch = "scratch_app:main"\n\n'
    printf '[dependency-groups]\ndev = ["aeth-devkit"]\n\n'
    printf '[tool.docker]\nservices = ["scratch-app"]\nrequired_persisted_dirs = ["persisted_data"]\n'
    [ "$mode" = on ] && printf 'wireguard = true\n'
    printf '\n[tool.uv.sources]\naeth-devkit = [{ index = "SFTPyPI" }]\ndevkit-container = { path = "%s" }\n\n' "$wheel"
    printf '[[tool.uv.index]]\nname = "SFTPyPI"\nurl = "https://pypi.sweetfiretobacco.com/jacob.ogden/internal/+simple"\nexplicit = true\n'
  } > "$root/pyproject.toml"
  ( cd "$root" && uv sync 2>&1 | tail -3 && uv run devkit --version && uv run devkit setup-project -y --no-vscode --no-commit )
  if grep -rnE '(^|[^$])\{[a-z_]+\}|# !|!if|!end|!rule' "$root/docker"; then
    echo "marker or placeholder left in the rendered docker/ files ($mode)" >&2; exit 1
  fi
  if [ "$mode" = on ]; then
    grep -q "NET_ADMIN" "$root/docker/compose.yaml" && grep -q "wireguard-heartbeat.txt" "$root/docker/compose.yaml" && grep -q "wireguard-tools" "$root/docker/Dockerfile"
  else
    ! grep -q "NET_ADMIN" "$root/docker/compose.yaml" && ! grep -q "wireguard" "$root/docker/Dockerfile"
    grep -q '"healthcheck"' "$root/docker/compose.yaml" || grep -q -- "- healthcheck" "$root/docker/compose.yaml"
  fi
  echo "render ok: mode $mode"
done
```

Run it locally (`bash ci/render.sh`) once aeth-devkit 15 and devkit-templates 1.2 are released; until then it fails at `setup-project` with the old marker syntax and that is expected. Note in the commit message that the check is live after those releases.

- [ ] **Step 2: The CI jobs**

In `.github/workflows/ci.yml`:
- add a job `render` named `"Render: both modes of a scratch Docker project through the released devkit"` on `ubuntu-latest` with checkout, `dtolnay/rust-toolchain@stable`, `Swatinem/rust-cache@v2`, `astral-sh/setup-uv@v5` (python 3.14), then a step `bash ci/render.sh` named `ci/render.sh: setup-project into a scratch project with the mode off and on, then scan docker/ for leftovers`;
- in `container-smoke`, before the test step add `- name: sudo modprobe wireguard\n  run: sudo modprobe wireguard`, and change the test step to run both smoke tests: `cargo test --test docker_smoke --test docker_supervisor -- --ignored --nocapture` (the second file lands in Task 10).

- [ ] **Step 3: Commit**

```bash
git add ci/render.sh .github/workflows/ci.yml
git commit -m "ci: render both modes through the released devkit; load the wireguard module for the smoke tests"
```

---

### Task 10: The supervisor and wireguard smoke tests

**Files:**
- Create: `tests/common/mod.rs` (the helpers moved out of `docker_smoke.rs`, parametrised)
- Modify: `tests/docker_smoke.rs` (uses the helpers; adds the healthcheck-in-the-image check)
- Create: `tests/docker_supervisor.rs`

**Interfaces:**
- Produces in `tests/common/mod.rs`: `pub fn root() -> PathBuf`, `pub fn ok(cmd: &mut Command) -> Output`, `pub fn text(out: &Output) -> String`, `pub fn write(root, rel, content)`, `pub struct Cleanup { pub image: String, pub volume: String, pub containers: Vec<String>, pub network: Option<String> }` (drop removes all), `pub fn build_wheel(root, out) -> PathBuf`, `pub fn scratch_repo(work, wheel, app: &str, pyproject_tail: &str) -> PathBuf`, `pub fn dockerfile(root, wheel_name, wireguard: bool) -> String`, `pub fn docker(args) -> Command`, `pub fn build_image(work, root, app, pyproject_tail, wireguard, tag) -> String` (wheel + scratch + build; returns the wheel name).

- [ ] **Step 1: Extract the helpers**

Move `root`, `ok`, `text`, `write`, `Cleanup`, `APP` (renamed `REPORT_APP`), `WHEEL_DIR`, `PYPROJECT`, `scratch_repo`, `build_wheel`, `dockerfile`, `docker` from `tests/docker_smoke.rs` into `tests/common/mod.rs` as `pub`, with these changes:
- `PYPROJECT` ends at `required_persisted_dirs = […]` and gains a `{tail}` placeholder on the next line; `scratch_repo(work, wheel, app, pyproject_tail)` substitutes `{tail}` and writes `app` as the package source.
- `dockerfile(root, wheel_name, wireguard)` applies the template's one gate: lines between `# !if keys("tool.docker.wireguard"):` and the following `# !end` are kept when `wireguard` and dropped otherwise, and both marker lines are always dropped; assert exactly one such block was seen (`assert_eq!(gated, 1, "the template changed shape; update this helper")`).
- `Cleanup` gains `containers: Vec<String>` (each `docker rm -f`) and `network: Option<String>` (`docker network rm`), removed before the image.

Then `tests/docker_smoke.rs` becomes `mod common; use common::*;` plus the existing test calling `scratch_repo(work.path(), &wheel, REPORT_APP, "")` and `dockerfile(&root, &wheel_name, false)`. Add to the end of that test, after the heartbeat-outlives-the-container check:

```rust
  // The healthcheck subcommand in the image, against the file the app just wrote and against
  // a stale one written by hand.
  let fresh = docker(&["run", "--rm", "-v", &mount, "--entrypoint", "/app/.venv/bin/devkit-container", &guard.image, "healthcheck"])
    .output()
    .unwrap();
  assert!(fresh.status.success(), "{}", String::from_utf8_lossy(&fresh.stderr));
  let stale = docker(&["run", "--rm", "-v", &mount, "--entrypoint", "sh", &guard.image])
    .args(["-c", "echo 2020-01-01T00:00:00+00:00 > /app/persisted_data/logs/heartbeat.txt && /app/.venv/bin/devkit-container healthcheck"])
    .output()
    .unwrap();
  assert_eq!(stale.status.code(), Some(1));
  assert!(String::from_utf8_lossy(&stale.stderr).contains("stale by"), "{}", String::from_utf8_lossy(&stale.stderr));
```

Run: `cargo test --test docker_smoke -- --ignored --nocapture` (needs Docker; on the Windows dev machine this works as today)
Expected: green.

- [ ] **Step 2: The serving app and the new test**

Create `tests/docker_supervisor.rs`:

```rust
//! The supervising entrypoint end to end (spec 12): one image built with the wireguard mode
//! on, run three ways: supervise without a tunnel (an edited pyproject), wireguard against a
//! hub container built from the same image, and the ping against a local HTTP listener.
//! Needs docker, the musl target, the network and a host kernel with the wireguard module
//! (`sudo modprobe wireguard`); `#[ignore]`, CI runs it with `--ignored`.

mod common;

use std::time::{Duration, Instant};

use common::*;

/// Writes a report of its environment, then beats every second until SIGTERM, exiting with
/// SMOKE_EXIT_ON_TERM so the pass-through of the code can be asserted.
const SERVE_APP: &str = r#"import datetime
import json
import os
import signal
import sys
import time


def main() -> None:
    caps = {}
    with open("/proc/self/status") as f:
        for line in f:
            if line.startswith("Cap"):
                k, v = line.split(":", 1)
                caps[k] = v.strip()
    r = {
        "pid": os.getpid(),
        "ppid": os.getppid(),
        "uid": os.getuid(),
        "gid": os.getgid(),
        "groups": os.getgroups(),
        "cap_eff": caps["CapEff"],
        "cap_prm": caps["CapPrm"],
        "cap_amb": caps["CapAmb"],
        "wg_private_key_present": "WG_PRIVATE_KEY" in os.environ,
        "wg_psk_present": "WG_PEER_PRESHARED_KEY" in os.environ,
        "supervised_ping": os.environ.get("DEVKIT_SUPERVISED_PING"),
        "heartbeat_slug": os.environ.get("HEARTBEAT_SLUG"),
    }
    with open("/app/persisted_data/report.json", "w") as f:
        json.dump(r, f)
    stop = []
    signal.signal(signal.SIGTERM, lambda *_: stop.append(1))
    while not stop:
        with open("/app/persisted_data/logs/heartbeat.txt", "w") as f:
            f.write(datetime.datetime.now(datetime.UTC).isoformat())
        time.sleep(1)
    sys.exit(int(os.environ.get("SMOKE_EXIT_ON_TERM", "0")))
"#;

fn wait_for(what: &str, timeout: Duration, mut probe: impl FnMut() -> bool) {
  let deadline = Instant::now() + timeout;
  while !probe() {
    assert!(Instant::now() < deadline, "timed out waiting for {what}");
    std::thread::sleep(Duration::from_secs(1));
  }
}

fn exec(container: &str, args: &[&str]) -> std::process::Output {
  docker(&["exec", container]).args(args).output().unwrap()
}

fn healthcheck(container: &str) -> std::process::Output {
  exec(
    container,
    &[
      "/app/.venv/bin/devkit-container",
      "healthcheck",
      "--file",
      "/app/persisted_data/logs/heartbeat.txt",
      "--file",
      "/app/persisted_data/logs/wireguard-heartbeat.txt",
    ],
  )
}

fn wg_key(image: &str) -> (String, String) {
  let private = text(&ok(&mut docker(&["run", "--rm", "--entrypoint", "wg", image, "genkey"]))).trim().to_string();
  let public = {
    use std::io::Write as _;
    let mut child = docker(&["run", "--rm", "-i", "--entrypoint", "wg", image, "pubkey"])
      .stdin(std::process::Stdio::piped())
      .stdout(std::process::Stdio::piped())
      .spawn()
      .unwrap();
    child.stdin.take().unwrap().write_all(private.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    text(&out).trim().to_string()
  };
  (private, public)
}

#[test]
#[ignore = "needs docker, the x86_64-unknown-linux-musl target, the network and the wireguard kernel module; run with --ignored"]
fn the_supervisor_runs_the_app_with_and_without_a_tunnel_and_pings() {
  ok(&mut docker(&["version", "--format", "{{.Server.Os}}"]));
  let root = root();
  let work = tempfile::tempdir().unwrap();
  let id = format!("{}-{}", std::process::id(), std::time::UNIX_EPOCH.elapsed().unwrap().as_secs());
  let mut guard = Cleanup {
    image: format!("devkit-smoke-wg:{id}"),
    volume: format!("devkit-smoke-wg-{id}"),
    containers: vec![],
    network: Some(format!("devkit-smoke-wg-{id}")),
  };
  let image = guard.image.clone();
  let net = guard.network.clone().unwrap();
  build_image(work.path(), &root, SERVE_APP, "wireguard = true\n", true, &image);
  ok(&mut docker(&["network", "create", &net]));
  let mount = format!("{}:/app/persisted_data", guard.volume);

  // --- supervise without a tunnel: the same image, the switch swapped in a copy of pyproject.
  let app = format!("app-{id}");
  guard.containers.push(app.clone());
  ok(
    docker(&["run", "-d", "--name", &app, "--network", &net, "-v", &mount, "-e", "SMOKE_EXIT_ON_TERM=7", "-e", "HEARTBEAT_SLUG=smoke", "-e", "PINGKEY=k", "--entrypoint", "sh", &image])
      .args(["-c", "sed 's/^wireguard = true/supervise = true/' /app/pyproject.toml > /tmp/p.toml && exec /app/.venv/bin/devkit-container run --pyproject /tmp/p.toml"]),
  );
  wait_for("the app's report", Duration::from_secs(30), || exec(&app, &["cat", "/app/persisted_data/report.json"]).status.success());
  let report: serde_json::Value = serde_json::from_slice(&exec(&app, &["cat", "/app/persisted_data/report.json"]).stdout).unwrap();
  eprintln!("{report:#}");
  assert_eq!(report["ppid"], 1, "the supervisor is PID 1");
  assert_ne!(report["pid"], 1);
  assert_eq!(report["uid"], 999);
  assert_eq!(report["gid"], 999);
  assert_eq!(report["groups"], serde_json::json!([999]));
  for cap in ["cap_eff", "cap_prm", "cap_amb"] {
    assert_eq!(report[cap], "0000000000000000", "{cap}");
  }
  assert_eq!(report["supervised_ping"], "1");
  assert_eq!(report["heartbeat_slug"], "smoke");
  // The supervisor itself dropped to 999 (no tunnel to keep root for).
  let top = text(&ok(&mut docker(&["top", &app, "-o", "uid,pid,comm"])));
  assert!(top.lines().skip(1).all(|l| l.trim_start().starts_with("999")), "{top}");
  ok(&mut docker(&["kill", "--signal", "TERM", &app]));
  let code = text(&ok(&mut docker(&["wait", &app]))).trim().to_string();
  assert_eq!(code, "7", "the child's exit code passes through");
  let logs = String::from_utf8_lossy(&docker(&["logs", &app]).output().unwrap().stderr).into_owned();
  assert!(logs.contains("app exited with 7"), "{logs}");
  ok(&mut docker(&["rm", "-f", &app]));
  ok(&mut docker(&["volume", "rm", "-f", &guard.volume]));

  // --- the hub: the same image, wireguard-tools inside, keys generated there too.
  let (hub_priv, hub_pub) = wg_key(&image);
  let (spoke_priv, spoke_pub) = wg_key(&image);
  let hub = format!("hub-{id}");
  guard.containers.push(hub.clone());
  ok(
    docker(&["run", "-d", "--name", &hub, "--network", &net, "--cap-add", "NET_ADMIN", "-e", &format!("HUB_KEY={hub_priv}"), "--entrypoint", "sh", &image]).args([
      "-c",
      &format!(
        "printf '%s\\n' \"$HUB_KEY\" > /tmp/k && ip link add dev wg0 type wireguard && wg set wg0 listen-port 51820 private-key /tmp/k peer {spoke_pub} allowed-ips 10.8.0.20/32 && ip address add 10.8.0.1/24 dev wg0 && ip link set up dev wg0 && exec sleep infinity"
      ),
    ]),
  );
  // --- the ping listener: python's http.server as 999; its access log is the assertion.
  let hc = format!("hc-{id}");
  guard.containers.push(hc.clone());
  ok(&mut docker(&["run", "-d", "--name", &hc, "--network", &net, "--user", "999:999", "--entrypoint", "/app/.venv/bin/python", &image, "-m", "http.server", "8080", "--bind", "0.0.0.0"]));

  // --- the spoke: the app under the supervisor with the tunnel, short intervals.
  let spoke = format!("spoke-{id}");
  guard.containers.push(spoke.clone());
  ok(
    docker(&["run", "-d", "--name", &spoke, "--network", &net, "--cap-add", "NET_ADMIN", "-v", &mount])
      .args(["-e", &format!("WG_PRIVATE_KEY={spoke_priv}"), "-e", "WG_ADDRESS=10.8.0.20/32", "-e", &format!("WG_PEER_PUBLIC_KEY={hub_pub}")])
      .args(["-e", &format!("WG_PEER_ENDPOINT={hub}:51820"), "-e", "WG_PEER_ALLOWED_IPS=10.8.0.0/24", "-e", "WG_PERSISTENT_KEEPALIVE=1"])
      .args(["-e", "WG_POLL_SECS=1", "-e", "WG_STALE_SECS=4", "-e", "SMOKE_EXIT_ON_TERM=7", "-e", &format!("ALERTS_HEALTHCHECK_PING_URL=http://{hc}:8080/ping/x")])
      .arg(&image),
  );
  wait_for("the tunnel heartbeat", Duration::from_secs(90), || exec(&spoke, &["cat", "/app/persisted_data/logs/wireguard-heartbeat.txt"]).status.success());
  let hc_ok = healthcheck(&spoke);
  assert!(hc_ok.status.success(), "{}", String::from_utf8_lossy(&hc_ok.stderr));
  let report: serde_json::Value = serde_json::from_slice(&exec(&spoke, &["cat", "/app/persisted_data/report.json"]).stdout).unwrap();
  eprintln!("{report:#}");
  assert_eq!(report["ppid"], 1);
  assert_eq!(report["uid"], 999);
  for cap in ["cap_eff", "cap_prm", "cap_amb"] {
    assert_eq!(report[cap], "0000000000000000", "{cap}");
  }
  assert_eq!(report["wg_private_key_present"], false);
  assert_eq!(report["wg_psk_present"], false);
  assert_eq!(report["supervised_ping"], "1");
  let stat = text(&exec_ok(&spoke, &["stat", "-c", "%a %u", "/app/persisted_data/logs/wireguard-heartbeat.txt"]));
  assert!(stat.trim().starts_with("644"), "world-readable: {stat}");

  // --- the hub forgets the peer: stale, then the healthcheck says which file, then a re-up.
  exec_ok(&hub, &["wg", "set", "wg0", "peer", &spoke_pub, "remove"]);
  wait_for("a stale tunnel", Duration::from_secs(30), || healthcheck(&spoke).status.code() == Some(1));
  let stale = healthcheck(&spoke);
  let err = String::from_utf8_lossy(&stale.stderr);
  assert!(err.contains("wireguard-heartbeat.txt") && err.contains("stale by") && !err.contains("logs/heartbeat.txt:"), "{err}");
  wait_for("a re-up in the log", Duration::from_secs(20), || {
    String::from_utf8_lossy(&docker(&["logs", &spoke]).output().unwrap().stderr).contains("re-up")
  });
  exec_ok(&hub, &["wg", "set", "wg0", "peer", &spoke_pub, "allowed-ips", "10.8.0.20/32"]);
  wait_for("a fresh tunnel again", Duration::from_secs(60), || healthcheck(&spoke).status.success());

  // --- the ping: /start once, plain while fresh, /fail on the stale transition.
  wait_for("the listener's log", Duration::from_secs(10), || {
    let log = String::from_utf8_lossy(&docker(&["logs", &hc]).output().unwrap().stderr).into_owned();
    log.contains("/ping/x/start") && log.contains("/ping/x/fail") && log.contains("\"GET /ping/x HTTP")
  });
  let log = String::from_utf8_lossy(&docker(&["logs", &hc]).output().unwrap().stderr).into_owned();
  assert_eq!(log.matches("/ping/x/start").count(), 1, "{log}");

  // --- SIGTERM reaches the child and its code passes through; the interface goes down.
  ok(&mut docker(&["kill", "--signal", "TERM", &spoke]));
  assert_eq!(text(&ok(&mut docker(&["wait", &spoke]))).trim(), "7");
  let logs = String::from_utf8_lossy(&docker(&["logs", &spoke]).output().unwrap().stderr).into_owned();
  assert!(logs.contains("wireguard public key") && logs.contains("app exited with 7"), "{logs}");
}

/// `docker exec` that must succeed; panics with both streams otherwise.
fn exec_ok(container: &str, args: &[&str]) -> std::process::Output {
  let mut cmd = docker(&["exec", container]);
  cmd.args(args);
  ok(&mut cmd)
}
```

Add `serde_json` to `[dev-dependencies]` if not present (it is). `build_image` in `common` does: `build_wheel`, `scratch_repo(work, &wheel, app, pyproject_tail)`, copies the wheel into the context, writes `dockerfile(root, &wheel_name, wireguard)`, and `docker build --build-arg GIT_TAG=v0.1.0 --build-arg GIT_REPO=file:///tmp/scratch.git -t <tag> <context>`.

- [ ] **Step 3: Run it**

On a Linux host or CI (`sudo modprobe wireguard` first):

```bash
cargo test --test docker_supervisor -- --ignored --nocapture
```

Expected: green. Iterate on the supervisor and tunnel code until every assertion holds; the usual first failures are the handshake wait (check `docker logs hub` for the `wg set` line failing) and the reap loop (an `ECHILD` from `waitpid(-1)` when no child exists is the `Err(_) => break` arm).

- [ ] **Step 4: Commit**

```bash
git add tests/common/mod.rs tests/docker_smoke.rs tests/docker_supervisor.rs Cargo.toml Cargo.lock
git commit -m "test(smoke): the supervisor with and without a tunnel, the healthcheck, the ping, against a hub built from the image"
```

---

### Task 11: The full suite, the PR, the release

**Files:**
- Modify: `todo.md` (delete the healthcheck entry: it ships here)

- [ ] **Step 1: Everything once**

```bash
cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test
cargo test --test docker_smoke --test docker_supervisor -- --ignored --nocapture   # Linux, module loaded
bash ci/render.sh                                                                   # after aeth-devkit 15 / templates 1.2 are out
```

Expected: all green. Delete the healthcheck entry from `todo.md`.

- [ ] **Step 2: PR and merge**

```bash
git push -u origin supervisor-wireguard
gh pr create --title "feat: supervisor, wireguard mode, heartbeat healthcheck, compose template" --body "$(cat <<'EOF'
Sections 4 to 9 of docs/superpowers/specs/2026-09-08-container-wireguard-mode-design.md: the `supervise` and `wireguard` switches, the supervising entrypoint, the tunnel with keys over stdin and the stale-and-re-up rule, the `healthcheck` subcommand over heartbeat files, the healthchecks.io ping through the venv's Python, the compose template as package data with rule annotations, and the gated Dockerfile block. Three smoke tests and a render check in CI.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

Merge once CI is green (`gh pr merge --squash --delete-branch`).

- [ ] **Step 3: Release**

```bash
git switch main && git pull && uv sync
uv run devkit release major "supervisor, wireguard mode, healthcheck subcommand, the compose template ships in the wheel"
```

Needs the SFTPyPI credentials in `.env`; if absent, report that the owner runs this step. Major: the compose file every project renders changes shape (the healthcheck line), and `run` gains behaviour.

- [ ] **Step 4: Prove it in `ScheduledReportAggregator` (spec 13)**

```bash
cd "/d/SFT Software Projects/ScheduledReportAggregator" && uv run poe lock && uv run poe setup-project --no-vscode
```

Expected: the run advances `devkit-container`, replaces `docker/Dockerfile` (no wireguard block: the mode is off), and edits `docker/compose.yaml`: `HEARTBEAT_SLUG=scheduled-report-aggregator` added under `environment`, `healthcheck.test` set to the binary form, nothing else. Then set `[tool.docker].wireguard = true`, commit, rerun: the diff adds exactly the `WG_*` lines, `cap_add`, the two-file healthcheck and `start_period: 90s`, and the Dockerfile gains the `wireguard-tools` line. Revert or keep as the owner decides; the first-deploy checks are the companion spec's.

---

## Self-review

**Spec coverage.** Section 3's package-data half: Task 8. Section 4 switches and the off-mode leftover removal: Tasks 1, 7. Section 5 entrypoint order, spawn as 999 with empty caps, drop-to-999 without a tunnel, signals, reaping, exit codes, scrubbed environment, `DEVKIT_SUPERVISED_PING`: Tasks 6, 7, 10. Section 6 preflight, bring-up over stdin, the public key logged, the handshake timeout as a refused start, the per-poll stale rule, the contract table: Task 5 (contract, rule, tunnel), Task 7 (ordering). Section 7 the one concept, both files, the subcommand, the ping's shape and ownership, the slug fallback, Docker health: Tasks 2, 3, 4, 6, 8. Section 8 compose and Dockerfile content: Task 8. Section 9 is the aeth_ext plan's. Section 10 step 2: Task 11. Section 11: Task 9 (modprobe). Section 12's unit, render and three smoke paragraphs: Tasks 2–6, 9, 10. Section 13: Tasks 10, 11.

**Placeholders.** None; the two places that invite adaptation (jiff's `Display` form in Task 2, the `exec_cmd` helper in Task 10) say exactly what to do instead.

**Type consistency.** `heartbeat::check(&Path, u64, jiff::Timestamp) -> Result<(), String>` is used that way in Tasks 3 and 6. `ping::Ping::configure(Option<&str>, Option<&str>, Option<&str>)`, `Ping::url(Kind)`, `ping::slug(Option<&str>, &[String])`, `ping::send(&Path, &str, &str, Option<u32>)` match Tasks 4, 6, 7. `wireguard::Config::from_env(&dyn Fn(&str) -> Option<String>)`, `Tunnel::start(Config)`, `latest_handshake() -> Result<Option<u64>>`, `act(Action)`, `down()`, `Assessor::new(u64)` / `assess(u64, Option<u64>) -> (bool, Action)` match Tasks 5, 6, 7. `supervisor::Plan { exe, app_root, tunnel: Option<(Tunnel, u64)>, poll_secs, ping }` and `supervisor::run(Plan) -> Result<u8>` match Tasks 6 and 7. `run::run -> Result<u8>` is consumed by `main.rs` in Task 7.
