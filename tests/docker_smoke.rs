//! The image end to end: the shipped Dockerfile template built around a scratch app,
//! started through the entrypoint with a mounted persisted dir, and the app reporting on
//! the environment it finds itself in. Needs docker, the `x86_64-unknown-linux-musl`
//! target and the network (base image, PyPI), so it is `#[ignore]`; CI runs it with
//! `--ignored`. The template is used verbatim except for where two things come from: the
//! entrypoint binary is the local cross-build, not the release download, and the git
//! clone reads a bare copy of the scratch repo inside the build context, not GitHub.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const IMAGE_TAG_PREFIX: &str = "devkit-smoke";
const MUSL: &str = "x86_64-unknown-linux-musl";

fn workspace() -> PathBuf {
  Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

/// Run to completion; panic with both streams on a non-zero exit.
fn ok(cmd: &mut Command) -> Output {
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

fn text(out: &Output) -> String {
  String::from_utf8_lossy(&out.stdout).into_owned()
}

fn write(root: &Path, rel: &str, content: &str) {
  let p = root.join(rel);
  std::fs::create_dir_all(p.parent().unwrap()).unwrap();
  std::fs::write(p, content).unwrap();
}

/// Best-effort removal of the image and the volume, whichever way the test ends.
struct Cleanup {
  image: String,
  volume: String,
}

impl Drop for Cleanup {
  fn drop(&mut self) {
    let _ = Command::new("docker").args(["rmi", "-f", &self.image]).output();
    let _ = Command::new("docker").args(["volume", "rm", "-f", &self.volume]).output();
  }
}

/// The app the image is built around. `main` checks the environment the entrypoint
/// promises and prints one JSON object; the test asserts its fields from outside.
const APP: &str = r#"import datetime
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

const PYPROJECT: &str = r#"[project]
name = "smoke-app"
version = "0.1.0"
description = "devkit-container smoke test app"
readme = "docs/README.md"
requires-python = ">=3.14"
dependencies = []

[project.optional-dependencies]
app = ["colorama>=0.4"]

[project.scripts]
run-app-smoke = "smoke_app:main"

[build-system]
requires = ["uv_build>=0.8.0,<0.12"]
build-backend = "uv_build"

[tool.docker]
services = ["smoke"]
required_persisted_dirs = ["persisted_data", "persisted_data/logs"]
"#;

/// The scratch project as a bare git repo with a `v0.1.0` tag, the way the Dockerfile
/// clones a real project. `uv lock` runs on the host: the image syncs `--frozen`.
fn scratch_repo(work: &Path) -> PathBuf {
  let src = work.join("scratch");
  write(&src, "pyproject.toml", PYPROJECT);
  write(&src, "src/smoke_app/__init__.py", APP);
  write(&src, "docs/README.md", "# smoke-app\n\nBuilt by the devkit-container smoke test.\n");
  write(&src, ".gitignore", ".venv/\n");
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

/// The entrypoint for the image, cross-built from this checkout.
fn build_entrypoint(ws: &Path) -> PathBuf {
  let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
  let target_dir = std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| ws.join("target"), PathBuf::from);
  let mut cmd = Command::new(cargo);
  cmd
    .args(["build", "--release", "-p", "aeth-devkit-container", "--target", MUSL])
    .arg("--target-dir")
    .arg(&target_dir)
    .current_dir(ws);
  // Windows has no `cc` for the musl target; rustc's bundled lld links it self-contained.
  if cfg!(windows) {
    cmd.env("CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER", "rust-lld");
  }
  ok(&mut cmd);
  target_dir.join(MUSL).join("release").join("devkit-container")
}

/// The shipped template with `{python_dir}` filled, the release download swapped for the
/// local binary, and the bare repo copied in for the clone to read. Nothing else changes.
fn dockerfile(ws: &Path) -> String {
  let template = std::fs::read_to_string(ws.join("python/aeth_devkit/templates/docker/template.Dockerfile")).unwrap();
  let (mut swapped, mut copied) = (0, 0);
  let mut out = String::new();
  for line in template.lines() {
    if line.starts_with("ADD https://") && line.contains("devkit-container") {
      out.push_str("COPY devkit-container /app/devkit-container\n");
      swapped += 1;
      continue;
    }
    if line.starts_with("RUN git clone ") {
      out.push_str("COPY scratch.git /tmp/scratch.git\n");
      copied += 1;
    }
    out.push_str(&line.replace("{python_dir}", "src"));
    out.push('\n');
  }
  assert_eq!((swapped, copied), (1, 1), "the template changed shape; update this test");
  out
}

fn docker(args: &[&str]) -> Command {
  let mut c = Command::new("docker");
  c.args(args);
  c
}

#[test]
#[ignore = "needs docker, the x86_64-unknown-linux-musl target and the network; run with --ignored"]
fn the_image_starts_the_app_through_the_entrypoint_with_a_working_environment() {
  ok(&mut docker(&["version", "--format", "{{.Server.Os}}"]));
  let ws = workspace();
  let work = tempfile::tempdir().unwrap();
  let id = format!("{}-{}", std::process::id(), std::time::UNIX_EPOCH.elapsed().unwrap().as_secs());
  let guard = Cleanup {
    image: format!("{IMAGE_TAG_PREFIX}:{id}"),
    volume: format!("{IMAGE_TAG_PREFIX}-{id}"),
  };

  eprintln!("cross-building the entrypoint for {MUSL}");
  let binary = build_entrypoint(&ws);
  eprintln!("scratch project + bare repo");
  scratch_repo(work.path());
  let context = work.path().join("context");
  std::fs::copy(&binary, context.join("devkit-container")).unwrap();
  std::fs::write(context.join("Dockerfile"), dockerfile(&ws)).unwrap();

  eprintln!("docker build {}", guard.image);
  ok(
    docker(&[
      "build",
      "--build-arg",
      "GIT_TAG=v0.1.0",
      "--build-arg",
      "GIT_REPO=file:///tmp/scratch.git",
      "-t",
    ])
    .arg(&guard.image)
    .arg(&context),
  );

  // Build-time queries, as the Dockerfile ran them.
  let readme = docker(&["run", "--rm", "--entrypoint", "/app/devkit-container", &guard.image, "readme"])
    .output()
    .unwrap();
  assert_eq!(text(&readme), "docs/README.md");
  let extra = docker(&["run", "--rm", "--entrypoint", "/app/devkit-container", &guard.image, "app-extra"])
    .output()
    .unwrap();
  assert_eq!(text(&extra), "--extra app");

  // Refused before anything is created: no volume, and not root.
  let no_volume = docker(&["run", "--rm", &guard.image]).output().unwrap();
  assert_eq!(no_volume.status.code(), Some(1));
  let err = String::from_utf8_lossy(&no_volume.stderr);
  assert!(
    err.contains("not backed by a bind mount") && err.contains("persisted_data"),
    "{err}"
  );
  let mount = format!("{}:/app/persisted_data", guard.volume);
  let not_root = docker(&["run", "--rm", "--user", "1000:1000", "-v", &mount, &guard.image])
    .output()
    .unwrap();
  assert_eq!(not_root.status.code(), Some(1));
  assert!(String::from_utf8_lossy(&not_root.stderr).contains("must run as root"));

  // The real start: a named volume (a Windows host folder would fake its ownership).
  eprintln!("docker run with {}", guard.volume);
  let run = docker(&["run", "--rm", "-v", &mount, &guard.image]).output().unwrap();
  let stdout = text(&run);
  let report: serde_json::Value = serde_json::from_str(stdout.trim())
    .unwrap_or_else(|e| panic!("no JSON report ({e}):\n{stdout}\n{}", String::from_utf8_lossy(&run.stderr)));
  eprintln!("{report:#}");
  assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
  assert_eq!(report["failures"], serde_json::json!([]));
  assert_eq!(report["pid_1"], 1);
  assert_eq!(report["uid_999"], 999);
  assert_eq!(report["gid_999"], 999);
  assert_eq!(report["cwd_app"], "/app");
  assert_eq!(report["persisted_owned"], serde_json::json!([999, 999]));
  assert_eq!(report["logs_owned"], serde_json::json!([999, 999]));
  assert_eq!(report["persisted_writable"], true);
  assert_eq!(report["app_read_only"], true);
  assert_eq!(report["optimize"], 1);
  assert!(report["installed_as_wheel"].as_str().unwrap().contains("/site-packages/"));

  // The heartbeat outlives the container and stays the app user's.
  let after = ok(
    docker(&["run", "--rm", "-v", &mount, "--entrypoint", "sh", &guard.image])
      .args(["-c", "stat -c %u:%g /app/persisted_data /app/persisted_data/logs /app/persisted_data/logs/heartbeat.txt && cat /app/persisted_data/logs/heartbeat.txt"]),
  );
  let after = text(&after);
  let mut lines = after.lines();
  for _ in 0..3 {
    assert_eq!(lines.next(), Some("999:999"), "{after}");
  }
  assert!(lines.next().is_some_and(|ts| ts.contains('T')), "{after}");
}
