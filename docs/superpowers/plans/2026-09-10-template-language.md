# Template Language Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `setup`'s `setup-project:` markers with the `# !` template language of the spec's section 2: Python gate expressions evaluated by Monty, structural and explicit blocks, rule annotations in the compose template, a generic HEAD-vs-working-copy refusal, and the compose scaffold read from the `devkit_container` package. Then rewrite every template in `devkit-templates` to the new syntax and release both.

**Architecture:** Two new modules in `aeth-devkit`'s `setup` crate: `gate_eval` (the Monty seam: one function, no Monty type escapes it) and `gate` (markers, blocks, structural units, the expression sweep, the `Gates` verdict table, the refusal). Every template passes through `Gates::apply` inside `templates::load`; the structural gating in `md_block` and `toml_merge`, the `aeth-ext` predicate in `scaffold`, and the `RULES` table in `compose_rules` are deleted. `devkit-templates` is rewritten in the same syntax and raises its `aeth-devkit` floor to the release that reads it.

**Tech Stack:** Rust 2024 (workspace `aeth-devkit`, crate `aeth-devkit-setup`), `monty` + `monty-types` 0.0.23 (pinned exactly), `toml_edit`, `anyhow`; the `devkit-templates` package (pure Python package data, `uv_build`).

**Spec:** `devkit-container/docs/superpowers/specs/2026-09-08-container-wireguard-mode-design.md`, sections 1, 2, 3 (the fallback half), 10 (step 1), 12 (the `setup` paragraph), 13 and 14. This plan is step 1 of the spec's release order; the container plan (`2026-09-10-container-supervisor-wireguard.md`) is step 2 and depends on the releases this plan ends with.

## Global Constraints

- Repos are sibling checkouts under one workspace folder (`D:\SFT Software Projects` on the authoring machine). `aeth-devkit` and `aeth_ext` are cloned; `devkit-templates` may not be: Task 1 clones it only if absent (spec section 1: decided at execution time).
- Every Python or uv invocation runs under `uv run`; `uv add`, `uv remove` and `uv lock` are refused by the project hook: use `uv sync`, `poe lock`, `setup-project`.
- `cargo fmt --all --check` and `cargo clippy --all-targets -- -D warnings` must pass on every commit (CI runs both on Windows and Linux).
- Marker syntax, exactly (spec 2.1–2.3, 2.6): `# !if <expr>:` / `# !if <expr> as <label>:` / `# S!if <expr>:` / `<line>  # !if <expr>` / `# !end` / `# !end <name>` / `<line>  # !end [name]` / `# !service-block:` … `# !end service-block` / `# !rule <kind>`. Markdown uses `<!-- !… -->` and `<!-- S!… -->`; JSONC uses `// !…`. The space before `!` is mandatory. Unknown marker words are render errors.
- Expressions see `keys("<dotted.path>")`, `dep("<name>")`, and the bare flags `rust`, `publish_index`, `docker_files`. Unknown names are render errors, never false (spec 2.4).
- The Monty crates are pinned with `=`; nothing outside `gate_eval.rs` names a Monty type (spec 2.5).
- No dual-syntax period: the old `setup-project:` markers are not accepted after this ships (spec 2.7).
- Commit messages follow Conventional Commits with the attribution trailer `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`.
- Don't run the full test suite while iterating on a branch; run the targeted tests named in each step, and the whole suite once at the end of the plan (AGENTS.md).

---

### Task 1: Branches, and the templates checkout

**Files:**
- Create (maybe): `../devkit-templates/` (a clone)

**Interfaces:**
- Produces: branch `template-language` in `aeth-devkit`; branch `template-language` in `devkit-templates`.

- [x] **Step 1: Clone `devkit-templates` if it is not beside the other repos**

```bash
cd "/d/SFT Software Projects"
[ -d devkit-templates ] || gh repo clone AetherBreaker/devkit-templates
cd devkit-templates && git status --short && git log --oneline -1
```

Expected: a clean checkout on `main`. If the directory already existed, skip the clone and make sure it is on `main` with no uncommitted changes (`git status --short` prints nothing).

- [x] **Step 2: Sync both environments and branch**

```bash
cd "/d/SFT Software Projects/aeth-devkit" && uv sync && git switch -c template-language
cd "/d/SFT Software Projects/devkit-templates" && uv sync && git switch -c template-language
```

Expected: both `git branch --show-current` print `template-language`.

- [x] **Step 3: Confirm the toolchain builds the workspace as it is**

```bash
cd "/d/SFT Software Projects/aeth-devkit" && cargo build -p aeth-devkit-setup
```

Expected: `Finished`. Nothing to commit yet.

---

### Task 2: The evaluator seam (`gate_eval.rs`)

**Files:**
- Modify: `aeth-devkit/Cargo.toml` (workspace dependencies)
- Modify: `aeth-devkit/crates/aeth-devkit-setup/Cargo.toml`
- Create: `aeth-devkit/crates/aeth-devkit-setup/src/gate_eval.rs`
- Modify: `aeth-devkit/crates/aeth-devkit-setup/src/lib.rs` (add `pub mod gate_eval;`)

**Interfaces:**
- Produces:
  - `pub enum Value { None, Bool(bool), Int(i64), Float(f64), Str(String), List(Vec<Value>), Dict(Vec<(String, Value)>) }`
  - `pub struct World<'a> { pub flags: &'a [(&'static str, bool)], pub keys: &'a dyn Fn(&str) -> Value, pub dep: &'a dyn Fn(&str) -> bool }`
  - `pub fn evaluate(expr: &str, world: &World) -> anyhow::Result<bool>`

- [x] **Step 1: Add the dependencies**

In `aeth-devkit/Cargo.toml` under `[workspace.dependencies]` add (keep the table's alignment style):

```toml
  monty       = "=0.0.23"
  monty-types = "=0.0.23"
```

In `crates/aeth-devkit-setup/Cargo.toml` under `[dependencies]` add:

```toml
  monty       = { workspace = true }
  monty-types = { workspace = true }
```

Run: `cargo build -p aeth-devkit-setup`
Expected: Monty compiles (a minute or two the first time; `target/` caches it afterwards).

- [x] **Step 2: Write the failing tests**

Create `crates/aeth-devkit-setup/src/gate_eval.rs` with only the test module for now:

```rust
//! The evaluator seam: one function, Monty behind it. Nothing outside this module names a
//! Monty type, so `rustpython-vm` could replace it by rewriting this file alone (spec 2.5).

#[cfg(test)]
mod tests {
  use super::*;

  fn world<'a>(flags: &'a [(&'static str, bool)], keys: &'a dyn Fn(&str) -> Value, dep: &'a dyn Fn(&str) -> bool) -> World<'a> {
    World { flags, keys, dep }
  }

  fn keys(path: &str) -> Value {
    match path {
      "tool.docker.wireguard" => Value::Bool(true),
      "tool.docker.services" => Value::List(vec![Value::Str("smoke".into())]),
      "project.name" => Value::Str("demo-app".into()),
      "tool.ruff.lint.per-file-ignores" => Value::Dict(vec![("tests/**".into(), Value::List(vec![Value::Str("D1".into())]))]),
      "project.version" => Value::Float(1.5),
      "tool.pytest.count" => Value::Int(3),
      _ => Value::None,
    }
  }

  fn dep(name: &str) -> bool {
    name == "aeth-ext"
  }

  #[test]
  fn python_semantics_over_the_three_kinds_of_name() {
    let flags = [("rust", true), ("publish_index", false)];
    let w = world(&flags, &keys, &dep);
    for (expr, want) in [
      ("keys(\"tool.docker.wireguard\")", true),
      ("keys(\"tool.docker.missing\")", false),
      ("not keys(\"tool.docker.missing\")", true),
      ("\"smoke\" in (keys(\"tool.docker.services\") or ())", true),
      ("\"other\" in (keys(\"tool.docker.services\") or ())", false),
      ("keys(\"project.name\") != \"aeth-ext\" and dep(\"aeth-ext\")", true),
      ("dep(\"mypy\")", false),
      ("rust and not publish_index", true),
      ("keys(\"tool.pytest.count\") > 2", true),
      ("keys(\"project.version\") == 1.5", true),
      ("\"tests/**\" in keys(\"tool.ruff.lint.per-file-ignores\")", true),
      ("keys(\"project.name\").startswith(\"demo\")", true),
      ("any(s.startswith(\"sm\") for s in keys(\"tool.docker.services\"))", true),
      ("keys(\"tool.\" + \"docker.wireguard\")", true),
    ] {
      assert_eq!(evaluate(expr, &w).unwrap(), want, "{expr}");
    }
  }

  #[test]
  fn errors_name_the_expression_and_the_cause() {
    let flags = [("rust", true)];
    let w = world(&flags, &keys, &dep);
    let err = evaluate("nope", &w).unwrap_err().to_string();
    assert!(err.contains("nope") && err.contains("unknown name"), "{err}");
    let err = evaluate("keys(1)", &w).unwrap_err().to_string();
    assert!(err.contains("one string argument"), "{err}");
    let err = evaluate("rust and", &w).unwrap_err().to_string();
    assert!(err.contains("rust and"), "{err}");
    let err = evaluate("keys(\"x\").bogus()", &w).unwrap_err().to_string();
    assert!(err.contains("bogus"), "{err}");
  }
}
```

Add `pub mod gate_eval;` to `src/lib.rs`'s module list (alphabetical: after `pub mod format;`).

- [x] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p aeth-devkit-setup gate_eval`
Expected: compile error, `Value`, `World` and `evaluate` not found.

- [x] **Step 4: Write the implementation**

Above the test module in `gate_eval.rs`:

```rust
use anyhow::{Result, anyhow, bail};
use monty::{MontyRun, RunProgress};
use monty_types::{CompileOptions, ExtFunctionResult, MontyObject, PrintWriter, ResourceTracker};

/// A `pyproject.toml` value as a gate sees it. The seam's own type: `gate` builds it from
/// `toml_edit`, this module turns it into whatever the evaluator wants.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
  None,
  Bool(bool),
  Int(i64),
  Float(f64),
  Str(String),
  List(Vec<Value>),
  Dict(Vec<(String, Value)>),
}

/// What an expression can ask about: bare boolean names, and the two functions.
pub struct World<'a> {
  pub flags: &'a [(&'static str, bool)],
  pub keys: &'a dyn Fn(&str) -> Value,
  pub dep: &'a dyn Fn(&str) -> bool,
}

/// The truth value of `expr`, a Python expression, in `world`. `keys` and `dep` are host
/// functions: the sandbox suspends on the call, the closure answers, the run resumes.
/// Any failure (syntax, an unknown name, a bad argument, a runtime exception) is an error
/// naming the expression: an unknown name is never silently false (spec 2.4).
pub fn evaluate(expr: &str, world: &World) -> Result<bool> {
  let code = format!("bool({expr})");
  let mut names: Vec<String> = world.flags.iter().map(|(n, _)| (*n).to_string()).collect();
  let mut values: Vec<MontyObject> = world.flags.iter().map(|(_, v)| MontyObject::Bool(*v)).collect();
  for f in ["keys", "dep"] {
    names.push(f.to_string());
    values.push(MontyObject::Function {
      name: f.to_string(),
      docstring: None,
    });
  }
  let fail = |e: &dyn std::fmt::Display| anyhow!("gate `{expr}`: {e}");
  let run = MontyRun::new(code, "gate", names, CompileOptions::default()).map_err(|e| fail(&e))?;
  let mut progress = run
    .start(values, ResourceTracker::default(), PrintWriter::Disabled)
    .map_err(|e| fail(&e))?;
  loop {
    progress = match progress {
      RunProgress::Complete(MontyObject::Bool(b)) => return Ok(b),
      RunProgress::Complete(other) => bail!("gate `{expr}`: bool() returned {other:?}"),
      RunProgress::FunctionCall(call) => {
        let arg = match call.args.as_slice() {
          [MontyObject::String(s)] if call.kwargs.is_empty() => s.clone(),
          _ => bail!("gate `{expr}`: {}() takes one string argument", call.function_name),
        };
        let result = match call.function_name.as_str() {
          "keys" => to_monty(&(world.keys)(&arg)),
          "dep" => MontyObject::Bool((world.dep)(&arg)),
          other => bail!("gate `{expr}`: unknown function {other}"),
        };
        call
          .resume(ExtFunctionResult::Return(result), PrintWriter::Disabled)
          .map_err(|e| fail(&e))?
      }
      RunProgress::NameLookup(lookup) => bail!("gate `{expr}`: unknown name `{}`", lookup.name),
      RunProgress::OsCall(_) | RunProgress::ResolveFutures(_) => bail!("gate `{expr}`: gates cannot do I/O or async"),
    };
  }
}

