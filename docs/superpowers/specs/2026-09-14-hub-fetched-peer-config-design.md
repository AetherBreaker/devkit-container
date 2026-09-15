# WireGuard hub, fetched peer configuration, startup scripts and shutdown consent

Date: 2026-09-14. Status: frozen after the grounding pass of 2026-09-14 (section 0.3). A change
from here needs the owner's explicit decision, recorded here first (rule 5 of 0.2).

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
| 3.7 The kept release job | `aeth-devkit` (`setup-project`) and `devkit-templates` (the workflow header) | 3.2 |
| 4 The bundle contract | `wireguard-hub` (producer) and this repo (consumer) | 3.2, 3.7 |
| 5 Spoke behaviour | this repo (`run`) | 4, 8 |
| 6 Shutdown consent | this repo (`run`); the app-side helper is deferred (6.4) | 5.4, 8 |
| 7 Startup scripts and `scrub_env` | this repo (`run`) | 8, 9 |
| 8 Environment contract | this repo (README), every consumer | 5, 6, 7 |
| 9 Templates and `[tool.docker]` | this repo (package data) | 3.6, 8, 10 |
| 9.3 The Dockerfile windows | `aeth-devkit` (`setup-project`) | 3.6 |
| 10 Consuming projects | ScheduledReportAggregator, `tunnel-probe`, the office PC | 4, 8, 9 |

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
   changes. Log lines and error wording this document does not fix verbatim are the exception:
   they are the implementer's, and the owner does not review them.
3. **The plan carries rules 1 and 2 verbatim in its own preamble**, so an agent that reads only the
   plan still sees them.
4. **The plan is executed inline** by the session that holds it, not delegated to subagents.
5. **After the grounding pass (0.3) this document is frozen.** A change after that point requires
   the owner's explicit decision, recorded here first.

### 0.3 Lifecycle

1. Written and committed to this repo.
2. **Grounding pass**, done 2026-09-14: a session in this repo re-read the document against the
   actual code, with the owner in the discussion. Every note marked *verify in grounding* was
   resolved and the marker removed; every decision reserved for the owner in section 11 was
   answered or deferred and written in. The document is frozen from that point.
3. The plan for this repo's piece is written (the writing-plans skill) and executed inline.
4. The other repositories' pieces each get a plan from their own sections of this same document,
   in the release order of section 14. A change discovered during any piece is written back here
   before the piece continues.

## 1. Summary

A spoke no longer receives its peer configuration through the environment. It asks the hub which
version is deployed, fetches that version's peer bundle from the hub's GitHub release, finds its
own entry by public key, and configures its interface from that, injecting its private key over
stdin as today. A running spoke re-checks the hub's version every five minutes and applies a
changed configuration in place. The hub is a single devkit-managed Python project that brings up
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
                                  |  1. wg0 + private key       (local: Broken)  |
                                  |  2. version -> bundle -> my entry -> apply   |
                                  |  3. handshake within 60 s, else refused      |
                                  |  4. spawn app; poll; re-check every 5 min    |
                                  |  5. disconnected 30 min -> ask app -> SIGINT |
                                  +----------------------------------------------+
```

## 2. Decisions taken, and what was rejected

Recorded so they are not reopened.

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
  Owner ruling, 2026-09-15: the repository is public for now, because the Dockerfile template
  clones the repository inside the image build and cannot clone a private one (no other project is
  private); the token path stays as designed and the proper fix is a devkit-container TODO. The
  roster holds public keys, names and private-range addresses only.
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
- **The hub's asset job is a job of the hub's own, kept inside `release.yml` by `setup-project`**
  through the new `[tool.devkit].release-workflow-jobs` key (3.7). Rejected: a second workflow file
  (the release command waits only for `release.yml`, so the tag could be deployed before the bundle
  is attached, and the file would duplicate the standard workflow's guards); opting the hub out of
  the devkit release workflow (it would drift from the standard).
- **No hub switch.** The hub's compose additions (its capability, the forwarding sysctl, the
  published port, its private-key line) are written by hand once and kept by the rule engine,
  which never removes a key; its Dockerfile additions live in a window that `setup-project`
  renders around (9.3). Rejected: a `wireguard_hub` key (a switch whose only purpose is to make
  the shared templates render one project's lines); gating those lines on the project's name (a
  shared template naming one project).
- **Startup scripts as a generic feature, minimal surface.** One ordered list of console script
  names, run as root, no arguments, no timeout, nonzero exit ends the container. `scrub_env`
  generalises the private-key scrubbing. Neither key is added to the pyproject template.
- **Health model: Broken, Disconnected, Connected.** Local failure ends the run, the app stopped
  gracefully first when one is running (5.3). A boot that does
  not reach Connected is refused: a spoke that cannot connect at start is misconfigured or not
  enrolled, and that must fail loudly (5.2). At runtime a missing hub makes the container
  unhealthy and alerting but running, and 30 minutes of continuous disconnection triggers a
  shutdown. One dumb switch, `WG_TOLERATE_DISCONNECTED`, turns every connect failure into "run
  anyway", for an emergency where a connection cannot happen for an external reason; it does not
  care which reason, and Broken stays fatal under it. Rejected: starting the app before the first
  handshake (it hides a misconfiguration behind a running container); a tunnel status file for
  the app to read (the heartbeat file and the ping already carry the state).
- **`restart: no` stays.** A hub outage longer than 30 minutes stops every spoke until it is
  redeployed. Chosen for an unambiguous state; the owner accepted the manual redeploy. The switch
  above is the escape hatch for an outage known to be long.
- **An explicitly removed peer shuts down.** A fetched bundle that is valid and has no entry for
  this key while a configuration is applied is the hub's instruction, not a connectivity failure:
  alert, then shut down (5.5), under both settings of the switch.
- **Shutdown consent over a Unix socket the app opens if it participates.** Non-participation is
  indistinguishable from "go ahead", so no existing app changes. No upper bound on holding by
  default; a knob exists. The supervisor's side ships now; the app-side helper in `aeth_ext` is
  deferred (6.4), which costs nothing because non-participation reads as consent. Rejected: two
  timestamp files (the app cannot learn a shutdown is pending without polling); loopback TCP
  (works on Windows dev boxes, but the supervisor never runs there).
- **Version polling every five minutes with in-place re-apply.** Closes the case where a hub
  release changes a spoke's address: the handshake stays fresh while the hub drops the spoke's
  data, so nothing else would ever re-fetch. Rejected: re-fetch only during repair; a data-plane
  probe (adds little once polling exists).
- **Cached last-good bundle in persisted data**, in a folder of the entrypoint's own (4.4). Lets a
  spoke reach Connected when GitHub is down but the hub is not. The bundle holds no secrets.
- **The supervisor never waits on the network or on the app inside its loop.** The version check
  with its fetch, and the consent ask, each run on a worker thread, one of each in flight, and the
  loop acts on the result at the poll after the thread completes; the loop wakes at once on a
  signal or on the child's exit, and otherwise every 250 ms; the boot's own fetch and handshake
  wait happen before the app exists (5.2). Rejected: an async runtime in the binary (the HTTP,
  socket and subprocess calls are blocking and would run on worker threads underneath anyway, and
  PID 1's reaping of every orphan stays hand-written either way).
- **The repair while Disconnected alternates** the endpoint re-set with the interface
  down-and-up (5.6), and the endpoint is the last command of every apply (5.2), so a
  name-resolution failure leaves a complete interface lacking only the endpoint.
- **A failed `wg show` on an existing interface is Broken.** The interface is then in a state the
  supervisor cannot reason about; exiting with a named error beats repairing blindly.
- **A plain log file as a placeholder for logging** (5.8). The binary has no logging system;
  hooking it into `aeth_ext`'s is later work, recorded in this repo's todo. Until then its own
  lines go to one dumb append-only file in the logs folder as well as to stderr, and a failed
  version check while Connected is logged once per change of outcome, never per attempt.
- **The smoke test fetches from real GitHub**, from a private fixture repository, with the test
  key pairs committed in the test source and one CI secret, the fixture token (13). Rejected: test-only override variables in the binary (a knob in
  production code, which anyone able to set could already outdo by setting the hub URL and the
  private key); a local stand-in over TLS (the binary would have to trust the image's certificate
  store, and a stand-in encodes the same assumptions as the code it tests).
- **The database flow rules are the next phase's.** `rules.v4` ships with the policy lines only
  (3.3); the per-flow lines, the office PC's firewall rule and `tunnel-probe`'s query follow once
  the engine, port and protocol are decided (section 11).
- **Environment rendering rule.** A variable is rendered into the compose template only if at
  least one app will set it at first deployment. Everything else is documented, not rendered.
- **Out of scope, recorded as TODO at implementation** (section 15): a per-project conf file
  source; preshared keys in fetched mode; generating the firewall rules from the peer table; a
  data-plane probe. The `aeth_ext` consent helper, async and thread-based, is recorded in
  `aeth_ext`'s own TODO (6.4). A hub mode in the binary is dropped, not deferred.

## 3. The hub project: `wireguard-hub`

Repository `AetherBreaker/wireguard-hub`, private by design and public for now (section 2, owner
ruling 2026-09-15). Python package `wireguard_hub`, compose service
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
name = "tunnel-probe"
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

An `iptables-restore` file for the `filter` table, applied whole by the startup script. In this
document it holds the policy only: `FORWARD DROP`, and accept `ESTABLISHED,RELATED` on `wg0` to
`wg0`. The per-flow `ACCEPT` lines, one per permitted flow from a spoke's `/32` to the database
PC's `/32` on the database port and protocol, are the next phase's, once the engine, port and
protocol are decided (section 11); the file's shape is fixed now so that phase adds lines, not
structure. Tunnel addresses in this file duplicate `peers.toml`; that is accepted for this spec,
and a hub test asserts every address in `rules.v4` is an address in `peers.toml` (trivially true
while it holds none). Generating the rules from the table is a TODO (section 15). The `INPUT`
chain is not touched: Docker publishes the UDP port and the container's default `INPUT` policy
accepts.

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
- The heartbeat, written every 60 s to `/app/persisted_data/logs/heartbeat.txt` with `aeth_ext`'s
  one-shot `send_heartbeat`, called from the app's own loop, but only while `/sys/class/net/wg0`
  exists. The one-shot call is used because `aeth_ext`'s scheduled helpers
  (`run_heartbeat_async`, `HeartbeatThread`) cannot pause while the interface is gone. The
  one-shot call also does not know about a supervisor owning the ping, so the loop passes the
  ping key and slug only when `DEVKIT_SUPERVISED_PING` is unset; with `supervise = false` (3.6)
  that is always. An interface that vanished stops the beats, the standard healthcheck turns
  unhealthy, and healthchecks.io alerts through the standard ping. The app cannot inspect
  handshakes (`wg show` needs `NET_ADMIN`); reachability of the hub is what the spokes' own health
  reports.
- Shutdown on SIGINT/SIGTERM. Nothing else.

### 3.6 `pyproject.toml`, compose and Dockerfile

```toml
[tool.docker]
services                = ["wireguard-hub"]
required_persisted_dirs = ["persisted_data"]
supervise               = false
wireguard               = false
startup_scripts         = ["wireguard-hub-up"]
scrub_env               = ["WG_HUB_PRIVATE_KEY"]

