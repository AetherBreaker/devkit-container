//! The bundle contract (spec 3.2, 4.1, 4.3): the strict tag, the peer table parsed and
//! validated, the spoke's own entry and the effective configuration it yields. Pure: no IO and
//! no network, so it runs and is tested on every platform.
use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr};

use anyhow::{Context as _, Result, anyhow, bail};
use toml_edit::{DocumentMut, Item, TableLike};

/// `^v[0-9]+\.[0-9]+\.[0-9]+$`, checked by hand: no regex crate, and the tag goes into a URL.
pub fn validate_tag(s: &str) -> Result<String> {
  let bad = || anyhow!("version {s:?} is not a v<major>.<minor>.<patch> tag");
  let rest = s.strip_prefix('v').ok_or_else(bad)?;
  let parts: Vec<&str> = rest.split('.').collect();
  if parts.len() != 3 || parts.iter().any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit())) {
    return Err(bad());
  }
  Ok(s.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hub {
  pub name: String,
  pub public_key: String,
  /// The interface address; its network is the tunnel subnet.
  pub address: String,
  pub listen_port: u16,
  pub endpoint: String,
  pub allowed_ips: Vec<String>,
  pub persistent_keepalive: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
  pub name: String,
  pub public_key: String,
  pub address: String,
  pub endpoint: Option<String>,
  pub allowed_ips: Option<Vec<String>>,
  pub persistent_keepalive: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bundle {
  /// Stamped by the hub's release; absent in the committed table.
  pub hub_version: Option<String>,
  pub hub: Hub,
  pub peers: Vec<Peer>,
}

/// Parse and validate a bundle or the committed table (spec 3.2). Every failure names the
/// field, `hub.<key>` or `peers[<index>].<key>`.
pub fn parse(text: &str) -> Result<Bundle> {
  let doc: DocumentMut = text.parse().map_err(|e| anyhow!("the bundle is not valid TOML: {e}"))?;
  match doc.get("schema").and_then(Item::as_integer) {
    Some(1) => {}
    _ => bail!("schema must be 1"),
  }
  let hub_version = match doc.get("hub_version") {
    None => None,
    Some(item) => Some(validate_tag(item.as_str().ok_or_else(|| anyhow!("hub_version must be a string"))?).context("hub_version")?),
  };
  let hub_table = doc
    .get("hub")
    .and_then(Item::as_table_like)
    .ok_or_else(|| anyhow!("[hub] is missing"))?;
  let hub = Hub {
    name: name(required_str(hub_table, "hub", "name")?, "hub.name")?,
    public_key: key(required_str(hub_table, "hub", "public_key")?, "hub.public_key")?,
    address: hub_address(required_str(hub_table, "hub", "address")?)?,
    listen_port: port(required_int(hub_table, "hub", "listen_port")?, "hub.listen_port")?,
    endpoint: endpoint(required_str(hub_table, "hub", "endpoint")?, "hub.endpoint")?,
    allowed_ips: allowed_ips(hub_table, "hub")?.ok_or_else(|| anyhow!("hub.allowed_ips is missing"))?,
    persistent_keepalive: keepalive(hub_table, "hub")?.ok_or_else(|| anyhow!("hub.persistent_keepalive is missing"))?,
  };
  let (hub_ip, hub_prefix) = v4_cidr(&hub.address).expect("validated above");
  let mut peers = Vec::new();
  if let Some(array) = doc.get("peers") {
    let tables = array
      .as_array_of_tables()
      .ok_or_else(|| anyhow!("peers must be an array of tables"))?;
    for (i, t) in tables.iter().enumerate() {
      let at = format!("peers[{i}]");
      let t: &dyn TableLike = t;
      let address = required_str(t, &at, "address")?;
      let field = format!("{at}.address");
      let (ip, prefix) = v4_cidr(address).ok_or_else(|| anyhow!("{field} must be an IPv4 CIDR, got {address:?}"))?;
      if prefix != 32 {
        bail!("{field} must be a /32, got {address:?}");
      }
      if v4_network(ip, hub_prefix) != v4_network(hub_ip, hub_prefix) {
        bail!("{field} {address:?} is outside the hub's network {}", hub.address);
      }
      if ip == hub_ip {
        bail!("{field} {address:?} is the hub's own address");
      }
      peers.push(Peer {
        name: name(required_str(t, &at, "name")?, &format!("{at}.name"))?,
        public_key: key(required_str(t, &at, "public_key")?, &format!("{at}.public_key"))?,
        address: address.to_string(),
        endpoint: match optional_str(t, &at, "endpoint")? {
          None => None,
          Some(e) => Some(endpoint(e, &format!("{at}.endpoint"))?),
        },
        allowed_ips: allowed_ips(t, &at)?,
        persistent_keepalive: keepalive(t, &at)?,
      });
    }
  }
  let mut names = BTreeSet::new();
  let mut keys = BTreeSet::new();
  let mut addresses = BTreeSet::new();
  names.insert(hub.name.as_str());
  keys.insert(hub.public_key.as_str());
  for (i, p) in peers.iter().enumerate() {
    if !names.insert(p.name.as_str()) {
      bail!("peers[{i}].name {:?} is not unique", p.name);
    }
    if !keys.insert(p.public_key.as_str()) {
      bail!("peers[{i}].public_key is not unique");
    }
    if !addresses.insert(p.address.as_str()) {
      bail!("peers[{i}].address {:?} is not unique", p.address);
    }
  }
  Ok(Bundle { hub_version, hub, peers })
}

fn required_str<'a>(t: &'a dyn TableLike, at: &str, k: &str) -> Result<&'a str> {
  optional_str(t, at, k)?.ok_or_else(|| anyhow!("{at}.{k} is missing"))
}

