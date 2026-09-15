//! Fetched mode end to end (spec 13): the image built with the mode on, a hub container on the
//! test network serving `/version` and holding the fixture hub's key, and the bundle fetched
//! from real GitHub, from `AetherBreaker/wireguard-hub-smoke`. Needs docker, the network, the
//! WireGuard kernel module and `DEVKIT_SMOKE_WG_HUB_TOKEN`; `#[ignore]`, CI runs it.

mod common;

use std::time::{Duration, Instant};

use common::*;

const FIXTURE_REPO: &str = "AetherBreaker/wireguard-hub-smoke";
const TOKEN_VAR: &str = "DEVKIT_SMOKE_WG_HUB_TOKEN";

// The fixture's key pairs. They guard nothing: the hub is a throwaway container on a test
// network and the "spoke" is this test. Committed so the test needs no secret beyond the token.
const HUB_PRIVATE: &str = "YEoYA+0twxQT91W5VQ+2fImxyT0TAythAx6W2tPkGHk="; // gitleaks:allow trufflehog:ignore ggignore
const HUB_PUBLIC: &str = "mLANVKHPCTOCpBzbTtJd+pKV69i0fQFxlV4EvHjxyWQ=";
const SPOKE_PRIVATE: &str = "ECDIbwJdnc3PZipjYiwQS2PuEPgszzterRF02ftUs38="; // gitleaks:allow trufflehog:ignore ggignore
const SPOKE_PUBLIC: &str = "q6CvssjZh5QkveaDbGhF3twh0Ff6Lq0jRBPUOfoDk3s=";

/// The app: heartbeats every second, serves the consent socket (`hold` while
/// `/app/persisted_data/hold` exists), logs every request and signal to consent.log, exits on
/// SIGINT/SIGTERM with SMOKE_EXIT_ON_TERM.
const CONSENT_APP: &str = r#"import asyncio
import datetime
import json
import os
import signal
import sys

HOLD = "/app/persisted_data/hold"
LOG = "/app/persisted_data/consent.log"


def note(line):
    with open(LOG, "a") as f:
        f.write(line + "\n")


async def handle(reader, writer):
    line = (await reader.readline()).decode().strip()
    reply = "hold" if os.path.exists(HOLD) else "ok"
    note(f"{line} -> {reply}")
    writer.write((reply + "\n").encode())
    await writer.drain()
    writer.close()


async def run():
    stop = asyncio.Event()
    loop = asyncio.get_running_loop()
    for sig in (signal.SIGINT, signal.SIGTERM):
        loop.add_signal_handler(sig, lambda s=sig: (note(f"signal {s.name}"), stop.set()))
    path = os.environ["DEVKIT_CONSENT_SOCKET"]
    if os.path.exists(path):
        os.remove(path)
    server = await asyncio.start_unix_server(handle, path)
    with open("/app/persisted_data/report.json", "w") as f:
        json.dump(
            {
                "pid": os.getpid(),
                "uid": os.getuid(),
                "wg_hub_token_present": "WG_HUB_TOKEN" in os.environ,
                "wg_private_key_present": "WG_PRIVATE_KEY" in os.environ,
                "consent_socket": path,
            },
            f,
        )
    while not stop.is_set():
        with open("/app/persisted_data/logs/heartbeat.txt.tmp", "w") as f:
            f.write(datetime.datetime.now(datetime.UTC).isoformat())
        os.replace("/app/persisted_data/logs/heartbeat.txt.tmp", "/app/persisted_data/logs/heartbeat.txt")
        try:
            await asyncio.wait_for(stop.wait(), 1)
        except TimeoutError:
            pass
    server.close()
    sys.exit(int(os.environ.get("SMOKE_EXIT_ON_TERM", "0")))


def main():
    asyncio.run(run())
"#;

/// `/version` from `/tmp/version`, on 8000, inside the hub container.
const VERSION_SERVER: &str = r#"import http.server, socketserver
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/version":
            body = open("/tmp/version", "rb").read()
            self.send_response(200); self.send_header("Content-Type", "text/plain; charset=utf-8"); self.send_header("Content-Length", str(len(body))); self.end_headers(); self.wfile.write(body)
        else:
            self.send_response(404); self.send_header("Content-Length", "0"); self.end_headers()
    def log_message(self, *a):
        pass
socketserver.ThreadingTCPServer.allow_reuse_address = True
socketserver.ThreadingTCPServer(("0.0.0.0", 8000), H).serve_forever()
"#;

fn wait_for(what: &str, timeout: Duration, containers: &[&str], mut probe: impl FnMut() -> bool) {
  let deadline = Instant::now() + timeout;
  while !probe() {
    if Instant::now() >= deadline {
      let dump: Vec<String> = containers.iter().map(|c| format!("--- docker logs {c}\n{}", logs(c))).collect();
      panic!("timed out waiting for {what}\n{}", dump.join("\n"));
    }
    std::thread::sleep(Duration::from_secs(1));
  }
}

