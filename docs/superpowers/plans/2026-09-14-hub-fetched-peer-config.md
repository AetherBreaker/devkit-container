# Hub-fetched peer configuration, startup scripts and shutdown consent: devkit-container implementation plan

> **For agentic workers:** this plan is executed inline by the session that holds it (spec rule
> 0.2.4), task by task, with the superpowers:executing-plans skill. It is not delegated to
> subagents. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** the `devkit-container` piece of the hub design: a spoke that fetches its peer
configuration from the hub's GitHub release, keeps it current in place, refuses a boot that cannot
connect, asks its app before a long-outage shutdown, runs a project's startup scripts as root, and
scrubs the secrets it names; plus the templates and the tests that prove it.

**Architecture:** the binary gains five pure or self-contained modules (`bundle`, `health`,
`cache`, `logfile`, and on Linux `fetch` and `consent`), the tunnel module gains a command planner
that the shell-outs execute, and the entrypoint and supervisor are rewritten on top of them. The
supervisor's loop never waits on the network: fetches and consent asks run on worker threads and
their results are consumed at the next poll; the loop itself wakes on a self-pipe that signals and
the child's exit write to.

**Tech Stack:** Rust 2024 edition; `anyhow`, `clap`, `jiff`, `toml_edit` (all platforms);
`nix` (with `poll`), `signal-hook`, `ureq` (rustls), `serde_json` (Linux only). Tests: `cargo test`
on both platforms; Docker smoke tests on Linux (CI) or a Docker Desktop with the WireGuard kernel
module.

**Spec:** `docs/superpowers/specs/2026-09-14-hub-fetched-peer-config-design.md` (frozen). The plan
argues from the spec; both travel together and the executor reads both.

## The two rules of the spec, verbatim (0.2)

1. **This document is the source of truth for the implementation plan.** Where the plan is
   ambiguous, or the plan and this document disagree, this document decides.
2. **Where this document is silent, incomplete or contradictory on a point the implementation
   needs, the implementer stops and asks the owner.** Nobody fills a gap with their own judgement:
   not the plan's author, not the agent executing it. This includes naming, defaults, error text,
   ordering, file locations, retry counts, and "the code already does X so I will keep X". However
   small or obvious the gap looks, the owner becomes the source of truth for it before anything
   else continues, and the answer is written into this document before the plan or the code
   changes. Log lines and error wording this document does not fix verbatim are the exception:
   they are the implementer's, and the owner does not review them.

"This document" in both rules is the spec, not this plan.

## Global constraints

- Every `cargo` command runs from the repo root. `cargo test` must pass on Windows and Linux;
  `cargo clippy --all-targets -- -D warnings` and `cargo fmt --all --check` must pass (CI runs
  them). Formatting: 2-space indent, the repo's `rustfmt.toml`.
- Python-side commands (pytest, ruff, pyright, devkit) run under `uv run`.
- Commit messages follow Conventional Commits with a scope (`feat(bundle): …`), and end with
  `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`. Commit after every task.
- The nonroot user is uid/gid `999` (`prepare::NONROOT`). The app root is `/app`; persisted data
  under `/app/persisted_data`; the logs folder `/app/persisted_data/logs`; the cache folder
  `/app/persisted_data/wireguard`; the consent directory `/run/devkit`, mode `0700`, owned
  `999:999`; the socket `/run/devkit/consent.sock`.
- Secrets scrubbed from the app, always: `WG_PRIVATE_KEY`, `WG_PEER_PRESHARED_KEY`,
  `WG_HUB_TOKEN`; plus every name in `[tool.docker].scrub_env`. On the spawn and the exec path.
- Timers and defaults (spec 5.4): `WG_POLL_SECS` 30; `WG_STALE_SECS` 180, at least 150;
  `WG_HANDSHAKE_TIMEOUT_SECS` 60; `WG_DISCONNECTED_LIMIT_SECS` 1800; `WG_HOLD_LIMIT_SECS` 0 (0 =
  no bound); `WG_VERSION_POLL_SECS` 300; `WG_TOLERATE_DISCONNECTED` unset, empty or `1`. Every
  `*_SECS` an integer at least 1 except the hold limit, which accepts 0. Empty is unset.
- Exit codes: the app's code passes through; Broken, a refused start and a failed startup script
  exit 1; give-up and the removal shutdown exit 75.
- The consent ask times out after 60 s; the next ask comes 60 s after a reply or timeout; the
  shutdown after consent, the removal shutdown and a runtime Broken give the app 30 s to exit on
  its signal (SIGINT for the first two, SIGTERM for Broken) before SIGKILL.
- The hub's tag is `^v[0-9]+\.[0-9]+\.[0-9]+$`; the bundle body and the release listing are each
  capped at 1 MiB; every HTTP request has a 10 s timeout; GitHub requests carry
  `Accept: application/vnd.github+json` (listing) or `application/octet-stream` (asset),
  `X-GitHub-Api-Version: 2022-11-28`, `User-Agent: devkit-container/<version>`; the token goes
  only to `api.github.com`.
- `WG_HUB_URL` is `http://` or `https://` plus host and an optional port, no path; one trailing
  slash is stripped; anything else after the host, a path, a query or a fragment, is refused.
  `WG_HUB_REPO` is `owner/repo`, each part non-empty, characters `[A-Za-z0-9._-]`.
- Log lines: every line the binary writes for itself goes through `logfile::Log::line`, which
  prints `devkit-container: <msg>` to stderr and appends `<timestamp> <msg>` to
  `persisted_data/logs/devkit-container.log`, best effort. Keys are never printed beyond their
  first eight characters except the spoke's own public key, which is public and is logged in
  full at every start.
- No test-only environment variables exist in the binary. Tests pass listener addresses to the
  fetch code as ordinary inputs.

## File structure

New files, all under `src/`:

| File | Responsibility |
| --- | --- |
| `bundle.rs` | the bundle contract: tag validation, `peers.toml` parse and validation (spec 3.2), entry selection and the effective configuration (4.3). Pure, all platforms. |
| `health.rs` | the tunnel's state machine (5.3, 5.4, 5.6, 6.1, 6.3) with an injected clock: Connected/Disconnected, the disconnected clock, the alternating repair, consent asks, give-up. Pure, all platforms. |
| `cache.rs` | the cached bundle file (4.4): path, read, atomic write, ownership. |
| `logfile.rs` | the placeholder log file (5.8). |
| `fetch.rs` | Linux only: the version endpoint (4.1) and the GitHub fetch (4.2) over `ureq`, and `obtain`, the version → bundle → select sequence. |
| `consent.rs` | Linux only: the supervisor's consent client (6.2). |

Modified files:

| File | Change |
| --- | --- |
| `Cargo.toml` | `nix` gains the `poll` feature; `serde_json` becomes a Linux-only runtime dependency at `1.0.151` (it stays a dev-dependency too). |
| `src/main.rs` | declares the new modules. |
| `src/wireguard.rs` | rewritten: `Settings` and `Mode` replace `Config` (the environment contract of 5.1 and 8); the command planner (`Cmd`, `apply_commands`, `endpoint_command`, `reapply_commands`); the Linux `Interface` (create, apply, set_endpoint, reapply, down_up, latest_handshake, wait_handshake, down) replaces `Tunnel`; `Assessor` is deleted (superseded by `health`). |
| `src/pyproject.rs` | `startup_scripts` and `scrub_env` (spec 7). |
| `src/run.rs` | the boot sequence of 5.2 and the order of 7. |
| `src/supervisor.rs` | the loop of 5.2 step 7: self-pipe wake, worker threads, health decisions, repairs, in-place re-apply, consent, the three shutdown paths. |
| `python/devkit_container/compose.template.yaml` | the four spoke lines (9.2). |
| `python/devkit_container/template.Dockerfile` | the two windows (9.3). |
| `README.md`, `todo.md` | the schema, the environment contract, the run description, the tests; the TODO entries of spec 15. |
| `.github/workflows/ci.yml` | the fixture token secret for the smoke job; the new smoke test binary. |
| `tests/common/mod.rs`, `tests/docker_supervisor.rs` | the consent socket in the report; the gate helper unchanged in behaviour. |
| `tests/docker_fetched.rs` | new: the fetched-mode smoke test of spec 13. |

While the new modules are being added (tasks 1 to 9) nothing calls them yet, so each carries
`#![allow(dead_code)]` at its top with the comment `// until run and the supervisor use it (task
11)`; task 11 removes every one of those lines. This keeps `cargo clippy -- -D warnings` green
after every task.

---

### Task 1: `bundle.rs`, the tag and the peer table

**Files:**

- Create: `src/bundle.rs`
- Modify: `src/main.rs` (module list)
- Test: unit tests inside `src/bundle.rs`

**Interfaces:**

- Produces: `bundle::validate_tag(&str) -> Result<String>`; `bundle::Bundle { hub_version: Option<String>, hub: Hub, peers: Vec<Peer> }`; `bundle::Hub`; `bundle::Peer`; `bundle::parse(&str) -> Result<Bundle>`.

- [ ] **Step 1: Declare the module**

In `src/main.rs`, after `mod healthcheck;` add:

```rust
#[cfg_attr(not(unix), allow(dead_code))] // the fetch and the cache (Linux) are its callers
mod bundle;
```

- [ ] **Step 2: Write the failing tests**

Create `src/bundle.rs` with the module doc, the `#![allow(dead_code)]` line and only the tests
module first:

```rust
//! The bundle contract (spec 3.2, 4.1, 4.3): the strict tag, the peer table parsed and
//! validated, the spoke's own entry and the effective configuration it yields. Pure: no IO and
//! no network, so it runs and is tested on every platform.
#![allow(dead_code)] // until run and the supervisor use it (task 11)

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
      ("endpoint = \"tunnels.example.com:51820\"", "endpoint = \"tunnels.example.com\"", "hub.endpoint"),
      ("endpoint = \"tunnels.example.com:51820\"", "endpoint = \"tunnels.example.com:0\"", "hub.endpoint"),
      ("endpoint = \"wireguard-hub:51820\"", "endpoint = \"wireguard-hub:70000\"", "peers[1].endpoint"),
      ("listen_port = 51820", "listen_port = 0", "hub.listen_port"),
      ("listen_port = 51820", "", "hub.listen_port"),
      ("persistent_keepalive = 25", "persistent_keepalive = 0", "hub.persistent_keepalive"),
      ("persistent_keepalive = 15", "persistent_keepalive = 70000", "peers[1].persistent_keepalive"),
      ("allowed_ips = [\"10.8.0.0/24\"]", "allowed_ips = []", "hub.allowed_ips"),
      ("allowed_ips = [\"10.8.0.0/24\"]", "allowed_ips = [\"10.8.0.0/33\"]", "hub.allowed_ips"),
      ("allowed_ips = [\"10.8.0.10/32\"]", "allowed_ips = [\"nope\"]", "peers[1].allowed_ips"),
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
    for bad in ["short=", &format!("{}", "A".repeat(44)), &format!("{}==", "A".repeat(42)), &format!("{}=", "A".repeat(42) + "*")] {
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
}
```

- [ ] **Step 3: Run the tests to see them fail**

Run: `cargo test bundle`
Expected: compile errors, `validate_tag` and `parse` not found.

- [ ] **Step 4: Implement the module**

Insert the implementation between the `#![allow(dead_code)]` line and `#[cfg(test)]`:

```rust
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
  let hub_table = doc.get("hub").and_then(Item::as_table_like).ok_or_else(|| anyhow!("[hub] is missing"))?;
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
    let tables = array.as_array_of_tables().ok_or_else(|| anyhow!("peers must be an array of tables"))?;
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
  if bytes.is_empty() || bytes.len() % 4 != 0 {
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
  u16::try_from(v).ok().filter(|p| *p != 0).ok_or_else(|| anyhow!("{field} must be from 1 to 65535, got {v}"))
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
```

- [ ] **Step 5: Run the tests**

Run: `cargo test bundle`
Expected: all six pass. If a case in `every_rule_of_3_2_rejects_with_the_field_named` fails on
the field name, fix the message, not the test: the spec says every failure names the field.

- [ ] **Step 6: Lint and commit**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings`
Expected: clean.

```bash
git add src/main.rs src/bundle.rs
git commit -m "feat(bundle): parse and validate the hub's peer table and the strict tag

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 2: `bundle.rs`, the spoke's entry and the effective configuration

**Files:**

- Modify: `src/bundle.rs`
- Test: unit tests inside `src/bundle.rs`

**Interfaces:**

- Consumes: `Bundle`, `Hub`, `Peer` from task 1.
- Produces: `bundle::Effective { address, hub_public_key, endpoint, allowed_ips: Vec<String>, keepalive: u32 }` with `PartialEq` comparing allowed IPs as sets; `bundle::select(&Bundle, public_key: &str) -> Option<Effective>`; `bundle::require_version(&Bundle, tag: &str) -> Result<()>`.

- [ ] **Step 1: Write the failing tests**

Append inside the `tests` module of `src/bundle.rs`:

```rust
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
    assert!(require_version(&unstamped, "v0.3.1").unwrap_err().to_string().contains("hub_version"));
  }
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test bundle`
Expected: compile errors, `select`, `Effective`, `require_version` not found.

- [ ] **Step 3: Implement**

Add above the `tests` module:

```rust
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
```

- [ ] **Step 4: Run the tests**

Run: `cargo test bundle`
Expected: all nine pass.

- [ ] **Step 5: Lint and commit**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings`

```bash
git add src/bundle.rs
git commit -m "feat(bundle): select the spoke's entry and build the effective configuration

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 3: `wireguard.rs`, the environment contract (`Settings`, `Mode`)

The old `Config` stays in the file until task 11 deletes it; `Settings` is added beside it.

**Files:**

- Modify: `src/wireguard.rs`
- Test: unit tests inside `src/wireguard.rs`

**Interfaces:**

- Consumes: `bundle::Effective` (task 2).
- Produces: `wireguard::SECRET_VARS: [&str; 3]`; `wireguard::Mode::{Env { effective: Effective, preshared_key: Option<String> }, Fetched { hub_url: String, repo: String, token: Option<String> }}`; `wireguard::Settings { private_key, mode, tolerate: bool, poll_secs, stale_secs, handshake_timeout_secs, limit_secs, hold_limit_secs, version_poll_secs }` with `Settings::from_env(get: &dyn Fn(&str) -> Option<String>) -> Result<Settings>`; `wireguard::validate_hub_url(&str) -> Result<String>`; `wireguard::validate_repo(&str) -> Result<String>`.

- [ ] **Step 1: Write the failing tests**

Inside the existing `tests` module of `src/wireguard.rs`, add:

```rust
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
      (s.poll_secs, s.stale_secs, s.handshake_timeout_secs, s.limit_secs, s.hold_limit_secs, s.version_poll_secs),
      (30, 180, 60, 1800, 0, 300)
    );
    let e = env(&FETCHED);
    let s = Settings::from_env(&|k| e.get(k).cloned()).unwrap();
    assert!(matches!(s.mode, Mode::Fetched { token: None, .. }), "the token is optional to the binary");
  }

  #[test]
  fn environment_mode_builds_the_effective_configuration() {
    let mut e = env(&REQUIRED);
    e.insert("WG_PEER_PRESHARED_KEY".into(), "psk".into());
    e.insert("WG_PERSISTENT_KEEPALIVE".into(), "10".into());
    let s = Settings::from_env(&|k| e.get(k).cloned()).unwrap();
    let Mode::Env { effective, preshared_key } = s.mode else { panic!("environment mode") };
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
    assert!(Settings::from_env(&|k| e.get(k).cloned()).unwrap_err().to_string().contains("WG_HUB_REPO"));
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
    assert_eq!(validate_hub_url("https://tunnels.example.com/").unwrap(), "https://tunnels.example.com");
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
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test wireguard`
Expected: compile errors: `Settings`, `Mode`, `validate_hub_url`, `validate_repo` not found.

- [ ] **Step 3: Implement**

At the top of `src/wireguard.rs`, replace the module doc, the imports and the secret list, and
add the settings after the existing `Config` block (before `Action`):

```rust
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
```

The old `Config` block keeps working as it is (it goes in task 11); if the new import line
duplicates the old `use anyhow::…` line, keep one line with the union of the names.

- [ ] **Step 4: Run the tests**

Run: `cargo test wireguard`
Expected: the five new tests and the four existing ones pass.

- [ ] **Step 5: Lint and commit**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings`

```bash
git add src/wireguard.rs
git commit -m "feat(wireguard): read the fetched-mode contract, the timers and the tolerate switch

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 4: `health.rs`, the state machine

**Files:**

- Create: `src/health.rs`
- Modify: `src/main.rs` (module list)
- Test: unit tests inside `src/health.rs`

**Interfaces:**

- Produces: `health::Reason::{NoHandshake, EndpointUnresolvable, ConfigUnavailable, NotEnrolled}` with `as_str()`; `health::State::{Connected, Disconnected(Reason)}` with `describe()`; `health::Interface::{Unconfigured(Reason), Configured { endpoint_ok: bool }}`; `health::Reply::{Ok, Hold}`; `health::Repair::{None, Obtain, ResetEndpoint, DownUp}`; `health::GiveUp { elapsed: u64, hold_limit: bool }`; `health::Decision { state, changed, repair, check_version, ask: Option<u64>, give_up: Option<GiveUp> }`; `health::Health::new(tolerate, fetched, limit_secs, hold_limit_secs)`, `Health::poll(&mut self, now: u64, iface, fresh, reply: Option<Reply>) -> Decision`, `Health::config_applied(&mut self)`; `health::ASK_INTERVAL_SECS = 60`.

- [ ] **Step 1: Declare the module**

In `src/main.rs`, after the `bundle` line:

```rust
#[cfg_attr(not(unix), allow(dead_code))] // the supervisor (Linux) drives it
mod health;
```

- [ ] **Step 2: Write the failing tests**

Create `src/health.rs`:

