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
`[tool.docker].wireguard`; the Dockerfile also carries two `# !window` regions a project may
fill, which `setup-project` renders around; the compose one also carries the `# !rule`
annotations the compose step enforces). The image installs the package with `uv sync --frozen` and uses
`/app/.venv/bin/devkit-container` for the build-time queries, as the entrypoint and as the
compose healthcheck. `devkit docker-pin` refreshes a Dockerfile that
drifted from the locked version before it pins. No Python runs in the image outside the app
itself.

## Subcommands

- `app-extra` prints `--extra app` when `[project.optional-dependencies].app` exists.
- `readme` prints `project.readme` (string or `{ file = … }` form).
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
| `startup_scripts` | console script names from `[project.scripts]` that `run` executes as root, in order, before the app; default empty |
| `scrub_env` | variable names removed from the app's environment on top of the built-in secrets; default empty |

`chown_paths` and `mkdirs` are legacy keys the entrypoint refuses.

## Environment contract

Read by `run`; empty is unset; a failure names the variable, never its value.

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
is an integer at least 1 and at most 31536000 (a year) except `WG_HOLD_LIMIT_SECS`, which
accepts 0.

## Heartbeat files

One concept: a file holding one timestamp, written by a process while it is healthy; fresh means
younger than 180 s. The timestamp is what `datetime.isoformat()` produces: an offset is honoured,
a bare one is read as container-local time. Both live in `/app/persisted_data/logs/`, a host bind
mount, so another container mounting the same path read-only can check them.

- `heartbeat.txt`: the app's, written by `aeth_ext` every 60 s.
- `wireguard-heartbeat.txt`: the tunnel's, written by the supervisor on each poll while the
  handshake is fresh, and not otherwise; current time, not handshake time, so a wedged supervisor
  stops the beats too. World-readable, written atomically, removed at start when the mode is off.
- `wireguard/peers.toml`, beside `logs/`: the last validated bundle, written by the supervisor
  after every successful fetch, read at boot when the hub's version endpoint or GitHub is
  unreachable. Holds no secrets.
- `logs/devkit-container.log`: the binary's own lines, timestamped, appended one at a time.

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
plain, `/fail`). `docker_fetched` runs fetched mode against real GitHub: a hub container serving
`/version` on the test network, the fixture repository `AetherBreaker/wireguard-hub-smoke` with
three releases (the test spoke enrolled at two addresses, then dropped), and the test's own key
pairs as constants in the source. It reads `DEVKIT_SMOKE_WG_HUB_TOKEN` from the environment (CI:
a repository secret) and refuses to run without it. Asserts the boot fetch and the cache, a
refused start when not enrolled or without a hub, the same boot tolerated under
`WG_TOLERATE_DISCONNECTED=1`, the in-place re-apply without the interface going down, the consent
hold and ok before exit 75, and the removal shutdown. The wireguard tests need the host kernel's
wireguard module (`sudo modprobe wireguard`). `ci/render.sh` dry-runs `setup-project` on a scratch project holding this checkout's
wheel, with the mode off and on, so both templates meet the released devkit's parser before a
release; a render error fails it.

## Releasing

`uv run devkit release <bump>`. The standard release workflow builds Windows and manylinux
wheels and publishes them to SFTPyPI. The Windows wheel is required, not optional: projects
install this package on Windows dev machines too, where only the query subcommands run.
