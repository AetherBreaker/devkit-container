//! Wireguard mode: the environment contract (spec 5.1, 8), the command planner every apply and
//! re-apply is made of (5.2, 5.5, 5.6), and the Linux `Interface` that runs those commands.
//! The planner is pure and tested on every platform; the shell-outs are exercised by the smoke
//! tests.

use anyhow::{Context as _, Result, anyhow, bail};

use crate::bundle::Effective;

/// Removed from the app's environment on both the spawn and the exec path (spec 7).
pub const SECRET_VARS: [&str; 3] = ["WG_PRIVATE_KEY", "WG_PEER_PRESHARED_KEY", "WG_HUB_TOKEN"];

/// The six variables of environment mode; any of them beside `WG_HUB_URL` is refused (5.1).
const ENV_MODE_VARS: [&str; 6] = [
  "WG_ADDRESS",
  "WG_PEER_PUBLIC_KEY",
  "WG_PEER_ENDPOINT",
  "WG_PEER_ALLOWED_IPS",
  "WG_PEER_PRESHARED_KEY",
  "WG_PERSISTENT_KEEPALIVE",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
  /// The whole peer configuration from `WG_*` variables (5.1), kept for projects that have not
  /// migrated.
  Env {
    effective: Effective,
    preshared_key: Option<String>,
  },
  /// Fetched from the hub's GitHub release (4.1, 4.2).
  Fetched {
    hub_url: String,
    repo: String,
    token: Option<String>,
  },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
  pub private_key: String,
  pub mode: Mode,
  /// `WG_TOLERATE_DISCONNECTED=1`: a boot that cannot connect runs anyway, and there is no
  /// give-up (5.2 step 5, 6.1).
  pub tolerate: bool,
  pub poll_secs: u64,
  pub stale_secs: u64,
  pub handshake_timeout_secs: u64,
  pub limit_secs: u64,
  pub hold_limit_secs: u64,
  pub version_poll_secs: u64,
}

impl Settings {
  /// The contract of spec 5.1, 5.4 and 8. `get` is the environment, injected for tests. Empty
  /// is unset. A failure names the variable and never echoes a value.
  pub fn from_env(get: &dyn Fn(&str) -> Option<String>) -> Result<Settings> {
    let var = |k: &str| get(k).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    let required = |k: &str| var(k).with_context(|| format!("{k} is not set; the wireguard mode needs it"));
    let number = |k: &str, default: u64, min: u64| -> Result<u64> {
      let n = match var(k) {
        None => default,
        Some(v) => v.parse().map_err(|_| anyhow!("{k} must be a whole number of seconds"))?,
      };
      if n < min {
        bail!("{k} must be at least {min}");
      }
      Ok(n)
    };
    let private_key = required("WG_PRIVATE_KEY")?;
    let mode = match var("WG_HUB_URL") {
      Some(url) => {
        if let Some(k) = ENV_MODE_VARS.iter().find(|k| var(k).is_some()) {
          bail!("WG_HUB_URL is set together with {k}; fetched mode takes no peer variables");
        }
        Mode::Fetched {
          hub_url: validate_hub_url(&url)?,
          repo: validate_repo(&required("WG_HUB_REPO")?)?,
          token: var("WG_HUB_TOKEN"),
        }
      }
      None => {
        let keepalive: u32 = match var("WG_PERSISTENT_KEEPALIVE") {
          None => 25,
          Some(v) => v
            .parse()
            .map_err(|_| anyhow!("WG_PERSISTENT_KEEPALIVE must be a whole number of seconds"))?,
        };
        if keepalive == 0 {
          bail!(
            "WG_PERSISTENT_KEEPALIVE must be nonzero: without a keepalive an idle tunnel never re-handshakes and would read as stale"
          );
        }
        Mode::Env {
          effective: Effective {
            address: required("WG_ADDRESS")?,
            hub_public_key: required("WG_PEER_PUBLIC_KEY")?,
            endpoint: required("WG_PEER_ENDPOINT")?,
            allowed_ips: required("WG_PEER_ALLOWED_IPS")?
              .split(',')
              .map(|s| s.trim().to_string())
              .filter(|s| !s.is_empty())
              .collect(),
            keepalive,
          },
          preshared_key: var("WG_PEER_PRESHARED_KEY"),
        }
      }
    };
    let tolerate = match var("WG_TOLERATE_DISCONNECTED").as_deref() {
      None => false,
      Some("1") => true,
      Some(_) => bail!("WG_TOLERATE_DISCONNECTED must be unset, empty or 1"),
    };
    let stale_secs: u64 = match var("WG_STALE_SECS") {
      None => 180,
      Some(v) => v.parse().map_err(|_| anyhow!("WG_STALE_SECS must be a whole number of seconds"))?,
    };
    if stale_secs < 150 {
      bail!("WG_STALE_SECS must be at least 150: WireGuard renews the handshake only every 120 s");
    }
    Ok(Settings {
      private_key,
      mode,
      tolerate,
      poll_secs: number("WG_POLL_SECS", 30, 1)?,
      stale_secs,
      handshake_timeout_secs: number("WG_HANDSHAKE_TIMEOUT_SECS", 60, 1)?,
      limit_secs: number("WG_DISCONNECTED_LIMIT_SECS", 1800, 1)?,
      hold_limit_secs: number("WG_HOLD_LIMIT_SECS", 0, 0)?,
      version_poll_secs: number("WG_VERSION_POLL_SECS", 300, 1)?,
    })
  }
}