```rust
//! The tunnel's health (spec 5.3 to 5.6, 6.1, 6.3) as a pure state machine with an injected
//! clock: Connected or Disconnected with a reason, the continuous-disconnected clock, the
//! alternating repair, and the consent asks up to the give-up. Every shell-out and network call
//! is the supervisor's; this decides what to do at each poll.
#![allow(dead_code)] // until run and the supervisor use it (task 11)

#[cfg(test)]
mod tests {
  use super::*;

  const OK: Interface = Interface::Configured { endpoint_ok: true };

  fn default() -> Health {
    Health::new(false, true, 1800, 0)
  }

  #[test]
  fn connected_polls_decide_nothing_and_log_the_first_transition() {
    let mut h = default();
    let d = h.poll(0, OK, true, None);
    assert_eq!(d.state, State::Connected);
    assert!(d.changed, "the first poll is a transition from nothing");
    assert_eq!((d.repair, d.check_version, d.ask, d.give_up), (Repair::None, false, None, None));
    let d = h.poll(30, OK, true, None);
    assert!(!d.changed);
  }

  #[test]
  fn the_reason_follows_the_interface() {
    let mut h = default();
    assert_eq!(h.poll(0, OK, false, None).state, State::Disconnected(Reason::NoHandshake));
    assert_eq!(
      h.poll(30, Interface::Configured { endpoint_ok: false }, false, None).state,
      State::Disconnected(Reason::EndpointUnresolvable)
    );
    let mut t = Health::new(true, true, 1800, 0);
    assert_eq!(
      t.poll(0, Interface::Unconfigured(Reason::ConfigUnavailable), false, None).state,
      State::Disconnected(Reason::ConfigUnavailable)
    );
    assert_eq!(
      t.poll(30, Interface::Unconfigured(Reason::NotEnrolled), false, None).state,
      State::Disconnected(Reason::NotEnrolled)
    );
    assert_eq!(Reason::EndpointUnresolvable.as_str(), "endpoint unresolvable");
    assert_eq!(State::Disconnected(Reason::NoHandshake).describe(), "Disconnected: no handshake");
    assert_eq!(State::Connected.describe(), "Connected");
  }

  #[test]
  fn the_repair_alternates_and_restarts_after_connected_or_an_apply() {
    let mut h = default();
    h.poll(0, OK, true, None);
    assert_eq!(h.poll(30, OK, false, None).repair, Repair::ResetEndpoint);
    assert_eq!(h.poll(60, OK, false, None).repair, Repair::DownUp);
    assert_eq!(h.poll(90, OK, false, None).repair, Repair::ResetEndpoint);
    assert_eq!(h.poll(120, OK, false, None).repair, Repair::DownUp);
    assert_eq!(h.poll(150, OK, true, None).repair, Repair::None, "Connected repairs nothing");
    assert_eq!(h.poll(180, OK, false, None).repair, Repair::ResetEndpoint, "starts over");
    assert_eq!(h.poll(210, OK, false, None).repair, Repair::DownUp);
    h.config_applied();
    assert_eq!(h.poll(240, OK, false, None).repair, Repair::ResetEndpoint, "starts over after an apply");
  }

  #[test]
  fn an_unconfigured_interface_is_obtained_and_never_version_checked() {
    let mut t = Health::new(true, true, 1800, 0);
    let d = t.poll(0, Interface::Unconfigured(Reason::ConfigUnavailable), false, None);
    assert_eq!((d.repair, d.check_version), (Repair::Obtain, false));
    let d = t.poll(30, Interface::Unconfigured(Reason::ConfigUnavailable), false, None);
    assert_eq!(d.repair, Repair::Obtain, "every poll");
    t.config_applied();
    let d = t.poll(60, OK, false, None);
    assert_eq!((d.repair, d.check_version), (Repair::ResetEndpoint, true));
    let mut env_mode = Health::new(false, false, 1800, 0);
    assert!(!env_mode.poll(0, OK, false, None).check_version, "no version check in environment mode");
  }

  #[test]
  fn the_first_ask_comes_when_the_clock_crosses_the_limit_then_every_60_s_after_a_reply() {
    let mut h = default();
    h.poll(0, OK, true, None);
    assert_eq!(h.poll(10, OK, false, None).ask, None, "the clock starts here");
    assert_eq!(h.poll(1790, OK, false, None).ask, None, "1780 s in");
    let d = h.poll(1810, OK, false, None);
    assert_eq!(d.ask, Some(1800), "crossed: ask with the elapsed seconds");
    assert_eq!(h.poll(1840, OK, false, None).ask, None, "one ask outstanding");
    let d = h.poll(1870, OK, false, Some(Reply::Hold));
    assert_eq!((d.ask, d.give_up), (None, None), "hold postpones");
    assert_eq!(h.poll(1900, OK, false, None).ask, None, "not 60 s yet");
    assert_eq!(h.poll(1930, OK, false, None).ask, Some(1920), "60 s after the reply");
    let d = h.poll(1960, OK, false, Some(Reply::Ok));
    assert_eq!(d.give_up, Some(GiveUp { elapsed: 1950, hold_limit: false }));
  }

  #[test]
  fn a_timeout_counts_as_ok_by_the_client_and_connected_cancels_everything() {
    let mut h = default();
    h.poll(0, OK, false, None);
    assert!(h.poll(1800, OK, false, None).ask.is_some());
    let d = h.poll(1830, OK, true, None);
    assert_eq!((d.state, d.ask, d.give_up), (State::Connected, None, None));
    let d = h.poll(1860, OK, false, Some(Reply::Ok));
    assert_eq!(d.give_up, None, "a late reply after Connected is ignored");
    assert_eq!(h.poll(3659, OK, false, None).ask, None, "the clock restarted at 1860");
    assert_eq!(h.poll(3660, OK, false, None).ask, Some(1800));
  }

  #[test]
  fn the_hold_limit_proceeds_without_asking_again() {
    let mut h = Health::new(false, true, 1800, 100);
    h.poll(0, OK, false, None);
    assert_eq!(h.poll(1800, OK, false, None).ask, Some(1800));
    h.poll(1830, OK, false, Some(Reply::Hold));
    assert_eq!(h.poll(1890, OK, false, None).ask, Some(1890));
    let d = h.poll(1900, OK, false, None);
    assert_eq!(d.give_up, Some(GiveUp { elapsed: 1900, hold_limit: true }), "100 s after the first ask");
  }

  #[test]
  fn under_the_switch_there_is_no_ask_and_no_give_up() {
    let mut t = Health::new(true, true, 1800, 100);
    t.poll(0, OK, false, None);
    let d = t.poll(10_000, OK, false, None);
    assert_eq!((d.ask, d.give_up), (None, None));
    assert_eq!(d.repair, Repair::DownUp, "repairs go on");
  }
}
```

- [ ] **Step 3: Run the tests to see them fail**

Run: `cargo test health`
Expected: compile errors, nothing defined.

- [ ] **Step 4: Implement**

Insert between the `#![allow(dead_code)]` line and `#[cfg(test)]`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
  NoHandshake,
  EndpointUnresolvable,
  /// Only under the tolerate switch: the boot got no bundle and no usable cache.
  ConfigUnavailable,
  /// Only under the tolerate switch: the hub's bundle is valid and lacks this key.
  NotEnrolled,
}

impl Reason {
  pub fn as_str(self) -> &'static str {
    match self {
      Reason::NoHandshake => "no handshake",
      Reason::EndpointUnresolvable => "endpoint unresolvable",
      Reason::ConfigUnavailable => "config unavailable",
      Reason::NotEnrolled => "not enrolled",
    }
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
  Connected,
  Disconnected(Reason),
}

impl State {
  pub fn describe(self) -> String {
    match self {
      State::Connected => "Connected".to_string(),
      State::Disconnected(r) => format!("Disconnected: {}", r.as_str()),
    }
  }
}

/// The interface as the supervisor observed it this poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interface {
  /// No configuration applied: only under the tolerate switch, after a boot that got none.
  Unconfigured(Reason),
  /// A configuration is applied; `endpoint_ok` is false after the endpoint command failed.
  Configured { endpoint_ok: bool },
}

/// What the app answered, with every failure and timeout already folded into `Ok` by the
/// consent client (spec 6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
  Ok,
  Hold,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Repair {
  None,
  /// Under the switch with no configuration: fetch one (5.6 step 1).
  Obtain,
  /// Re-set the endpoint, re-resolving the hub's name; the interface keeps running.
  ResetEndpoint,
  /// The interface down and up again with the applied configuration.
  DownUp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GiveUp {
  /// Continuous Disconnected seconds, for `gave up after <n> s`.
  pub elapsed: u64,
  /// `WG_HOLD_LIMIT_SECS` ran out; proceeded without asking again.
  pub hold_limit: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
  pub state: State,
  /// The state differs from the previous poll's (or this is the first poll): log it.
  pub changed: bool,
  pub repair: Repair,
  /// Fetched mode, Disconnected, a configuration applied: start a version check if none is in flight.
  pub check_version: bool,
  /// Start a consent ask with `wireguard-disconnected <n>s`, `n` being the seconds carried here.
  pub ask: Option<u64>,
  pub give_up: Option<GiveUp>,
}

/// Seconds between asks, measured from the previous reply or timeout (6.3).
pub const ASK_INTERVAL_SECS: u64 = 60;

#[derive(Debug)]
struct Pending {
  first_ask: u64,
  outstanding: bool,
  next_ask: u64,
}

#[derive(Debug)]
pub struct Health {
  tolerate: bool,
  fetched: bool,
  limit_secs: u64,
  hold_limit_secs: u64,
  last_state: Option<State>,
  disconnected_since: Option<u64>,
  next_repair: Repair,
  pending: Option<Pending>,
}

impl Health {
  pub fn new(tolerate: bool, fetched: bool, limit_secs: u64, hold_limit_secs: u64) -> Health {
    Health {
      tolerate,
      fetched,
      limit_secs,
      hold_limit_secs,
      last_state: None,
      disconnected_since: None,
      next_repair: Repair::ResetEndpoint,
      pending: None,
    }
  }

  /// One poll. `now` is any monotonic clock in seconds; `fresh` is a handshake younger than
  /// `WG_STALE_SECS`; `reply` is a consent reply that arrived since the previous poll.
  pub fn poll(&mut self, now: u64, iface: Interface, fresh: bool, reply: Option<Reply>) -> Decision {
    let state = if fresh {
      State::Connected
    } else {
      State::Disconnected(match iface {
        Interface::Unconfigured(r) => r,
        Interface::Configured { endpoint_ok: false } => Reason::EndpointUnresolvable,
        Interface::Configured { endpoint_ok: true } => Reason::NoHandshake,
      })
    };
    let changed = self.last_state != Some(state);
    self.last_state = Some(state);
    let mut d = Decision {
      state,
      changed,
      repair: Repair::None,
      check_version: false,
      ask: None,
      give_up: None,
    };
    if fresh {
      // Connected resets the clock, cancels a pending shutdown and restarts the alternation.
      self.disconnected_since = None;
      self.pending = None;
      self.next_repair = Repair::ResetEndpoint;
      return d;
    }
    let since = *self.disconnected_since.get_or_insert(now);
    let elapsed = now.saturating_sub(since);
    match iface {
      Interface::Unconfigured(_) => d.repair = Repair::Obtain,
      Interface::Configured { .. } => {
        d.repair = self.next_repair;
        self.next_repair = match self.next_repair {
          Repair::ResetEndpoint => Repair::DownUp,
          _ => Repair::ResetEndpoint,
        };
        d.check_version = self.fetched;
      }
    }
    if self.tolerate || elapsed < self.limit_secs {
      return d;
    }
    match &mut self.pending {
      None => {
        self.pending = Some(Pending {
          first_ask: now,
          outstanding: true,
          next_ask: now,
        });
        d.ask = Some(elapsed);
      }
      Some(p) => {
        if let Some(r) = reply {
          p.outstanding = false;
          match r {
            Reply::Ok => d.give_up = Some(GiveUp { elapsed, hold_limit: false }),
            Reply::Hold => p.next_ask = now + ASK_INTERVAL_SECS,
          }
        }
        if d.give_up.is_none() && self.hold_limit_secs > 0 && now.saturating_sub(p.first_ask) >= self.hold_limit_secs {
          d.give_up = Some(GiveUp { elapsed, hold_limit: true });
        } else if d.give_up.is_none() && !p.outstanding && now >= p.next_ask {
          p.outstanding = true;
          d.ask = Some(elapsed);
        }
      }
    }
    d
  }

  /// A configuration was applied by a repair or a re-apply: the alternation starts over (5.6).
  pub fn config_applied(&mut self) {
    self.next_repair = Repair::ResetEndpoint;
  }
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test health`
Expected: all eight pass. If the ask test fails on a number, check the arithmetic by hand: the
clock starts at the first Disconnected poll (10), so 1810 is 1800 in, and a reply at 1870 sets
the next ask at 1930.

- [ ] **Step 6: Lint and commit**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings`

```bash
git add src/main.rs src/health.rs
git commit -m "feat(health): the tunnel state machine with the alternating repair and the consent asks

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 5: `cache.rs` and `logfile.rs`, the two files under `persisted_data`

**Files:**

- Create: `src/cache.rs`, `src/logfile.rs`
- Modify: `src/main.rs` (module list)
- Test: unit tests inside each file

**Interfaces:**

- Consumes: `prepare::NONROOT`, `heartbeat::logs_dir`.
- Produces: `cache::DIR = "persisted_data/wireguard"`, `cache::FILE = "peers.toml"`, `cache::dir(app_root) -> PathBuf`, `cache::path(app_root) -> PathBuf`, `cache::write(app_root, text) -> Result<()>`, `cache::read(app_root) -> Result<String>`; `logfile::FILE = "devkit-container.log"`, `logfile::Log::new(app_root) -> Log`, `Log::path() -> &Path`, `Log::line(&self, msg: &str)`.

- [ ] **Step 1: Declare the modules**

In `src/main.rs`, after the `health` line:

```rust
#[cfg_attr(not(unix), allow(dead_code))] // written by the supervisor (Linux)
mod cache;
#[cfg_attr(not(unix), allow(dead_code))] // written by run and the supervisor (Linux)
mod logfile;
```

- [ ] **Step 2: Write the failing tests**

Create `src/cache.rs`:

```rust
//! The cached bundle (spec 4.4): the last bundle a fetch validated, in the entrypoint's own
//! folder under `persisted_data`, so a boot can reach Connected with GitHub down. Best effort at
//! every call site and never a health signal; the folder is `prepare`'s to create.
#![allow(dead_code)] // until run and the supervisor use it (task 11)

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn write_is_atomic_world_readable_and_read_back() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("persisted_data").join("wireguard")).unwrap();
    write(dir.path(), "schema = 1\n").unwrap();
    assert_eq!(read(dir.path()).unwrap(), "schema = 1\n");
    assert_eq!(path(dir.path()), dir.path().join("persisted_data").join("wireguard").join("peers.toml"));
    assert!(!dir.path().join("persisted_data").join("wireguard").join("peers.toml.tmp").exists());
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt as _;
      assert_eq!(std::fs::metadata(path(dir.path())).unwrap().permissions().mode() & 0o777, 0o644);
    }
    write(dir.path(), "schema = 1\n# second\n").unwrap();
    assert!(read(dir.path()).unwrap().contains("second"));
  }

  #[test]
  fn a_missing_folder_or_file_is_an_error_naming_the_path() {
    let dir = tempfile::tempdir().unwrap();
    let err = write(dir.path(), "x").unwrap_err().to_string();
    assert!(err.contains("peers.toml.tmp"), "{err}");
    let err = read(dir.path()).unwrap_err().to_string();
    assert!(err.contains("peers.toml"), "{err}");
  }
}
```

Create `src/logfile.rs`:

```rust
//! The placeholder log file (spec 5.8): every line the binary writes for itself goes to stderr
//! as before and is appended, with a timestamp, to `persisted_data/logs/devkit-container.log`.
//! Dumb on purpose: open, append one line, close; nothing buffered, rotated or capped; a failed
//! write changes nothing. Hooking the binary into aeth_ext's logging is later work (todo.md).
#![allow(dead_code)] // until run and the supervisor use it (task 11)

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn each_line_is_appended_with_a_timestamp_and_the_file_is_world_readable() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(crate::heartbeat::logs_dir(dir.path())).unwrap();
    let log = Log::new(dir.path());
    log.line("first");
    log.line("second: with details");
    let text = std::fs::read_to_string(log.path()).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    for (line, msg) in lines.iter().zip(["first", "second: with details"]) {
      let (stamp, rest) = line.split_once(' ').unwrap();
      assert!(stamp.parse::<jiff::Timestamp>().is_ok(), "{stamp} is not a timestamp");
      assert_eq!(rest, msg);
    }
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt as _;
      assert_eq!(std::fs::metadata(log.path()).unwrap().permissions().mode() & 0o777, 0o644);
    }
  }

  #[test]
  fn a_missing_folder_loses_the_line_quietly() {
    let dir = tempfile::tempdir().unwrap();
    let log = Log::new(dir.path());
    log.line("nowhere to go");
    assert!(!log.path().exists());
  }
}
```

- [ ] **Step 3: Run the tests to see them fail**

Run: `cargo test cache && cargo test logfile`
Expected: compile errors.

- [ ] **Step 4: Implement**

In `src/cache.rs`, between the allow line and the tests:

```rust
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

pub const DIR: &str = "persisted_data/wireguard";
pub const FILE: &str = "peers.toml";

pub fn dir(app_root: &Path) -> PathBuf {
  app_root.join("persisted_data").join("wireguard")
}

pub fn path(app_root: &Path) -> PathBuf {
  dir(app_root).join(FILE)
}

/// Atomic (a sibling temp file renamed into place), world-readable, handed to nonroot on Linux
/// so the folder stays uniformly owned. The chown is best effort: a non-root test cannot do it,
/// and `prepare` re-chowns the whole tree at the next boot anyway.
pub fn write(app_root: &Path, text: &str) -> Result<()> {
  let path = path(app_root);
  let tmp = path.with_extension("toml.tmp");
  std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644)).with_context(|| format!("chmod {}", tmp.display()))?;
    let _ = std::os::unix::fs::lchown(&tmp, Some(crate::prepare::NONROOT), Some(crate::prepare::NONROOT));
  }
  std::fs::rename(&tmp, &path).with_context(|| format!("renaming {} to {}", tmp.display(), path.display()))
}

