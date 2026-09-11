# devkit-container: supervisor, wireguard mode, heartbeat healthcheck

Date: 2026-09-08; rewritten 2026-09-10 after review. Status: approved. One implementation plan
covers every repo in section 1; the `aeth_ext` part lands on a branch and a PR, unmerged.

Consuming project's view: `ScheduledReportAggregator/docs/superpowers/specs/2026-09-08-wireguard-db-access-design.md`
(the tunnel lives inside the app container). Its monitoring table changes with section 6: the
tunnel's Pushover path is the healthchecks.io integration, and the `aeth_ext` status-file reader it
lists as follow-on work no longer exists.

## 1. Boundary

| Repo | Changes |
| --- | --- |
| `devkit-container` | the `supervise` and `wireguard` switches; the supervisor; the tunnel; the `healthcheck` subcommand; the ping; the Dockerfile `if-wireguard` block; schema doc; tests |
| `aeth-devkit` (`setup`) | `wireguard` context flag; the gate applied to the Dockerfile; the `wireguard` gate on compose; `cap_add` in the rule table; the HEAD-vs-working-copy refusal extended to `wireguard` |
| `devkit-templates` | the compose template: `HEARTBEAT_SLUG`, the binary healthcheck line, the gated blocks |
| `aeth_ext` | `HEARTBEAT_SLUG` from the environment; the periodic ping stands down under `DEVKIT_SUPERVISED_PING` |

The hub, the office PC and the test project are the companion spec's. The plan runs against
sibling checkouts under one workspace folder; a repo that is not cloned on the executing machine
(`devkit-templates` is not, on the machine this was written on) is cloned first per
`aeth-devkit/WORKSPACE.md`, decided at execution time, not assumed.

## 2. Switches

Both under `[tool.docker]` in `pyproject.toml`, read by the binary at `run` (the image carries
the file).

- `supervise = true`: the entrypoint spawns and supervises the app instead of exec'ing it
  (section 3). Off by default: an app that never spawns a child gains nothing from a reaper and
  should not pay for one.
- `wireguard = true`: the tunnel (section 4). Implies `supervise`. Also read by `setup-project`
  for gating, so the working copy and HEAD must agree on it, the same refusal as for `services`.
  `supervise` affects only the binary and needs no such check.

Both off is today's behaviour exactly: `exec`, the app is PID 1. The one addition on that path is
removing a leftover `wireguard-heartbeat.txt` (section 5) before the exec.

## 3. Entrypoint

In order: root check; launch script; mount check; the tunnel (section 4) when on; `prepare`; then
one branch. The mount check precedes the tunnel because it is a pure read that fails in
milliseconds, and a missing volume should not wait out a handshake timeout. `prepare` follows the
tunnel so every check still happens before the filesystem is touched.

**Exec** (neither switch): `setgroups`, `setgid`, `setuid`, `exec`, as today.

**Supervise**: spawn `/app/.venv/bin/<run-app-*>` as 999:999 with empty supplementary groups and
empty capability sets, via `pre_exec` doing what the exec path does. The supervisor stays PID 1.
Without `wireguard` it drops to 999 itself before spawning, so no root process lingers; with
`wireguard` it stays root because re-upping the tunnel needs `NET_ADMIN`. Duties:

- reap zombies; forward `SIGTERM`, `SIGINT` and `SIGHUP` to the child;
- every `WG_POLL_SECS` (default 30, the name is kept in both modes): the tunnel check (section 4)
  when on, then the heartbeat adjudication and the ping (section 5);
- on child exit: bring `wg0` down if up, send `/fail` on a nonzero exit (section 5), exit with the
  child's code, signal death as 128+n.

The child's environment is the supervisor's minus `WG_PRIVATE_KEY` and `WG_PEER_PRESHARED_KEY`,
plus `DEVKIT_SUPERVISED_PING=1` when the supervisor owns the ping (section 5). A failure reading
the environment names the variable, never the value.

## 4. Wireguard mode

Root phase, in this order:

1. **Preflight.** `wg` and `ip` on PATH, else refused with "image built without the wireguard
   block; rerun setup-project with a devkit that knows it". `WG_PERSISTENT_KEEPALIVE` nonzero
   (default 25), else refused: with no keepalive and an idle app WireGuard never re-handshakes,
   so every poll after `WG_STALE_SECS` would read as stale.
