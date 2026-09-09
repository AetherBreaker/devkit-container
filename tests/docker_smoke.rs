//! The image end to end: the shipped Dockerfile template built around a scratch app,
//! started through the entrypoint with a mounted persisted dir, and the app reporting on
//! the environment it finds itself in. Needs docker, the `x86_64-unknown-linux-musl`
//! target and the network (base image, PyPI), so it is `#[ignore]`; CI runs it with
//! `--ignored`. The template is used verbatim except for where two things come from: the
//! package resolves to this checkout's wheel through a path source, not the release on the
//! index, and the git clone reads a bare copy of the scratch repo inside the build context,
//! not GitHub.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const IMAGE_TAG_PREFIX: &str = "devkit-smoke";
const MUSL: &str = "x86_64-unknown-linux-musl";

fn root() -> PathBuf {
  Path::new(env!("CARGO_MANIFEST_DIR")).canonicalize().unwrap()
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

/// Where the scratch app's path source finds the wheel, relative to its `pyproject.toml`:
/// under the scratch tree on the host for `uv lock`, and under `/app` in the image for the
/// frozen syncs.
const WHEEL_DIR: &str = "wheels";

const PYPROJECT: &str = r#"[project]
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
"#;

/// The scratch project as a bare git repo with a `v0.1.0` tag, the way the Dockerfile
/// clones a real project. It depends on `devkit-container` like a real project does, through
/// a path source to the local wheel in place of the index, so every `uv sync --frozen` in the
/// image installs and keeps it from the lock. Installing the wheel between the syncs instead
/// cannot work: sync removes whatever the lock does not name. `uv lock` runs on the host; the
/// image syncs `--frozen`.
fn scratch_repo(work: &Path, wheel: &Path) -> PathBuf {
  let src = work.join("scratch");
  let wheel_name = wheel.file_name().unwrap().to_string_lossy();
  write(
    &src,
    "pyproject.toml",
    &PYPROJECT.replace("{wheel_dir}", WHEEL_DIR).replace("{wheel}", &wheel_name),
  );
  std::fs::create_dir_all(src.join(WHEEL_DIR)).unwrap();
  std::fs::copy(wheel, src.join(WHEEL_DIR).join(&*wheel_name)).unwrap();
  write(&src, "src/smoke_app/__init__.py", APP);
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

/// The wheel the image installs, built from this checkout for the image's platform. The
/// binary is a static musl build so the same wheel serves a glibc image, and the platform
/// tag is the generic `linux` one so uv accepts it there; a released wheel is manylinux,
/// which is the only difference between this wheel and a released one.
fn build_wheel(root: &Path, out: &Path) -> PathBuf {
  let mut cmd = Command::new("uv");
  cmd
    .args([
      "run",
      "maturin",
      "build",
      "--release",
      "--target",
      MUSL,
      "--compatibility",
      "linux",
      "--out",
    ])
    .arg(out)
    .current_dir(root);
  // Windows has no `cc` for the musl target; rustc's bundled lld links it self-contained.
  if cfg!(windows) {
    cmd.env("CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER", "rust-lld");
  }
  ok(&mut cmd);
  let wheel = std::fs::read_dir(out)
    .unwrap()
    .flatten()
    .map(|e| e.path())
    .find(|p| p.extension().is_some_and(|x| x == "whl"))
    .expect("maturin wrote a wheel");
  let name = wheel.file_name().unwrap().to_string_lossy().into_owned();
  assert!(
    name.ends_with("linux_x86_64.whl"),
    "the image needs the generic linux tag, got {name}"
  );
  wheel
}

/// The shipped template with `{python_dir}` filled, the bare repo copied in for the clone to
/// read, and the local wheel copied to where the scratch app's lock points before the first
/// sync, so the package arrives the way a real build gets it: from uv.lock, by `uv sync
/// --frozen`. Nothing else changes.
fn dockerfile(root: &Path, wheel_name: &str) -> String {
  let template = std::fs::read_to_string(root.join("python/devkit_container/template.Dockerfile")).unwrap();
  let (mut copied, mut wheeled) = (0, 0);
  let mut out = String::new();
  for line in template.lines() {
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
  assert_eq!((copied, wheeled), (1, 1), "the template changed shape; update this test");
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
  let root = root();
  let work = tempfile::tempdir().unwrap();
  let id = format!("{}-{}", std::process::id(), std::time::UNIX_EPOCH.elapsed().unwrap().as_secs());
  let guard = Cleanup {
    image: format!("{IMAGE_TAG_PREFIX}:{id}"),
    volume: format!("{IMAGE_TAG_PREFIX}-{id}"),
  };

  eprintln!("building the wheel for {MUSL}");
  let wheel = build_wheel(&root, &work.path().join("wheels"));
  let wheel_name = wheel.file_name().unwrap().to_string_lossy().into_owned();
  eprintln!("scratch project + bare repo");
  scratch_repo(work.path(), &wheel);
  let context = work.path().join("context");
  std::fs::copy(&wheel, context.join(&wheel_name)).unwrap();
  std::fs::write(context.join("Dockerfile"), dockerfile(&root, &wheel_name)).unwrap();

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
  let readme = docker(&[
    "run",
    "--rm",
    "--entrypoint",
    "/app/.venv/bin/devkit-container",
    &guard.image,
    "readme",
  ])
  .output()
  .unwrap();
  assert_eq!(text(&readme), "docs/README.md");
  let extra = docker(&[
    "run",
    "--rm",
    "--entrypoint",
    "/app/.venv/bin/devkit-container",
    &guard.image,
    "app-extra",
  ])
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
