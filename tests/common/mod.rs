//! Shared by the smoke tests: the wheel built in the maturin container, the scratch project
//! as a bare repo, the shipped Dockerfile with its one gate applied locally, and cleanup.
#![allow(dead_code)] // each test binary uses a different subset

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub fn root() -> PathBuf {
  Path::new(env!("CARGO_MANIFEST_DIR")).canonicalize().unwrap()
}

/// Run to completion; panic with both streams on a non-zero exit.
pub fn ok(cmd: &mut Command) -> Output {
  let out = cmd.output().unwrap_or_else(|e| panic!("spawning {cmd:?}: {e}"));
  assert!(
    out.status.success(),
    "{cmd:?} failed ({}):\n--- stdout\n{}\n--- stderr\n{}",
    out.status,
    String::from_utf8_lossy(&out.stdout),
    String::from_utf8_lossy(&out.stderr)
  );
  out
}

pub fn text(out: &Output) -> String {
  String::from_utf8_lossy(&out.stdout).into_owned()
}

pub fn write(root: &Path, rel: &str, content: &str) {
  let p = root.join(rel);
  std::fs::create_dir_all(p.parent().unwrap()).unwrap();
  std::fs::write(p, content).unwrap();
}

/// Best-effort removal of everything a test created, whichever way it ends: the containers
/// first (they hold the network and the volume), then the network, the image, the volume.
pub struct Cleanup {
  pub image: String,
  pub volume: String,
  pub containers: Vec<String>,
  pub network: Option<String>,
}

impl Drop for Cleanup {
  fn drop(&mut self) {
    for c in &self.containers {
      let _ = Command::new("docker").args(["rm", "-f", c]).output();
    }
    if let Some(n) = &self.network {
      let _ = Command::new("docker").args(["network", "rm", n]).output();
    }
    let _ = Command::new("docker").args(["rmi", "-f", &self.image]).output();
    let _ = Command::new("docker").args(["volume", "rm", "-f", &self.volume]).output();
  }
}

/// The app the exec-mode image is built around. `main` checks the environment the entrypoint
/// promises and prints one JSON object; the test asserts its fields from outside.
pub const REPORT_APP: &str = r#"import datetime
import json
import os
import sys


def main() -> None:
    r: dict[str, object] = {}
    fails: list[str] = []

    def check(name: str, cond: bool, detail: object = None) -> None:
        r[name] = detail if detail is not None else cond
        if not cond:
            fails.append(name)

    # The entrypoint execs the app: it is PID 1, with no wrapper between it and the signals.
    check("pid_1", os.getpid() == 1, os.getpid())
    check("uid_999", os.getuid() == 999, os.getuid())
    check("gid_999", os.getgid() == 999, os.getgid())
    check("no_extra_groups", all(g == 999 for g in os.getgroups()), os.getgroups())
    check("cwd_app", os.getcwd() == "/app", os.getcwd())
    check("venv_python", sys.executable.startswith("/app/.venv/"), sys.executable)
    path = os.environ.get("PATH", "")
    check("venv_first_on_path", path.split(":")[0] == "/app/.venv/bin", path)
    check("optimize", sys.flags.optimize == 1 and not __debug__, sys.flags.optimize)
    check("no_bytecode", sys.dont_write_bytecode, sys.dont_write_bytecode)
    check("unbuffered", os.environ.get("PYTHONUNBUFFERED") == "1")

    persisted = "/app/persisted_data"
    st = os.stat(persisted)
    check("persisted_owned", (st.st_uid, st.st_gid) == (999, 999), [st.st_uid, st.st_gid])
    logs = os.path.join(persisted, "logs")
    check("logs_dir_created", os.path.isdir(logs))
    if os.path.isdir(logs):
        st = os.stat(logs)
        check("logs_owned", (st.st_uid, st.st_gid) == (999, 999), [st.st_uid, st.st_gid])
    try:
        # What the compose healthcheck reads.
        with open(os.path.join(logs, "heartbeat.txt"), "w") as f:
            f.write(datetime.datetime.now(datetime.UTC).isoformat())
        check("persisted_writable", True)
    except OSError as e:
        check("persisted_writable", False, str(e))
    try:
        open("/app/should-fail", "w").close()
        check("app_read_only", False, "wrote /app/should-fail")
    except PermissionError:
        check("app_read_only", True)

    try:
        import colorama

        check("app_extra_installed", True, colorama.__version__)
    except ImportError as e:
        check("app_extra_installed", False, str(e))
    import smoke_app

    check(
        "installed_as_wheel",
        "/site-packages/" in smoke_app.__file__ and not os.path.exists("/app/src"),
        smoke_app.__file__,
    )

    r["ok"] = not fails
    r["failures"] = fails
    print(json.dumps(r))
    sys.exit(0 if not fails else 1)
