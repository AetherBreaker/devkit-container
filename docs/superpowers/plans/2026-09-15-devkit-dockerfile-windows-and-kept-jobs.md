# Dockerfile windows and kept release jobs: the aeth-devkit and devkit-templates implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task, inline in the session that holds it (spec 0.2 rule 4: never delegated to subagents). Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Release-order step 1 of the hub design (spec 14): `setup-project` learns the `window` marker so a project's Dockerfile additions survive every render, and the `[tool.devkit].release-workflow-jobs` key so a project's own job survives every re-render of `release.yml`; the release templates say so in their header; both packages are released so the hub can be rendered and `devkit-container`'s render check goes green.

**Architecture:** Two additive features in the `aeth-devkit-setup` crate, each a small module beside the merge shapes that already exist (`lines.rs`, `md_block.rs`): the gate pass (`gate.rs`) gains a `Window` body and frame whose marker pair is emitted instead of stripped; a new `docker/windows.rs` scans both Dockerfiles for windows and splices the project's lines into the rendered text before the diff; `context.rs` gains the key; a new `kept_jobs.rs` copies named jobs out of the existing workflow with the compose engine's line tree and inserts them under the rendered `jobs`. `devkit-templates` changes one header line and raises its devkit floor. Nothing in `devkit-container` changes; its render job is re-run as the proof.

**Tech Stack:** Rust 2024 workspace (`anyhow`, `toml_edit`, `similar`, `tempfile`; the `aeth_devkit_core::compose::tree` line tree), 2-space rustfmt; `uv`, `poe` for the releases; `gh` for PRs and CI.

**Spec:** `docs/superpowers/specs/2026-09-14-hub-fetched-peer-config-design.md` in every repository this plan touches (Task 0 puts the copies there). Sections implemented here: 3.7 (the kept job and the header line), 9.3 (the windows), 9.4 (what changes in devkit and what already holds), 13 (the `aeth-devkit` tests), 14 (release order, step 1 and its branch/rebase ruling).

## Progress tracking (owner's instruction, 2026-09-15)

- The executor ticks each step's box (`- [ ]` to `- [x]`) **as the step completes**, not at the end of the task, in the plan copy of the repository the step's commit lands in: `aeth_devkit` for Tasks 0 to 5 and 8, `devkit-templates` for Task 6, `devkit-container` for Task 7. Every "Lint and commit" step names the plan file in its `git add` so the ticks ship with the work.
- Task 8 copies the fully ticked plan over the other repositories' copies so the three match, and the owner reads progress from any of them.
- A step that cannot be completed is left unticked with a one-line note under it saying why; the plan is never edited to make it pass.

## The two rules of the spec, verbatim (0.2)

1. **This document is the source of truth for the implementation plan.** Where the plan is
   ambiguous, or the plan and this document disagree, this document decides.
2. **Where this document is silent, incomplete or contradictory on a point the implementation
   needs, the implementer stops and asks the owner.** Nobody fills a gap with their own judgement:
   not the plan's author, not the agent executing it. This includes naming, defaults, error text,
   ordering, file locations, retry counts, and "the code already does X so I will keep X". However
   small or obvious the gap looks, the owner becomes the source of truth for it before anything
   else continues, and the answer is written into this document before the plan or the code
   changes. Log lines and error wording this document does not fix verbatim are the exception:
   they are the implementer's, and the owner does not review them.

The owner's later ruling on what "stop" means (2026-09-15): stop only when the fix would change one of the owner's decisions or produce an unexpected change in externally visible behaviour; implementation details (the exact command, the ordering inside a step, log and error text, test mechanics) are the implementer's. When a stop is needed, keep working on every part of the plan that is not gated by it, then present the blockers together.

## Global constraints

- Repositories: `aeth_devkit` (GitHub `AetherBreaker/aeth-devkit`, at `D:\SFT Software Projects\SFT Workspace\aeth_devkit`), `devkit-templates` (`AetherBreaker/devkit-templates`), `devkit-container` (`AetherBreaker/devkit-container`). All three carry the same `AGENTS.md` rules: run Python under `uv run`; tests are the implementer's and never define intent; plan docs never override the owner's live word or the current code; no small single-use helpers (a body of 4 lines or fewer is inlined); comments carry the *why*, densely; Conventional Commits with the crate or module as scope (`feat(setup): …`, `docs(templates): …`); workflow names say what runs.
- The marker word is exactly `window`; the template writes `# !window builder:` … `# !end builder` and `# !window final:` … `# !end final` (spec 9.3). "Unlike every other marker its pair stays in the rendered file, so the next run can find it."
- The key is exactly `[tool.devkit].release-workflow-jobs`, "a list of job names"; the hub's job is named `peers` (spec 3.7). "A named job the existing file does not hold is reported, not an error." "The key joins `[tool.devkit]`'s known keys, so an unknown key stays an error."
- The header line of both release templates: "edits are replaced on the next run" gains "except the jobs named in `[tool.devkit].release-workflow-jobs`" (spec 3.7). The first line must keep the prefix ``# Installed and kept current by `devkit setup-project` `` because `lib.rs`'s `DEVKIT_WORKFLOW_HEADER` recognises a devkit-owned file by it.
- A window in the project's file that the template does not have "is left out of the render: the template's omission is a choice, so its lines go with it, shown in the diff and named in a `note:`, never an error" (spec 9.3, owner ruling 2026-09-15).
- Release order (spec 14): `aeth-devkit` first, then `devkit-templates`; the `devkit-container` render job is red until the `aeth-devkit` release and needs no change of its own.
- Branching (spec 14, owner ruling 2026-09-15): the `aeth-devkit` work branches from `main`, not from the open `feat/review-everything-but-docker`; once merged, that branch is rebased onto the new `main` as part of this step.
- Rust checks in `aeth_devkit` are the CI's: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`; Python: `uv sync` then `uv run pytest`. Run the targeted test while iterating and the full set once at the end of a task (AGENTS.md, Testing Workflow).
- Every command below runs from the repository root named in the step; Bash syntax (Git Bash on Windows).
- Versions at the time of writing: `aeth-devkit` 15.0.2, `devkit-templates` 1.2.2 (floor `aeth-devkit>=15.0.1`).

## Decisions this plan makes where the spec is silent

Implementation details under the owner's 2026-09-15 ruling; listed so the owner can veto any of them before execution begins. None changes an owner decision or adds a visible interface beyond what the spec names.

