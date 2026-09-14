# WireGuard hub, fetched peer configuration, startup scripts and shutdown consent

Date: 2026-09-14. Status: design approved in discussion, in the ScheduledReportAggregator session
that owns the tunnel design. This document is the draft of record until the grounding pass in
section 0.3 freezes it.

Predecessors: `2026-09-08-container-wireguard-mode-design.md` in this repo (the spoke-side
supervisor, tunnel and heartbeat this document changes) and
`ScheduledReportAggregator/docs/superpowers/specs/2026-09-08-wireguard-db-access-design.md` (the
tunnel-inside-the-app-container decision, the topology and the first-deploy checklist). Section 12
lists what this document supersedes in each.

## 0. How this document is used

### 0.1 Scope: the whole shape, in one place

This document describes every piece of the change across every repository it touches, not only
the `devkit-container` piece. Each piece is implemented in its own repository from its own section
here, and the sections depend on each other. Whoever changes a section during implementation
updates every other section that relies on it in the same edit, so no piece is implemented
against text that has gone stale. The dependency map:

| Section | Implemented in | Depends on |
| --- | --- | --- |
| 3 The hub project | `wireguard-hub` (new repo) | 4, 7, 8, 9 |
| 4 The bundle contract | `wireguard-hub` (producer) and this repo (consumer) | 3.2, 3.7 |
| 5 Spoke behaviour | this repo (`run`) | 4, 8 |
| 6 Shutdown consent | this repo (`run`) and `aeth_ext` | 5.4, 8 |
| 7 Startup scripts and `scrub_env` | this repo (`run`) | 8, 9 |
| 8 Environment contract | this repo (README), every consumer | 5, 6, 7 |
| 9 Templates and `[tool.docker]` | this repo (package data) | 3.6, 8, 10 |
| 10 Consuming projects | ScheduledReportAggregator, the test project, the office PC | 4, 8, 9 |

### 0.2 Source of truth, and the stop-and-ask rule

These rules exist because the implementation plan written from this document will be executed
largely unreviewed. They are not advisory.

1. **This document is the source of truth for the implementation plan.** Where the plan is
   ambiguous, or the plan and this document disagree, this document decides.
2. **Where this document is silent, incomplete or contradictory on a point the implementation
   needs, the implementer stops and asks the owner.** Nobody fills a gap with their own judgement:
   not the plan's author, not the agent executing it. This includes naming, defaults, error text,
   ordering, file locations, retry counts, and "the code already does X so I will keep X". However
   small or obvious the gap looks, the owner becomes the source of truth for it before anything
   else continues, and the answer is written into this document before the plan or the code
   changes.
3. **The plan carries rules 1 and 2 verbatim in its own preamble**, so an agent that reads only the
   plan still sees them.
4. **The plan is executed inline** by the session that holds it, not delegated to subagents.
5. **After the grounding pass (0.3) this document is frozen.** A change after that point requires
   the owner's explicit decision, recorded here first.

### 0.3 Lifecycle

1. Written in the ScheduledReportAggregator session and committed to this repo. This is that step.
2. **Grounding pass.** A session in this repo re-reads the document against the actual code, with
   the owner in the discussion. Every note marked *verify in grounding* is resolved and the marker
   removed. Every decision reserved for the owner in section 11 is answered and written in. The
   document is then frozen.
3. The plan for this repo's piece is written (the writing-plans skill) and executed inline.
4. The other repositories' pieces each get a plan from their own sections of this same document,
   in the release order of section 14. A change discovered during any piece is written back here
   before the piece continues.

## 1. Summary

A spoke no longer receives its peer configuration through five environment variables. It asks the
hub which version is deployed, fetches that version's peer bundle from the hub's GitHub release,
finds its own entry by public key, and configures its interface from that, injecting its private
key over stdin as today. A running spoke re-checks the hub's version every five minutes and applies
a changed configuration in place. The hub is a single devkit-managed Python project that brings up
its own interface as root in a startup script the entrypoint runs before dropping privileges, then
serves its version string and writes a heartbeat as an ordinary unprivileged app. The hub's
committed peer table is the single source for both sides: the hub configures itself from it at
start, and its release workflow validates it, stamps it with the release tag, attaches it as the
bundle, and renders one human-readable `.conf` per peer for peers that are not containers.

```
 wireguard-hub repo                    Coolify VPS                          GitHub
 +------------------+   release   +--------------------+            +------------------+
 | peers.toml       | ----------> | wireguard-hub      |            | release vX.Y.Z   |
 | rules.v4         |  (devkit    |  startup script:   |            |  peers.toml      |
 | src/wireguard_hub|   release)  |   wg0 from table   |            |  <peer>.conf ... |
 +------------------+             |  app: /version,    |            +--------+---------+
                                  |       heartbeat    |                     |
                                  +-------+------------+                     |
                     UDP 51820 <----------+ ^ GET /version                   | GET bundle (token)
                                            |                                |
                                  +---------+--------------------------------v---+
                                  | spoke container (devkit-container run)       |
                                  |  1. wg0 + private key   (local: Broken?)     |
                                  |  2. version -> bundle -> my entry -> apply   |
                                  |  3. spawn app; poll; re-check every 5 min    |
                                  |  4. disconnected 30 min -> ask app -> SIGINT |
                                  +----------------------------------------------+
```

## 2. Decisions taken, and what was rejected

Recorded so the grounding pass does not reopen them.

- **The hub owns both sides of every peering.** Its committed peer table plus a `[hub]` section is
  the only hand-edited input; the spoke's whole configuration except its private key derives from
  it. Rejected: a per-project `docker/wireguard/wg0.conf` as the primary source (each project would
  hold a copy of hub facts); the supervisor templating a config itself (nothing left to template
  once the hub renders).
- **Deployed version, resolved by asking the hub.** The spoke fetches the bundle at the tag the
  running hub reports, so what the spoke applies is by construction what the hub is running.
  Rejected: fetching GitHub's latest release (a skew window after each hub release); the hub
  serving the bundle itself (a live file with no version, so the contract could change under a
  running spoke; and one more thing on the hub).
