# Cleanup and aeth_ext Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Finish the spec's release order: remove the compose template from `devkit-templates` and the templates fallback from `aeth-devkit` now that `devkit-container` ships the template, and open the `aeth_ext` PR that reads `HEARTBEAT_SLUG` from the environment and stands its periodic ping down under `DEVKIT_SUPERVISED_PING`.

**Architecture:** Three small, independent changes in three repos. The two devkit ones are deletions with a test each; the `aeth_ext` one is two one-condition edits in `heartbeat.py` with tests beside the existing ones. The `aeth_ext` branch is pushed and a PR opened, never merged or released here (the owner does both).

**Tech Stack:** Rust (`aeth-devkit`), the `devkit-templates` package, Python 3.14 with pytest (`aeth_ext`).

**Spec:** `devkit-container/docs/superpowers/specs/2026-09-08-container-wireguard-mode-design.md`, sections 3 (the fallback's removal), 9, 10 (steps 3 and 4), 13 (last bullet), and section 7's **Ownership** and **Slug** paragraphs (the contract `aeth_ext` implements). Runs after the container plan's release (step 2).

## Global Constraints

- Everything Python runs under `uv run`; `uv add`/`uv remove`/`uv lock` are refused by the project hooks.
- `aeth_ext` changes land on a branch with a PR opened; nothing is merged or released there by this plan (owner's call).
- The contract names are fixed by the spec and the container: `HEARTBEAT_SLUG`, `DEVKIT_SUPERVISED_PING` (any non-empty value means set).
- Commit messages: Conventional Commits with `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`; PR bodies end with `🤖 Generated with [Claude Code](https://claude.com/claude-code)`.
- `aeth_ext`'s test layout: `tests/monitoring/test_heartbeat.py`, run with `uv run pytest tests/monitoring/test_heartbeat.py -q`.

---

### Task 1: `devkit-templates` drops the compose template

**Files (in `../devkit-templates`):**
- Delete: `python/devkit_templates/templates/docker/compose.template.yaml`
- Modify: `README.md` (the file list no longer names the compose scaffold)

- [ ] **Step 1: Confirm the container release carries the template**

```bash
cd "/d/SFT Software Projects/devkit-container" && uv sync && uv run python -c "import devkit_container, os; d=os.path.dirname(devkit_container.__file__); print(sorted(os.listdir(d)))"
```

Expected: the listing includes `compose.template.yaml` and `template.Dockerfile`, and `uv run devkit-container --version` prints the container plan's release (2.0.0 or later). If it does not, stop: this plan runs after that release.

- [ ] **Step 2: Delete the template, fix the README, verify the render**

```bash
cd "/d/SFT Software Projects/devkit-templates" && git switch main && git pull && git switch -c drop-compose-template
git rm python/devkit_templates/templates/docker/compose.template.yaml
```

In `README.md`, remove `the compose scaffold` from the sentence listing what the package contains, and add one sentence after it: `The Dockerfile and compose templates ship in devkit-container, beside the binary they describe.`

Render the docker scratch project through the released devkit (the `ci/render.sh` script's docker case):

```bash
bash ci/render.sh docker aeth-devkit
```

Expected: `render ok: docker …`; the scratch project's `docker/compose.yaml` exists (from the container package) with `HEARTBEAT_SLUG=scratch-app` under `environment`.

- [ ] **Step 3: Commit, PR, merge, release**

```bash
git commit -am "chore(templates): the compose template ships in devkit-container now"
git push -u origin drop-compose-template
gh pr create --title "chore(templates): drop the compose template" --body "$(cat <<'EOF'
devkit-container ships compose.template.yaml beside template.Dockerfile (spec section 3, release step 3); aeth-devkit reads it from there. This copy is dead.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

Merge when green, then `git switch main && git pull && uv sync && uv run devkit release patch "drop the compose template; it ships in devkit-container"` (credentials in `.env`; else the owner runs it).

---

### Task 2: `aeth-devkit` drops the templates fallback

**Files (in `../aeth-devkit`):**
- Modify: `crates/aeth-devkit-setup/src/docker/scaffold.rs`
- Modify: `crates/aeth-devkit-setup/tests/docker.rs` (the fallback test becomes a refusal test)
- Delete: `crates/aeth-devkit-setup/tests/fixtures/templates/docker/compose.template.yaml`

**Interfaces:**
- Produces: `scaffold::load(ctx, venv, gates) -> Result<Scaffold>` (no `templates_dir`); `docker::apply(ctx, deps, gates, changes)` and `docker::compose(ctx, venv, runner, consent, gates, changes)` lose the `templates_dir` parameter; `lib.rs`'s call site follows.

- [ ] **Step 1: Write the failing test**

In `tests/docker.rs`, replace `the_templates_copy_is_the_fallback_without_a_container_compose_template` with:

```rust
#[test]
fn a_container_package_without_the_compose_template_is_refused_with_the_version_named() {
  // Only a devkit-container older than the one that ships the template lacks it; the run
  // says so instead of rendering nothing.
  let dir = project(&["demo-app"], "https://github.com/o/r.git");
  let site = tempfile::tempdir().unwrap();
  let pkg = site.path().join("devkit_container");
  std::fs::create_dir_all(&pkg).unwrap();
  std::fs::copy(fixtures().join("docker").join("template.Dockerfile"), pkg.join("template.Dockerfile")).unwrap();
  let mut map = std::collections::HashMap::new();
  map.insert("devkit_container".to_string(), Installed { dir: pkg, version: "1.4.0".into() });
  for name in ["devkit_claude_hooks", "devkit_poe_complete"] {
    map.insert(name.to_string(), Installed { dir: fixtures().join("docker"), version: "1.0.0".into() });
  }
  let venv = StubVenv(map);
  let prompt = ScriptedPrompt::new(&[]);
  let runner = RecordingRunner::new(0);
  let index = StubIndexClient { versions: vec![] };
  let docker = Deps { runner: &runner, prompt: &prompt, reviewer: None, mode: Mode::Yes };
  let ctx = aeth_devkit_setup::context::ProjectContext::discover(dir.path()).unwrap();
  let err = aeth_devkit_setup::run_with(&ctx, Some(&templates()), false, &deps(docker, &index, &venv))
    .unwrap_err()
    .to_string();
  assert!(err.contains("compose.template.yaml") && err.contains("1.4.0"), "{err}");
}
```

Run: `cargo test -p aeth-devkit-setup --test docker a_container_package_without`
Expected: FAIL (today the templates copy is used and the run succeeds).

- [ ] **Step 2: Remove the fallback**

In `scaffold.rs`, `load` becomes:

```rust
/// The compose template from the installed container package, gated and substituted
/// (spec section 3). A package without it is older than the release that ships it.
pub fn load(ctx: &ProjectContext, venv: &dyn Venv, gates: &Gates) -> Result<Scaffold> {
  let installed = venv
    .installed(&ctx.root, &packages::CONTAINER)
    .context("devkit-container is not installed in this environment; a plain run installs it")?;
  let path = installed.dir.join(TEMPLATE_FILE);
  let text = std::fs::read_to_string(&path).with_context(|| {
    format!(
      "devkit-container {} has no {TEMPLATE_FILE}: the compose template ships with the binary from 2.0.0; run setup-project on a plain run to advance the package",
      installed.version
    )
  })?;
  parse(&templates::substitute(&gates.apply(&text, Format::Yaml, TEMPLATE_FILE)?, ctx, templates::Escape::None))
}
```

Remove the `templates_dir` parameter from `docker::apply`, `docker::compose` and the `lib.rs` call (`docker::apply(ctx, deps, &gates, &mut changes)?`), drop the `use std::path::Path;` imports that become unused, delete the fixture `tests/fixtures/templates/docker/compose.template.yaml` (the `docker/` fixture directory keeps its copy as the container package), and delete the `without_the_container_package_the_dockerfile_is_skipped_with_a_note` expectation that a compose file is still created when the package is absent: with no package there is neither template, so that test now asserts the note about the Dockerfile and that the run reports no compose change (adjust its assertions to what the run does; keep its name).

Run: `cargo test -p aeth-devkit-setup --test docker && cargo test -p aeth-devkit-setup --test apply && cargo clippy --all-targets -- -D warnings`
Expected: green.

- [ ] **Step 3: Commit, PR, merge, release**

```bash
git switch -c drop-compose-fallback   # from an up-to-date main
git add -A crates && git commit -m "refactor(setup): the compose scaffold comes from the container package only"
git push -u origin drop-compose-fallback
gh pr create --title "refactor(setup): drop the templates fallback for the compose scaffold" --body "$(cat <<'EOF'
Release step 3 of devkit-container/docs/superpowers/specs/2026-09-08-container-wireguard-mode-design.md: devkit-container 2.0.0 ships compose.template.yaml, so the templates-package fallback goes; a package without the file is refused naming its version.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

Merge when green, then `uv run devkit release patch "the compose scaffold comes from devkit-container only"` on `main` (credentials as before).

---

### Task 3: `aeth_ext` reads the slug from the environment and stands down under a supervisor

**Files (in `../aeth_ext`):**
- Modify: `src/aeth_ext/monitoring/heartbeat.py`
- Modify: `tests/monitoring/test_heartbeat.py`

**Interfaces:**
- Produces: `_auto_slug(caller_file)` prefers `os.environ["HEARTBEAT_SLUG"]`; `_run_heartbeat_async` and `HeartbeatThread.run` skip the ping (keep the file write) when `DEVKIT_SUPERVISED_PING` is set; `send_heartbeat(..., failure=True)` unchanged.

- [ ] **Step 1: Branch**

```bash
cd "/d/SFT Software Projects/aeth_ext" && git switch main && git pull && uv sync && git switch -c heartbeat-under-supervisor
```

- [ ] **Step 2: Write the failing tests**

Append to `tests/monitoring/test_heartbeat.py`, at module level after the existing classes:

```python
class TestSlugFromEnvironment:
  def test_heartbeat_slug_env_wins_over_the_code_constant(self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    calls: list[SecretStr | None] = []
    monkeypatch.setattr(heartbeat_module, "ping_healthcheck", lambda url, **_: calls.append(url))
    monkeypatch.setenv("HEARTBEAT_SLUG", "from-compose")
    heartbeat_module._auto_slug.cache_clear()

    heartbeat_module.send_heartbeat(tmp_path / "heartbeat.txt", pingkey=SecretStr("key"))

    assert calls == [SecretStr("https://hc-ping.com/key/from-compose")]

  def test_an_empty_heartbeat_slug_is_unset(self, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("HEARTBEAT_SLUG", "  ")
    heartbeat_module._auto_slug.cache_clear()

    assert heartbeat_module._auto_slug(__file__) is None  # no HEARTBEAT_SLUG constant in this package either

  def test_an_explicit_slug_argument_still_wins(self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    calls: list[SecretStr | None] = []
    monkeypatch.setattr(heartbeat_module, "ping_healthcheck", lambda url, **_: calls.append(url))
    monkeypatch.setenv("HEARTBEAT_SLUG", "from-compose")
    heartbeat_module._auto_slug.cache_clear()

    heartbeat_module.send_heartbeat(tmp_path / "heartbeat.txt", pingkey=SecretStr("key"), slug="explicit")

    assert calls == [SecretStr("https://hc-ping.com/key/explicit")]


class TestUnderASupervisor:
  def test_the_periodic_ping_stands_down_but_the_file_is_still_written(self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    pings: list[object] = []
    monkeypatch.setattr(heartbeat_module, "ping_healthcheck", lambda *a, **k: pings.append((a, k)))
    monkeypatch.setenv("DEVKIT_SUPERVISED_PING", "1")
    heartbeat_file = tmp_path / "heartbeat.txt"

    async def scenario() -> None:
      task = asyncio.create_task(heartbeat_module.run_heartbeat_async(heartbeat_file, pingkey=SecretStr("key"), slug="app", interval=0.05))
      await asyncio.sleep(0.2)
      task.cancel()
      with contextlib.suppress(asyncio.CancelledError):
        await task

    asyncio.run(scenario())

    assert datetime.fromisoformat(heartbeat_file.read_text())
    assert pings == []

  def test_the_thread_stands_down_too(self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    pings: list[object] = []
    monkeypatch.setattr(heartbeat_module, "ping_healthcheck", lambda *a, **k: pings.append((a, k)))
    monkeypatch.setenv("DEVKIT_SUPERVISED_PING", "1")
    heartbeat_file = tmp_path / "heartbeat.txt"

    thread = heartbeat_module.start_heartbeat_thread(heartbeat_file, pingkey=SecretStr("key"), slug="app", interval=0.05)
    time.sleep(0.2)
    heartbeat_module.SHUTDOWN.set()
    thread.join(timeout=2)

    assert datetime.fromisoformat(heartbeat_file.read_text())
    assert pings == []

  def test_a_known_failure_is_still_the_apps_to_send(self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    calls: list[tuple[object, bool]] = []
    monkeypatch.setattr(heartbeat_module, "ping_healthcheck", lambda url, *, failure=False, **_: calls.append((url, failure)))
    monkeypatch.setenv("DEVKIT_SUPERVISED_PING", "1")

    heartbeat_module.send_heartbeat(tmp_path / "heartbeat.txt", pingkey=SecretStr("key"), slug="app", failure=True)

    assert calls == [(SecretStr("https://hc-ping.com/key/app"), True)]

  def test_without_the_variable_the_app_pings_as_before(self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    pings: list[object] = []
    monkeypatch.setattr(heartbeat_module, "ping_healthcheck", lambda *a, **k: pings.append((a, k)))
    monkeypatch.delenv("DEVKIT_SUPERVISED_PING", raising=False)

    thread = heartbeat_module.start_heartbeat_thread(tmp_path / "heartbeat.txt", pingkey=SecretStr("key"), slug="app", interval=0.05)
    time.sleep(0.2)
    heartbeat_module.SHUTDOWN.set()
    thread.join(timeout=2)

    assert len(pings) >= 2
```

Add `import contextlib` to the file's standard-library imports. Check how the existing tests reset `SHUTDOWN` between tests (a fixture in `tests/conftest.py`, or a `SHUTDOWN.clear()` in each test); do the same here so `SHUTDOWN.set()` in one test does not stop the next.

- [ ] **Step 3: Run the tests to verify they fail**

Run: `uv run pytest tests/monitoring/test_heartbeat.py -q -k "SlugFromEnvironment or UnderASupervisor"`
Expected: the env-slug and stand-down tests fail; `test_an_explicit_slug_argument_still_wins`, `test_a_known_failure_is_still_the_apps_to_send` and `test_without_the_variable_the_app_pings_as_before` already pass (they pin behaviour that must not change).

- [ ] **Step 4: Write the implementation**

In `heartbeat.py`:

1. Add `import os` to the standard-library imports.
2. Replace `_auto_slug`'s body so the environment wins:

```python
@cache
def _auto_slug(caller_file: str) -> str | None:
  """The heartbeat slug: ``HEARTBEAT_SLUG`` from the environment, else a ``HEARTBEAT_SLUG``
  constant in *caller_file*'s own package ancestry.

  The environment form is what devkit-managed containers set (compose writes the service name),
  and the ``devkit-container`` supervisor reads the same variable when it owns the ping, so the
  two never disagree. The constant remains for hosts that are not containers. Memoised per file
  for the life of the process.

  Must be called with the file of whichever consumer actually asked for a heartbeat, never this
  module's own file (see the callers).
  """
  from_env = os.environ.get("HEARTBEAT_SLUG", "").strip()
  if from_env:
    return from_env
  found = parse_and_grab_constants(expected_constants={"HEARTBEAT_SLUG": "heartbeat_slug"}, caller_file=caller_file)
  return found.get("heartbeat_slug")
```

3. Add, after `_resolve_ping_url`:

```python
def _supervised() -> bool:
  """Whether ``devkit-container``'s supervisor owns the periodic ping.

  It sets ``DEVKIT_SUPERVISED_PING`` on the app at spawn when it has a ping key and slug (or a
  fixed URL), and then pings healthchecks.io itself from the heartbeat files, the tunnel's
  included. The app keeps writing its file and keeps ``failure=True`` for a known job failure;
  only the periodic liveness ping stands down, so exactly one process pings.
  """
  return bool(os.environ.get("DEVKIT_SUPERVISED_PING", "").strip())
```

4. In `_run_heartbeat_async`'s `_ping` and in `HeartbeatThread.run`'s `_ping`, pass the ping URL only when not supervised. The least invasive edit is in both `_ping` closures:

```python
    await to_thread(
      _send_heartbeat,
      heartbeat_file,
      ping_url=None if _supervised() else ping_url,
      pingkey=None if _supervised() else pingkey,
      slug=slug,
      start=start,
      failure=False,
      tz=tz,
    )
```

and the thread's equivalent with `_send_heartbeat(self._heartbeat_file, ping_url=None if _supervised() else self._ping_url, pingkey=None if _supervised() else self._pingkey, …)`. `_send_heartbeat` then writes the file and `ping_healthcheck(None, …)` is its documented no-op. `send_heartbeat` / `send_heartbeat_async` (the one-shot and `failure=True` paths) are untouched.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `uv run pytest tests/monitoring/test_heartbeat.py -q`
Expected: the whole file passes, the new tests included.

- [ ] **Step 6: Lint, type-check, commit, push, open the PR (do not merge)**

```bash
uv run ruff check src tests && uv run ruff format --check src tests && uv run pyright src
git add src/aeth_ext/monitoring/heartbeat.py tests/monitoring/test_heartbeat.py
git commit -m "feat(monitoring): read HEARTBEAT_SLUG from the environment; stand the periodic ping down under devkit-container's supervisor"
git push -u origin heartbeat-under-supervisor
gh pr create --title "feat(monitoring): heartbeat slug from the environment; periodic ping stands down under a supervisor" --body "$(cat <<'EOF'
Section 9 of devkit-container/docs/superpowers/specs/2026-09-08-container-wireguard-mode-design.md.

- `HEARTBEAT_SLUG` in the environment wins over the code constant (compose sets it to the service name; the container's supervisor reads the same variable).
- When `DEVKIT_SUPERVISED_PING` is set, `run_heartbeat_async` and `HeartbeatThread` keep writing the heartbeat file and skip the periodic ping: the supervisor pings from the files, so exactly one process pings. `send_heartbeat(failure=True)` is unchanged.

Not to be merged by the plan that opened it; the owner reviews, merges and releases.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

Expected: the PR URL. Stop here; the branch and PR are the deliverable.

---

## Self-review

**Spec coverage.** Section 3's fallback removal: Task 2. Section 9's two `aeth_ext` changes: Task 3. Section 10 steps 3 and 4: Tasks 1, 2 (releases) and 3 (the PR, unmerged). Section 13's last bullet ("the aeth_ext PR is open and its tests pass on the branch"): Task 3 Steps 5 and 6. Section 7's ownership rule (`/fail` stays the app's; exactly one process pings) is pinned by `test_a_known_failure_is_still_the_apps_to_send` and the two stand-down tests.

**Placeholders.** None. The one instruction that depends on reading existing code (how `SHUTDOWN` is reset between tests) says what to look for and what to do in either case.

**Type consistency.** `scaffold::load(ctx, venv, gates)` (no templates dir) matches the `docker::apply`/`compose` signatures Task 2 changes together, and the `Installed`/`StubVenv` shapes used in the test are the ones `tests/docker.rs` already imports. `_auto_slug` keeps its `(caller_file: str) -> str | None` signature and `@cache`, which is why the tests call `cache_clear()`.
