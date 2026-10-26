# Trixie base and permanent tolerate Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move the Dockerfile template to Debian 13 with a `builder-setup` window, and make running permanently under `WG_TOLERATE_DISCONNECTED=1` work: the healthcheck judges the app alone, and an offline spoke stops flooding its log.

**Architecture:** One template edit; one flag read by the `healthcheck` subcommand from the container's environment; one logging rule added to the pure `Health` state machine and honored by the supervisor's poll. No new env vars, no new `[tool.docker]` keys, no compose-template change.

**Tech Stack:** Rust (devkit-container 2.2.0), the Dockerfile template shipped in the wheel, `cargo test`.

**Spec:** `D:\SFT Software Projects\SFT Workspace\timeclock-system\docs\superpowers\specs\2026-10-26-timeclock-design.md` §11.1 and §11.2 (requirements R18.1, R18.2 in `2026-10-23-timeclock-requirements.md` beside it). §11.3 (LAN peers) is a separate, later plan.

**Execution:** Task 1 inline (a template edit and a shape test). Tasks 2 and 3 each by a fresh **high-effort** implementer subagent with a reviewer after it. Task 4 inline by the orchestrating session, after the owner merges. Work on branch `feat/trixie-permanent-tolerate`; commit this plan as the branch's first commit. Check each step off in this file as it lands (the owner reads it while work runs). The owner deletes this plan once the work lands.

## Global Constraints

- Read `AGENTS.md` first. On the branch, run only targeted tests while working, then `cargo test` once at the end of each task; CI runs the full set on the PR.
- `cargo fmt --all --check` and `cargo clippy --all-targets -- -D warnings` stay clean (CI enforces both, on Windows and Linux).
- Base image, both stages, exactly `ghcr.io/astral-sh/uv:python3.14-trixie-slim`. No per-project base-image setting.
- The new window is named exactly `builder-setup`. No aeth-devkit change: window names are free-form (aeth-devkit `gate.rs:113-124`) and windows already survive re-renders (≥ 15.1.1).
- `compose.template.yaml` does not change: its healthcheck `test:` is an `exact-list` rule (`:49-57`), and any change there makes setup-project propose a diff to every wireguard project.
- New comments cite "timeclock design §11.1" / "§11.2", never "spec N" (that already means two other documents in this repo). Keep comments dense (`AGENTS.md`).
- `healthcheck` stays file-only and cross-platform: no root, no `wg`, no pyproject parse; it runs on Windows dev machines too.
- Commits follow Conventional Commits; end each message with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Release: minor bump to 2.3.0 with `uv run devkit release minor`; the Windows wheel is required.

## Review Focus

1. **Existing projects' windows across the re-render:** a project's `builder` and `final` contents must come through unchanged and the new `builder-setup` arrive empty. Covered by aeth-devkit's splice (`windows.rs:59-99`) and CI's `render` job; the reviewer confirms the `render` job passes on the PR.
2. **A tolerate value other than `1`** (e.g. `true`) in the container: the entrypoint refuses to boot, and the healthcheck must still count the tunnel file. Pinned by Task 2's test.
3. **A healthcheck whose only `--file` is the tunnel file, under the switch:** nothing is left to check and it exits 0. Pinned by Task 2's test so it's a decision, not an accident.
4. **Every per-poll line while offline:** the repair line and the `endpoint unresolvable` line a failed endpoint reset logs (`classify`, `supervisor.rs:310-325`) must both be suppressed together; the `wireguard Disconnected: …` / `Connected` transitions still log. The reviewer reads the Disconnected poll path once for any other line that repeats every poll.
5. **A new disconnection after a reconnect** logs its first repair at once rather than waiting out the previous hour. Pinned by Task 3's test.

---

### Task 1: Trixie base images and the `builder-setup` window (§11.1)