1. A window opens with `# !window <name>:` on its own line (never structural, never trailing content) and closes only with `# !end <name>` on its own line; a bare `# !end` on a window is an error. Windows do not nest, and a name appears once per file. The name follows the label rule of `!if … as <label>`: one word of letters, digits, `_`, `-`.
2. A window inside a false `!if` block disappears with the block, markers included. The shipped template never does this.
3. The project's window lines replace the rendered window's lines verbatim (the template's windows are empty by contract, so nothing is lost).
4. A window the template lacks (spec 9.3 as ruled) is dropped with its lines; the removal shows in the diff the step already prints and the file is replaced only on consent, and a `note:` names the window and its line count. A malformed window in the project's file (opened and never closed, a name twice) is different: the project's file cannot be read, so that is an `error:` line and the file is left whole, like a compose shape the engine cannot edit.
5. In the project's Dockerfile only a `!window` line and the `!end` naming it count; any other marker-looking line (a project's own `# !note`) is content, never a render error.
6. Change log: `kept N line(s) in window <name>` per window that carried lines, alongside the existing "replaced with the devkit template" detail.
7. A kept job is its `  <name>:` line, the comment lines directly above it at the same indent, and every deeper line up to the next job (the compose engine's `Node`), re-indented to the rendered file's job indent, preceded by one blank line, inserted at the end of the rendered `jobs` mapping. Detail: `kept job <name>`.
8. A kept-job name that is one of the template's own jobs (`build`, `publish`) is an error naming it; splicing it would duplicate a YAML key.
9. `release-workflow-jobs` must be a TOML array of non-empty strings with no whitespace and no `:`; a repeated name and a wrong type are errors naming the key. Absent means empty.
10. The note for a job the file lacks: ``.github/workflows/release.yml has no job `<name>` yet; [tool.devkit].release-workflow-jobs keeps it once it is written.``
11. `devkit-templates` raises its floor to the releasing devkit (`aeth-devkit>=15.1.0`) in the header commit, following its README ("a change that needs a new feature of that language is an aeth-devkit change first, then a commit here that raises the floor"): the header promises a behaviour only 15.1.0 has.
12. Versions: `aeth-devkit` 15.1.0 (`poe release minor`), `devkit-templates` 1.3.0 (`poe release minor`). Releases publish to SFTPyPI and create GitHub releases, so the executor asks the owner before each one.
13. Branches: `feat/dockerfile-windows-and-kept-jobs` in `aeth_devkit`, `feat/kept-jobs-header` in `devkit-templates`.

## File structure

`aeth_devkit` (all under `crates/aeth-devkit-setup/`):

- Modify `src/gate.rs`: `Body::Window`, `Frame::Window`, `parse_body`, `Gates::apply`; tests in `marker_tests` and `apply_tests`.
- Create `src/docker/windows.rs`: `Window`, `scan`, `splice`, unit tests. Modify `src/docker/mod.rs` (one `pub mod windows;`).
- Modify `src/docker/static_files.rs`: `apply` splices the project's windows before the comparison and the diff.
- Modify `src/context.rs`: `DEVKIT_KEYS`, `ProjectContext::release_workflow_jobs`, its parsing, tests. Modify every literal `ProjectContext { … }` in `src/docker/scaffold.rs`, `src/docker/static_files.rs`, `src/templates.rs` (4), `src/toml_merge.rs` (2): one new field.
- Create `src/kept_jobs.rs`: `Kept`, `splice`, unit tests. Modify `src/lib.rs` (module list; step 10b).
- Modify `tests/fixtures/docker/template.Dockerfile` (the two windows), `tests/fixtures/templates/github/workflows/release.template.yml` and `release.rust.template.yml` (the header), `tests/docker.rs` (one test), `tests/apply.rs` (one test).
- Modify `README.md` (Template language; the Docker and Release workflow bullets).
- Create `docs/superpowers/specs/2026-09-14-hub-fetched-peer-config-design.md` and `docs/superpowers/plans/2026-09-15-devkit-dockerfile-windows-and-kept-jobs.md` (copies).

`devkit-templates`:

- Modify `python/devkit_templates/templates/github/workflows/release.template.yml` and `release.rust.template.yml` (the header), `pyproject.toml` (the floor), `uv.lock` (by `uv lock`).
- Create `docs/superpowers/specs/…` and `docs/superpowers/plans/…` (copies).

`devkit-container`: nothing but the plan copy's ticks (Task 7) and the sync (Task 8).

---

### Task 0: branches and the document copies

**Files:**
- Create: `aeth_devkit/docs/superpowers/specs/2026-09-14-hub-fetched-peer-config-design.md`, `aeth_devkit/docs/superpowers/plans/2026-09-15-devkit-dockerfile-windows-and-kept-jobs.md`
- Create: `devkit-templates/docs/superpowers/specs/2026-09-14-hub-fetched-peer-config-design.md`, `devkit-templates/docs/superpowers/plans/2026-09-15-devkit-dockerfile-windows-and-kept-jobs.md`

- [ ] **Step 1: Branch `aeth_devkit` from `main`**

```bash
cd "/d/SFT Software Projects/SFT Workspace/aeth_devkit"
git status --short            # must be empty
git checkout main && git pull --ff-only
git checkout -b feat/dockerfile-windows-and-kept-jobs
```

- [ ] **Step 2: Copy the spec and this plan into `aeth_devkit` and commit**

```bash
cd "/d/SFT Software Projects/SFT Workspace/aeth_devkit"
src="/d/SFT Software Projects/SFT Workspace/devkit-container/docs/superpowers"
cp "$src/specs/2026-09-14-hub-fetched-peer-config-design.md" docs/superpowers/specs/
cp "$src/plans/2026-09-15-devkit-dockerfile-windows-and-kept-jobs.md" docs/superpowers/plans/
git add docs/superpowers
git commit -m "docs(superpowers): the hub design and its devkit plan, copied for release-order step 1

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

- [ ] **Step 3: Branch `devkit-templates` from `main` and copy the two documents**

```bash
cd "/d/SFT Software Projects/SFT Workspace/devkit-templates"
git status --short            # must be empty
git checkout main && git pull --ff-only
git checkout -b feat/kept-jobs-header
mkdir -p docs/superpowers/specs docs/superpowers/plans
src="/d/SFT Software Projects/SFT Workspace/devkit-container/docs/superpowers"
cp "$src/specs/2026-09-14-hub-fetched-peer-config-design.md" docs/superpowers/specs/
cp "$src/plans/2026-09-15-devkit-dockerfile-windows-and-kept-jobs.md" docs/superpowers/plans/
git add docs/superpowers
git commit -m "docs(superpowers): the hub design and its devkit plan, copied for release-order step 1

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

- [ ] **Step 4: Tick this task in the `aeth_devkit` copy and commit the tick**

```bash
cd "/d/SFT Software Projects/SFT Workspace/aeth_devkit"
# edit docs/superpowers/plans/2026-09-15-devkit-dockerfile-windows-and-kept-jobs.md: Task 0 boxes -> [x]
git add docs/superpowers/plans/2026-09-15-devkit-dockerfile-windows-and-kept-jobs.md
git commit -m "docs(plans): tick task 0

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 1: the `window` marker in the gate pass

**Files:**
- Modify: `crates/aeth-devkit-setup/src/gate.rs` (`Body` at line 89, `parse_body` at 100, `Frame` at 227, `Gates::apply` at 252, `marker_tests::marker_bodies_parse` at 598, `apply_tests` at 752)

**Interfaces:**
- Produces: `pub(crate) enum Body { …, Window(String) }` (the name); `Gates::apply` emits a window's two marker lines verbatim and everything between them as usual. `find_marker` and `parse_body` stay `pub(crate)` and unchanged in signature; Task 2 calls them.

- [ ] **Step 1: Write the failing tests**

In `marker_tests::marker_bodies_parse`, before the `for bad in […]` loop, add:

```rust
    assert!(matches!(parse_body("window builder:").unwrap(), Body::Window(n) if n == "builder"));
    for bad in ["window", "window builder", "window :", "window two words:", "window a/b:"] {
      assert!(parse_body(bad).is_err(), "{bad}");
    }
```

In `apply_tests`, after `lines_keep_their_indentation_and_crlf_is_normalised_to_lf`, add:

```rust
  #[test]
  fn windows_keep_their_markers_and_close_by_name() {
    // The pair survives at its own indentation; a window in a false block goes with it.
    let tpl = "FROM x\n# !window builder:\n# !end builder\n# !if t:\n  # !window final:\n  # !end final\n# !end\n# !if f:\n# !window gone:\nRUN never\n# !end gone\n# !end\nRUN c\n";
    assert_eq!(
      apply(tpl, Format::Dockerfile),
      "FROM x\n# !window builder:\n# !end builder\n  # !window final:\n  # !end final\nRUN c\n"
    );
    // Template lines inside a window render like any other line.
    assert_eq!(
      apply("# !window w:\nRUN a  # !if t\nRUN b  # !if f\n# !end w\n", Format::Dockerfile),
      "# !window w:\nRUN a\n# !end w\n"
    );
    for (tpl, needle) in [
      ("# !window w:\n# !end\n", "closes with `!end w`"),
      ("# !window w:\nRUN a\n", "not closed"),
      ("# !window w:\n# !window v:\n# !end v\n# !end w\n", "do not nest"),
      ("# !window w:\n# !end w\n# !window w:\n# !end w\n", "twice"),
      ("RUN a  # !window w:\n", "own line"),
      ("# S!window w:\nRUN a\n# !end w\n", "own line"),
      ("# !window w:\nRUN a  # !end w\n", "own line"),
      ("# !window w\n", "colon"),
      ("# !if t as b:\n# !window w:\n# !end b\n", "must close before `!end b`"),
    ] {
      let e = err(tpl, Format::Dockerfile);
      assert!(e.contains(needle), "{tpl:?}: {e}");
    }
    // A window cannot outlive the structural unit it opened in, like an explicit block.
    let e = err("[a]\n# S!if t:\n[b]\n# !window w:\ny = 1\n[c]\nx = 1\n# !end w\n", Format::Toml);
    assert!(e.contains("before the structural unit"), "{e}");
    // The sweep ignores windows: they carry no expression.
    assert!(expressions("# !window w:\n# !end w\n", Format::Dockerfile).unwrap().is_empty());
  }
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `cd "/d/SFT Software Projects/SFT Workspace/aeth_devkit" && cargo test -p aeth-devkit-setup gate::`
Expected: compile error, `no variant named Window` (the parse test), or the new apply test failing with `unknown marker`.

- [ ] **Step 3: Implement**

In `gate.rs`, add the variant to `Body` (after `PassThrough`):

```rust
  /// `window <name>:` (hub design 9.3): an always-kept explicit block whose marker pair
  /// survives rendering, so the next run can find the project's lines inside it.
  Window(String),
```

In `parse_body`, before `let Some(rest) = body.strip_prefix("if ") else {`, add:

```rust
  if let Some(rest) = body.strip_prefix("window ") {
    let Some(name) = rest.trim().strip_suffix(':') else {
      bail!("`!window` needs a trailing colon");
    };
    let name = name.trim();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
      bail!("window name `{name}` must be one word of letters, digits, `_` or `-`");
    }
    return Ok(Body::Window(name.to_string()));
  }
```

and change the unknown-marker message to:

```rust
    bail!("unknown marker `!{body}`; expected if, end, window, service-block or rule");
```

Add the frame variant to `Frame` (after `Explicit`), and make `keep()` return `true` for it:

```rust
  /// A window (hub design 9.3): always kept, both marker lines emitted, closed by name only.
  Window {
    name: String,
    line: usize,
  },
```

```rust
  fn keep(&self) -> bool {
    match self {
      Frame::Explicit { keep, .. } | Frame::Structural { keep, .. } => *keep,
      Frame::Window { .. } => true,
    }
  }
```

Replace the body of `Gates::apply` with this (the `If` and `PassThrough` arms are unchanged; the structural-close check, the `Window` arm, the `End` arm and the final unclosed check are new):

```rust
  pub fn apply(&self, text: &str, format: Format, name: &str) -> Result<String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = String::with_capacity(text.len());
    let mut frames: Vec<Frame> = Vec::new();
    let mut windows: Vec<String> = Vec::new();
    let at = |i: usize| format!("{name} line {}", i + 1);
    for (i, line) in lines.iter().enumerate() {
      // Structural units that end here close first; an explicit block or a window still
      // open inside one is an error rather than a silently extended unit.
      if let Some(pos) = frames.iter().position(|f| matches!(f, Frame::Structural { end, .. } if *end == i))
        && let Some(Frame::Explicit { name: n, line, .. } | Frame::Window { name: n, line }) = frames.get(pos + 1)
      {
        bail!(
          "{}: block `{n}` opened at line {} must close before the structural unit ends",
          at(i),
          line + 1
        );
      }
      while matches!(frames.last(), Some(Frame::Structural { end, .. }) if *end == i) {
        frames.pop();
      }
      let suppressed = frames.iter().any(|f| !f.keep());
      let Some(m) = find_marker(line, format).map_err(|e| anyhow!("{}: {e:#}", at(i)))? else {
        if !suppressed {
          out.push_str(line);
          out.push('\n');
        }
        continue;
      };
      match parse_body(m.body).map_err(|e| anyhow!("{}: {e:#}", at(i)))? {
        Body::PassThrough => {
          if !suppressed {
            out.push_str(line);
            out.push('\n');
          }
        }
        Body::If { expr, label, block } => {
          let keep = self.verdict(&expr).map_err(|e| anyhow!("{}: {e:#}", at(i)))?;
          if m.content.is_empty() {
            if !block {
              bail!("{}: `!if` on its own line needs a trailing colon", at(i));
            }
            if m.structural {
              let start = (i + 1..lines.len())
                .find(|&j| !lines[j].trim().is_empty() && find_marker(lines[j], format).ok().flatten().is_none())
                .with_context(|| format!("{}: nothing follows the structural gate", at(i)))?;
              let end = unit_end(format, &lines, start).map_err(|e| anyhow!("{}: {e:#}", at(i)))?;
              frames.push(Frame::Structural { keep, end });
            } else {
              frames.push(Frame::Explicit {
                name: label.unwrap_or(expr),
                keep,
                line: i,
              });
            }
          } else {
            if m.structural {
              bail!("{}: a trailing gate cannot be structural", at(i));
            }
            if block {
              bail!("{}: a trailing `!if` gates one line and takes no colon", at(i));
            }
            if !suppressed && keep {
              out.push_str(m.content);
              out.push('\n');
            }
          }
        }
        Body::Window(w) => {
          if !m.content.is_empty() || m.structural {
            bail!("{}: `!window` stands on its own line and is never structural", at(i));
          }
          if let Some(Frame::Window { name: open, line }) = frames.iter().find(|f| matches!(f, Frame::Window { .. })) {
            bail!("{}: windows do not nest; `{open}` opened at line {} is still open", at(i), line + 1);
          }
          if windows.contains(&w) {
            bail!("{}: window `{w}` appears twice", at(i));
          }
          windows.push(w.clone());
          if !suppressed {
            out.push_str(line);
            out.push('\n');
          }
          frames.push(Frame::Window { name: w, line: i });
        }
        Body::End(target) => {
          // A window's end is emitted whole, marker included, so it must stand alone: a
          // trailing form would print its content twice.
          let closes_window = target
            .as_ref()
            .is_some_and(|n| frames.iter().any(|f| matches!(f, Frame::Window { name, .. } if name == n)));
          if closes_window && !m.content.is_empty() {
            bail!("{}: a window's `!end` stands on its own line", at(i));
          }
          if !m.content.is_empty() && !suppressed {
            out.push_str(m.content);
            out.push('\n');
          }
          match target {
            None => match frames.last() {
              Some(Frame::Explicit { .. }) => {
                frames.pop();
              }
              Some(Frame::Window { name: w, .. }) => bail!("{}: window `{w}` closes with `!end {w}`", at(i)),
              Some(Frame::Structural { .. }) => bail!("{}: `!end` cannot close a structural block", at(i)),
              None => bail!("{}: `!end` with no open block", at(i)),
            },
            Some(n) => loop {
              match frames.pop() {
                Some(Frame::Explicit { name: open, .. }) if open == n => break,
                Some(Frame::Explicit { .. }) => {}
                Some(Frame::Window { name: open, .. }) if open == n => {
                  if !suppressed {
                    out.push_str(line);
                    out.push('\n');
                  }
                  break;
                }
                Some(Frame::Window { name: open, line: l }) => {
                  bail!("{}: window `{open}` opened at line {} must close before `!end {n}`", at(i), l + 1)
                }
                Some(Frame::Structural { .. }) => bail!("{}: `!end {n}` would close a structural block", at(i)),
                None => bail!("{}: no open block named `{n}`", at(i)),
              }
            },
          }
        }
      }
    }
    if let Some(Frame::Explicit { name: n, line, .. } | Frame::Window { name: n, line }) =
      frames.iter().find(|f| !matches!(f, Frame::Structural { .. }))
    {
      bail!("{name}: block `{n}` opened at line {} is not closed", line + 1);
    }
    Ok(out)
  }
```

Update the module doc's first line and the `apply` doc comment: "resolve every block against the swept verdicts and strip every marker but a window's pair (2.2, 2.3; hub design 9.3)".

- [ ] **Step 4: Run the tests**

Run: `cargo test -p aeth-devkit-setup gate::`
Expected: every `gate::` test passes, the two new ones included.

- [ ] **Step 5: Lint, tick, commit**

```bash
cd "/d/SFT Software Projects/SFT Workspace/aeth_devkit"
cargo fmt --all && cargo clippy -p aeth-devkit-setup --all-targets -- -D warnings
# tick Task 1 in docs/superpowers/plans/2026-09-15-devkit-dockerfile-windows-and-kept-jobs.md
git add crates/aeth-devkit-setup/src/gate.rs docs/superpowers/plans/2026-09-15-devkit-dockerfile-windows-and-kept-jobs.md
git commit -m "feat(setup): the window marker, an explicit block whose pair survives rendering

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 2: the Dockerfile windows, spliced before the diff

**Files:**
- Create: `crates/aeth-devkit-setup/src/docker/windows.rs`
- Modify: `crates/aeth-devkit-setup/src/docker/mod.rs:5-8` (module list), `crates/aeth-devkit-setup/src/docker/static_files.rs:57-99` (`apply`), `crates/aeth-devkit-setup/tests/fixtures/docker/template.Dockerfile` (two windows), `crates/aeth-devkit-setup/tests/docker.rs` (one test), `README.md` (Template language; the Docker bullet)

**Interfaces:**
- Consumes: `crate::gate::{find_marker, parse_body, Body::{Window, End}, Format::Dockerfile}` from Task 1.
- Produces: `pub struct Window { pub name: String, pub open: usize, pub close: usize }`; `pub fn scan(lines: &[&str], what: &str) -> Result<Vec<Window>>`; `pub fn splice(rendered: &str, project: &str) -> Result<Spliced>` with `pub struct Spliced { pub text: String, pub details: Vec<String>, pub notes: Vec<String> }` (the spliced LF text, the change-log details, the advisories for windows the template lacks).

- [ ] **Step 1: Write the failing unit tests**

Create `crates/aeth-devkit-setup/src/docker/windows.rs` with only the module doc and the tests for now:

```rust
//! The Dockerfile windows (hub design 9.3): the regions between `# !window <name>:` and
//! `# !end <name>` belong to the project. Every render copies their lines over unchanged
//! and replaces everything outside them.

#[cfg(test)]
mod tests {
  use super::*;

  const TPL: &str = "FROM a\n# !window builder:\n# !end builder\nFROM b\n# !window final:\n# !end final\nWORKDIR /app\n";

  #[test]
  fn scan_finds_windows_and_ignores_other_marker_like_lines() {
    let lines: Vec<&str> = TPL.lines().collect();
    assert_eq!(
      scan(&lines, "t").unwrap(),
      vec![
        Window { name: "builder".into(), open: 1, close: 2 },
        Window { name: "final".into(), open: 4, close: 5 }
      ]
    );
    // A project's own marker-looking comments, and an `!end` naming nothing open, are content.
    let odd = ["# !note to self", "# !window w:", "RUN a  # !end other", "# !!!", "# !end w"];
    assert_eq!(scan(&odd, "t").unwrap(), vec![Window { name: "w".into(), open: 1, close: 4 }]);
    assert!(scan(&[], "t").unwrap().is_empty());
    for (bad, needle) in [
      (vec!["# !window w:", "RUN a"], "not closed"),
      (vec!["# !window w:", "# !window v:", "# !end v", "# !end w"], "still open"),
      (vec!["# !window w:", "# !end w", "# !window w:", "# !end w"], "twice"),
    ] {
      let e = scan(&bad, "t").unwrap_err().to_string();
      assert!(e.contains(needle) && e.contains("t:"), "{bad:?}: {e}");
    }
  }

  #[test]
  fn the_projects_lines_replace_the_templates_and_a_missing_window_is_noted() {
    let project = "FROM old\n# !window builder:\nRUN one\n# !end builder\nFROM older\n# !window final:\nRUN two\nRUN three\n# !end final\nWORKDIR /app\n";
    let s = splice(TPL, project).unwrap();
    assert_eq!(
      s.text,
      "FROM a\n# !window builder:\nRUN one\n# !end builder\nFROM b\n# !window final:\nRUN two\nRUN three\n# !end final\nWORKDIR /app\n"
    );
    assert_eq!(s.details, vec!["kept 1 line(s) in window builder", "kept 2 line(s) in window final"]);
    assert!(s.notes.is_empty());
    // No markers in the project's file: the template as rendered, its windows empty.
    let s = splice(TPL, "FROM old\n").unwrap();
    assert!(s.text == TPL && s.details.is_empty() && s.notes.is_empty());
    // Empty windows carry nothing and report nothing.
    let s = splice(TPL, TPL).unwrap();
    assert!(s.text == TPL && s.details.is_empty() && s.notes.is_empty());
    // Only the windows the project filled are touched; order in the file does not matter.
    let only_final = "# !window final:\nRUN two\n# !end final\n";
    assert_eq!(splice(TPL, only_final).unwrap().text, TPL.replace("# !window final:\n", "# !window final:\nRUN two\n"));
    // A window the template lacks goes with its lines, and a note says so (9.3).
    let s = splice(TPL, "# !window extra:\nRUN mine\nRUN more\n# !end extra\n# !window final:\nRUN two\n# !end final\n").unwrap();
    assert_eq!(s.text, TPL.replace("# !window final:\n", "# !window final:\nRUN two\n"));
    assert_eq!(s.details, vec!["kept 1 line(s) in window final"]);
    assert_eq!(s.notes.len(), 1);
    assert!(s.notes[0].contains("window `extra`") && s.notes[0].contains("2 line(s)"), "{}", s.notes[0]);
    // A malformed window in the project's file is still an error: the file cannot be read.
    assert!(splice(TPL, "# !window w:\nRUN a\n").is_err());
  }
}
```

Add `pub mod windows;` to `src/docker/mod.rs` after `pub mod static_files;`.

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test -p aeth-devkit-setup docker::windows::`
Expected: compile errors, `cannot find function scan`, `cannot find type Window`.

- [ ] **Step 3: Implement the module**

Insert between the module doc and the tests:

```rust
use anyhow::{Result, bail};

use crate::gate::{self, Body, Format};

/// One window of a Dockerfile: the 0-based indices of its two marker lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
  pub name: String,
  pub open: usize,
  pub close: usize,
}

/// Every window of `lines`, in file order; `what` names the file in errors. Only a
/// `!window` line and the `!end` naming it count: any other marker-looking line (a project's
/// own `# !note`) is content, never a render error, since this also reads the project's file.
pub fn scan(lines: &[&str], what: &str) -> Result<Vec<Window>> {
  let mut out: Vec<Window> = Vec::new();
  let mut open: Option<(String, usize)> = None;
  for (i, line) in lines.iter().enumerate() {
    let Ok(Some(m)) = gate::find_marker(line, Format::Dockerfile) else {
      continue;
    };
    match gate::parse_body(m.body) {
      Ok(Body::Window(name)) => {
        if let Some((o, at)) = &open {
          bail!("{what}: window `{o}` opened at line {} is still open at line {}", at + 1, i + 1);
        }
        if out.iter().any(|w| w.name == name) {
          bail!("{what}: window `{name}` appears twice (line {})", i + 1);
        }
        open = Some((name, i));
      }
      Ok(Body::End(Some(name))) if open.as_ref().is_some_and(|(o, _)| *o == name) => {
        let (name, at) = open.take().expect("matched above");
        out.push(Window { name, open: at, close: i });
      }
      _ => {}
    }
  }
  if let Some((o, at)) = open {
    bail!("{what}: window `{o}` opened at line {} is not closed", at + 1);
  }
  Ok(out)
}

/// What a splice produced: the text, the change-log details (one per window that carried
/// lines) and the advisories (one per window of the project's file the template lacks).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Spliced {
  pub text: String,
  pub details: Vec<String>,
  pub notes: Vec<String>,
}