pub fn read(app_root: &Path) -> Result<String> {
  let path = path(app_root);
  std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))
}
```

In `src/logfile.rs`:

```rust
use std::io::Write as _;
use std::path::{Path, PathBuf};

pub const FILE: &str = "devkit-container.log";

#[derive(Debug, Clone)]
pub struct Log {
  path: PathBuf,
}

impl Log {
  pub fn new(app_root: &Path) -> Log {
    Log {
      path: crate::heartbeat::logs_dir(app_root).join(FILE),
    }
  }

  pub fn path(&self) -> &Path {
    &self.path
  }

  /// `devkit-container: <msg>` to stderr, then `<timestamp> <msg>` appended to the file. The
  /// file write is best effort: before `prepare` the folder may not exist yet, and nothing here
  /// may ever fail the run.
  pub fn line(&self, msg: &str) {
    eprintln!("devkit-container: {msg}");
    let _ = self.append(msg);
  }

  fn append(&self, msg: &str) -> std::io::Result<()> {
    let existed = self.path.exists();
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&self.path)?;
    writeln!(file, "{} {msg}", jiff::Timestamp::now())?;
    #[cfg(unix)]
    if !existed {
      use std::os::unix::fs::PermissionsExt as _;
      let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o644));
      let _ = std::os::unix::fs::lchown(&self.path, Some(crate::prepare::NONROOT), Some(crate::prepare::NONROOT));
    }
    Ok(())
  }
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test cache && cargo test logfile`
Expected: four tests pass.

- [ ] **Step 6: Lint and commit**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings`

```bash
git add src/main.rs src/cache.rs src/logfile.rs
git commit -m "feat(cache,logfile): the cached bundle file and the placeholder log file

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 6: `consent.rs`, the supervisor's client (Linux)

**Files:**

- Create: `src/consent.rs`
- Modify: `src/main.rs` (module list)
- Test: unit tests inside `src/consent.rs` (Linux only; on Windows the module does not compile in)

**Interfaces:**

- Consumes: `health::Reply`.
- Produces: `consent::DIR = "/run/devkit"`, `consent::SOCKET = "/run/devkit/consent.sock"`, `consent::TIMEOUT = 60 s`, `consent::ask(path: &Path, reason: &str, timeout: Duration) -> Reply`.

- [ ] **Step 1: Declare the module**

In `src/main.rs`, after the `logfile` line:

```rust
#[cfg(unix)]
mod consent;
```

- [ ] **Step 2: Write the failing tests**

Create `src/consent.rs`:

```rust
//! The supervisor's consent client (spec 6.2): one request per connection to the app's Unix
//! socket, one reply line back. Everything that is not a literal `hold` within the timeout is
//! consent: a missing socket, a refused connection, an error, an empty stream, any other line.
#![allow(dead_code)] // until the supervisor uses it (task 11)

#[cfg(test)]
mod tests {
  use std::os::unix::net::UnixListener;
  use std::sync::mpsc;
  use std::time::Duration;

  use super::*;

  enum Server {
    Reply(&'static str),
    CloseAtOnce,
    Silent(Duration),
  }

  /// One connection served on a fresh socket; the request line is sent back through the channel.
  fn serve(dir: &std::path::Path, behaviour: Server) -> (std::path::PathBuf, mpsc::Receiver<String>) {
    let path = dir.join("consent.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
      use std::io::{BufRead as _, BufReader, Write as _};
      let (stream, _) = listener.accept().unwrap();
      let mut reader = BufReader::new(&stream);
      let mut line = String::new();
      reader.read_line(&mut line).unwrap();
      tx.send(line.trim_end().to_string()).unwrap();
      match behaviour {
        Server::Reply(text) => {
          (&stream).write_all(text.as_bytes()).unwrap();
        }
        Server::CloseAtOnce => {}
        Server::Silent(d) => std::thread::sleep(d),
      }
    });
    (path, rx)
  }

  #[test]
  fn only_a_literal_hold_holds() {
    let dir = tempfile::tempdir().unwrap();
    for (behaviour, want) in [
      (Server::Reply("hold\n"), Reply::Hold),
      (Server::Reply("ok\n"), Reply::Ok),
      (Server::Reply("HOLD\n"), Reply::Ok),
      (Server::Reply("hold please\n"), Reply::Ok),
      (Server::Reply("garbage"), Reply::Ok),
      (Server::CloseAtOnce, Reply::Ok),
      (Server::Silent(Duration::from_millis(600)), Reply::Ok),
    ] {
      let sub = tempfile::tempdir_in(dir.path()).unwrap();
      let (path, rx) = serve(sub.path(), behaviour);
      let got = ask(&path, "wireguard-disconnected 1800s", Duration::from_millis(200));
      assert_eq!(got, want);
      assert_eq!(rx.recv().unwrap(), "may-shutdown wireguard-disconnected 1800s");
    }
  }

  #[test]
  fn no_socket_and_a_refused_connection_are_consent() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(ask(&dir.path().join("absent.sock"), "x", Duration::from_millis(200)), Reply::Ok);
    let path = dir.path().join("dead.sock");
    drop(UnixListener::bind(&path).unwrap());
    assert!(path.exists(), "the socket file stays behind, with nobody listening");
    assert_eq!(ask(&path, "x", Duration::from_millis(200)), Reply::Ok);
  }
}
```

- [ ] **Step 3: Run the tests to see them fail**

Run: `cargo test consent`
Expected: compile errors (`ask`, `Reply` not found).

- [ ] **Step 4: Implement**

Between the allow line and the tests:

```rust
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use crate::health::Reply;

pub const DIR: &str = "/run/devkit";
pub const SOCKET: &str = "/run/devkit/consent.sock";
/// No reply within this long is consent (6.2).
pub const TIMEOUT: Duration = Duration::from_secs(60);

/// `may-shutdown <reason>`; `Hold` only for a literal `hold` line back within `timeout`.
pub fn ask(path: &Path, reason: &str, timeout: Duration) -> Reply {
  match reply_line(path, reason, timeout).as_deref() {
    Some("hold") => Reply::Hold,
    _ => Reply::Ok,
  }
}

fn reply_line(path: &Path, reason: &str, timeout: Duration) -> Option<String> {
  let mut stream = UnixStream::connect(path).ok()?;
  stream.set_read_timeout(Some(timeout)).ok()?;
  stream.set_write_timeout(Some(timeout)).ok()?;
  stream.write_all(format!("may-shutdown {reason}\n").as_bytes()).ok()?;
  // Bounded: a reply longer than this is not a protocol reply.
  let mut reader = BufReader::new(stream).take(256);
  let mut line = String::new();
  reader.read_line(&mut line).ok()?;
  let line = line.trim_end_matches(['\r', '\n']).to_string();
  (!line.is_empty()).then_some(line)
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test consent`
Expected: both pass (the silent case takes 200 ms).

- [ ] **Step 6: Lint and commit**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings`

```bash
git add src/main.rs src/consent.rs
git commit -m "feat(consent): the supervisor's shutdown-consent client

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 7: `fetch.rs`, the version endpoint and the GitHub fetch (Linux)

**Files:**

- Modify: `Cargo.toml` (`serde_json` as a Linux runtime dependency)
- Create: `src/fetch.rs`
- Modify: `src/main.rs` (module list)
- Test: unit tests inside `src/fetch.rs` against an in-process listener (Linux only)

**Interfaces:**

- Consumes: `bundle::{parse, require_version, select, validate_tag, Effective}`, `ping::agent()`.
- Produces: `fetch::Hosts { api, web }` with `Default` (the real GitHub hosts); `fetch::Fetcher::new(hub_url, repo, token: Option<String>, hosts) -> Fetcher` (`Clone`); `Fetcher::version(&self) -> Result<String>`; `Fetcher::bundle(&self, tag) -> Result<String>`; `fetch::Outcome::{Unchanged(String), New { tag, text, effective }, NotEnrolled { tag, text }, Unavailable(String)}`; `fetch::obtain(&Fetcher, public_key: &str, applied_tag: Option<&str>) -> Outcome`.

- [ ] **Step 1: Add the dependency and declare the module**

In `Cargo.toml`, inside `[target.'cfg(unix)'.dependencies]`, add after the `ureq` line:

```toml
  serde_json  = "1.0.151"
```

Run `cargo check` once: the lock already holds 1.0.151 from the dev-dependency, so nothing new
is fetched.

In `src/main.rs`, after the `consent` line:

```rust
#[cfg(unix)]
mod fetch;
```

- [ ] **Step 2: Write the failing tests**

Create `src/fetch.rs`:

```rust
//! Linux only: the version endpoint (spec 4.1) and the bundle fetch from the hub's GitHub
//! release (4.2) over `ureq`, and `obtain`, the version → bundle → entry sequence the boot and
//! the worker thread run. Errors name the step and the HTTP status, never the token or a body.
//! The two GitHub hosts are ordinary inputs, so the tests point them at a local listener.
#![allow(dead_code)] // until run and the supervisor use it (task 11)

#[cfg(test)]
mod tests {
  use std::io::{Read as _, Write as _};
  use std::net::TcpListener;
  use std::sync::{Arc, Mutex};

  use super::*;

  #[derive(Debug, Clone)]
  struct Route {
    path: String,
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
  }

  fn route(path: &str, status: u16, headers: &[(&str, &str)], body: &str) -> Route {
    Route {
      path: path.into(),
      status,
      headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
      body: body.into(),
    }
  }

  type Request = (String, Vec<(String, String)>);

  /// A one-thread HTTP/1.1 listener on localhost answering canned routes and recording every
  /// request's path and headers. `{base}` in a header value becomes the listener's base URL.
  fn serve(routes: Vec<Route>) -> (String, Arc<Mutex<Vec<Request>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen: Arc<Mutex<Vec<Request>>> = Arc::default();
    let record = Arc::clone(&seen);
    let routes: Vec<Route> = routes
      .into_iter()
      .map(|mut r| {
        for (_, v) in &mut r.headers {
          *v = v.replace("{base}", &base);
        }
        r
      })
      .collect();
    std::thread::spawn(move || {
      for stream in listener.incoming() {
        let Ok(mut stream) = stream else { break };
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        while !buf.ends_with(b"\r\n\r\n") && stream.read(&mut byte).is_ok_and(|n| n == 1) {
          buf.push(byte[0]);
        }
        let text = String::from_utf8_lossy(&buf).into_owned();
        let mut lines = text.lines();
        let path = lines.next().and_then(|l| l.split_whitespace().nth(1)).unwrap_or("").to_string();
        let headers: Vec<(String, String)> = lines
          .filter_map(|l| l.split_once(':'))
          .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
          .collect();
        record.lock().unwrap().push((path.clone(), headers));
        let (status, extra, body) = match routes.iter().find(|r| r.path == path) {
          Some(r) => (r.status, r.headers.clone(), r.body.clone()),
          None => (404, vec![], "not found".to_string()),
        };
        let mut response = format!("HTTP/1.1 {status} X\r\nConnection: close\r\nContent-Length: {}\r\n", body.len());
        for (k, v) in extra {
          response.push_str(&format!("{k}: {v}\r\n"));
        }
        response.push_str("\r\n");
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.write_all(body.as_bytes());
        let _ = stream.shutdown(std::net::Shutdown::Both);
      }
    });
    (base, seen)
  }

  fn header<'a>(req: &'a Request, name: &str) -> Option<&'a str> {
    req.1.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
  }

  fn key(c: char) -> String {
    format!("{}=", c.to_string().repeat(43))
  }

  fn bundle_text(tag: &str, spoke: char) -> String {
    format!(
      "hub_version = \"{tag}\"\nschema = 1\n\n[hub]\nname = \"wireguard-hub\"\npublic_key = \"{}\"\naddress = \"10.8.0.1/24\"\nlisten_port = 51820\nendpoint = \"wireguard-hub:51820\"\nallowed_ips = [\"10.8.0.0/24\"]\npersistent_keepalive = 25\n\n[[peers]]\nname = \"smoke-spoke\"\npublic_key = \"{}\"\naddress = \"10.8.0.20/32\"\n",
      key('A'),
      key(spoke)
    )
  }

  fn github_routes(tag: &str, spoke: char) -> Vec<Route> {
    vec![
      route("/version", 200, &[], &format!("{tag}\n")),
      route(
        &format!("/repos/o/r/releases/tags/{tag}"),
        200,
        &[("Content-Type", "application/json")],
        &format!(
          r#"{{"tag_name":"{tag}","assets":[{{"name":"other.conf","url":"{{base}}/assets/1"}},{{"name":"peers.toml","url":"{{base}}/assets/2"}}]}}"#
        ),
      ),
      route("/assets/2", 302, &[("Location", "{base}/download/peers.toml")], ""),
      route("/download/peers.toml", 200, &[], &bundle_text(tag, spoke)),
      route(&format!("/o/r/releases/download/{tag}/peers.toml"), 200, &[], &bundle_text(tag, spoke)),
    ]
  }

  fn fetcher(base: &str, token: Option<&str>) -> Fetcher {
    Fetcher::new(
      base.to_string(),
      "o/r".into(),
      token.map(str::to_string),
      Hosts {
        api: base.to_string(),
        web: base.to_string(),
      },
    )
  }

  #[test]
  fn the_version_is_a_strict_tag_and_failures_name_the_step() {
    let (base, _) = serve(vec![
      route("/version", 200, &[], "v0.1.0\n"),
    ]);
    assert_eq!(fetcher(&base, None).version().unwrap(), "v0.1.0");
    let (base, _) = serve(vec![route("/version", 200, &[], "<html>welcome to the wrong server, this is a long page</html>")]);
    let err = fetcher(&base, None).version().unwrap_err().to_string();
    assert!(err.contains("version endpoint") && err.contains("not a tag") && !err.contains("</html>"), "{err}");
    let (base, _) = serve(vec![route("/version", 503, &[], "")]);
    let err = fetcher(&base, None).version().unwrap_err().to_string();
    assert!(err.contains("version endpoint") && err.contains("503"), "{err}");
  }

  #[test]
  fn the_token_goes_to_the_api_host_only() {
    let (base, seen) = serve(github_routes("v0.1.0", 'B'));
    let text = fetcher(&base, Some("tok")).bundle("v0.1.0").unwrap();
    assert!(text.contains("hub_version = \"v0.1.0\""));
    let seen = seen.lock().unwrap();
    let paths: Vec<&str> = seen.iter().map(|r| r.0.as_str()).collect();
    assert_eq!(paths, ["/repos/o/r/releases/tags/v0.1.0", "/assets/2", "/download/peers.toml"]);
    let listing = &seen[0];
    assert_eq!(header(listing, "authorization"), Some("Bearer tok"));
    assert_eq!(header(listing, "accept"), Some("application/vnd.github+json"));
    assert_eq!(header(listing, "x-github-api-version"), Some("2022-11-28"));
    assert!(header(listing, "user-agent").unwrap().starts_with("devkit-container/"));
    let asset = &seen[1];
    assert_eq!(header(asset, "authorization"), Some("Bearer tok"));
    assert_eq!(header(asset, "accept"), Some("application/octet-stream"));
    let download = &seen[2];
    assert_eq!(header(download, "authorization"), None, "the token never leaves the API host");
    assert_eq!(header(download, "accept"), Some("application/octet-stream"));
  }

  #[test]
  fn without_a_token_the_public_download_is_used() {
    let (base, seen) = serve(github_routes("v0.1.0", 'B'));
    fetcher(&base, None).bundle("v0.1.0").unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0].0, "/o/r/releases/download/v0.1.0/peers.toml");
    assert_eq!(header(&seen[0], "authorization"), None);
  }

  #[test]
  fn each_step_fails_with_its_name_and_never_the_token() {
    let missing = vec![
      route("/repos/o/r/releases/tags/v0.1.0", 200, &[], r#"{"assets":[{"name":"x.conf","url":"u"}]}"#),
    ];
    let (base, _) = serve(missing);
    let err = fetcher(&base, Some("tok")).bundle("v0.1.0").unwrap_err().to_string();
    assert!(err.contains("no peers.toml asset") && err.contains("v0.1.0") && !err.contains("tok"), "{err}");
    let (base, _) = serve(vec![]);
    let err = fetcher(&base, Some("tok")).bundle("v0.1.0").unwrap_err().to_string();
    assert!(err.contains("release listing") && err.contains("404"), "{err}");
    let mut no_redirect = github_routes("v0.1.0", 'B');
    no_redirect[2] = route("/assets/2", 200, &[], "peers");
    let (base, _) = serve(no_redirect);
    let err = fetcher(&base, Some("tok")).bundle("v0.1.0").unwrap_err().to_string();
    assert!(err.contains("expected 302"), "{err}");
    let mut huge = github_routes("v0.1.0", 'B');
    huge[3] = route("/download/peers.toml", 200, &[], &"#".repeat(1024 * 1024 + 1));
    let (base, _) = serve(huge);
    assert!(fetcher(&base, Some("tok")).bundle("v0.1.0").is_err(), "a body over 1 MiB is refused");
  }

  #[test]
  fn obtain_reports_unchanged_new_not_enrolled_and_unavailable() {
    let (base, _) = serve(github_routes("v0.1.0", 'B'));
    let f = fetcher(&base, Some("tok"));
    assert_eq!(obtain(&f, &key('B'), Some("v0.1.0")), Outcome::Unchanged("v0.1.0".into()));
    match obtain(&f, &key('B'), Some("v0.0.9")) {
      Outcome::New { tag, text, effective } => {
        assert_eq!(tag, "v0.1.0");
        assert!(text.contains("smoke-spoke"));
        assert_eq!(effective.address, "10.8.0.20/32");
        assert_eq!(effective.endpoint, "wireguard-hub:51820");
      }
      other => panic!("{other:?}"),
    }
    match obtain(&f, &key('Z'), None) {
      Outcome::NotEnrolled { tag, .. } => assert_eq!(tag, "v0.1.0"),
      other => panic!("{other:?}"),
    }
    let mut stale = github_routes("v0.2.0", 'B');
    stale[3] = route("/download/peers.toml", 200, &[], &bundle_text("v0.1.0", 'B'));
    let (base, _) = serve(stale);
    match obtain(&fetcher(&base, Some("tok")), &key('B'), None) {
      Outcome::Unavailable(msg) => assert!(msg.contains("v0.1.0") && msg.contains("v0.2.0"), "{msg}"),
      other => panic!("{other:?}"),
    }
    let (base, _) = serve(vec![]);
    match obtain(&fetcher(&base, Some("tok")), &key('B'), None) {
      Outcome::Unavailable(msg) => assert!(msg.contains("version endpoint"), "{msg}"),
      other => panic!("{other:?}"),
    }
  }
}
```

- [ ] **Step 3: Run the tests to see them fail**

Run: `cargo test fetch`
Expected: compile errors.

- [ ] **Step 4: Implement**

Between the allow line and the tests:

```rust
use anyhow::{Result, anyhow, bail};

