//! Wireguard mode (spec 6): the environment contract, `ip` + `wg set` bring-up with the keys
//! over stdin, the first-handshake wait, and the stale-and-re-up rule. The rule is a pure
//! state machine (`Assessor`) so it is tested with an injected clock; the shell-outs are
//! `Tunnel`, exercised by the smoke test.

use anyhow::{Context as _, Result, bail};

pub const SECRET_VARS: [&str; 2] = ["WG_PRIVATE_KEY", "WG_PEER_PRESHARED_KEY"];

#[derive(Debug, Clone)]
pub struct Config {
  pub private_key: String,
  pub address: String,
  pub peer_public_key: String,
  pub peer_endpoint: String,
  pub peer_allowed_ips: Vec<String>,
  pub peer_preshared_key: Option<String>,
  pub keepalive: u32,
  pub handshake_timeout_secs: u64,
  pub poll_secs: u64,
  pub stale_secs: u64,
}

impl Config {
  /// The `WG_*` contract (spec 6). `get` is the environment, injected for tests. Empty is
  /// unset. A failure names the variable and never echoes a value.
  pub fn from_env(get: &dyn Fn(&str) -> Option<String>) -> Result<Config> {
    let var = |k: &str| get(k).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    let required = |k: &str| var(k).with_context(|| format!("{k} is not set; the wireguard mode needs it"));
    let number = |k: &str, default: u64| -> Result<u64> {
      match var(k) {
        None => Ok(default),
        Some(v) => v.parse().map_err(|_| anyhow::anyhow!("{k} must be a whole number of seconds")),
      }
    };
    let keepalive: u32 = match var("WG_PERSISTENT_KEEPALIVE") {
      None => 25,
      Some(v) => v
        .parse()
        .map_err(|_| anyhow::anyhow!("WG_PERSISTENT_KEEPALIVE must be a whole number of seconds"))?,
    };
    if keepalive == 0 {
      bail!("WG_PERSISTENT_KEEPALIVE must be nonzero: without a keepalive an idle tunnel never re-handshakes and would read as stale");
    }
    Ok(Config {
      private_key: required("WG_PRIVATE_KEY")?,
      address: required("WG_ADDRESS")?,
      peer_public_key: required("WG_PEER_PUBLIC_KEY")?,
      peer_endpoint: required("WG_PEER_ENDPOINT")?,
      peer_allowed_ips: required("WG_PEER_ALLOWED_IPS")?
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect(),
      peer_preshared_key: var("WG_PEER_PRESHARED_KEY"),
      keepalive,
      handshake_timeout_secs: number("WG_HANDSHAKE_TIMEOUT_SECS", 60)?,
      poll_secs: number("WG_POLL_SECS", 30)?,
      stale_secs: number("WG_STALE_SECS", 180)?,
    })
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
  Nothing,
  /// Re-set the peer endpoint so its name is resolved again (the hub may have moved).
  ResetEndpoint,
  /// The interface down and up again: the second and every later response to a stale poll.
  DownUp,
}

/// The stale rule: a handshake older than `stale_secs` (or none) is stale; the first stale
/// poll resets the endpoint, every following one cycles the interface, and a fresh poll
/// starts the sequence over.
#[derive(Debug)]
pub struct Assessor {
  stale_secs: u64,
  stale_polls: u32,
}

impl Assessor {
  pub fn new(stale_secs: u64) -> Assessor {
    Assessor {
      stale_secs,
      stale_polls: 0,
    }
  }

  /// `(fresh, what to do)`.
  pub fn assess(&mut self, now: u64, latest_handshake: Option<u64>) -> (bool, Action) {
    let fresh = latest_handshake.is_some_and(|h| now.saturating_sub(h) < self.stale_secs);
    if fresh {
      self.stale_polls = 0;
      return (true, Action::Nothing);
    }
    self.stale_polls += 1;
    (
      false,
      if self.stale_polls == 1 {
        Action::ResetEndpoint
      } else {
        Action::DownUp
      },
    )
  }
}

/// `wg show wg0 latest-handshakes` prints `<peer public key>\t<epoch seconds>`; 0 is never.
pub fn parse_latest_handshake(wg_show: &str, peer_public_key: &str) -> Option<u64> {
  wg_show
    .lines()
    .filter_map(|l| l.split_once('\t'))
    .find(|(k, _)| k.trim() == peer_public_key)
    .and_then(|(_, v)| v.trim().parse::<u64>().ok())
    .filter(|&epoch| epoch != 0)
}

#[cfg(unix)]
pub use unix::Tunnel;

#[cfg(unix)]
mod unix {
  use std::io::Write as _;
  use std::process::{Command, Stdio};