fn to_monty(v: &Value) -> MontyObject {
  match v {
    Value::None => MontyObject::None,
    Value::Bool(b) => MontyObject::Bool(*b),
    Value::Int(i) => MontyObject::Int(*i),
    Value::Float(f) => MontyObject::Float(*f),
    Value::Str(s) => MontyObject::String(s.clone()),
    Value::List(items) => MontyObject::List(items.iter().map(to_monty).collect()),
    Value::Dict(pairs) => MontyObject::dict(
      pairs
        .iter()
        .map(|(k, v)| (MontyObject::String(k.clone()), to_monty(v)))
        .collect::<Vec<_>>(),
    ),
  }
}
```

If the Monty API differs from this in detail (the crate is 0.0.x), read `target/`'s copy of `monty/src/run_progress.rs` and `monty-types/src/object.rs` and adapt the four call sites (`MontyRun::new`, `start`, `FunctionCall::resume`, `MontyObject::dict`); keep the public signatures of this module unchanged.

- [x] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p aeth-devkit-setup gate_eval`
Expected: 2 passed. If `keys(1)` produces a different wording, adjust the implementation's message, not the test's expectation: the test states the contract.

- [x] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/aeth-devkit-setup/Cargo.toml crates/aeth-devkit-setup/src/gate_eval.rs crates/aeth-devkit-setup/src/lib.rs
git commit -m "feat(setup): evaluate gate expressions with Monty behind a one-function seam"
```

---

### Task 3: Markers and formats (`gate.rs`, part 1)

**Files:**
- Create: `aeth-devkit/crates/aeth-devkit-setup/src/gate.rs`
- Modify: `aeth-devkit/crates/aeth-devkit-setup/src/lib.rs` (add `pub mod gate;`)

**Interfaces:**
- Produces:
  - `pub enum Format { Toml, Yaml, Dockerfile, Markdown, Jsonc, Plain }` with `pub fn for_target(name: &str) -> Format`
  - `pub(crate) struct Marker<'a> { structural: bool, body: &'a str, content: &'a str }` (`content` is the text before a trailing marker; empty for an own-line marker)
  - `pub(crate) fn find_marker(line: &str, format: Format) -> Result<Option<Marker<'_>>>`
  - `pub(crate) enum Body { If { expr: String, label: Option<String>, block: bool }, End(Option<String>), PassThrough }` with `pub(crate) fn parse_body(body: &str) -> Result<Body>`

- [x] **Step 1: Write the failing tests**

Create `src/gate.rs`:

```rust
//! The template language (spec section 2): `# !` markers, explicit and structural blocks,
//! Python gate expressions, and the `Gates` verdict table every template is rendered through.

use std::collections::HashMap;

use anyhow::{Context as _, Result, bail};
use toml_edit::{DocumentMut, Item};

use crate::context::normalize_dist_name;
use crate::gate_eval::{self, Value, World};

/// The file type a template renders into: it fixes the comment syntax a marker uses and
/// what a structural block's unit is (2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
  Toml,
  Yaml,
  Dockerfile,
  Markdown,
  Jsonc,
  /// `#` comments, no structural unit (`.env`, `.gitignore`, `.gitattributes`).
  Plain,
}

impl Format {
  /// By extension of a target or template file name; `Dockerfile` anywhere in the name.
  pub fn for_target(name: &str) -> Format {
    let lower = name.to_ascii_lowercase();
    if lower.contains("dockerfile") {
      Format::Dockerfile
    } else if lower.ends_with(".toml") {
      Format::Toml
    } else if lower.ends_with(".yaml") || lower.ends_with(".yml") {
      Format::Yaml
    } else if lower.ends_with(".md") {
      Format::Markdown
    } else if lower.ends_with(".json") || lower.ends_with(".jsonc") {
      Format::Jsonc
    } else {
      Format::Plain
    }
  }

  /// The comment opener a marker sits in, and the closer a markdown one needs.
  fn leader(self) -> (&'static str, &'static str) {
    match self {
      Format::Markdown => ("<!-- ", " -->"),
      Format::Jsonc => ("// ", ""),
      _ => ("# ", ""),
    }
  }
}

#[cfg(test)]
mod marker_tests {
  use super::*;

  #[test]
  fn formats_come_from_the_file_name() {
    assert_eq!(Format::for_target("pyproject.toml"), Format::Toml);
    assert_eq!(Format::for_target("pyproject.template.toml"), Format::Toml);
    assert_eq!(Format::for_target("docker/compose.yaml"), Format::Yaml);
    assert_eq!(Format::for_target("github/workflows/release.rust.template.yml"), Format::Yaml);
    assert_eq!(Format::for_target("template.Dockerfile"), Format::Dockerfile);
    assert_eq!(Format::for_target("AGENTS.md"), Format::Markdown);
    assert_eq!(Format::for_target("vscode/settings.template.jsonc"), Format::Jsonc);
    assert_eq!(Format::for_target(".mcp.json"), Format::Jsonc);
    assert_eq!(Format::for_target("env"), Format::Plain);
    assert_eq!(Format::for_target("template.gitignore"), Format::Plain);
  }

  #[test]
  fn markers_are_found_in_each_comment_syntax_at_any_indent() {
    let m = find_marker("    # !if rust:", Format::Yaml).unwrap().unwrap();
    assert!(!m.structural && m.body == "if rust:" && m.content.is_empty());
    let m = find_marker("# S!if dep(\"mypy\"):", Format::Toml).unwrap().unwrap();
    assert!(m.structural && m.body == "if dep(\"mypy\"):");
    let m = find_marker("  <!-- S!if dep(\"aeth-ext\"): -->", Format::Markdown).unwrap().unwrap();
    assert!(m.structural && m.body == "if dep(\"aeth-ext\"):");
    let m = find_marker("  // !end", Format::Jsonc).unwrap().unwrap();
    assert!(m.body == "end");
    // Trailing: the content before the marker is kept, trailing whitespace trimmed.
    let m = find_marker("  - FOO=bar   # !if not publish_index", Format::Yaml).unwrap().unwrap();
    assert_eq!(m.content, "  - FOO=bar");
    assert_eq!(m.body, "if not publish_index");
    let m = find_marker("Some text. <!-- !end -->", Format::Markdown).unwrap().unwrap();
    assert_eq!((m.content, m.body), ("Some text.", "end"));
  }

  #[test]
  fn shebangs_comments_and_other_syntaxes_are_not_markers() {
    assert!(find_marker("#!/bin/sh", Format::Plain).unwrap().is_none());
    assert!(find_marker("# a comment with ! in it", Format::Yaml).unwrap().is_none());
    assert!(find_marker("# setup-project: if-x", Format::Yaml).unwrap().is_none());
    assert!(find_marker("// !end", Format::Yaml).unwrap().is_none(), "wrong leader for yaml");
    assert!(find_marker("# !end", Format::Markdown).unwrap().is_none(), "wrong leader for markdown");
    let err = find_marker("<!-- !end", Format::Markdown).unwrap_err().to_string();
    assert!(err.contains("-->"), "{err}");
  }

  #[test]
  fn marker_bodies_parse() {
    assert!(matches!(parse_body("if rust:").unwrap(), Body::If { expr, label: None, block: true } if expr == "rust"));
    assert!(matches!(parse_body("if rust").unwrap(), Body::If { block: false, .. }));
    match parse_body("if dep(\"a\") or keys(\"x\") as heartbeat:").unwrap() {
      Body::If { expr, label, block } => {
        assert_eq!(expr, "dep(\"a\") or keys(\"x\")");
        assert_eq!(label.as_deref(), Some("heartbeat"));
        assert!(block);
      }
      other => panic!("{other:?}"),
    }
    // ` as ` inside a string literal is not a label.
    match parse_body("if keys(\"a\") == \"x as y\":").unwrap() {
      Body::If { expr, label, .. } => {
        assert_eq!(expr, "keys(\"a\") == \"x as y\"");
        assert!(label.is_none());
      }
      other => panic!("{other:?}"),
    }
    assert!(matches!(parse_body("end").unwrap(), Body::End(None)));
    assert!(matches!(parse_body("end docker.wireguard").unwrap(), Body::End(Some(n)) if n == "docker.wireguard"));
    assert!(matches!(parse_body("service-block:").unwrap(), Body::PassThrough));
    assert!(matches!(parse_body("end service-block").unwrap(), Body::PassThrough));
    assert!(matches!(parse_body("rule presence").unwrap(), Body::PassThrough));
    for bad in ["fi rust:", "if:", "if  :", "ends", "if rust as :", "if rust as bad label:"] {
      assert!(parse_body(bad).is_err(), "{bad}");
    }
  }
}
```

Add `pub mod gate;` to `src/lib.rs` (before `pub mod gate_eval;`).

- [x] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p aeth-devkit-setup gate::marker_tests`
Expected: compile errors for `find_marker`, `Marker`, `Body`, `parse_body`.

- [x] **Step 3: Write the implementation**

Insert between the `impl Format` block and the tests:

```rust
/// A marker on one line: the text after `!`, and the content before it when it trails a
/// content line (empty when the line is the marker alone).
#[derive(Debug)]
pub(crate) struct Marker<'a> {
  pub structural: bool,
  pub body: &'a str,
  pub content: &'a str,
}

/// The marker on `line`, if any. `Err` only for a markdown marker that never closes.
pub(crate) fn find_marker(line: &str, format: Format) -> Result<Option<Marker<'_>>> {
  let (open, close) = format.leader();
  for (bang, structural) in [("S!", true), ("!", false)] {
    let prefix = format!("{open}{bang}");
    let Some(idx) = line.find(&prefix) else { continue };
    let content = line[..idx].trim_end();
    let rest = &line[idx + prefix.len()..];
    let body = if close.is_empty() {
      rest
    } else {
      rest
        .trim_end()
        .strip_suffix(close.trim_start())
        .with_context(|| format!("marker `{}` is not closed with `{}`", line.trim(), close.trim()))?
    };
    return Ok(Some(Marker {
      structural,
      body: body.trim(),
      content,
    }));
  }
  Ok(None)
}

/// What a marker says.
#[derive(Debug)]
pub(crate) enum Body {
  /// `if <expr>[ as <label>]` with (`block`) or without a trailing colon.
  If { expr: String, label: Option<String>, block: bool },
  /// `end` or `end <name>`.
  End(Option<String>),
  /// A compose annotation (`service-block:`, `end service-block`, `rule <kind>`): not the
  /// gate pass's business, emitted unchanged for the scaffold parser (2.6).
  PassThrough,
}

pub(crate) fn parse_body(body: &str) -> Result<Body> {
  if body == "service-block:" || body == "end service-block" || body.starts_with("rule ") {
    return Ok(Body::PassThrough);
  }
  if body == "end" {
    return Ok(Body::End(None));
  }
  if let Some(name) = body.strip_prefix("end ") {
    return Ok(Body::End(Some(name.trim().to_string())));
  }
  let Some(rest) = body.strip_prefix("if ") else {
    bail!("unknown marker `!{body}`; expected if, end, service-block or rule");
  };
  let (rest, block) = match rest.strip_suffix(':') {
    Some(r) => (r, true),
    None => (rest, false),
  };
  let (expr, label) = split_label(rest.trim());
  if expr.is_empty() {
    bail!("`!if` has no expression");
  }
  if let Some(l) = &label
    && (l.is_empty() || !l.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'))
  {
    bail!("label `{l}` after `as` must be one word of letters, digits, `_` or `-`");
  }
  Ok(Body::If { expr, label, block })
}

/// Split `<expr> as <label>` at the last ` as ` outside a string literal.
fn split_label(s: &str) -> (String, Option<String>) {
  let bytes = s.as_bytes();
  let mut quote: Option<u8> = None;
  let mut split: Option<usize> = None;
  let mut i = 0;
  while i < bytes.len() {
    let b = bytes[i];
    match quote {
      Some(q) => {
        if b == b'\\' {
          i += 1;
        } else if b == q {
          quote = None;
        }
      }
      None => {
        if b == b'"' || b == b'\'' {
          quote = Some(b);
        } else if s[i..].starts_with(" as ") {
          split = Some(i);
        }
      }
    }
    i += 1;
  }
  match split {
    Some(at) => (s[..at].trim().to_string(), Some(s[at + 4..].trim().to_string())),
    None => (s.to_string(), None),
  }
}
```