use crate::bundle::{self, Effective};

const API_VERSION: &str = "2022-11-28";
/// The cap on the release listing and on the bundle body (4.2).
const LIMIT: u64 = 1024 * 1024;
const USER_AGENT: &str = concat!("devkit-container/", env!("CARGO_PKG_VERSION"));

/// The two GitHub hosts. Ordinary inputs, so a test can point them at a local listener; there
/// is no environment variable for them.
#[derive(Debug, Clone)]
pub struct Hosts {
  pub api: String,
  pub web: String,
}

impl Default for Hosts {
  fn default() -> Hosts {
    Hosts {
      api: "https://api.github.com".into(),
      web: "https://github.com".into(),
    }
  }
}

#[derive(Clone)]
pub struct Fetcher {
  agent: ureq::Agent,
  hub_url: String,
  repo: String,
  token: Option<String>,
  hosts: Hosts,
}

impl Fetcher {
  pub fn new(hub_url: String, repo: String, token: Option<String>, hosts: Hosts) -> Fetcher {
    Fetcher {
      agent: crate::ping::agent(),
      hub_url,
      repo,
      token,
      hosts,
    }
  }

  /// `GET <hub>/version` (4.1): a strict tag, else an error carrying the status or the body's
  /// first 64 bytes.
  pub fn version(&self) -> Result<String> {
    let url = format!("{}/version", self.hub_url);
    let mut resp = self
      .agent
      .get(&url)
      .header("User-Agent", USER_AGENT)
      .call()
      .map_err(|e| anyhow!("version endpoint: {}", describe(&e)))?;
    let body = resp
      .body_mut()
      .with_config()
      .limit(1024)
      .read_to_string()
      .map_err(|e| anyhow!("version endpoint: reading the body: {}", describe(&e)))?;
    let text = body.strip_suffix('\n').unwrap_or(&body);
    bundle::validate_tag(text).map_err(|_| anyhow!("version endpoint: the body is not a tag: {:?}", truncate(text, 64)))
  }

  /// The bundle's text at `tag` (4.2), capped at 1 MiB.
  pub fn bundle(&self, tag: &str) -> Result<String> {
    let Some(token) = &self.token else {
      return self.public(tag);
    };
    let bearer = format!("Bearer {token}");
    let listing_url = format!("{}/repos/{}/releases/tags/{tag}", self.hosts.api, self.repo);
    let mut resp = self
      .agent
      .get(&listing_url)
      .header("Authorization", &bearer)
      .header("Accept", "application/vnd.github+json")
      .header("X-GitHub-Api-Version", API_VERSION)
      .header("User-Agent", USER_AGENT)
      .call()
      .map_err(|e| anyhow!("release listing for {tag}: {}", describe(&e)))?;
    let listing = resp
      .body_mut()
      .with_config()
      .limit(LIMIT)
      .read_to_string()
      .map_err(|e| anyhow!("release listing for {tag}: reading the body: {}", describe(&e)))?;
    let json: serde_json::Value = serde_json::from_str(&listing).map_err(|_| anyhow!("release listing for {tag}: the body is not JSON"))?;
    let asset_url = json
      .get("assets")
      .and_then(|a| a.as_array())
      .and_then(|assets| assets.iter().find(|a| a.get("name").and_then(|n| n.as_str()) == Some("peers.toml")))
      .and_then(|a| a.get("url"))
      .and_then(|u| u.as_str())
      .ok_or_else(|| anyhow!("release {tag} has no peers.toml asset"))?
      .to_string();
    // Redirects off: the 302 comes back as a response, and its Location is fetched without the
    // token (the client would strip it on a followed redirect too; this keeps it testable).
    let resp = self
      .agent
      .get(&asset_url)
      .header("Authorization", &bearer)
      .header("Accept", "application/octet-stream")
      .header("X-GitHub-Api-Version", API_VERSION)
      .header("User-Agent", USER_AGENT)
      .config()
      .max_redirects(0)
      .build()
      .call()
      .map_err(|e| anyhow!("asset request for {tag}: {}", describe(&e)))?;
    if resp.status() != 302 {
      bail!("asset request for {tag}: expected 302, got {}", resp.status());
    }
    let location = resp
      .headers()
      .get("location")
      .and_then(|v| v.to_str().ok())
      .ok_or_else(|| anyhow!("asset request for {tag}: 302 without a Location"))?
      .to_string();
    let mut resp = self
      .agent
      .get(&location)
      .header("Accept", "application/octet-stream")
      .header("User-Agent", USER_AGENT)
      .call()
      .map_err(|e| anyhow!("asset download for {tag}: {}", describe(&e)))?;
    resp
      .body_mut()
      .with_config()
      .limit(LIMIT)
      .read_to_string()
      .map_err(|e| anyhow!("asset download for {tag}: reading the body: {}", describe(&e)))
  }

  /// The public-repo path: supported, not the decided configuration (4.2).
  fn public(&self, tag: &str) -> Result<String> {
    let url = format!("{}/{}/releases/download/{tag}/peers.toml", self.hosts.web, self.repo);
    let mut resp = self
      .agent
      .get(&url)
      .header("User-Agent", USER_AGENT)
      .call()
      .map_err(|e| anyhow!("public download for {tag}: {}", describe(&e)))?;
    resp
      .body_mut()
      .with_config()
      .limit(LIMIT)
      .read_to_string()
      .map_err(|e| anyhow!("public download for {tag}: reading the body: {}", describe(&e)))
  }
}

/// ureq's error text names hosts, statuses and timeouts, never a URL or a header value.
fn describe(e: &ureq::Error) -> String {
  match e {
    ureq::Error::StatusCode(code) => format!("HTTP {code}"),
    other => other.to_string(),
  }
}

fn truncate(s: &str, n: usize) -> &str {
  match s.char_indices().nth(n) {
    Some((i, _)) => &s[..i],
    None => s,
  }
}

/// The result of one version → bundle → entry attempt (4.1 to 4.3).
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
  /// The hub reports the tag that is already applied.
  Unchanged(String),
  /// A valid bundle at another tag, with this spoke's entry.
  New { tag: String, text: String, effective: Effective },
  /// A valid bundle at another tag, without this spoke's entry.
  NotEnrolled { tag: String, text: String },
  /// The version endpoint, the fetch or the validation failed; the message names the step.
  Unavailable(String),
}

