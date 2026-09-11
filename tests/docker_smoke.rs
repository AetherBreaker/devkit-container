//! The image end to end: the shipped Dockerfile template built around a scratch app,
//! started through the entrypoint with a mounted persisted dir, and the app reporting on
//! the environment it finds itself in. Needs docker and the network (base image, PyPI), so
//! it is `#[ignore]`; CI runs it with `--ignored`. The template is used verbatim except for
//! where two things come from: the package resolves to this checkout's wheel through a path
//! source, not the release on the index, and the git clone reads a bare copy of the scratch
//! repo inside the build context, not GitHub.

mod common;

use common::*;

const IMAGE_TAG_PREFIX: &str = "devkit-smoke";

#[test]
#[ignore = "needs docker and the network; run with --ignored"]
fn the_image_starts_the_app_through_the_entrypoint_with_a_working_environment() {
  ok(&mut docker(&["version", "--format", "{{.Server.Os}}"]));
  let root = root();
  let work = tempfile::tempdir().unwrap();
  let id = format!("{}-{}", std::process::id(), std::time::UNIX_EPOCH.elapsed().unwrap().as_secs());
  let guard = Cleanup {
    image: format!("{IMAGE_TAG_PREFIX}:{id}"),
    volume: format!("{IMAGE_TAG_PREFIX}-{id}"),
    containers: vec![],
    network: None,
  };
  build_image(work.path(), &root, REPORT_APP, "", false, &guard.image);

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

  // The healthcheck subcommand in the image, against the file the app just wrote and against
  // a stale one written by hand.
  let fresh = docker(&[
    "run",
    "--rm",
    "-v",
    &mount,
    "--entrypoint",
    "/app/.venv/bin/devkit-container",
    &guard.image,
    "healthcheck",
  ])
  .output()
  .unwrap();
  assert!(fresh.status.success(), "{}", String::from_utf8_lossy(&fresh.stderr));
  let stale = docker(&["run", "--rm", "-v", &mount, "--entrypoint", "sh", &guard.image])
    .args([
      "-c",
      "echo 2020-01-01T00:00:00+00:00 > /app/persisted_data/logs/heartbeat.txt && /app/.venv/bin/devkit-container healthcheck",
    ])
    .output()
    .unwrap();
  assert_eq!(stale.status.code(), Some(1));
  assert!(
    String::from_utf8_lossy(&stale.stderr).contains("stale by"),
    "{}",
    String::from_utf8_lossy(&stale.stderr)
  );
}