**Files:**
- Modify: `python/devkit_container/template.Dockerfile:4,63` (both `FROM`), and insert after `:19`
- Modify: `README.md:14` ("two `# !window` regions"); `todo.md` (the item beginning "Consider building the image from the application's released wheel")
- Create: `tests/template.rs`

**Interfaces:**
- Produces: the window markers `# !window builder-setup:` / `# !end builder-setup`, which the timeclock-system and timeclock-backend plans fill.

- [x] **Step 1: Write the failing test** `tests/template.rs`:

```rust
//! The shipped Dockerfile template's shape (timeclock design §11.1).

fn template() -> String {
  std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/python/devkit_container/template.Dockerfile")).unwrap()
}

#[test]
fn both_stages_build_on_trixie() {
  let t = template();
  let froms: Vec<&str> = t.lines().filter(|l| l.starts_with("FROM ")).collect();
  assert_eq!(
    froms,
    [
      "FROM ghcr.io/astral-sh/uv:python3.14-trixie-slim AS builder",
      "FROM ghcr.io/astral-sh/uv:python3.14-trixie-slim",
    ]
  );
  assert!(!t.contains("bookworm"));
}

#[test]
fn builder_setup_window_sits_between_the_git_install_and_the_clone() {
  let t = template();
  let at = |needle: &str| t.lines().position(|l| l.starts_with(needle)).unwrap_or_else(|| panic!("{needle:?} missing"));
  let git = at("RUN apt-get update && apt-get install -y --no-install-recommends git");
  let open = at("# !window builder-setup:");
  let close = at("# !end builder-setup");
  let clone = at("RUN git clone ");
  let sync = at("RUN --mount=type=cache");
  assert!(git < open && open < close && close < clone && clone < sync, "{git} {open} {close} {clone} {sync}");
  assert_eq!(close, open + 1, "the window ships empty");
}
```

- [x] **Step 2: Run it to verify it fails**

Run: `cargo test --test template`
Expected: FAIL on both tests (bookworm `FROM` lines; `"# !window builder-setup:" missing`).

- [x] **Step 3: Edit the template**

Both `FROM` lines to `ghcr.io/astral-sh/uv:python3.14-trixie-slim` (keep `AS builder` on the first). After the `git` install `RUN` (ends `:19`), insert a blank line, then:

```dockerfile
# Project additions before the first sync: system packages dependencies need to compile.
# The repo isn't cloned yet, so a file this needs is COPYed from the build context.
# !window builder-setup:
# !end builder-setup
```

The existing `builder` and `final` windows stay where they are.

- [x] **Step 4: Update the docs**

`README.md:14`: "two `# !window` regions" → "three `# !window` regions (`builder-setup` before the first `uv sync`, `builder` after the project install, `final`)". In `todo.md`'s "Consider building the image from the application's released wheel" item, replace `uv:python3.14-bookworm-slim` with `uv:python3.14-trixie-slim` and "the window comes after that sync" with "the `builder-setup` window (timeclock design §11.1) now runs before that sync, so a project can install a toolchain there". Leave the item open; closing it is the owner's call.

- [x] **Step 5: Run the tests** (full `cargo test` deferred to Task 4 Step 1: ledger ruling)

Run: `cargo test --test template` → PASS. Then `cargo test` → PASS.

- [x] **Step 6: Commit**

```bash
git add python/devkit_container/template.Dockerfile tests/template.rs README.md todo.md
git commit -m "feat(template): build on trixie, add the builder-setup window"
```

The smoke tests (`docker_smoke`, `docker_supervisor`, `docker_fetched`, all `--ignored`) rebuild the image on trixie; they run in CI's `container-smoke` job on the PR (Task 4). Locally they need Linux, Docker, `sudo modprobe wireguard` and `DEVKIT_SMOKE_WG_HUB_TOKEN`; only the owner runs them by hand.

---

### Task 2: The healthcheck judges the app alone under the switch (§11.2)