pub fn obtain(f: &Fetcher, public_key: &str, applied_tag: Option<&str>) -> Outcome {
  let tag = match f.version() {
    Ok(t) => t,
    Err(e) => return Outcome::Unavailable(format!("{e:#}")),
  };
  if applied_tag == Some(tag.as_str()) {
    return Outcome::Unchanged(tag);
  }
  let text = match f.bundle(&tag) {
    Ok(t) => t,
    Err(e) => return Outcome::Unavailable(format!("{e:#}")),
  };
  let parsed = match bundle::parse(&text).and_then(|b| bundle::require_version(&b, &tag).map(|()| b)) {
    Ok(b) => b,
    Err(e) => return Outcome::Unavailable(format!("bundle {tag}: {e:#}")),
  };
  match bundle::select(&parsed, public_key) {
    Some(effective) => Outcome::New { tag, text, effective },
    None => Outcome::NotEnrolled { tag, text },
  }
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test fetch`
Expected: five tests pass. The listener answers from a thread, so a test that hangs means the
request never arrived: check the route path against what the fetcher builds.

- [ ] **Step 6: Lint and commit**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings`

```bash
git add Cargo.toml Cargo.lock src/main.rs src/fetch.rs
git commit -m "feat(fetch): the version endpoint and the release-asset fetch, token to the API host only

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 8: `[tool.docker].startup_scripts` and `scrub_env`, and the script runner

**Files:**

- Modify: `src/pyproject.rs`, `src/run.rs`
- Test: unit tests in both files (the runner's test is Linux only, no root needed)

**Interfaces:**

- Produces: `pyproject::startup_scripts(&DocumentMut) -> Result<Vec<String>>`; `pyproject::scrub_env(&DocumentMut) -> Result<Vec<String>>`; `run::run_startup_scripts(app_root: &Path, names: &[String]) -> Result<()>`.

- [ ] **Step 1: Write the failing tests**

In `src/pyproject.rs`'s `tests` module, add:

```rust
  #[test]
  fn startup_scripts_must_be_project_scripts_and_scrub_env_is_a_name_list() {
    let d = doc(
      "[project.scripts]
run-app-x = \"m:main\"
hub-up = \"m:up\"
[tool.docker]
startup_scripts = [\"hub-up\"]
scrub_env = [\"WG_HUB_PRIVATE_KEY\", \"OTHER\"]
",
    );
    assert_eq!(startup_scripts(&d).unwrap(), ["hub-up"]);
    assert_eq!(scrub_env(&d).unwrap(), ["WG_HUB_PRIVATE_KEY", "OTHER"]);
    let none = doc("[project.scripts]\nrun-app-x = \"m:main\"\n");
    assert!(startup_scripts(&none).unwrap().is_empty() && scrub_env(&none).unwrap().is_empty());
    let typo = doc("[project.scripts]\nrun-app-x = \"m:main\"\n[tool.docker]\nstartup_scripts = [\"hub-upp\"]\n");
    let err = startup_scripts(&typo).unwrap_err().to_string();
    assert!(err.contains("hub-upp") && err.contains("[project.scripts]"), "{err}");
    let bad = doc("[tool.docker]\nscrub_env = \"WG_HUB_PRIVATE_KEY\"\n");
    assert!(scrub_env(&bad).unwrap_err().to_string().contains("list of strings"));
    let bad = doc("[tool.docker]\nstartup_scripts = [1]\n");
    assert!(startup_scripts(&bad).unwrap_err().to_string().contains("list of strings"));
  }
```

In `src/run.rs`, add at the bottom:

```rust
#[cfg(test)]
mod tests {
  use std::os::unix::fs::PermissionsExt as _;

  use super::*;

  fn script(root: &std::path::Path, name: &str, body: &str) {
    let dir = root.join(".venv").join("bin");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
  }

  #[test]
  fn startup_scripts_run_in_order_in_the_app_root_with_the_environment_and_stop_at_the_first_failure() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    script(&root, "first", "pwd >> log.txt; echo \"$CARGO_MANIFEST_DIR\" >> log.txt; echo first $# >> log.txt");
    script(&root, "second", "echo second >> log.txt");
    run_startup_scripts(&root, &["first".into(), "second".into()]).unwrap();
    let log = std::fs::read_to_string(root.join("log.txt")).unwrap();
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines[0], root.to_string_lossy(), "working directory is the app root");
    assert_eq!(lines[1], env!("CARGO_MANIFEST_DIR"), "the full environment is inherited");
    assert_eq!(&lines[2..], ["first 0", "second"], "in order, no arguments");
    script(&root, "third", "exit 3");
    script(&root, "fourth", "echo fourth >> log.txt");
    let err = run_startup_scripts(&root, &["third".into(), "fourth".into()]).unwrap_err().to_string();
    assert_eq!(err, "startup script third exited 3");
    assert!(!std::fs::read_to_string(root.join("log.txt")).unwrap().contains("fourth"), "stopped at the failure");
    let err = run_startup_scripts(&root, &["missing".into()]).unwrap_err().to_string();
    assert!(err.contains("startup script missing"), "{err}");
  }
}
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test pyproject && cargo test run::`
Expected: compile errors, the three functions not found.

- [ ] **Step 3: Implement**

In `src/pyproject.rs`, after `services`:

```rust
/// `[tool.docker].startup_scripts`: console script names run as root before the app (spec 7).
/// Each must be a `[project.scripts]` key, so a typo fails before the tunnel or the mount check.
#[cfg_attr(not(unix), allow(dead_code))]
pub fn startup_scripts(doc: &DocumentMut) -> Result<Vec<String>> {
  let names = string_list(doc, "startup_scripts")?;
  let scripts = doc.get("project").and_then(|p| p.get("scripts")).and_then(|s| s.as_table_like());
  for name in &names {
    if !scripts.is_some_and(|s| s.get(name).is_some()) {
      bail!("[tool.docker].startup_scripts names {name:?}, which is not in [project.scripts]");
    }
  }
  Ok(names)
}

/// `[tool.docker].scrub_env`: variable names removed from the app's environment (spec 7).
#[cfg_attr(not(unix), allow(dead_code))]
pub fn scrub_env(doc: &DocumentMut) -> Result<Vec<String>> {
  string_list(doc, "scrub_env")
}

fn string_list(doc: &DocumentMut, key: &str) -> Result<Vec<String>> {
  let Some(item) = doc.get("tool").and_then(|t| t.get("docker")).and_then(|d| d.get(key)) else {
    return Ok(Vec::new());
  };
  let arr = item
    .as_array()
    .with_context(|| format!("[tool.docker].{key} must be a list of strings"))?;
  arr
    .iter()
    .map(|v| {
      v.as_str()
        .map(str::to_string)
        .with_context(|| format!("[tool.docker].{key} must be a list of strings, got {v}"))
    })
    .collect()
}
```

In `src/run.rs`, after the `run` function (the imports gain `use std::path::Path;` and
`use std::os::unix::process::ExitStatusExt as _;`):

```rust
/// Run each startup script as root, in list order, one at a time, with the full environment,
/// working directory `app_root`, inherited stdio, no arguments, no timeout (spec 7). The first
/// nonzero exit is the error, naming the script and its code or signal.
pub fn run_startup_scripts(app_root: &Path, names: &[String]) -> Result<()> {
  for name in names {
    let exe = app_root.join(".venv").join("bin").join(name);
    let status = std::process::Command::new(&exe)
      .current_dir(app_root)
      .status()
      .with_context(|| format!("startup script {name}: running {}", exe.display()))?;
    if !status.success() {
      match status.code() {
        Some(code) => bail!("startup script {name} exited {code}"),
        None => bail!("startup script {name} was killed by signal {}", status.signal().unwrap_or(0)),
      }
    }
  }
  Ok(())
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test pyproject && cargo test run::`
Expected: pass on Linux; on Windows the `run` test does not exist (the module is Linux only)
and the pyproject test passes.

- [ ] **Step 5: Lint and commit**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings`

```bash
git add src/pyproject.rs src/run.rs
git commit -m "feat(run): read startup_scripts and scrub_env, and run the scripts in order as root

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 9: `wireguard.rs`, the command planner and the Linux `Interface`

The old `Tunnel`, `Config`, `Action` and `Assessor` stay until task 11. The planner is pure; the
`Interface` runs its commands and is exercised by the smoke tests.

**Files:**

- Modify: `src/wireguard.rs`
- Test: unit tests inside `src/wireguard.rs` (the planner, all platforms)

**Interfaces:**

- Consumes: `bundle::Effective`.
- Produces: `wireguard::IFACE = "wg0"`; `wireguard::Cmd { program: &'static str, args: Vec<String>, kind: CmdKind }`; `wireguard::CmdKind::{Local, Endpoint}`; `wireguard::apply_commands(&Effective) -> Vec<Cmd>`; `wireguard::endpoint_command(&Effective) -> Cmd`; `wireguard::reapply_commands(old: &Effective, new: &Effective, endpoint_ok: bool) -> Vec<Cmd>`; on Linux `wireguard::ApplyError::{Local(anyhow::Error), Endpoint(anyhow::Error)}` and `wireguard::Interface` with `create(private_key: &str, preshared_key: Option<String>) -> Result<Interface>` (field `public_key: String`), `apply(&self, &Effective) -> Result<(), ApplyError>`, `set_endpoint(&self, &Effective) -> Result<(), ApplyError>`, `reapply(&self, old, new, endpoint_ok) -> Result<(), ApplyError>`, `down_up(&self, &Effective) -> Result<(), ApplyError>`, `latest_handshake(&self, peer_key: &str) -> Result<Option<u64>>`, `wait_handshake(&self, peer_key: &str, timeout: Duration) -> Result<bool>`, `down(&self)`.

- [ ] **Step 1: Write the failing tests**

In the `tests` module of `src/wireguard.rs`, add:

```rust
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
        "wg set wg0 peer HUBKEY allowed-ips 10.8.0.0/24 persistent-keepalive 25",
        "ip address add 10.8.0.20/32 dev wg0",
        "ip link set up dev wg0",
        "ip route replace 10.8.0.0/24 dev wg0",
        "wg set wg0 peer HUBKEY endpoint tunnels.example.com:51820",
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
        "wg set wg0 peer NEWKEY allowed-ips 10.8.0.0/24 persistent-keepalive 25",
        "wg set wg0 peer NEWKEY endpoint tunnels.example.com:51820",
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
      ["wg set wg0 peer HUBKEY allowed-ips 10.9.0.0/24", "ip route delete 10.8.0.0/24 dev wg0"],
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
    assert_eq!(lines(&cmds), ["wg set wg0 peer HUBKEY endpoint other.example.com:51820"]);
    assert_eq!(cmds[0].kind, CmdKind::Endpoint);
    assert_eq!(
      lines(&reapply_commands(&old, &old, false)),
      ["wg set wg0 peer HUBKEY endpoint tunnels.example.com:51820"],
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
        "wg set wg0 peer NEWKEY allowed-ips 10.9.0.0/24 persistent-keepalive 15",
        "ip address replace 10.8.0.21/32 dev wg0",
        "ip address delete 10.8.0.20/32 dev wg0",
        "ip route replace 10.9.0.0/24 dev wg0",
        "ip route delete 10.8.0.0/24 dev wg0",
        "wg set wg0 peer NEWKEY endpoint other.example.com:51820",
      ]
    );
  }
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test wireguard`
Expected: compile errors, `Cmd` and the planners not found.

- [ ] **Step 3: Implement the planner**

Add to `src/wireguard.rs`, after `validate_repo` and before `Action`:

```rust
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

/// The apply of 5.2 step 3: peer, address, link up, routes, and the endpoint last.
pub fn apply_commands(eff: &Effective) -> Vec<Cmd> {
  let mut v = vec![
    cmd(
      "wg",
      &[
        "set",
        IFACE,
        "peer",
        &eff.hub_public_key,
        "allowed-ips",
        &eff.allowed_ips.join(","),
        "persistent-keepalive",
        &eff.keepalive.to_string(),
      ],
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
    &["set", IFACE, "peer", &eff.hub_public_key, "endpoint", &eff.endpoint],
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
      &[
        "set",
        IFACE,
        "peer",
        &new.hub_public_key,
        "allowed-ips",
        &new.allowed_ips.join(","),
        "persistent-keepalive",
        &new.keepalive.to_string(),
      ],
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
```

- [ ] **Step 4: Run the planner tests**

Run: `cargo test wireguard`
Expected: the two new tests pass with the earlier ones.

- [ ] **Step 5: Add the Linux `Interface` beside the old `Tunnel`**

Inside the existing `#[cfg(unix)] mod unix`, add after `Tunnel`'s `impl` block, reusing the
module's `run` function and `IFACE` (remove the module's own `pub const IFACE` and import the
crate-level one with `use super::IFACE;`):

```rust
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

    fn exec(&self, c: &super::Cmd) -> Result<(), ApplyError> {
      let args: Vec<&str> = c.args.iter().map(String::as_str).collect();
      match run(c.program, &args, None) {
        Ok(_) => Ok(()),
        Err(e) => Err(match c.kind {
          super::CmdKind::Local => ApplyError::Local(e),
          super::CmdKind::Endpoint => ApplyError::Endpoint(e),
        }),
      }
    }

    /// The apply of 5.2 step 3; the preshared key, when there is one, goes over stdin right
    /// after the peer command.
    pub fn apply(&self, eff: &Effective) -> Result<(), ApplyError> {
      for (i, c) in super::apply_commands(eff).iter().enumerate() {
        self.exec(c)?;
        if i == 0 && let Some(psk) = &self.preshared_key {
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
```

Add `use crate::bundle::Effective;` to the unix module's imports and export the new items
next to the existing `pub use unix::Tunnel;`:

```rust
#[cfg(unix)]
pub use unix::{ApplyError, Interface, Tunnel};
```

- [ ] **Step 6: Build on both platforms' terms and commit**

Run: `cargo test wireguard && cargo fmt --all && cargo clippy --all-targets -- -D warnings`
Expected: green. The new `Interface` is unused until task 11; clippy is quiet because the
module is `pub` and the items are `pub`.

```bash
git add src/wireguard.rs
git commit -m "feat(wireguard): plan every apply and re-apply as commands, the endpoint last

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 10: the supervisor wakes on a signal or the child's exit

A self-contained change to the existing loop, verified by the existing smoke test before the
loop is rewritten in task 11.

**Files:**

- Modify: `Cargo.toml` (`nix` gains `poll`), `src/supervisor.rs`
- Test: a unit test in `src/supervisor.rs` (Linux only); the existing `docker_supervisor` smoke test

**Interfaces:**

- Produces (inside the Linux module of `supervisor.rs`): `Waker::new() -> Result<Waker>` registering SIGTERM, SIGINT, SIGHUP and SIGCHLD; `Waker::wait(&self, max: Duration)` returning at once when a registered signal arrives, else after `max`.

- [ ] **Step 1: Add the `poll` feature**

In `Cargo.toml`, the `nix` line becomes:

```toml
  nix         = { version = "0.31.3", features = ["user", "signal", "process", "poll"] }
```

- [ ] **Step 2: Write the failing test**

In `src/supervisor.rs`'s `tests` module, add:

```rust
  #[cfg(unix)]
  #[test]
  fn the_waker_returns_at_once_on_a_write_and_after_the_timeout_otherwise() {
    use std::time::{Duration, Instant};
    let waker = super::unix::Waker::new().unwrap();
    let start = Instant::now();
    waker.wait(Duration::from_millis(200));
    assert!(start.elapsed() >= Duration::from_millis(150), "waited out the timeout");
    waker.poke();
    let start = Instant::now();
    waker.wait(Duration::from_secs(5));
    assert!(start.elapsed() < Duration::from_secs(1), "woke at once");
    let start = Instant::now();
    waker.wait(Duration::from_millis(200));
    assert!(start.elapsed() >= Duration::from_millis(150), "the poke was drained");
  }
```

- [ ] **Step 3: Run it to see it fail**

Run: `cargo test waker`
Expected: compile error, `Waker` not found.

- [ ] **Step 4: Implement the waker and use it in the loop**

In the `unix` module of `src/supervisor.rs`, add (imports: `use std::io::{Read as _, Write as _};`,
`use std::os::fd::AsFd as _;`, `use std::os::unix::net::UnixStream;`):

```rust
  /// The loop's wake-up (spec 5.2 step 7): a socket pair the signal handlers write a byte to,
  /// so a signal or the child's exit (SIGCHLD) ends the wait at once instead of at the next
  /// 250 ms tick. The flags and `waitpid` still carry the facts; this only ends the sleep.
  pub struct Waker {
    rx: UnixStream,
    tx: UnixStream,
  }

  impl Waker {
    pub fn new() -> Result<Waker> {
      let (rx, tx) = UnixStream::pair().context("creating the wake-up socket pair")?;
      rx.set_nonblocking(true).context("wake-up socket")?;
      tx.set_nonblocking(true).context("wake-up socket")?;
      for sig in [
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGHUP,
        signal_hook::consts::SIGCHLD,
      ] {
        signal_hook::low_level::pipe::register(sig, tx.try_clone().context("wake-up socket")?).context("installing the wake-up handler")?;
      }
      Ok(Waker { rx, tx })
    }

    /// Wait until a byte arrives or `max` passes, then drain every byte that arrived.
    pub fn wait(&self, max: Duration) {
      use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
      let mut fds = [PollFd::new(self.rx.as_fd(), PollFlags::POLLIN)];
      let timeout = PollTimeout::try_from(max).unwrap_or(PollTimeout::MAX);
      let _ = poll(&mut fds, timeout);
      let mut buf = [0u8; 64];
      while (&self.rx).read(&mut buf).is_ok_and(|n| n > 0) {}
    }

    /// What a signal handler does; here for the test and for the child-exit path.
    pub fn poke(&self) {
      let _ = (&self.tx).write_all(&[1]);
    }
  }
```

Then in `run`, replace the flag registration and the sleep:

- keep the three `signal_hook::flag::register` calls as they are (the flags say which signal);
- create `let waker = Waker::new()?;` right after them;
- replace `std::thread::sleep(Duration::from_millis(250));` at the bottom of the loop with:

```rust
      let until_poll = next_poll.saturating_duration_since(Instant::now());
      waker.wait(until_poll.min(Duration::from_millis(250)));
```

- [ ] **Step 5: Run the unit tests, then the existing smoke test**

Run: `cargo test && cargo fmt --all && cargo clippy --all-targets -- -D warnings`
Expected: green on Linux; on Windows the waker test does not exist.

Run: `cargo test --test docker_supervisor -- --ignored --nocapture` on a Docker host with the
WireGuard module (CI does this; locally only if Docker Desktop's kernel has it).
Expected: green; the SIGTERM pass-through and the exit code are the assertions that matter here.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/supervisor.rs
git commit -m "feat(supervisor): wake the loop at once on a signal or the child's exit

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 11: the entrypoint and the supervisor on the new modules

The integration: `run.rs` becomes the boot sequence of spec 5.2 and the order of 7,
`supervisor.rs` becomes the loop of 5.2 step 7 with the repairs of 5.6, the re-apply and removal
of 5.5, the consent of 6.3 and the three shutdown paths; the old `Config`, `Assessor` and `Tunnel`
go. One commit: a reviewer cannot accept one half without the other, because neither compiles
alone.

**Files:**

- Modify: `src/bundle.rs` (two log helpers), `src/wireguard.rs` (deletions), `src/run.rs`
  (rewritten), `src/supervisor.rs` (rewritten), `src/main.rs` (attribute cleanup),
  `tests/docker_supervisor.rs` (the consent socket in the report)
- Test: `cargo test`, then the existing `docker_supervisor` smoke test

**Interfaces:**

- Consumes: everything tasks 1 to 10 produced.
- Produces: `supervisor::Applied { tag: Option<String>, effective: Effective }`; `supervisor::Plan { exe, app_root, poll_secs, ping, scrub: Vec<String>, consent_socket: PathBuf, log: Log, tunnel: Option<TunnelPlan> }`; `supervisor::TunnelPlan { interface: Interface, settings: Settings, fetcher: Option<Fetcher>, applied: Option<Applied>, unconfigured: Reason, endpoint_ok: bool }` (Linux); `bundle::Effective::describe(&self) -> String` and `Effective::diff(&self, new: &Effective) -> String`.

- [ ] **Step 1: The two log helpers on `Effective`, with their test**

In `src/bundle.rs` add to the `tests` module:

```rust
  #[test]
  fn describe_and_diff_shorten_keys_and_name_only_what_changed() {
    let b = parse(&good()).unwrap();
    let old = select(&b, &key('B')).unwrap();
    assert_eq!(
      old.describe(),
      format!("address 10.8.0.10/32, endpoint tunnels.example.com:51820, allowed IPs 10.8.0.0/24, keepalive 25, hub key {}…", "A".repeat(8))
    );
    assert_eq!(old.diff(&old), "");
    let mut new = old.clone();
    new.address = "10.8.0.11/32".into();
    new.keepalive = 15;
    new.hub_public_key = key('Z');
    assert_eq!(
      old.diff(&new),
      format!("hub key {}… -> {}…, address 10.8.0.10/32 -> 10.8.0.11/32, keepalive 25 -> 15", "A".repeat(8), "Z".repeat(8))
    );
  }
```

and after the `PartialEq for Effective` impl:

```rust
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
      parts.push(format!("allowed IPs {} -> {}", self.allowed_ips.join(","), new.allowed_ips.join(",")));
    }
    if self.keepalive != new.keepalive {
      parts.push(format!("keepalive {} -> {}", self.keepalive, new.keepalive));
    }
    parts.join(", ")
  }
}
```

Run: `cargo test bundle` — passes.

- [ ] **Step 2: Delete the old tunnel code from `wireguard.rs`**

Remove: the `Config` struct and its `impl`; `Action`; `Assessor` and its `impl`; inside the
`unix` module, the `Tunnel` struct and its `impl` (keep `run`, `IFACE` import, `ApplyError`,
`Interface`); the exports become `pub use unix::{ApplyError, Interface};`. In the tests module
remove `the_environment_contract_with_its_defaults`, `a_missing_or_bad_variable_is_named_never_its_value`
and `stale_resets_the_endpoint_first_then_cycles_the_interface`; keep `REQUIRED`, `env`, the
`Settings` tests, the planner tests and `the_latest_handshake_is_read_from_wg_show`. Then remove
the `#![allow(dead_code)]` line from `src/bundle.rs`, `src/health.rs`, `src/cache.rs`,
`src/logfile.rs`, `src/consent.rs` and `src/fetch.rs`.

- [ ] **Step 3: Replace `src/run.rs`**

Keep the `run_startup_scripts` function and the `tests` module from task 8; replace everything
above them with:

```rust
//! The container entrypoint (Linux only): the shell script's job in the order of spec 7, the
//! tunnel's boot (5.2 steps 1 to 5) before any file is touched, then one branch: exec the app
//! (the default) or spawn and supervise it (`supervise` / `wireguard`).

use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use nix::unistd::{Gid, Uid, getuid, setgid, setgroups, setuid};

use crate::bundle::{self, Effective};
use crate::fetch::{self, Fetcher, Hosts, Outcome};
use crate::health::Reason;
use crate::logfile::Log;
use crate::ping::{self, Kind, Ping};
use crate::supervisor::{Applied, Plan, TunnelPlan};
use crate::wireguard::{self, ApplyError, Interface, Mode, Settings};
use crate::{cache, consent, heartbeat, mounts, prepare, pyproject, supervisor};

pub struct RunArgs {
  pub pyproject: PathBuf,
  pub app_root: PathBuf,
  pub mountinfo: PathBuf,
}

/// Returns the exit code when supervising; the exec path only ever returns an error.
pub fn run(args: &RunArgs) -> Result<u8> {
  if !getuid().is_root() {
    bail!("entrypoint must run as root (uid 0); got uid {}", getuid());
  }
  let doc = pyproject::load(&args.pyproject)?;
  // Resolved before any directory is created, so a misconfigured pyproject fails first.
  let script = pyproject::launch_script(&doc)?;
  let startup = pyproject::startup_scripts(&doc)?;
  let scrub = pyproject::scrub_env(&doc)?;
  let entries = pyproject::required_persisted_dirs(&doc)?;
  let wireguard = pyproject::wireguard(&doc)?;
  let supervise = wireguard || pyproject::supervise(&doc)?;
  let mountinfo = std::fs::read_to_string(&args.mountinfo).with_context(|| format!("reading {}", args.mountinfo.display()))?;
  let missing = mounts::unbacked(&mounts::parse_mountinfo(&mountinfo), &args.app_root, &entries);
  if !missing.is_empty() {
    bail!(
      "required_persisted_dirs not backed by a bind mount (start the container with its volume): {}",
      missing.join(", ")
    );
  }
  let env = |k: &str| std::env::var(k).ok();
  // The ping before the tunnel, so a refused start can send /fail (5.2).
  let ping = if supervise {
    let services = pyproject::services(&doc);
    let ping = Ping::configure(
      env("ALERTS_HEALTHCHECK_PING_URL").as_deref(),
      env("PINGKEY").as_deref(),
      ping::slug(env("HEARTBEAT_SLUG").as_deref(), &services).as_deref(),
    );
    match (&ping, wireguard) {
      (None, true) => eprintln!(
        "devkit-container: no ping configured (PINGKEY and HEARTBEAT_SLUG, or ALERTS_HEALTHCHECK_PING_URL): the tunnel is visible to Docker but not to healthchecks.io"
      ),
      (None, false) => eprintln!("devkit-container: no ping configured; the app pings for itself"),
      _ => {}
    }
    ping
  } else {
    None
  };
  let log = Log::new(&args.app_root);
  // The tunnel, before `prepare`: a refused start touches no file.
  let tunnel = if wireguard {
    Some(boot_tunnel(&args.app_root, &log, ping.as_ref())?)
  } else {
    None
  };
  let down = |t: &Option<BootTunnel>| {
    if let Some(t) = t {
      t.plan.interface.down();
    }
  };
  // `prepare`, with the implicit folders of 4.4 and 5.7.
  let mut to_prepare = entries.clone();
  if supervise {
    to_prepare.push("persisted_data/logs".to_string());
  }
  if wireguard {
    to_prepare.push(cache::DIR.to_string());
  }
  if let Err(e) = prepare::prepare(&args.app_root, &to_prepare, &mut prepare::chown_nonroot) {
    down(&tunnel);
    return Err(e);
  }
  if supervise && let Err(e) = consent_dir() {
    down(&tunnel);
    return Err(e);
  }
  if let Some(text) = tunnel.as_ref().and_then(|t| t.cache_text.as_deref())
    && let Err(e) = cache::write(&args.app_root, text)
  {
    log.line(&format!("cache write failed: {e:#}"));
  }
  // A tunnel heartbeat left by an earlier run must not read as a stale tunnel.
  if !wireguard {
    let _ = std::fs::remove_file(heartbeat::logs_dir(&args.app_root).join(heartbeat::TUNNEL_FILE));
  }
  if let Err(e) = run_startup_scripts(&args.app_root, &startup) {
    down(&tunnel);
    return Err(e);
  }
  let exe = args.app_root.join(".venv").join("bin").join(&script);
  if supervise {
    let poll_secs = match &tunnel {
      Some(t) => t.plan.settings.poll_secs,
      None => env("WG_POLL_SECS").and_then(|v| v.parse().ok()).unwrap_or(30),
    };
    return supervisor::run(Plan {
      exe,
      app_root: args.app_root.clone(),
      poll_secs,
      ping,
      scrub,
      consent_socket: PathBuf::from(consent::SOCKET),
      log,
      tunnel: tunnel.map(|t| t.plan),
    });
  }
  // Drop privileges, then replace this process with the app. Order matters: once the uid is
  // 999 the process may no longer change its groups, so groups and gid go first.
  setgroups(&[]).context("setgroups")?;
  setgid(Gid::from_raw(prepare::NONROOT)).context("setgid")?;
  setuid(Uid::from_raw(prepare::NONROOT)).context("setuid")?;
  use std::os::unix::process::CommandExt as _;
  let mut cmd = std::process::Command::new(&exe);
  for var in wireguard::SECRET_VARS.iter().copied().chain(scrub.iter().map(String::as_str)) {
    cmd.env_remove(var);
  }
  // `exec` never returns on success; the only thing it can hand back is the error that stopped it.
  let err = cmd.exec();
  Err(anyhow!(err)).with_context(|| format!("exec {}", exe.display()))
}

/// `/run/devkit`, mode 0700, owned by nonroot: where a participating app opens the consent
/// socket (spec 6.2).
fn consent_dir() -> Result<()> {
  use std::os::unix::fs::PermissionsExt as _;
  std::fs::create_dir_all(consent::DIR).with_context(|| format!("creating {}", consent::DIR))?;
  std::fs::set_permissions(consent::DIR, std::fs::Permissions::from_mode(0o700)).with_context(|| format!("chmod {}", consent::DIR))?;
  prepare::chown_nonroot(Path::new(consent::DIR))
}

/// What the boot hands the supervisor, plus the bundle text to cache once `prepare` has made
/// the folder.
pub struct BootTunnel {
  pub plan: TunnelPlan,
  pub cache_text: Option<String>,
}

/// Spec 5.2 steps 1 to 5. `Err` is Broken or a refused start, `/fail` sent and the interface
/// down; `Ok` is Connected, or Disconnected under the tolerate switch.
fn boot_tunnel(app_root: &Path, log: &Log, ping: Option<&Ping>) -> Result<BootTunnel> {
  let settings = Settings::from_env(&|k| std::env::var(k).ok())?;
  let (preshared_key, fetcher) = match &settings.mode {
    Mode::Env { preshared_key, .. } => (preshared_key.clone(), None),
    Mode::Fetched { hub_url, repo, token } => (
      None,
      Some(Fetcher::new(hub_url.clone(), repo.clone(), token.clone(), Hosts::default())),
    ),
  };
  let alert = |body: &str| {
    if let Some(p) = ping
      && let Err(e) = ping::send(&ping::agent(), &p.url(Kind::Fail), body)
    {
      eprintln!("devkit-container: ping Fail failed: {e}");
    }
  };
  // 1. The interface and the key. Broken: exit 1, nothing left behind.
  let iface = match Interface::create(&settings.private_key, preshared_key) {
    Ok(i) => i,
    Err(e) => {
      alert(&format!("{e:#}"));
      return Err(e);
    }
  };
  log.line(&format!("wireguard public key {}", iface.public_key));
  // 2. The configuration.
  let mut cache_text = None;
  let mut failure: Option<(Reason, String)> = None;
  let obtained: Option<(Option<String>, Effective)> = match &settings.mode {
    Mode::Env { effective, .. } => Some((None, effective.clone())),
    Mode::Fetched { .. } => match fetch::obtain(fetcher.as_ref().expect("fetched mode"), &iface.public_key, None) {
      Outcome::New { tag, text, effective } => {
        log.line(&format!("fetched bundle {tag}"));
        cache_text = Some(text);
        Some((Some(tag), effective))
      }
      Outcome::NotEnrolled { tag, text } => {
        cache_text = Some(text);
        failure = Some((
          Reason::NotEnrolled,
          format!(
            "not enrolled in {tag}: no entry for {}; turn [tool.docker].wireguard off or enrol the key, then redeploy",
            iface.public_key
          ),
        ));
        None
      }
      Outcome::Unchanged(tag) => {
        failure = Some((Reason::ConfigUnavailable, format!("config unavailable: unexpected unchanged {tag}")));
        None
      }
      Outcome::Unavailable(msg) => {
        log.line(&format!("config unavailable: {msg}"));
        // The cache is the fallback (4.4); one without this spoke's entry counts as none.
        let cached = cache::read(app_root)
          .ok()
          .and_then(|text| bundle::parse(&text).ok())
          .and_then(|b| Some((b.hub_version.clone()?, bundle::select(&b, &iface.public_key)?)));
        match cached {
          Some((tag, effective)) => {
            log.line(&format!("using cached bundle {tag}"));
            Some((Some(tag), effective))
          }
          None => {
            failure = Some((Reason::ConfigUnavailable, "config unavailable: no bundle and no usable cache".to_string()));
            None
          }
        }
      }
    },
  };
  // 3 and 4. Apply, then the first handshake.
  let mut applied = None;
  let mut endpoint_ok = true;
  if let Some((tag, effective)) = obtained {
    match iface.apply(&effective) {
      Err(ApplyError::Local(e)) => {
        iface.down();
        alert(&format!("{e:#}"));
        return Err(e);
      }
      Err(ApplyError::Endpoint(e)) => {
        endpoint_ok = false;
        failure = Some((Reason::EndpointUnresolvable, format!("endpoint unresolvable: {e:#}")));
      }
      Ok(()) => {}
    }
    log.line(&format!("applied {}: {}", tag.as_deref().unwrap_or("the environment"), effective.describe()));
    if failure.is_none() {
      match iface.wait_handshake(&effective.hub_public_key, Duration::from_secs(settings.handshake_timeout_secs)) {
        Err(e) => {
          iface.down();
          alert(&format!("{e:#}"));
          return Err(e);
        }
        Ok(true) => log.line(&format!("wireguard handshake with {}", effective.endpoint)),
        Ok(false) => {
          failure = Some((
            Reason::NoHandshake,
            format!(
              "no wireguard handshake with {} within {} s",
              effective.endpoint, settings.handshake_timeout_secs
            ),
          ))
        }
      }
    }
    applied = Some(Applied { tag, effective });
  }
  // 5. The gate.
  let unconfigured = match (&failure, &applied) {
    (Some((reason, _)), None) => *reason,
    _ => Reason::ConfigUnavailable,
  };
  if let Some((_, msg)) = &failure {
    if !settings.tolerate {
      iface.down();
      log.line(&format!("refusing to start: {msg}"));
      alert(msg);
      bail!("{msg}");
    }
    log.line(&format!("WG_TOLERATE_DISCONNECTED=1: starting Disconnected: {msg}"));
    alert(msg);
  }
  Ok(BootTunnel {
    plan: TunnelPlan {
      interface: iface,
      settings,
      fetcher,
      applied,
      unconfigured,
      endpoint_ok,
    },
    cache_text,
  })
}
```