"#;

/// Where the scratch app's path source finds the wheel, relative to its `pyproject.toml`:
/// under the scratch tree on the host for `uv lock`, and under `/app` in the image for the
/// frozen syncs.
pub const WHEEL_DIR: &str = "wheels";

/// `{tail}` is the test's own `[tool.docker]` additions (`wireguard = true`, or nothing).
pub const PYPROJECT: &str = r#"[project]
name = "smoke-app"
version = "0.1.0"
description = "devkit-container smoke test app"
readme = "docs/README.md"
requires-python = ">=3.14"
dependencies = ["devkit-container"]

[project.optional-dependencies]
app = ["colorama>=0.4"]

[project.scripts]
run-app-smoke = "smoke_app:main"

[build-system]
requires = ["uv_build>=0.8.0,<0.12"]
build-backend = "uv_build"

[tool.uv.sources]
devkit-container = { path = "{wheel_dir}/{wheel}" }

[tool.docker]
services = ["smoke"]
required_persisted_dirs = ["persisted_data", "persisted_data/logs"]
{tail}"#;

/// The scratch project as a bare git repo with a `v0.1.0` tag, the way the Dockerfile
/// clones a real project. It depends on `devkit-container` like a real project does, through
/// a path source to the local wheel in place of the index, so every `uv sync --frozen` in the
/// image installs and keeps it from the lock. Installing the wheel between the syncs instead
/// cannot work: sync removes whatever the lock does not name. `uv lock` runs on the host; the
/// image syncs `--frozen`.
pub fn scratch_repo(work: &Path, wheel: &Path, app: &str, pyproject_tail: &str) -> PathBuf {
  let src = work.join("scratch");
  let wheel_name = wheel.file_name().unwrap().to_string_lossy();
  write(
    &src,
    "pyproject.toml",
    &PYPROJECT
      .replace("{wheel_dir}", WHEEL_DIR)
      .replace("{wheel}", &wheel_name)
      .replace("{tail}", pyproject_tail),
  );
  std::fs::create_dir_all(src.join(WHEEL_DIR)).unwrap();
  std::fs::copy(wheel, src.join(WHEEL_DIR).join(&*wheel_name)).unwrap();
  write(&src, "src/smoke_app/__init__.py", app);
  write(&src, "docs/README.md", "# smoke-app\n\nBuilt by the devkit-container smoke test.\n");
  // The wheel stays out of the repo: the image gets it from the build context.
  write(&src, ".gitignore", &format!(".venv/\n{WHEEL_DIR}/\n"));
  ok(Command::new("uv").args(["lock"]).current_dir(&src));
  let git = |args: &[&str]| {
    ok(
      Command::new("git")
        .args(["-c", "user.name=smoke", "-c", "user.email=smoke@example.invalid"])
        .args(args)
        .current_dir(&src),
    );
  };
  git(&["init", "-q", "-b", "main"]);
  git(&["add", "-A"]);
  git(&["commit", "-q", "-m", "smoke app"]);
  git(&["tag", "v0.1.0"]);
  let bare = work.join("context").join("scratch.git");
  std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
  ok(Command::new("git").arg("clone").arg("-q").arg("--bare").arg(&src).arg(&bare));
  bare
}

/// The maturin release that builds the wheel; the image carries its own Rust toolchain and gcc.
pub const MATURIN_IMAGE: &str = "ghcr.io/pyo3/maturin:v1.15.0";