fn optional_str<'a>(t: &'a dyn TableLike, at: &str, k: &str) -> Result<Option<&'a str>> {
  match t.get(k) {
    None => Ok(None),
    Some(item) => item.as_str().map(Some).ok_or_else(|| anyhow!("{at}.{k} must be a string")),
  }
}

fn required_int(t: &dyn TableLike, at: &str, k: &str) -> Result<i64> {
  optional_int(t, at, k)?.ok_or_else(|| anyhow!("{at}.{k} is missing"))
}

fn optional_int(t: &dyn TableLike, at: &str, k: &str) -> Result<Option<i64>> {
  match t.get(k) {
    None => Ok(None),
    Some(item) => item.as_integer().map(Some).ok_or_else(|| anyhow!("{at}.{k} must be an integer")),
  }
}

/// `^[a-z0-9][a-z0-9-]*$`.
fn name(s: &str, field: &str) -> Result<String> {
  let ok = !s.is_empty()
    && s
      .chars()
      .enumerate()
      .all(|(i, c)| c.is_ascii_lowercase() || c.is_ascii_digit() || (i > 0 && c == '-'));
  if !ok {
    bail!("{field} {s:?} must match ^[a-z0-9][a-z0-9-]*$");
  }
  Ok(s.to_string())
}

/// 44 base64 characters decoding to 32 bytes: a WireGuard key.
fn key(s: &str, field: &str) -> Result<String> {
  if s.len() != 44 || !base64_decode(s).is_some_and(|b| b.len() == 32) {
    bail!("{field} must be a 44-character base64 key decoding to 32 bytes");
  }
  Ok(s.to_string())
}

/// Standard alphabet with `=` padding; `None` on any other byte or a padding character out of
/// place. Enough of RFC 4648 to check a key; the value is never used, only its length.
fn base64_decode(s: &str) -> Option<Vec<u8>> {
  const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  let bytes = s.as_bytes();
  if bytes.is_empty() || !bytes.len().is_multiple_of(4) {
    return None;
  }
  let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
  for chunk in bytes.chunks(4) {
    let mut acc: u32 = 0;
    let mut pad = 0;
    for (i, &b) in chunk.iter().enumerate() {
      let v = if b == b'=' {
        if i < 2 {
          return None;
        }
        pad += 1;
        0
      } else {
        if pad > 0 {
          return None;
        }
        ALPHABET.iter().position(|&a| a == b)? as u32
      };
      acc = (acc << 6) | v;
    }
    let [_, b1, b2, b3] = acc.to_be_bytes();
    out.push(b1);
    if pad < 2 {
      out.push(b2);
    }
    if pad < 1 {
      out.push(b3);
    }
  }
  Some(out)
}