- [ ] **Step 4: Replace `src/supervisor.rs`**

Keep the `Pinger` struct, its `impl` and its test, and the `Waker` from task 10; replace
everything else:

```rust
//! The supervising entrypoint: the app spawned as 999 with nothing kept, signals forwarded,
//! zombies reaped, and every `WG_POLL_SECS` the tunnel assessed (spec 5.3), repaired (5.6) or
//! re-applied (5.5), its heartbeat written, every heartbeat file adjudicated and the ping sent.
//! Fetches and consent asks run on worker threads; the loop only ever waits on its waker.

use std::path::PathBuf;

use crate::bundle::Effective;
use crate::logfile::Log;
use crate::ping::{Kind, Ping};

// … Pinger unchanged …

/// The applied configuration and the tag it came from (`None` in environment mode).
#[derive(Debug, Clone)]
pub struct Applied {
  pub tag: Option<String>,
  pub effective: Effective,
}

pub struct Plan {
  pub exe: PathBuf,
  pub app_root: PathBuf,
  pub poll_secs: u64,
  pub ping: Option<Ping>,
  /// `[tool.docker].scrub_env`, on top of the built-in secrets.
  pub scrub: Vec<String>,
  pub consent_socket: PathBuf,
  pub log: Log,
  #[cfg(unix)]
  pub tunnel: Option<TunnelPlan>,
}

/// The tunnel as the boot left it (spec 5.2), handed to the loop.
#[cfg(unix)]
pub struct TunnelPlan {
  pub interface: crate::wireguard::Interface,
  pub settings: crate::wireguard::Settings,
  pub fetcher: Option<crate::fetch::Fetcher>,
  pub applied: Option<Applied>,
  /// The reason while `applied` is `None`, which only the tolerate switch allows.
  pub unconfigured: crate::health::Reason,
  pub endpoint_ok: bool,
}

#[cfg(unix)]
pub use unix::run;

#[cfg(unix)]
pub mod unix {
  use std::io::{Read as _, Write as _};
  use std::os::fd::AsFd as _;
  use std::os::unix::net::UnixStream;
  use std::os::unix::process::CommandExt as _;
  use std::path::Path;
  use std::process::Command;
  use std::sync::Arc;
  use std::sync::atomic::{AtomicBool, Ordering};
  use std::thread::JoinHandle;
  use std::time::{Duration, Instant};

  use anyhow::{Context as _, Result};
  use nix::sys::signal::{Signal, kill};
  use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
  use nix::unistd::{Gid, Pid, Uid, getuid, setgid, setgroups, setuid};

  use super::{Applied, Pinger, Plan, TunnelPlan};
  use crate::fetch::{self, Outcome};
  use crate::health::{Health, Interface, Reason, Repair, Reply, State};
  use crate::logfile::Log;
  use crate::ping::{self, Kind};
  use crate::prepare::NONROOT;
  use crate::wireguard::{ApplyError, SECRET_VARS};
  use crate::{cache, consent, heartbeat};

  // … Waker unchanged from task 10 …

  fn drop_privileges() -> Result<()> {
    setgroups(&[]).context("setgroups")?;
    setgid(Gid::from_raw(NONROOT)).context("setgid")?;
    setuid(Uid::from_raw(NONROOT)).context("setuid")?;
    Ok(())
  }

  /// Why the loop ends the run before the app did, and how the app is stopped first.
  enum Exit {
    /// A local command failed (5.3): SIGTERM, then down, /fail, exit 1.
    Broken(anyhow::Error),
    /// The consent path ran out (6.3): SIGINT, then down, /fail, exit 75.
    GiveUp { elapsed: u64, reason: Reason, hold_limit: bool },
    /// The hub's newest bundle dropped this spoke (5.5): /fail, SIGINT, down, exit 75.
    Removed(String),
  }

  /// The tunnel side of the loop: the health machine, the worker threads and their results.
  struct TunnelState {
    plan: TunnelPlan,
    health: Health,
    started: Instant,
    fetch_job: Option<JoinHandle<Outcome>>,
    ask_job: Option<JoinHandle<Reply>>,
    reply: Option<Reply>,
    next_version_check: Instant,
    version_check_failing: bool,
    last_obtain_failure: Option<String>,
  }

  impl TunnelState {
    fn new(plan: TunnelPlan, started: Instant) -> TunnelState {
      let s = &plan.settings;
      TunnelState {
        health: Health::new(s.tolerate, plan.fetcher.is_some(), s.limit_secs, s.hold_limit_secs),
        next_version_check: started + Duration::from_secs(s.version_poll_secs),
        plan,
        started,
        fetch_job: None,
        ask_job: None,
        reply: None,
        version_check_failing: false,
        last_obtain_failure: None,
      }
    }

    fn observation(&self) -> Interface {
      match self.plan.applied {
        None => Interface::Unconfigured(self.plan.unconfigured),
        Some(_) => Interface::Configured {
          endpoint_ok: self.plan.endpoint_ok,
        },
      }
    }

    /// One version → bundle → entry attempt on a thread, if none is in flight.
    fn start_fetch(&mut self) {
      let (Some(f), None) = (&self.plan.fetcher, &self.fetch_job) else { return };
      let f = f.clone();
      let public_key = self.plan.interface.public_key.clone();
      let tag = self.plan.applied.as_ref().and_then(|a| a.tag.clone());
      self.fetch_job = Some(std::thread::spawn(move || fetch::obtain(&f, &public_key, tag.as_deref())));
    }

    /// Take the results of finished threads. Runs before every poll, so a result is acted on at
    /// the first poll after it arrived (5.2 step 7).
    fn collect(&mut self, log: &Log, app_root: &Path) -> Result<(), Exit> {
      if self.fetch_job.as_ref().is_some_and(|h| h.is_finished())
        && let Some(h) = self.fetch_job.take()
      {
        match h.join() {
          Ok(outcome) => self.take_outcome(outcome, log, app_root)?,
          Err(_) => log.line("the fetch thread panicked; the next poll retries"),
        }
      }
      if self.ask_job.as_ref().is_some_and(|h| h.is_finished())
        && let Some(h) = self.ask_job.take()
      {
        let reply = h.join().unwrap_or(Reply::Ok);
        log.line(match reply {
          Reply::Hold => "the app replied hold; postponing",
          Reply::Ok => "the app replied ok, or did not reply",
        });
        self.reply = Some(reply);
      }
      Ok(())
    }

    fn take_outcome(&mut self, outcome: Outcome, log: &Log, app_root: &Path) -> Result<(), Exit> {
      match outcome {
        Outcome::Unchanged(_) => self.version_check_ok(log),
        Outcome::Unavailable(msg) => {
          if self.plan.applied.is_none() {
            if self.last_obtain_failure.as_deref() != Some(msg.as_str()) {
              log.line(&format!("config unavailable: {msg}"));
              self.last_obtain_failure = Some(msg);
            }
            self.plan.unconfigured = Reason::ConfigUnavailable;
          } else if !self.version_check_failing {
            // Once per change of outcome (5.5), never per attempt.
            self.version_check_failing = true;
            log.line(&format!("version check failing: {msg}"));
          }
        }
        Outcome::New { tag, text, effective } => {
          self.version_check_ok(log);
          if let Err(e) = cache::write(app_root, &text) {
            log.line(&format!("cache write failed: {e:#}"));
          }
          match self.plan.applied.clone() {
            None => {
              let r = self.plan.interface.apply(&effective);
              self.classify(r, log)?;
              log.line(&format!("applied {tag}: {}", effective.describe()));
              self.plan.applied = Some(Applied {
                tag: Some(tag),
                effective,
              });
              self.health.config_applied();
            }
            Some(old) if old.effective == effective => {
              log.line(&format!("applied {tag}: no change for this spoke"));
              self.plan.applied = Some(Applied {
                tag: Some(tag),
                effective,
              });
            }
            Some(old) => {
              let r = self.plan.interface.reapply(&old.effective, &effective, self.plan.endpoint_ok);
              self.classify(r, log)?;
              log.line(&format!("re-applied {tag} in place: {}", old.effective.diff(&effective)));
              self.plan.applied = Some(Applied {
                tag: Some(tag),
                effective,
              });
              self.health.config_applied();
            }
          }
        }
        Outcome::NotEnrolled { tag, text } => {
          self.version_check_ok(log);
          if let Err(e) = cache::write(app_root, &text) {
            log.line(&format!("cache write failed: {e:#}"));
          }
          if self.plan.applied.is_some() {
            return Err(Exit::Removed(tag));
          }
          let msg = format!("not enrolled in {tag}: no entry for {}", self.plan.interface.public_key);
          if self.last_obtain_failure.as_deref() != Some(msg.as_str()) {
            log.line(&msg);
            self.last_obtain_failure = Some(msg);
          }
          self.plan.unconfigured = Reason::NotEnrolled;
        }
      }
      Ok(())
    }

    fn version_check_ok(&mut self, log: &Log) {
      if self.version_check_failing {
        self.version_check_failing = false;
        log.line("version check succeeding again");
      }
    }

    /// Spec 5.3: a local failure is Broken; the endpoint failing is Disconnected.
    fn classify(&mut self, r: Result<(), ApplyError>, log: &Log) -> Result<(), Exit> {
      match r {
        Ok(()) => {
          self.plan.endpoint_ok = true;
          Ok(())
        }
        Err(ApplyError::Endpoint(e)) => {
          self.plan.endpoint_ok = false;
          log.line(&format!("endpoint unresolvable: {e:#}"));
          Ok(())
        }
        Err(ApplyError::Local(e)) => Err(Exit::Broken(e)),
      }
    }

    /// The tunnel's share of one poll: assess, write the heartbeat, repair, check the version,
    /// ask, or give up.
    fn poll(&mut self, log: &Log, socket: &Path, tunnel_beat: &Path) -> Result<(), Exit> {
      let now = jiff::Timestamp::now();
      let handshake = match &self.plan.applied {
        None => None,
        Some(a) => self.plan.interface.latest_handshake(&a.effective.hub_public_key).map_err(Exit::Broken)?,
      };
      let fresh = handshake.is_some_and(|h| (now.as_second() as u64).saturating_sub(h) < self.plan.settings.stale_secs);
      let d = self.health.poll(self.started.elapsed().as_secs(), self.observation(), fresh, self.reply.take());
      if d.changed {
        log.line(&format!("wireguard {}", d.state.describe()));
      }
      if fresh && let Err(e) = heartbeat::write(tunnel_beat, now) {
        log.line(&format!("{e:#}"));
      }
      match (d.repair, self.plan.applied.clone()) {
        (Repair::Obtain, _) => self.start_fetch(),
        (Repair::ResetEndpoint, Some(a)) => {
          log.line(&format!("wireguard re-up: resetting the endpoint {}", a.effective.endpoint));
          let r = self.plan.interface.set_endpoint(&a.effective);
          self.classify(r, log)?;
        }
        (Repair::DownUp, Some(a)) => {
          log.line("wireguard re-up: wg0 down and up");
          let r = self.plan.interface.down_up(&a.effective);
          self.classify(r, log)?;
        }
        _ => {}
      }
      if d.check_version {
        self.start_fetch();
      }
      if d.state == State::Connected && self.plan.fetcher.is_some() && Instant::now() >= self.next_version_check {
        self.next_version_check = Instant::now() + Duration::from_secs(self.plan.settings.version_poll_secs);
        self.start_fetch();
      }
      if let Some(elapsed) = d.ask
        && self.ask_job.is_none()
      {
        let path = socket.to_path_buf();
        let reason = format!("wireguard-disconnected {elapsed}s");
        log.line(&format!("shutdown pending: asking the app ({reason})"));
        self.ask_job = Some(std::thread::spawn(move || consent::ask(&path, &reason, consent::TIMEOUT)));
      }
      if let Some(g) = d.give_up {
        let State::Disconnected(reason) = d.state else { unreachable!("a give-up is Disconnected") };
        return Err(Exit::GiveUp {
          elapsed: g.elapsed,
          reason,
          hold_limit: g.hold_limit,
        });
      }
      Ok(())
    }
  }

  /// Reap every finished child (orphans adopted as PID 1 included); the app's exit code once
  /// it has ended (signal death as 128+n).
  fn reap(child: Pid) -> Option<u8> {
    let mut app_exit = None;
    loop {
      match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
        Ok(WaitStatus::Exited(pid, code)) if pid == child => app_exit = Some(code as u8),
        Ok(WaitStatus::Signaled(pid, sig, _)) if pid == child => app_exit = Some(128u8.wrapping_add(sig as i32 as u8)),
        Ok(WaitStatus::StillAlive) | Err(_) => break,
        Ok(_) => {}
      }
    }
    app_exit
  }

  /// Signal the app and give it 30 s to exit before SIGKILL (5.3, 5.5, 6.3); reaped either way.
  fn stop_app(child: Pid, sig: Signal, waker: &Waker) {
    let _ = kill(child, sig);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
      if reap(child).is_some() {
        return;
      }
      if Instant::now() >= deadline {
        let _ = kill(child, Signal::SIGKILL);
        let _ = waitpid(child, None);
        return;
      }
      waker.wait(Duration::from_millis(250).min(deadline.saturating_duration_since(Instant::now())));
    }
  }

  /// Supervise `plan.exe` until it exits; returns the code to exit with. Never returns while
  /// the child runs, except through the three shutdown paths, which stop it first.
  pub fn run(plan: Plan) -> Result<u8> {
    let Plan {
      exe,
      app_root,
      poll_secs,
      ping,
      scrub,
      consent_socket,
      log,
      tunnel,
    } = plan;
    let root = getuid().is_root();
    // Without a tunnel nothing later needs root: drop now, and the child inherits 999.
    if tunnel.is_none() && root {
      drop_privileges()?;
    }
    let spawner_is_root = tunnel.is_some() && root;

    let term = Arc::new(AtomicBool::new(false));
    let int = Arc::new(AtomicBool::new(false));
    let hup = Arc::new(AtomicBool::new(false));
    for (sig, flag) in [
      (signal_hook::consts::SIGTERM, &term),
      (signal_hook::consts::SIGINT, &int),
      (signal_hook::consts::SIGHUP, &hup),
    ] {
      signal_hook::flag::register(sig, Arc::clone(flag)).context("installing signal handler")?;
    }
    let waker = Waker::new()?;

    let mut cmd = Command::new(&exe);
    for var in SECRET_VARS.iter().copied().chain(scrub.iter().map(String::as_str)) {
      cmd.env_remove(var);
    }
    if ping.is_some() {
      cmd.env("DEVKIT_SUPERVISED_PING", "1");
    }
    cmd.env("DEVKIT_CONSENT_SOCKET", &consent_socket);
    if spawner_is_root {
      // SAFETY: only async-signal-safe syscalls between fork and exec.
      unsafe {
        cmd.pre_exec(|| drop_privileges().map_err(|e| std::io::Error::other(e.to_string())));
      }
    }
    let child = cmd.spawn().with_context(|| format!("spawning {}", exe.display()))?;
    let child_pid = Pid::from_raw(child.id() as i32);
    log.line(&format!("supervising pid {child_pid}"));

    let logs = heartbeat::logs_dir(&app_root);
    let app_beat = logs.join(heartbeat::APP_FILE);
    let tunnel_beat = logs.join(heartbeat::TUNNEL_FILE);
    let agent = ping::agent();
    let mut pinger = Pinger::default();
    let mut inflight: Option<JoinHandle<()>> = None;
    let mut next_poll = Instant::now();
    let mut tunnel = tunnel.map(|t| TunnelState::new(t, Instant::now()));
    let exit_code: u8;

    // Off the loop's thread: a slow host must not delay signal forwarding or reaping. One in
    // flight at a time, bounded by the agent's 10 s timeout.
    let send = |inflight: &mut Option<JoinHandle<()>>, kind: Kind, body: &str| {
      let Some(p) = &ping else { return };
      if inflight.as_ref().is_some_and(|h| !h.is_finished()) {
        eprintln!("devkit-container: ping {kind:?} skipped: the previous one is still in flight");
        return;
      }
      let (agent, url, body) = (agent.clone(), p.url(kind), body.to_string());
      *inflight = Some(std::thread::spawn(move || {
        if let Err(e) = ping::send(&agent, &url, &body) {
          eprintln!("devkit-container: ping {kind:?} failed: {e}");
        }
      }));
    };
    // The exit paths wait for the in-flight ping, then send theirs synchronously.
    let send_now = |inflight: &mut Option<JoinHandle<()>>, body: &str| {
      if let Some(h) = inflight.take() {
        let _ = h.join();
      }
      if let Some(p) = &ping
        && let Err(e) = ping::send(&agent, &p.url(Kind::Fail), body)
      {
        eprintln!("devkit-container: ping Fail failed: {e}");
      }
    };

    loop {
      for (flag, sig) in [(&term, Signal::SIGTERM), (&int, Signal::SIGINT), (&hup, Signal::SIGHUP)] {
        if flag.swap(false, Ordering::SeqCst) {
          let _ = kill(child_pid, sig);
        }
      }
      if let Some(code) = reap(child_pid) {
        exit_code = code;
        break;
      }
      let mut exit: Option<Exit> = None;
      if let Some(ts) = &mut tunnel
        && let Err(e) = ts.collect(&log, &app_root)
      {
        exit = Some(e);
      }
      if exit.is_none() && Instant::now() >= next_poll {
        next_poll = Instant::now() + Duration::from_secs(poll_secs);
        let now = jiff::Timestamp::now();
        let mut reasons: Vec<String> = Vec::new();
        if let Some(ts) = &mut tunnel {
          if let Err(e) = ts.poll(&log, &consent_socket, &tunnel_beat) {
            exit = Some(e);
          }
          if let Err(r) = heartbeat::check(&tunnel_beat, ts.plan.settings.stale_secs, now) {
            reasons.push(r);
          }
        }
        if exit.is_none() {
          if let Err(r) = heartbeat::check(&app_beat, heartbeat::DEFAULT_MAX_AGE_SECS, now) {
            reasons.push(r);
          }
          match pinger.decide(reasons.is_empty()) {
            Some(Kind::Fail) => {
              log.line(&format!("unhealthy: {}", reasons.join("; ")));
              send(&mut inflight, Kind::Fail, &reasons.join("\n"));
            }
            Some(kind) => send(&mut inflight, kind, ""),
            None => {}
          }
        }
      }
      if let Some(exit) = exit {
        let ts = tunnel.as_ref().expect("an exit comes from the tunnel");
        match exit {
          Exit::Broken(err) => {
            log.line(&format!("broken: {err:#}; stopping the app"));
            stop_app(child_pid, Signal::SIGTERM, &waker);
            ts.plan.interface.down();
            send_now(&mut inflight, &format!("{err:#}"));
            return Err(err);
          }
          Exit::GiveUp { elapsed, reason, hold_limit } => {
            let msg = format!("gave up after {elapsed} s: {}", reason.as_str());
            if hold_limit {
              log.line("WG_HOLD_LIMIT_SECS reached; proceeding without asking again");
            }
            log.line(&format!("{msg}; SIGINT to the app"));
            stop_app(child_pid, Signal::SIGINT, &waker);
            ts.plan.interface.down();
            send_now(&mut inflight, &msg);
            return Ok(75);
          }
          Exit::Removed(tag) => {
            let msg = format!("removed from the hub's peer table in {tag}");
            log.line(&format!("{msg}; SIGINT to the app"));
            send_now(&mut inflight, &msg);
            stop_app(child_pid, Signal::SIGINT, &waker);
            ts.plan.interface.down();
            return Ok(75);
          }
        }
      }
      let until_poll = next_poll.saturating_duration_since(Instant::now());
      waker.wait(until_poll.min(Duration::from_millis(250)));
    }

    if let Some(ts) = &tunnel {
      ts.plan.interface.down();
    }
    if exit_code != 0 {
      log.line(&format!("app exited with {exit_code}"));
      send(&mut inflight, Kind::Fail, &format!("exit code {exit_code}"));
    }
    // The container ends with this process; give the last ping its chance to leave.
    if let Some(h) = inflight {
      let _ = h.join();
    }
    Ok(exit_code)
  }
}
```