- **A file at a git tag, not a file on a server.** Immutable bytes per tag let the bundle format be
  versioned like any API.
- **No pinning of the hub's public key on spokes.** The owner trusts the GitHub account more than
  the DNS name. A hijacked name can therefore only choose which real tag is fetched, so the version
  string is validated as a strict tag before any URL is built from it.
- **Public, unauthenticated version endpoint.** Reveals one string.
- **Private hub repo, token per spoke.** A fine-grained token is a plain string in the environment,
  scrubbed from the app like the private key. Rejected: public repo (the roster and the rules file
  would be readable by anyone).
- **Peer identity by public key.** The spoke derives its public key from its private key, as it
  already does, and picks the bundle entry whose key matches. No name exists on the spoke side.
  Rejected: by service name (two repos must agree); a dedicated name variable (one more value).
- **One bundle file per release**, structured TOML, not one file per peer and not wg-quick text.
  The binary already parses TOML; an INI parser would be new code; a directory listing or probing
  would be extra requests. The wg-quick `.conf` is rendered per peer alongside, for humans.
- **The hub is one devkit-managed Python project that owns its tunnel.** Root work happens in a
  startup script the entrypoint runs before dropping privileges; the long-running app is
  unprivileged. Rejected: the stock `linuxserver/wireguard` image beside a Python app (a second
  init system, a shared volume, start ordering, the hub's private key written to a file, rules in
  that image's dialect); a hub mode in the Rust binary (more Rust to move fifty lines of subprocess
  calls out of Python); the Python app dropping privileges itself (needs a "run the app as root"
  switch in the binary, a standing footgun); the entrypoint supervising the wireguard image
  (breaks every devkit Docker assumption at once).
- **Startup scripts as a generic feature, minimal surface.** One ordered list of console script
  names, run as root, no arguments, no timeout, nonzero exit ends the container. `scrub_env`
  generalises the private-key scrubbing. Neither key is added to the pyproject template.
- **Health model: Broken, Disconnected, Connected.** Local failure exits at once; a missing hub
  makes the container unhealthy and alerting but running; 30 minutes of continuous disconnection
  triggers a shutdown. The app starts as soon as the local bring-up succeeds, without waiting for
  the first handshake, because most of a spoke's work does not need the tunnel and the runtime
  model already accepts running with the hub down. Rejected: holding the app for the first
  handshake; exiting after 60 s as today.
- **`restart: no` stays.** A hub outage longer than 30 minutes stops every spoke until it is
  redeployed. Chosen for an unambiguous state; the owner accepted the manual redeploy.
- **Shutdown consent over a Unix socket the app opens if it participates.** Non-participation is
  indistinguishable from "go ahead", so no existing app changes. No upper bound on holding by
  default; a knob exists. Rejected: two timestamp files (the app cannot learn a shutdown is pending
  without polling); loopback TCP (works on Windows dev boxes, but the supervisor never runs there).
- **Version polling every five minutes with in-place re-apply.** Closes the case where a hub
  release changes a spoke's address: the handshake stays fresh while the hub drops the spoke's
  data, so nothing else would ever re-fetch. Rejected: re-fetch only during repair; a data-plane
  probe (adds little once polling exists).
- **Cached last-good bundle in persisted data.** Lets a spoke reach Connected when GitHub is down
  but the hub is not. The bundle holds no secrets.
- **Environment rendering rule.** A variable is rendered into the compose template only if at
  least one app will set it at first deployment. Everything else is documented, not rendered.
- **Out of scope, recorded as TODO at implementation** (section 15): a per-project conf file
  source; preshared keys in fetched mode; generating the firewall rules from the peer table; a
  thread-based consent helper; a data-plane probe. A hub mode in the binary is dropped, not
  deferred.

## 3. The hub project: `wireguard-hub`

Repository `AetherBreaker/wireguard-hub`, private. Python package `wireguard_hub`, compose service
and container name `wireguard-hub`, healthchecks.io slug `wireguard-hub`. A devkit-managed project
like every other: `setup-project`, the devkit release command, the standard Dockerfile and compose
scaffold, `aeth_ext` for heartbeat and alerts. Depends on sections 4, 7, 8 and 9.

### 3.1 Shape

Two programs in one package, both console scripts:

- `wireguard-hub-up`: the startup script (section 7). Runs once as root before the app. Brings the
  interface up from the peer table and applies the firewall rules. Exits.
- `run-app-wireguard-hub`: the app. Unprivileged. Serves the version endpoint and writes the
  heartbeat. Nothing else.

Package data, inside `src/wireguard_hub/` because the Dockerfile copies `src/` and not `docker/`:

- `peers.toml`: the peer table and hub section (3.2). The single hand-edited source.
- `rules.v4`: the iptables rules (3.3).

### 3.2 `peers.toml`: the source, and the bundle

The committed file is the bundle minus one stamped field. Format, with the address plan decided in
section 11:

```toml
schema = 1

[hub]
name = "wireguard-hub"
public_key = "<hub public key, base64>"
address = "10.8.0.1/24"                        # interface address; its network is the tunnel subnet
listen_port = 51820
endpoint = "tunnels.sweetfiretobacco.com:51820"
allowed_ips = ["10.8.0.0/24"]                  # default AllowedIPs for every peer
persistent_keepalive = 25                      # default for every peer

[[peers]]
name = "office-db-pc"
public_key = "<base64>"
address = "10.8.0.10/32"

[[peers]]
name = "scheduled-report-aggregator"
public_key = "<base64>"
address = "10.8.0.20/32"
# Optional per-peer overrides, each with the meaning of the [hub] default it replaces:
# endpoint = "wireguard-hub:51820"      a VPS-side peer's fallback if the hairpin check fails
# allowed_ips = ["10.8.0.10/32"]
# persistent_keepalive = 25

[[peers]]
name = "<test project name>"
public_key = "<base64>"
address = "10.8.0.21/32"
```