  use anyhow::{Context as _, Result, bail};

  use super::{Action, Config, parse_latest_handshake};

  pub const IFACE: &str = "wg0";

  /// The interface, up. Dropping it does not tear it down: the supervisor decides when.
  pub struct Tunnel {
    cfg: Config,
  }

  /// Run a command, feeding `stdin` if given; an error names the command and its stderr.
  fn run(program: &str, args: &[&str], stdin: Option<&str>) -> Result<String> {
    let mut cmd = Command::new(program);
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() });
    let mut child = cmd.spawn().with_context(|| format!("running {program} {}", args.join(" ")))?;
    if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
      // Written and closed before waiting, or `wg` blocks reading a key that never ends.
      pipe.write_all(text.as_bytes()).context("writing to stdin")?;
      pipe.write_all(b"\n").context("writing to stdin")?;
    }
    let out = child.wait_with_output().with_context(|| format!("waiting for {program}"))?;
    if !out.status.success() {
      bail!(
        "{program} {} failed ({}): {}",
        args.join(" "),
        out.status,
        String::from_utf8_lossy(&out.stderr).trim()
      );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
  }

  impl Tunnel {
    /// Preflight, bring-up, the public key logged, the first handshake within the timeout
    /// (spec 6, steps 1 to 3). A refused start leaves no interface behind.
    pub fn start(cfg: Config) -> Result<Tunnel> {
      for tool in ["wg", "ip"] {
        if Command::new(tool).arg("--version").output().is_err() && Command::new(tool).arg("-V").output().is_err() {
          bail!(
            "{tool} is not in the image: it was built without the wireguard block; rerun setup-project with a devkit that knows the `wireguard` switch and rebuild"
          );
        }
      }
      let public = run("wg", &["pubkey"], Some(&cfg.private_key))?;
      eprintln!("devkit-container: wireguard public key {}", public.trim());
      let tunnel = Tunnel { cfg };
      if let Err(e) = tunnel.up() {
        tunnel.down();
        return Err(e);
      }
      let deadline = std::time::Instant::now() + std::time::Duration::from_secs(tunnel.cfg.handshake_timeout_secs);
      loop {
        if tunnel.latest_handshake()?.is_some() {
          eprintln!("devkit-container: wireguard handshake with {}", tunnel.cfg.peer_endpoint);
          return Ok(tunnel);
        }
        if std::time::Instant::now() >= deadline {
          tunnel.down();
          bail!(
            "no wireguard handshake with {} within {} s (WG_HANDSHAKE_TIMEOUT_SECS)",
            tunnel.cfg.peer_endpoint,
            tunnel.cfg.handshake_timeout_secs
          );
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
      }
    }

    fn up(&self) -> Result<()> {
      let c = &self.cfg;
      run("ip", &["link", "add", "dev", IFACE, "type", "wireguard"], None)?;
      run("wg", &["set", IFACE, "private-key", "/dev/stdin"], Some(&c.private_key))?;
      let keepalive = c.keepalive.to_string();
      let allowed = c.peer_allowed_ips.join(",");
      run(
        "wg",
        &[
          "set",
          IFACE,
          "peer",
          &c.peer_public_key,
          "endpoint",
          &c.peer_endpoint,
          "allowed-ips",
          &allowed,
          "persistent-keepalive",
          &keepalive,
        ],
        None,
      )?;
      if let Some(psk) = &c.peer_preshared_key {
        run(
          "wg",
          &["set", IFACE, "peer", &c.peer_public_key, "preshared-key", "/dev/stdin"],
          Some(psk),
        )?;
      }
      run("ip", &["address", "add", &c.address, "dev", IFACE], None)?;
      run("ip", &["link", "set", "up", "dev", IFACE], None)?;
      for cidr in &c.peer_allowed_ips {
        // `replace`, not `add`: the kernel may already have added the interface's own subnet.
        run("ip", &["route", "replace", cidr, "dev", IFACE], None)?;
      }
      Ok(())
    }

    pub fn latest_handshake(&self) -> Result<Option<u64>> {
      let out = run("wg", &["show", IFACE, "latest-handshakes"], None)?;
      Ok(parse_latest_handshake(&out, &self.cfg.peer_public_key))
    }

    pub fn act(&self, action: Action) -> Result<()> {
      match action {
        Action::Nothing => Ok(()),
        Action::ResetEndpoint => {
          eprintln!(
            "devkit-container: wireguard stale; re-up: resetting the endpoint {}",
            self.cfg.peer_endpoint
          );
          run(
            "wg",
            &["set", IFACE, "peer", &self.cfg.peer_public_key, "endpoint", &self.cfg.peer_endpoint],
            None,
          )
          .map(|_| ())
        }
        Action::DownUp => {
          eprintln!("devkit-container: wireguard still stale; re-up: {IFACE} down and up");
          self.down();
          self.up()
        }
      }
    }

    /// Best effort: an interface that is already gone is not an error.
    pub fn down(&self) {
      let _ = Command::new("ip").args(["link", "del", "dev", IFACE]).output();
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::collections::HashMap;

  fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
  }

  const REQUIRED: [(&str, &str); 5] = [
    ("WG_PRIVATE_KEY", "priv"),
    ("WG_ADDRESS", "10.8.0.20/32"),
    ("WG_PEER_PUBLIC_KEY", "pub"),
    ("WG_PEER_ENDPOINT", "hub:51820"),
    ("WG_PEER_ALLOWED_IPS", "10.8.0.0/24, 192.168.1.0/24"),
  ];

  #[test]
  fn the_environment_contract_with_its_defaults() {
    let e = env(&REQUIRED);
    let c = Config::from_env(&|k| e.get(k).cloned()).unwrap();
    assert_eq!(c.peer_allowed_ips, ["10.8.0.0/24", "192.168.1.0/24"]);
    assert_eq!(
      (c.keepalive, c.handshake_timeout_secs, c.poll_secs, c.stale_secs),
      (25, 60, 30, 180)
    );
    assert!(c.peer_preshared_key.is_none());
    let mut e = env(&REQUIRED);
    e.insert("WG_PEER_PRESHARED_KEY".into(), "psk".into());
    e.insert("WG_PERSISTENT_KEEPALIVE".into(), "10".into());
    e.insert("WG_POLL_SECS".into(), "1".into());
    e.insert("WG_STALE_SECS".into(), "".into()); // empty is unset
    let c = Config::from_env(&|k| e.get(k).cloned()).unwrap();
    assert_eq!(c.peer_preshared_key.as_deref(), Some("psk"));
    assert_eq!((c.keepalive, c.poll_secs, c.stale_secs), (10, 1, 180));
  }

  #[test]
  fn a_missing_or_bad_variable_is_named_never_its_value() {
    for (missing, _) in REQUIRED {
      let e: HashMap<String, String> = REQUIRED
        .iter()
        .filter(|(k, _)| *k != missing)
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
      let err = Config::from_env(&|k| e.get(k).cloned()).unwrap_err().to_string();
      assert!(err.contains(missing), "{missing}: {err}");
      assert!(!err.contains("priv"), "{err}");
    }
    let mut e = env(&REQUIRED);
    e.insert("WG_PERSISTENT_KEEPALIVE".into(), "0".into());
    let err = Config::from_env(&|k| e.get(k).cloned()).unwrap_err().to_string();
    assert!(err.contains("WG_PERSISTENT_KEEPALIVE") && err.contains("nonzero"), "{err}");
    e.insert("WG_PERSISTENT_KEEPALIVE".into(), "soon".into());
    let err = Config::from_env(&|k| e.get(k).cloned()).unwrap_err().to_string();
    assert!(err.contains("WG_PERSISTENT_KEEPALIVE") && !err.contains("soon"), "{err}");
  }

  #[test]
  fn stale_resets_the_endpoint_first_then_cycles_the_interface() {
    let mut a = Assessor::new(180);
    assert_eq!(a.assess(1000, Some(900)), (true, Action::Nothing));
    assert_eq!(a.assess(1179, Some(1000)), (true, Action::Nothing), "179 s old is fresh");
    assert_eq!(a.assess(1180, Some(1000)), (false, Action::ResetEndpoint), "180 s old is stale");
    assert_eq!(a.assess(1210, Some(1000)), (false, Action::DownUp));
    assert_eq!(a.assess(1240, Some(1000)), (false, Action::DownUp));
    assert_eq!(a.assess(1270, Some(1265)), (true, Action::Nothing), "recovered");
    assert_eq!(
      a.assess(1500, Some(1265)),
      (false, Action::ResetEndpoint),
      "the next outage starts over"
    );
    assert_eq!(
      Assessor::new(180).assess(5, None),
      (false, Action::ResetEndpoint),
      "never handshaken is stale"
    );
  }

  #[test]
  fn the_latest_handshake_is_read_from_wg_show() {
    let out = "otherpub=\t1757505000\npubkey=\t1757505600\n";
    assert_eq!(parse_latest_handshake(out, "pubkey="), Some(1757505600));
    assert_eq!(parse_latest_handshake("pubkey=\t0\n", "pubkey="), None, "0 means never");
    assert_eq!(parse_latest_handshake(out, "missing"), None);
  }
}