- [x] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p aeth-devkit-setup gate::marker_tests`
Expected: 4 passed.

- [x] **Step 5: Commit**

```bash
git add crates/aeth-devkit-setup/src/gate.rs crates/aeth-devkit-setup/src/lib.rs
git commit -m "feat(setup): parse the # ! markers of the template language"
```

---

### Task 4: Verdicts, the sweep and the refusal (`gate.rs`, part 2)

**Files:**
- Modify: `aeth-devkit/crates/aeth-devkit-setup/src/gate.rs`
- Modify: `aeth-devkit/crates/aeth-devkit-setup/src/context.rs` (extract `dependencies_of`)

**Interfaces:**
- Consumes: `gate_eval::{evaluate, Value, World}` (Task 2), `find_marker`/`parse_body` (Task 3).
- Produces:
  - `pub struct Facts { pub rust: bool, pub docker_files: bool }`
  - `pub struct Gates` with `pub fn build(templates: &[(String, String)], doc: &DocumentMut, head: Option<&DocumentMut>, facts: &Facts) -> Result<Gates>` and `pub fn verdict(&self, expr: &str) -> Result<bool>`
  - `pub fn expressions(text: &str, format: Format) -> Result<Vec<String>>`
  - `pub fn collect_dir(dir: &Path) -> Result<Vec<(String, String)>>` (every file under `dir`, as `(relative name with '/', text)`)
  - in `context.rs`: `pub fn dependencies_of(doc: &DocumentMut) -> HashSet<String>` (the body of the `collect` closure in `discover`, which now calls it)

- [x] **Step 1: Extract `dependencies_of` in `context.rs`**

Replace the `let mut dependencies = HashSet::new(); let mut collect = …; collect(…); … ` block in `ProjectContext::discover` with `let dependencies = dependencies_of(&doc);` and add, next to `services_key`:

```rust
/// Normalised names of every declared dependency: `[project].dependencies`, every
/// optional-dependencies extra, every dependency group.
pub fn dependencies_of(doc: &toml_edit::DocumentMut) -> HashSet<String> {
  let mut dependencies = HashSet::new();
  let mut collect = |item: Option<&toml_edit::Item>| {
    if let Some(arr) = item.and_then(|i| i.as_array()) {
      for v in arr.iter() {
        if let Some(s) = v.as_str() {
          dependencies.insert(dependency_name(s));
        }
      }
    }
  };
  collect(doc.get("project").and_then(|p| p.get("dependencies")));
  if let Some(t) = doc
    .get("project")
    .and_then(|p| p.get("optional-dependencies"))
    .and_then(|o| o.as_table_like())
  {
    for (_, v) in t.iter() {
      collect(Some(v));
    }
  }
  if let Some(t) = doc.get("dependency-groups").and_then(|g| g.as_table_like()) {
    for (_, v) in t.iter() {
      collect(Some(v));
    }
  }
  dependencies
}
```

Run: `cargo test -p aeth-devkit-setup context`
Expected: the existing context tests still pass.

- [x] **Step 2: Write the failing tests**

Append to `gate.rs`:

```rust
#[cfg(test)]
mod gates_tests {
  use super::*;

  const PYPROJECT: &str = r#"
[project]
name = "Demo-App"
version = "1.2.3"
dependencies = ["aeth-ext[sftp]>=8", "requests"]

[dependency-groups]
dev = ["mypy>=1"]

[tool.docker]
services = ["demo-app", "worker"]
wireguard = true

[tool.ruff.lint.per-file-ignores]
"tests/**" = ["D1"]

[[tool.uv.index]]
name = "SFTPyPI"
url = "https://x/+simple"
publish-url = "https://x/"
"#;

  fn doc(s: &str) -> DocumentMut {
    s.parse().unwrap()
  }

  fn facts() -> Facts {
    Facts {
      rust: false,
      docker_files: true,
    }
  }

  #[test]
  fn the_sweep_collects_each_distinct_expression_once() {
    let text = "a\n# !if rust:\nb\n# !end\n# S!if dep(\"mypy\"):\n[t]\nx = 1  # !if rust\n# !rule exact\n";
    assert_eq!(expressions(text, Format::Toml).unwrap(), vec!["rust".to_string(), "dep(\"mypy\")".to_string()]);
    let md = "<!-- S!if dep(\"aeth-ext\"): -->\n## H\n";
    assert_eq!(expressions(md, Format::Markdown).unwrap(), vec!["dep(\"aeth-ext\")".to_string()]);
    assert!(expressions("# !bogus\n", Format::Yaml).is_err());
  }

  #[test]
  fn keys_dep_and_flags_answer_from_the_document() {
    let templates = vec![(
      "t.yaml".to_string(),
      "# !if keys(\"tool.docker.wireguard\"):\n# !end\n# !if dep(\"aeth-ext\") and dep(\"demo_app\") and dep(\"mypy\"):\n# !end\n# !if \"worker\" in keys(\"tool.docker.services\"):\n# !end\n# !if \"tests/**\" in keys(\"tool.ruff.lint.per-file-ignores\"):\n# !end\n# !if keys(\"project.version\") == \"1.2.3\":\n# !end\n# !if keys(\"tool.docker.nope\") is None:\n# !end\n# !if publish_index and docker_files and not rust:\n# !end\n# !if keys(\"tool.uv.index\")[0][\"name\"] == \"SFTPyPI\":\n# !end\n".to_string(),
    )];
    let g = Gates::build(&templates, &doc(PYPROJECT), None, &facts()).unwrap();
    for e in [
      "keys(\"tool.docker.wireguard\")",
      "dep(\"aeth-ext\") and dep(\"demo_app\") and dep(\"mypy\")",
      "\"worker\" in keys(\"tool.docker.services\")",
      "\"tests/**\" in keys(\"tool.ruff.lint.per-file-ignores\")",
      "keys(\"project.version\") == \"1.2.3\"",
      "keys(\"tool.docker.nope\") is None",
      "publish_index and docker_files and not rust",
      "keys(\"tool.uv.index\")[0][\"name\"] == \"SFTPyPI\"",
    ] {
      assert!(g.verdict(e).unwrap(), "{e}");
    }
    assert!(g.verdict("never swept").is_err());
  }

  #[test]
  fn a_gate_that_flips_between_head_and_the_working_copy_is_refused() {
    let templates = vec![("t.yaml".to_string(), "# !if keys(\"tool.docker.wireguard\"):\n# !end\n# !if dep(\"requests\"):\n# !end\n".to_string())];
    let head = doc(&PYPROJECT.replace("wireguard = true\n", ""));
    let err = Gates::build(&templates, &doc(PYPROJECT), Some(&head), &facts()).unwrap_err().to_string();
    assert!(err.contains("not committed") && err.contains("keys(\"tool.docker.wireguard\")") && err.contains("t.yaml"), "{err}");
    // A key that changed without flipping any gate does not refuse.
    let head = doc(&PYPROJECT.replace("version = \"1.2.3\"", "version = \"1.2.4\""));
    assert!(Gates::build(&templates, &doc(PYPROJECT), Some(&head), &facts()).is_ok());
    // No pyproject at HEAD: an empty document, so a gate that is true now is refused.
    let err = Gates::build(&templates, &doc(PYPROJECT), Some(&doc("")), &facts()).unwrap_err().to_string();
    assert!(err.contains("not committed"), "{err}");
  }

  #[test]
  fn an_expression_error_names_the_template() {
    let templates = vec![("AGENTS.template.md".to_string(), "<!-- S!if dep(\"a\") or nope: -->\n## H\n".to_string())];
    let err = Gates::build(&templates, &doc(PYPROJECT), None, &facts()).unwrap_err().to_string();
    assert!(err.contains("AGENTS.template.md") && err.contains("nope"), "{err}");
  }

  #[test]
  fn collect_dir_reads_every_file_with_slash_paths() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("docker")).unwrap();
    std::fs::write(dir.path().join("pyproject.template.toml"), "a").unwrap();
    std::fs::write(dir.path().join("docker").join("compose.template.yaml"), "b").unwrap();
    let mut got = collect_dir(dir.path()).unwrap();
    got.sort();
    assert_eq!(
      got,
      vec![
        ("docker/compose.template.yaml".to_string(), "b".to_string()),
        ("pyproject.template.toml".to_string(), "a".to_string())
      ]
    );
  }
}
```

- [x] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p aeth-devkit-setup gate::gates_tests`
Expected: compile errors for `Facts`, `Gates`, `expressions`, `collect_dir`.

- [x] **Step 4: Write the implementation**

Insert after `split_label`:

```rust
/// Facts a gate can ask about that are not in `pyproject.toml`.
#[derive(Debug, Clone)]
pub struct Facts {
  /// A `Cargo.toml` at the project root.
  pub rust: bool,
  /// A Dockerfile or compose file on disk (`ProjectContext::docker_files`).
  pub docker_files: bool,
}

/// The truth value of every gate expression in the templates of one run, evaluated once
/// against the working copy's `pyproject.toml` (2.5).
#[derive(Debug, Default)]
pub struct Gates {
  verdicts: HashMap<String, bool>,
}

impl Gates {
  /// Sweep `templates` (`(name, text)` pairs; the name picks the comment syntax and names
  /// the file in errors), evaluate each distinct expression against `doc`, and, when `head`
  /// is given, against it too: a gate whose verdicts differ is the refusal (2.5). A missing
  /// HEAD file is passed as an empty document.
  pub fn build(templates: &[(String, String)], doc: &DocumentMut, head: Option<&DocumentMut>, facts: &Facts) -> Result<Gates> {
    let mut exprs: Vec<(String, String)> = Vec::new();
    for (name, text) in templates {
      for e in expressions(text, Format::for_target(name)).with_context(|| format!("template {name}"))? {
        if !exprs.iter().any(|(_, x)| x == &e) {
          exprs.push((name.clone(), e));
        }
      }
    }
    let verdicts = verdicts(&exprs, doc, facts)?;
    if let Some(head) = head {
      let at_head = verdicts_for(&exprs, head, facts)?;
      for (name, e) in &exprs {
        if verdicts.get(e) != at_head.get(e) {
          bail!(
            "pyproject.toml is not committed: the gate `{e}` in {name} evaluates differently against HEAD; commit that change, then rerun setup-project"
          );
        }
      }
    }
    Ok(Gates { verdicts })
  }

  /// The swept verdict for `expr`; an expression the sweep never saw is a bug.
  pub fn verdict(&self, expr: &str) -> Result<bool> {
    self
      .verdicts
      .get(expr)
      .copied()
      .with_context(|| format!("gate `{expr}` was not swept before rendering"))
  }
}

fn verdicts(exprs: &[(String, String)], doc: &DocumentMut, facts: &Facts) -> Result<HashMap<String, bool>> {
  verdicts_for(exprs, doc, facts)
}

fn verdicts_for(exprs: &[(String, String)], doc: &DocumentMut, facts: &Facts) -> Result<HashMap<String, bool>> {
  let deps = crate::context::dependencies_of(doc);
  let own = doc
    .get("project")
    .and_then(|p| p.get("name"))
    .and_then(|n| n.as_str())
    .map(normalize_dist_name)
    .unwrap_or_default();
  let publish_index = aeth_devkit_core::pyproject::publish_indexes(doc)
    .map(|v| !v.is_empty())
    .unwrap_or(false);
  let flags = [
    ("rust", facts.rust),
    ("publish_index", publish_index),
    ("docker_files", facts.docker_files),
  ];
  let keys = |path: &str| lookup(doc, path);
  let dep = |name: &str| {
    let n = normalize_dist_name(name);
    deps.contains(&n) || own == n
  };
  let world = World {
    flags: &flags,
    keys: &keys,
    dep: &dep,
  };
  let mut out = HashMap::new();
  for (name, e) in exprs {
    let v = gate_eval::evaluate(e, &world).with_context(|| format!("template {name}"))?;
    out.insert(e.clone(), v);
  }
  Ok(out)
}

/// Every `!if` expression in `text`, in order, each once.
pub fn expressions(text: &str, format: Format) -> Result<Vec<String>> {
  let mut out: Vec<String> = Vec::new();
  for (i, line) in text.lines().enumerate() {
    let Some(m) = find_marker(line, format).with_context(|| format!("line {}", i + 1))? else { continue };
    if let Body::If { expr, .. } = parse_body(m.body).with_context(|| format!("line {}", i + 1))?
      && !out.contains(&expr)
    {
      out.push(expr);
    }
  }
  Ok(out)
}

/// Every file under `dir`, recursively, as `(path relative to dir with '/', text)`.
pub fn collect_dir(dir: &std::path::Path) -> Result<Vec<(String, String)>> {
  fn walk(base: &std::path::Path, dir: &std::path::Path, out: &mut Vec<(String, String)>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("listing {}", dir.display()))? {
      let path = entry?.path();
      if path.is_dir() {
        walk(base, &path, out)?;
      } else {
        let rel = path.strip_prefix(base).unwrap_or(&path).to_string_lossy().replace('\\', "/");
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        out.push((rel, text));
      }
    }
    Ok(())
  }
  let mut out = Vec::new();
  walk(dir, dir, &mut out)?;
  Ok(out)
}

/// The value at a dotted path, `None` when any segment is missing (2.4).
fn lookup(doc: &DocumentMut, path: &str) -> Value {
  let mut item: &Item = doc.as_item();
  for seg in path.split('.') {
    match item.as_table_like().and_then(|t| t.get(seg)) {
      Some(next) => item = next,
      None => return Value::None,
    }
  }
  to_value(item)
}

fn to_value(item: &Item) -> Value {
  match item {
    Item::None => Value::None,
    Item::Value(v) => scalar(v),
    Item::Table(t) => Value::Dict(t.iter().map(|(k, i)| (k.to_string(), to_value(i))).collect()),
    Item::ArrayOfTables(a) => Value::List(
      a.iter()
        .map(|t| Value::Dict(t.iter().map(|(k, i)| (k.to_string(), to_value(i))).collect()))
        .collect(),
    ),
  }
}

fn scalar(v: &toml_edit::Value) -> Value {
  match v {
    toml_edit::Value::String(s) => Value::Str(s.value().clone()),
    toml_edit::Value::Integer(i) => Value::Int(*i.value()),
    toml_edit::Value::Float(f) => Value::Float(*f.value()),
    toml_edit::Value::Boolean(b) => Value::Bool(*b.value()),
    toml_edit::Value::Datetime(d) => Value::Str(d.value().to_string()),
    toml_edit::Value::Array(a) => Value::List(a.iter().map(scalar).collect()),
    toml_edit::Value::InlineTable(t) => Value::Dict(t.iter().map(|(k, v)| (k.to_string(), scalar(v))).collect()),
  }
}
```

