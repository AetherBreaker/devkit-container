//! The supervising entrypoint end to end (spec 12): one image built with the wireguard mode
//! on, run three ways: supervise without a tunnel (an edited pyproject), wireguard against a
//! hub container built from the same image, and the ping against a local HTTP listener.
//! Needs docker, the network and a host kernel with the wireguard module
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
        # Atomic, as aeth_ext writes it: a reader must never see a truncated file.
        with open("/app/persisted_data/logs/heartbeat.txt.tmp", "w") as f:
            f.write(datetime.datetime.now(datetime.UTC).isoformat())
        os.replace("/app/persisted_data/logs/heartbeat.txt.tmp", "/app/persisted_data/logs/heartbeat.txt")
        time.sleep(1)
    sys.exit(int(os.environ.get("SMOKE_EXIT_ON_TERM", "0")))
"#;

/// Poll `probe` until true; a timeout panics with the logs of `containers`, the only
/// evidence left once `Cleanup` has removed them.
fn wait_for(what: &str, timeout: Duration, containers: &[&str], mut probe: impl FnMut() -> bool) {
  let deadline = Instant::now() + timeout;
  while !probe() {
    if Instant::now() >= deadline {
      let dump: Vec<String> = containers
        .iter()
        .map(|c| {
          format!(
            "--- docker logs {c}
{}",
            logs(c)
          )
        })
        .collect();
      panic!(
        "timed out waiting for {what}
{}",
        dump.join(
          "
"
        )
      );
    }
    std::thread::sleep(Duration::from_secs(1));
  }
}

fn exec(container: &str, args: &[&str]) -> std::process::Output {
  docker(&["exec", container]).args(args).output().unwrap()
}

/// `docker exec` that must succeed; panics with both streams otherwise.
fn exec_ok(container: &str, args: &[&str]) -> std::process::Output {
  let mut cmd = docker(&["exec", container]);
  cmd.args(args);
  ok(&mut cmd)
}

/// The two-file check with a max age of 10 s: both files are rewritten every second here (the
/// app's by the app, the tunnel's by each fresh poll), so a file the supervisor has stopped
/// writing reads stale within seconds instead of the production 180.
fn healthcheck(container: &str) -> std::process::Output {
  exec(
    container,
    &[
      "/app/.venv/bin/devkit-container",
      "healthcheck",
      "--max-age",
      "10",
      "--file",
      "/app/persisted_data/logs/heartbeat.txt",
      "--file",
      "/app/persisted_data/logs/wireguard-heartbeat.txt",
    ],
  )
}

fn logs(container: &str) -> String {
  String::from_utf8_lossy(&docker(&["logs", container]).output().unwrap().stderr).into_owned()
}

