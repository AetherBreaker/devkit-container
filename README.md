# devkit-container

The image-side helper for devkit-managed Docker projects. One small static-free binary,
`devkit-container`, installed into the project's venv like any dependency, plus the Dockerfile
and compose templates `devkit setup-project` renders for the project. All three ship in one
wheel, so the files a project builds and runs with always match the binary its image installs.

## How a project uses it

`setup-project` adds `devkit-container` to `[project].dependencies` of every project with
`[tool.docker].services`, locks it to the newest release the installed devkit accepts, and
renders `docker/Dockerfile` from `devkit_container/template.Dockerfile` and the compose file from
`devkit_container/compose.template.yaml` in the venv (both carry `# !` gates on
`[tool.docker].wireguard`; the compose one also carries the `# !rule` annotations the compose
step enforces). The image installs the package with `uv sync --frozen` and uses
`/app/.venv/bin/devkit-container` for the build-time queries, as the entrypoint and as the
compose healthcheck. `devkit docker-pin` refreshes a Dockerfile that
drifted from the locked version before it pins. No Python runs in the image outside the app
itself.

## Subcommands

- `app-extra` prints `--extra app` when `[project.optional-dependencies].app` exists.
- `readme` prints `project.readme` (string or `{ file = … }` form).
- `run` is the entrypoint (Linux only). Must be root. Resolves the single `run-app-*` script
  in `[project.scripts]`; checks every `[tool.docker].required_persisted_dirs` entry is backed
  by a bind mount (the path or an ancestor below `/app`, per `/proc/self/mountinfo`) and refuses
  to start otherwise; `mkdir -p` + recursive chown to `999:999`; `setgroups([])`, `setgid`,
  `setuid`; `exec /app/.venv/bin/<script>`. `/app` itself stays root-owned: the app writes only
  to its mounted dirs or temp dirs. Entries that are empty, `.`, `..`, absolute or escape `/app`
  are errors; a table still carrying `chown_paths`/`mkdirs` is refused with the migration hint.
  Flags `--pyproject`, `--app-root`, `--mountinfo` exist for tests.

  With `[tool.docker].supervise` or `wireguard` on, the last step is a branch instead of the
  exec: the app is spawned as 999:999 with empty supplementary groups and empty capability
  sets, and the supervisor stays PID 1. It reaps zombies, forwards `SIGTERM`, `SIGINT` and
  `SIGHUP` to the child, and exits with the child's code (signal death as 128+n). Without a
  tunnel it drops to 999 itself before spawning, so no root process lingers. The child's
  environment is the supervisor's minus `WG_PRIVATE_KEY` and `WG_PEER_PRESHARED_KEY`, plus
  `DEVKIT_SUPERVISED_PING=1` when the supervisor owns the ping. A leftover
  `wireguard-heartbeat.txt` is removed at start when the mode is off.

  With `wireguard` on, the tunnel comes up after the mount check and before `prepare`: `wg` and
  `ip` on PATH (else refused: the image was built without the wireguard block), `ip link add
  wg0 type wireguard`, `wg set` with the keys over stdin, `ip address add`, `ip link set up`,
  one route per allowed IP; the public key derived from `WG_PRIVATE_KEY` is logged at every
  start; a first handshake within `WG_HANDSHAKE_TIMEOUT_SECS`, else a refused start naming the
  endpoint. Every `WG_POLL_SECS` (both modes) the supervisor reads `wg show wg0
  latest-handshakes`: a handshake older than `WG_STALE_SECS` is stale, the first stale poll
  re-sets the peer endpoint (re-resolving its name), every following one takes `wg0` down and
  up, and each re-up is logged. While the handshake is fresh it writes the current time to the
  tunnel's heartbeat file; then it checks every heartbeat file it is responsible for and pings:
  `/start` once when every file is first fresh, plain on every fresh poll, `/fail` with the
  reason on the transition to stale and with the code on a nonzero child exit. The URL is
  `ALERTS_HEALTHCHECK_PING_URL`, else `https://hc-ping.com/<PINGKEY>/<HEARTBEAT_SLUG>` with
  `?create=1`; the request goes out in-process over TLS (rustls, Mozilla's roots) on a thread,
  with a 10 s timeout, best-effort, one log line per failure. Without a URL, or a key with a
  slug, nothing pings and one line at start says so (a warning in wireguard mode, where the
  tunnel is then visible to Docker but not to healthchecks.io).
- `healthcheck` exits 0 when every heartbeat file is fresh, else 1 with one line on stderr per
  problem (missing, empty, not an ISO 8601 timestamp, stale by N s), which is what `docker
  inspect` shows. `--file PATH` is repeatable (default: the app's file under `--app-root`);
  `--max-age SECS` defaults to 180. It reads files only: no root, no capabilities, no `wg`, no
  pyproject parse, no dependence on the supervisor. A bare timestamp (no offset) is read as
  container-local time.

## `[tool.docker]` schema

| Key | Meaning |
|---|---|
| `services` | compose services `setup-project` manages; the only Docker switch |
| `required_persisted_dirs` | paths relative to `/app` the entrypoint guarantees exist, are bind-mounted and are owned by nonroot |
| `silence_unlisted_services_warning` | quiets `setup-project`'s warning when Docker files exist but `services` is empty |
| `supervise` | `run` spawns and supervises the app instead of exec'ing it; off by default |
| `wireguard` | `run` brings up the tunnel before the app and keeps it up; implies `supervise`; also gates the compose and Dockerfile templates |

`chown_paths` and `mkdirs` are legacy keys the entrypoint refuses.

## Environment contract

Read by `run`; empty is unset; a failure names the variable, never its value.

| Variable | Meaning | Required |
|---|---|---|
| `WG_PRIVATE_KEY` | this peer's private key; secret, scrubbed from the child | with `wireguard` |
| `WG_ADDRESS` | this peer's tunnel address, CIDR (`10.8.0.20/32`) | with `wireguard` |
| `WG_PEER_PUBLIC_KEY` | the hub's public key | with `wireguard` |
| `WG_PEER_ENDPOINT` | `host:port` of the hub | with `wireguard` |
| `WG_PEER_ALLOWED_IPS` | comma-separated CIDRs routed through the hub | with `wireguard` |
| `WG_PEER_PRESHARED_KEY` | secret, scrubbed from the child | no |
| `WG_PERSISTENT_KEEPALIVE` | seconds; default 25; zero refused (an idle tunnel would never re-handshake) | no |
| `WG_HANDSHAKE_TIMEOUT_SECS` | default 60 | no |
| `WG_POLL_SECS` | the supervisor's poll, both modes; default 30 | no |
| `WG_STALE_SECS` | default 180 | no |
| `HEARTBEAT_SLUG` | the healthchecks.io slug; compose sets it to the service name; fallback: the single `services` entry | no |
| `PINGKEY` | the healthchecks.io ping key; with a slug, builds the autoprovisioning URL | no |
| `ALERTS_HEALTHCHECK_PING_URL` | a fixed ping URL; wins over the key | no |
| `DEVKIT_SUPERVISED_PING` | set to `1` on the child when the supervisor owns the ping; `aeth_ext` then skips its own periodic ping | set by `run` |

## Heartbeat files

One concept: a file holding one timestamp, written by a process while it is healthy; fresh means
younger than 180 s. The timestamp is what `datetime.isoformat()` produces: an offset is honoured,
a bare one is read as container-local time. Both live in `/app/persisted_data/logs/`, a host bind
mount, so another container mounting the same path read-only can check them.

- `heartbeat.txt`: the app's, written by `aeth_ext` every 60 s.
- `wireguard-heartbeat.txt`: the tunnel's, written by the supervisor on each poll while the
  handshake is fresh, and not otherwise; current time, not handshake time, so a wedged supervisor
  stops the beats too. World-readable, written atomically, removed at start when the mode is off.

## Tests

`cargo test` covers the parsers, the heartbeat files, the ping URLs, the `WG_*` contract, the
stale-and-re-up rule, the ping decision and the `healthcheck` subcommand; on Linux as root, the
entrypoint. The smoke tests (`cargo test --test docker_smoke --test docker_supervisor -- --ignored
--nocapture`; CI runs them) build the wheel inside the official maturin container (a manylinux
wheel, as a release builds one; the host needs Docker and nothing else), build the template
Dockerfile around a scratch app with that wheel installed into the venv, and start it on a named
volume. `docker_smoke` checks the app's own report in exec mode: PID 1, uid/gid 999, `/app`
read-only, the persisted dirs created, owned and writable, the venv, the `app` extra and the
wheel install; a run without the volume or as non-root is refused first; then `healthcheck` in
the image against the fresh file and a stale one. `docker_supervisor` builds the image with the
mode on and runs it three ways: supervised without a tunnel (parent PID 1, uid 999, empty
capabilities, `SIGTERM` and the exit code passed through), with the tunnel against a hub
container built from the same image (handshake, the tunnel heartbeat fresh and readable, the
secrets scrubbed, stale then re-upped when the hub forgets and re-learns the peer, the
two-file healthcheck naming the file), and the ping against a local HTTP listener (`/start` once,
plain, `/fail`). The wireguard test needs the host kernel's wireguard module (`sudo modprobe
wireguard`). `ci/render.sh` renders both templates through the released devkit into a scratch
project with the mode off and on and fails on a render error or a marker left behind.

## Releasing

`uv run devkit release <bump>`. The standard release workflow builds Windows and manylinux
wheels and publishes them to SFTPyPI. The Windows wheel is required, not optional: projects
install this package on Windows dev machines too, where only the query subcommands run.