/// `http://host[:port]` or `https://host[:port]`, one trailing slash stripped, nothing else
/// after the host (spec 4.1).
pub fn validate_hub_url(s: &str) -> Result<String> {
  let bad = |why: &str| anyhow!("WG_HUB_URL must be http://host[:port] or https://host[:port] with no path ({why})");
  let (scheme, rest) = ["https://", "http://"]
    .into_iter()
    .find_map(|p| s.strip_prefix(p).map(|rest| (p, rest)))
    .ok_or_else(|| bad("the scheme is not http or https"))?;
  let authority = rest.strip_suffix('/').unwrap_or(rest);
  if authority.is_empty() {
    return Err(bad("no host"));
  }
  if authority.contains(['/', '?', '#']) || authority.contains(char::is_whitespace) {
    return Err(bad("a path, query or fragment follows the host"));
  }
  Ok(format!("{scheme}{authority}"))
}

/// `owner/repo`, both parts non-empty, characters `[A-Za-z0-9._-]` (spec 8).
pub fn validate_repo(s: &str) -> Result<String> {
  let ok = s.split('/').count() == 2
    && s
      .split('/')
      .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')));
  if !ok {
    bail!("WG_HUB_REPO must be owner/repo");
  }
  Ok(s.to_string())
}

pub const IFACE: &str = "wg0";

/// One shell-out of an apply. `Endpoint` failures are Disconnected, `endpoint unresolvable`
/// (the one command that resolves a name); everything else failing is Broken (spec 5.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cmd {
  pub program: &'static str,
  pub args: Vec<String>,
  pub kind: CmdKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmdKind {
  Local,
  Endpoint,
}

fn cmd(program: &'static str, args: &[&str], kind: CmdKind) -> Cmd {
  Cmd {
    program,
    args: args.iter().map(|a| a.to_string()).collect(),
    kind,
  }
}

/// The apply of 5.2 step 3: peer, address, link up, routes, and the endpoint last. The keepalive
/// goes with the endpoint: WireGuard fires its first handshake when the keepalive is set, so set
/// before the endpoint exists that attempt is lost and only the 5 s retry can succeed (measured).
pub fn apply_commands(eff: &Effective) -> Vec<Cmd> {
  let mut v = vec![
    cmd(
      "wg",
      &["set", IFACE, "peer", &eff.hub_public_key, "allowed-ips", &eff.allowed_ips.join(",")],
      CmdKind::Local,
    ),
    cmd("ip", &["address", "add", &eff.address, "dev", IFACE], CmdKind::Local),
    cmd("ip", &["link", "set", "up", "dev", IFACE], CmdKind::Local),
  ];
  for cidr in &eff.allowed_ips {
    // `replace`, not `add`: the kernel may already have added the interface's own subnet.
    v.push(cmd("ip", &["route", "replace", cidr, "dev", IFACE], CmdKind::Local));
  }
  v.push(endpoint_command(eff));
  v
}

pub fn endpoint_command(eff: &Effective) -> Cmd {
  cmd(
    "wg",
    &[
      "set",
      IFACE,
      "peer",
      &eff.hub_public_key,
      "endpoint",
      &eff.endpoint,
      "persistent-keepalive",
      &eff.keepalive.to_string(),
    ],
    CmdKind::Endpoint,
  )
}

