# todo

- Hook the binary's log lines into aeth_ext's logging. Today `run` writes plain lines to stderr
  with no levels, so the container log is the only sink and nothing reaches the central log
  server. The likely shape is a Rust library in aeth_ext that connects to the central log server
  under the app's own identity and sends simple levelled messages following the server's
  protocol, as the Python client does; the binary then logs through it beside the app. Decided
  2026-09-14 as later work, outside the hub design.
- A per-project `docker/wireguard/wg0.conf` as a third configuration source, for projects without
  a hub and for local development.
- Preshared keys in fetched mode (needs a per-peer secret on the hub side).
- Generating the hub's `rules.v4` from a per-peer `allow` list in `peers.toml`, removing the
  duplicated addresses.
- A data-plane probe (ping the hub's tunnel address each poll) as a second health signal.
- Shorten the CI smoke job (about 8 minutes today). The env and fetched tests walk the tunnel
  through its states in real time: the consent reply window (60 s) and the stop grace before
  SIGKILL (30 s) are fixed constants, and WireGuard renews a handshake only every 120 s. Two
  options, both spec decisions: make the reply window and the stop grace configurable so the
  tests can set them to their floors, and run the env and fetched tests in parallel on the runner
  (they use separate networks and volumes).
- The Dockerfile template clones the project from GitHub inside the image build, so a private
  repository cannot be built: the first one, `wireguard-hub`, was made public to deploy (2026-09-15).
  Decide the proper fix: build from Coolify's checkout (`COPY` the manifests and `src/` from the
  build context, with a guard that the checked-out version equals `GIT_TAG`, keeping the pin
  honest and no credential in the build), or feed the clone a token through a BuildKit secret
  mount (never a plain `ARG`: Coolify passes every environment variable as a build arg, and an
  `ARG` value lands in the image history).
- Widen what `run` accepts as the app and as startup scripts, so a maturin `bin` project fits.
  maturin refuses `[project.scripts]` in a `bin` project ("Defining scripts and working with a
  binary doesn't mix well"), yet `run` finds both the `run-app-*` script and every
  `startup_scripts` name only through `[project.scripts]` (`pyproject.rs`). Fallbacks: exactly one
  `run-app-*` executable in `.venv/bin` (e.g. a Rust shim that execs `python -m <app>`; maturin
  ships a `python-source` package beside the binaries), and startup entries that name a
  `.venv/bin` executable or a module run with `python -m`. Typo detection then moves from the
  pyproject read to a filesystem check at `run`. Raised by pos-tunnel's relay, which wanted Rust
  one-shot tools beside a Python app (2026-10-08).
- Consider building the image from the application's released wheel instead of its git-tagged
  source. The builder's `uv sync --no-editable` builds the project from source, so a project with
  Rust in it (maturin or setuptools-rust) can't build: `uv:python3.14-bookworm-slim` has no Rust
  toolchain, and the window comes after that sync. Likely shape: sync dependencies only from the
  tagged `pyproject.toml`/`uv.lock`, then install the project's prebuilt manylinux wheel for
  `GIT_TAG` (from SFTPyPI, or the GitHub release, which the devkit release workflow already
  attaches it to). Same origin (2026-10-08).
- Restart on unhealthy, opt-in. A stale app heartbeat today only logs `unhealthy:` and sends
  `/fail` (`supervisor.rs`); the app keeps running, frozen, until someone restarts the
  container. Shape: a `[tool.docker]` switch that stops the app (`stop_all`: SIGTERM, grace,
  SIGKILL) and exits non-zero, so the restart policy brings it back; plus a setting for the app's
  max heartbeat age, hardcoded at 180 s (`heartbeat.rs`). Raised by pos-tunnel's relay
  (2026-10-08).