(`verdicts` is a thin alias kept so the working-copy call reads distinctly from the HEAD one at the call site; clippy accepts it, but if it flags the duplication, inline it.)

- [x] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p aeth-devkit-setup gate::gates_tests`
Expected: 5 passed.

- [x] **Step 6: Commit**

```bash
git add crates/aeth-devkit-setup/src/gate.rs crates/aeth-devkit-setup/src/context.rs
git commit -m "feat(setup): sweep gate expressions, evaluate them once, refuse a gate that flips against HEAD"
```

---

### Task 5: Blocks and structural units (`gate.rs`, part 3)

**Files:**
- Modify: `aeth-devkit/crates/aeth-devkit-setup/src/gate.rs`

**Interfaces:**
- Consumes: `Gates::verdict`, `find_marker`, `parse_body`.
- Produces: `impl Gates { pub fn apply(&self, text: &str, format: Format, name: &str) -> Result<String> }`; `pub(crate) fn heading_level(line: &str) -> Option<usize>` and `pub(crate) fn fence_delimiter(line: &str) -> Option<String>` (moved from `md_block.rs` in Task 6).

- [x] **Step 1: Write the failing tests**

Append to `gate.rs`:

```rust
#[cfg(test)]
mod apply_tests {
  use super::*;

  /// Gates with fixed verdicts: `t` true, `f` false.
  fn gates() -> Gates {
    let mut verdicts = HashMap::new();
    verdicts.insert("t".to_string(), true);
    verdicts.insert("f".to_string(), false);
    verdicts.insert("t or f".to_string(), true);
    Gates { verdicts }
  }

  fn apply(text: &str, format: Format) -> String {
    gates().apply(text, format, "x").unwrap()
  }

  fn err(text: &str, format: Format) -> String {
    gates().apply(text, format, "x").unwrap_err().to_string()
  }

  #[test]
  fn explicit_blocks_keep_or_drop_their_lines_and_markers_never_survive() {
    let tpl = "a\n# !if t:\nb\n# !end\n  # !if f:\n  c\n  # !end\nd\n";
    assert_eq!(apply(tpl, Format::Yaml), "a\nb\nd\n");
    assert_eq!(apply("a\n# !if f:\nb\n# !end\n", Format::Yaml), "a\n");
  }

  #[test]
  fn one_liners_gate_their_own_line() {
    let tpl = "- a  # !if t\n- b  # !if f\n- c\n";
    assert_eq!(apply(tpl, Format::Yaml), "- a\n- c\n");
  }

  #[test]
  fn trailing_ends_keep_the_line_and_close_the_block() {
    let tpl = "# !if t:\na\nb  # !end\nc\n";
    assert_eq!(apply(tpl, Format::Yaml), "a\nb\nc\n");
    let tpl = "# !if f as x:\na\nb  # !end x\nc\n";
    assert_eq!(apply(tpl, Format::Yaml), "c\n");
  }

  #[test]
  fn nesting_three_deep_with_a_named_end_unwinding_two() {
    let tpl = "# !if t:\n1\n  # !if t as mid:\n  2\n    # !if t or f:\n    3\n  # !end mid\n4\n# !end\n5\n";
    assert_eq!(apply(tpl, Format::Yaml), "1\n  2\n    3\n4\n5\n");
    // The same shape with the middle block false drops 2 and 3 but not 4.
    let tpl = "# !if t:\n1\n  # !if f as mid:\n  2\n    # !if t:\n    3\n  # !end mid\n4\n# !end\n";
    assert_eq!(apply(tpl, Format::Yaml), "1\n4\n");
    // A block is named by its expression when it has no label.
    let tpl = "# !if t:\n1\n# !if f:\n2\n# !end f\n# !end t\n";
    assert_eq!(apply(tpl, Format::Yaml), "1\n");
  }

  #[test]
  fn structural_toml_unit_runs_to_the_next_header_minus_its_comment_block() {
    let tpl = "[a]\nx = 1\n\n# S!if f:\n[b]\ny = 2\n\n# a comment above c\n# S!if t:\n[c]\nz = 3\n";
    assert_eq!(apply(tpl, Format::Toml), "[a]\nx = 1\n\n# a comment above c\n[c]\nz = 3\n");
    // A key-level gate in TOML is an explicit block or a one-liner, never structural.
    let tpl = "[a]\nx = 1  # !if f\ny = 2\n";
    assert_eq!(apply(tpl, Format::Toml), "[a]\ny = 2\n");
  }

  #[test]
  fn structural_markdown_unit_is_the_section_and_fences_hide_headings() {
    let tpl = "## Always\n\na\n\n<!-- S!if f: -->\n## Gated\n\n```bash\n# not a heading\n```\n\n### Sub\n\nb\n\n<!-- S!if t: -->\n## Kept\n\nc\n";
    assert_eq!(apply(tpl, Format::Markdown), "## Always\n\na\n\n## Kept\n\nc\n");
    let e = err("<!-- S!if t: -->\nnot a heading\n", Format::Markdown);
    assert!(e.contains("heading"), "{e}");
  }

  #[test]
  fn structural_yaml_unit_is_the_next_node_and_dockerfile_the_next_instruction() {
    let tpl = "svc:\n  # S!if f:\n  environment:\n    - A=1\n    - B=2\n  networks:\n    - x\n";
    assert_eq!(apply(tpl, Format::Yaml), "svc:\n  networks:\n    - x\n");
    // A rule line between the marker and the node stays with the node.
    let tpl = "svc:\n  # S!if t:\n  # !rule presence\n  cap_add:\n    - NET_ADMIN\n  x: 1\n";
    assert_eq!(apply(tpl, Format::Yaml), "svc:\n  # !rule presence\n  cap_add:\n    - NET_ADMIN\n  x: 1\n");
    let tpl = "FROM x\n# S!if f:\nRUN a \\\n  && b\nRUN c\n";
    assert_eq!(apply(tpl, Format::Dockerfile), "FROM x\nRUN c\n");
  }

  #[test]
  fn compose_annotations_pass_through_untouched() {
    let tpl = "services:\n# !service-block:\n  {service}:\n    # !rule exact\n    a: 1\n# !end service-block\n";
    assert_eq!(apply(tpl, Format::Yaml), tpl);
  }

  #[test]
  fn malformed_structure_is_an_error_naming_the_line() {
    for (tpl, needle) in [
      ("# !if t:\na\n", "not closed"),
      ("a\n# !end\n", "no open block"),
      ("# !if t:\n# !end nope\n", "no open block named"),
      ("# !if t\na\n# !end\n", "colon"),
      ("a  # !if t:\n", "colon"),
      ("[a]\n# S!if t:\n[b]\n# !end\n", "structural"),
      ("[a]\n# S!if t:\n[b]\n# !if t:\n[c]\nx = 1\n# !end\n", "before the structural unit"),
      ("# !bogus\n", "unknown marker"),
      ("a  # S!if t:\n", "structural"),
      ("# S!if t:\n", "nothing follows"),
    ] {
      let e = err(tpl, Format::Toml);
      assert!(e.contains(needle), "{tpl:?}: {e}");
    }
    let e = err("# S!if t:\na\n", Format::Plain);
    assert!(e.contains("not defined for"), "{e}");
  }

  #[test]
  fn lines_keep_their_indentation_and_crlf_is_normalised_to_lf() {
    assert_eq!(apply("  a\r\n  # !if t:\r\n    b\r\n  # !end\r\n", Format::Yaml), "  a\n    b\n");
  }
}
```

- [x] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p aeth-devkit-setup gate::apply_tests`
Expected: compile error, no method `apply`.

- [x] **Step 3: Write the implementation**

Insert after the `impl Gates` block (before `fn verdicts`):