/// The in-place re-apply of 5.5: one row per changed field, in the table's order, the endpoint
/// last. `endpoint_ok` false re-sets the endpoint even when it did not change, so a name that
/// failed to resolve is retried by the same apply.
pub fn reapply_commands(old: &Effective, new: &Effective, endpoint_ok: bool) -> Vec<Cmd> {
  use std::collections::BTreeSet;
  let mut v = Vec::new();
  let key_changed = old.hub_public_key != new.hub_public_key;
  let old_ips: BTreeSet<&str> = old.allowed_ips.iter().map(String::as_str).collect();
  let new_ips: BTreeSet<&str> = new.allowed_ips.iter().map(String::as_str).collect();
  if key_changed {
    v.push(cmd("wg", &["set", IFACE, "peer", &old.hub_public_key, "remove"], CmdKind::Local));
    v.push(cmd(
      "wg",
      &["set", IFACE, "peer", &new.hub_public_key, "allowed-ips", &new.allowed_ips.join(",")],
      CmdKind::Local,
    ));
  } else if old_ips != new_ips || old.keepalive != new.keepalive {
    let mut args: Vec<String> = vec!["set".into(), IFACE.into(), "peer".into(), new.hub_public_key.clone()];
    if old_ips != new_ips {
      args.push("allowed-ips".into());
      args.push(new.allowed_ips.join(","));
    }
    if old.keepalive != new.keepalive {
      args.push("persistent-keepalive".into());
      args.push(new.keepalive.to_string());
    }
    v.push(Cmd {
      program: "wg",
      args,
      kind: CmdKind::Local,
    });
  }
  if old.address != new.address {
    v.push(cmd("ip", &["address", "replace", &new.address, "dev", IFACE], CmdKind::Local));
    v.push(cmd("ip", &["address", "delete", &old.address, "dev", IFACE], CmdKind::Local));
  }
  for cidr in new_ips.difference(&old_ips) {
    v.push(cmd("ip", &["route", "replace", cidr, "dev", IFACE], CmdKind::Local));
  }
  for cidr in old_ips.difference(&new_ips) {
    v.push(cmd("ip", &["route", "delete", cidr, "dev", IFACE], CmdKind::Local));
  }
  if key_changed || old.endpoint != new.endpoint || !endpoint_ok {
    v.push(endpoint_command(new));
  }
  v
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
pub use unix::{ApplyError, Interface};

#[cfg(unix)]
mod unix {
  use std::io::Write as _;
  use std::process::{Command, Stdio};

  use anyhow::{Context as _, Result, bail};

  use super::parse_latest_handshake;

  use super::{Cmd, CmdKind, IFACE};
  use crate::bundle::Effective;

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

  /// A failed apply command, classified per spec 5.3.
  #[derive(Debug)]
  pub enum ApplyError {
    /// A local command failed: Broken.
    Local(anyhow::Error),
    /// The endpoint command failed, the one that resolves a name: `endpoint unresolvable`.
    Endpoint(anyhow::Error),
  }

  /// `wg0`, created and holding the private key. Dropping it does not tear it down: the
  /// supervisor decides when.
  pub struct Interface {
    pub public_key: String,
    private_key: String,
    preshared_key: Option<String>,
  }

  impl Interface {
    /// Preflight, `ip link add`, the private key over stdin, the public key derived (spec 5.2
    /// step 1). A failure leaves no interface behind.
    pub fn create(private_key: &str, preshared_key: Option<String>) -> Result<Interface> {
      for tool in ["wg", "ip"] {
        if Command::new(tool).arg("--version").output().is_err() && Command::new(tool).arg("-V").output().is_err() {
          bail!(
            "{tool} is not in the image: it was built without the wireguard block; rerun setup-project with a devkit that knows the `wireguard` switch and rebuild"
          );
        }
      }
      let public_key = run("wg", &["pubkey"], Some(private_key))?.trim().to_string();
      let iface = Interface {
        public_key,
        private_key: private_key.to_string(),
        preshared_key,
      };
      if let Err(e) = iface.link_up_with_key() {
        iface.down();
        return Err(e);
      }
      Ok(iface)
    }

    fn link_up_with_key(&self) -> Result<()> {
      run("ip", &["link", "add", "dev", IFACE, "type", "wireguard"], None)?;
      run("wg", &["set", IFACE, "private-key", "/dev/stdin"], Some(&self.private_key))?;
      Ok(())
    }

    fn exec(&self, c: &Cmd) -> Result<(), ApplyError> {
      let args: Vec<&str> = c.args.iter().map(String::as_str).collect();
      match run(c.program, &args, None) {
        Ok(_) => Ok(()),
        Err(e) => Err(match c.kind {
          CmdKind::Local => ApplyError::Local(e),
          CmdKind::Endpoint => ApplyError::Endpoint(e),
        }),
      }
    }

    /// The apply of 5.2 step 3; the preshared key, when there is one, goes over stdin right
    /// after the peer command.
    pub fn apply(&self, eff: &Effective) -> Result<(), ApplyError> {
      for (i, c) in super::apply_commands(eff).iter().enumerate() {
        self.exec(c)?;
        if i == 0
          && let Some(psk) = &self.preshared_key
        {
          run(
            "wg",
            &["set", IFACE, "peer", &eff.hub_public_key, "preshared-key", "/dev/stdin"],
            Some(psk),
          )
          .map_err(ApplyError::Local)?;
        }
      }
      Ok(())
    }

    pub fn set_endpoint(&self, eff: &Effective) -> Result<(), ApplyError> {
      self.exec(&super::endpoint_command(eff))
    }

    pub fn reapply(&self, old: &Effective, new: &Effective, endpoint_ok: bool) -> Result<(), ApplyError> {
      for c in super::reapply_commands(old, new, endpoint_ok) {
        self.exec(&c)?;
      }
      Ok(())
    }

    /// Down and up again with `eff` (5.6): the interface deleted, created, keyed, applied.
    pub fn down_up(&self, eff: &Effective) -> Result<(), ApplyError> {
      self.down();
      self.link_up_with_key().map_err(ApplyError::Local)?;
      self.apply(eff)
    }

    pub fn latest_handshake(&self, peer_key: &str) -> Result<Option<u64>> {
      let out = run("wg", &["show", IFACE, "latest-handshakes"], None)?;
      Ok(parse_latest_handshake(&out, peer_key))
    }

    /// Poll every 500 ms for up to `timeout`; `Ok(false)` is no handshake in time (5.2 step 4).
    pub fn wait_handshake(&self, peer_key: &str, timeout: std::time::Duration) -> Result<bool> {
      let deadline = std::time::Instant::now() + timeout;
      loop {
        if self.latest_handshake(peer_key)?.is_some() {
          return Ok(true);
        }
        if std::time::Instant::now() >= deadline {
          return Ok(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
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
  fn the_latest_handshake_is_read_from_wg_show() {
    let out = "otherpub=\t1757505000\npubkey=\t1757505600\n";
    assert_eq!(parse_latest_handshake(out, "pubkey="), Some(1757505600));
    assert_eq!(parse_latest_handshake("pubkey=\t0\n", "pubkey="), None, "0 means never");
    assert_eq!(parse_latest_handshake(out, "missing"), None);
  }
  const FETCHED: [(&str, &str); 3] = [
    ("WG_PRIVATE_KEY", "priv"),
    ("WG_HUB_URL", "https://tunnels.example.com/"),
    ("WG_HUB_REPO", "AetherBreaker/wireguard-hub"),
  ];

  #[test]
  fn fetched_mode_reads_the_hub_variables_and_the_defaults() {
    let mut e = env(&FETCHED);
    e.insert("WG_HUB_TOKEN".into(), "tok".into());
    let s = Settings::from_env(&|k| e.get(k).cloned()).unwrap();
    assert_eq!(
      s.mode,
      Mode::Fetched {
        hub_url: "https://tunnels.example.com".into(),
        repo: "AetherBreaker/wireguard-hub".into(),
        token: Some("tok".into())
      }
    );
    assert!(!s.tolerate);
    assert_eq!(
      (
        s.poll_secs,
        s.stale_secs,
        s.handshake_timeout_secs,
        s.limit_secs,
        s.hold_limit_secs,
        s.version_poll_secs
      ),
      (30, 180, 60, 1800, 0, 300)
    );
    let e = env(&FETCHED);
    let s = Settings::from_env(&|k| e.get(k).cloned()).unwrap();
    assert!(
      matches!(s.mode, Mode::Fetched { token: None, .. }),
      "the token is optional to the binary"
    );
  }

  #[test]
  fn environment_mode_builds_the_effective_configuration() {
    let mut e = env(&REQUIRED);
    e.insert("WG_PEER_PRESHARED_KEY".into(), "psk".into());
    e.insert("WG_PERSISTENT_KEEPALIVE".into(), "10".into());
    let s = Settings::from_env(&|k| e.get(k).cloned()).unwrap();
    let Mode::Env { effective, preshared_key } = s.mode else {
      panic!("environment mode")
    };
    assert_eq!(effective.address, "10.8.0.20/32");
    assert_eq!(effective.hub_public_key, "pub");
    assert_eq!(effective.endpoint, "hub:51820");
    assert_eq!(effective.allowed_ips, ["10.8.0.0/24", "192.168.1.0/24"]);
    assert_eq!(effective.keepalive, 10);
    assert_eq!(preshared_key.as_deref(), Some("psk"));
  }

  #[test]
  fn fetched_mode_refuses_every_peer_variable_by_name() {
    for (k, v) in [
      ("WG_ADDRESS", "10.8.0.20/32"),
      ("WG_PEER_PUBLIC_KEY", "pub"),
      ("WG_PEER_ENDPOINT", "hub:51820"),
      ("WG_PEER_ALLOWED_IPS", "10.8.0.0/24"),
      ("WG_PEER_PRESHARED_KEY", "psk"),
      ("WG_PERSISTENT_KEEPALIVE", "25"),
    ] {
      let mut e = env(&FETCHED);
      e.insert(k.into(), v.into());
      let err = Settings::from_env(&|k| e.get(k).cloned()).unwrap_err().to_string();
      assert!(err.contains("WG_HUB_URL") && err.contains(k), "{k}: {err}");
      assert!(!err.contains("psk") && !err.contains("priv"), "{err}");
    }
    let mut e = env(&FETCHED);
    e.remove("WG_HUB_REPO");
    assert!(
      Settings::from_env(&|k| e.get(k).cloned())
        .unwrap_err()
        .to_string()
        .contains("WG_HUB_REPO")
    );
  }

  #[test]
  fn the_timers_are_validated_with_their_floors() {
    for (k, v, needle) in [
      ("WG_STALE_SECS", "149", "at least 150"),
      ("WG_STALE_SECS", "x", "whole number"),
      ("WG_POLL_SECS", "0", "at least 1"),
      ("WG_HANDSHAKE_TIMEOUT_SECS", "0", "at least 1"),
      ("WG_DISCONNECTED_LIMIT_SECS", "0", "at least 1"),
      ("WG_VERSION_POLL_SECS", "0", "at least 1"),
      ("WG_HOLD_LIMIT_SECS", "-1", "whole number"),
      ("WG_TOLERATE_DISCONNECTED", "yes", "unset, empty or 1"),
    ] {
      let mut e = env(&FETCHED);
      e.insert(k.into(), v.into());
      let err = Settings::from_env(&|k| e.get(k).cloned()).unwrap_err().to_string();
      assert!(err.contains(k) && err.contains(needle), "{k}={v}: {err}");
    }
    let mut e = env(&FETCHED);
    e.insert("WG_HOLD_LIMIT_SECS".into(), "0".into());
    e.insert("WG_STALE_SECS".into(), "150".into());
    e.insert("WG_TOLERATE_DISCONNECTED".into(), "1".into());
    let s = Settings::from_env(&|k| e.get(k).cloned()).unwrap();
    assert!(s.tolerate && s.hold_limit_secs == 0 && s.stale_secs == 150);
    e.insert("WG_TOLERATE_DISCONNECTED".into(), "".into());
    assert!(!Settings::from_env(&|k| e.get(k).cloned()).unwrap().tolerate, "empty is unset");
  }

  #[test]
  fn the_hub_url_is_a_scheme_and_a_host_with_no_path() {
    assert_eq!(
      validate_hub_url("https://tunnels.example.com/").unwrap(),
      "https://tunnels.example.com"
    );
    assert_eq!(validate_hub_url("http://wireguard-hub:8000").unwrap(), "http://wireguard-hub:8000");
    for bad in [
      "tunnels.example.com",
      "ftp://x",
      "https://tunnels.example.com/version",
      "https://tunnels.example.com//",
      "https://x?y",
      "https://x#y",
      "https://",
    ] {
      assert!(validate_hub_url(bad).is_err(), "{bad:?} must be refused");
    }
    assert_eq!(validate_repo("AetherBreaker/wireguard-hub").unwrap(), "AetherBreaker/wireguard-hub");
    for bad in ["wireguard-hub", "a/b/c", "/b", "a/", "a b/c", "https://github.com/a/b"] {
      assert!(validate_repo(bad).is_err(), "{bad:?} must be refused");
    }
  }
  fn eff() -> Effective {
    Effective {
      address: "10.8.0.20/32".into(),
      hub_public_key: "HUBKEY".into(),
      endpoint: "tunnels.example.com:51820".into(),
      allowed_ips: vec!["10.8.0.0/24".into()],
      keepalive: 25,
    }
  }

  fn lines(cmds: &[Cmd]) -> Vec<String> {
    cmds.iter().map(|c| format!("{} {}", c.program, c.args.join(" "))).collect()
  }

  #[test]
  fn the_apply_runs_the_endpoint_last() {
    let cmds = apply_commands(&eff());
    assert_eq!(
      lines(&cmds),
      [
        "wg set wg0 peer HUBKEY allowed-ips 10.8.0.0/24",
        "ip address add 10.8.0.20/32 dev wg0",
        "ip link set up dev wg0",
        "ip route replace 10.8.0.0/24 dev wg0",
        "wg set wg0 peer HUBKEY endpoint tunnels.example.com:51820 persistent-keepalive 25",
      ]
    );
    assert!(cmds[..4].iter().all(|c| c.kind == CmdKind::Local));
    assert_eq!(cmds[4].kind, CmdKind::Endpoint);
  }

  #[test]
  fn the_reapply_emits_one_row_per_changed_field_in_order_endpoint_last() {
    let old = eff();
    assert!(reapply_commands(&old, &old, true).is_empty(), "nothing changed");
    let mut key = old.clone();
    key.hub_public_key = "NEWKEY".into();
    assert_eq!(
      lines(&reapply_commands(&old, &key, true)),
      [
        "wg set wg0 peer HUBKEY remove",
        "wg set wg0 peer NEWKEY allowed-ips 10.8.0.0/24",
        "wg set wg0 peer NEWKEY endpoint tunnels.example.com:51820 persistent-keepalive 25",
      ]
    );
    let mut ips = old.clone();
    ips.allowed_ips = vec!["10.8.0.0/24".into(), "10.9.0.0/24".into()];
    assert_eq!(
      lines(&reapply_commands(&old, &ips, true)),
      [
        "wg set wg0 peer HUBKEY allowed-ips 10.8.0.0/24,10.9.0.0/24",
        "ip route replace 10.9.0.0/24 dev wg0",
      ]
    );
    let mut fewer = ips.clone();
    fewer.allowed_ips = vec!["10.9.0.0/24".into()];
    assert_eq!(
      lines(&reapply_commands(&ips, &fewer, true)),
      [
        "wg set wg0 peer HUBKEY allowed-ips 10.9.0.0/24",
        "ip route delete 10.8.0.0/24 dev wg0"
      ],
      "a kept CIDR is not re-added"
    );
    let mut keepalive = old.clone();
    keepalive.keepalive = 15;
    assert_eq!(
      lines(&reapply_commands(&old, &keepalive, true)),
      ["wg set wg0 peer HUBKEY persistent-keepalive 15"]
    );
    let mut address = old.clone();
    address.address = "10.8.0.21/32".into();
    assert_eq!(
      lines(&reapply_commands(&old, &address, true)),
      ["ip address replace 10.8.0.21/32 dev wg0", "ip address delete 10.8.0.20/32 dev wg0"]
    );
    let mut endpoint = old.clone();
    endpoint.endpoint = "other.example.com:51820".into();
    let cmds = reapply_commands(&old, &endpoint, true);
    assert_eq!(
      lines(&cmds),
      ["wg set wg0 peer HUBKEY endpoint other.example.com:51820 persistent-keepalive 25"]
    );
    assert_eq!(cmds[0].kind, CmdKind::Endpoint);
    assert_eq!(
      lines(&reapply_commands(&old, &old, false)),
      ["wg set wg0 peer HUBKEY endpoint tunnels.example.com:51820 persistent-keepalive 25"],
      "an unresolved endpoint is retried even when unchanged"
    );
    let mut everything = key.clone();
    everything.address = "10.8.0.21/32".into();
    everything.allowed_ips = vec!["10.9.0.0/24".into()];
    everything.keepalive = 15;
    everything.endpoint = "other.example.com:51820".into();
    assert_eq!(
      lines(&reapply_commands(&old, &everything, true)),
      [
        "wg set wg0 peer HUBKEY remove",
        "wg set wg0 peer NEWKEY allowed-ips 10.9.0.0/24",
        "ip address replace 10.8.0.21/32 dev wg0",
        "ip address delete 10.8.0.20/32 dev wg0",
        "ip route replace 10.9.0.0/24 dev wg0",
        "ip route delete 10.8.0.0/24 dev wg0",
        "wg set wg0 peer NEWKEY endpoint other.example.com:51820 persistent-keepalive 15",
      ]
    );
  }
}