2. **Bring-up** with `ip link add wg0 type wireguard`, `wg set` (keys over stdin, never a file),
   `ip address add`, `ip link set up`, one `ip route add` per allowed IP. No `wg-quick`, no config
   file. The public key derived from `WG_PRIVATE_KEY` (`wg pubkey`, stdin again) is logged every
   start: not a secret, and what the hub operator enrols.
3. **First handshake** within `WG_HANDSHAKE_TIMEOUT_SECS` (default 60), else a refused start
   naming the endpoint. With `restart: no` that leaves the container down until a redeploy;
   accepted, because "the app never runs without its tunnel" is the invariant, and a deploy that
   fails loudly is the right signal for a hub that is not there.

Per poll: `wg show wg0 latest-handshakes`. A handshake older than `WG_STALE_SECS` (default 180,
WireGuard's own reject-after-time) is stale. First response: re-set the peer endpoint (`wg set
wg0 peer … endpoint …`), which re-resolves its DNS; the companion spec lets the hub move hosts.
If the next poll is still stale: `wg0` down and up. Every re-up is logged.

Environment contract. The `[tool.docker]` schema doc in the README records it.

| Variable | Meaning | Required |
| --- | --- | --- |
| `WG_PRIVATE_KEY` | this peer's private key; secret | yes |
| `WG_ADDRESS` | this peer's tunnel address, CIDR (`10.8.0.20/32`) | yes |
| `WG_PEER_PUBLIC_KEY` | the hub's public key | yes |
| `WG_PEER_ENDPOINT` | `host:port` of the hub | yes |
| `WG_PEER_ALLOWED_IPS` | comma-separated CIDRs routed through the hub | yes |
| `WG_PEER_PRESHARED_KEY` | secret | no |
| `WG_PERSISTENT_KEEPALIVE` | seconds; default 25; zero refused | no |
| `WG_HANDSHAKE_TIMEOUT_SECS`, `WG_POLL_SECS`, `WG_STALE_SECS` | defaults 60, 30, 180 | no |

Empty is read as unset.

## 5. Heartbeats, healthcheck, ping

**One concept.** A heartbeat file holds one timestamp, written by a process while it is healthy.
Fresh means younger than 180 s. The timestamp is what `datetime.isoformat()` produces: an offset
is honoured, a bare timestamp is read as container-local time (`TZ`, else UTC), because `date -d`
did and existing apps write both forms.

**Files.**

- The app's, `/app/persisted_data/logs/heartbeat.txt`, written by `aeth_ext` every 60 s, as today.
- The tunnel's, `/app/persisted_data/logs/wireguard-heartbeat.txt`: the supervisor writes the
  current time on each poll while the handshake is fresh, and nothing otherwise. Current time,
  not handshake time, so the file means what the app's means: "a process that knows it is fine
  said so at T". A stale tunnel and a wedged supervisor both stop the beats. World-readable,
  written atomically. It lives beside the app's because that directory is a host bind mount:
  another container mounting the same path read-only can check it. Removed at start when the
  mode is off, so a leftover cannot read as a stale tunnel.

**`devkit-container healthcheck`** replaces the compose `bash -ec` one-liner. `--file PATH`
(repeatable; default the app's file) and `--max-age SECS` (default 180). Exit 0 when every file is
fresh; else exit 1 with one line on stderr per problem (missing, empty, unparseable, stale by N s),
which is what `docker inspect` shows. It reads files only: no root, no capabilities, no `wg`, no
pyproject parse, no dependence on the supervisor being alive. A project without `supervise` uses
it exactly as one with.

**The ping**, sent by the supervisor, follows `aeth_ext.monitoring.ping` exactly: URL from
`ALERTS_HEALTHCHECK_PING_URL`, else `https://hc-ping.com/<PINGKEY>/<HEARTBEAT_SLUG>` with
`?create=1`; `/start` once, when every file is first fresh after boot; a plain ping on every poll
while every file the supervisor is responsible for is fresh (the app's, plus the tunnel's when
on); `/fail` on the transition to stale and on a nonzero child exit, with the reason or the code
in the request body, plain pings resuming when every file is fresh again; 10 s timeout; best-effort, one log line per failure, never fatal. The check's period is set at or above the poll interval and its grace at or
above 180 s, on the healthchecks.io side. No URL and no key, or a key without a slug: no
pinging, one log line at start (a warning in wireguard mode,
where the tunnel is then visible to Docker but not to healthchecks.io). The client is a Rust
HTTPS client with rustls (plan's call; the static musl smoke build must keep working). It runs as
root in wireguard mode: outbound only, to one host, and the response body is never parsed.

**Ownership.** When the supervisor has a URL, or a key and a slug, at spawn, it owns the ping and sets
`DEVKIT_SUPERVISED_PING=1` on the child. `aeth_ext` skips its periodic ping under that variable
and keeps the file write; `/fail` for a known job failure stays the app's to send, on the same
check. Without the variable the app pings as today. Exactly one process pings, by construction,
and the environment is the only channel: it flows down once at spawn, which is the one direction
it flows.

**Slug.** `HEARTBEAT_SLUG`, set by compose to the service name (section 6), read by both. It is
per-container data and lives in the service block for the same reason `container_name` does: two
services of one project share an image and a `pyproject.toml`, and "the first service" would be
right by luck and wrong exactly when the list exists. Fallback when the variable is absent: the
single entry of `[tool.docker].services` when there is exactly one, else no pinging and the log
line. `aeth_ext` reads the same variable in place of its `HEARTBEAT_SLUG` code constant.

**Docker health** in wireguard mode is the two-file line in section 6: app fresh and tunnel fresh,
same threshold, same meaning, the stderr line saying which. It costs nothing operationally:
Docker acts on nothing with `restart: no`, Coolify shows the badge and gates a deploy on it, and a
deploy with a dead tunnel failing is correct.

## 6. Rendering

**Compose** (`devkit-templates`). In the service block:

- `environment:` unconditional, with `HEARTBEAT_SLUG={service}` as its first line, always. The
  `if-aeth-ext` block moves inside it and the `if-wireguard` lines follow as a sibling gated
  block. Gates do not nest, and the key can no longer be gated-out: the slug line is what makes
  that safe, since `environment:` is never empty.
- `healthcheck.test: ["CMD", "/app/.venv/bin/devkit-container", "healthcheck"]`.
- `if-wireguard`: `cap_add: [NET_ADMIN]`; the `WG_*` lines, required ones as `${NAME:?}`,
  optional as `${NAME:-}`, so every project's compose file is identical and values live in the
  deploy environment; `healthcheck.test` with `--file` for both heartbeat files; and
  `start_period: 90s` in place of `15s`, since the handshake timeout precedes the app's first
  beat. No `/dev/net/tun`, no `src_valid_mark` sysctl: the first serves only userspace WireGuard,
  the second only `wg-quick`'s full-tunnel fwmark, and neither applies to a fixed peer subnet.
  `PINGKEY` and `ALERTS_HEALTHCHECK_PING_URL` are not rendered; they come from the deploy
  environment as today.

**Rule engine** (`compose_rules.rs`): `cap_add` as a `Presence` rule. `EnvKeys` already appends
`HEARTBEAT_SLUG` and the `WG_*` lines; `ExactList` already replaces `healthcheck.test`; `Exact`
already sets `start_period`. Rules whose path the scaffold lacks are skipped, so a project with
the mode off is untouched. Keys are never removed: a project turning the mode off keeps its
`WG_*` lines until edited by hand.

**Gates** (`setup`): `wireguard` in `ProjectContext` from `[tool.docker].wireguard`;
`scaffold::load`'s predicate gains `wireguard`; `static_files::render` applies
`templates::gate` to the Dockerfile, which it does not today (a devkit older than this renders the
Dockerfile without the block, which section 4's preflight catches). `cli::refuse_uncommitted_services`
compares `wireguard` as well.

**Dockerfile** (this repo's template): an `if-wireguard` block in the final stage installing
`wireguard-tools` and `iproute2`, so the binary and the tools it shells out to version together.

## 7. `aeth_ext`

On a branch, PR opened, not merged or released; the owner does both.

- `_auto_slug` prefers `os.environ["HEARTBEAT_SLUG"]`; the constant lookup stays as the fallback
  for hosts that are not containers.
- `run_heartbeat_async` and `HeartbeatThread` skip the ping, keeping the file write, when
  `DEVKIT_SUPERVISED_PING` is set. `send_heartbeat(failure=True)` is unaffected.

Both are one-condition changes; the contract (the two names and their meaning) is owned here.

## 8. Release order

1. `devkit-container`: everything in sections 2 to 5 and the Dockerfile block. Its smoke tests
   are the gate.
2. `aeth-devkit`: the gates, the flag, the rule, the refusal. Until this ships, a templates
   release using the new gates renders them as disabled.
3. `devkit-templates`: the compose template. The pyproject template already floors
   `devkit-container>={latest}` and `setup-project` advances the package every run, so a project
   rendering the new healthcheck line gets a binary that has the subcommand in the same run.
4. `aeth_ext`: the owner's, after the PR.

## 9. Host requirements

A Docker host kernel with the wireguard module (Linux 5.6 or later) and a deploy platform that
passes `cap_add` through; both verified on the first deploy per the companion spec's checklist.
The CI runner loads the module (`sudo modprobe wireguard`) before the wireguard smoke test.

## 10. Tests

Unit: timestamp parsing (offset, bare, garbage), freshness with an injected clock, environment
scrubbing, the stale-and-re-up state machine with an injected clock, ping URL and suffix
building, slug fallback.

Smoke, off mode: today's test unchanged and green, plus the `healthcheck` subcommand run in the
image against a fresh and a stale file.

Smoke, supervise without wireguard: the app reports its parent is PID 1, uid 999, empty
capability sets; `SIGTERM` reaches it and its exit code passes through; `DEVKIT_SUPERVISED_PING`
is present when a slug and key are given and absent otherwise.

Smoke, wireguard: a hub container with generated keys and an image built from the template with
the mode on, on one Docker network. Asserts: handshake within the timeout; child uid 999 with
empty capability sets and `WG_PRIVATE_KEY` absent from its environment; the tunnel heartbeat
present, readable by 999, fresh; `healthcheck --file` both files exits 0; removing the peer on the
hub and restoring it makes the tunnel file go stale (the healthcheck exits 1 naming it) then fresh
again, with a re-up logged; `SIGTERM` and exit code pass through. Poll and stale intervals are set
short through the environment. The ping is tested against a local HTTP listener standing in for
healthchecks.io: `/start` once, plain while healthy, `/fail` on the stale transition.

`setup`: a gated compose render for each combination of aeth-ext and wireguard, `cap_add`
inserted into an existing file, the Dockerfile block present only with the mode on, the refusal
when `wireguard` differs from HEAD.

## 11. Done means

- The existing smoke test is unchanged and green with both switches off.
- The three new smoke tests are green in this repo's CI.
- `setup-project` on a project with the mode off renders no wireguard line anywhere; turning it
  on changes only the gated blocks, `cap_add`, `healthcheck.test` and `start_period`.
- `ScheduledReportAggregator` builds and starts in both modes; with the mode on, the companion
  spec's first-deploy checks pass.
- The `aeth_ext` PR is open and its tests pass on the branch.

## 12. Decisions taken, and what was rejected

- **Supervisor opt-in, not universal.** A universal supervisor was considered for `/fail` on
  crash and uniform monitoring; rejected because most apps never spawn a child and would pay for a
  reaper and a signal path they cannot use. `supervise` gives the benefit to projects that want it.
- **No status file.** `/run/devkit/wireguard.json` was in the original design for `aeth_ext` to
  read and alert on; the ping covers that. It was then kept for the healthcheck and for a ping
  handoff; the tunnel heartbeat file and the spawn-time environment variable cover those, with no
  dependence on the supervisor's poll loop and no second file format.
- **Keys over stdin; `ip` + `wg set`; no `wg-quick`.** No key touches a filesystem, no bash in the
  root phase, and the two compose extras `wg-quick` would have needed are gone with it.
- **Stale counts as unhealthy, immediately.** A local retry budget before the badge flips was
  rejected: healthchecks.io's grace period already filters transients server-side, and
  `WG_STALE_SECS` is a deploy-time knob if the badge is ever noisy.
- **Ping ownership decided by configuration presence, not a switch.** A `[tool.docker]` key for
  who pings was rejected once the slug moved to compose: key and slug present means the supervisor
  pings, and the child is told at spawn. A consent file written by the app was rejected for the
  same reason.
- **Slug from compose, not the first service.** See section 5.
- **Optional `WG_*` lines rendered as `${NAME:-}`** rather than omitted, so the knobs are visible.
- **Same HEAD-vs-working-copy refusal** for `wireguard` as for `services`.