The release stamps `hub_version = "vX.Y.Z"` at the top level (3.7). Nothing else differs between
the committed file and the bundle asset.

Validation, applied identically by the startup script at hub start, by the release workflow, and
by the hub's tests: `schema == 1`; every `name` unique and matching `^[a-z0-9][a-z0-9-]*$`; every
`public_key` unique and a 44-character base64 string decoding to 32 bytes; every peer `address` a
`/32` inside the network of `hub.address`, unique, and not the hub's own address; `hub.address` a
CIDR with a host part; `endpoint` (hub and overrides) `host:port` with `port` in 1 to 65535;
`listen_port` in 1 to 65535; `persistent_keepalive` (hub and overrides) an integer 1 to 65535;
`allowed_ips` (hub and overrides) a non-empty list of CIDRs. Any failure names the field.

### 3.3 `rules.v4`

An `iptables-restore` file for the `filter` table, applied whole by the startup script. Policy:
`FORWARD DROP`; accept `ESTABLISHED,RELATED` on `wg0` to `wg0`; one `ACCEPT` line per permitted
flow, from a spoke's `/32` to the database PC's `/32` on the database port and protocol. Tunnel
addresses in this file duplicate `peers.toml`; that is accepted for this spec, and a hub test
asserts every address in `rules.v4` is an address in `peers.toml`. Generating the rules from the
table is a TODO (section 15). The `INPUT` chain is not touched: Docker publishes the UDP port and
the container's default `INPUT` policy accepts.

### 3.4 The startup script `wireguard-hub-up`

Runs as root with the full environment (section 7). In order:

1. Load and validate `peers.toml` (3.2). Read `WG_HUB_PRIVATE_KEY`; empty or missing is an error
   naming the variable.
2. Check `/proc/sys/net/ipv4/ip_forward` reads `1`; otherwise fail with
   `net.ipv4.ip_forward is 0: the compose file needs sysctls net.ipv4.ip_forward=1`. The script
   does not write the sysctl: inside a container that requires the compose entry anyway.
3. `ip link add dev wg0 type wireguard`.
4. `wg set wg0 listen-port <hub.listen_port> private-key /dev/stdin`, the key piped to stdin.
5. For each peer: `wg set wg0 peer <public_key> allowed-ips <address>`. No endpoint: spokes
   initiate.
6. `ip address add <hub.address> dev wg0`; `ip link set up dev wg0`.
7. `iptables-restore < rules.v4`.
8. Log the hub's derived public key (`wg pubkey` on stdin) and the peer count. Exit 0.

Any command failing is an error naming the command, never the key. The script uses `subprocess`
with argument lists, never a shell string.

### 3.5 The app `run-app-wireguard-hub`

- An HTTP server on `0.0.0.0:8000` using the standard library's threading server, in a thread.
  `GET /version` answers `200`, `Content-Type: text/plain; charset=utf-8`, body
  `v<package version>\n` where the package version comes from `importlib.metadata` and the `v`
  prefix matches the devkit release tag. Every other path answers `404`. No other routes.
- The heartbeat, written with `aeth_ext`'s existing helper to `/app/persisted_data/logs/heartbeat.txt`
  every 60 s, but only while `/sys/class/net/wg0` exists. An interface that vanished stops the
  beats, the standard healthcheck turns unhealthy, and healthchecks.io alerts through the
  standard ping. The app cannot inspect handshakes (`wg show` needs `NET_ADMIN`); reachability of
  the hub is what the spokes' own health reports.
- Shutdown on SIGINT/SIGTERM. Nothing else.

### 3.6 `pyproject.toml` and compose

```toml
[tool.docker]
services                = ["wireguard-hub"]
required_persisted_dirs = ["persisted_data"]
supervise               = false
wireguard               = false
wireguard_hub           = true
startup_scripts         = ["wireguard-hub-up"]
scrub_env               = ["WG_HUB_PRIVATE_KEY"]
```

`setup-project` renders, from the template blocks in section 9: `cap_add: [NET_ADMIN]`,
`sysctls: [net.ipv4.ip_forward=1]`, `ports: ["51820:51820/udp"]`, the `WG_HUB_PRIVATE_KEY`
environment line, and the standard single-file healthcheck. The Dockerfile installs
`wireguard-tools`, `iproute2` and `iptables`. Coolify: the domain `tunnels.sweetfiretobacco.com`
is attached to the `wireguard-hub` service on container port 8000 in the Coolify UI, which
provides the Traefik route and the certificate; the UDP port is published directly by compose and
never passes through Traefik. Environment values: `WG_HUB_PRIVATE_KEY`, plus the standard
`PINGKEY` and alert values every project has.

### 3.7 Release

The devkit release command is used unchanged. The repository's release workflow gains one job
that runs after the tag exists: validate `peers.toml` (3.2), write a copy with
`hub_version = "<tag>"` inserted as the first top-level key, and render `<peer name>.conf` for
every peer:

```ini
[Interface]
PrivateKey = REPLACE_WITH_THIS_PEERS_PRIVATE_KEY
Address = <peer address>

[Peer]
PublicKey = <hub public key>
Endpoint = <peer endpoint override, else hub endpoint>
AllowedIPs = <peer allowed_ips override, else hub allowed_ips, comma-separated>
PersistentKeepalive = <peer override, else hub default>
```

All of these are attached to the GitHub release as assets: `peers.toml` and one `.conf` per peer.
The job fails the release if validation fails. Every push also runs the validation in CI, so a
broken table is caught before a release is attempted.

### 3.8 Enrolling a peer

A spoke logs its derived public key at every start (existing behaviour). Enrolment is: add a
`[[peers]]` row with that key and the next address from the plan, add the flow to `rules.v4` if the
peer may reach the database, release the hub. Running spokes pick the change up within the
version poll interval (5.5). Nothing on the spoke changes.

## 4. The bundle contract

Between a hub release (producer) and every container spoke (consumer). Depends on 3.2 and 3.7.

### 4.1 The version endpoint