```rust
/// One open block while rendering.
#[derive(Debug)]
enum Frame {
  Explicit { name: String, keep: bool, line: usize },
  /// A structural block: no end line; it closes when `end` (exclusive line index) is reached.
  Structural { keep: bool, end: usize },
}

impl Frame {
  fn keep(&self) -> bool {
    match self {
      Frame::Explicit { keep, .. } | Frame::Structural { keep, .. } => *keep,
    }
  }
}

impl Gates {
  /// Render `text`: resolve every block against the swept verdicts and strip every marker
  /// (2.2, 2.3). Line endings come out as LF; the caller restores CRLF where a file has it.
  pub fn apply(&self, text: &str, format: Format, name: &str) -> Result<String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = String::with_capacity(text.len());
    let mut frames: Vec<Frame> = Vec::new();
    let at = |i: usize| format!("{name} line {}", i + 1);
    for (i, line) in lines.iter().enumerate() {
      // Structural units that end here close first; an explicit block still open inside one
      // is an error rather than a silently extended unit.
      while matches!(frames.last(), Some(Frame::Structural { end, .. }) if *end == i) {
        frames.pop();
      }
      if let Some(pos) = frames.iter().position(|f| matches!(f, Frame::Structural { end, .. } if *end == i))
        && let Some(Frame::Explicit { name: n, line, .. }) = frames.get(pos + 1)
      {
        bail!("{}: block `{n}` opened at line {} must close before the structural unit ends", at(i), line + 1);
      }
      let suppressed = frames.iter().any(|f| !f.keep());
      let Some(m) = find_marker(line, format).with_context(|| at(i))? else {
        if !suppressed {
          out.push_str(line);
          out.push('\n');
        }
        continue;
      };
      match parse_body(m.body).with_context(|| at(i))? {
        Body::PassThrough => {
          if !suppressed {
            out.push_str(line);
            out.push('\n');
          }
        }
        Body::If { expr, label, block } => {
          let keep = self.verdict(&expr).with_context(|| at(i))?;
          if m.content.is_empty() {
            if !block {
              bail!("{}: `!if` on its own line needs a trailing colon", at(i));
            }
            if m.structural {
              let start = (i + 1..lines.len())
                .find(|&j| !lines[j].trim().is_empty() && find_marker(lines[j], format).ok().flatten().is_none())
                .with_context(|| format!("{}: nothing follows the structural gate", at(i)))?;
              let end = unit_end(format, &lines, start).with_context(|| at(i))?;
              frames.push(Frame::Structural { keep, end });
            } else {
              frames.push(Frame::Explicit {
                name: label.unwrap_or(expr),
                keep,
                line: i,
              });
            }
          } else {
            if block {
              bail!("{}: a trailing `!if` gates one line and takes no colon", at(i));
            }
            if m.structural {
              bail!("{}: a trailing gate cannot be structural", at(i));
            }
            if !suppressed && keep {
              out.push_str(m.content);
              out.push('\n');
            }
          }
        }
        Body::End(target) => {
          if !m.content.is_empty() && !suppressed {
            out.push_str(m.content);
            out.push('\n');
          }
          match target {
            None => match frames.last() {
              Some(Frame::Explicit { .. }) => {
                frames.pop();
              }
              Some(Frame::Structural { .. }) => bail!("{}: `!end` cannot close a structural block", at(i)),
              None => bail!("{}: `!end` with no open block", at(i)),
            },
            Some(n) => loop {
              match frames.pop() {
                Some(Frame::Explicit { name: open, .. }) if open == n => break,
                Some(Frame::Explicit { .. }) => {}
                Some(Frame::Structural { .. }) => bail!("{}: `!end {n}` would close a structural block", at(i)),
                None => bail!("{}: no open block named `{n}`", at(i)),
              }
            },
          }
        }
      }
    }
    if let Some(Frame::Explicit { name: n, line, .. }) = frames.iter().find(|f| matches!(f, Frame::Explicit { .. })) {
      bail!("{name}: block `{n}` opened at line {} is not closed", line + 1);
    }
    Ok(out)
  }
}

/// The exclusive end of the structural unit starting at `start` (2.3).
fn unit_end(format: Format, lines: &[&str], start: usize) -> Result<usize> {
  let n = lines.len();
  let is_marker = |l: &str| find_marker(l, format).ok().flatten().is_some();
  match format {
    Format::Toml => {
      let mut j = start + 1;
      while j < n && !lines[j].trim_start().starts_with('[') {
        j += 1;
      }
      // The blank and comment lines directly above the next header are its own decor.
      while j > start + 1 && (lines[j - 1].trim().is_empty() || lines[j - 1].trim_start().starts_with('#')) {
        j -= 1;
      }
      Ok(j)
    }
    Format::Markdown => {
      let level = heading_level(lines[start]).with_context(|| "a structural gate in markdown must precede a heading")?;
      let mut fence: Option<String> = None;
      let mut j = start + 1;
      while j < n {
        let token = fence_delimiter(lines[j]);
        match (&fence, token) {
          (None, Some(t)) => fence = Some(t),
          (Some(open), Some(t)) if t.starts_with(open.as_str()) => fence = None,
          _ => {}
        }
        if fence.is_none() && heading_level(lines[j]).is_some_and(|l| l <= level) {
          break;
        }
        j += 1;
      }
      while j > start + 1 && (lines[j - 1].trim().is_empty() || is_marker(lines[j - 1])) {
        j -= 1;
      }
      Ok(j)
    }
    Format::Yaml => {
      let indent = indent_of(lines[start]);
      let mut j = start + 1;
      while j < n && (lines[j].trim().is_empty() || indent_of(lines[j]) > indent) {
        j += 1;
      }
      Ok(j)
    }
    Format::Dockerfile => {
      let mut j = start;
      while j < n && lines[j].trim_end().ends_with('\\') {
        j += 1;
      }
      Ok((j + 1).min(n))
    }
    Format::Jsonc | Format::Plain => bail!("structural gates are not defined for {format:?} files"),
  }
}

fn indent_of(line: &str) -> usize {
  line.len() - line.trim_start().len()
}

/// The run of ``` or ~~~ opening or closing a fenced code block, if this line is one. A fence
/// closes only on a run at least as long as the one that opened it.
pub(crate) fn fence_delimiter(line: &str) -> Option<String> {
  let t = line.trim_start();
  for c in ['`', '~'] {
    let n = t.chars().take_while(|&x| x == c).count();
    if n >= 3 {
      return Some(c.to_string().repeat(n));
    }
  }
  None
}

/// `Some(n)` for an ATX heading line with `n` leading `#`s, `None` otherwise.
pub(crate) fn heading_level(line: &str) -> Option<usize> {
  let hashes = line.chars().take_while(|&c| c == '#').count();
  (1..=6).contains(&hashes).then_some(hashes).filter(|&n| line[n..].starts_with(' '))
}
```

- [x] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p aeth-devkit-setup gate::`
Expected: every `gate::` test passes (marker, gates, apply modules). Adjust error wording in the implementation until each `needle` in `malformed_structure_is_an_error_naming_the_line` matches; do not weaken the test.

- [x] **Step 5: Commit**

```bash
git add crates/aeth-devkit-setup/src/gate.rs
git commit -m "feat(setup): render explicit, one-line and structural blocks of the template language"
```

---

### Task 6: Route every template through `Gates` (`templates`, `lib`, `md_block`, `cli`, `pin`)

**Files:**
- Modify: `aeth-devkit/crates/aeth-devkit-setup/src/templates.rs`
- Modify: `aeth-devkit/crates/aeth-devkit-setup/src/lib.rs`
- Modify: `aeth-devkit/crates/aeth-devkit-setup/src/md_block.rs`
- Modify: `aeth-devkit/crates/aeth-devkit-setup/src/cli.rs`
- Modify: `aeth-devkit/crates/aeth-devkit-pin/src/lib.rs:310`

**Interfaces:**
- Consumes: `gate::{Gates, Facts, Format, collect_dir}`.
- Produces:
  - `templates::load(templates_dir, name, ctx, escape, gates: &Gates) -> Result<String>` and `templates::load_optional(…, gates: &Gates)`; `templates::gate` is deleted.
  - `pub fn gates_for(ctx: &ProjectContext, templates_dir: Option<&Path>, venv: &dyn packages::Venv, head_pyproject: Option<&str>) -> Result<gate::Gates>` in `lib.rs`.
  - `docker::apply(ctx, templates_dir, deps, gates: &Gates, changes)` (signature change consumed by Task 7).

- [x] **Step 1: `templates.rs`: gate inside `load`, delete `gate`**

Change `load` and `load_optional`:

```rust
/// Read a template (by its target name, e.g. `pyproject.toml`), render its gates, then
/// substitute the placeholders (see `substitute`).
pub fn load(templates_dir: &Path, name: &str, ctx: &ProjectContext, escape: Escape, gates: &Gates) -> Result<String> {
  let path = templates_dir.join(template_file_name(name));
  let text = std::fs::read_to_string(&path).with_context(|| format!("reading template {}", path.display()))?;
  let gated = gates.apply(&text, Format::for_target(name), name)?;
  Ok(substitute(&gated, ctx, escape))
}

pub fn load_optional(templates_dir: &Path, name: &str, ctx: &ProjectContext, escape: Escape, gates: &Gates) -> Result<Option<String>> {
  let path = templates_dir.join(template_file_name(name));
  if !path.is_file() {
    return Ok(None);
  }
  load(templates_dir, name, ctx, escape, gates).map(Some)
}
```

Add `use crate::gate::{Format, Gates};` to the imports. Delete `pub fn gate` (the `# setup-project:` block-marker function) and its `gate_tests` module (the tests using `if-publish-index` / `if-no-aeth-ext`).

- [x] **Step 2: `lib.rs`: build the gates once and pass them everywhere**

Add after `run_with`'s templates step (right after `let templates_dir = templates_dir.as_path();`):

```rust
  // 0b. The gates (spec 2.5): every expression in every template this run can render,
  //     evaluated once. The compose and Dockerfile templates come from the container
  //     package, so they are swept from there.
  let gates = gates_for(ctx, Some(templates_dir), deps.venv, None)?;
```

Then thread `&gates` into every `templates::load` / `load_optional` call in `run_with` and `load_with_rust_overlay` (which gains a `gates: &Gates` parameter), change step 8b to `docker::apply(ctx, templates_dir, deps, &gates, &mut changes)?;`, replace step 9's two lines

```rust
    let block = md_block::apply_if_dep(&template, ctx, &mut log);
    let merged = md_block::merge_managed_block(original.as_deref(), &block, &mut log)?;
```

with

```rust
    let merged = md_block::merge_managed_block(original.as_deref(), &template, &mut log)?;
```

and in step 10b replace

```rust
    let raw = templates::load(templates_dir, template_name, ctx, templates::Escape::None)?;
    let rendered = templates::gate(&raw, &|name| match name {
      "publish-index" => ctx.publish_index.is_some(),
      _ => false,
    });
```

with

```rust
    let rendered = templates::load(templates_dir, template_name, ctx, templates::Escape::None, &gates)?;
```

Add the public constructor at the end of `lib.rs` (before `read_optional`):

```rust
/// The gates for one render: every template under `templates_dir` (when given) plus the two
/// templates of the installed container package, swept and evaluated against the working
/// copy's `pyproject.toml`, and against `head_pyproject` when a committing run must refuse a
/// gate that differs (spec 2.5). `docker-pin` passes no templates dir: it renders only the
/// container's Dockerfile.
pub fn gates_for(
  ctx: &ProjectContext,
  templates_dir: Option<&Path>,
  venv: &dyn packages::Venv,
  head_pyproject: Option<&str>,
) -> Result<gate::Gates> {
  let mut templates = match templates_dir {
    Some(dir) => gate::collect_dir(dir)?,
    None => Vec::new(),
  };
  if let Some(installed) = venv.installed(&ctx.root, &packages::CONTAINER) {
    for file in [docker::static_files::TEMPLATE_FILE, docker::scaffold::TEMPLATE_FILE] {
      let path = installed.dir.join(file);
      if path.is_file() {
        templates.push((
          file.to_string(),
          std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?,
        ));
      }
    }
  }
  let text = std::fs::read_to_string(ctx.root.join("pyproject.toml")).context("reading pyproject.toml")?;
  let doc: toml_edit::DocumentMut = text.parse().context("parsing pyproject.toml")?;
  let head = head_pyproject
    .map(|t| t.parse::<toml_edit::DocumentMut>().context("parsing HEAD's pyproject.toml"))
    .transpose()?;
  gate::Gates::build(
    &templates,
    &doc,
    head.as_ref(),
    &gate::Facts {
      rust: ctx.has_rust,
      docker_files: ctx.docker_files,
    },
  )
}
```

(`docker::scaffold::TEMPLATE_FILE` is added in Task 7; until then use the literal `"compose.template.yaml"` and switch to the constant there.)

- [x] **Step 3: `md_block.rs`: delete the structural gating**

Delete `IF_DEP_MARKER`, `apply_if_dep`, `marker_dep`, `fence_delimiter`, `heading_level` (the last two now live in `gate.rs`) and every test from `if_dep_section_kept_when_dependency_present` to the end of the file (`if_dep_section_dropped_when_dependency_absent`, `if_dep_marker_without_heading_closes_at_any_heading`, `if_dep_gated_section_at_end_of_block`, `a_blank_line_between_marker_and_heading_does_not_disable_the_gate`, `adjacent_if_dep_sections_are_gated_independently`, `a_hash_inside_a_code_fence_does_not_close_a_gated_section`, `a_code_fence_is_preserved_when_the_dependency_is_present`). Keep `merge_managed_block` and its tests; drop the now-unused `ctx` helper and `use crate::context::ProjectContext;` if nothing else uses them. Update the module doc's mention of `if-dep`.