Keep the `tests` module with the `Pinger` test and the waker test from task 10.

- [ ] **Step 5: Build, lint, unit tests**

Run: `cargo build && cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: green. Typical first-build errors and their fixes: a `use` left over from the deleted
`Tunnel` (remove it); `Effective` needing `Clone` in a `match … .clone()` (it derives `Clone`);
the `let` chains, which need the 2024 edition the crate already uses. Do not weaken any test to
get green: the tests are the spec's.

- [ ] **Step 6: The consent socket in the smoke test's report**

In `tests/docker_supervisor.rs`, in `SERVE_APP`'s report dict add
`"consent_socket": os.environ.get("DEVKIT_CONSENT_SOCKET"),` and after the two existing
`assert_eq!(report["supervised_ping"], "1");` lines add
`assert_eq!(report["consent_socket"], "/run/devkit/consent.sock");`.

- [ ] **Step 7: The environment-mode smoke test**

Run: `cargo test --test docker_supervisor -- --ignored --nocapture` where Docker has the
WireGuard module (CI does; locally only with a capable Docker Desktop kernel).
Expected: green. What changed underneath it: the boot now waits for the handshake before the
app starts (the hub container is up first, so it does), the re-up alternates, and the
supervisor writes `devkit-container.log`.

- [ ] **Step 8: Commit**

```bash
git add src tests/docker_supervisor.rs
git commit -m "feat(run,supervisor): the hub-fetched boot, the repairs, in-place re-apply, consent and the shutdown paths

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 12: the templates, the README, the todo and CI

**Files:**

- Modify: `python/devkit_container/compose.template.yaml`, `python/devkit_container/template.Dockerfile`, `README.md`, `todo.md`, `.github/workflows/ci.yml`
- Test: `cargo test --test docker_smoke -- --ignored` still passes (the Dockerfile helper in `tests/common/mod.rs` passes the window markers through as plain lines); `bash ci/render.sh` once the aeth-devkit release that knows `!window` exists

- [ ] **Step 1: The compose template (spec 9.2)**

In `python/devkit_container/compose.template.yaml`, replace the ten lines between
`# !if keys("tool.docker.wireguard"):` (the environment one, line 33) and its `# !end` with:

```yaml
    # !if keys("tool.docker.wireguard"):
      - WG_PRIVATE_KEY=${WG_PRIVATE_KEY:?}
      - WG_HUB_URL=${WG_HUB_URL:?}
      - WG_HUB_REPO=${WG_HUB_REPO:?}
      - WG_HUB_TOKEN=${WG_HUB_TOKEN:?}
    # !end
```

Nothing else in the file changes.

- [ ] **Step 2: The Dockerfile template (spec 9.3)**

In `python/devkit_container/template.Dockerfile`, after the last builder-stage `RUN` (the
`uv sync --frozen --no-dev --no-editable $extras` step) and before `# ---- Final stage ----`,
insert:

```dockerfile

# Project additions to the builder stage; setup-project renders the template around this window.
# !window builder:
# !end builder
```

and after the wireguard block's `# !end` and before `WORKDIR /app`, insert:

```dockerfile

# Project additions to the final stage; setup-project renders the template around this window.
# !window final:
# !end final
```

The `dockerfile()` helper in `tests/common/mod.rs` matches only the one wireguard gate and
copies every other line, markers included, so it needs no change; Docker reads the markers as
comments.

- [ ] **Step 3: The README**

In `README.md`:

1. In "How a project uses it", the sentence in parentheses becomes: `(both carry `# !` gates on
   `[tool.docker].wireguard`; the Dockerfile also carries two `# !window` regions a project may
   fill, which `setup-project` renders around; the compose one also carries the `# !rule`
   annotations the compose step enforces)`.
2. Replace the whole `run` bullet under "Subcommands" (from "- `run` is the entrypoint" to the
   end of the paragraph ending "the tunnel is then visible to Docker but not to healthchecks.io).")
   with:

```markdown
- `run` is the entrypoint (Linux only). Must be root. In order: resolves the single `run-app-*`
  script in `[project.scripts]` and every `[tool.docker].startup_scripts` name (each must be a
  `[project.scripts]` key); checks every `[tool.docker].required_persisted_dirs` entry is backed
  by a bind mount (the path or an ancestor below `/app`, per `/proc/self/mountinfo`) and refuses
  to start otherwise; with `wireguard` on, boots the tunnel (below); `mkdir -p` + recursive chown
  to `999:999` of the required dirs plus, implicitly, `persisted_data/logs` when supervising and
  `persisted_data/wireguard` with `wireguard` on; runs the startup scripts as root, in order, in
  `/app`, with the full environment, no arguments and no timeout (the first nonzero exit ends
  the run with exit 1); removes `WG_PRIVATE_KEY`, `WG_PEER_PRESHARED_KEY`, `WG_HUB_TOKEN` and
  every `[tool.docker].scrub_env` name from the app's environment; `setgroups([])`, `setgid`,
  `setuid`; `exec /app/.venv/bin/<script>`. `/app` itself stays root-owned: the app writes only
  to its mounted dirs or temp dirs. Entries that are empty, `.`, `..`, absolute or escape `/app`
  are errors; a table still carrying `chown_paths`/`mkdirs` is refused with the migration hint.
  Flags `--pyproject`, `--app-root`, `--mountinfo` exist for tests.

  With `[tool.docker].supervise` or `wireguard` on, the last step is a branch instead of the
  exec: the app is spawned as 999:999 with empty supplementary groups and empty capability sets,
  with `DEVKIT_CONSENT_SOCKET=/run/devkit/consent.sock` (the directory created `0700`, owned
  `999:999`) and, when the supervisor owns the ping, `DEVKIT_SUPERVISED_PING=1`; the supervisor
  stays PID 1. It reaps zombies, forwards `SIGTERM`, `SIGINT` and `SIGHUP` to the child at once
  (its loop wakes on a signal or the child's exit, and otherwise every 250 ms), and exits with
  the child's code (signal death as 128+n). Without a tunnel it drops to 999 itself before
  spawning, so no root process lingers. Every line it writes for itself also goes, timestamped,
  to `/app/persisted_data/logs/devkit-container.log`, an append-only placeholder until the
  binary logs through aeth_ext. A leftover `wireguard-heartbeat.txt` is removed at start when
  the mode is off.

  With `wireguard` on, the tunnel boots before `prepare`, in fetched mode (`WG_HUB_URL` set) or
  environment mode (the `WG_*` peer variables): `wg` and `ip` on PATH (else refused: the image
  was built without the wireguard block); `ip link add wg0 type wireguard` and the private key
  over stdin, the derived public key logged at every start; the configuration, in fetched mode
  by asking `<WG_HUB_URL>/version` for the hub's tag, fetching `peers.toml` from that tag of the
  `WG_HUB_REPO` GitHub release (the token goes to `api.github.com` only) and taking the entry
  whose key is this spoke's, with the last validated bundle cached at
  `/app/persisted_data/wireguard/peers.toml` as the fallback when GitHub is unreachable; the
  apply (`wg set peer`, `ip address add`, `ip link set up`, one route per allowed IP, and the
  endpoint last, the one command that resolves a name); then the first handshake within
  `WG_HANDSHAKE_TIMEOUT_SECS`. A boot that does not reach Connected is refused: `wg0` down, a
  `/fail` ping with the reason, exit 1. The reasons are `config unavailable`, `not enrolled`
  (naming the key to enrol; then redeploy), `endpoint unresolvable`, and no handshake in time.
  `WG_TOLERATE_DISCONNECTED=1` runs anyway, Disconnected, for an emergency where a connection
  cannot happen for an external reason; a local failure (Broken) is exit 1 either way.

  Every `WG_POLL_SECS` the supervisor reads `wg show wg0 latest-handshakes`: younger than
  `WG_STALE_SECS` is Connected (the tunnel heartbeat written, the disconnected clock reset);
  else Disconnected, with the repair alternating between re-setting the endpoint (re-resolving
  its name) and taking `wg0` down and up, each re-up logged, and in fetched mode a version check
  each poll, because a hub change is a common cause. While Connected in fetched mode the hub's
  version is checked every `WG_VERSION_POLL_SECS`; a new tag whose bundle changes this spoke's
  entry is applied in place, field by field, without the interface going down; one whose bundle
  drops this spoke ends the run: `/fail`, SIGINT to the app (30 s, then SIGKILL), `wg0` down,
  exit 75. After `WG_DISCONNECTED_LIMIT_SECS` of continuous Disconnected the supervisor asks the
  app over the consent socket (`may-shutdown wireguard-disconnected <n>s`, one line; only a
  literal `hold` back within 60 s postpones, asked again 60 s later; `WG_HOLD_LIMIT_SECS` above
  zero caps the holding) and then proceeds the same way, exit 75. A local command failing at
  runtime (Broken) stops the app with SIGTERM (30 s, then SIGKILL), brings `wg0` down, sends
  `/fail` and exits 1. Under `WG_TOLERATE_DISCONNECTED=1` there is no give-up. The heartbeat
  files and the ping are as before: while the handshake is fresh the tunnel's file is written;
  then every file the supervisor is responsible for is checked and it pings `/start` once when
  every file is first fresh, plain on every fresh poll, `/fail` with the reason on the
  transition to stale and with the code on a nonzero child exit. The URL is
  `ALERTS_HEALTHCHECK_PING_URL`, else `https://hc-ping.com/<PINGKEY>/<HEARTBEAT_SLUG>` with
  `?create=1`; the request goes out in-process over TLS (rustls, Mozilla's roots) on a thread,
  with a 10 s timeout, best-effort, one log line per failure. Without a URL, or a key with a
  slug, nothing pings and one line at start says so.
```

3. In the `[tool.docker]` schema table add two rows after `wireguard`:

```markdown
| `startup_scripts` | console script names from `[project.scripts]` that `run` executes as root, in order, before the app; default empty |
| `scrub_env` | variable names removed from the app's environment on top of the built-in secrets; default empty |
```

4. Replace the "Environment contract" table with:

```markdown
| Variable | Meaning | Required |
|---|---|---|
| `WG_PRIVATE_KEY` | this peer's private key; secret, scrubbed from the app | with `wireguard` |
| `WG_HUB_URL` | `http://host[:port]` or `https://host[:port]` of the hub, no path; present means fetched mode | fetched mode |
| `WG_HUB_REPO` | `owner/repo` of the hub's GitHub repository whose releases carry `peers.toml` | fetched mode |
| `WG_HUB_TOKEN` | a fine-grained token with read access to that repository; secret, scrubbed; sent to `api.github.com` only. Optional to the binary (a public repository needs none); the rendered compose file requires it | no |
| `WG_ADDRESS` | environment mode: this peer's tunnel address, CIDR (`10.8.0.20/32`) | environment mode |
| `WG_PEER_PUBLIC_KEY` | environment mode: the hub's public key | environment mode |
| `WG_PEER_ENDPOINT` | environment mode: `host:port` of the hub | environment mode |
| `WG_PEER_ALLOWED_IPS` | environment mode: comma-separated CIDRs routed through the hub | environment mode |
| `WG_PEER_PRESHARED_KEY` | environment mode; secret, scrubbed | no |
| `WG_PERSISTENT_KEEPALIVE` | environment mode; seconds; default 25; zero refused | no |
| `WG_POLL_SECS` | the supervisor's poll, both modes; default 30 | no |
| `WG_STALE_SECS` | handshake age past which the tunnel is Disconnected; default 180, at least 150 | no |
| `WG_HANDSHAKE_TIMEOUT_SECS` | at boot, how long the first handshake may take before the start is refused; default 60 | no |
| `WG_DISCONNECTED_LIMIT_SECS` | continuous Disconnected time after which the shutdown is pending; default 1800 | no |
| `WG_HOLD_LIMIT_SECS` | how long the app may hold a pending shutdown; default 0, no bound | no |
| `WG_VERSION_POLL_SECS` | fetched mode: interval between version checks while Connected; default 300 | no |
| `WG_TOLERATE_DISCONNECTED` | `1`: a boot that cannot connect runs Disconnected instead of being refused, and there is no give-up; for emergencies; anything but unset, empty or `1` is refused | no |
| `HEARTBEAT_SLUG` | the healthchecks.io slug; compose sets it to the service name; fallback: the single `services` entry | no |
| `PINGKEY` | the healthchecks.io ping key; with a slug, builds the autoprovisioning URL | no |
| `ALERTS_HEALTHCHECK_PING_URL` | a fixed ping URL; wins over the key | no |
| `DEVKIT_SUPERVISED_PING` | set to `1` on the app when the supervisor owns the ping; `aeth_ext` then skips its own periodic ping | set by `run` |
| `DEVKIT_CONSENT_SOCKET` | set on the app under `supervise`: the Unix socket a participating app listens on to answer `may-shutdown` with `ok` or `hold` | set by `run` |

`WG_HUB_URL` together with any environment-mode variable is refused, naming it. Every `*_SECS`
is an integer at least 1 except `WG_HOLD_LIMIT_SECS`, which accepts 0.
```

5. In "Heartbeat files" add after the two bullets:

```markdown
- `wireguard/peers.toml`, beside `logs/`: the last validated bundle, written by the supervisor
  after every successful fetch, read at boot when the hub's version endpoint or GitHub is
  unreachable. Holds no secrets.
- `logs/devkit-container.log`: the binary's own lines, timestamped, appended one at a time.
```

6. In "Tests", after the sentence about `docker_supervisor`, add:

```markdown
`docker_fetched` runs fetched mode against real GitHub: a hub container serving `/version` on the
test network, the fixture repository `AetherBreaker/wireguard-hub-smoke` with three releases (the
test spoke enrolled at two addresses, then dropped), and the test's own key pairs as constants
in the source. It reads `DEVKIT_SMOKE_WG_HUB_TOKEN` from the environment (CI: a repository
secret) and refuses to run without it. Asserts the boot fetch and the cache, a refused start
when not enrolled or without a hub, the same boot tolerated under `WG_TOLERATE_DISCONNECTED=1`,
the in-place re-apply without the interface going down, the consent hold and ok before exit 75,
and the removal shutdown.
```

- [ ] **Step 4: The todo entries of spec 15**

Append to `todo.md`:

```markdown
- A per-project `docker/wireguard/wg0.conf` as a third configuration source, for projects without
  a hub and for local development.
- Preshared keys in fetched mode (needs a per-peer secret on the hub side).
- Generating the hub's `rules.v4` from a per-peer `allow` list in `peers.toml`, removing the
  duplicated addresses.
- A data-plane probe (ping the hub's tunnel address each poll) as a second health signal.
```

- [ ] **Step 5: CI**

In `.github/workflows/ci.yml`, the container-smoke step becomes:

```yaml
      - name: cargo test --test docker_smoke --test docker_supervisor --test docker_fetched -- --ignored (builds the image, runs the entrypoint in exec, supervise, environment and fetched mode)
        env:
          DEVKIT_SMOKE_WG_HUB_TOKEN: ${{ secrets.DEVKIT_SMOKE_WG_HUB_TOKEN }}
        run: |
          uv sync
          cargo test --test docker_smoke --test docker_supervisor --test docker_fetched -- --ignored --nocapture
```

The `docker_fetched` binary arrives in task 14; until then this step lists a test that does not
exist, so land tasks 12 to 14 on one branch and push once. The render job stays as it is: it
renders through the released devkit, which refuses the `!window` marker as unknown until the
aeth-devkit release of spec 14 step 1 is out. That failure is the guard working; it is expected
until that release, and green after it.

- [ ] **Step 6: Verify and commit**

Run: `cargo test --test docker_smoke -- --ignored --nocapture` (Docker needed): green, the
window markers pass through the local gate strip.

```bash
git add python/devkit_container/compose.template.yaml python/devkit_container/template.Dockerfile README.md todo.md .github/workflows/ci.yml
git commit -m "docs(templates,readme): the fetched-mode compose lines, the Dockerfile windows, the contract and the todo entries

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 13: the fixture repository, the key pairs and the CI secret

External state, owner-visible: a private repository under the owner's account, three releases,
and one repository secret in this repository. The owner approved all of it in spec 11. The token
itself can only be created in the GitHub UI, so this task stops for the owner at step 5.

**Files:**

- Create: `AetherBreaker/wireguard-hub-smoke` on GitHub (a README and three releases)
- Record: the four key values for task 14, in the scratchpad (never in this repo except as the
  task 14 constants)

- [ ] **Step 1: Generate the two key pairs**

```bash
docker run --rm ghcr.io/astral-sh/uv:python3.14-bookworm-slim sh -c 'apt-get update -qq >/dev/null && apt-get install -y -qq wireguard-tools >/dev/null 2>&1 && for n in hub spoke; do k=$(wg genkey); echo "$n private $k"; echo "$n public $(echo "$k" | wg pubkey)"; done'
```

Expected: four lines. Keep them in a scratch file for task 14; they are test fixtures, not
secrets, and the spoke's and hub's private keys will be committed as constants there.

- [ ] **Step 2: Create the repository with one commit**

```bash
cd "$SCRATCH" && gh repo create AetherBreaker/wireguard-hub-smoke --private --description "Fixture releases for devkit-container's fetched-mode smoke test" --clone
cd wireguard-hub-smoke
printf '# wireguard-hub-smoke\n\nFixture releases for the fetched-mode smoke test of devkit-container. Every key in every release is a throwaway that guards nothing.\n' > README.md
git add README.md && git commit -m "Fixture repository" && git push -u origin main
```

`$SCRATCH` is the session's scratchpad directory.

- [ ] **Step 3: The three releases**

With `HUB_PUB` and `SPOKE_PUB` from step 1, in the cloned fixture repository:

```bash
release() {   # tag, spoke address or empty for none
  mkdir -p "$1" && {
    printf 'hub_version = "%s"\nschema = 1\n\n[hub]\nname = "wireguard-hub"\npublic_key = "%s"\naddress = "10.8.0.1/24"\nlisten_port = 51820\nendpoint = "wireguard-hub:51820"\nallowed_ips = ["10.8.0.0/24"]\npersistent_keepalive = 1\n' "$1" "$HUB_PUB"
    [ -n "$2" ] && printf '\n[[peers]]\nname = "smoke-spoke"\npublic_key = "%s"\naddress = "%s"\n' "$SPOKE_PUB" "$2"
  } > "$1/peers.toml"
  gh release create "$1" --title "$1" --notes "smoke fixture" "$1/peers.toml"
}
release v0.1.0 10.8.0.20/32
release v0.2.0 10.8.0.21/32
release v0.3.0 ""
gh release list
```

Expected: three releases listed, each with a `peers.toml` asset. The keepalive of 1 s is the
fixture's, for the test's short timings; the hub endpoint `wireguard-hub:51820` is the network
alias the test gives its hub container.

- [ ] **Step 4: Validate the three files with the binary's parser**

Run, from this repository: `cargo test bundle` already covers the format; additionally check
each fixture parses by running the spoke's own validation on it once task 14's test runs.
Nothing to do here beyond eyeballing the three files: `schema`, `hub_version` equal to the tag,
the hub key, the spoke key and address.

- [ ] **Step 5: The token (owner's step)**

Stop and ask the owner to create the token at
https://github.com/settings/personal-access-tokens/new with: resource owner `AetherBreaker`,
repository access "Only select repositories" → `wireguard-hub-smoke`, repository permission
Contents: Read-only, expiration one year, name `devkit-container smoke fixture`. Then, with the
value in the shell as `TOKEN`:

```bash
printf '%s' "$TOKEN" | gh secret set DEVKIT_SMOKE_WG_HUB_TOKEN
curl -sS -H "Authorization: Bearer $TOKEN" -H "Accept: application/vnd.github+json" -H "X-GitHub-Api-Version: 2022-11-28" https://api.github.com/repos/AetherBreaker/wireguard-hub-smoke/releases/tags/v0.1.0 | grep -c '"name": "peers.toml"'
```

Expected: `1` from the `grep`, and `gh secret list` shows `DEVKIT_SMOKE_WG_HUB_TOKEN`. For local
runs the owner keeps the value in `DEVKIT_SMOKE_WG_HUB_TOKEN` in their shell; it is never
written to the repository.

No commit: this task changes nothing in this repository.

---

### Task 14: `tests/docker_fetched.rs`, the fetched-mode smoke test

**Files:**

- Modify: `tests/common/mod.rs` (move `wg_key` here), `tests/docker_supervisor.rs` (use it from `common`)
- Create: `tests/docker_fetched.rs`
- Test: `cargo test --test docker_fetched -- --ignored --nocapture` with `DEVKIT_SMOKE_WG_HUB_TOKEN` set

**Interfaces:**

- Consumes: the four key values of task 13; `common::{build_image, docker, ok, text, Cleanup, root}`.

- [ ] **Step 1: Share `wg_key`**

Move `fn wg_key(image: &str) -> (String, String)` from `tests/docker_supervisor.rs` into
`tests/common/mod.rs` as `pub fn wg_key`, unchanged; the supervisor test keeps calling it
through `use common::*;`.

- [ ] **Step 2: Write the test**

Create `tests/docker_fetched.rs`, filling the four constants from task 13:

```rust
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
const HUB_PRIVATE: &str = "<hub private from task 13>"; // gitleaks:allow trufflehog:ignore ggignore
const HUB_PUBLIC: &str = "<hub public from task 13>";
const SPOKE_PRIVATE: &str = "<spoke private from task 13>"; // gitleaks:allow trufflehog:ignore ggignore
const SPOKE_PUBLIC: &str = "<spoke public from task 13>";

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
    docker(&["run", "-d", "--name", &hub, "--network", &net, "--network-alias", "wireguard-hub", "--cap-add", "NET_ADMIN"])
      .args(["-e", &format!("HUB_KEY={HUB_PRIVATE}"), "-e", &format!("VERSION_SERVER={VERSION_SERVER}"), "--entrypoint", "sh", &image])
      .args([
        "-c",
        &format!(
          "printf '%s\\n' \"$HUB_KEY\" > /tmp/k && ip link add dev wg0 type wireguard && wg set wg0 listen-port 51820 private-key /tmp/k peer {SPOKE_PUBLIC} allowed-ips 10.8.0.20/32 && ip address add 10.8.0.1/24 dev wg0 && ip link set up dev wg0 && echo v0.1.0 > /tmp/version && exec /app/.venv/bin/python -c \"$VERSION_SERVER\""
        ),
      ]),
  );
  wait_for("the hub's version endpoint", Duration::from_secs(30), &[&hub], || {
    exec(&hub, &["sh", "-c", "wg show wg0 public-key && cat /tmp/version"]).status.success()
  });
  let hub_pub = text(&exec_ok(&hub, &["wg", "show", "wg0", "public-key"])).trim().to_string();
  assert_eq!(hub_pub, HUB_PUBLIC, "the constants match the fixture");

  // --- a fresh key is not enrolled: refused, the key in the log.
  let (fresh_priv, fresh_pub) = wg_key(&image);
  let stranger = format!("stranger-{id}");
  guard.containers.push(stranger.clone());
  ok(
    docker(&["run", "-d", "--name", &stranger, "--network", &net, "--cap-add", "NET_ADMIN", "-v", &mount])
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
    docker(&["run", "-d", "--name", &tolerated, "--network", &net, "--cap-add", "NET_ADMIN", "-v", &mount])
      .args(spoke_env(&token, &fresh_priv, &[("WG_TOLERATE_DISCONNECTED", "1")]))
      .arg(&image),
  );
  wait_for("the tolerated app's report", Duration::from_secs(60), &[&tolerated], || {
    exec(&tolerated, &["cat", "/app/persisted_data/report.json"]).status.success()
  });
  let log = logs(&tolerated);
  assert!(log.contains("starting Disconnected") && log.contains("not enrolled"), "{log}");
  let hc = healthcheck(&tolerated);
  assert!(String::from_utf8_lossy(&hc.stderr).contains("wireguard-heartbeat.txt"), "unhealthy, naming the tunnel file");
  ok(&mut docker(&["rm", "-f", &tolerated]));
  ok(&mut docker(&["volume", "rm", "-f", &guard.volume]));

  // --- no hub and no cache: refused with config unavailable.
  let lost = format!("lost-{id}");
  guard.containers.push(lost.clone());
  ok(
    docker(&["run", "-d", "--name", &lost, "--network", &net, "--cap-add", "NET_ADMIN", "-v", &mount])
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
    docker(&["run", "-d", "--name", &spoke, "--network", &net, "--cap-add", "NET_ADMIN", "-v", &mount])
      .args(spoke_env(&token, SPOKE_PRIVATE, &[]))
      .arg(&image),
  );
  wait_for("both heartbeats", Duration::from_secs(90), &[&spoke, &hub], || {
    exec(&spoke, &["cat", "/app/persisted_data/logs/wireguard-heartbeat.txt"]).status.success()
      && exec(&spoke, &["cat", "/app/persisted_data/logs/heartbeat.txt"]).status.success()
  });
  assert!(healthcheck(&spoke).status.success());
  let log = logs(&spoke);
  assert!(log.contains("fetched bundle v0.1.0") && log.contains("applied v0.1.0") && log.contains("wireguard handshake"), "{log}");
  let stat = text(&exec_ok(&spoke, &["stat", "-c", "%a %u", "/app/persisted_data/wireguard/peers.toml"]));
  assert_eq!(stat.trim(), "644 999", "the cache file's mode and owner");
  let cached = text(&exec_ok(&spoke, &["cat", "/app/persisted_data/wireguard/peers.toml"]));
  assert!(cached.contains("hub_version = \"v0.1.0\""));
  let logfile = text(&exec_ok(&spoke, &["cat", "/app/persisted_data/logs/devkit-container.log"]));
  assert!(logfile.contains("applied v0.1.0"), "{logfile}");
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
  assert_eq!(text(&exec_ok(&spoke, &["cat", "/sys/class/net/wg0/ifindex"])), ifindex, "wg0 never went down");
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
  let consent_log = text(&ok(docker(&["run", "--rm", "-v", &mount, "--entrypoint", "cat", &image]).arg("/app/persisted_data/consent.log")));
  assert!(consent_log.contains("signal SIGINT"), "{consent_log}");
  ok(&mut docker(&["rm", "-f", &spoke]));
  ok(docker(&["run", "--rm", "-v", &mount, "--entrypoint", "rm", &image]).args(["-f", "/app/persisted_data/consent.log", "/app/persisted_data/report.json"]));

  // --- a new spoke boots at v0.2.0; the hub releases v0.3.0 without it: alert, SIGINT, exit 75.
  exec_ok(&hub, &["wg", "set", "wg0", "peer", SPOKE_PUBLIC, "allowed-ips", "10.8.0.21/32"]);
  let last = format!("last-{id}");
  guard.containers.push(last.clone());
  ok(
    docker(&["run", "-d", "--name", &last, "--network", &net, "--cap-add", "NET_ADMIN", "-v", &mount])
      .args(spoke_env(&token, SPOKE_PRIVATE, &[]))
      .arg(&image),
  );
  wait_for("the new spoke Connected", Duration::from_secs(90), &[&last, &hub], || healthcheck(&last).status.success());
  exec_ok(&hub, &["sh", "-c", "echo v0.3.0 > /tmp/version"]);
  wait_for("the removal", Duration::from_secs(30), &[&last, &hub], || {
    logs(&last).contains("removed from the hub's peer table in v0.3.0")
  });
  assert_eq!(wait_code(&last), "75");
  let consent_log = text(&ok(docker(&["run", "--rm", "-v", &mount, "--entrypoint", "cat", &image]).arg("/app/persisted_data/consent.log")));
  assert!(!consent_log.contains("may-shutdown"), "no ask on a removal: {consent_log}");
  assert!(consent_log.contains("signal SIGINT"), "{consent_log}");
}
```

- [ ] **Step 3: Run it**

Run, with the token in the environment: `cargo test --test docker_fetched -- --ignored --nocapture`
Expected: green in roughly ten minutes; the stale window (up to 150 s) and the two 60 s ask
intervals are most of it. The test's assertions map one to one onto the smoke list of spec 13.
If the local Docker kernel lacks the WireGuard module, push the branch and read the CI run
instead; the task is not done until that run is green.

- [ ] **Step 4: Lint and commit**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings`

```bash
git add tests/common/mod.rs tests/docker_supervisor.rs tests/docker_fetched.rs
git commit -m "test(smoke): fetched mode against the fixture release: boot, cache, refusals, re-apply, consent, removal

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 15: final verification

**Files:** none new.

- [ ] **Step 1: Everything on Windows**

Run: `cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: green. The Linux-only modules are absent here; what runs is the bundle, health, cache,
log file, planner, settings and pyproject tests, plus the query subcommands.

- [ ] **Step 2: Everything on Linux (CI)**

Push the branch and read the CI run: the Rust job on both runners, the wheel job, the smoke job
(three test binaries), and the render job. The render job fails with `unknown marker` until the
aeth-devkit release of spec 14 step 1 is out; every other job must be green.

- [ ] **Step 3: The spec's done list**

Check against the spec's section 13, one line each: the unit list, the render note, the smoke
list, and confirm nothing in sections 5 to 9 lacks a test or a smoke assertion. Then the plan's
own checklist: every task's boxes ticked.

- [ ] **Step 4: Hand back**

Report to the owner: the branch, what CI says, and the two items outside this repository that
gate a release: the aeth-devkit release (kept jobs and windows) and the fixture token being in
place. Do not merge or release; those are the owner's, per spec 14.