fn v4_cidr(s: &str) -> Option<(Ipv4Addr, u8)> {
  let (ip, prefix) = s.split_once('/')?;
  let ip: Ipv4Addr = ip.parse().ok()?;
  let prefix: u8 = prefix.parse().ok()?;
  (prefix <= 32).then_some((ip, prefix))
}

fn v4_network(ip: Ipv4Addr, prefix: u8) -> u32 {
  if prefix == 0 {
    0
  } else {
    u32::from(ip) & (u32::MAX << (32 - u32::from(prefix)))
  }
}

/// An IPv4 CIDR with a host part: a prefix shorter than 32 and host bits that are not all zero.
fn hub_address(s: &str) -> Result<String> {
  let (ip, prefix) = v4_cidr(s).ok_or_else(|| anyhow!("hub.address must be an IPv4 CIDR, got {s:?}"))?;
  if prefix == 32 || u32::from(ip) == v4_network(ip, prefix) {
    bail!("hub.address {s:?} must be an interface address with a host part, like 10.8.0.1/24");
  }
  Ok(s.to_string())
}

fn any_cidr(s: &str) -> bool {
  let Some((ip, prefix)) = s.split_once('/') else { return false };
  let Ok(ip) = ip.parse::<IpAddr>() else { return false };
  let Ok(prefix) = prefix.parse::<u8>() else { return false };
  prefix <= if ip.is_ipv4() { 32 } else { 128 }
}

/// `host:port`, port 1 to 65535. The host is not resolved here.
fn endpoint(s: &str, field: &str) -> Result<String> {
  let ok = s
    .rsplit_once(':')
    .is_some_and(|(host, port)| !host.is_empty() && port.parse::<u16>().is_ok_and(|p| p != 0));
  if !ok {
    bail!("{field} must be host:port with a port from 1 to 65535, got {s:?}");
  }
  Ok(s.to_string())
}

fn port(v: i64, field: &str) -> Result<u16> {
  u16::try_from(v)
    .ok()
    .filter(|p| *p != 0)
    .ok_or_else(|| anyhow!("{field} must be from 1 to 65535, got {v}"))
}

fn keepalive(t: &dyn TableLike, at: &str) -> Result<Option<u32>> {
  match optional_int(t, at, "persistent_keepalive")? {
    None => Ok(None),
    Some(v) => match u32::try_from(v) {
      Ok(n) if (1..=65535).contains(&n) => Ok(Some(n)),
      _ => bail!("{at}.persistent_keepalive must be from 1 to 65535, got {v}"),
    },
  }
}

fn allowed_ips(t: &dyn TableLike, at: &str) -> Result<Option<Vec<String>>> {
  let Some(item) = t.get("allowed_ips") else {
    return Ok(None);
  };
  let arr = item.as_array().ok_or_else(|| anyhow!("{at}.allowed_ips must be a list of CIDRs"))?;
  let mut out = Vec::new();
  for v in arr.iter() {
    match v.as_str() {
      Some(s) if any_cidr(s) => out.push(s.to_string()),
      _ => bail!("{at}.allowed_ips must be a list of CIDRs, got {v}"),
    }
  }
  if out.is_empty() {
    bail!("{at}.allowed_ips must not be empty");
  }
  Ok(Some(out))
}

/// What the spoke applies (spec 4.3): its entry's fields with the hub's defaults filled in.
#[derive(Debug, Clone, Eq)]
pub struct Effective {
  pub address: String,
  pub hub_public_key: String,
  pub endpoint: String,
  pub allowed_ips: Vec<String>,
  pub keepalive: u32,
}

impl PartialEq for Effective {
  /// Allowed IPs compare as sets: the order the hub wrote them in carries no meaning.
  fn eq(&self, other: &Self) -> bool {
    self.address == other.address
      && self.hub_public_key == other.hub_public_key
      && self.endpoint == other.endpoint
      && self.keepalive == other.keepalive
      && self.allowed_ips.iter().collect::<BTreeSet<_>>() == other.allowed_ips.iter().collect::<BTreeSet<_>>()
  }
}

impl Effective {
  fn short_key(&self) -> &str {
    self.hub_public_key.get(..8).unwrap_or(&self.hub_public_key)
  }