**Files:**
- Modify: `src/healthcheck.rs` (`run`, module doc), `src/main.rs:91-98` (the `Healthcheck` arm) and its doc comment at `:59`
- Modify: `tests/entrypoint.rs` (new test), `tests/docker_fetched.rs:282-286`
- Modify: `README.md` (`:79-80`, the `healthcheck` subcommand paragraph, the `WG_TOLERATE_DISCONNECTED` row at `:149`)

**Interfaces:**
- Consumes: `heartbeat::TUNNEL_FILE` (`src/heartbeat.rs:11`, `"wireguard-heartbeat.txt"`).
- Produces: `pub fn run(files: &[PathBuf], max_age: u64, tolerate: bool) -> u8` in `src/healthcheck.rs`.

- [x] **Step 1: Write the failing test** in `tests/entrypoint.rs`, beside `healthcheck_reads_files_only_and_says_why_it_fails` (reuse its `bin()`):

```rust
#[test]
fn healthcheck_ignores_the_tunnel_file_under_the_tolerate_switch() {
  let dir = tempfile::tempdir().unwrap();
  let logs = dir.path().join("persisted_data").join("logs");
  std::fs::create_dir_all(&logs).unwrap();
  let (app, tunnel) = (logs.join("heartbeat.txt"), logs.join("wireguard-heartbeat.txt"));
  let now = jiff::Timestamp::now();
  let old = now.checked_sub(jiff::Span::new().seconds(400)).unwrap();
  std::fs::write(&app, now.to_string()).unwrap();
  std::fs::write(&tunnel, old.to_string()).unwrap();
  let check = |tolerate: Option<&str>, files: &[&std::path::Path]| {
    let mut cmd = bin();
    cmd.arg("healthcheck").env_remove("WG_TOLERATE_DISCONNECTED");
    if let Some(v) = tolerate {
      cmd.env("WG_TOLERATE_DISCONNECTED", v);
    }
    for f in files {
      cmd.arg("--file").arg(f);
    }
    cmd.output().unwrap()
  };
  // Under the switch a stale tunnel file is ignored.
  let out = check(Some("1"), &[&app, &tunnel]);
  assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
  // Only exactly `1` is the switch; the entrypoint refuses anything else at boot.
  let out = check(Some("true"), &[&app, &tunnel]);
  assert_eq!(out.status.code(), Some(1));
  assert!(String::from_utf8_lossy(&out.stderr).contains("wireguard-heartbeat.txt"));
  // A missing tunnel file is ignored too; a stale app file is still reported, alone.
  std::fs::remove_file(&tunnel).unwrap();
  std::fs::write(&app, old.to_string()).unwrap();
  let out = check(Some("1"), &[&app, &tunnel]);
  assert_eq!(out.status.code(), Some(1));
  let err = String::from_utf8_lossy(&out.stderr);
  assert!(err.contains("heartbeat.txt") && !err.contains("wireguard-heartbeat.txt"), "{err}");
  // Only the tunnel file named: nothing is left to check, so healthy.
  assert!(check(Some("1"), &[&tunnel]).status.success());
}
```

Also add `.env_remove("WG_TOLERATE_DISCONNECTED")` to each `bin()` call in `healthcheck_reads_files_only_and_says_why_it_fails`, so a developer's shell can't change its outcome.

- [x] **Step 2: Run it to verify it fails**

Run: `cargo test --test entrypoint healthcheck_ignores_the_tunnel_file_under_the_tolerate_switch`
Expected: FAIL at the first assertion (the stale tunnel file is reported).

- [x] **Step 3: Implement**

`healthcheck::run(files, max_age, tolerate)`: when `tolerate`, skip every file whose `file_name()` is `heartbeat::TUNNEL_FILE`; everything else is unchanged. One comment: matched by name so the compose template's exact-list healthcheck stays as it is; the tunnel's staleness still reaches healthchecks.io through the supervisor's `/fail` (timeclock design §11.2). In `main.rs`, the `Healthcheck` arm passes `std::env::var("WG_TOLERATE_DISCONNECTED").as_deref() == Ok("1")`. Update the module doc and the subcommand's doc comment to say so.