/// The wheel the image installs, built from this checkout inside the maturin container: a
/// manylinux wheel exactly as a release builds one, so no cross-compiling and no C toolchain
/// on the host. Two named volumes keep the cargo registry and the target dir between runs
/// (the host's `target/` is never written to); they are caches, so `Cleanup` leaves them.
pub fn build_wheel(root: &Path, out: &Path) -> PathBuf {
  // Created here so it is ours, not the container's root: the temp dir must stay removable.
  std::fs::create_dir_all(out).unwrap();
  // `root()` is canonical, which on Windows is a `\\?\` verbatim path Docker does not take.
  let src = root.to_string_lossy().trim_start_matches(r"\\?\").to_string();
  let mut cmd = Command::new("docker");
  cmd
    .args(["run", "--rm", "-v"])
    .arg(format!("{src}:/io"))
    .arg("-v")
    .arg(format!("{}:/out", out.display()))
    .args(["-v", "devkit-container-cargo-registry:/usr/local/cargo/registry"])
    .args(["-v", "devkit-container-target:/build", "-e", "CARGO_TARGET_DIR=/build"])
    .args([MATURIN_IMAGE, "build", "--release", "--out", "/out"]);
  ok(&mut cmd);
  let wheel = std::fs::read_dir(out)
    .unwrap()
    .flatten()
    .map(|e| e.path())
    .find(|p| p.extension().is_some_and(|x| x == "whl"))
    .expect("maturin wrote a wheel");
  let name = wheel.file_name().unwrap().to_string_lossy().into_owned();
  assert!(
    name.contains("manylinux") && name.ends_with("x86_64.whl"),
    "expected a manylinux x86_64 wheel, got {name}"
  );
  wheel
}

/// The shipped template with `{python_dir}` filled, its one gate applied (the wireguard block
/// kept or dropped, both marker lines always dropped; the parser lives in `setup`, so this is
/// a local strip), the bare repo copied in for the clone to read, and the local wheel copied
/// to where the scratch app's lock points before the first sync, so the package arrives the
/// way a real build gets it: from uv.lock, by `uv sync --frozen`. Nothing else changes.
pub fn dockerfile(root: &Path, wheel_name: &str, wireguard: bool) -> String {
  let template = std::fs::read_to_string(root.join("python/devkit_container/template.Dockerfile")).unwrap();
  let (mut copied, mut wheeled, mut gated) = (0, 0, 0);
  let mut in_gate = false;
  let mut out = String::new();
  for line in template.lines() {
    if line.trim() == "# !if keys(\"tool.docker.wireguard\"):" {
      in_gate = true;
      gated += 1;
      continue;
    }
    if in_gate && line.trim() == "# !end" {
      in_gate = false;
      continue;
    }
    if in_gate && !wireguard {
      continue;
    }
    if line.starts_with("RUN git clone ") {
      out.push_str("COPY scratch.git /tmp/scratch.git\n");
      copied += 1;
    }
    if wheeled == 0 && line.starts_with("RUN --mount=type=cache") {
      out.push_str(&format!("COPY {wheel_name} /app/{WHEEL_DIR}/{wheel_name}\n"));
      wheeled += 1;
    }
    out.push_str(&line.replace("{python_dir}", "src"));
    out.push('\n');
  }
  assert_eq!((copied, wheeled), (1, 1), "the template changed shape; update this helper");
  assert_eq!(gated, 1, "the template changed shape; update this helper");
  assert!(!in_gate, "the wireguard block never closed");
  out
}

pub fn docker(args: &[&str]) -> Command {
  let mut c = Command::new("docker");
  c.args(args);
  c
}

/// Wheel, scratch repo, Dockerfile, `docker build`; returns the wheel's file name.
pub fn build_image(work: &Path, root: &Path, app: &str, pyproject_tail: &str, wireguard: bool, tag: &str) -> String {
  eprintln!("building the wheel in {MATURIN_IMAGE}");
  let wheel = build_wheel(root, &work.join("wheels"));
  let wheel_name = wheel.file_name().unwrap().to_string_lossy().into_owned();
  eprintln!("scratch project + bare repo");
  scratch_repo(work, &wheel, app, pyproject_tail);
  let context = work.join("context");
  std::fs::copy(&wheel, context.join(&wheel_name)).unwrap();
  std::fs::write(context.join("Dockerfile"), dockerfile(root, &wheel_name, wireguard)).unwrap();
  eprintln!("docker build {tag}");
  ok(
    docker(&[
      "build",
      "--build-arg",
      "GIT_TAG=v0.1.0",
      "--build-arg",
      "GIT_REPO=file:///tmp/scratch.git",
      "-t",
    ])
    .arg(tag)
    .arg(&context),
  );
  wheel_name
}