  /// One line for the log, the hub key shortened to eight characters.
  pub fn describe(&self) -> String {
    format!(
      "address {}, endpoint {}, allowed IPs {}, keepalive {}, hub key {}…",
      self.address,
      self.endpoint,
      self.allowed_ips.join(","),
      self.keepalive,
      self.short_key()
    )
  }

  /// The fields that differ, `old -> new`, for the re-apply line (spec 5.5).
  pub fn diff(&self, new: &Effective) -> String {
    let mut parts = Vec::new();
    if self.hub_public_key != new.hub_public_key {
      parts.push(format!("hub key {}… -> {}…", self.short_key(), new.short_key()));
    }
    if self.address != new.address {
      parts.push(format!("address {} -> {}", self.address, new.address));
    }
    if self.endpoint != new.endpoint {
      parts.push(format!("endpoint {} -> {}", self.endpoint, new.endpoint));
    }
    if self.allowed_ips.iter().collect::<BTreeSet<_>>() != new.allowed_ips.iter().collect::<BTreeSet<_>>() {
      parts.push(format!(
        "allowed IPs {} -> {}",
        self.allowed_ips.join(","),
        new.allowed_ips.join(",")
      ));
    }
    if self.keepalive != new.keepalive {
      parts.push(format!("keepalive {} -> {}", self.keepalive, new.keepalive));
    }
    parts.join(", ")
  }
}

/// The `[[peers]]` row whose key is `public_key`, as the effective configuration; `None` is "not
/// enrolled". The hub's own key never matches: it is not a peer of itself.
pub fn select(bundle: &Bundle, public_key: &str) -> Option<Effective> {
  let peer = bundle.peers.iter().find(|p| p.public_key == public_key)?;
  Some(Effective {
    address: peer.address.clone(),
    hub_public_key: bundle.hub.public_key.clone(),
    endpoint: peer.endpoint.clone().unwrap_or_else(|| bundle.hub.endpoint.clone()),
    allowed_ips: peer.allowed_ips.clone().unwrap_or_else(|| bundle.hub.allowed_ips.clone()),
    keepalive: peer.persistent_keepalive.unwrap_or(bundle.hub.persistent_keepalive),
  })
}