- [x] **Step 4: Rewrite the smoke assertion** `tests/docker_fetched.rs:282-286`: the tolerated container's healthcheck no longer names the tunnel file:

```rust
let hc = healthcheck(&tolerated);
assert!(
  !String::from_utf8_lossy(&hc.stderr).contains("wireguard-heartbeat.txt"),
  "under the switch the healthcheck judges the app's file alone"
);
```

(`docker exec` runs with the container's environment, so the helper's `healthcheck` sees the switch.)

- [x] **Step 5: Update `README.md`**

- `:79-80`: after "for an emergency where a connection cannot happen for an external reason", add: "or permanently, for an app that must keep working offline (a time clock); the `healthcheck` subcommand then judges the app's file alone".
- `healthcheck` paragraph: "Under `WG_TOLERATE_DISCONNECTED=1` a `--file` named `wireguard-heartbeat.txt` is skipped, so a spoke running offline stays healthy while its app is; the supervisor still sends `/fail` when the tunnel goes stale."
- `:149` row: replace "for emergencies" with "for an emergency, or permanently for an app that must keep working offline; the `healthcheck` then skips the tunnel file".

- [x] **Step 6: Run the tests** (full `cargo test` deferred to Task 4 Step 1: ledger ruling)

Run: `cargo test --test entrypoint` → PASS, then `cargo test` → PASS. (The rewritten smoke assertion runs in CI.)

- [x] **Step 7: Commit**

```bash
git add src/healthcheck.rs src/main.rs tests/entrypoint.rs tests/docker_fetched.rs README.md
git commit -m "feat(healthcheck): judge the app alone under WG_TOLERATE_DISCONNECTED"
```

---

### Task 3: Rate-limit the Disconnected repair lines (§11.2)

**Files:**
- Modify: `src/health.rs` (`Decision` `:78-89`, `Health` `:101-125`, `poll` `:129-203`, tests)
- Modify: `src/supervisor.rs` (the repair arms in `TunnelState::poll`, `:349-361`; `classify` `:310-325`)
- Modify: `README.md:85` ("each re-up logged")

**Interfaces:**
- Produces: `pub const REPAIR_LOG_INTERVAL_SECS: u64 = 3600;` and `Decision::log_repair: Option<u64>` in `src/health.rs`: `Some(n)` means log this poll's repair line, `n` being the repairs left unlogged since the previous line; `None` means stay quiet.

- [x] **Step 1: Write the failing test** in `src/health.rs`'s `tests` (uses its `OK`):

```rust
#[test]
fn repair_lines_come_first_then_hourly_with_the_unlogged_count() {
  // Tolerate on: the permanent-offline case, no asks or give-up to interleave.
  let mut h = Health::new(true, true, 1800, 0);
  assert_eq!(h.poll(0, OK, true, None).log_repair, None, "Connected repairs nothing");
  assert_eq!(h.poll(30, OK, false, None).log_repair, Some(0), "the first repair of a disconnection");
  for t in (60..3630).step_by(30) {
    assert_eq!(h.poll(t, OK, false, None).log_repair, None, "suppressed at {t}");
  }
  assert_eq!(h.poll(3630, OK, false, None).log_repair, Some(119), "an hour on, with the count");
  h.poll(3660, OK, true, None);
  assert_eq!(h.poll(3690, OK, false, None).log_repair, Some(0), "a new disconnection logs at once");
  // An Obtain repair logs no line of its own, so it never claims one.
  let mut u = Health::new(true, true, 1800, 0);
  assert_eq!(u.poll(0, Interface::Unconfigured(Reason::ConfigUnavailable), false, None).log_repair, None);
}
```