fn exec(container: &str, args: &[&str]) -> std::process::Output {
  docker(&["exec", container]).args(args).output().unwrap()
}

fn exec_ok(container: &str, args: &[&str]) -> std::process::Output {
  let mut cmd = docker(&["exec", container]);
  cmd.args(args);
  ok(&mut cmd)
}

fn logs(container: &str) -> String {
  String::from_utf8_lossy(&docker(&["logs", container]).output().unwrap().stderr).into_owned()
}

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

fn wait_code(container: &str) -> String {
  text(&ok(&mut docker(&["wait", container]))).trim().to_string()
}

/// Everything a spoke needs; `extra` overrides or adds.
fn spoke_env(token: &str, private_key: &str, extra: &[(&str, &str)]) -> Vec<String> {
  let mut env: Vec<(String, String)> = vec![
    ("WG_PRIVATE_KEY".into(), private_key.into()),
    ("WG_HUB_URL".into(), "http://wireguard-hub:8000".into()),
    ("WG_HUB_REPO".into(), FIXTURE_REPO.into()),
    ("WG_HUB_TOKEN".into(), token.into()),
    ("WG_POLL_SECS".into(), "1".into()),
    ("WG_STALE_SECS".into(), "150".into()),
    ("WG_VERSION_POLL_SECS".into(), "3".into()),
    ("WG_DISCONNECTED_LIMIT_SECS".into(), "5".into()),
    ("SMOKE_EXIT_ON_TERM".into(), "7".into()),
  ];
  for (k, v) in extra {
    env.retain(|(name, _)| name != k);
    env.push((k.to_string(), v.to_string()));
  }
  env.into_iter().flat_map(|(k, v)| ["-e".to_string(), format!("{k}={v}")]).collect()
}