`GET <WG_HUB_URL>/version`. Success is HTTP 200 with a body that, after stripping one trailing
newline, matches `^v[0-9]+\.[0-9]+\.[0-9]+$`. Anything else, including a body that fails the
match, is "unreachable" for the purposes of section 5; the body is logged truncated to 64 bytes.
Request timeout 10 s. `WG_HUB_URL` is a URL with scheme `http` or `https`, no path, no trailing
slash; a trailing slash is stripped, anything else in the path is refused at start. `http` is
accepted for a hub reached over a private Docker network; over the public name it is `https`.

### 4.2 Fetching the bundle

With `WG_HUB_TOKEN` set (the decided configuration), against the GitHub REST API, with headers
`Authorization: Bearer <token>`, `Accept: application/vnd.github+json`,
`X-GitHub-Api-Version: 2022-11-28`, `User-Agent: devkit-container/<version>`, each request with
a 10 s timeout:

1. `GET https://api.github.com/repos/<WG_HUB_REPO>/releases/tags/<tag>`. Expect 200 and a JSON
   body with an `assets` array.
2. Take the asset whose `name` is `peers.toml`; its `url` field is the API asset URL. A missing
   asset is a failure naming the tag.
3. `GET <asset url>` with the same headers except `Accept: application/octet-stream`, **with
   automatic redirects disabled**. Expect 302 with a `Location`.
4. `GET <Location>` **with no `Authorization` header**. Expect 200; the body is the bundle. The
   token is never sent to any host but `api.github.com`.

Without `WG_HUB_TOKEN`: `GET https://github.com/<WG_HUB_REPO>/releases/download/<tag>/peers.toml`,
following redirects. This is the public-repo path; it is supported but not the decided
configuration.

The body is capped at 1 MiB and parsed as TOML; then validated as in 3.2 plus: `hub_version` is
present and equals the tag requested. Any failure at any step is "config unavailable" for section
5, never Broken. Failures name the step and the HTTP status; never the token, never the body.

*Verify in grounding:* whether the binary's HTTP client can disable redirects per request and
whether a JSON parser is already a dependency; if not, the plan adds one, pinned.

### 4.3 Selecting the entry and the effective configuration

The spoke's public key is derived from `WG_PRIVATE_KEY` (existing). The entry is the `[[peers]]`
row whose `public_key` equals it; none is "not enrolled" for section 5. The effective
configuration is:

| Field | Value |
| --- | --- |
| address | the entry's `address` |
| hub public key | `hub.public_key` |
| endpoint | the entry's `endpoint`, else `hub.endpoint` |
| allowed IPs | the entry's `allowed_ips`, else `hub.allowed_ips` |
| keepalive | the entry's `persistent_keepalive`, else `hub.persistent_keepalive` |

Everything else in the bundle (`hub.listen_port`, `hub.address`, other peers) is ignored by the
spoke. Two effective configurations are equal when all five fields are equal, allowed IPs compared
as sets.

### 4.4 The cache

After every successful fetch and validation, the bundle is written atomically, mode 0644, to
`/app/persisted_data/logs/wireguard-peers.toml`, beside the heartbeat files, because that
directory is the bind mount every spoke already has. At boot, when the version endpoint or the
fetch fails, the cache is read and validated (3.2) and, if it holds an entry for this spoke, used
as the configuration, logged as `using cached bundle <hub_version>`. A fresh fetch always wins over
the cache. A cache that fails validation is ignored and overwritten by the next successful fetch.

## 5. Spoke behaviour in `devkit-container run`

Changes to the supervisor's wireguard mode. Depends on sections 4 and 8. Everything not mentioned
here (the poll, the tunnel heartbeat file, the healthcheck subcommand, the ping and its ownership,
`HEARTBEAT_SLUG`, the privilege drop, signal forwarding, zombie reaping) is unchanged from the
predecessor spec.

### 5.1 Modes

`WG_HUB_URL` present means **fetched mode** (this document). Absent means **environment mode**,
the predecessor's contract with `WG_ADDRESS`, `WG_PEER_PUBLIC_KEY`, `WG_PEER_ENDPOINT`,
`WG_PEER_ALLOWED_IPS`, `WG_PEER_PRESHARED_KEY` and `WG_PERSISTENT_KEEPALIVE`, kept for projects
that have not migrated. `WG_HUB_URL` together with any of those six is refused at start, naming
the conflicting variable. `WG_HUB_REPO` is required in fetched mode; `WG_HUB_TOKEN` is optional to
the binary (4.2). The health model, timers, consent and exit codes of 5.3 to 5.6 apply in both
modes; version polling and the cache apply in fetched mode only.

### 5.2 Boot sequence, fetched mode

Root check, `pyproject.toml`, resolution of the run script and the startup scripts, and the mount
check, as today. Then:

1. Preflight (`wg`, `ip` on PATH), `ip link add dev wg0 type wireguard`, `wg set wg0 private-key`
   over stdin, log the derived public key. A failure here is **Broken** (5.3).
2. Obtain a configuration: version endpoint (4.1), then fetch (4.2), then select (4.3). On any
   failure, the cache (4.4). On no usable configuration, continue with none; the tunnel state is
   Disconnected with reason `config unavailable` or `not enrolled`.
3. If a configuration was obtained, apply it: `wg set wg0 peer <hub key> endpoint <endpoint>
   allowed-ips <cidrs> persistent-keepalive <n>`, `ip address add <address> dev wg0`,
   `ip link set up dev wg0`, one `ip route replace <cidr> dev wg0` per allowed IP. Classification
   of failures per 5.3.
4. `prepare`, startup scripts (none for a spoke unless declared), scrubbing, privilege drop, spawn
   the app. **The app starts here regardless of tunnel state.**
5. Enter the poll loop (5.4) with the disconnected clock at zero and the boot-alert timer running.

### 5.3 The three states, and classification

Evaluated every poll. Every transition is logged with its reason.