- [x] **Step 4: `cli.rs`: the refusal covers every gate**

Replace the `if committing { refuse_uncommitted_services(&root)?; }` block with:

```rust
  if committing {
    refuse_uncommitted_services(&root)?;
    // Every gate the run will evaluate must agree between HEAD and the working copy (spec
    // 2.5). Before the first run installs the templates package there is nothing to sweep
    // but the container's templates; the `services` check above still applies.
    let head = aeth_devkit_core::git::head_blob(&root, "pyproject.toml")?
      .map(|b| String::from_utf8_lossy(&b).into_owned())
      .unwrap_or_default();
    let venv = crate::packages::SystemVenv;
    let templates_dir = templates_override
      .clone()
      .or_else(|| venv.installed(&root, &crate::packages::TEMPLATES).map(|i| i.dir.join("templates")));
    crate::gates_for(&ctx, templates_dir.as_deref(), &venv, Some(&head))?;
  }
```

Update the doc comment on `refuse_uncommitted_services` to say it is the `services` half of the refusal and that `gates_for` is the other.

- [x] **Step 5: `pin`: build gates for the Dockerfile render**

In `crates/aeth-devkit-pin/src/lib.rs` replace

```rust
  let rendered = render(&ctx, deps.venv)?.expect("the installed package renders");
```

with

```rust
  let gates = aeth_devkit_setup::gates_for(&ctx, None, deps.venv, None)?;
  let rendered = render(&ctx, deps.venv, &gates)?.expect("the installed package renders");
```

(`render`'s new signature lands in Task 7; this line compiles after it.)

- [x] **Step 6: Build**

Run: `cargo build -p aeth-devkit-setup`
Expected: errors only in `docker/` (Task 7's files: `scaffold::load`, `static_files::render`, `docker::apply`). Everything in `lib.rs`, `templates.rs`, `md_block.rs`, `cli.rs` compiles. Do not commit yet; Task 7 completes the build.

---

### Task 7: The compose scaffold from the container package, rule annotations, Dockerfile gating

**Files:**
- Modify: `aeth-devkit/crates/aeth-devkit-setup/src/docker/compose_rules.rs`
- Modify: `aeth-devkit/crates/aeth-devkit-setup/src/docker/scaffold.rs`
- Modify: `aeth-devkit/crates/aeth-devkit-setup/src/docker/static_files.rs`
- Modify: `aeth-devkit/crates/aeth-devkit-setup/src/docker/mod.rs`
- Create: `aeth-devkit/crates/aeth-devkit-setup/tests/fixtures/docker/compose.template.yaml`
- Modify: `aeth-devkit/crates/aeth-devkit-setup/tests/fixtures/templates/docker/compose.template.yaml`

**Interfaces:**
- Produces:
  - `compose_rules::Kind` is `pub`; `pub struct Rule { pub path: Vec<String>, pub kind: Kind }`; `pub const RULE_MARKER: &str = "# !rule "`; `pub fn parse_rules(lines: &[String]) -> Result<(Vec<String>, Vec<Rule>)>`; `service_edits(lines, svc, sc_lines, sc_svc, name, rules: &[Rule])`.
  - `scaffold::TEMPLATE_FILE = "compose.template.yaml"`, `BLOCK_START = "# !service-block:"`, `BLOCK_END = "# !end service-block"`; `Scaffold { head, block, tail, rules: Vec<Rule> }`; `scaffold::load(ctx, venv, templates_dir, gates) -> Result<Scaffold>`.
  - `static_files::render(ctx, venv, gates) -> Result<Option<String>>`.
  - `docker::apply(ctx, templates_dir, deps, gates, changes)`.

- [x] **Step 1: Write the failing `parse_rules` tests**

In `compose_rules.rs`'s test module add:

```rust
  #[test]
  fn rules_are_read_from_annotations_and_stripped() {
    let block = split_lines(
      "  app:\n    # !rule exact\n    container_name: app\n    build:\n      # !rule exact\n      context: .\n      args:\n        # !rule repo\n        GIT_REPO: x\n    # !rule volume-target\n    volumes:\n      - type: bind\n        target: /app/persisted_data\n    healthcheck:\n      # !rule exact-list\n      test:\n        - CMD\n      # !rule exact\n      interval: 30s\n    plain: 1\n",
    );
    let (clean, rules) = parse_rules(&block).unwrap();
    assert!(clean.iter().all(|l| !l.contains("!rule")), "{clean:?}");
    assert_eq!(clean.len(), block.len() - 6);
    let paths: Vec<(String, Kind)> = rules.iter().map(|r| (r.path.join("."), r.kind)).collect();
    assert_eq!(
      paths,
      vec![
        ("container_name".to_string(), Kind::Exact),
        ("build.context".to_string(), Kind::Exact),
        ("build.args.GIT_REPO".to_string(), Kind::Repo),
        ("volumes".to_string(), Kind::VolumeTarget),
        ("healthcheck.test".to_string(), Kind::ExactList),
        ("healthcheck.interval".to_string(), Kind::Exact),
      ]
    );
    for (bad, needle) in [
      ("  app:\n    # !rule exact\n    - item\n", "followed by a key"),
      ("  app:\n    # !rule exact\n", "annotates nothing"),
      ("  app:\n    # !rule bogus\n    x: 1\n", "unknown rule kind"),
      ("  app:\n    # !rule exact\n    # !rule exact\n    x: 1\n", "another rule line"),
    ] {
      let e = parse_rules(&split_lines(bad)).unwrap_err().to_string();
      assert!(e.contains(needle), "{bad:?}: {e}");
    }
  }
```

Rewrite the test module's `STD` constant to carry the annotations (every key that had a `RULES` entry):

```rust
  const STD: &str = "\
services:
  app:
    # !rule exact
    container_name: app
    build:
      # !rule exact
      context: .
      # !rule exact
      dockerfile: docker/Dockerfile
      args:
        # !rule repo
        GIT_REPO: https://github.com/o/r.git
        # !rule presence
        GIT_TAG: {git_tag}
    # !rule presence
    restart: no
    # !rule volume-target
    volumes:
      - type: bind
        source: /data/app_files
        target: /app/persisted_data
    # !rule env-keys
    environment:
      - ALERTS_EMAIL=info@sweetfiretobacco.com
      - ALERTS_EMAIL_PWD=${ALERTS_EMAIL_PWD:?}
      - ALERTS_RECIPIENTS=[\"jacob.ogden@sweetfiretobacco.com\"]
    # !rule presence
    networks:
      - coolify
    healthcheck:
      # !rule exact-list
      test:
        - CMD-SHELL
        - bash -ec 'heartbeat'
      # !rule exact
      interval: 30s
      # !rule exact
      timeout: 5s
      # !rule exact
      retries: 3
      # !rule exact
      start_period: 15s
";
```

and make `run_full` and `the_repo_rule_skips_itself_without_an_origin` parse it:

```rust
    let (sc, rules) = parse_rules(&split_lines(STD)).unwrap();
    let sc_svc = child(&sc, &top_level(&sc, "services").unwrap(), "app").unwrap();
    // …
    let mut o = service_edits(&lines, &svc, &sc, &sc_svc, "app", &rules);
```

(In the origin test, apply `.replace(…)` to `STD` before `parse_rules`.) Note `parse_rules` on the whole `services:` document: the first key on the stack is `services`, then `app`; the paths must be relative to the service, so the test passes the block starting at `services:` and the implementation below skips two levels when the first line is `services:`; simpler: `parse_rules` takes the lines and a `depth: usize` of leading keys to skip. Use `parse_rules(&lines, 2)` in these tests (`services`, `app`) and `parse_rules(&block, 1)` from the scaffold (`{service}`). Update the signature everywhere to `parse_rules(lines: &[String], skip: usize)`.

- [x] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p aeth-devkit-setup compose_rules`
Expected: compile errors for `parse_rules`, `Rule`, and the `service_edits` arity.

- [x] **Step 3: Implement rules in `compose_rules.rs`**

Make `Kind` `pub` and add, replacing the `RULES` constant:

```rust
impl Kind {
  fn parse(s: &str) -> Option<Kind> {
    Some(match s {
      "exact" => Kind::Exact,
      "presence" => Kind::Presence,
      "repo" => Kind::Repo,
      "volume-target" => Kind::VolumeTarget,
      "env-keys" => Kind::EnvKeys,
      "exact-list" => Kind::ExactList,
      _ => return None,
    })
  }
}

/// One `# !rule <kind>` annotation: the key it sits above, as a path relative to the service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
  pub path: Vec<String>,
  pub kind: Kind,
}

pub const RULE_MARKER: &str = "# !rule ";

/// Strip the rule lines from a scaffold block and return them keyed by the annotated key's
/// path, `skip` leading mapping levels dropped (`services`, the service). A rule line must
/// be followed by a mapping key (spec 2.6).
pub fn parse_rules(lines: &[String], skip: usize) -> Result<(Vec<String>, Vec<Rule>)> {
  let mut out = Vec::with_capacity(lines.len());
  let mut rules = Vec::new();
  let mut pending: Option<(Kind, usize)> = None;
  let mut stack: Vec<(usize, String)> = Vec::new();
  for (i, line) in lines.iter().enumerate() {
    let trimmed = line.trim();
    if let Some(kind) = trimmed.strip_prefix(RULE_MARKER) {
      if let Some((_, at)) = pending {
        bail!("line {}: a rule line follows another rule line (line {})", i + 1, at + 1);
      }
      let kind = Kind::parse(kind.trim()).with_context(|| format!("line {}: unknown rule kind `{}`", i + 1, kind.trim()))?;
      pending = Some((kind, i));
      continue;
    }
    out.push(line.clone());
    let key = (!trimmed.is_empty() && !trimmed.starts_with('#') && !trimmed.starts_with("- "))
      .then(|| trimmed.split_once(':').map(|(k, _)| k.trim().to_string()))
      .flatten();
    let Some(key) = key else {
      if let Some((_, at)) = pending {
        bail!("line {}: `# !rule` must be followed by a key, not `{trimmed}`", at + 1);
      }
      continue;
    };
    let indent = line.len() - line.trim_start().len();
    while stack.last().is_some_and(|(d, _)| *d >= indent) {
      stack.pop();
    }
    stack.push((indent, key));
    if let Some((kind, _)) = pending.take() {
      rules.push(Rule {
        path: stack.iter().skip(skip).map(|(_, k)| k.clone()).collect(),
        kind,
      });
    }
  }
  if let Some((_, at)) = pending {
    bail!("line {}: `# !rule` at the end of the block annotates nothing", at + 1);
  }
  Ok((out, rules))
}
```

Add `use anyhow::{Context as _, Result, bail};` at the top. Change `service_edits`'s signature to `…, name: &str, rules: &[Rule]) -> Outcome` and its loop head to:

```rust
  for Rule { path, kind } in rules {
    let path: Vec<&str> = path.iter().map(String::as_str).collect();
    let path = path.as_slice();
    let kind = *kind;
```

(the body below already works on `path: &[&str]` and `kind: Kind`; remove the old `for (path, kind) in RULES` line and the now-dead `RULES` doc comment.) Update the module doc: the rule kinds come from the scaffold's annotations.

- [x] **Step 4: `scaffold.rs`: the new markers, rules, and the container package source**

Replace the constants and `Scaffold`/`parse`/`load`:

```rust
use crate::docker::compose_rules::{self, Rule};
use crate::gate::{Format, Gates};
use crate::packages::{self, Venv};

/// The template's file name inside the installed `devkit_container` package (spec section 3).
pub const TEMPLATE_FILE: &str = "compose.template.yaml";
pub const BLOCK_START: &str = "# !service-block:";
pub const BLOCK_END: &str = "# !end service-block";

/// `head` + one `block` per service + `tail` is a complete compose file. The block still
/// carries `{service}` and `{git_tag}`; every other placeholder was substituted on load, and
/// its `# !rule` lines were lifted into `rules`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scaffold {
  pub head: String,
  pub block: String,
  pub tail: String,
  pub rules: Vec<Rule>,
}

