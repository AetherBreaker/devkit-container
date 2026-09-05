//! The questions the image asks of `/app/pyproject.toml`, read with `toml_edit` (a parse
//! only; nothing is written back).

use std::path::Path;

use anyhow::{Context as _, Result, bail};
use toml_edit::{DocumentMut, Item};

pub fn load(path: &Path) -> Result<DocumentMut> {
  std::fs::read_to_string(path)
    .with_context(|| format!("reading {}", path.display()))?
    .parse()
    .with_context(|| format!("parsing {}", path.display()))
}

/// `[project.optional-dependencies].app` exists → the image syncs `--extra app`.
pub fn app_extra(doc: &DocumentMut) -> bool {
  doc
    .get("project")
    .and_then(|p| p.get("optional-dependencies"))
    .and_then(|o| o.get("app"))
    .is_some()
}

/// `project.readme` as a path: either the string form or the `{ file = "…" }` table form.
pub fn readme(doc: &DocumentMut) -> Option<String> {
  let item = doc.get("project")?.get("readme")?;
  match item {
    Item::Value(v) if v.is_str() => v.as_str().map(str::to_string),
    // `as_table_like` covers both `[project.readme]` and the inline `{ file = … }`.
    _ => item.as_table_like()?.get("file")?.as_str().map(str::to_string),
  }
}

/// The single `[project.scripts]` key with the `run-app-` prefix; zero or several is an
/// error that names what was found (the old `get_launch_script.py` rule).
#[cfg_attr(not(unix), allow(dead_code))] // only `run` (Unix) calls this
pub fn launch_script(doc: &DocumentMut) -> Result<String> {
  let scripts: Vec<String> = doc
    .get("project")
    .and_then(|p| p.get("scripts"))
    .and_then(|s| s.as_table_like())
    .map(|t| t.iter().map(|(k, _)| k.to_string()).collect())
    .unwrap_or_default();
  let matches: Vec<&String> = scripts.iter().filter(|s| s.starts_with("run-app-")).collect();
  // Slice patterns: exactly one, none, or "anything else" (two or more).
  match matches.as_slice() {
    [one] => Ok((*one).clone()),
    [] => {
      let available = if scripts.is_empty() {
        "(none)".to_string()
      } else {
        scripts.join(", ")
      };
      bail!("no [project.scripts] entry with a 'run-app-' prefix found (available scripts: {available})")
    }
    many => bail!(
      "multiple 'run-app-' scripts found ({}); define exactly one",
      many.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
    ),
  }
}

/// `[tool.docker].required_persisted_dirs`, each entry validated so nothing can resolve to
/// `/app` itself or outside it. A table still carrying a legacy key is refused with the
/// migration hint — even next to the new key, which is exactly what `setup-project` leaves
/// behind: honouring only the new list would silently drop whatever `chown_paths` named.
#[cfg_attr(not(unix), allow(dead_code))] // only `run` (Unix) calls this
pub fn required_persisted_dirs(doc: &DocumentMut) -> Result<Vec<String>> {
  let docker = doc.get("tool").and_then(|t| t.get("docker"));
  if docker.is_some_and(|d| d.get("chown_paths").is_some() || d.get("mkdirs").is_some()) {
    bail!(
      "[tool.docker] still has chown_paths/mkdirs; fold chown_paths into required_persisted_dirs, move mkdirs scratch directories to temp dirs, and delete both keys"
    );
  }
  let Some(arr) = docker.and_then(|d| d.get("required_persisted_dirs")).and_then(|v| v.as_array()) else {
    return Ok(Vec::new());
  };
  let mut out = Vec::new();
  for v in arr.iter() {
    let Some(s) = v.as_str() else {
      bail!("required_persisted_dirs entries must be strings, got {v}")
    };
    let entry = s.trim().trim_end_matches('/');
    // Empty, absolute, or any `.`/`..`/empty component (`a//b`) — each could escape /app
    // or name /app itself, and `chown -R /app` is exactly the accident to prevent.
    let bad = entry.is_empty() || entry.starts_with('/') || entry.split('/').any(|c| c.is_empty() || c == "." || c == "..");
    if bad {
      bail!("required_persisted_dirs entry {s:?} is not a relative path inside /app");
    }
    out.push(entry.to_string());
  }
  Ok(out)
}

#[cfg(test)]
mod tests {
  use super::*;

  fn doc(s: &str) -> DocumentMut {
    s.parse().unwrap()
  }

  #[test]
  fn app_extra_and_readme() {
    let d = doc("[project]\nreadme = \"README.md\"\n[project.optional-dependencies]\napp = [\"x\"]\n");
    assert!(app_extra(&d));
    assert_eq!(readme(&d).as_deref(), Some("README.md"));
    let d = doc("[project]\nreadme = { file = \"docs/R.md\" }\n");
    assert!(!app_extra(&d));
    assert_eq!(readme(&d).as_deref(), Some("docs/R.md"));
    assert_eq!(readme(&doc("[project]\n")), None);
  }

  #[test]
  fn exactly_one_run_app_script() {
    let d = doc("[project.scripts]\nrun-app-x = \"m:main\"\nother = \"m:o\"\n");
    assert_eq!(launch_script(&d).unwrap(), "run-app-x");
    let none = launch_script(&doc("[project.scripts]\nother = \"m:o\"\n")).unwrap_err().to_string();
    assert!(none.contains("run-app-") && none.contains("other"), "{none}");
    let many = launch_script(&doc("[project.scripts]\nrun-app-a = \"m\"\nrun-app-b = \"m\"\n"))
      .unwrap_err()
      .to_string();
    assert!(many.contains("run-app-a, run-app-b"), "{many}");
    assert!(launch_script(&doc("[project]\n")).unwrap_err().to_string().contains("(none)"));
  }

  #[test]
  fn persisted_dirs_are_validated() {
    let d = doc("[tool.docker]\nrequired_persisted_dirs = [\"persisted_data\", \"data/sub/\"]\n");
    assert_eq!(required_persisted_dirs(&d).unwrap(), ["persisted_data", "data/sub"]);
    assert_eq!(required_persisted_dirs(&doc("[project]\n")).unwrap(), Vec::<String>::new());
    for bad in ["\"\"", "\".\"", "\"..\"", "\"/abs\"", "\"a/../b\"", "\"./x\""] {
      let d = doc(&format!("[tool.docker]\nrequired_persisted_dirs = [{bad}]\n"));
      assert!(required_persisted_dirs(&d).is_err(), "{bad} must be rejected");
    }
    let legacy = required_persisted_dirs(&doc("[tool.docker]\nchown_paths = [\"x\"]\n"))
      .unwrap_err()
      .to_string();
    assert!(legacy.contains("chown_paths"), "{legacy}");
    let both = doc("[tool.docker]\nrequired_persisted_dirs = [\"persisted_data\"]\nmkdirs = [\"x\"]\n");
    assert!(required_persisted_dirs(&both).unwrap_err().to_string().contains("mkdirs"));
  }
}