- **Broken.** A local operation failed: the interface cannot be created, a key, address or route is
  rejected by the kernel, a tool is missing, a startup script failed. Never the hub. The supervisor
  brings the interface down, sends `/fail` with the error (best effort), and exits 1 with an
  `error:` line naming the failing command. Immediate, no retry.
- **Disconnected.** The interface exists and holds the private key, but there is no fresh handshake
  (older than `WG_STALE_SECS`, or none), or no configuration has been obtained. The tunnel
  heartbeat is not written, so Docker turns unhealthy. Repairs run every poll (5.6). The
  continuous-disconnected clock runs.
- **Connected.** `wg show wg0 latest-handshakes` reports a handshake younger than `WG_STALE_SECS`.
  The tunnel heartbeat is written, the clock resets, a pending shutdown (section 6) is cancelled.

| Operation | A failure means |
| --- | --- |
| `wg` or `ip` missing; `ip link add`; `wg set private-key`; `ip address add/replace/delete`; `ip link set up`; `ip route replace/delete`; `wg set peer ... allowed-ips/persistent-keepalive`; `wg set peer ... remove`; `wg show` on an existing interface; `ip link delete` followed by a failed `ip link add` | Broken |
| `wg set peer ... endpoint <host:port>` (resolves the hub's name) | Disconnected, reason `endpoint unresolvable` |
| version endpoint unreachable; fetch failure; bundle invalid; cache unusable | Disconnected, reason `config unavailable` |
| bundle valid but no entry for this key | Disconnected, reason `not enrolled` |
| no handshake, or older than `WG_STALE_SECS` | Disconnected, reason `no handshake` |
| a startup script exits nonzero | exit 1 before the app is spawned, naming the script and its code |

A single `wg set` invocation that sets the endpoint together with other fields is split so that the
endpoint is its own command; otherwise a DNS failure could not be told from a local one.

### 5.4 Timers, alerts and exit codes

| Name | Default | Meaning |
| --- | --- | --- |
| `WG_POLL_SECS` | 30 | the poll interval, as today |
| `WG_STALE_SECS` | 180 | handshake age past which the tunnel is Disconnected; must be at least 150, since WireGuard renews only every 120 s |
| `WG_HANDSHAKE_ALERT_SECS` | 60 | at boot only: if not Connected this long after step 1 of 5.2, send `/fail` once with the current reason. Replaces `WG_HANDSHAKE_TIMEOUT_SECS`, which is refused at start with a message naming the new variable and its changed meaning |
| `WG_DISCONNECTED_LIMIT_SECS` | 1800 | continuous Disconnected time after which shutdown is pending (section 6) |
| `WG_HOLD_LIMIT_SECS` | 0 | upper bound on how long the app may hold a pending shutdown; 0 means no bound |
| `WG_VERSION_POLL_SECS` | 300 | fetched mode: interval between version checks while Connected |

Alerts through the ping, best effort as today: at boot, one `/fail` per the alert timer; at
runtime, one `/fail` on the Connected-to-Disconnected transition, as today; on give-up, `/fail`
with `gave up after <n> s: <reason>`; on Broken, `/fail` with the error. Plain pings resume on
Connected as today.

Exit codes: the app's own code passes through as today; Broken and a failed startup script exit 1;
give-up exits **75**, chosen as `EX_TEMPFAIL`, meaning a redeploy is the retry.

### 5.5 Version polling and in-place re-apply (fetched mode)

While Connected, every `WG_VERSION_POLL_SECS`: query the version endpoint. Unreachable or invalid
is logged at debug level and skipped; it is never a health signal while the tunnel is Connected.
A tag equal to the applied bundle's `hub_version` is a no-op. A different tag: fetch and validate
(4.2), select (4.3), write the cache (4.4), and compare the new effective configuration to the
applied one. Equal: record the new tag as applied, done. Different: apply the difference in place,
without bringing the interface down, so nothing in flight is disturbed:

| Changed | Commands |
| --- | --- |
| hub public key | `wg set wg0 peer <old key> remove`; then a full `wg set wg0 peer <new key> ...` with endpoint, allowed IPs and keepalive |
| endpoint, allowed IPs, keepalive (key unchanged) | `wg set wg0 peer <key> ...` with the changed fields; `allowed-ips` is given as the full new set |
| address | `ip address replace <new> dev wg0`; `ip address delete <old> dev wg0` |
| allowed IPs (routes) | `ip route replace <cidr> dev wg0` for each added CIDR; `ip route delete <cidr> dev wg0` for each removed |

Failures classify per 5.3. On success the new tag and configuration are the applied ones and the
change is logged field by field, never printing keys beyond their first eight characters.

### 5.6 Repairs while Disconnected

Every poll while Disconnected, in this order, stopping at the first that yields Connected on the
next poll:

1. If no configuration is applied, or the reason is `config unavailable` or `not enrolled`: try to
   obtain one (4.1 to 4.4) and apply it. In fetched mode with a configuration already applied,
   also query the version endpoint; a new tag is fetched and applied exactly as in 5.5, because
   a hub change is a common cause of disconnection.
2. Otherwise alternate as today: on the first Disconnected poll after a Connected one, re-set the
   endpoint (re-resolving DNS); on the next, bring `wg0` down and up with the applied
   configuration; then alternate. Every re-up is logged.

The clock is not reset by a repair attempt, only by Connected.

### 5.7 What the healthcheck sees

Unchanged: `devkit-container healthcheck --file heartbeat.txt --file wireguard-heartbeat.txt`
with the 180 s threshold and the 90 s start period. During boot-time Disconnected the tunnel file
does not exist yet, which the healthcheck reports as missing; the container turns unhealthy after
the start period plus the retries, which is the intended alert path alongside the `/fail` ping.

## 6. Shutdown consent

Depends on 5.4 and section 8. Implemented in this repo's supervisor and in `aeth_ext`.

### 6.1 When

Only when the disconnected clock passes `WG_DISCONNECTED_LIMIT_SECS`. Signals from Docker or
Coolify are forwarded to the app immediately, as today, with no consent step. Consent exists in
both modes and under `supervise` without `wireguard` the machinery is present but never
triggered.

### 6.2 The protocol

- At `prepare`, the supervisor creates `/run/devkit`, owned `999:999`, mode `0700`. The socket path
  is `/run/devkit/consent.sock`. Whenever `supervise` is on (including via `wireguard`), the app is
  spawned with `DEVKIT_CONSENT_SOCKET=/run/devkit/consent.sock` in its environment. A participating
  app listens on that path; the supervisor connects as a client.
- One request per connection. Request: one line, `may-shutdown <reason>\n`, where `<reason>` is
  `wireguard-disconnected <seconds>s`. Reply: one line, `ok\n` or `hold\n`.
- The supervisor treats each of these as `ok`: connection refused, no socket file, any error,
  end of stream without a line, any line other than `hold`, and no reply within 60 s. Only a
  literal `hold` postpones.

### 6.3 The supervisor's loop while shutdown is pending

Each poll still runs the repair of 5.6 first; Connected cancels the pending shutdown and the loop
returns to normal. Otherwise: the first ask happens on the poll where the clock crosses the limit,
after that poll's repair fails. Subsequent asks happen 60 s after the previous reply or timeout,
so at most one ask is outstanding. A `hold` postpones to the next ask. With `WG_HOLD_LIMIT_SECS`
above zero, measured from the first ask, exceeding it proceeds without asking again, logged.

Proceeding: send SIGINT to the app; wait up to 30 s for it to exit; SIGKILL if it has not; bring
`wg0` down; send `/fail` per 5.4; exit 75.

### 6.4 The `aeth_ext` helper

A module under `aeth_ext.monitoring` (name settled in grounding) providing:

- `ShutdownConsent.start()`: a no-op, returning `False`, unless `DEVKIT_CONSENT_SOCKET` is set in
  the environment. When set, it also requires `asyncio.start_unix_server` to exist; if it does not,
  it logs one warning and returns `False`. Otherwise it removes any stale socket file at the path,
  starts a Unix server there, and returns `True`.
- `async with consent.busy():` increments a counter for the duration of the block.
- `consent.on_request(callback)`: an optional callback receiving the reason string, returning
  `True` to hold or `False` to allow; it may also start draining (stop accepting new work).
- Reply logic per request: `hold` if the counter is above zero or the callback returned `True`;
  else `ok`. Malformed requests are answered `ok`. The server never raises into the app.
- `stop()` closes the server and removes the socket file.

Async only in this spec; a thread-based variant for `HeartbeatThread`-style apps is a TODO
(section 15).

### 6.5 Windows and unsupervised runs

Verified on 2026-09-14 with CPython 3.14.5 on Windows: `socket.AF_UNIX`, `asyncio.start_unix_server`
and `asyncio.open_unix_connection` do not exist. The helper never touches them unless the variable
is set, and the variable is set only by the supervisor, which runs only in Linux containers. Every
Windows run and every unsupervised Linux run is therefore the no-op path. The supervisor's client
side lives in the existing Unix-only compiled code, so the Windows wheel is unaffected.

## 7. Startup scripts and `scrub_env`

Generic features of `run`, added for the hub and kept minimal. Depends on sections 8 and 9.

- `[tool.docker].startup_scripts`: a list of console script names from the project's
  `[project.scripts]`, resolved to `/app/.venv/bin/<name>` at the same time the run script is
  resolved, so a typo fails before the tunnel or the mount check. Default empty.
- They run after `prepare` and before scrubbing and the privilege drop, in list order, one at a
  time, as root, with the supervisor's full environment, working directory `/app`, inherited stdio,
  no arguments, no timeout. The first nonzero exit ends the run: the tunnel comes down if up, and
  the binary exits 1 with `error: startup script <name> exited <code>` (or the signal).
- `[tool.docker].scrub_env`: a list of variable names removed from the app's environment before
  spawn, after the startup scripts have run. Default empty. The built-in scrubbing of
  `WG_PRIVATE_KEY`, `WG_PEER_PRESHARED_KEY` and `WG_HUB_TOKEN` is unconditional and additional.
- Both keys are read only by the binary and are available to template gates through `keys()`.
  Neither is added to the pyproject template (section 2, rendering rule).
- Order of `run`, complete: root check; `pyproject.toml`; resolve run script and startup scripts;
  mount check; tunnel (5.2 steps 1 to 3, spoke modes only); `prepare`; startup scripts; scrub;
  drop; spawn or exec the app.

## 8. Environment contract

Every variable this document touches. "Rendered" means the compose template emits the line under
the mode's gate; per the rendering rule, only variables at least one app sets at first deployment
are rendered. "Scrubbed" means removed from the app's environment.

| Variable | Side | Required | Default | Rendered | Scrubbed |
| --- | --- | --- | --- | --- | --- |
| `WG_PRIVATE_KEY` | spoke | yes | | `${WG_PRIVATE_KEY:?}` | yes |
| `WG_HUB_URL` | spoke | fetched mode | | `${WG_HUB_URL:?}` | no |
| `WG_HUB_REPO` | spoke | fetched mode | | `${WG_HUB_REPO:?}` | no |
| `WG_HUB_TOKEN` | spoke | no (private repo needs it) | | `${WG_HUB_TOKEN:?}` | yes |
| `WG_POLL_SECS` | spoke | no | 30 | no | no |
| `WG_STALE_SECS` | spoke | no | 180 | no | no |
| `WG_HANDSHAKE_ALERT_SECS` | spoke | no | 60 | no | no |
| `WG_DISCONNECTED_LIMIT_SECS` | spoke | no | 1800 | no | no |
| `WG_HOLD_LIMIT_SECS` | spoke | no | 0 | no | no |
| `WG_VERSION_POLL_SECS` | spoke | no | 300 | no | no |
| `WG_ADDRESS`, `WG_PEER_PUBLIC_KEY`, `WG_PEER_ENDPOINT`, `WG_PEER_ALLOWED_IPS` | spoke, environment mode | in that mode | | no | no |
| `WG_PEER_PRESHARED_KEY` | spoke, environment mode | no | | no | yes |
| `WG_PERSISTENT_KEEPALIVE` | spoke, environment mode | no | 25 | no | no |
| `WG_HANDSHAKE_TIMEOUT_SECS` | spoke | refused if set | | no | |
| `WG_HUB_PRIVATE_KEY` | hub | yes | | `${WG_HUB_PRIVATE_KEY:?}` | via `scrub_env` |
| `DEVKIT_CONSENT_SOCKET` | set on the app | | | set by `run` under `supervise` | |
| `DEVKIT_SUPERVISED_PING` | set on the app | | | unchanged | |
| `HEARTBEAT_SLUG`, `PINGKEY`, `ALERTS_HEALTHCHECK_PING_URL` | both | | | unchanged | |

Format rules: `WG_HUB_REPO` is `owner/repo`; `WG_HUB_URL` per 4.1; every `*_SECS` an integer at
least 1 except `WG_HOLD_LIMIT_SECS`, which accepts 0. Empty is unset, as today. A failure names the
variable, never its value.

A test-only override, `DEVKIT_GITHUB_API_BASE` and `DEVKIT_GITHUB_BASE`, redirects the two GitHub
hosts of 4.2 to a local stand-in for the smoke tests. Undocumented in the README, never rendered,
refused when the process is not running the smoke test harness (*verify in grounding:* how the
harness identifies itself).

## 9. Templates and the `[tool.docker]` schema

### 9.1 Schema additions

| Key | Meaning |
| --- | --- |
| `wireguard_hub` | the project is the hub: gates the hub's Dockerfile and compose blocks; refused together with `wireguard` at `run` and reported by the render check |
| `startup_scripts` | section 7 |
| `scrub_env` | section 7 |

None of the three is added to the pyproject template. `wireguard` keeps its meaning (a spoke).

### 9.2 Compose template, the changed regions

The spoke's environment block becomes:

```yaml
    # !if keys("tool.docker.wireguard"):
      - WG_PRIVATE_KEY=${WG_PRIVATE_KEY:?}
      - WG_HUB_URL=${WG_HUB_URL:?}
      - WG_HUB_REPO=${WG_HUB_REPO:?}
      - WG_HUB_TOKEN=${WG_HUB_TOKEN:?}
    # !end
    # !if keys("tool.docker.wireguard_hub"):
      - WG_HUB_PRIVATE_KEY=${WG_HUB_PRIVATE_KEY:?}
    # !end
```

and the capability and hub-only keys:

```yaml
    # !if keys("tool.docker.wireguard") or keys("tool.docker.wireguard_hub"):
    # !rule presence
    cap_add:
      - NET_ADMIN
    # !end
    # !if keys("tool.docker.wireguard_hub"):
    # !rule presence
    sysctls:
      - net.ipv4.ip_forward=1
    # !rule presence
    ports:
      - "51820:51820/udp"
    # !end
```

The healthcheck arms are unchanged: the hub has `wireguard = false` and gets the single-file arm.
The ten previous `WG_*` lines leave the template. The rule engine never removes keys, so a project
rendered before this change keeps its old lines until edited by hand (section 10). The published
port is a literal because the template language substitutes only its fixed placeholders;
`presence` means a project that edits the port afterwards keeps its edit.

### 9.3 Dockerfile template

In the final stage, replacing the existing wireguard block. Each `RUN` ends with the same apt
list cleanup the existing block already has, unchanged:

```dockerfile
# !if keys("tool.docker.wireguard") or keys("tool.docker.wireguard_hub"):
# Wireguard: the tools the entrypoint (spoke) or the startup script (hub) shells out to.
RUN apt-get update && apt-get install -y --no-install-recommends wireguard-tools iproute2 \
  && <the existing apt list cleanup>
# !end
# !if keys("tool.docker.wireguard_hub"):
# Wireguard hub: forwarding rules.
RUN apt-get update && apt-get install -y --no-install-recommends iptables \
  && <the existing apt list cleanup>
# !end
```

### 9.4 `aeth-devkit` and `devkit-templates`

No change is expected: the gates use `keys()` on new paths, which the language already resolves
to `None` when absent, and the compose rules are read from the annotations. *Verify in grounding:*
that `presence` handles `sysctls` and `ports` (list-valued keys) exactly as it handles `cap_add`.

## 10. Consuming projects

### 10.1 ScheduledReportAggregator and the test project (spokes)

- `pyproject.toml`: unchanged, `wireguard = true`.
- Run `setup-project` after this repo's release. Then, by hand, remove the ten old `WG_*` lines
  from `docker/compose.yaml`; the rule engine does not remove keys.
- Coolify environment: `WG_PRIVATE_KEY`, `WG_HUB_URL=https://tunnels.sweetfiretobacco.com`,
  `WG_HUB_REPO=AetherBreaker/wireguard-hub`, `WG_HUB_TOKEN`. The four old peer values, if present
  from an earlier deploy, are removed; the binary refuses them alongside `WG_HUB_URL`.
- Enrol: take the public key from the container's start log, add the row to the hub's
  `peers.toml` (3.8), release the hub.
- Consent adoption is optional and independent: ScheduledReportAggregator wraps each job run in
  `busy()` and uses the request callback to stop scheduling new jobs; the test project needs
  nothing.
- The test project is a spoke whose app connects to the database over the tunnel and runs a
  trivial query, so the Python connection-and-query workflow is exercised outside production. Its
  DB client is its own spec's business.

### 10.2 The office PC

Not a container. Download `office-db-pc.conf` from the hub's release assets, replace the private
key placeholder with the PC's own key, install with WireGuard for Windows. Re-download after any
hub release that changes the `[hub]` section. Its firewall rule allows the database port only from
the spoke addresses in `peers.toml`.

## 11. Decisions the owner delegated, and decisions reserved for the owner

Delegated to this document and decided here:

- Hub repository `AetherBreaker/wireguard-hub`; package `wireguard_hub`; service, container and
  healthchecks.io slug `wireguard-hub`.
- Tunnel subnet `10.8.0.0/24`. Addresses: hub `10.8.0.1/24`; office database PC `10.8.0.10/32`;
  ScheduledReportAggregator `10.8.0.20/32`; the test project `10.8.0.21/32`. Further peers from
  `.22` upward. The first-deploy check confirms neither the office LAN nor the VPS uses this
  network.
- UDP port 51820. Public name `tunnels.sweetfiretobacco.com` (the owner's choice, recorded).

Reserved for the owner, to be answered in the grounding pass and written in here before any plan
is written. Under rule 0.2 an implementer who reaches one of these unanswered stops:

- The database engine, port and protocol for `rules.v4`, and whether it listens on the PC's LAN
  address.
- The test project's name (its `peers.toml` row and its `.conf` asset name).
- The GitHub token's owner account, its expiry policy, and who rotates it.
- Whether ScheduledReportAggregator adopts consent in the same change as its migration or later.
- Whether `wireguard-hub` runs with `supervise = true` (gains `/fail` on a crash; nothing else).

## 12. Superseded text in the predecessor documents

In `2026-09-08-container-wireguard-mode-design.md` (this repo): the environment contract table of
section 6 and the `WG_*` lines and healthcheck rationale in section 8 are superseded by sections
4, 5, 8 and 9 here; the handshake timeout's "refused start" in section 6 step 3 is superseded by
5.3 and 5.4; the "No status file" decision stands.

In `2026-09-08-wireguard-db-access-design.md` (ScheduledReportAggregator): section 5 steps 2 and 3
(compose additions and Coolify values) are superseded by 9.2 and 10.1; section 6.1 (the hub as a
`linuxserver/wireguard` container) by section 3; section 8 step 2 (Coolify passing `devices` and
`sysctls`) becomes "passes `cap_add` for spokes, and `cap_add`, `sysctls` and `ports` for the
hub"; section 8 step 4's status file by 5.7; section 9's "Final `WG_*` names" by section 8 here.
Its section 3 address plan is confirmed by 11. That document is edited to say so when the
ScheduledReportAggregator piece is implemented.

## 13. Tests

**This repo, unit.** Tag validation (accepts `v1.2.3`, rejects `1.2.3`, `v1.2`, `v1.2.3-rc1`,
`../x`). Bundle parsing, every validation rule of 3.2 with one failing fixture each, entry
selection, effective configuration with and without overrides, equality as sets. The
configuration diff to commands of 5.5, one case per row. The state machine with an injected
clock: Broken classification per row of 5.3, the boot alert at 60 s, the runtime `/fail` on
transition, the 30-minute give-up, the clock reset on Connected, hold postponing, the hold limit.
The consent client against a fake socket: `ok`, `hold`, garbage, EOF, timeout, absent socket,
refused connection. Startup script resolution, order, environment, failure. `scrub_env` and the
built-in scrubs. Mode detection and the refusals of 5.1.

**This repo, render.** The existing dry-run render check gains the hub mode: `wireguard_hub = true`
renders the hub blocks and the single-file healthcheck, and both switches together are reported.

**This repo, smoke (Linux).** A hub container built from the test image running a minimal hub (the
commands of 3.4 in shell are acceptable here), a stand-in HTTP server on the test network serving
`/version` and playing GitHub through the test-only override of section 8, and a spoke built from
the template in fetched mode. Asserts: boot fetch, Connected, both heartbeats fresh, the token
never appears in the stand-in's redirect-target request; the cache file exists; the hub removes
the peer, the spoke goes Disconnected, the healthcheck names the tunnel file; with the limit set to
seconds, the spoke asks, a participating test app answers `hold`, the spoke is not signalled, the
app answers `ok`, the spoke sends SIGINT and exits 75; the stand-in changes the version and the
bundle's address for the spoke, the spoke re-applies in place and the new address is on `wg0`
without the interface having gone down. The existing off-mode, supervise-mode and
environment-mode smoke tests stay green.

**`aeth_ext`.** The helper's reply logic through an in-memory transport on both platforms; the
real-socket round trip in one Linux-only test; the no-op path with the variable absent and, on
Windows, with it present.

**`wireguard-hub`.** `peers.toml` validation, one test per rule; the `rules.v4` cross-check; the
startup script with a mocked `subprocess` asserting the exact command lines and that the key goes
to stdin; the `/version` response; the heartbeat gating on the interface path.

## 14. Release order

1. `aeth_ext`: the consent helper. Independent; nothing breaks without it.
2. `devkit-container`: everything in sections 5 to 9. Backward compatible: a spoke rendered before
   this release keeps its old compose lines and runs in environment mode.
3. `wireguard-hub`: created with `gh repo create AetherBreaker/wireguard-hub --private`, a stub
   `pyproject.toml`, then `setup-project` against the release from step 2; first release with the
   owner's peer rows; deployed in Coolify with the domain attached.
4. Spokes re-rendered and migrated per 10.1; the office PC per 10.2.
5. The first-deploy checklist of the ScheduledReportAggregator document, with section 12's edits.

## 15. TODO entries to record in this repo at implementation

- A per-project `docker/wireguard/wg0.conf` as a third configuration source, for projects without
  a hub and for local development.
- Preshared keys in fetched mode (needs a per-peer secret on the hub side).
- Generating `rules.v4` from a per-peer `allow` list in `peers.toml`, removing the duplicated
  addresses.
- A thread-based consent helper in `aeth_ext` for non-async apps.
- A data-plane probe (ping the hub's tunnel address each poll) as a second health signal.

## 16. Host requirements, deltas

In addition to the predecessor's: the host kernel provides the netfilter modules `iptables` needs
(`nf_tables` and the `xt_conntrack` match on bookworm's `iptables-nft`); Coolify passes `sysctls`
and `ports` through for the hub; the Docker hairpin path works for both UDP 51820 and HTTPS 443 to
the public name from a container on the same host. All three are first-deploy checks, and the
per-peer `endpoint` override in 3.2 is the fallback for the hairpin.