/// `rendered` with the project's windows copied in: the lines between the project's markers
/// replace the lines between the template's. Both texts are LF. A window the template lacks
/// goes with its lines (the template's omission is a choice, 9.3): the diff shows the removal
/// and a note names it. `Err` only when the project's file cannot be read as windows.
pub fn splice(rendered: &str, project: &str) -> Result<Spliced> {
  let theirs: Vec<&str> = project.lines().collect();
  let kept = scan(&theirs, "docker/Dockerfile")?;
  let mut out = Spliced {
    text: rendered.to_string(),
    ..Spliced::default()
  };
  if kept.is_empty() {
    return Ok(out);
  }
  let mut ours: Vec<String> = rendered.lines().map(str::to_string).collect();
  let mine = scan(&ours.iter().map(String::as_str).collect::<Vec<_>>(), "the Dockerfile template")?;
  let mut edits: Vec<(&Window, Vec<String>)> = Vec::new();
  for w in &kept {
    let lines: Vec<String> = theirs[w.open + 1..w.close].iter().map(|l| l.to_string()).collect();
    let Some(target) = mine.iter().find(|t| t.name == w.name) else {
      out.notes.push(format!(
        "docker/Dockerfile: window `{}` is not in the template, so its {} line(s) are left out of the render.",
        w.name,
        lines.len()
      ));
      continue;
    };
    if !lines.is_empty() {
      out.details.push(format!("kept {} line(s) in window {}", lines.len(), w.name));
    }
    edits.push((target, lines));
  }
  // Back to front, so an earlier window's indices stay valid while a later one is replaced.
  edits.sort_by_key(|(t, _)| std::cmp::Reverse(t.open));
  for (t, lines) in edits {
    ours.splice(t.open + 1..t.close, lines);
  }
  out.text = ours.join("\n");
  out.text.push('\n');
  Ok(out)
}
```

- [ ] **Step 4: Run the unit tests**

Run: `cargo test -p aeth-devkit-setup docker::windows::`
Expected: 2 passed.

- [ ] **Step 5: Give the fixture Dockerfile the two windows**

In `crates/aeth-devkit-setup/tests/fixtures/docker/template.Dockerfile`, after the last builder-stage instruction (the `RUN --mount=type=cache,target=/root/.cache/uv \ … uv sync --frozen --no-dev --no-editable $extras` block) and before `# ---- Final stage ----`, insert:

```dockerfile

# Project additions to the builder stage; setup-project renders the template around this window.
# !window builder:
# !end builder
```

In the final stage, directly before `WORKDIR /app` (the one after the `useradd` RUN), insert:

```dockerfile
# Project additions to the final stage; setup-project renders the template around this window.
# !window final:
# !end final

```

so the file reads `… --create-home nonroot\n\n# Project additions to the final stage; …\n# !window final:\n# !end final\n\nWORKDIR /app`. This mirrors `devkit-container`'s real template (spec 9.3), minus the wireguard block the fixture never had.

- [ ] **Step 6: Write the failing integration test**

Append to `crates/aeth-devkit-setup/tests/docker.rs`:

```rust
#[test]
fn window_lines_survive_a_re_render_and_a_window_the_template_lacks_is_an_error() {
  let dir = project(&["demo-app"], "https://github.com/O/Demo.git");
  let root = dir.path();
  run(root, Mode::Ask, &[], false);
  let fresh = read(root, "docker/Dockerfile");
  assert!(fresh.contains("# !window builder:\n# !end builder\n"), "empty windows on a fresh render:\n{fresh}");
  assert!(fresh.contains("# !window final:\n# !end final\n\nWORKDIR /app"), "{fresh}");

  // Lines in both windows are not drift.
  let filled = fresh
    .replace(
      "# !window builder:\n# !end builder\n",
      "# !window builder:\nRUN echo builder\n# !end builder\n",
    )
    .replace(
      "# !window final:\n# !end final\n",
      "# !window final:\nRUN apt-get install -y iptables\nCOPY rules.v4 /etc/iptables/rules.v4\n# !end final\n",
    );
  write(root, "docker/Dockerfile", &filled);
  let (changes, prompt, _) = run(root, Mode::Ask, &[], false);
  assert!(changes.is_empty() && prompt.asked.borrow().is_empty(), "{}", changes.report(root));

  // Drift outside the windows is replaced around them, and the kept lines are reported.
  write(root, "docker/Dockerfile", &filled.replace("PYTHONOPTIMIZE=1", "PYTHONOPTIMIZE=2"));
  let (changes, prompt, _) = run(root, Mode::Ask, &["replace"], false);
  assert_eq!(prompt.asked.borrow().len(), 1);
  assert_eq!(read(root, "docker/Dockerfile"), filled);
  let df = changes.files.iter().find(|f| f.path.ends_with("Dockerfile")).unwrap();
  assert!(df.details.iter().any(|d| d == "kept 2 line(s) in window final"), "{:?}", df.details);
  assert!(df.details.iter().any(|d| d == "kept 1 line(s) in window builder"), "{:?}", df.details);

  // A file rendered before the windows existed has no markers: they arrive empty.
  let old = fresh
    .replace("# Project additions to the builder stage; setup-project renders the template around this window.\n# !window builder:\n# !end builder\n\n", "")
    .replace("# Project additions to the final stage; setup-project renders the template around this window.\n# !window final:\n# !end final\n\n", "");
  assert_ne!(old, fresh);
  write(root, "docker/Dockerfile", &old);
  run(root, Mode::Ask, &["replace"], false);
  assert_eq!(read(root, "docker/Dockerfile"), fresh);

  // A window the template does not have goes with its lines: shown as drift, replaced only
  // on consent, and named in a note either way; never an error.
  let stray = fresh.replace(
    "# !end final\n\nWORKDIR /app",
    "# !end final\n\n# !window extra:\nRUN echo mine\n# !end extra\n\nWORKDIR /app",
  );
  write(root, "docker/Dockerfile", &stray);
  let (changes, prompt, _) = run(root, Mode::Ask, &[""], false);
  assert_eq!(prompt.asked.borrow().len(), 1, "the removal is a diff to consent to");
  assert!(changes.errors.is_empty(), "{:?}", changes.errors);
  assert!(
    changes.notes.iter().any(|n| n.contains("window `extra`") && n.contains("1 line(s)")),
    "{:?}",
    changes.notes
  );
  assert_eq!(read(root, "docker/Dockerfile"), stray, "kept on an empty answer");
  let (changes, _, _) = run(root, Mode::Ask, &["replace"], false);
  assert_eq!(read(root, "docker/Dockerfile"), fresh);
  assert!(changes.notes.iter().any(|n| n.contains("window `extra`")), "{:?}", changes.notes);

  // A window the project's file opens and never closes cannot be read: an `error:`, file kept.
  write(root, "docker/Dockerfile", &fresh.replace("# !end final\n", ""));
  let (changes, prompt, _) = run(root, Mode::Ask, &[], false);
  assert!(prompt.asked.borrow().is_empty());
  assert!(
    changes.errors.iter().any(|e| e.contains("window `final`") && e.contains("not closed")),
    "{:?}",
    changes.errors
  );
  assert!(changes.managed.iter().any(|p| p.ends_with("Dockerfile")), "still managed");
}
```