- [x] **Step 2: Run it to verify it fails**

Run: `cargo test --bin devkit-container health::tests::repair_lines_come_first_then_hourly_with_the_unlogged_count`
(a binary crate; `health` compiles on Windows too, so this runs on the workstation)
Expected: FAIL to compile (`no field log_repair`).

- [x] **Step 3: Implement in `src/health.rs`**

`Health` gains `last_repair_line: Option<u64>` and `unlogged_repairs: u64`, both reset on a fresh poll (where the clock and alternation already reset). For a `ResetEndpoint` or `DownUp` repair: when `last_repair_line` is `None` or `now - last >= REPAIR_LOG_INTERVAL_SECS`, set `log_repair = Some(unlogged_repairs)`, record `now`, zero the count; otherwise count it and leave `None`. `Obtain` and `None` repairs leave `log_repair` at `None`. Doc the field and constant in one line each, citing timeclock design §11.2: offline under the switch, a line per poll is about 2,900 a day into `devkit-container.log`, which is never rotated.

- [x] **Step 4: Honor it in `src/supervisor.rs`**

In the `ResetEndpoint` and `DownUp` arms, log the existing line only when `d.log_repair` is `Some(n)`, appending ` ({n} more since the last line)` when `n > 0`. The repair itself still runs every poll. `classify` (`:310-325`) gains a `bool` for whether to log `endpoint unresolvable`: the repair arms pass `d.log_repair.is_some()`, every other call site `true`, so a failing name lookup offline doesn't log every other poll either. `endpoint_ok` is set the same either way.

- [x] **Step 5: Update `README.md:85`**: "each re-up logged" → "the first re-up of a disconnection logged, then at most once an hour with the count since".

- [x] **Step 6: Run the tests** (full `cargo test` deferred to Task 4 Step 1: ledger ruling)

Run: `cargo test --bin devkit-container health` → PASS, then `cargo test` → PASS. `docker_supervisor`'s "a re-up in the log" wait still holds: the first repair is logged (CI).

- [x] **Step 7: Commit**

```bash
git add src/health.rs src/supervisor.rs README.md
git commit -m "feat(supervisor): log Disconnected repairs once, then hourly"
```

---

### Task 4: Pull request, then release 2.3.0

Inline, by the orchestrating session. The merge is the owner's.

- [x] **Step 1: Final checks on the branch** (Windows and a Linux rust:1 container: fmt, clippy, `cargo test` clean)

Run: `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`. Expected: clean, PASS.

- [ ] **Step 2: Push and open the PR** (PR #4 open; CI red only in `docker_fetched`: the fixture repo `wireguard-hub-smoke` is gone and the token gets HTTP 401 — owner action)

```bash
git push -u origin feat/trixie-permanent-tolerate
gh pr create --title "Trixie base, builder-setup window, permanent tolerate" --body "<summary of tasks 1-3; ends with the Claude Code attribution line>"
```

Expected: every CI job passes, including `container-smoke` (the image now builds on trixie; the hub container in `docker_supervisor` uses the same image) and `render` (Review Focus 1). A failing smoke test is debugged on the branch, not waved through.

- [ ] **Step 3: The owner reviews and merges.** Wait for it.

- [ ] **Step 4: Release from `main`**

```bash
git checkout main && git pull
cargo test
uv run devkit release minor
```

Expected: tag `v2.3.0`, the release workflow green, the Windows and manylinux wheels on SFTPyPI (an anonymous GET of `https://pypi.sweetfiretobacco.com/jacob.ogden/internal/+simple/devkit-container/` lists `2.3.0`).

- [x] **Step 5: Hand-off note** (in the PR body, #4) in the release notes or PR: consumers move to trixie on their next `setup-project` or `docker-pin`; tunnel-probe's `final` window hard-codes Microsoft's `debian/12` apt repo and needs `debian/13` then (timeclock design §14).