/// Split an already substituted and gated template at its block markers and lift the rules.
pub fn parse(template: &str) -> Result<Scaffold> {
  // … the existing state machine, unchanged, filling head/block/tail …
  if !matches!(part, Part::Tail) {
    bail!("compose template is missing the `{BLOCK_START}` / `{BLOCK_END}` markers");
  }
  let (block_lines, rules) = compose_rules::parse_rules(&aeth_devkit_core::compose::tree::split_lines(&block), 1)?;
  let mut block = block_lines.join("\n");
  block.push('\n');
  Ok(Scaffold { head, block, tail, rules })
}

/// The compose template from the installed container package (its `compose.template.yaml`),
/// gated and substituted; until a release of `devkit-container` carries it, the templates
/// package's copy (spec section 3, the fallback that step 3 of the release order removes).
pub fn load(ctx: &ProjectContext, venv: &dyn Venv, templates_dir: &Path, gates: &Gates) -> Result<Scaffold> {
  let from_container = venv
    .installed(&ctx.root, &packages::CONTAINER)
    .map(|i| i.dir.join(TEMPLATE_FILE))
    .filter(|p| p.is_file());
  let raw = match from_container {
    Some(path) => {
      let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
      templates::substitute(&gates.apply(&text, Format::Yaml, TEMPLATE_FILE)?, ctx, templates::Escape::None)
    }
    None => templates::load(templates_dir, "docker/compose.yaml", ctx, templates::Escape::None, gates)?,
  };
  parse(&raw)
}
```

Update `parse`'s imports (`Context as _`) and the unit test `TPL` constant to the new markers with one rule line, asserting `sc.rules` holds `container_name` as `Exact` and that `sc.block` has no `!rule` line.

- [x] **Step 5: `static_files.rs`: gate the Dockerfile**

```rust
pub fn render(ctx: &ProjectContext, venv: &dyn Venv, gates: &Gates) -> Result<Option<String>> {
  let Some(installed) = venv.installed(&ctx.root, &crate::packages::CONTAINER) else {
    return Ok(None);
  };
  let path = installed.dir.join(TEMPLATE_FILE);
  let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
  let gated = gates.apply(&text, Format::Dockerfile, TEMPLATE_FILE)?;
  Ok(Some(templates::substitute(&gated, ctx, templates::Escape::None)))
}
```

`apply(ctx, venv, consent, changes)` gains `gates: &Gates` and passes it to `render`. In the module's test `render_substitutes_python_dir_from_the_installed_package`, build gates with `Gates::default()` (no gates in that fixture line) and add a second assertion: a template `"FROM x\n# !if rust:\nRUN cargo\n# !end\n"` renders the `RUN` line only when the gates hold `rust` true (construct via `Gates::build(&[("template.Dockerfile".into(), text.into())], &"[project]\nname = \"p\"\n".parse().unwrap(), None, &Facts { rust: true, docker_files: false })`).

- [x] **Step 6: `docker/mod.rs`: thread the gates and the rules**

`pub fn apply(ctx, templates_dir, deps, gates: &Gates, changes)` calls `static_files::apply(ctx, deps.venv, &consent, gates, changes)?` and `compose(ctx, templates_dir, deps.venv, docker.runner, &consent, gates, changes)`. In `compose`, `let sc = scaffold::load(ctx, venv, templates_dir, gates)?;` and `compose_rules::service_edits(&lines, &svc, &sc_doc, &sc_svc, name, &sc.rules)`.

- [x] **Step 7: The two fixture compose templates**

Overwrite `tests/fixtures/templates/docker/compose.template.yaml` with today's content in the new syntax (the aeth-ext block gated, every enforced key annotated; this is also exactly what Task 10 writes into `devkit-templates`):

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
    # !if dep("aeth-ext"):
    # !rule env-keys
    environment:
      - ALERTS_EMAIL=info@sweetfiretobacco.com
      - ALERTS_EMAIL_PWD=${ALERTS_EMAIL_PWD:?}
      - ALERTS_RECIPIENTS=["jacob.ogden@sweetfiretobacco.com"]
    # !end
    # !rule presence
    networks:
      - coolify
    healthcheck:
      # !rule exact-list
      test:
        - CMD-SHELL
        - bash -ec '[ -f /app/persisted_data/logs/heartbeat.txt ] && ts=$$(cat /app/persisted_data/logs/heartbeat.txt 2>/dev/null) && [ -n "$$ts" ] && hb=$$(date -d "$$ts" +%s 2>/dev/null) && now=$$(date +%s) && [ $$((now - hb)) -lt 180 ]'
      # !rule exact
      interval: 30s
      # !rule exact
      timeout: 5s
      # !rule exact
      retries: 3
      # !rule exact
      start_period: 15s
# !end service-block

networks:
  coolify:
    external: true
```