- [ ] **Step 7: Run it to see it fail**

Run: `cargo test -p aeth-devkit-setup --test docker window_lines_survive`
Expected: FAIL at the second run: the filled file is reported as drift (windows not yet spliced).

- [ ] **Step 8: Splice in `static_files::apply`**

Replace the loop body of `apply` in `src/docker/static_files.rs` from `let Some(original) = original else {` through the `match decision.text(&proposal) { … }` block with:

```rust
    let Some(original) = original else {
      changes.record_optional(&path, None, &rendered, vec!["created from template".into()])?;
      continue;
    };
    // The project's windows (hub design 9.3) go in before the comparison, so lines a
    // project wrote there are never drift; a window the template lacks drops out with its
    // lines, visibly (the diff) and named (a note). A file whose windows cannot be read is
    // drift the step cannot edit: an `error:` like an unmodelled compose shape, file whole.
    let spliced = match super::windows::splice(&rendered, &normalize_newlines(&original)) {
      Ok(spliced) => spliced,
      Err(e) => {
        changes.errors.push(format!("{e:#}"));
        changes.record_optional(&path, Some(&original), &original, vec![])?;
        continue;
      }
    };
    changes.notes.extend(spliced.notes);
    let (rendered, kept) = (spliced.text, spliced.details);
    if normalize_newlines(&original) == rendered {
      // Managed, unchanged. CRLF-only drift is not drift: .gitattributes owns line endings.
      changes.record_optional(&path, Some(&original), &original, vec![])?;
      continue;
    }
    // Written in the file's own line endings (the template is LF).
    let rendered = if original.contains("\r\n") {
      rendered.replace('\n', "\r\n")
    } else {
      rendered
    };
    println!("{}", unified_diff(&rel, &original, &rendered));
    let proposal = Proposal::new(&rel, format!("Replace {rel}?"), &original, &rendered);
    let decision = consent.decide(&proposal, true)?;
    let mut details = vec![decision.detail("replaced with the devkit template")];
    details.extend(kept);
    match decision.text(&proposal) {
      Some(text) => changes.record_optional(&path, Some(&original), &text, details)?,
      None => {
        changes.record_optional(&path, Some(&original), &original, vec![])?;
        println!("Kept {rel}.");
      }
    }
```

Update the module doc's first sentence: "Whole-file replacement of `docker/Dockerfile`, rendered from the template inside the installed `devkit_container` package around the project's windows, shown as a diff and applied only on consent."

- [ ] **Step 9: Run the integration test and the module's tests**

Run: `cargo test -p aeth-devkit-setup --test docker && cargo test -p aeth-devkit-setup docker::`
Expected: all pass, `window_lines_survive_a_re_render_and_a_window_the_template_lacks_is_an_error` included.

- [ ] **Step 10: Document the marker and the windows in `README.md`**

In the **Template language** section: change "A marker whose word is not `if`, `end`, `service-block` or `rule` is a render error." to "A marker whose word is not `if`, `end`, `window`, `service-block` or `rule` is a render error." Add to the code block, after the last `!end` line:

```yaml
# !window <name>:              a window: an always-kept block whose two marker lines survive rendering
# !end <name>                  closes it; a window's end is never trailing and never bare
```

After the paragraph on structural units, add:

> A window belongs to the project. Every render copies the lines the project's existing file holds between the same window's markers into the rendered file unchanged and replaces everything outside; a fresh file renders with empty windows, and a file from before the windows existed gets them empty. Windows do not nest. A window in the project's file that the template does not have is left out of the render with its lines: the template's omission is a choice, so the diff shows the removal and a `note:` names the window. Today only the Dockerfile template has windows (`builder`, after the builder stage's last instruction; `final`, before `WORKDIR /app`).

In the **Docker** bullet, after "`docker/Dockerfile` is created when missing; when present and different — ignoring CRLF/LF, and", insert "the project's `# !window` regions (see **Template language**), and" so it reads "…ignoring CRLF/LF, the project's `# !window` regions (see **Template language**), and written back in the file's own line endings…".

- [ ] **Step 11: Lint, tick, commit**

```bash
cd "/d/SFT Software Projects/SFT Workspace/aeth_devkit"
cargo fmt --all && cargo clippy -p aeth-devkit-setup --all-targets -- -D warnings
# tick Task 2 in the plan copy
git add crates/aeth-devkit-setup/src/docker/windows.rs crates/aeth-devkit-setup/src/docker/mod.rs \
  crates/aeth-devkit-setup/src/docker/static_files.rs crates/aeth-devkit-setup/tests/fixtures/docker/template.Dockerfile \
  crates/aeth-devkit-setup/tests/docker.rs README.md docs/superpowers/plans/2026-09-15-devkit-dockerfile-windows-and-kept-jobs.md
git commit -m "feat(setup): render the Dockerfile around the project's windows

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 3: the `release-workflow-jobs` key

**Files:**
- Modify: `crates/aeth-devkit-setup/src/context.rs` (fields at 51-66, `DEVKIT_KEYS` at 70, the parse after `templates_dir` at ~172-186, the struct literal at ~188-208, tests at ~490)
- Modify: the eight `ProjectContext { … }` literals: `src/docker/scaffold.rs` (1), `src/docker/static_files.rs` (1), `src/templates.rs` (4), `src/toml_merge.rs` (2)

**Interfaces:**
- Produces: `pub release_workflow_jobs: Vec<String>` on `ProjectContext` (empty when the key is absent).

- [ ] **Step 1: Write the failing test**

In `context.rs`'s test module, after `release_workflow_is_on_unless_tool_devkit_turns_it_off`, add:

```rust
  #[test]
  fn release_workflow_jobs_is_a_list_of_job_names() {
    let dir = tempfile::tempdir().unwrap();
    let py = dir.path().join("pyproject.toml");
    std::fs::write(&py, "[project]\nname = \"p\"\n").unwrap();
    assert!(ProjectContext::discover(dir.path()).unwrap().release_workflow_jobs.is_empty());
    std::fs::write(
      &py,
      "[project]\nname = \"p\"\n\n[tool.devkit]\nrelease-workflow-jobs = [\"peers\", \"docs\"]\n",
    )
    .unwrap();
    assert_eq!(
      ProjectContext::discover(dir.path()).unwrap().release_workflow_jobs,
      vec!["peers".to_string(), "docs".to_string()]
    );
    for (bad, needle) in [
      ("release-workflow-jobs = \"peers\"", "list of job names"),
      ("release-workflow-jobs = [1]", "list of job names"),
      ("release-workflow-jobs = [\"\"]", "not a job name"),
      ("release-workflow-jobs = [\"two words\"]", "not a job name"),
      ("release-workflow-jobs = [\"a:\"]", "not a job name"),
      ("release-workflow-jobs = [\"peers\", \"peers\"]", "twice"),
    ] {
      std::fs::write(&py, format!("[project]\nname = \"p\"\n\n[tool.devkit]\n{bad}\n")).unwrap();
      let err = ProjectContext::discover(dir.path()).unwrap_err().to_string();
      assert!(err.contains("release-workflow-jobs") && err.contains(needle), "{bad}: {err}");
    }
  }
```

- [ ] **Step 2: Run it to see it fail**

Run: `cargo test -p aeth-devkit-setup context::`
Expected: compile error, `no field release_workflow_jobs`.

- [ ] **Step 3: Implement**

In `ProjectContext`, after the `release_workflow` field's doc and declaration, add:

```rust
  /// `[tool.devkit].release-workflow-jobs`: the names of jobs the project writes into
  /// `release.yml` itself; each is copied out of the existing file into every re-render
  /// (hub design 3.7). Empty when absent.
  pub release_workflow_jobs: Vec<String>,
```

Change the known keys:

```rust
const DEVKIT_KEYS: &[&str] = &["release-workflow", "release-workflow-jobs", "templates-dir"];
```

After the `let templates_dir = match … ;` block in `discover`, add:

```rust
    let release_workflow_jobs: Vec<String> = match devkit.and_then(|d| d.get("release-workflow-jobs")) {
      None => vec![],
      Some(item) => {
        let shown = item.to_string();
        let bad = || anyhow!("[tool.devkit].release-workflow-jobs must be a list of job names, got {}", shown.trim());
        let names = item
          .as_array()
          .ok_or_else(bad)?
          .iter()
          .map(|v| v.as_str().map(str::to_string).ok_or_else(bad))
          .collect::<Result<Vec<_>>>()?;
        for (i, n) in names.iter().enumerate() {
          if n.is_empty() || n.contains(char::is_whitespace) || n.contains(':') {
            bail!("[tool.devkit].release-workflow-jobs: {n:?} is not a job name");
          }
          if names[..i].contains(n) {
            bail!("[tool.devkit].release-workflow-jobs lists `{n}` twice");
          }
        }
        names
      }
    };
```

and `release_workflow_jobs,` to the `Ok(Self { … })` literal after `release_workflow,`.

In each of the eight test literals (`scaffold.rs` `ctx`, `static_files.rs` `render_substitutes_python_dir_from_the_installed_package`, `templates.rs` four `ctx` helpers, `toml_merge.rs` two), add `release_workflow_jobs: vec![],` directly after `release_workflow: true,`. Find them with:

```bash
grep -rn "release_workflow: true," crates/aeth-devkit-setup/src
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p aeth-devkit-setup`
Expected: everything compiles and passes (the literals are the only other compile sites; `tests/*.rs` build contexts through `discover`).

- [ ] **Step 5: Lint, tick, commit**

```bash
cd "/d/SFT Software Projects/SFT Workspace/aeth_devkit"
cargo fmt --all && cargo clippy -p aeth-devkit-setup --all-targets -- -D warnings
# tick Task 3 in the plan copy
git add crates/aeth-devkit-setup/src docs/superpowers/plans/2026-09-15-devkit-dockerfile-windows-and-kept-jobs.md
git commit -m "feat(setup): read [tool.devkit].release-workflow-jobs

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 4: the kept jobs, spliced under the rendered `jobs`

**Files:**
- Create: `crates/aeth-devkit-setup/src/kept_jobs.rs`
- Modify: `crates/aeth-devkit-setup/src/lib.rs:4-20` (module list), `:224-243` (step 10b), `crates/aeth-devkit-setup/tests/fixtures/templates/github/workflows/release.template.yml:1` and `release.rust.template.yml:1` (the header), `crates/aeth-devkit-setup/tests/apply.rs` (one test), `README.md` (the Release workflow bullet)

**Interfaces:**
- Consumes: `ctx.release_workflow_jobs` from Task 3; `aeth_devkit_core::compose::tree::{split_lines, top_level, child, child_indent, re_indent, apply_edits, Edit::Insert}` (existing).
- Produces: `pub struct Kept { pub text: String, pub details: Vec<String>, pub notes: Vec<String> }`; `pub fn splice(rendered: &str, existing: &str, names: &[String]) -> Result<Kept>`.

- [ ] **Step 1: Write the failing unit tests**

Create `crates/aeth-devkit-setup/src/kept_jobs.rs` with the module doc and tests:

```rust
//! The kept release jobs (hub design 3.7): every job named in
//! `[tool.devkit].release-workflow-jobs` is copied out of the project's `release.yml` into
//! the rendered one, under `jobs` after the template's own, so the devkit-owned file can
//! carry a job the project wrote.

#[cfg(test)]
mod tests {
  use super::*;

  const RENDERED: &str = "# header\nname: Release\non:\n  release:\n    types: [published]\n\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: build\n\n  publish:\n    needs: build\n    runs-on: ubuntu-latest\n    steps:\n      - run: publish\n";
  const PEERS: &str = "  # The hub's bundle.\n  peers:\n    runs-on: ubuntu-latest\n    permissions:\n      contents: write\n    steps:\n      - run: peers\n";

  fn names(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
  }

  #[test]
  fn a_named_job_is_copied_under_jobs_with_its_comment_and_nested_lines() {
    let existing = format!("{RENDERED}\n{PEERS}\n");
    let k = splice(RENDERED, &existing, &names(&["peers"])).unwrap();
    assert_eq!(k.text, format!("{RENDERED}\n{PEERS}"));
    assert_eq!(k.details, vec!["kept job peers"]);
    assert!(k.notes.is_empty());
    // Idempotent: the result spliced again is the same text.
    assert_eq!(splice(RENDERED, &k.text, &names(&["peers"])).unwrap().text, k.text);
    // Found after other jobs and re-indented when the existing file used four spaces.
    let four = "jobs:\n    other:\n        runs-on: x\n    peers:\n        runs-on: y\n        steps:\n            - run: z\n";
    let k = splice(RENDERED, four, &names(&["peers"])).unwrap();
    assert!(
      k.text.ends_with("      - run: publish\n\n  peers:\n    runs-on: y\n    steps:\n      - run: z\n"),
      "{}",
      k.text
    );
    // Two names keep their order; one present and one absent give one detail and one note.
    let k = splice(RENDERED, &existing, &names(&["docs", "peers"])).unwrap();
    assert_eq!(k.details, vec!["kept job peers"]);
    assert_eq!(k.notes.len(), 1);
    assert!(k.notes[0].contains("no job `docs` yet"), "{}", k.notes[0]);
  }

  #[test]
  fn a_missing_job_is_a_note_and_a_template_job_name_is_refused() {
    let k = splice(RENDERED, "name: mine\n", &names(&["peers"])).unwrap();
    assert_eq!(k.text, RENDERED);
    assert!(k.details.is_empty());
    assert!(k.notes[0].contains("no job `peers` yet"), "{:?}", k.notes);
    assert_eq!(splice(RENDERED, "", &names(&["peers"])).unwrap().text, RENDERED);
    let e = splice(RENDERED, RENDERED, &names(&["publish"])).unwrap_err().to_string();
    assert!(e.contains("`publish`") && e.contains("template"), "{e}");
    let e = splice("name: x\n", "", &names(&["peers"])).unwrap_err().to_string();
    assert!(e.contains("jobs"), "{e}");
  }
}
```

Add `pub mod kept_jobs;` to `lib.rs`'s module list between `pub mod json_merge;` and `pub mod lines;`.

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test -p aeth-devkit-setup kept_jobs::`
Expected: compile error, `cannot find function splice`.

- [ ] **Step 3: Implement the module**

Insert between the module doc and the tests:

```rust
use anyhow::{Result, bail};

use aeth_devkit_core::compose::tree::{self, Edit};

pub struct Kept {
  pub text: String,
  /// One change-log detail per job copied.
  pub details: Vec<String>,
  /// One advisory per named job the existing file does not hold.
  pub notes: Vec<String>,
}

/// `rendered` with the named jobs of `existing` spliced under its `jobs`, in the order named.
/// Both are read as line trees (the compose engine's): a job is its `<name>:` line, the
/// comment lines directly above it at the same indent, and every deeper line up to the next
/// job, re-indented to the rendered file's job indent with one blank line above.
pub fn splice(rendered: &str, existing: &str, names: &[String]) -> Result<Kept> {
  let ours = tree::split_lines(rendered);
  let Some(jobs) = tree::top_level(&ours, "jobs") else {
    bail!("the rendered release workflow has no top-level `jobs:` key");
  };
  let theirs = tree::split_lines(existing);
  let their_jobs = tree::top_level(&theirs, "jobs");
  let indent = tree::child_indent(&ours, &jobs);
  let mut details = Vec::new();
  let mut notes = Vec::new();
  let mut block: Vec<String> = Vec::new();
  for name in names {
    if tree::child(&ours, &jobs, name).is_some() {
      bail!("[tool.devkit].release-workflow-jobs names `{name}`, a job of the devkit template's own; rename the project's job");
    }
    let Some(job) = their_jobs.as_ref().and_then(|j| tree::child(&theirs, j, name)) else {
      notes.push(format!(
        ".github/workflows/release.yml has no job `{name}` yet; [tool.devkit].release-workflow-jobs keeps it once it is written."
      ));
      continue;
    };
    let mut start = job.line;
    while start > 0 {
      let above = &theirs[start - 1];
      if !(above.trim_start().starts_with('#') && above.len() - above.trim_start().len() == job.indent) {
        break;
      }
      start -= 1;
    }
    block.push(String::new());
    block.extend(tree::re_indent(&theirs[start..job.end], job.indent, indent));
    details.push(format!("kept job {name}"));
  }
  let text = if block.is_empty() {
    rendered.to_string()
  } else {
    // `jobs.end` excludes trailing blank lines, so the block lands under the last job.
    tree::apply_edits(rendered, &[Edit::Insert { at: jobs.end, lines: block }])
  };
  Ok(Kept { text, details, notes })
}
```

- [ ] **Step 4: Run the unit tests**

Run: `cargo test -p aeth-devkit-setup kept_jobs::`
Expected: 2 passed.

- [ ] **Step 5: Change the fixture headers**

In both `crates/aeth-devkit-setup/tests/fixtures/templates/github/workflows/release.template.yml` and `release.rust.template.yml`, replace line 1

```yaml
# Installed and kept current by `devkit setup-project`; edits are replaced on the next run.
```

with the two lines

```yaml
# Installed and kept current by `devkit setup-project`; edits are replaced on the next run,
# except the jobs named in `[tool.devkit].release-workflow-jobs`.
```

Confirm nothing else in the crate depends on the old first line as a whole (only the prefix constant does):

```bash
grep -rn "replaced on the next run" crates/ python/ tests/ README.md
```

Expected: the two fixture files only (plus nothing in `src/`; `DEVKIT_WORKFLOW_HEADER` is the prefix).

- [ ] **Step 6: Write the failing integration test**

Append to `crates/aeth-devkit-setup/tests/apply.rs`:

```rust
#[test]
fn a_named_job_is_kept_through_the_re_render_and_a_missing_one_is_only_noted() {
  let dir = make_project_without_docker();
  let root = dir.path();
  let py = read(root, "pyproject.toml");
  write(
    root,
    "pyproject.toml",
    &format!("{py}\n[tool.devkit]\n  release-workflow-jobs = [\"peers\"]\n"),
  );
  // Not written yet: a note, not an error, and the template renders with the new header.
  let changes = run(root, false).unwrap();
  let wf = read(root, ".github/workflows/release.yml");
  assert!(
    wf.starts_with(
      "# Installed and kept current by `devkit setup-project`; edits are replaced on the next run,\n# except the jobs named in `[tool.devkit].release-workflow-jobs`.\n"
    ),
    "{wf}"
  );
  assert!(changes.notes.iter().any(|n| n.contains("no job `peers` yet")), "{:?}", changes.notes);
  assert!(read(root, "pyproject.toml").contains("release-workflow-jobs"), "the merge keeps the key");

  // The job written by hand, comment and nested steps included, is not drift.
  let job = "  # The hub's bundle.\n  peers:\n    runs-on: ubuntu-latest\n    permissions:\n      contents: write\n    steps:\n      - run: echo peers\n";
  write(root, ".github/workflows/release.yml", &format!("{wf}\n{job}"));
  let changes = run(root, false).unwrap();
  assert!(changes.is_empty(), "the kept job is not drift: {}", changes.report(root));

  // Drift outside the job is replaced; the job rides along and is reported.
  write(
    root,
    ".github/workflows/release.yml",
    &format!("{}\n{job}", wf.replace("ubuntu-latest", "ubuntu-22.04")),
  );
  let changes = run(root, false).unwrap();
  let f = changes.files.iter().find(|f| f.path.ends_with("release.yml")).unwrap();
  assert!(f.details.iter().any(|d| d == "kept job peers"), "{:?}", f.details);
  assert_eq!(read(root, ".github/workflows/release.yml"), format!("{wf}\n{job}"));

  // A name the template already uses is refused, naming it.
  write(
    root,
    "pyproject.toml",
    &format!("{py}\n[tool.devkit]\n  release-workflow-jobs = [\"publish\"]\n"),
  );
  let err = run(root, false).unwrap_err().to_string();
  assert!(err.contains("`publish`"), "{err}");
}
```

- [ ] **Step 7: Run it to see it fail**

Run: `cargo test -p aeth-devkit-setup --test apply a_named_job_is_kept`
Expected: FAIL at the header assertion or at "the kept job is not drift" (the job is replaced away).

- [ ] **Step 8: Splice in `lib.rs` step 10b**

Replace, inside `if ctx.release_workflow { … }`, the lines from `let rendered = templates::load(…)?;` through `changes.record_optional(&path, original.as_deref(), &rendered, details)?;` with:

```rust
    let mut rendered = templates::load(templates_dir, template_name, ctx, templates::Escape::None, &gates)?;
    let original = read_optional(&path)?;
    let devkit_owned = original.as_deref().is_some_and(|o| o.starts_with(DEVKIT_WORKFLOW_HEADER));
    let first_install = !devkit_owned;
    let mut details = if original.is_none() {
      vec![]
    } else {
      vec!["replaced with the devkit release workflow".into()]
    };
    // The project's own jobs ride along (hub design 3.7); a name the file lacks yet is a note.
    if !ctx.release_workflow_jobs.is_empty() {
      let kept = kept_jobs::splice(&rendered, original.as_deref().unwrap_or(""), &ctx.release_workflow_jobs)?;
      rendered = kept.text;
      details.extend(kept.details);
      changes.notes.extend(kept.notes);
    }
    changes.record_optional(&path, original.as_deref(), &rendered, details)?;
```

Extend the step's comment: after "so drift is replaced and reported." add "The jobs named in `[tool.devkit].release-workflow-jobs` are the exception, copied out of the existing file (`kept_jobs`)."

- [ ] **Step 9: Run the integration tests**

Run: `cargo test -p aeth-devkit-setup --test apply`
Expected: all pass, the new test and `release_workflow_is_installed_and_replaced_on_drift` (which now sees the two-line header) included.

- [ ] **Step 10: Document the key in `README.md`**

In the **Release workflow** bullet, after "any drift is replaced and reported.", insert:

> The jobs named in `[tool.devkit].release-workflow-jobs` (a list of job names) are the exception: each is copied out of the existing file into the rendered one, under `jobs` after the template's own, and reported as `kept job <name>`; a named job the file does not hold yet is a `note:`, and a name the template itself uses is an error.

- [ ] **Step 11: Lint, tick, commit**

```bash
cd "/d/SFT Software Projects/SFT Workspace/aeth_devkit"
cargo fmt --all && cargo clippy -p aeth-devkit-setup --all-targets -- -D warnings
# tick Task 4 in the plan copy
git add crates/aeth-devkit-setup/src/kept_jobs.rs crates/aeth-devkit-setup/src/lib.rs \
  crates/aeth-devkit-setup/tests/fixtures/templates/github/workflows crates/aeth-devkit-setup/tests/apply.rs \
  README.md docs/superpowers/plans/2026-09-15-devkit-dockerfile-windows-and-kept-jobs.md
git commit -m "feat(setup): keep the jobs named in release-workflow-jobs through every re-render

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 5: verification, PR, merge and the `aeth-devkit` 15.1.0 release

**Files:** none new.

- [ ] **Step 1: The CI's checks, locally**

```bash
cd "/d/SFT Software Projects/SFT Workspace/aeth_devkit"
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
uv sync && uv run pytest
git diff --exit-code -- python/aeth_devkit/_tasks_generated.py
```

Expected: every command exits 0. (The Python suite and the task table are untouched by this plan; running them is the pre-merge full suite of AGENTS.md.)

- [ ] **Step 2: The shipped template's windows, by inspection**

The fixture of Task 2 mirrors `devkit-container`'s real `template.Dockerfile`; confirm the two agree on the windows' names and places, since the real one is only rendered end to end by Task 7:

```bash
grep -n "!window\|!end builder\|!end final\|WORKDIR /app\|---- Final"   "/d/SFT Software Projects/SFT Workspace/devkit-container/python/devkit_container/template.Dockerfile"   crates/aeth-devkit-setup/tests/fixtures/docker/template.Dockerfile
```

Expected: in both files, `# !window builder:` / `# !end builder` sit before `# ---- Final stage ----`, and `# !window final:` / `# !end final` sit directly before the final stage's `WORKDIR /app`.

- [ ] **Step 3: Push, open the PR, watch CI**

```bash
cd "/d/SFT Software Projects/SFT Workspace/aeth_devkit"
git push -u origin feat/dockerfile-windows-and-kept-jobs
gh pr create --base main --title "feat(setup): Dockerfile windows and kept release jobs" --body-file - <<'EOF'
## Summary

Release-order step 1 of the WireGuard hub design (`docs/superpowers/specs/2026-09-14-hub-fetched-peer-config-design.md`, sections 3.7, 9.3, 9.4):

- The `window` marker: `# !window <name>:` … `# !end <name>` is an always-kept block whose marker pair survives rendering. The Dockerfile step copies the lines a project wrote inside each window of its existing file into the same window of the rendered file, before the diff; a window the template lacks is left out with its lines, shown in the diff and named in a note.
- `[tool.devkit].release-workflow-jobs`: the named jobs are copied out of the existing `release.yml` into every re-render, under `jobs` after the template's own, and reported; a name the file lacks yet is a note.
- The release templates' header says so (fixtures here; `devkit-templates` follows in its own PR).

## Test plan

- [ ] `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`
- [ ] `uv run pytest`
- [ ] CI green, the `Templates` job included

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
gh pr checks --watch
```

Expected: every job green. The `Templates` job renders `devkit-templates` `main` (old header) through this tree, which is fine: the header is a comment.

- [ ] **Step 4: Merge (owner's call), then release 15.1.0 (owner's go-ahead first)**

The owner merges on GitHub, or says to. Then:

```bash
cd "/d/SFT Software Projects/SFT Workspace/aeth_devkit"
git checkout main && git pull --ff-only
uv run poe release --dry-run minor "Dockerfile windows and kept release jobs"
```

Show the owner the dry-run plan and ask before the real run; a release publishes to SFTPyPI and creates a GitHub release:

```bash
uv run poe release minor "Dockerfile windows and kept release jobs"
```

Expected: the command bumps to 15.1.0, tags, and waits for the release workflow to attach and publish; it exits 0.

- [ ] **Step 5: Tick and commit**

```bash
cd "/d/SFT Software Projects/SFT Workspace/aeth_devkit"
# tick Task 5 in the plan copy (on main, after the merge)
git add docs/superpowers/plans/2026-09-15-devkit-dockerfile-windows-and-kept-jobs.md
git commit -m "docs(plans): tick task 5

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
git push
```

---

### Task 6: `devkit-templates`: the header line, the floor, the 1.3.0 release

**Files:**
- Modify: `python/devkit_templates/templates/github/workflows/release.template.yml:1`, `release.rust.template.yml:1`, `pyproject.toml` (`[project].dependencies`), `uv.lock`

- [ ] **Step 1: The header in both templates**

On branch `feat/kept-jobs-header` (Task 0), replace line 1 of both files

```yaml
# Installed and kept current by `devkit setup-project`; edits are replaced on the next run.
```

with

```yaml
# Installed and kept current by `devkit setup-project`; edits are replaced on the next run,
# except the jobs named in `[tool.devkit].release-workflow-jobs`.
```

- [ ] **Step 2: Raise the floor and re-lock**

In `pyproject.toml`, change `dependencies    = ["aeth-devkit>=15.0.1"]` to `dependencies    = ["aeth-devkit>=15.1.0"]` (keep the alignment). Then, with 15.1.0 on the index (Task 5):

```bash
cd "/d/SFT Software Projects/SFT Workspace/devkit-templates"
uv lock
uv sync
git diff --stat
```

Expected: `uv.lock` now resolves `aeth-devkit` at 15.1.0 or newer; nothing else moves.

- [ ] **Step 3: Render locally through the new devkit**

```bash
cd "/d/SFT Software Projects/SFT Workspace/devkit-templates"
bash ci/render.sh python "aeth-devkit==15.1.0"
bash ci/render.sh docker "aeth-devkit==15.1.0"
```

Expected: both end with `render ok: … through devkit 15.1.0`, and the rendered `.github/workflows/release.yml` in the scratch project starts with the two-line header.

- [ ] **Step 4: Tick, commit, PR, CI, merge**

```bash
cd "/d/SFT Software Projects/SFT Workspace/devkit-templates"
# tick Task 6 steps 1 to 3 in docs/superpowers/plans/2026-09-15-devkit-dockerfile-windows-and-kept-jobs.md
git add python/devkit_templates/templates/github/workflows pyproject.toml uv.lock docs/superpowers/plans/2026-09-15-devkit-dockerfile-windows-and-kept-jobs.md
git commit -m "feat(templates): the release header names the kept jobs; floor aeth-devkit 15.1.0

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
git push -u origin feat/kept-jobs-header
gh pr create --base main --title "feat(templates): the release header names the kept jobs" --body-file - <<'EOF'
## Summary

Release-order step 1 of the WireGuard hub design (spec 3.7, copied under `docs/superpowers/specs/`): the release workflow header gains "except the jobs named in `[tool.devkit].release-workflow-jobs`", the behaviour aeth-devkit 15.1.0 implements, and the floor rises to that release.

## Test plan

- [ ] CI: every render matrix cell green through the floor (15.1.0) and the newest devkit

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
gh pr checks --watch
```

Expected: every matrix cell green. The owner merges.

- [ ] **Step 5: Release 1.3.0 (owner's go-ahead first)**

```bash
cd "/d/SFT Software Projects/SFT Workspace/devkit-templates"
git checkout main && git pull --ff-only
uv run poe release --dry-run minor "The release header names the kept jobs"
```

Ask the owner, then:

```bash
uv run poe release minor "The release header names the kept jobs"
```

Expected: 1.3.0 published; exit 0. Tick this step and push the tick:

```bash
git add docs/superpowers/plans/2026-09-15-devkit-dockerfile-windows-and-kept-jobs.md
git commit -m "docs(plans): tick task 6

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
git push
```

---

### Task 7: the `devkit-container` render job goes green

**Files:** none; the plan copy in `devkit-container` is ticked.

- [ ] **Step 1: Re-run the last `main` CI run of `devkit-container`**

Its `ci/render.sh` installs the newest `aeth-devkit` from the index, so the release of Task 5 is all it needs:

```bash
cd "/d/SFT Software Projects/SFT Workspace/devkit-container"
id="$(gh run list --branch main --workflow ci.yml --limit 1 --json databaseId -q '.[0].databaseId')"
gh run rerun "$id"
gh run watch "$id" --exit-status
gh run view "$id" --json jobs -q '.jobs[] | "\(.conclusion)\t\(.name)"'
```

Expected: every job `success`, the `Render: dry-run both modes …` job included, with the run's log showing `devkit 15.1.0` or newer and both docker files listed.

- [ ] **Step 2: Tick and commit in `devkit-container`**

```bash
cd "/d/SFT Software Projects/SFT Workspace/devkit-container"
git checkout main && git pull --ff-only
# tick Task 7 in docs/superpowers/plans/2026-09-15-devkit-dockerfile-windows-and-kept-jobs.md
git add docs/superpowers/plans/2026-09-15-devkit-dockerfile-windows-and-kept-jobs.md
git commit -m "docs(plans): tick task 7, the render job is green on devkit 15.1.0

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
git push
```

---

### Task 8: rebase the review branch, sync the plan copies, report

**Files:** the three plan copies.

- [ ] **Step 1: Rebase `feat/review-everything-but-docker` onto the new `main` (spec 14 ruling)**

```bash
cd "/d/SFT Software Projects/SFT Workspace/aeth_devkit"
git checkout feat/review-everything-but-docker
git fetch origin
git rebase origin/main
```

If the rebase stops on a conflict, resolve it keeping both sides' intent (this plan's changes are additive: new modules, one new field, new match arms), `git add` the files and `git rebase --continue`; a conflict that cannot be resolved without changing the review branch's own work is a stop for the owner. Then:

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git log --oneline origin/main..HEAD     # the branch's own two commits, replayed
```

Ask the owner before the force push (it rewrites the remote branch), then:

```bash
git push --force-with-lease
git checkout main
```

- [ ] **Step 2: Sync the fully ticked plan to every copy**

The `aeth_devkit` copy holds the ticks for Tasks 0 to 5 and 8; merge them by hand with the `devkit-templates` copy (Task 6) and the `devkit-container` copy (Task 7) into one file in which every box is ticked, then write that file over all three copies and commit each:

```bash
plan=docs/superpowers/plans/2026-09-15-devkit-dockerfile-windows-and-kept-jobs.md
for r in aeth_devkit devkit-templates devkit-container; do
  cd "/d/SFT Software Projects/SFT Workspace/$r"
  git checkout main && git pull --ff-only
  # write the merged, fully ticked plan to $plan
  git add "$plan"
  git commit -m "docs(plans): the devkit plan fully ticked, synced across the three repositories

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
  git push
done
```

- [ ] **Step 3: Report to the owner**

State, in this order: the two releases and their versions; the `devkit-container` render job's result; the rebase result; the spec's next release-order step (14 step 3, the `wireguard-hub` repository, which gets its own plan from sections 3, 4 and 16 of the same spec); and that the spec and plan copies stay in all three repositories until the whole multi-stage change has landed (owner's instruction, 2026-09-15).

---

## Self-review

**Spec coverage.** 3.7: the key (Task 3), the splice after the template's jobs and the change-log report (Task 4), "a named job the existing file does not hold is reported, not an error" (Task 4's notes), "the key joins the known keys, so an unknown key stays an error" (Task 3, existing test kept), the header line in `devkit-templates` (Task 6) and in the fixture (Task 4), "nothing else in setup-project or the release command changes" (no other file touched). 9.3: the marker word, its pair surviving (Task 1), the copy of window lines into the same window, empty windows on a fresh file and on a file from before the windows, a window the template lacks left out and named in a note (Task 2). 9.4: the marker word joining the accepted four, the Dockerfile step splicing before the diff (Tasks 1, 2); the stale-devkit guard needs no code (the old `unknown marker` error is what fires today). 13, `aeth-devkit`: every listed assertion has a test named above. 14 step 1: the release order (Tasks 5, 6), the branch from `main` and the rebase (Tasks 0, 8). `devkit-container`'s render job as the external proof (Task 7).

**Placeholders.** None: every code step carries its code, every command its expected result. The real template's end-to-end render is Task 7, through the released devkit, since `setup-project` only reads the container template from a venv.

**Type consistency.** `Body::Window(String)` (Task 1) is matched as `Body::Window(name)` in Task 2; `windows::splice(&str, &str) -> Result<(String, Vec<String>)>` is called with `(&rendered, &normalize_newlines(&original))` in Task 2 Step 8; `ProjectContext::release_workflow_jobs: Vec<String>` (Task 3) is passed as `&ctx.release_workflow_jobs` to `kept_jobs::splice(&str, &str, &[String]) -> Result<Kept>` in Task 4; `Kept { text, details, notes }` is consumed field by field there.