#[test]
#[ignore = "needs docker, the network, the wireguard kernel module and DEVKIT_SMOKE_WG_HUB_TOKEN; run with --ignored"]
fn fetched_mode_boots_from_the_hub_release_re_applies_in_place_asks_before_giving_up_and_obeys_a_removal() {
  let token = std::env::var(TOKEN_VAR)
    .ok()
    .filter(|t| !t.trim().is_empty())
    .unwrap_or_else(|| panic!("{TOKEN_VAR} is not set: the fetched-mode smoke test reads the fixture release with it (spec 11)"));
  ok(&mut docker(&["version", "--format", "{{.Server.Os}}"]));
  let root = root();
  let work = tempfile::tempdir().unwrap();
  let id = format!("{}-{}", std::process::id(), std::time::UNIX_EPOCH.elapsed().unwrap().as_secs());
  let mut guard = Cleanup {
    image: format!("devkit-smoke-fetched:{id}"),
    volume: format!("devkit-smoke-fetched-{id}"),
    containers: vec![],
    network: Some(format!("devkit-smoke-fetched-{id}")),
  };
  let image = guard.image.clone();
  let net = guard.network.clone().unwrap();
  build_image(work.path(), &root, CONSENT_APP, "wireguard = true\n", true, &image);
  ok(&mut docker(&["network", "create", &net]));
  let mount = format!("{}:/app/persisted_data", guard.volume);

  // --- the hub: the fixture's key, the spoke enrolled at .20, /version at v0.1.0.
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
      "--network-alias",
      "wireguard-hub",
      "--cap-add",
      "NET_ADMIN",
    ])
    .args([
      "-e",
      &format!("HUB_KEY={HUB_PRIVATE}"),
      "-e",
      &format!("VERSION_SERVER={VERSION_SERVER}"),
      "--entrypoint",
      "sh",
      &image,
    ])
    .args([
      "-c",
      &format!(
        "printf '%s\\n' \"$HUB_KEY\" > /tmp/k && ip link add dev wg0 type wireguard && wg set wg0 listen-port 51820 private-key /tmp/k peer {SPOKE_PUBLIC} allowed-ips 10.8.0.20/32 && ip address add 10.8.0.1/24 dev wg0 && ip link set up dev wg0 && echo v0.1.0 > /tmp/version && exec /app/.venv/bin/python -c \"$VERSION_SERVER\""
      ),
    ]),
  );
  wait_for("the hub's version endpoint", Duration::from_secs(30), &[&hub], || {
    exec(&hub, &["sh", "-c", "wg show wg0 public-key && cat /tmp/version"])
      .status
      .success()
  });
  let hub_pub = text(&exec_ok(&hub, &["wg", "show", "wg0", "public-key"])).trim().to_string();
  assert_eq!(hub_pub, HUB_PUBLIC, "the constants match the fixture");

  // --- a fresh key is not enrolled: refused, the key in the log.
  let (fresh_priv, fresh_pub) = wg_key(&image);
  let stranger = format!("stranger-{id}");
  guard.containers.push(stranger.clone());
  ok(
    docker(&[
      "run",
      "-d",
      "--name",
      &stranger,
      "--network",
      &net,
      "--cap-add",
      "NET_ADMIN",
      "-v",
      &mount,
    ])
    .args(spoke_env(&token, &fresh_priv, &[]))
    .arg(&image),
  );
  assert_eq!(wait_code(&stranger), "1");
  let log = logs(&stranger);
  assert!(
    log.contains("not enrolled in v0.1.0") && log.contains(&fresh_pub) && log.contains("then redeploy"),
    "{log}"
  );
  assert!(!exec(&stranger, &["true"]).status.success(), "the container has exited");
  ok(&mut docker(&["rm", "-f", &stranger]));

  // --- the same key under the switch: runs Disconnected, the app started.
  let tolerated = format!("tolerated-{id}");
  guard.containers.push(tolerated.clone());
  ok(
    docker(&[
      "run",
      "-d",
      "--name",
      &tolerated,
      "--network",
      &net,
      "--cap-add",
      "NET_ADMIN",
      "-v",
      &mount,
    ])
    .args(spoke_env(&token, &fresh_priv, &[("WG_TOLERATE_DISCONNECTED", "1")]))
    .arg(&image),
  );
  wait_for("the tolerated app's report", Duration::from_secs(60), &[&tolerated], || {
    exec(&tolerated, &["cat", "/app/persisted_data/report.json"]).status.success()
  });
  let log = logs(&tolerated);
  assert!(log.contains("starting Disconnected") && log.contains("not enrolled"), "{log}");
  let hc = healthcheck(&tolerated);
  assert!(
    String::from_utf8_lossy(&hc.stderr).contains("wireguard-heartbeat.txt"),
    "unhealthy, naming the tunnel file"
  );
  ok(&mut docker(&["rm", "-f", &tolerated]));
  ok(&mut docker(&["volume", "rm", "-f", &guard.volume]));

  // --- no hub and no cache: refused with config unavailable.
  let lost = format!("lost-{id}");
  guard.containers.push(lost.clone());
  ok(
    docker(&[
      "run",
      "-d",
      "--name",
      &lost,
      "--network",
      &net,
      "--cap-add",
      "NET_ADMIN",
      "-v",
      &mount,
    ])
    .args(spoke_env(&token, SPOKE_PRIVATE, &[("WG_HUB_URL", "http://nowhere.invalid:8000")]))
    .arg(&image),
  );
  assert_eq!(wait_code(&lost), "1");
  let log = logs(&lost);
  assert!(log.contains("config unavailable") && log.contains("refusing to start"), "{log}");
  ok(&mut docker(&["rm", "-f", &lost]));
  ok(&mut docker(&["volume", "rm", "-f", &guard.volume]));

  // --- the enrolled spoke: boot fetch, Connected, the cache, the log file, the scrubbed token.
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
    .args(spoke_env(&token, SPOKE_PRIVATE, &[]))
    .arg(&image),
  );
  wait_for("both heartbeats", Duration::from_secs(90), &[&spoke, &hub], || {
    exec(&spoke, &["cat", "/app/persisted_data/logs/wireguard-heartbeat.txt"])
      .status
      .success()
      && exec(&spoke, &["cat", "/app/persisted_data/logs/heartbeat.txt"]).status.success()
  });
  assert!(healthcheck(&spoke).status.success());
  let log = logs(&spoke);
  assert!(
    log.contains("fetched bundle v0.1.0") && log.contains("applied v0.1.0") && log.contains("wireguard handshake"),
    "{log}"
  );
  let stat = text(&exec_ok(
    &spoke,
    &["stat", "-c", "%a %u", "/app/persisted_data/wireguard/peers.toml"],
  ));
  assert_eq!(stat.trim(), "644 999", "the cache file's mode and owner");
  let cached = text(&exec_ok(&spoke, &["cat", "/app/persisted_data/wireguard/peers.toml"]));
  assert!(cached.contains("hub_version = \"v0.1.0\""));
  let logfile = text(&exec_ok(&spoke, &["cat", "/app/persisted_data/logs/devkit-container.log"]));
  // The boot lines precede prepare, which makes the folder on a fresh volume; the file holds the
  // supervisor's lines from then on (spec 5.8).
  assert!(logfile.contains("wireguard Connected"), "{logfile}");
  let report: serde_json::Value = serde_json::from_slice(&exec(&spoke, &["cat", "/app/persisted_data/report.json"]).stdout).unwrap();
  assert_eq!(report["wg_hub_token_present"], false);
  assert_eq!(report["wg_private_key_present"], false);
  assert_eq!(report["consent_socket"], "/run/devkit/consent.sock");

  // --- the hub releases v0.2.0 (the spoke at .21): applied in place, the interface untouched.
  let ifindex = text(&exec_ok(&spoke, &["cat", "/sys/class/net/wg0/ifindex"]));
  exec_ok(&hub, &["wg", "set", "wg0", "peer", SPOKE_PUBLIC, "allowed-ips", "10.8.0.21/32"]);
  exec_ok(&hub, &["sh", "-c", "echo v0.2.0 > /tmp/version"]);
  wait_for("the in-place re-apply", Duration::from_secs(30), &[&spoke, &hub], || {
    logs(&spoke).contains("re-applied v0.2.0 in place")
  });
  let addrs = text(&exec_ok(&spoke, &["ip", "-4", "-o", "addr", "show", "wg0"]));
  assert!(addrs.contains("10.8.0.21/32") && !addrs.contains("10.8.0.20/32"), "{addrs}");
  assert_eq!(
    text(&exec_ok(&spoke, &["cat", "/sys/class/net/wg0/ifindex"])),
    ifindex,
    "wg0 never went down"
  );
  assert!(!logs(&spoke).contains("down and up"));
  wait_for("Connected at the new address", Duration::from_secs(60), &[&spoke, &hub], || {
    healthcheck(&spoke).status.success()
  });

  // --- the hub forgets the peer: Disconnected, the ask, hold, ok, exit 75.
  exec_ok(&spoke, &["touch", "/app/persisted_data/hold"]);
  exec_ok(&hub, &["wg", "set", "wg0", "peer", SPOKE_PUBLIC, "remove"]);
  wait_for("a stale tunnel", Duration::from_secs(200), &[&spoke, &hub], || {
    healthcheck(&spoke).status.code() == Some(1)
  });
  wait_for("the app to be asked and to hold", Duration::from_secs(60), &[&spoke], || {
    text(&exec(&spoke, &["cat", "/app/persisted_data/consent.log"])).contains("-> hold")
  });
  let consent_log = text(&exec_ok(&spoke, &["cat", "/app/persisted_data/consent.log"]));
  assert!(consent_log.contains("may-shutdown wireguard-disconnected"), "{consent_log}");
  assert!(!consent_log.contains("signal"), "not signalled while holding: {consent_log}");
  assert!(exec(&spoke, &["true"]).status.success(), "still running");
  exec_ok(&spoke, &["rm", "/app/persisted_data/hold"]);
  wait_for("the app to answer ok", Duration::from_secs(120), &[&spoke], || {
    text(&exec(&spoke, &["cat", "/app/persisted_data/consent.log"])).contains("-> ok")
  });
  assert_eq!(wait_code(&spoke), "75");
  let log = logs(&spoke);
  assert!(log.contains("gave up after") && log.contains("no handshake"), "{log}");
  let consent_log = text(&ok(
    docker(&["run", "--rm", "-v", &mount, "--entrypoint", "cat", &image]).arg("/app/persisted_data/consent.log"),
  ));
  assert!(consent_log.contains("signal SIGINT"), "{consent_log}");
  ok(&mut docker(&["rm", "-f", &spoke]));
  ok(docker(&["run", "--rm", "-v", &mount, "--entrypoint", "rm", &image]).args([
    "-f",
    "/app/persisted_data/consent.log",
    "/app/persisted_data/report.json",
  ]));

  // --- a new spoke boots at v0.2.0; the hub releases v0.3.0 without it: alert, SIGINT, exit 75.
  exec_ok(&hub, &["wg", "set", "wg0", "peer", SPOKE_PUBLIC, "allowed-ips", "10.8.0.21/32"]);
  let last = format!("last-{id}");
  guard.containers.push(last.clone());
  ok(
    docker(&[
      "run",
      "-d",
      "--name",
      &last,
      "--network",
      &net,
      "--cap-add",
      "NET_ADMIN",
      "-v",
      &mount,
    ])
    .args(spoke_env(&token, SPOKE_PRIVATE, &[]))
    .arg(&image),
  );
  wait_for("the new spoke Connected", Duration::from_secs(90), &[&last, &hub], || {
    healthcheck(&last).status.success()
  });
  exec_ok(&hub, &["sh", "-c", "echo v0.3.0 > /tmp/version"]);
  wait_for("the removal", Duration::from_secs(30), &[&last, &hub], || {
    logs(&last).contains("removed from the hub's peer table in v0.3.0")
  });
  assert_eq!(wait_code(&last), "75");
  let consent_log = text(&ok(
    docker(&["run", "--rm", "-v", &mount, "--entrypoint", "cat", &image]).arg("/app/persisted_data/consent.log"),
  ));
  assert!(!consent_log.contains("may-shutdown"), "no ask on a removal: {consent_log}");
  assert!(consent_log.contains("signal SIGINT"), "{consent_log}");
}