/// A fetched bundle must carry the tag it was fetched at (spec 4.2).
pub fn require_version(bundle: &Bundle, tag: &str) -> Result<()> {
  match bundle.hub_version.as_deref() {
    Some(v) if v == tag => Ok(()),
    Some(v) => bail!("the bundle is stamped hub_version {v} but was fetched at {tag}"),
    None => bail!("the bundle has no hub_version; the release did not stamp it"),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  /// A syntactically valid WireGuard key: 43 base64 characters and one `=`, 32 bytes.
  pub fn key(c: char) -> String {
    format!("{}=", c.to_string().repeat(43))
  }

  pub fn good() -> String {
    format!(
      r#"schema = 1

[hub]
name = "wireguard-hub"
public_key = "{hub}"
address = "10.8.0.1/24"
listen_port = 51820
endpoint = "tunnels.example.com:51820"
allowed_ips = ["10.8.0.0/24"]
persistent_keepalive = 25

[[peers]]
name = "office-db-pc"
public_key = "{pc}"
address = "10.8.0.10/32"

[[peers]]
name = "scheduled-report-aggregator"
public_key = "{sra}"
address = "10.8.0.20/32"
endpoint = "wireguard-hub:51820"
allowed_ips = ["10.8.0.10/32"]
persistent_keepalive = 15
"#,
      hub = key('A'),
      pc = key('B'),
      sra = key('C')
    )
  }

  #[test]
  fn the_tag_is_strict() {
    assert_eq!(validate_tag("v1.2.3").unwrap(), "v1.2.3");
    assert_eq!(validate_tag("v10.0.100").unwrap(), "v10.0.100");
    for bad in ["1.2.3", "v1.2", "v1.2.3-rc1", "../x", "v1.2.3\n", "", "v", "v1..3", "V1.2.3"] {
      assert!(validate_tag(bad).is_err(), "{bad:?} must be rejected");
    }
  }

  #[test]
  fn a_good_table_parses_with_its_overrides() {
    let b = parse(&good()).unwrap();
    assert_eq!(b.hub_version, None);
    assert_eq!(b.hub.name, "wireguard-hub");
    assert_eq!(b.hub.listen_port, 51820);
    assert_eq!(b.hub.allowed_ips, ["10.8.0.0/24"]);
    assert_eq!(b.hub.persistent_keepalive, 25);
    assert_eq!(b.peers.len(), 2);
    assert_eq!(b.peers[0].name, "office-db-pc");
    assert_eq!(b.peers[0].endpoint, None);
    assert_eq!(b.peers[1].endpoint.as_deref(), Some("wireguard-hub:51820"));
    assert_eq!(b.peers[1].allowed_ips.as_deref(), Some(&["10.8.0.10/32".to_string()][..]));
    assert_eq!(b.peers[1].persistent_keepalive, Some(15));
    let stamped = format!("hub_version = \"v0.3.1\"\n{}", good());
    assert_eq!(parse(&stamped).unwrap().hub_version.as_deref(), Some("v0.3.1"));
  }

  #[test]
  fn every_rule_of_3_2_rejects_with_the_field_named() {
    // (what to change in `good()`, the substring the error must carry)
    let cases: Vec<(&str, &str, &str)> = vec![
      ("schema = 1", "schema = 2", "schema"),
      ("schema = 1", "", "schema"),
      ("name = \"office-db-pc\"", "name = \"Office-DB\"", "peers[0].name"),
      ("name = \"office-db-pc\"", "name = \"-x\"", "peers[0].name"),
      ("name = \"office-db-pc\"", "name = \"scheduled-report-aggregator\"", "name"),
      ("address = \"10.8.0.10/32\"", "address = \"10.8.0.10/24\"", "peers[0].address"),
      ("address = \"10.8.0.10/32\"", "address = \"10.9.0.10/32\"", "peers[0].address"),
      ("address = \"10.8.0.10/32\"", "address = \"10.8.0.1/32\"", "peers[0].address"),
      ("address = \"10.8.0.10/32\"", "address = \"10.8.0.20/32\"", "address"),
      ("address = \"10.8.0.1/24\"", "address = \"10.8.0.1/32\"", "hub.address"),
      ("address = \"10.8.0.1/24\"", "address = \"10.8.0.0/24\"", "hub.address"),
      ("address = \"10.8.0.1/24\"", "address = \"10.8.0.1\"", "hub.address"),
      (
        "endpoint = \"tunnels.example.com:51820\"",
        "endpoint = \"tunnels.example.com\"",
        "hub.endpoint",
      ),
      (
        "endpoint = \"tunnels.example.com:51820\"",
        "endpoint = \"tunnels.example.com:0\"",
        "hub.endpoint",
      ),
      (
        "endpoint = \"wireguard-hub:51820\"",
        "endpoint = \"wireguard-hub:70000\"",
        "peers[1].endpoint",
      ),
      ("listen_port = 51820", "listen_port = 0", "hub.listen_port"),
      ("listen_port = 51820", "", "hub.listen_port"),
      ("persistent_keepalive = 25", "persistent_keepalive = 0", "hub.persistent_keepalive"),
      (
        "persistent_keepalive = 15",
        "persistent_keepalive = 70000",
        "peers[1].persistent_keepalive",
      ),
      ("allowed_ips = [\"10.8.0.0/24\"]", "allowed_ips = []", "hub.allowed_ips"),
      (
        "allowed_ips = [\"10.8.0.0/24\"]",
        "allowed_ips = [\"10.8.0.0/33\"]",
        "hub.allowed_ips",
      ),
      (
        "allowed_ips = [\"10.8.0.10/32\"]",
        "allowed_ips = [\"nope\"]",
        "peers[1].allowed_ips",
      ),
    ];
    for (from, to, field) in cases {
      let text = good().replacen(from, to, 1);
      assert_ne!(text, good(), "the case {from:?} -> {to:?} changed nothing");
      let err = parse(&text).unwrap_err().to_string();
      assert!(err.contains(field), "{from:?} -> {to:?}: {err}");
    }
  }

  #[test]
  fn keys_are_44_base64_characters_decoding_to_32_bytes_and_unique() {
    let no_padding = "A".repeat(44);
    let double_padding = format!("{}==", "A".repeat(42));
    let bad_char = format!("{}*=", "A".repeat(42));
    for bad in ["short=", no_padding.as_str(), double_padding.as_str(), bad_char.as_str()] {
      let text = good().replacen(&key('B'), bad, 1);
      let err = parse(&text).unwrap_err().to_string();
      assert!(err.contains("peers[0].public_key"), "{bad:?}: {err}");
    }
    let dup = good().replacen(&key('B'), &key('C'), 1);
    assert!(parse(&dup).unwrap_err().to_string().contains("public_key"));
    let hub_dup = good().replacen(&key('A'), &key('B'), 1);
    assert!(parse(&hub_dup).unwrap_err().to_string().contains("public_key"));
  }

  #[test]
  fn a_stamped_version_must_be_a_tag() {
    let text = format!("hub_version = \"0.3.1\"\n{}", good());
    assert!(parse(&text).unwrap_err().to_string().contains("hub_version"));
  }

  #[test]
  fn garbage_is_named_as_toml() {
    assert!(parse("not = [toml").unwrap_err().to_string().contains("TOML"));
  }
  #[test]
  fn the_entry_is_picked_by_key_and_overrides_win() {
    let b = parse(&good()).unwrap();
    let pc = select(&b, &key('B')).unwrap();
    assert_eq!(pc.address, "10.8.0.10/32");
    assert_eq!(pc.hub_public_key, key('A'));
    assert_eq!(pc.endpoint, "tunnels.example.com:51820");
    assert_eq!(pc.allowed_ips, ["10.8.0.0/24"]);
    assert_eq!(pc.keepalive, 25);
    let sra = select(&b, &key('C')).unwrap();
    assert_eq!(sra.endpoint, "wireguard-hub:51820");
    assert_eq!(sra.allowed_ips, ["10.8.0.10/32"]);
    assert_eq!(sra.keepalive, 15);
    assert_eq!(select(&b, &key('Z')), None, "not enrolled");
    assert_eq!(select(&b, &key('A')), None, "the hub is not a peer");
  }

  #[test]
  fn effective_configurations_compare_allowed_ips_as_sets() {
    let a = Effective {
      address: "10.8.0.20/32".into(),
      hub_public_key: key('A'),
      endpoint: "h:1".into(),
      allowed_ips: vec!["10.8.0.0/24".into(), "10.9.0.0/24".into()],
      keepalive: 25,
    };
    let mut b = a.clone();
    b.allowed_ips.reverse();
    assert_eq!(a, b);
    b.allowed_ips.push("10.10.0.0/24".into());
    assert_ne!(a, b);
    let mut c = a.clone();
    c.keepalive = 26;
    assert_ne!(a, c);
  }

  #[test]
  fn the_stamped_version_must_equal_the_requested_tag() {
    let b = parse(&format!("hub_version = \"v0.3.1\"\n{}", good())).unwrap();
    assert!(require_version(&b, "v0.3.1").is_ok());
    let err = require_version(&b, "v0.3.2").unwrap_err().to_string();
    assert!(err.contains("v0.3.1") && err.contains("v0.3.2"), "{err}");
    let unstamped = parse(&good()).unwrap();
    assert!(
      require_version(&unstamped, "v0.3.1")
        .unwrap_err()
        .to_string()
        .contains("hub_version")
    );
  }
  #[test]
  fn describe_and_diff_shorten_keys_and_name_only_what_changed() {
    let b = parse(&good()).unwrap();
    let old = select(&b, &key('B')).unwrap();
    assert_eq!(
      old.describe(),
      format!(
        "address 10.8.0.10/32, endpoint tunnels.example.com:51820, allowed IPs 10.8.0.0/24, keepalive 25, hub key {}…",
        "A".repeat(8)
      )
    );
    assert_eq!(old.diff(&old), "");
    let mut new = old.clone();
    new.address = "10.8.0.11/32".into();
    new.keepalive = 15;
    new.hub_public_key = key('Z');
    assert_eq!(
      old.diff(&new),
      format!(
        "hub key {}… -> {}…, address 10.8.0.10/32 -> 10.8.0.11/32, keepalive 25 -> 15",
        "A".repeat(8),
        "Z".repeat(8)
      )
    );
  }
}