Copy the same file to `tests/fixtures/docker/compose.template.yaml` (the fixture standing in for the container package; `docker.rs`'s `package_dirs` points every package at `fixtures/docker`, so from now on the compose scaffold in those tests comes from "the container package" and exercises the container path of `scaffold::load`). Keep the templates copy too: `apply.rs`'s `run_via` also maps `devkit_container` to `fixtures/docker`, so both paths are covered by the two suites only if one fixture lacks the file; to cover the fallback, `apply.rs`'s stub venv for `devkit_container` stays at `fixtures/docker` (container path) and add one test in `docker.rs`, `the_templates_copy_is_the_fallback_without_a_container_compose_template`, that builds a `StubVenv` whose `devkit_container` dir is a temp copy of `fixtures/docker` without `compose.template.yaml` and asserts the run still creates `docker/compose.yaml`.

- [x] **Step 8: Build and run the docker tests**

Run: `cargo build -p aeth-devkit-setup -p aeth-devkit-pin && cargo test -p aeth-devkit-setup compose_rules scaffold static_files && cargo test -p aeth-devkit-setup --test docker`
Expected: the workspace builds; `compose_rules`, `scaffold`, `static_files` unit tests pass; `tests/docker.rs` fails only in tests whose fixtures still carry old markers (fixed in Task 8) and otherwise passes. If `without_aeth_ext_the_alerts_block_is_absent` or `fresh_project_gets_dockerfile_and_compose_then_is_idempotent` fail for another reason, fix the implementation now.

- [x] **Step 9: Commit Tasks 6 and 7 together (one compiling state)**

```bash
git add -A crates/aeth-devkit-setup crates/aeth-devkit-pin
git commit -m "feat(setup): render every template through the gate language; compose scaffold and rule kinds from the container package"
```

---

### Task 8: `toml_merge` without markers, and the fixtures in the new syntax

**Files:**
- Modify: `aeth-devkit/crates/aeth-devkit-setup/src/toml_merge.rs`
- Modify: `aeth-devkit/crates/aeth-devkit-setup/tests/fixtures/templates/pyproject.template.toml`
- Modify: `aeth-devkit/crates/aeth-devkit-setup/tests/fixtures/templates/AGENTS.template.md`
- Modify: `aeth-devkit/crates/aeth-devkit-setup/tests/fixtures/templates/github/workflows/release.template.yml`
- Modify: `aeth-devkit/crates/aeth-devkit-setup/tests/fixtures/templates/github/workflows/release.rust.template.yml`
- Modify: `aeth-devkit/crates/aeth-devkit-setup/tests/apply.rs`

**Interfaces:**
- Consumes: gated text from `templates::load` (no markers reach `merge_pyproject`).
- Produces: `merge_pyproject(original, template, ctx, log)` unchanged in signature, marker-free in behaviour.

- [x] **Step 1: Rewrite the fixture markers**

In `pyproject.template.toml`:
- line 9 `# setup-project: if-docker-services` (above `[project]`) → `# S!if keys("tool.docker.services"):`
- line 54 `# setup-project: if-docker` (above `[tool.docker]`) → `# S!if keys("tool.docker.services") or docker_files:`
- line 65 `# setup-project: if-dep mypy` (above `[tool.mypy]`) → `# S!if dep("mypy"):`
- line 249: delete the `  # setup-project: if-docker-services` line and make the next line a one-liner: `  devkit-container    = [{ index = "{devkit_index}" }]  # !if keys("tool.docker.services")`

In `AGENTS.template.md`: `<!-- setup-project: if-dep aeth-ext -->` → `<!-- S!if dep("aeth-ext"): -->`.

In both release workflow templates: every `# setup-project: if-publish-index` → `# !if publish_index:`, every `# setup-project: if-no-publish-index` → `# !if not publish_index:`, every `# setup-project: end` → `# !end` (keep each line's indentation).

Check nothing is left: `grep -rn "setup-project: " crates/aeth-devkit-setup/tests/fixtures` prints nothing.

- [x] **Step 2: Delete the marker machinery in `toml_merge.rs`**

Delete: the constants `MARKER`, `IF_DEP_MARKER`, `IF_DOCKER_MARKER`, `IF_DOCKER_SERVICES_MARKER`; `check_markers` and its call in `merge_pyproject`; `Merger::gated_off` and its two call sites (the `if self.gated_off(template, key) { continue; }` in `merge_table` and the `fresh.retain(|k, _| !self.gated_off(ttable, k))` line); `strip_marker_lines`, `is_marker_line`, `marker_lines`, `conditional_dep`, `conditional_docker` and their call sites. `Merger` keeps `ctx` only if something else reads it; if `drop_own_package` is the only user of `ctx.name`, drop the field and pass `ctx` where needed. Delete the tests `a_conditional_table_follows_the_dependency`, `a_marker_above_a_value_gates_that_key_only`, `if_docker_table_is_skipped_without_a_docker_setup`, `if_docker_table_is_merged_with_a_docker_setup`, `if_docker_services_needs_the_switch_not_just_docker_files`, `an_unknown_marker_is_an_error`, `the_marker_comment_never_reaches_the_project`, `stripping_the_marker_keeps_the_other_comments_and_spacing`, and the `ctx(has_docker)` helper if unused. Update the module doc.

Run: `cargo test -p aeth-devkit-setup toml_merge`
Expected: the remaining `toml_merge` tests pass.

- [x] **Step 3: Add the gate-flip refusal test in `apply.rs`**

After `an_uncommitted_services_change_cancels_a_committing_run` add:

```rust
#[test]
fn a_pyproject_edit_that_flips_a_gate_cancels_a_committing_run() {
  // `[tool.mypy]` in the template is gated on `dep("mypy")`; adding mypy to the working copy
  // without committing it flips that gate, which the run refuses before staging anything.
  let dir = make_project();
  let root = dir.path();
  let committed = read(root, "pyproject.toml");
  git_init(root);
  git(root, &["add", "-A"]);
  git(root, &["commit", "-q", "-m", "init"]);
  let edited = committed.replacen("[dependency-groups]\n  dev = [", "[dependency-groups]\n  dev = [\n    \"mypy>=1\",", 1);
  assert_ne!(committed, edited, "the fixture's dev group must be where this expects it");
  write(root, "pyproject.toml", &edited);
  let err = aeth_devkit_setup::cli::run(&aeth_devkit_setup::cli::Args {
    root: root.to_path_buf(),
    templates_dir: Some(templates()),
    dry_run: false,
    no_commit: false,
    yes: false,
    vscode: false,
    no_vscode: true,
  })
  .unwrap_err()
  .to_string();
  assert!(err.contains("not committed") && err.contains("dep(\"mypy\")"), "{err}");
  assert_eq!(read(root, "pyproject.toml"), edited, "nothing touched");
  assert_eq!(git(root, &["rev-list", "--count", "HEAD"]), "1");
}
```

(If the fixture's dev-group line is spelled differently, adjust the `replacen` needle to the fixture, keeping the assertion that the edit adds `mypy` to a dependency group.)

- [x] **Step 4: Run the integration suites**

Run: `cargo test -p aeth-devkit-setup --test apply && cargo test -p aeth-devkit-setup --test docker && cargo test -p aeth-devkit-setup --test packages`
Expected: all pass. `uv_init_gitignore_is_replaced_and_mypy_is_conditional` proves the structural TOML gate; `agents_md_gets_a_managed_block_and_keeps_project_text` the markdown one; `release_workflow_*` the explicit YAML ones; `an_uncommitted_services_change_cancels_a_committing_run` still passes because the `services` check precedes the gate sweep. Fix implementation bugs these surface before moving on.

- [x] **Step 5: Commit**

```bash
git add -A crates/aeth-devkit-setup
git commit -m "refactor(setup): drop the setup-project: marker machinery; fixtures in the # ! syntax; refuse a flipped gate"
```

---

### Task 9: The language reference, the CI templates job, fmt and clippy

**Files:**
- Modify: `aeth-devkit/README.md` (new `### Template language` section after `### devkit setup-project`; edit the `if-dep` mentions in the **Project discovery** and **pyproject merge** bullets)
- Modify: `aeth-devkit/.github/workflows/ci.yml` (the `Templates:` job)

- [x] **Step 1: Write the language reference**

Insert after the `### devkit setup-project` section's last bullet:

````markdown
### Template language

Templates are gated with markers in the file's own comment syntax: `# !…` in YAML, TOML,
Dockerfiles, `.env` and the ignore files; `<!-- !… -->` in markdown; `// !…` in JSONC. The
space before `!` is mandatory (`#!` is a shebang). Markers match at any indentation and never
reach the rendered file. A marker whose word is not `if`, `end`, `service-block` or `rule` is a
render error.

```yaml
# !if <expr>:                  opens a block; closed by an end
# !if <expr> as <label>:       the same, named for a targeted end
# S!if <expr>:                 a structural block: one unit of the file, no end line
<content line>  # !if <expr>   a one-line block: gates that line only
# !end                         closes the innermost open block
# !end <name>                  closes the named block (its label, else its expression text) and everything inside it
<content line>  # !end [name]  the same, trailing the block's last line
```

A structural block's unit is: in TOML, to the next table header (minus the comment block
directly above it); in markdown, the heading and its section, fenced code excluded; in YAML,
the next node (the next line and every line indented deeper); in a Dockerfile, the next
instruction with its `\` continuations. Explicit blocks nest without limit; an explicit block
inside a structural one must close before the unit ends.

`<expr>` is a Python expression, evaluated for truthiness (with [Monty](https://github.com/pydantic/monty)).
It sees `keys("tool.docker.wireguard")` (the value at that dotted path in the project's
`pyproject.toml`; tables as dicts, arrays as lists, a missing path as `None`), `dep("aeth-ext")`
(the project depends on the package, in any group, or is it), and the flags `rust` (a
`Cargo.toml`), `publish_index` (an index with a publish URL) and `docker_files` (a Dockerfile
or compose file on disk). Everything else is Python: `not`, `and`, `or`, comparisons, `in`,
string methods, `any`, `all`. An unknown name or a failing expression is a render error,
never false. Every distinct expression is evaluated once per run; a committing run also
evaluates each against HEAD's `pyproject.toml` and refuses when any verdict differs.

The compose template additionally carries `# !service-block:` … `# !end service-block` around
the per-service block and `# !rule <kind>` above each key the compose step enforces in an
existing file (`exact`, `presence`, `repo`, `volume-target`, `env-keys`, `exact-list`).
````

Then edit the two older mentions: in **Project discovery**, `(drives \`if-dep\` gating)` → `(drives \`dep("…")\` gates)`; in **pyproject merge**, replace the clause about `if-dep` / `if-docker` / `if-docker-services` markers with `gates above a table header (structural) or on a key line (see **Template language**) keep a table or key out of projects the condition excludes`.

- [x] **Step 2: Point the CI templates job at the templates tree, not the last release**

In `.github/workflows/ci.yml`, in the `Templates:` job's install line, replace

```
uv pip install --python "$RUNNER_TEMP/tpl/bin/python" --no-deps --index https://pypi.sweetfiretobacco.com/jacob.ogden/internal/+simple devkit-templates
```

with

```
uv pip install --python "$RUNNER_TEMP/tpl/bin/python" --no-deps "devkit-templates @ git+https://github.com/AetherBreaker/devkit-templates@main"
```

and add a comment above it: `# main of the templates repo, not the last release: this job checks the two trees against each other, and a language change lands in both before either is released.` Rename the job's `name:` to `"Templates: dry-run the templates repo's main through this tree's devkit"`.

- [x] **Step 3: fmt, clippy, the whole setup crate**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test -p aeth-devkit-setup -p aeth-devkit-pin -p aeth-devkit-core`
Expected: clean, all green.

- [x] **Step 4: Commit**

```bash
git add README.md .github/workflows/ci.yml
git add -A crates
git commit -m "docs(setup): document the template language; CI renders the templates repo's main"
```

---

### Task 10: `devkit-templates` in the new syntax

**Files (in `../devkit-templates`):**
- Modify: `python/devkit_templates/templates/pyproject.template.toml`
- Modify: `python/devkit_templates/templates/AGENTS.template.md`
- Modify: `python/devkit_templates/templates/docker/compose.template.yaml`
- Modify: `python/devkit_templates/templates/github/workflows/release.template.yml`
- Modify: `python/devkit_templates/templates/github/workflows/release.rust.template.yml`
- Modify: `pyproject.toml` (the floor)
- Modify: `README.md` (the language paragraph)

- [x] **Step 1: Rewrite the markers exactly as in Task 8, Step 1**

Apply the same four edits to `pyproject.template.toml`, the one to `AGENTS.template.md`, and the `publish_index` replacements in both workflow templates. Overwrite `docker/compose.template.yaml` with the file from Task 7, Step 7. Then:

```bash
grep -rn "setup-project: " python/devkit_templates/templates && echo "OLD MARKERS LEFT" || echo "clean"
```

Expected: `clean`.

- [x] **Step 2: Raise the floor and fix the README**

In `pyproject.toml`: `dependencies = ["aeth-devkit>=15.0.0"]` (the release Task 11 makes; the version is the workspace's `14.1.0` plus a major). In `README.md`, in **The floor**, replace `the \`# setup-project:\` line gates, the table and value markers` with `the \`# !\` gate language (see aeth-devkit's README, **Template language**)`.

- [x] **Step 3: Render the tree through the local devkit build**

The released devkit cannot read this syntax yet, so render with the branch build:

```bash
cd "/d/SFT Software Projects/aeth-devkit" && cargo build -p aeth-devkit
cd "/d/SFT Software Projects/devkit-templates"
rm -rf /tmp/render-docker && mkdir -p /tmp/render-docker/src/scratch_app && : > /tmp/render-docker/src/scratch_app/__init__.py
printf '[project]\nname = "scratch-app"\nversion = "0.1.0"\nrequires-python = ">=3.14"\ndependencies = []\n\n[dependency-groups]\ndev = ["aeth-devkit"]\n\n[tool.docker]\nservices = ["scratch-app"]\n\n[tool.uv.sources]\naeth-devkit = [{ index = "SFTPyPI" }]\n\n[[tool.uv.index]]\nname = "SFTPyPI"\nurl = "https://pypi.sweetfiretobacco.com/jacob.ogden/internal/+simple"\npublish-url = "https://pypi.sweetfiretobacco.com/jacob.ogden/internal/"\nexplicit = true\n' > /tmp/render-docker/pyproject.toml
../aeth-devkit/target/debug/devkit setup-project --root /tmp/render-docker --templates-dir python/devkit_templates/templates --dry-run --no-vscode
```

Expected: a `Would change:` report listing `pyproject.toml`, `docker/Dockerfile` (or a note that the container package is absent, which is fine here), `docker/compose.yaml`, `AGENTS.md`, `.github/workflows/release.yml` and the rest; no `error:` line; no `!if`/`!end`/`setup-project` text in the report's previews. Repeat with `services = []` removed from the scratch pyproject (a plain project) and confirm the report has no `docker/` entries and no `[tool.docker]` in the pyproject preview.

- [x] **Step 4: Commit and push the branch**

```bash
git add -A && git commit -m "feat(templates): the # ! gate language; floor aeth-devkit>=15.0.0"
git push -u origin template-language
```

Its CI will be red until aeth-devkit 15.0.0 exists (the `floor` matrix leg installs `aeth-devkit==15.0.0`); that is expected and is re-run in Task 11.

---

### Task 11: Merge and release, in order

**Files:** none new.

- [x] **Step 1: The full aeth-devkit suite, once**

```bash
cd "/d/SFT Software Projects/aeth-devkit" && cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test
```

Expected: green.

- [x] **Step 2: Open and merge the aeth-devkit PR**

```bash
git push -u origin template-language
gh pr create --title "feat(setup): the # ! template language" --body "$(cat <<'EOF'
Implements section 2 of devkit-container/docs/superpowers/specs/2026-09-08-container-wireguard-mode-design.md: Python gate expressions evaluated by Monty behind a one-function seam, explicit and structural blocks, rule annotations read from the compose template, the compose scaffold from the devkit_container package (templates fallback until it ships one), and a HEAD-vs-working-copy refusal on any gate whose verdict differs.

Breaking: the `# setup-project:` markers are no longer read. Pairs with the devkit-templates branch `template-language`, which raises its floor to this release.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

Wait for CI (the `Templates:` job now renders the templates repo's `main`, which still has the old syntax until Step 4 merges there; if that job fails on that alone, merge the templates branch first, then re-run). Merge with `gh pr merge --squash --delete-branch` once green, or hand the PR to the owner if the repository requires it.

- [x] **Step 3: Release aeth-devkit 15.0.0**

```bash
git switch main && git pull && uv sync
uv run devkit release major "the # ! template language: Python gate expressions, structural blocks, rule annotations; setup-project: markers are no longer read"
```

Needs the SFTPyPI credentials in `.env`; if they are absent on this machine, stop here and report that the owner runs this step. Expected: the release workflow builds and publishes; `uv run devkit --version` after `uv sync` in any project shows 15.0.0 on the index.

- [x] **Step 4: Merge and release devkit-templates**

```bash
cd "/d/SFT Software Projects/devkit-templates"
gh pr create --title "feat(templates): the # ! gate language" --body "$(cat <<'EOF'
Every marker rewritten in aeth-devkit 15's template language; floor raised to aeth-devkit>=15.0.0. Pairs with aeth-devkit's `template-language` PR.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

Re-run its CI (both matrix legs now find 15.0.0), merge when green, then:

```bash
git switch main && git pull && uv sync
uv run devkit release minor "the # ! gate language; needs aeth-devkit 15"
```

Same credentials note as Step 3.

- [x] **Step 5: Verify the pairing in a real project**

```bash
cd "/d/SFT Software Projects/devkit-container" && uv run poe lock && uv run poe setup-project --dry-run --no-vscode
```

Expected: after `lock` moves `aeth-devkit` to 15.0.0 the run bootstraps templates 1.2.0 and reports either nothing to do or only content changes; no `error:` line, no marker text in any preview. (This repo's own Dockerfile template has no gates yet; the container plan adds them.)

---

## Self-review

**Spec coverage.** 2.1 markers and formats: Task 3. 2.2 blocks, one-liners, ends, nesting: Task 5. 2.3 structural units per format: Task 5. 2.4 expressions, `keys`/`dep`/flags, unknown names as errors: Tasks 2 and 4. 2.5 sweep, single evaluation, Monty seam, refusal by double evaluation: Tasks 2, 4, 6 (`gates_for`, `cli`). 2.6 `service-block` and `rule`: Task 7. 2.7 migration and lockstep release, README language reference: Tasks 8, 9, 10, 11. Section 3 (compose from the container package with the templates fallback): Task 7. Section 10 step 1: Task 11. Section 12's `setup` paragraph: the test lists in Tasks 3–5, 7, 8. Section 13's first bullet: Task 10 Step 1's grep and Task 9's README.

**Placeholders.** None: every step has its command or code. The one deliberate openness is Task 2 Step 4's note that Monty's 0.0.x API may differ in detail, with the files to read if it does.

**Type consistency.** `Gates::build(&[(String, String)], &DocumentMut, Option<&DocumentMut>, &Facts)`, `Gates::apply(&self, &str, Format, &str)`, `Gates::verdict(&self, &str)` are used with those shapes in Tasks 5, 6, 7. `templates::load(dir, name, ctx, escape, &gates)` in Tasks 6 and 7. `scaffold::load(ctx, venv, templates_dir, gates)` and `static_files::render(ctx, venv, gates)` in Tasks 6 (pin), 7. `compose_rules::parse_rules(&[String], usize)` and `service_edits(…, &[Rule])` in Task 7 only. `context::dependencies_of(&DocumentMut)` defined in Task 4 and used there.