fn wg_key(image: &str) -> (String, String) {
  let private = text(&ok(&mut docker(&["run", "--rm", "--entrypoint", "wg", image, "genkey"])))
    .trim()
    .to_string();
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
#[ignore = "needs docker, the network and the wireguard kernel module; run with --ignored"]
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
    docker(&[
      "run",
      "-d",
      "--name",
      &app,
      "--network",
      &net,
      "-v",
      &mount,
      "-e",
      "SMOKE_EXIT_ON_TERM=7",
      "-e",
      "HEARTBEAT_SLUG=smoke",
      "-e",
      "PINGKEY=k",
      "--entrypoint",
      "sh",
      &image,
    ])
    .args([
      "-c",
      "sed 's/^wireguard = true/supervise = true/' /app/pyproject.toml > /tmp/p.toml && exec /app/.venv/bin/devkit-container run --pyproject /tmp/p.toml",
    ]),
  );
  wait_for("the app's report", Duration::from_secs(30), &[&app], || {
    exec(&app, &["cat", "/app/persisted_data/report.json"]).status.success()
  });
  let report: serde_json::Value = serde_json::from_slice(&exec(&app, &["cat", "/app/persisted_data/report.json"]).stdout).unwrap();
  eprintln!("{report:#}");
  assert_eq!(report["ppid"], 1, "the supervisor is PID 1");
  assert_ne!(report["pid"], 1);
  assert_eq!(report["uid"], 999);
  assert_eq!(report["gid"], 999);
  assert_eq!(report["groups"], serde_json::json!([]), "no supplementary groups");
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
  assert!(logs(&app).contains("app exited with 7"), "{}", logs(&app));
  ok(&mut docker(&["rm", "-f", &app]));
  ok(&mut docker(&["volume", "rm", "-f", &guard.volume]));

  // --- the hub: the same image, wireguard-tools inside, keys generated there too.
  let (hub_priv, hub_pub) = wg_key(&image);
  let (spoke_priv, spoke_pub) = wg_key(&image);
  let hub = format!("hub-{id}");
  guard.containers.push(hub.clone());
  ok(
    docker(&[
      "run",
      "-d",
      "--name",
      &hub,
      "--network",
      &net,
      "--cap-add",
      "NET_ADMIN",
      "-e",
      &format!("HUB_KEY={hub_priv}"),
      "--entrypoint",
      "sh",
      &image,
    ])
    .args([
      "-c",
      &format!(
        "printf '%s\\n' \"$HUB_KEY\" > /tmp/k && ip link add dev wg0 type wireguard && wg set wg0 listen-port 51820 private-key /tmp/k peer {spoke_pub} allowed-ips 10.8.0.20/32 && ip address add 10.8.0.1/24 dev wg0 && ip link set up dev wg0 && exec sleep infinity"
      ),
    ]),
  );
  // --- the ping listener: python's http.server as 999; its access log is the assertion.
  let hc = format!("hc-{id}");
  guard.containers.push(hc.clone());
  ok(&mut docker(&[
    "run",
    "-d",
    "--name",
    &hc,
    "--network",
    &net,
    "--user",
    "999:999",
    "--entrypoint",
    "/app/.venv/bin/python",
    &image,
    "-m",
    "http.server",
    "8080",
    "--bind",
    "0.0.0.0",
  ]));

  // --- the spoke: the app under the supervisor with the tunnel, a 1 s poll. The stale window
  // must exceed WireGuard's 120 s rekey period (`latest-handshakes` advances only on a rekey),
  // or a healthy tunnel reads as stale every few seconds and flaps; 150 leaves 30 s of slack.
  let spoke = format!("spoke-{id}");
  guard.containers.push(spoke.clone());
  ok(
    docker(&[
      "run",
      "-d",
      "--name",
      &spoke,
      "--network",
      &net,
      "--cap-add",
      "NET_ADMIN",
      "-v",
      &mount,
    ])
    .args([
      "-e",
      &format!("WG_PRIVATE_KEY={spoke_priv}"),
      "-e",
      "WG_ADDRESS=10.8.0.20/32",
      "-e",
      &format!("WG_PEER_PUBLIC_KEY={hub_pub}"),
    ])
    .args([
      "-e",
      &format!("WG_PEER_ENDPOINT={hub}:51820"),
      "-e",
      "WG_PEER_ALLOWED_IPS=10.8.0.0/24",
      "-e",
      "WG_PERSISTENT_KEEPALIVE=1",
    ])
    .args([
      "-e",
      "WG_POLL_SECS=1",
      "-e",
      "WG_STALE_SECS=150",
      "-e",
      "SMOKE_EXIT_ON_TERM=7",
      "-e",
      &format!("ALERTS_HEALTHCHECK_PING_URL=http://{hc}:8080/ping/x"),
    ])
    .arg(&image),
  );
  wait_for("the tunnel heartbeat", Duration::from_secs(90), &[&spoke, &hub], || {
    exec(&spoke, &["cat", "/app/persisted_data/logs/wireguard-heartbeat.txt"])
      .status
      .success()
  });
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
  let stat = text(&exec_ok(
    &spoke,
    &["stat", "-c", "%a %u", "/app/persisted_data/logs/wireguard-heartbeat.txt"],
  ));
  assert!(stat.trim().starts_with("644"), "world-readable: {stat}");

  // --- the hub forgets the peer: stale, then the healthcheck says which file, then a re-up.
  exec_ok(&hub, &["wg", "set", "wg0", "peer", &spoke_pub, "remove"]);
  // Stale lands up to 150 s after the last handshake, whenever that was.
  wait_for("a stale tunnel", Duration::from_secs(200), &[&spoke, &hub], || {
    healthcheck(&spoke).status.code() == Some(1)
  });
  let stale = healthcheck(&spoke);
  let err = String::from_utf8_lossy(&stale.stderr);
  assert!(
    err.contains("wireguard-heartbeat.txt") && err.contains("stale by") && !err.contains("logs/heartbeat.txt:"),
    "{err}"
  );
  wait_for("a re-up in the log", Duration::from_secs(20), &[&spoke], || {
    logs(&spoke).contains("re-up")
  });
  // --- the ping so far: /start once, then plain while fresh. The tunnel file the supervisor
  // stopped writing goes stale a full window after that, and only then comes the one /fail;
  // the peer stays forgotten until it does.
  wait_for("/fail at the listener", Duration::from_secs(200), &[&spoke, &hc], || {
    logs(&hc).contains("/ping/x/fail")
  });
  let log = logs(&hc);
  eprintln!("--- listener log after the outage");
  eprintln!("{log}");
  assert_eq!(log.matches("/ping/x/start").count(), 1, "{log}");
  assert_eq!(log.matches("/ping/x/fail").count(), 1, "{log}");
  assert!(log.contains("\"GET /ping/x HTTP"), "{log}");
  let plain_before = log.matches("\"GET /ping/x HTTP").count();
  // --- the hub learns the peer again: a re-up handshakes, the file is fresh, plain pings resume.
  exec_ok(&hub, &["wg", "set", "wg0", "peer", &spoke_pub, "allowed-ips", "10.8.0.20/32"]);
  wait_for("a fresh tunnel again", Duration::from_secs(60), &[&spoke, &hub], || {
    healthcheck(&spoke).status.success()
  });
  wait_for("plain pings again", Duration::from_secs(30), &[&spoke, &hc], || {
    logs(&hc).matches("\"GET /ping/x HTTP").count() > plain_before
  });
  let log = logs(&hc);
  assert_eq!(log.matches("/ping/x/start").count(), 1, "no second /start: {log}");
  assert_eq!(log.matches("/ping/x/fail").count(), 1, "one /fail per outage: {log}");

  // --- SIGTERM reaches the child and its code passes through; the interface goes down.
  ok(&mut docker(&["kill", "--signal", "TERM", &spoke]));
  assert_eq!(text(&ok(&mut docker(&["wait", &spoke]))).trim(), "7");
  let spoke_logs = logs(&spoke);
  eprintln!("--- spoke log");
  eprintln!("{spoke_logs}");
  assert!(
    spoke_logs.contains("wireguard public key") && spoke_logs.contains("app exited with 7"),
    "{spoke_logs}"
  );
}