[tool.devkit]
release-workflow-jobs   = ["peers"]
```

There is no hub switch. `setup-project` renders the standard scaffold and the single-file
healthcheck; the hub's own additions are written by hand once and survive every later run:

- In `docker/compose.yaml`, under the service: `cap_add: [NET_ADMIN]`,
  `sysctls: [net.ipv4.ip_forward=1]`, `ports: ["51820:51820/udp"]`, and
  `- WG_HUB_PRIVATE_KEY=${WG_HUB_PRIVATE_KEY:?}` under `environment`. The rule engine never
  removes a key it does not know and only appends to `environment`, so all four stay (9.2).
- In `docker/Dockerfile`, inside the `final` window (9.3):
  `RUN apt-get update && apt-get install -y --no-install-recommends wireguard-tools iproute2 iptables && rm -rf /var/lib/apt/lists/*`.
  `setup-project` renders the template around the window, so the line stays.

Coolify: the domain `tunnels.sweetfiretobacco.com` is attached to the `wireguard-hub` service on
container port 8000 in the Coolify UI, which provides the Traefik route and the certificate; the
UDP port is published directly by compose and never passes through Traefik. Environment values:
`WG_HUB_PRIVATE_KEY`, plus the standard `PINGKEY` and alert values every project has. Learned on
the first deploy (2026-09-15): the domain is entered in Coolify as
`https://tunnels.sweetfiretobacco.com:8000`, the port in the URL being how Coolify tells Traefik
the container port; the healthchecks.io check `wireguard-hub` is auto-provisioned by `PINGKEY`
with period 1 minute and grace 3 minutes, matching the app's 60 s beat and the devkit
healthcheck's 180 s staleness; and Coolify passes every environment variable to the image build
as a `--build-arg`, so no Dockerfile in this design may declare a secret as an `ARG` (a build
arg's value lands in the image history). The image is built from the Dockerfile of the checked-out
branch while the source is cloned at `GIT_TAG`, so a drift of `docker/Dockerfile` on `main`
reaches the next build even when the pin does not move.

### 3.7 Release, and the kept job in `release.yml`

The devkit release command is used unchanged. The validation, stamping, rendering and attaching
happen in a job of the hub's own inside `.github/workflows/release.yml`, the file `setup-project`
otherwise owns and rewrites on every run. To keep it there, `setup-project` gains one setting,
implemented in `aeth-devkit` with its header line in `devkit-templates`:

- `[tool.devkit].release-workflow-jobs`: a list of job names. When `setup-project` re-renders
  `release.yml`, it copies each named job's block out of the existing file and splices it into the
  rendered file under `jobs`, after the template's own jobs, and reports the kept jobs in its
  change log. A named job the existing file does not hold is reported, not an error, so the first
  run before the job is written passes. The key joins `[tool.devkit]`'s known keys, so an unknown
  key stays an error.
- The template's header line "edits are replaced on the next run" gains "except the jobs named in
  `[tool.devkit].release-workflow-jobs`".
- Nothing else in `setup-project` or in the release command changes. The release command watches
  the whole run, so it waits for the kept job as it waits for the others: the bundle is attached
  before the release command reports success, and before any pin or redeploy that follows it.

The hub's job is named `peers`. It runs on the same release event, carries its own
`permissions: contents: write` as the publish job does, guards as the publish job does (checks out
`github.sha`, and verifies the release still owns the tag before uploading), validates
`peers.toml` (3.2), writes a copy with `hub_version = "<tag>"` inserted as the first top-level
key, and renders `<peer name>.conf` for every peer:

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
The job fails the run if validation fails. Since it runs beside the publish job, a failure can
land after the wheel is published, the same exposure a publish failure has today; that is why
every push also runs the validation in the hub's hand-written `ci.yml` (devkit has no CI
template), so a broken table is caught before a release is attempted.

### 3.8 Enrolling a peer

A spoke logs its derived public key at every start and, when the hub's bundle lacks it, refuses
to start with `not enrolled` and that key (5.2). Enrolment is: take the key from that log, add a
`[[peers]]` row with it and the next address from the plan, add the flow to `rules.v4` if the peer
may reach the database (next phase, 3.3), release the hub, redeploy the spoke. A running, enrolled
spoke picks up a later change to its row within the version poll interval (5.5) without a
redeploy.

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

The binary's HTTP client (`ureq`, already a Linux-only dependency) takes per-request settings, and
with redirects disabled a 302 comes back as an ordinary response, so step 3 works as written. The
client also strips `Authorization` by itself on any redirect it follows; step 4 stays explicit
because it is what the unit test asserts (13). Reading the release listing needs a JSON reader:
`serde_json`, today a test-only dependency, becomes a Linux-only runtime dependency at the version
the lock already holds, used through its untyped value API, with no derived types. The two GitHub
hosts are the fetch code's ordinary inputs, so a test can point them at a local listener without
any environment variable existing for it.

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

After every successful fetch and validation, the bundle is written to
`/app/persisted_data/wireguard/peers.toml`: atomically, mode 0644, owned by 999:999 like everything
under `persisted_data`. The folder `persisted_data/wireguard` is the entrypoint's own: with
`wireguard = true` it is created and chowned in the same pass as the `required_persisted_dirs`
entries, without appearing in that key, and it is not part of the mount check, since a cache that
turns out unbacked still works and merely does not outlive the container. Every write is best
effort: a failure is one log line and never a health signal. The bundle fetched at boot is held in
memory and written right after `prepare` (5.2 step 6), once the folder exists; runtime writes go
straight there. At boot, when the version endpoint or the fetch fails, the cache is read and
validated (3.2) and, if it holds an entry for this spoke, used as the configuration, logged as
`using cached bundle <hub_version>`. A cache with no entry for this spoke counts as no cache: it
is not the hub's word on enrolment. A fresh fetch always wins over the cache. A cache that fails
validation is ignored and overwritten by the next successful fetch.

## 5. Spoke behaviour in `devkit-container run`

Changes to the supervisor's wireguard mode. Depends on sections 4 and 8. Everything not mentioned
here (the poll, the tunnel heartbeat file, the healthcheck subcommand, the ping and its ownership,
`HEARTBEAT_SLUG`, the privilege drop, signal forwarding, zombie reaping) is unchanged from what
the binary does today, as the README describes.

### 5.1 Modes

`WG_HUB_URL` present means **fetched mode** (this document). Absent means **environment mode**,
the existing contract with `WG_ADDRESS`, `WG_PEER_PUBLIC_KEY`, `WG_PEER_ENDPOINT`,
`WG_PEER_ALLOWED_IPS`, `WG_PEER_PRESHARED_KEY` and `WG_PERSISTENT_KEEPALIVE`, kept for projects
that have not migrated. `WG_HUB_URL` together with any of those six is refused at start, naming
the conflicting variable. `WG_HUB_REPO` is required in fetched mode; `WG_HUB_TOKEN` is optional to
the binary (4.2). The health model, timers, consent and exit codes of 5.3 to 5.6 apply in both
modes; version polling and the cache apply in fetched mode only.

### 5.2 Boot sequence

Root check, `pyproject.toml`, resolution of the run script and the startup scripts, and the mount
check, as today; then the ping is configured (5.4), before the tunnel, so a refused start can
send `/fail`. Then, all before the app exists:

1. Preflight (`wg`, `ip` on PATH), `ip link add dev wg0 type wireguard`, `wg set wg0 private-key`
   over stdin, log the derived public key. A failure here is **Broken** (5.3).
2. Obtain the configuration. Environment mode: from the variables of 5.1. Fetched mode: version
   endpoint (4.1), then fetch (4.2), then select (4.3); when the version endpoint or the fetch
   fails, the cache (4.4). The two outcomes that are not a configuration: `config unavailable`
   (no bundle and no usable cache) and `not enrolled in <tag>: no entry for <public key>; turn
   [tool.docker].wireguard off or enrol the key, then redeploy` (the hub's bundle is valid and
   lacks this key). This is the one network wait done inline: it is bounded by the request
   timeouts of 4.1 and 4.2, at most four requests of 10 s each.
3. Apply it, in this order: `wg set wg0 peer <hub key> allowed-ips <cidrs>`; `ip address add
   <address> dev wg0`; `ip link set up dev wg0`; one `ip route replace <cidr> dev wg0` per
   allowed IP; last, `wg set wg0 peer <hub key> endpoint <endpoint> persistent-keepalive <n>`.
   The endpoint is the last command of every apply (here, in 5.5 and in the re-up of 5.6)
   because it is the one command that resolves a name: a DNS failure then leaves a complete
   interface lacking only the endpoint, and the repair is exactly a re-set. The keepalive rides
   with the endpoint because WireGuard sends its first handshake when the keepalive is set;
   set before the endpoint exists, that attempt is lost and only the 5 s retry can succeed.
   Classification per 5.3: a local failure is Broken; the endpoint failing is `endpoint
   unresolvable`.
4. Wait for the first handshake, polling `wg show wg0 latest-handshakes` every 500 ms, for up to
   `WG_HANDSHAKE_TIMEOUT_SECS` (5.4). None is `no wireguard handshake with <endpoint> within <n> s`.
5. **The gate.** By default a boot that has not reached Connected here is refused: `wg0` comes
   down, `/fail` goes out with the reason from step 2, 3 or 4, and the binary exits 1 with the
   same text as its `error:` line. Something is wrong and it must fail loudly. With
   `WG_TOLERATE_DISCONNECTED=1` (section 8) the spoke instead logs the reason, sends `/fail`
   once, and carries on into step 6 with the tunnel Disconnected under that reason and, when step
   2 got none, no configuration applied. Broken is exit 1 under both settings.
6. `prepare`, which also creates and chowns the implicit folders `persisted_data/wireguard` (4.4)
   and `persisted_data/logs` (5.7); the cache write of the bundle held from step 2, if step 2
   fetched one; startup scripts (none for a spoke unless declared); scrubbing; privilege drop;
   spawn the app.
7. Enter the poll loop with the disconnected clock at zero. The loop wakes at once on a signal or
   on the child's exit, and otherwise every 250 ms. It never waits on the network or on the app:
   the version check with its fetch (5.5, 5.6) and the consent ask (6.3) each run on a worker
   thread, one of each in flight at a time, and the loop acts on the result at the first poll
   after the thread completes. The local `wg` and `ip` commands run inline, as today.

### 5.3 The three states, and classification

Evaluated every poll. Every transition is logged with its reason.

- **Broken.** A local operation failed: the interface cannot be created or queried, a key, address
  or route is rejected by the kernel, a tool is missing, a startup script failed. Never the hub.
  At boot there is no app yet: the supervisor brings the interface down, sends `/fail` with the
  error (best effort), and exits 1 with an `error:` line naming the failing command. At runtime
  the app is stopped first, gracefully: SIGTERM, up to 30 s for it to exit, SIGKILL if it has
  not; then the same three steps. No retry, under both settings of the switch.
- **Disconnected.** The interface exists and holds the private key, but there is no fresh handshake
  (older than `WG_STALE_SECS`, or none). The reason is `no handshake`, or `endpoint unresolvable`
  after the endpoint command failed; under the switch, a boot that got no configuration adds
  `config unavailable` and `not enrolled`. The tunnel heartbeat is not written, so Docker turns
  unhealthy. Repairs run every poll (5.6). The continuous-disconnected clock runs.
- **Connected.** `wg show wg0 latest-handshakes` reports a handshake younger than `WG_STALE_SECS`.
  The tunnel heartbeat is written, the clock resets, a pending shutdown (section 6) is cancelled.

| Operation | A failure means |
| --- | --- |
| `wg` or `ip` missing; `ip link add`; `wg set private-key`; `ip address add/replace/delete`; `ip link set up`; `ip route replace/delete`; `wg set peer ... allowed-ips/persistent-keepalive`; `wg set peer ... remove`; `wg show` on an existing interface; `ip link delete` followed by a failed `ip link add` | Broken |
| `wg set peer ... endpoint <host:port>` (resolves the hub's name) | at boot, a refused start unless the switch is on; at runtime, Disconnected, reason `endpoint unresolvable` |
| version endpoint unreachable; fetch failure; bundle invalid; cache unusable | at boot, a refused start (`config unavailable`) unless the switch is on; at runtime, never a health signal: logged per 5.5, the applied configuration stays |
| bundle valid but no entry for this key | at boot, a refused start (`not enrolled`) unless the switch is on; at runtime with a configuration applied, the removal shutdown of 5.5 |
| no handshake, or older than `WG_STALE_SECS` | at boot, a refused start unless the switch is on; at runtime, Disconnected, reason `no handshake` |
| a startup script exits nonzero | exit 1 before the app is spawned, naming the script and its code |

A single `wg set` invocation that sets the endpoint together with other fields is split so that the
endpoint is its own command, and that command is the last of every apply (5.2 step 3); otherwise a
DNS failure could not be told from a local one.

### 5.4 Timers, alerts and exit codes

| Name | Default | Meaning |
| --- | --- | --- |
| `WG_POLL_SECS` | 30 | the poll interval, as today |
| `WG_STALE_SECS` | 180 | handshake age past which the tunnel is Disconnected; must be at least 150, since WireGuard renews only every 120 s |
| `WG_HANDSHAKE_TIMEOUT_SECS` | 60 | at boot: how long the first handshake may take (5.2 step 4) before the start is refused or, under the switch, before the app starts Disconnected |
| `WG_DISCONNECTED_LIMIT_SECS` | 1800 | continuous Disconnected time after which shutdown is pending (section 6); no effect under the switch |
| `WG_HOLD_LIMIT_SECS` | 0 | upper bound on how long the app may hold a pending shutdown; 0 means no bound |
| `WG_VERSION_POLL_SECS` | 300 | fetched mode: interval between version checks while Connected |
| `WG_TOLERATE_DISCONNECTED` | unset | `1` turns every boot-time connect failure into a running, Disconnected spoke (5.2 step 5) and switches the give-up off (6.1); anything but unset, empty or `1` is refused at start |

Alerts through the ping, best effort as today: at boot, `/fail` with the reason on a refused
start, or once under the switch; at runtime, one `/fail` on the Connected-to-Disconnected
transition, as today; on give-up, `/fail` with `gave up after <n> s: <reason>`; on removal,
`/fail` with `removed from the hub's peer table in <tag>` (5.5); on Broken, `/fail` with the
error. Plain pings resume on Connected as today.

Exit codes: the app's own code passes through as today; Broken, a refused start and a failed
startup script exit 1; give-up and the removal shutdown exit **75**, chosen as `EX_TEMPFAIL`,
meaning a redeploy is the retry.

### 5.5 Version polling and in-place re-apply (fetched mode)

While Connected, every `WG_VERSION_POLL_SECS`: query the version endpoint, on the worker thread of
5.2 step 7; the fetch that may follow runs on the same thread, and the apply below happens inline
at the poll that receives the result. Unreachable or invalid is skipped and is never a health
signal while the tunnel is Connected; it is logged once when the checks start failing, with the
error, and once when they succeed again, never per attempt (5.8). A tag equal to the applied bundle's
`hub_version` is a no-op. A different tag: fetch and validate (4.2), select (4.3), write the cache
(4.4), and compare the new effective configuration to the applied one. Equal: record the new tag
as applied, done. Different: apply the difference in place, without bringing the interface down,
so nothing in flight is disturbed, in the table's order, the endpoint last:

| Changed | Commands |
| --- | --- |
| hub public key | `wg set wg0 peer <old key> remove`; `wg set wg0 peer <new key> allowed-ips <cidrs>`; then the endpoint row |
| allowed IPs, keepalive (key unchanged) | `wg set wg0 peer <key> ...` with the changed fields; `allowed-ips` is given as the full new set |
| address | `ip address replace <new> dev wg0`; `ip address delete <old> dev wg0` |
| allowed IPs (routes) | `ip route replace <cidr> dev wg0` for each added CIDR; `ip route delete <cidr> dev wg0` for each removed |
| endpoint, or a new hub key | `wg set wg0 peer <key> endpoint <endpoint> persistent-keepalive <n>`, after every other row |

Failures classify per 5.3. On success the new tag and configuration are the applied ones and the
change is logged field by field, never printing keys beyond their first eight characters.

A fetched bundle that is valid and has no entry for this key while a configuration is applied is
the hub's instruction, not a connectivity failure, and it is honoured under both settings of the
switch: the cache is written, `/fail` goes out with `removed from the hub's peer table in <tag>`,
the same line is logged, then the shutdown of 6.3 without the consent ask: SIGINT to the app, up
to 30 s, SIGKILL, `wg0` down, exit 75.

### 5.6 Repairs while Disconnected

Every poll while Disconnected, both steps in the same poll, neither waiting for the other:

1. Under the switch, when the boot got no configuration: try to obtain one (4.1 to 4.3, the fetch
   alone: the cache was the boot's fallback and only changes on a successful fetch) and apply
   it. In fetched mode with a configuration applied: query the version endpoint; a new tag is
   fetched and applied exactly as in 5.5, because a hub change is a common cause of
   disconnection. Either runs on the worker thread of 5.2 step 7, one attempt in flight, started
   only when none is; the apply happens inline at the poll that receives the result.
2. With a configuration applied, alternate: on the first Disconnected poll after a Connected one,
   re-set the endpoint (`wg set wg0 peer <key> endpoint <endpoint> persistent-keepalive <n>`,
   which re-resolves the name and leaves the interface running); on the next, bring `wg0` down and up with the applied
   configuration (the apply of 5.2 step 3, the endpoint last); then the endpoint again, then down
   and up, alternating. Every re-up is logged. The sequence starts over at the endpoint re-set
   after every Connected poll and after a configuration is applied by step 1.

The clock is not reset by a repair attempt, only by Connected. The repair never runs on a
Connected tunnel.

### 5.7 What the healthcheck sees

Unchanged: `devkit-container healthcheck --file heartbeat.txt --file wireguard-heartbeat.txt`
with the 180 s threshold and the 90 s start period. `persisted_data/logs`, where both files live,
is created and chowned by `prepare` whenever the supervisor runs (`supervise` or `wireguard` on),
implicitly, like the cache folder of 4.4, so the tunnel heartbeat can be written from the first
poll on a fresh volume. The tunnel file is first written on the first poll after the app starts,
seconds into the 90 s start period. Under the switch, a spoke that booted Disconnected has no
tunnel file, which the healthcheck reports as missing once the start period and the retries have
passed, alongside the `/fail` ping already sent.

### 5.8 The binary's log file

A placeholder until the binary logs through `aeth_ext` (this repo's todo), kept as simple as it
can be. Every line the binary writes for itself from the tunnel step onward, the transitions,
repairs, applied configurations, version-check outcome changes (5.5), consent asks and replies, is
also appended to `/app/persisted_data/logs/devkit-container.log`, in the implicit logs folder
(5.7), its own file beside the app's, each line prefixed with a timestamp. The same lines keep
going to stderr, so the container log is unchanged. The file is opened for append on every line
and closed again: nothing buffered, nothing rotated, nothing capped. It is created world-readable
and handed to nonroot like the heartbeat file, so the folder stays uniformly owned. A line that
cannot be written still reaches stderr and nothing else happens.

## 6. Shutdown consent

Depends on 5.4 and section 8. Implemented in this repo's supervisor; the app-side helper is
deferred (6.4).

### 6.1 When

Only when the disconnected clock passes `WG_DISCONNECTED_LIMIT_SECS` and `WG_TOLERATE_DISCONNECTED`
is off: under the switch there is no give-up, so the spoke repairs until the hub returns (5.4).
Signals from Docker or Coolify are forwarded to the app immediately, as today, with no consent
step, and the removal shutdown of 5.5 does not ask. Consent exists in both modes and under
`supervise` without `wireguard` the machinery is present but never triggered.

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
- The ask runs on the worker thread of 5.2 step 7; the loop reads the reply at the first poll
  after it arrives or times out.

### 6.3 The supervisor's loop while shutdown is pending

Each poll still runs the repair of 5.6 first; Connected cancels the pending shutdown and the loop
returns to normal. Otherwise: the first ask is started on the poll where the clock crosses the
limit, after that poll's repair fails, and its reply is read at a later poll. Subsequent asks
happen 60 s after the previous reply or timeout, so at most one ask is outstanding. A `hold`
postpones to the next ask. With `WG_HOLD_LIMIT_SECS` above zero, measured from the first ask,
exceeding it proceeds without asking again, logged.

Proceeding: send SIGINT to the app; wait up to 30 s for it to exit; SIGKILL if it has not; bring
`wg0` down; send `/fail` per 5.4; exit 75.

### 6.4 The app-side helper: deferred

The `aeth_ext` helper that lets an app participate is out of this document's scope and is not a
blocker for it: the supervisor's side works unchanged against an app that never opens the socket.
Its rough shape is recorded in `aeth_ext`'s `TODO.md` when this document is frozen, so the
protocol of 6.2 is what it will implement:

- a module `aeth_ext.monitoring.consent`, exported from `aeth_ext.monitoring` like its siblings,
  providing `ShutdownConsent`; `start()` is a no-op returning `False` unless `DEVKIT_CONSENT_SOCKET`
  is set, and also when `asyncio.start_unix_server` does not exist (one warning logged);
  otherwise it removes a stale socket file at the path, starts a Unix server there and returns
  `True`;
- `async with consent.busy():` counting work in progress for the duration of the block;
  `consent.on_request(callback)`, an optional callback receiving the reason string and returning
  `True` to hold or `False` to allow, which may also start draining (stop accepting new work);
- reply logic per request: `hold` if the counter is above zero or the callback returned `True`,
  else `ok`; malformed requests answered `ok`; the server never raises into the app; `stop()`
  closes the server and removes the socket file;
- Windows and unsupervised runs are the no-op path by construction: `socket.AF_UNIX`,
  `asyncio.start_unix_server` and `asyncio.open_unix_connection` do not exist on Windows
  (verified 2026-09-14 with CPython 3.14.5), the helper never touches them unless the variable is
  set, and only the supervisor sets it, in Linux containers;
- a thread-based variant for `HeartbeatThread`-style apps.

The supervisor's client side lives in the Linux-only compiled code, so the Windows wheel is
unaffected either way.

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
  `WG_PRIVATE_KEY`, `WG_PEER_PRESHARED_KEY` and `WG_HUB_TOKEN` is unconditional and additional,
  on the spawn and the exec path alike.
- Both keys are read only by the binary and are available to template gates through `keys()`.
  Neither is added to the pyproject template (section 2, rendering rule).
- Order of `run`, complete: root check; `pyproject.toml`; resolve run script and startup scripts;
  mount check; configure the ping (5.4); tunnel (5.2 steps 1 to 5, spoke modes only); `prepare`,
  including the implicit folders (4.4, 5.7); the boot cache write (4.4); startup scripts; scrub;
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
| `WG_HUB_TOKEN` | spoke | no: the binary accepts its absence (4.2) | | `${WG_HUB_TOKEN:-}`, empty when unset (owner ruling, 2026-09-15; devkit-container 2.1.1) | yes |
| `WG_POLL_SECS` | spoke | no | 30 | no | no |
| `WG_STALE_SECS` | spoke | no | 180 | no | no |
| `WG_HANDSHAKE_TIMEOUT_SECS` | spoke | no | 60 | no | no |
| `WG_DISCONNECTED_LIMIT_SECS` | spoke | no | 1800 | no | no |
| `WG_HOLD_LIMIT_SECS` | spoke | no | 0 | no | no |
| `WG_VERSION_POLL_SECS` | spoke | no | 300 | no | no |
| `WG_ADDRESS`, `WG_PEER_PUBLIC_KEY`, `WG_PEER_ENDPOINT`, `WG_PEER_ALLOWED_IPS` | spoke, environment mode | in that mode | | no | no |
| `WG_PEER_PRESHARED_KEY` | spoke, environment mode | no | | no | yes |
| `WG_PERSISTENT_KEEPALIVE` | spoke, environment mode | no | 25 | no | no |
| `WG_TOLERATE_DISCONNECTED` | spoke | no | unset | no | no |
| `WG_HUB_PRIVATE_KEY` | hub | yes | | by hand in the hub's compose file (3.6) | via `scrub_env` |
| `DEVKIT_CONSENT_SOCKET` | set on the app | | | set by `run` under `supervise` | |
| `DEVKIT_SUPERVISED_PING` | set on the app | | | unchanged | |
| `ALERTS_EMAIL_PWD` | hub | yes: `aeth_ext` builds its settings at import and requires it | | by the compose template's `aeth-ext` block | no |
| `HEARTBEAT_SLUG`, `PINGKEY`, `ALERTS_HEALTHCHECK_PING_URL` | both | | | unchanged | |

Format rules: `WG_HUB_REPO` is `owner/repo`; `WG_HUB_URL` per 4.1; every `*_SECS` an integer at
least 1 except `WG_HOLD_LIMIT_SECS`, which accepts 0; `WG_TOLERATE_DISCONNECTED` is unset, empty
or `1`. Empty is unset, as today. A failure names the variable, never its value.

No test-only variable exists: the smoke test fetches from real GitHub (13), and the unit test of
the fetch passes its listener's address to the fetch code directly (4.2).

## 9. Templates and the `[tool.docker]` schema

### 9.1 Schema additions

| Key | Meaning |
| --- | --- |
| `startup_scripts` | section 7 |
| `scrub_env` | section 7 |

Neither is added to the pyproject template. `wireguard` keeps its meaning (a spoke); there is no
hub switch (3.6).

### 9.2 Compose template, the changed region

The spoke's environment block becomes:

```yaml
    # !if keys("tool.docker.wireguard"):
      - WG_PRIVATE_KEY=${WG_PRIVATE_KEY:?}
      - WG_HUB_URL=${WG_HUB_URL:?}
      - WG_HUB_REPO=${WG_HUB_REPO:?}
      - WG_HUB_TOKEN=${WG_HUB_TOKEN:-}
    # !end
```

Nothing else in the template changes: `cap_add: [NET_ADMIN]` stays gated on `wireguard` with its
`presence` rule, and the healthcheck arms are unchanged. The ten previous `WG_*` lines leave the
template. The rule engine never removes keys, so a project rendered before this change keeps its
old lines until edited by hand (section 10); the same property is what lets the hub keep its own
`cap_add`, `sysctls`, `ports` and private-key line, written by hand (3.6): a key the scaffold does
not annotate is never touched, a `presence` key the scaffold lacks is skipped, and `env-keys` only
appends.

### 9.3 Dockerfile template: the spoke block and the two windows

The wireguard block stays gated on `wireguard` as today. Two windows are added, regions
`setup-project` renders around: `builder`, after the last instruction of the builder stage, and
`final`, after the wireguard block and before `WORKDIR /app` in the final stage:

```dockerfile
# ---- Builder stage ----
...
RUN --mount=type=cache,target=/root/.cache/uv \
  extras=$(/app/.venv/bin/devkit-container app-extra) \
  && uv sync --frozen --no-dev --no-editable $extras

# Project additions to the builder stage; setup-project renders the template around this window.
# !window builder:
# !end builder

# ---- Final stage ----
...
# !if keys("tool.docker.wireguard"):
# Wireguard mode: the tools the entrypoint shells out to, so they version with the binary.
RUN apt-get update && apt-get install -y --no-install-recommends wireguard-tools iproute2 \
  && rm -rf /var/lib/apt/lists/*
# !end

# Project additions to the final stage; setup-project renders the template around this window.
# !window final:
# !end final

WORKDIR /app
```

A window is an explicit block of the template language with the new marker word `window`, and
unlike every other marker its pair stays in the rendered file, so the next run can find it. On
every render `setup-project` copies the lines a project wrote between a window's markers in its
existing Dockerfile into the same window of the rendered file, unchanged, and replaces everything
outside the windows as today. A new file renders with empty windows; a file rendered before the
windows existed has no markers, so its windows start empty and the diff shows the markers
arriving. A window in the project's file that the template does not have is left out of the
render: the template's omission is a choice, so its lines go with it, shown in the diff and named
in a `note:`, never an error (owner ruling, 2026-09-15). The hub's additions live in its `final`
window (3.6).

### 9.4 `aeth-devkit` and `devkit-templates`

Two devkit changes serve this document, both in `aeth-devkit`'s `setup-project`: the kept release
job (3.7, with its header line in `devkit-templates`) and the Dockerfile windows (9.3): the marker
word `window` joins the four the gate pass accepts, its markers survive rendering, and the
Dockerfile step splices the existing file's window contents into the rendered text before the
diff. So does every other render of the file: `docker-pin`'s Dockerfile refresh dropped a filled
window until aeth-devkit 15.1.1 (found on the hub's first deploy, 2026-09-15). Everything else the templates need already holds: `keys()` returns the value at a path,
`None` when absent; `env-keys` only appends and no rule removes a key, which is what lets the hub
keep its hand-written compose additions (3.6); and `setup-project` validates no `[tool.docker]`
key beyond `services`, the silence flag and the two legacy keys, so `startup_scripts` and
`scrub_env` pass through.

The window marker is the guard against a stale devkit: a devkit older than the release of 14
step 1 refuses this repo's template with "unknown marker", a hard error naming it, never a silent
drop. `devkit-container` declares no dependency on `aeth-devkit` (it would pull the devkit into
every image), so the release order of 14 is what keeps the two in step.

## 10. Consuming projects

### 10.1 ScheduledReportAggregator and `tunnel-probe` (spokes)

- `pyproject.toml`: unchanged, `wireguard = true`.
- Run `setup-project` after this repo's release. Then, by hand, remove the ten old `WG_*` lines
  from `docker/compose.yaml`; the rule engine does not remove keys.
- Coolify environment: `WG_PRIVATE_KEY`, `WG_HUB_URL=https://tunnels.sweetfiretobacco.com`,
  `WG_HUB_REPO=AetherBreaker/wireguard-hub`, `WG_HUB_TOKEN`. The same URL on the hub's own host:
  the hairpin works once the host firewall allows it (16). The six old peer values of 5.1, if
  present from an earlier deploy, are removed; the binary refuses them alongside `WG_HUB_URL`.
  No token while the hub repository is public (owner ruling, 2026-09-15): the template renders
  `WG_HUB_TOKEN=${WG_HUB_TOKEN:-}` from devkit-container 2.1.1, empty when unset, which the
  binary reads as absent (8, 4.2). A compose file rendered earlier keeps its `:?` line, since
  the env-keys rule keeps a present key's value; edit it by hand to match.
- Enrol: the first start is refused with `not enrolled` and logs the public key; add the row to
  the hub's `peers.toml` (3.8), release the hub, redeploy the spoke.
- Neither adopts consent in this change: the app-side helper is deferred (6.4), and
  non-participation reads as consent.
- `tunnel-probe` is a one-shot sandbox (owner ruling, 2026-09-15): its app is a placeholder that
  prints one line and exits 0, replaced by hand with whatever smoke test the owner wants run over
  the tunnel from inside Coolify. It uses neither `aeth_ext` nor healthchecks.io; the rendered
  heartbeat and healthcheck lines stay as the template emits them and are ignored, and Coolify
  reporting the exited container as unhealthy is accepted. `restart: no` makes each deploy one
  run. Its key pair was generated at enrolment and its row added to the hub before its first
  deploy, so its first start is not refused. Its database query, which exercises the Python
  connection-and-query workflow over the tunnel outside production, is the next phase's
  (section 11), as is its database client.

### 10.2 The office PC

Not a container. Download `office-db-pc.conf` from the hub's release assets, replace the private
key placeholder with the PC's own key, install with WireGuard for Windows. Re-download after any
hub release that changes the `[hub]` section. Its firewall rule for the database port is the next
phase's (section 11).

## 11. Decisions the owner delegated, and decisions reserved for the owner

Delegated to this document and decided here:

- Hub repository `AetherBreaker/wireguard-hub`; package `wireguard_hub`; service, container and
  healthchecks.io slug `wireguard-hub`.
- The test project: repository `AetherBreaker/tunnel-probe`; package `tunnel_probe`; service,
  container and peer-table name `tunnel-probe`; conf asset `tunnel-probe.conf`. No healthchecks.io
  check: it is a one-shot sandbox (10.1).
- The hub's kept release job is named `peers` (3.7).
- The Dockerfile windows are named `builder` and `final`, marked `# !window <name>:` and
  `# !end <name>` (9.3).
- Tunnel subnet `10.8.0.0/24`. Addresses: hub `10.8.0.1/24`; office database PC `10.8.0.10/32`;
  ScheduledReportAggregator `10.8.0.20/32`; `tunnel-probe` `10.8.0.21/32`. Further peers from
  `.22` upward. The first-deploy check confirms neither the office LAN nor the VPS uses this
  network.
- UDP port 51820. Public name `tunnels.sweetfiretobacco.com` (the owner's choice, recorded).

Reserved for the owner and answered in the grounding pass of 2026-09-14:

- **The database engine, port and protocol for `rules.v4`, and where it listens:** deferred to the
  next phase, after this document is implemented. `rules.v4` ships with the policy only (3.3);
  the per-flow rules, the office PC's firewall rule (10.2), `tunnel-probe`'s query (10.1) and the
  three checks of section 16 that need the database follow with that decision.
- **The test project's name:** delegated to this document, `tunnel-probe` (above).
- **The production token:** not created while the hub repository is public (owner ruling,
  2026-09-15; 10.1 makes the compose line optional). When the repository goes private: a
  fine-grained token on the `AetherBreaker` account with read-only
  access to the contents of `wireguard-hub` and nothing else, one-year expiry, rotated by the
  owner when it expires. An expired token surfaces as `config unavailable` in the spoke's log
  and, if the hub changes meanwhile, as the tunnel going Disconnected.
- **The smoke-test fixture:** repository `AetherBreaker/wireguard-hub-smoke`, private, holding
  three releases: two whose `peers.toml` enrol the test spoke's public key at two addresses under
  the fixture hub's public key, and a third whose `peers.toml` has no entry for it (13). Its token is a second fine-grained token of the same shape,
  scoped to that repository only. The two key pairs, the fixture hub's and the test spoke's, are
  constants in the smoke test source: WireGuard keys match no provider pattern GitHub's secret
  scanning knows, and each constant's line carries the `gitleaks:allow`, `trufflehog:ignore` and
  `ggignore` markers with a comment that it guards nothing. The token is read from
  `DEVKIT_SMOKE_WG_HUB_TOKEN`; CI holds it as a secret under the same name.
- **Whether ScheduledReportAggregator adopts consent in the same change as its migration:** later,
  with the `aeth_ext` helper (6.4).
- **Whether `wireguard-hub` runs with `supervise = true`:** no; 3.6 stands. A crashed hub exits
  its container, its pings stop and healthchecks.io alerts; `supervise` would add only an
  immediate `/fail` and oblige the hub's heartbeat loop to know about the supervisor owning the
  ping.
- **The `[tool.devkit]` key for the kept release job:** `release-workflow-jobs` (3.7).
- **The cache location:** `persisted_data/wireguard/peers.toml`, in an implicit folder of the
  entrypoint's, not under `logs` and not in `required_persisted_dirs` (4.4).

Nothing in this list is open. An implementer who needs a decision this document does not hold
stops under rule 0.2.

## 12. Constraints, topology and the cleanup list

Constraints, all the owner's:

- In-house end to end: no third-party overlay or tunnel service in the data path.
- Every real user of the network has its own key pair and tunnel address, so activity is
  attributable; no shared forwarder between apps.
- The database must not be reachable by any other container on the `coolify` network.
- Coolify conventions hold: auto-deploy on changes under `docker/**`, consistent container names,
  every container on the external `coolify` network so `central-log-server` resolves.
- healthchecks.io and Pushover for every new component, preferred not required.

Topology and security:

- **The tunnel lives inside the app container**: no sidecar, no relay, no extra Docker network.
  Rejected: a sidecar sharing the app's network namespace, a relay sidecar on a private network
  with DNAT, a relay reachable on `coolify`, Cloudflare Tunnel, Tailscale-class overlays, and a
  userspace WireGuard forwarder.
- **Hub and spoke.** Every spoke talks only to the hub; spokes on the VPS use the hub's public
  endpoint too, through Docker's hairpin path, with the per-peer `endpoint` override of 3.2 as the
  fallback. No DNS inside the tunnel: the database host is reached by its tunnel address.
- **Spokes get `cap_add: [NET_ADMIN]` and nothing else**, no `devices` and no `sysctls` (9.2); the
  hub alone adds the forwarding sysctl and the published port, by hand (3.6).
- **Security layers.** The hub's cryptokey routing and its `FORWARD` rules (3.3); the office PC's
  firewall allowing the database port only from approved tunnel addresses (next phase, 10.2); a
  read-only database account for the app; private keys only in Coolify secrets or the gitignored
  local `.env`.
- **The office PC** runs native WireGuard for Windows as a service, not Docker Desktop under WSL2,
  so the tunnel is up before anyone logs in and the database port is directly reachable at the
  PC's tunnel address. It needs only outbound UDP to the hub; no router port forward.

The cleanup list, artefacts of an earlier attempt that are removed and never read as inputs: the
workspace folders `wireguard-test-sender`, `wireguard-vps-relay` and `wireguard-warehouse-client`;
the workspace-root `compose.test.yaml`; ScheduledReportAggregator's gitignored
`docker/wireguard/keys.env`; and the `SERVER_PUBLIC_KEY`, `SERVER_ENDPOINT` and
`TAXES_JOB_PRIVATE_KEY` entries in its `.env`. Key pairs from that attempt are retired, never
reused.

## 13. Tests

**This repo, unit.** Tag validation (accepts `v1.2.3`, rejects `1.2.3`, `v1.2`, `v1.2.3-rc1`,
`../x`). Bundle parsing, every validation rule of 3.2 with one failing fixture each, entry
selection, effective configuration with and without overrides, equality as sets. The fetch of 4.2
against an in-process HTTP listener on localhost, plain HTTP, Linux-only like the client, the
two GitHub hosts being the fetch code's ordinary inputs: the listing, the 302, the redirect
target, and the assertion that the `Authorization` header reaches only the API address and never
the redirect target; a missing asset; a body over 1 MiB; a `hub_version` that does not match the
tag. The configuration diff to commands of 5.5, one case per row, the endpoint last. The state
machine with an injected clock: Broken classification per row of 5.3, the refused start for each
boot outcome and the same outcomes tolerated under the switch, the runtime `/fail` on transition,
the 30-minute give-up and its absence under the switch, the removal shutdown, the clock reset on
Connected, the alternating repair of 5.6 and its two restart points, hold postponing, the hold
limit. The consent
client against a fake socket: `ok`, `hold`, garbage, EOF, timeout, absent socket, refused
connection. Startup script resolution, order, environment, failure. `scrub_env` and the built-in
scrubs on both paths. The implicit folders of 4.4 and 5.7 created and chowned, and not part of
the mount check. The log file of 5.8: a line appended with its timestamp, the file created
world-readable, a write failure ignored. Mode detection and the refusals of 5.1.

**This repo, render.** Unchanged in shape: the two modes through the released devkit, now with
the window markers in the template.

**This repo, smoke (Linux).** A hub container built from the test image running a minimal hub (the
commands of 3.4 in shell are acceptable here) and serving `/version` from a small HTTP listener on
the test network, reached through `WG_HUB_URL` over plain `http`. The bundle comes from real
GitHub, from the fixture repository of section 11: three releases, two whose `peers.toml` enrol
the test spoke's public key at two addresses under the fixture hub's public key and one without
the entry, fetched with the fixture token. Both key pairs are constants in the test source
(section 11). The test reads `DEVKIT_SMOKE_WG_HUB_TOKEN` from its environment and refuses to run,
naming it, when it is missing (it does not skip); CI provides it as a secret. A spoke built from
the template in fetched mode with the test spoke's key. Asserts: boot fetch, Connected, both
heartbeats fresh; the cache file exists at its path with its mode and owner; a second spoke with
a freshly generated key is refused, exiting 1 with `not enrolled` and its key in the log, and the
same spoke under `WG_TOLERATE_DISCONNECTED=1` runs Disconnected with its app started; a spoke
pointed at an unreachable `WG_HUB_URL` on an empty volume is refused with `config unavailable`;
the hub's `/version` moves to the second tag and the test hub's allowed IPs to the second
address, the spoke re-applies in place and the new address is on `wg0` without the interface
having gone down; the hub removes the peer, the spoke goes Disconnected, the healthcheck names
the tunnel file; with the limit set to seconds, the spoke asks, a participating test app (which
speaks the protocol of 6.2 itself, in a few lines of standard-library Python) answers `hold`, the
spoke is not signalled, the app answers `ok`, the spoke sends SIGINT and exits 75; a fresh spoke
container with the test key boots against the second tag and, when `/version` moves to the third
tag, logs the removal, sends `/fail`, asks nothing, and exits 75. The existing off-mode,
supervise-mode and environment-mode smoke tests stay green. The helper that applies the Dockerfile template's
gate locally leaves the window markers in place, as the rendered file keeps them.

**`aeth-devkit`.** The kept-jobs key of 3.7: a `release.yml` holding a named job survives a
re-render with the job spliced under `jobs` and reported; a named job the file lacks is reported,
not an error; an unknown `[tool.devkit]` key is still refused; the fixture template's header line
carries the new wording. The windows of 9.3: a Dockerfile with lines inside each window survives a
re-render with the template applied around them; a file without markers renders with empty
windows; a window the template lacks is left out, named in a note, never an error; the marker word is
accepted by the gate pass and kept in the output.

**`wireguard-hub`.** `peers.toml` validation, one test per rule; the `rules.v4` cross-check; the
startup script with a mocked `subprocess` asserting the exact command lines and that the key goes
to stdin; the `/version` response; the heartbeat gating on the interface path and on
`DEVKIT_SUPERVISED_PING`.

## 14. Release order

- [x] 1. `aeth-devkit` and `devkit-templates`: the kept release job (3.7) and the Dockerfile windows
   (9.3). Nothing breaks without them, and both are needed before step 2's template can be
   rendered anywhere and before the hub's first release. This work branches from `aeth-devkit`'s
   `main`, not from its open branch `feat/review-everything-but-docker`, which is unfinished; once
   this step is merged, that branch is rebased onto the new `main` as part of this step (owner
   ruling, 2026-09-15).
- [x] 2. `devkit-container`: everything in sections 5 to 9. Backward compatible: a spoke rendered before
   this release keeps its old compose lines and runs in environment mode. Its fetched-mode smoke
   test needs the fixture repository and the secret of section 11 in place first.
- [x] 3. `wireguard-hub`: created with `gh repo create AetherBreaker/wireguard-hub --private`, a stub
   `pyproject.toml`, then `setup-project` against the releases from steps 1 and 2, the `peers`
   job, the compose additions and the window content written by hand (3.6); first release with
   the owner's peer rows; deployed in Coolify with the domain attached.
- [ ] 4. Spokes re-rendered and migrated per 10.1, `tunnel-probe` created the same way as the hub; the
   office PC per 10.2.
- [ ] 5. The first-deploy checklist of section 16, in order.

The boxes above and in section 16 are ticked as each step lands (owner's instruction, 2026-09-15);
section 16 step 1 waits for the owner's word that the subnet is unused.

Status, 2026-09-15: steps 1 to 3 are done and deployed: aeth-devkit 15.1.1 (15.1.0 the features,
15.1.1 the `docker-pin` window fix), devkit-templates 1.3.0, devkit-container 2.1.0, wireguard-hub
1.0.0 with an empty roster, healthy in Coolify and on healthchecks.io, `/version` answering over
the public name; every devkit repository is locked on those versions. Section 16 steps 1 to 3
hold; step 4 waits for the first peer. Step 4 of this list is next and gets its own plan from
sections 5, 8, 10 and 16. Open before it: the production token of section 11 is not created yet
(with the repository public for now the spokes could fetch without one; the plan should still
create and use it so nothing changes when the repository goes private again), and the database
decision of section 11 stays deferred, gating `tunnel-probe`'s query and the per-flow rules but
not the tunnels. The smoke-test fixture repository and its secret on devkit-container exist.

Status, 2026-09-15, later: the `tunnel-probe` part of step 4 is released and enrolled ahead of
its first deploy: tunnel-probe 1.0.0 (the placeholder app of 10.1, compose pinned), its key pair
generated at enrolment (the private key in an ignored file beside the checkout, never
committed), its row in wireguard-hub 1.1.0, whose bundle and `tunnel-probe.conf` asset carry it,
and the hub's compose pinned to v1.1.0. The repository is public like the hub's, for the same
reason (section 2). Left in step 4: the hub redeployed at v1.1.0 so its interface knows the peer,
tunnel-probe deployed in Coolify with the three variables of 10.1 (no token while the hub is
public), then ScheduledReportAggregator and the office PC.

Status, 2026-09-15, later still: ScheduledReportAggregator is migrated per 10.1 and released as
3.1.0 on devkit-container 2.1.1 (compose pinned; the nine environment-mode lines removed), and
enrolled in wireguard-hub 1.2.0 at 10.8.0.20 with the public key derived from the private key
its checkout's `.env` holds; the hub's compose is pinned to v1.2.0. tunnel-probe is pinned to
devkit-container 2.1.1 without a new release, since 2.1.1 changed only the template and its
compose line already matched. Left in step 4: the hub redeployed at v1.2.0, both spokes deployed
with the three variables (ScheduledReportAggregator's Coolify environment loses the nine old
values and its `WG_PRIVATE_KEY` must be the key whose public key is enrolled), and the office PC.

## 15. TODO entries to record in this repo at implementation

- A per-project `docker/wireguard/wg0.conf` as a third configuration source, for projects without
  a hub and for local development.
- Preshared keys in fetched mode (needs a per-peer secret on the hub side).
- Generating `rules.v4` from a per-peer `allow` list in `peers.toml`, removing the duplicated
  addresses.
- A data-plane probe (ping the hub's tunnel address each poll) as a second health signal.

The `aeth_ext` consent helper of 6.4, async first and a thread-based variant for non-async apps,
is recorded in `aeth_ext`'s own `TODO.md`, not here.

## 16. Host requirements and the first-deploy checklist

Host requirements, in addition to the kernel WireGuard module the wireguard mode already needs:
the host kernel provides the netfilter modules `iptables` needs (`nf_tables` and the
`xt_conntrack` match on bookworm's `iptables-nft`); Coolify passes `sysctls` and `ports` through
for the hub; and the host firewall allows `443/tcp` and `51820/udp` in (`ufw allow`). The last
is the Docker hairpin, found 2026-09-15: Docker never forwards traffic that arrives from the
same bridge as the published container, so a spoke on the `coolify` network reaches the public
name as ordinary input on the host, served by `docker-proxy`, and ufw's default deny dropped it
(`[UFW BLOCK] IN=br-… DPT=443` in the kernel log) while outside traffic, forwarded before ufw
sees it, worked. The two rules open only that path, persist with ufw and name no container.
Hub 1.3.0 carried the per-peer `endpoint` fallback of 3.2 on the VPS-side rows meanwhile; 1.4.0
removed it, so every row uses the public name and the fallback stays available for a host that
cannot be fixed this way.

First deploy, in this order, each a hard stop if it fails:

- [ ] 1. Neither the office LAN nor the VPS uses `10.8.0.0/24`.
- [x] 2. `modprobe wireguard` succeeds on the VPS host, and the netfilter modules above are present.
- [x] 3. The hub deploys; `docker inspect` on its container shows `NET_ADMIN`, the forwarding sysctl and
   the published UDP port passed through unchanged; `GET /version` over the public name answers
   the hub's tag; the hub's heartbeat is fresh.
- [ ] 4. The hub and the office PC handshake with each other before any app is involved.
- [ ] 5. ScheduledReportAggregator's first start is refused with `not enrolled` and its key in the log;
   enrolled and redeployed, it fetches the bundle, handshakes with the hub at the public endpoint
   (the hairpin check, with the firewall rules above), and both of its heartbeat files are fresh;
   `docker inspect` shows `cap_add` passed through.
- [ ] 6. A hub release that changes nothing for the spoke is picked up within the version poll interval
   with no re-apply logged; one that changes its keepalive is applied in place without the
   interface going down.

The next phase's checks, once the database decision of section 11 is made: `tunnel-probe` runs
its query against the database over the tunnel; a sibling container on the `coolify` network
cannot reach the database port (negative test); the office PC's firewall rejects the database
port from a tunnel address that is not approved.
