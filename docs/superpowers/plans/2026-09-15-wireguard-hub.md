# The WireGuard hub: the `wireguard-hub` implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task, inline in the session that holds it (spec 0.2 rule 4: never delegated to subagents). Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Release-order step 3 of the hub design (spec 14): the `wireguard-hub` package, its peer table and firewall policy, the startup script that brings `wg0` up from the table, the app that serves `/version` and writes the heartbeat, the `peers` release job that validates, stamps and attaches the bundle, the hand-written Docker additions, and the first release, so spokes have a hub to fetch from.

**Architecture:** One Python package `wireguard_hub` with two console scripts (3.1) and three plain modules: `peers.py` (load, validate, stamp, render a peer's conf; the one place the table's rules live), `up.py` (`wireguard-hub-up`, root, `subprocess` argument lists, exits), `__main__.py` (`run-app-wireguard-hub`, unprivileged, a threading HTTP server plus a heartbeat loop gated on the interface), and `bundle.py` (the release job's entry, `python -m wireguard_hub.bundle <tag> <dir>`, so the workflow and CI run the same validation the tests do). Package data `peers.toml` and `rules.v4` live inside `src/wireguard_hub/` because the image copies `src/`. Everything devkit renders stays devkit's; the hub's additions to compose, the Dockerfile window and `release.yml` are written by hand once and survive re-renders by the features shipped in release-order step 1.

**Tech Stack:** Python 3.14 (`tomllib`, `ipaddress`, `http.server`, `subprocess`, `importlib.metadata`), `aeth-ext` 9.0.2 for `send_heartbeat`, `uv` with `uv_build`, `pytest`, `ruff` (2-space indent, line length 135, google docstrings), `pyright` standard; devkit 15.1.0 with devkit-templates 1.3.0 and devkit-container 2.1.0 (already locked in this repo); `gh` for PRs, CI and the release.

**Spec:** `docs/superpowers/specs/2026-09-14-hub-fetched-peer-config-design.md`, in this repository and in every repository that carries this plan. Sections implemented here: 3 (all of it), 4.1 (the endpoint the hub serves), 7 and 8 (how the entrypoint runs the startup script and scrubs the key; consumed, not changed), 9.2 and 9.3 (why the hand-written lines survive), 11 (the decided names, addresses and port), 13 (the `wireguard-hub` tests), 14 step 3, 16 (the host requirements and the first-deploy checklist, steps 1 to 4).

## Progress tracking (owner's instruction, 2026-09-15)

- The executor ticks each step's box (`- [ ]` to `- [x]`) **as the step completes**, not at the end of the task, in the plan copy of the repository the step's commit lands in: `wireguard-hub` for every task. After every task the ticked file is mirrored over the copies in `aeth_devkit`, `devkit-templates` and `devkit-container` (an uncommitted mirror is enough between tasks; Task 9 commits the final one everywhere), so the owner reads progress from any copy.
- Every "Lint and commit" step names the plan file in its `git add` so the ticks ship with the work.
- A step that cannot be completed is left unticked with a one-line note under it saying why; the plan is never edited to make it pass.

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

The owner's ruling on what "stop" means (2026-09-15): stop only when the fix would change one of the owner's decisions or produce an unexpected change in externally visible behaviour; implementation details (the exact command, the ordering inside a step, log and error text, test mechanics) are the implementer's. When a stop is needed, keep working on every part of the plan that is not gated by it, then present the blockers together.

## Owner inputs this plan needs, and when

1. **The hub's key pair, before Task 1.** The owner generates it where `wg` is installed (`wg genkey | tee hub.key | wg pubkey`), keeps the private key for Coolify's `WG_HUB_PRIVATE_KEY` (never pasted into chat, never committed) and gives the executor the public key only; it goes into `peers.toml` `[hub].public_key`.
2. **Peer rows, before Task 8 (the first release).** A `[[peers]]` row needs the spoke's public key, which a spoke logs at its first refused start (3.8), and the office PC's, which the owner generates the same way. The first release may ship with the hub section and no peer rows: 3.2's rules are all "every peer …" and hold vacuously, and enrolment is a later release by design (3.8). The owner says which rows go into the first release; the addresses are fixed by section 11 (office PC `10.8.0.10/32`, ScheduledReportAggregator `10.8.0.20/32`, `tunnel-probe` `10.8.0.21/32`, further peers from `.22`).
3. **The Coolify deployment (Task 8, step 5), the owner's hands.** Environment values `WG_HUB_PRIVATE_KEY`, `PINGKEY`, `ALERTS_EMAIL_PWD`; the domain `tunnels.sweetfiretobacco.com` attached to the `wireguard-hub` service on container port 8000 (3.6); then the checks of section 16 steps 1 to 4, which the executor cannot run from here.

## Global constraints

- Repository `AetherBreaker/wireguard-hub`, private, at `D:\SFT Software Projects\SFT Workspace\wireguard-hub`; package `wireguard_hub`; compose service, container name and healthchecks.io slug `wireguard-hub` (spec 3, 11). The three other repositories carrying this plan: `aeth_devkit`, `devkit-templates`, `devkit-container`, same workspace folder.
- The repository's `AGENTS.md` (rendered by devkit) applies: run Python under `uv run`; tests never define intent; no small single-use helpers (a body of 4 lines or fewer is inlined); comments carry the *why*, densely; Conventional Commits with the module as scope; every workflow, job and step `name:` says what runs.
- Console scripts, exactly two (3.1): `wireguard-hub-up` = `wireguard_hub.up:main`, `run-app-wireguard-hub` = `wireguard_hub.__main__:run_app` (already in `pyproject.toml`).
- `[tool.docker]` and `[tool.devkit]` exactly as 3.6 (already in `pyproject.toml`): `services = ["wireguard-hub"]`, `required_persisted_dirs = ["persisted_data"]`, `supervise = false`, `wireguard = false`, `startup_scripts = ["wireguard-hub-up"]`, `scrub_env = ["WG_HUB_PRIVATE_KEY"]`, `release-workflow-jobs = ["peers"]`.
- `peers.toml` format and every validation rule verbatim from 3.2; the release stamps `hub_version = "vX.Y.Z"` as the first top-level key and nothing else differs between the committed file and the bundle asset (3.2, 3.7).
- `rules.v4` holds the policy only: `FORWARD DROP`, accept `ESTABLISHED,RELATED` on `wg0` to `wg0`; the `INPUT` chain is not touched (3.3).
- The startup script's eight steps and their exact commands (3.4); "any command failing is an error naming the command, never the key"; `subprocess` with argument lists, never a shell string.
- The app (3.5): `0.0.0.0:8000`, threading server in a thread, `GET /version` → `200`, `text/plain; charset=utf-8`, body `v<package version>\n`, everything else `404`, no other routes; the heartbeat every 60 s to `/app/persisted_data/logs/heartbeat.txt` with `aeth_ext`'s one-shot `send_heartbeat`, only while `/sys/class/net/wg0` exists, ping key and slug passed only when `DEVKIT_SUPERVISED_PING` is unset; shutdown on SIGINT/SIGTERM.
- Hand-written and surviving re-renders (3.6): compose `cap_add: [NET_ADMIN]`, `sysctls: [net.ipv4.ip_forward=1]`, `ports: ["51820:51820/udp"]`, `- WG_HUB_PRIVATE_KEY=${WG_HUB_PRIVATE_KEY:?}` under `environment`; the Dockerfile `final` window holds `RUN apt-get update && apt-get install -y --no-install-recommends wireguard-tools iproute2 iptables && rm -rf /var/lib/apt/lists/*`.
- The `peers` job (3.7): same release event, own `permissions: contents: write`, guards as the publish job (checks out `github.sha`, verifies the release still owns the tag before uploading), validates, stamps, renders `<peer name>.conf` per peer in the given ini shape, attaches `peers.toml` and every conf; fails the run on invalid input. Every push also validates through the hand-written `ci.yml`.
- Addresses and port (11): subnet `10.8.0.0/24`, hub `10.8.0.1/24`, `listen_port` and UDP port 51820, endpoint `tunnels.sweetfiretobacco.com:51820`, `allowed_ips = ["10.8.0.0/24"]`, `persistent_keepalive = 25`.
- The release is the devkit release command, unchanged (3.7); it publishes to SFTPyPI and creates a GitHub release, so the executor asks the owner before running it.
- Commands run from the repository root, Bash syntax (Git Bash on Windows). `uv` needs the SFTPyPI credentials in the environment: `set -a; . ./.env; set +a` before `uv sync`, never printing `.env`.

## Decisions this plan makes where the spec is silent

Implementation details under the owner's ruling; listed so the owner can veto any before execution.

1. Module layout: `peers.py`, `up.py`, `__main__.py`, `bundle.py` under `src/wireguard_hub/`; tests under `tests/` (`test_peers.py`, `test_up.py`, `test_app.py`, `test_bundle.py`, `test_rules.py`), all pure Python, no Docker, runnable on Windows.
2. The release job's entry is `python -m wireguard_hub.bundle <tag> <out-dir>`, a module and not a third console script, so 3.1's "two programs" holds; CI runs the same module with a placeholder tag `v0.0.0` to validate on every push.
3. `peers.toml` validation refuses unknown keys in `[hub]` and in a `[[peers]]` row, and unknown top-level keys other than `hub_version`, naming the key: a misspelt `persistent_keepalive` must not silently become "no override". "Any failure names the field" is read to include this. `hub_version`, when present, must be a string matching `^v[0-9]+\.[0-9]+\.[0-9]+$`; the hub never checks it against anything (the spoke does, 4.2).
4. "`hub.address` a CIDR with a host part" is read as: `ipaddress.ip_interface` parses it, its prefix is shorter than the address family's maximum, and its host address is neither the network address nor the broadcast address. An `endpoint` is `host:port` split at the last colon, host non-empty and without whitespace, port an integer 1 to 65535; the hostname is not resolved.
5. Errors are `PeersError(ValueError)` with one message naming the field, e.g. ``peers[2].address: 10.9.0.5/32 is not inside 10.8.0.0/24``, ``hub.listen_port: must be an integer 1 to 65535``. Booleans are refused where integers are required (`True` is an `int` in Python).
6. The startup script reads `peers.toml` and `rules.v4` from the installed package (`importlib.resources`), logs to stderr with the prefix `wireguard-hub-up:`, and exits 1 on any error. A failing command is reported as ``wireguard-hub-up: `<argv joined>` exited <code>``; the argument lists never contain the key (it goes to stdin), so the message never can either. Step 8's log line: ``wireguard-hub-up: wg0 up, public key <key>, <n> peer(s)``.
7. The app creates `/app/persisted_data/logs` (parents, exist-ok) before its first beat: the entrypoint creates `persisted_data/logs` implicitly only when supervising (devkit-container README), and `persisted_data` is owned by `999:999`, so the unprivileged app may create the folder. The paths and the interval are module constants (`HEARTBEAT_FILE`, `INTERFACE`, `BEAT_SECS`), so the tests point them elsewhere.
8. The app reads `PINGKEY`, `HEARTBEAT_SLUG` and `ALERTS_HEALTHCHECK_PING_URL` straight from the environment and passes them to `send_heartbeat` (as `pydantic.SecretStr` where the signature wants one) instead of building `aeth_ext`'s settings object, whose required fields (`ALERTS_EMAIL_PWD`) the hub has no other use for. The HTTP handler's per-request log line is suppressed (`log_message` overridden to nothing): a 30 s Traefik probe would otherwise fill the container log.
9. Conf rendering (3.7): `AllowedIPs` joined with `", "`; the file's line order and blank line exactly as the spec's block; the `PrivateKey` placeholder `REPLACE_WITH_THIS_PEERS_PRIVATE_KEY` verbatim.
10. The stamped bundle is the committed text with the line `hub_version = "<tag>"` and one blank line prepended, so "nothing else differs" is literally true and the attached file is diffable against the commit.
11. `ci.yml`: workflow `Verify: tests, the peer table validates and renders`, one job on `ubuntu-latest` running `uv sync`, `uv run pytest`, `uv run ruff check`, `uv run pyright`, and the bundle module with tag `v0.0.0` into a scratch folder; it needs the SFTPyPI secrets for `uv sync` (already set on the repository). The `peers` job needs the same two secrets for its `uv sync`.
12. Versions: the first release is `1.0.0` (`poe release major`, from the skeleton's `0.1.0`). After it, `poe docker-pin` pins the compose file's `GIT_TAG` (the rendered file says `v0.1.0` with a note to pin after the first release).
13. Branch `feat/hub` in `wireguard-hub`; the plan copies in the other three repositories go straight onto their `main` (docs only).
14. The heartbeat test uses the real `send_heartbeat` with no ping key (it writes the file and pings nothing) and checks the file's timestamp parses; the gating tests monkeypatch `send_heartbeat` in the module's namespace to record its keyword arguments.

## File structure

- Create `src/wireguard_hub/peers.py`: `Hub`, `Peer`, `Table`, `PeersError`, `parse`, `load`, `default_path`, `stamp`, `render_conf`, `TAG_RE`.
- Create `src/wireguard_hub/peers.toml` (the owner's hub public key; rows per owner input 2) and `src/wireguard_hub/rules.v4`.
- Replace `src/wireguard_hub/up.py` (the stub) with the startup script.
- Replace `src/wireguard_hub/__main__.py` (the stub) with the app.
- Create `src/wireguard_hub/bundle.py`.
- Create `tests/test_peers.py`, `tests/test_rules.py`, `tests/test_up.py`, `tests/test_app.py`, `tests/test_bundle.py`.
- Modify `pyproject.toml` (`aeth-ext` dependency), `docker/compose.yaml` and `docker/Dockerfile` (the hand-written lines), `.github/workflows/release.yml` (the `peers` job); create `.github/workflows/ci.yml`; modify `README.md`.
- Create the plan copies: `aeth_devkit/docs/superpowers/plans/2026-09-15-wireguard-hub.md`, `devkit-templates/docs/superpowers/plans/2026-09-15-wireguard-hub.md`, `devkit-container/docs/superpowers/plans/2026-09-15-wireguard-hub.md` (the spec is already in all four).

---

### Task 0: the plan copies and the branch

**Files:**
- Create: the three plan copies named above.

- [ ] **Step 1: Copy this plan into the three other repositories and commit each on `main`**

```bash
ws="/d/SFT Software Projects/SFT Workspace"
plan=docs/superpowers/plans/2026-09-15-wireguard-hub.md
for r in aeth_devkit devkit-templates devkit-container; do
  cd "$ws/$r" && git checkout main && git pull --ff-only
  cp "$ws/wireguard-hub/$plan" "$plan"
  git add "$plan"
  git commit -m "docs(plans): the wireguard-hub plan, copied for release-order step 3

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
  git push
done
```

- [ ] **Step 2: Branch `wireguard-hub`**

```bash
cd "/d/SFT Software Projects/SFT Workspace/wireguard-hub"
git status --short            # must be empty
git checkout main && git pull --ff-only
git checkout -b feat/hub
```

- [ ] **Step 3: Tick Task 0 and commit the tick**

```bash
cd "/d/SFT Software Projects/SFT Workspace/wireguard-hub"
# tick Task 0 in docs/superpowers/plans/2026-09-15-wireguard-hub.md
git add docs/superpowers/plans/2026-09-15-wireguard-hub.md
git commit -m "docs(plans): tick task 0

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 1: the dependency, the package data and the peer table module

**Files:**
- Modify: `pyproject.toml` (`[project].dependencies`)
- Create: `src/wireguard_hub/peers.toml`, `src/wireguard_hub/rules.v4`, `src/wireguard_hub/peers.py`, `tests/test_peers.py`, `tests/test_rules.py`

**Interfaces:**
- Produces: `peers.parse(text: str) -> Table`, `peers.load(path: Path) -> Table`, `peers.default_path() -> Path`, `peers.rules_path() -> Path`, `peers.stamp(text: str, tag: str) -> str`, `peers.render_conf(table: Table, peer: Peer) -> str`, `peers.TAG_RE`, `peers.PeersError`; dataclasses `Hub`, `Peer`, `Table` (fields below). Tasks 2, 3 and 4 consume them.

- [ ] **Step 1: Add `aeth-ext` and sync**

In `pyproject.toml`, change `dependencies = ["devkit-container>=2.1.0"]` (keep the trailing `# setup-project added` comment tombi placed) to hold both:

```toml
  dependencies    = ["aeth-ext>=9.0.2", "devkit-container>=2.1.0"]
```

Then:

```bash
cd "/d/SFT Software Projects/SFT Workspace/wireguard-hub"
set -a; . ./.env; set +a
uv lock && uv sync
uv run python -c "from aeth_ext.monitoring.heartbeat import send_heartbeat; print('ok')"
```

Expected: `ok`. (The `[tool.uv.sources]` entry for `aeth-ext` already points at SFTPyPI.)

- [ ] **Step 2: Write the package data**

`src/wireguard_hub/peers.toml`, with the owner's hub public key (owner input 1) in place of `<HUB PUBLIC KEY>` and no peer rows until owner input 2:

```toml
schema = 1

[hub]
name = "wireguard-hub"
public_key = "<HUB PUBLIC KEY>"
address = "10.8.0.1/24"                        # interface address; its network is the tunnel subnet
listen_port = 51820
endpoint = "tunnels.sweetfiretobacco.com:51820"
allowed_ips = ["10.8.0.0/24"]                  # default AllowedIPs for every peer
persistent_keepalive = 25                      # default for every peer

# One [[peers]] row per enrolled spoke (hub design 3.2, 3.8). Addresses: office database PC
# 10.8.0.10/32, ScheduledReportAggregator 10.8.0.20/32, tunnel-probe 10.8.0.21/32, further
# peers from .22 (section 11). Optional per-row overrides, each with the meaning of the [hub]
# default it replaces: endpoint, allowed_ips, persistent_keepalive.
```

`src/wireguard_hub/rules.v4`:

```text
# The filter table, applied whole by wireguard-hub-up (hub design 3.3). Policy only: forwarding
# is dropped unless a flow is listed here; replies to a permitted flow are accepted. Per-flow
# ACCEPT lines (a spoke's /32 to the database PC's /32 on the database port) are the next
# phase's, once the engine, port and protocol are decided. INPUT is not touched: Docker
# publishes the UDP port and the container's default INPUT policy accepts.
*filter
:INPUT ACCEPT [0:0]
:FORWARD DROP [0:0]
:OUTPUT ACCEPT [0:0]
-A FORWARD -i wg0 -o wg0 -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
COMMIT
```

- [ ] **Step 3: Write the failing tests**

`tests/test_peers.py`:

```python
# Standard library imports
import base64

# Third party imports
import pytest

# First party imports
from wireguard_hub import peers
from wireguard_hub.peers import PeersError

HUB_KEY = base64.b64encode(bytes(range(32))).decode()
KEY_A = base64.b64encode(bytes([1] * 32)).decode()
KEY_B = base64.b64encode(bytes([2] * 32)).decode()

GOOD = f"""
schema = 1

[hub]
name = "wireguard-hub"
public_key = "{HUB_KEY}"
address = "10.8.0.1/24"
listen_port = 51820
endpoint = "tunnels.example:51820"
allowed_ips = ["10.8.0.0/24"]
persistent_keepalive = 25

[[peers]]
name = "office-db-pc"
public_key = "{KEY_A}"
address = "10.8.0.10/32"

[[peers]]
name = "scheduled-report-aggregator"
public_key = "{KEY_B}"
address = "10.8.0.20/32"
endpoint = "wireguard-hub:51820"
allowed_ips = ["10.8.0.10/32"]
persistent_keepalive = 15
"""


def test_a_valid_table_parses_with_defaults_and_overrides():
  t = peers.parse(GOOD)
  assert t.hub.name == "wireguard-hub" and t.hub.listen_port == 51820 and t.hub.allowed_ips == ("10.8.0.0/24",)
  assert t.hub_version is None
  assert [p.name for p in t.peers] == ["office-db-pc", "scheduled-report-aggregator"]
  a, b = t.peers
  assert a.endpoint is None and a.allowed_ips is None and a.persistent_keepalive is None
  assert b.endpoint == "wireguard-hub:51820" and b.allowed_ips == ("10.8.0.10/32",) and b.persistent_keepalive == 15
  # No peers is a valid table: every rule is "every peer ...".
  assert peers.parse(GOOD.split("[[peers]]")[0]).peers == ()
  # A stamped bundle parses and carries its version.
  assert peers.parse(peers.stamp(GOOD, "v1.2.3")).hub_version == "v1.2.3"


@pytest.mark.parametrize(
  ("edit", "field"),
  [
    (("schema = 1", "schema = 2"), "schema"),
    (("schema = 1", ""), "schema"),
    (('name = "wireguard-hub"', 'name = "Wireguard-Hub"'), "hub.name"),
    ((f'public_key = "{HUB_KEY}"', 'public_key = "not-base64"'), "hub.public_key"),
    ((f'public_key = "{HUB_KEY}"', f'public_key = "{base64.b64encode(bytes(31)).decode()}"'), "hub.public_key"),
    (('address = "10.8.0.1/24"', 'address = "10.8.0.0/24"'), "hub.address"),
    (('address = "10.8.0.1/24"', 'address = "10.8.0.1/32"'), "hub.address"),
    (('address = "10.8.0.1/24"', 'address = "10.8.0.1"'), "hub.address"),
    (("listen_port = 51820", "listen_port = 0"), "hub.listen_port"),
    (("listen_port = 51820", "listen_port = 70000"), "hub.listen_port"),
    (("listen_port = 51820", "listen_port = true"), "hub.listen_port"),
    (('endpoint = "tunnels.example:51820"', 'endpoint = "tunnels.example"'), "hub.endpoint"),
    (('endpoint = "tunnels.example:51820"', 'endpoint = "tunnels.example:0"'), "hub.endpoint"),
    (('endpoint = "tunnels.example:51820"', 'endpoint = ":51820"'), "hub.endpoint"),
    (('allowed_ips = ["10.8.0.0/24"]', "allowed_ips = []"), "hub.allowed_ips"),
    (('allowed_ips = ["10.8.0.0/24"]', 'allowed_ips = ["10.8.0.1/24"]'), "hub.allowed_ips"),
    (("persistent_keepalive = 25", "persistent_keepalive = 0"), "hub.persistent_keepalive"),
    (("persistent_keepalive = 25\n", 'persistent_keepalive = 25\ncolour = "red"\n'), "hub.colour"),
    (('name = "office-db-pc"', 'name = "scheduled-report-aggregator"'), "peers[1].name"),
    (('name = "office-db-pc"', 'name = "-bad"'), "peers[0].name"),
    ((f'public_key = "{KEY_A}"', f'public_key = "{KEY_B}"'), "peers[1].public_key"),
    ((f'public_key = "{KEY_A}"', f'public_key = "{KEY_A[:-1]}"'), "peers[0].public_key"),
    (('address = "10.8.0.10/32"', 'address = "10.8.0.10/24"'), "peers[0].address"),
    (('address = "10.8.0.10/32"', 'address = "10.9.0.10/32"'), "peers[0].address"),
    (('address = "10.8.0.10/32"', 'address = "10.8.0.20/32"'), "peers[1].address"),
    (('address = "10.8.0.10/32"', 'address = "10.8.0.1/32"'), "peers[0].address"),
    (('endpoint = "wireguard-hub:51820"', 'endpoint = "wireguard-hub:99999"'), "peers[1].endpoint"),
    (('allowed_ips = ["10.8.0.10/32"]', 'allowed_ips = ["10.8.0.10/33"]'), "peers[1].allowed_ips"),
    (("persistent_keepalive = 15", "persistent_keepalive = 65536"), "peers[1].persistent_keepalive"),
    (("persistent_keepalive = 15\n", 'persistent_keepalive = 15\nextra = 1\n'), "peers[1].extra"),
    (("schema = 1\n", 'schema = 1\nhub_version = "1.2.3"\n'), "hub_version"),
    (("schema = 1\n", "schema = 1\nnote = 1\n"), "note"),
  ],
)
def test_each_rule_of_3_2_fails_naming_the_field(edit, field):
  old, new = edit
  assert old in GOOD
  with pytest.raises(PeersError) as e:
    peers.parse(GOOD.replace(old, new, 1))
  assert field in str(e.value), str(e.value)


def test_a_missing_field_and_bad_toml_name_what_is_wrong():
  with pytest.raises(PeersError, match="hub.endpoint"):
    peers.parse(GOOD.replace('endpoint = "tunnels.example:51820"\n', ""))
  with pytest.raises(PeersError, match="TOML"):
    peers.parse("schema = [")
  with pytest.raises(PeersError, match="hub"):
    peers.parse("schema = 1\n")


def test_stamp_prepends_hub_version_and_nothing_else():
  stamped = peers.stamp(GOOD, "v1.2.3")
  assert stamped == 'hub_version = "v1.2.3"\n\n' + GOOD
  for bad in ["1.2.3", "v1.2", "v1.2.3-rc1", "../x"]:
    with pytest.raises(PeersError, match="tag"):
      peers.stamp(GOOD, bad)


def test_render_conf_uses_overrides_else_hub_defaults():
  t = peers.parse(GOOD)
  a, b = t.peers
  assert peers.render_conf(t, a) == (
    "[Interface]\n"
    "PrivateKey = REPLACE_WITH_THIS_PEERS_PRIVATE_KEY\n"
    "Address = 10.8.0.10/32\n"
    "\n"
    "[Peer]\n"
    f"PublicKey = {HUB_KEY}\n"
    "Endpoint = tunnels.example:51820\n"
    "AllowedIPs = 10.8.0.0/24\n"
    "PersistentKeepalive = 25\n"
  )
  conf = peers.render_conf(t, b)
  assert "Endpoint = wireguard-hub:51820\n" in conf
  assert "AllowedIPs = 10.8.0.10/32\n" in conf
  assert "PersistentKeepalive = 15\n" in conf
  # Two hub-level lists render comma-separated.
  t2 = peers.parse(GOOD.replace('allowed_ips = ["10.8.0.0/24"]', 'allowed_ips = ["10.8.0.0/24", "10.9.0.0/24"]'))
  assert "AllowedIPs = 10.8.0.0/24, 10.9.0.0/24\n" in peers.render_conf(t2, t2.peers[0])


def test_the_shipped_table_is_valid():
  t = peers.load(peers.default_path())
  assert t.hub.name == "wireguard-hub"
  assert t.hub.address == "10.8.0.1/24" and t.hub.listen_port == 51820
  assert t.hub.endpoint == "tunnels.sweetfiretobacco.com:51820"
```

`tests/test_rules.py`:

```python
# Standard library imports
import re

# First party imports
from wireguard_hub import peers


def test_every_address_in_rules_v4_is_a_peer_or_hub_address_and_the_policy_holds():
  text = peers.rules_path().read_text(encoding="utf-8")
  lines = [line for line in text.splitlines() if line and not line.startswith("#")]
  assert lines[0] == "*filter" and lines[-1] == "COMMIT"
  assert ":FORWARD DROP [0:0]" in lines
  assert "-A FORWARD -i wg0 -o wg0 -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT" in lines
  assert not any(line.startswith(("-A INPUT", "-I INPUT", ":INPUT DROP")) for line in lines)
  table = peers.load(peers.default_path())
  known = {p.address for p in table.peers} | {table.hub.address}
  for addr in re.findall(r"\b\d+\.\d+\.\d+\.\d+/\d+\b", text):
    assert addr in known, f"{addr} is in rules.v4 but not in peers.toml"
```

- [ ] **Step 4: Run the tests to see them fail**

Run: `cd "/d/SFT Software Projects/SFT Workspace/wireguard-hub" && uv run pytest tests/test_peers.py tests/test_rules.py -q`
Expected: collection errors, `No module named 'wireguard_hub.peers'`.

- [ ] **Step 5: Implement `peers.py`**

```python
"""The peer table `peers.toml` (hub design 3.2): loading, validation, the stamped bundle (3.7) and a peer's conf.

Validation is the one copy of 3.2's rules, applied by the startup script at hub start, by the release
job through `bundle.py`, and by the tests; every failure names the field.
"""

# Standard library imports
import base64
import binascii
import ipaddress
import re
import tomllib
from dataclasses import dataclass
from importlib import resources
from pathlib import Path

NAME_RE = re.compile(r"^[a-z0-9][a-z0-9-]*$")
TAG_RE = re.compile(r"^v[0-9]+\.[0-9]+\.[0-9]+$")
PORT_MAX = 65535
KEY_CHARS = 44
KEY_BYTES = 32
HUB_KEYS = frozenset({"name", "public_key", "address", "listen_port", "endpoint", "allowed_ips", "persistent_keepalive"})
PEER_KEYS = frozenset({"name", "public_key", "address", "endpoint", "allowed_ips", "persistent_keepalive"})


class PeersError(ValueError):
  """A `peers.toml` that fails 3.2; the message names the field."""


@dataclass(frozen=True)
class Hub:
  """The `[hub]` table."""

  name: str
  public_key: str
  address: str
  listen_port: int
  endpoint: str
  allowed_ips: tuple[str, ...]
  persistent_keepalive: int


@dataclass(frozen=True)
class Peer:
  """One `[[peers]]` row; `None` means the hub default applies."""

  name: str
  public_key: str
  address: str
  endpoint: str | None = None
  allowed_ips: tuple[str, ...] | None = None
  persistent_keepalive: int | None = None


@dataclass(frozen=True)
class Table:
  """A validated table; `hub_version` is set only on a stamped bundle."""

  hub: Hub
  peers: tuple[Peer, ...]
  hub_version: str | None = None


def default_path() -> Path:
  """The shipped `peers.toml`, inside the package because the image copies `src/`."""
  return Path(str(resources.files("wireguard_hub").joinpath("peers.toml")))


def rules_path() -> Path:
  """The shipped `rules.v4` (3.3)."""
  return Path(str(resources.files("wireguard_hub").joinpath("rules.v4")))


def load(path: Path) -> Table:
  """Read and validate the table at `path`."""
  return parse(path.read_text(encoding="utf-8"))


def parse(text: str) -> Table:
  """Validate `text` as 3.2 describes and return the table."""
  try:
    data = tomllib.loads(text)
  except tomllib.TOMLDecodeError as e:
    raise PeersError(f"peers.toml is not valid TOML: {e}") from None
  for key in data:
    if key not in {"schema", "hub", "peers", "hub_version"}:
      raise PeersError(f"{key}: unknown top-level key")
  if data.get("schema") is not 1 or isinstance(data.get("schema"), bool):  # noqa: F632 - `is` rejects True and 1.0
    raise PeersError("schema: must be 1")
  version = data.get("hub_version")
  if version is not None and not (isinstance(version, str) and TAG_RE.match(version)):
    raise PeersError("hub_version: must be a release tag like v1.2.3")
  hub_data = data.get("hub")
  if not isinstance(hub_data, dict):
    raise PeersError("hub: missing table")
  hub = _hub(hub_data)
  network = ipaddress.ip_interface(hub.address).network
  rows = data.get("peers", [])
  if not isinstance(rows, list) or not all(isinstance(r, dict) for r in rows):
    raise PeersError("peers: must be [[peers]] tables")
  peers: list[Peer] = []
  for i, row in enumerate(rows):
    peers.append(_peer(row, f"peers[{i}]", hub, network, peers))
  return Table(hub=hub, peers=tuple(peers), hub_version=version)


def _hub(d: dict[str, object]) -> Hub:
  for key in d:
    if key not in HUB_KEYS:
      raise PeersError(f"hub.{key}: unknown key")
  address = _text(d, "hub.address")
  try:
    iface = ipaddress.ip_interface(address)
  except ValueError:
    raise PeersError(f"hub.address: {address} is not an address with a prefix") from None
  net = iface.network
  if iface.network.prefixlen >= iface.max_prefixlen or iface.ip in (net.network_address, net.broadcast_address):
    raise PeersError(f"hub.address: {address} must be a host address inside its own network")
  return Hub(
    name=_name(d, "hub.name"),
    public_key=_key(d, "hub.public_key"),
    address=address,
    listen_port=_port(_integer(d, "hub.listen_port"), "hub.listen_port"),
    endpoint=_endpoint(_text(d, "hub.endpoint"), "hub.endpoint"),
    allowed_ips=_cidrs(d, "hub.allowed_ips"),
    persistent_keepalive=_port(_integer(d, "hub.persistent_keepalive"), "hub.persistent_keepalive"),
  )


def _peer(d: dict[str, object], at: str, hub: Hub, network: ipaddress.IPv4Network | ipaddress.IPv6Network, seen: list[Peer]) -> Peer:
  for key in d:
    if key not in PEER_KEYS:
      raise PeersError(f"{at}.{key}: unknown key")
  name = _name(d, f"{at}.name")
  if any(p.name == name for p in seen):
    raise PeersError(f"{at}.name: {name} is used twice")
  key = _key(d, f"{at}.public_key")
  if key == hub.public_key or any(p.public_key == key for p in seen):
    raise PeersError(f"{at}.public_key: used twice")
  address = _text(d, f"{at}.address")
  try:
    iface = ipaddress.ip_interface(address)
  except ValueError:
    raise PeersError(f"{at}.address: {address} is not an address with a prefix") from None
  if iface.network.prefixlen != iface.max_prefixlen:
    raise PeersError(f"{at}.address: {address} must be a /{iface.max_prefixlen}")
  if iface.ip not in network:
    raise PeersError(f"{at}.address: {address} is not inside {network}")
  if iface.ip == ipaddress.ip_interface(hub.address).ip:
    raise PeersError(f"{at}.address: {address} is the hub's own address")
  if any(p.address == address for p in seen):
    raise PeersError(f"{at}.address: {address} is used twice")
  return Peer(
    name=name,
    public_key=key,
    address=address,
    endpoint=_endpoint(_text(d, f"{at}.endpoint"), f"{at}.endpoint") if "endpoint" in d else None,
    allowed_ips=_cidrs(d, f"{at}.allowed_ips") if "allowed_ips" in d else None,
    persistent_keepalive=_port(_integer(d, f"{at}.persistent_keepalive"), f"{at}.persistent_keepalive")
    if "persistent_keepalive" in d
    else None,
  )


def _text(d: dict[str, object], field: str) -> str:
  value = d.get(field.rsplit(".", 1)[1])
  if not isinstance(value, str) or not value:
    raise PeersError(f"{field}: must be a non-empty string")
  return value


def _integer(d: dict[str, object], field: str) -> int:
  value = d.get(field.rsplit(".", 1)[1])
  if not isinstance(value, int) or isinstance(value, bool):
    raise PeersError(f"{field}: must be an integer 1 to {PORT_MAX}")
  return value


def _port(value: int, field: str) -> int:
  if not 1 <= value <= PORT_MAX:
    raise PeersError(f"{field}: must be an integer 1 to {PORT_MAX}")
  return value


def _name(d: dict[str, object], field: str) -> str:
  value = _text(d, field)
  if not NAME_RE.match(value):
    raise PeersError(f"{field}: {value!r} must match ^[a-z0-9][a-z0-9-]*$")
  return value


def _key(d: dict[str, object], field: str) -> str:
  value = _text(d, field)
  try:
    raw = base64.b64decode(value, validate=True)
  except (binascii.Error, ValueError):
    raw = b""
  if len(value) != KEY_CHARS or len(raw) != KEY_BYTES:
    raise PeersError(f"{field}: must be a {KEY_CHARS}-character base64 string of {KEY_BYTES} bytes")
  return value


def _endpoint(value: str, field: str) -> str:
  host, sep, port = value.rpartition(":")
  if not sep or not host or any(c.isspace() for c in host) or not port.isdigit() or not 1 <= int(port) <= PORT_MAX:
    raise PeersError(f"{field}: {value!r} must be host:port with port 1 to {PORT_MAX}")
  return value


def _cidrs(d: dict[str, object], field: str) -> tuple[str, ...]:
  value = d.get(field.rsplit(".", 1)[1])
  if not isinstance(value, list) or not value or not all(isinstance(v, str) for v in value):
    raise PeersError(f"{field}: must be a non-empty list of CIDRs")
  for v in value:
    try:
      ipaddress.ip_network(v)
    except ValueError:
      raise PeersError(f"{field}: {v!r} is not a CIDR") from None
  return tuple(value)


def stamp(text: str, tag: str) -> str:
  """The bundle: `text` with `hub_version = "<tag>"` as the first top-level key (3.7), nothing else changed."""
  if not TAG_RE.match(tag):
    raise PeersError(f"tag {tag!r} is not a release tag like v1.2.3")
  return f'hub_version = "{tag}"\n\n{text}'


def render_conf(table: Table, peer: Peer) -> str:
  """A spoke's `<name>.conf` (3.7): the peer's overrides, else the hub's defaults."""
  hub = table.hub
  allowed = peer.allowed_ips if peer.allowed_ips is not None else hub.allowed_ips
  keepalive = peer.persistent_keepalive if peer.persistent_keepalive is not None else hub.persistent_keepalive
  return (
    "[Interface]\n"
    "PrivateKey = REPLACE_WITH_THIS_PEERS_PRIVATE_KEY\n"
    f"Address = {peer.address}\n"
    "\n"
    "[Peer]\n"
    f"PublicKey = {hub.public_key}\n"
    f"Endpoint = {peer.endpoint or hub.endpoint}\n"
    f"AllowedIPs = {', '.join(allowed)}\n"
    f"PersistentKeepalive = {keepalive}\n"
  )
```

Note on the `schema` line: `data.get("schema") is not 1` is deliberate (`1 == True` and `1 == 1.0` in Python; `is` against the small-int cache rejects both) but ruff flags it (F632); if the `noqa` does not silence it in this ruff version, use `type(data.get("schema")) is int and data["schema"] == 1` instead.

- [ ] **Step 6: Run the tests**

Run: `uv run pytest tests/test_peers.py tests/test_rules.py -q`
Expected: all pass. `test_the_shipped_table_is_valid` needs the owner's real hub public key in `peers.toml` (owner input 1); with a placeholder it fails on `hub.public_key`, which is the stop for that input.

- [ ] **Step 7: Lint, tick, commit**

```bash
cd "/d/SFT Software Projects/SFT Workspace/wireguard-hub"
uv run ruff format && uv run ruff check && uv run pyright
# tick Task 1 in the plan copy
git add pyproject.toml uv.lock src/wireguard_hub/peers.py src/wireguard_hub/peers.toml src/wireguard_hub/rules.v4 \
  tests/test_peers.py tests/test_rules.py docs/superpowers/plans/2026-09-15-wireguard-hub.md
git commit -m "feat(peers): the peer table, its validation, the stamped bundle and a peer's conf

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 2: the startup script `wireguard-hub-up`

**Files:**
- Modify: `src/wireguard_hub/up.py` (replace the stub)
- Create: `tests/test_up.py`

**Interfaces:**
- Consumes: `peers.load`, `peers.default_path`, `peers.rules_path` (Task 1).
- Produces: `up.main() -> None` (the console script), module constants `IP_FORWARD: Path`, `PEERS_PATH: Path`, `RULES_PATH: Path` the tests redirect.

- [ ] **Step 1: Write the failing test**

`tests/test_up.py`:

```python
# Standard library imports
import base64
import subprocess
from pathlib import Path

# Third party imports
import pytest

# First party imports
from wireguard_hub import up

HUB_KEY = base64.b64encode(bytes(range(32))).decode()
KEY_A = base64.b64encode(bytes([1] * 32)).decode()
TABLE = f"""
schema = 1
[hub]
name = "wireguard-hub"
public_key = "{HUB_KEY}"
address = "10.8.0.1/24"
listen_port = 51820
endpoint = "tunnels.example:51820"
allowed_ips = ["10.8.0.0/24"]
persistent_keepalive = 25
[[peers]]
name = "a"
public_key = "{KEY_A}"
address = "10.8.0.10/32"
"""
RULES = "*filter\n:FORWARD DROP [0:0]\nCOMMIT\n"


class Recorder:
  def __init__(self, fail_on: str | None = None) -> None:
    self.calls: list[tuple[list[str], str | None]] = []
    self.fail_on = fail_on

  def __call__(self, args: list[str], *, input: str | None = None, **kwargs: object) -> subprocess.CompletedProcess[str]:  # noqa: A002
    self.calls.append((list(args), input))
    if self.fail_on and args[0] == self.fail_on:
      return subprocess.CompletedProcess(args, 3, "", "boom")
    return subprocess.CompletedProcess(args, 0, "PUBKEY\n" if args == ["wg", "pubkey"] else "", "")


@pytest.fixture
def host(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Recorder:
  (tmp_path / "peers.toml").write_text(TABLE, encoding="utf-8")
  (tmp_path / "rules.v4").write_text(RULES, encoding="utf-8")
  (tmp_path / "ip_forward").write_text("1\n", encoding="utf-8")
  monkeypatch.setattr(up, "PEERS_PATH", tmp_path / "peers.toml")
  monkeypatch.setattr(up, "RULES_PATH", tmp_path / "rules.v4")
  monkeypatch.setattr(up, "IP_FORWARD", tmp_path / "ip_forward")
  monkeypatch.setenv("WG_HUB_PRIVATE_KEY", "PRIVATE")
  rec = Recorder()
  monkeypatch.setattr(up.subprocess, "run", rec)
  return rec


def test_the_eight_steps_run_in_order_with_the_key_on_stdin(host: Recorder, capsys: pytest.CaptureFixture[str]):
  up.main()
  assert host.calls == [
    (["ip", "link", "add", "dev", "wg0", "type", "wireguard"], None),
    (["wg", "set", "wg0", "listen-port", "51820", "private-key", "/dev/stdin"], "PRIVATE\n"),
    (["wg", "set", "wg0", "peer", KEY_A, "allowed-ips", "10.8.0.10/32"], None),
    (["ip", "address", "add", "10.8.0.1/24", "dev", "wg0"], None),
    (["ip", "link", "set", "up", "dev", "wg0"], None),
    (["iptables-restore"], RULES),
    (["wg", "pubkey"], "PRIVATE\n"),
  ]
  err = capsys.readouterr().err
  assert "PUBKEY" in err and "1 peer(s)" in err
  assert "PRIVATE" not in err


def test_failures_name_the_cause_never_the_key(host: Recorder, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]):
  monkeypatch.setenv("WG_HUB_PRIVATE_KEY", " ")
  with pytest.raises(SystemExit, match="1"):
    up.main()
  assert "WG_HUB_PRIVATE_KEY" in capsys.readouterr().err
  monkeypatch.setenv("WG_HUB_PRIVATE_KEY", "PRIVATE")
  up.IP_FORWARD.write_text("0\n", encoding="utf-8")
  with pytest.raises(SystemExit):
    up.main()
  assert "net.ipv4.ip_forward is 0: the compose file needs sysctls net.ipv4.ip_forward=1" in capsys.readouterr().err
  assert host.calls == [], "nothing runs before the checks pass"
  up.IP_FORWARD.write_text("1\n", encoding="utf-8")
  up.PEERS_PATH.write_text(TABLE.replace("listen_port = 51820", "listen_port = 0"), encoding="utf-8")
  with pytest.raises(SystemExit):
    up.main()
  assert "hub.listen_port" in capsys.readouterr().err
  up.PEERS_PATH.write_text(TABLE, encoding="utf-8")
  monkeypatch.setattr(up.subprocess, "run", Recorder(fail_on="iptables-restore"))
  with pytest.raises(SystemExit):
    up.main()
  err = capsys.readouterr().err
  assert "iptables-restore" in err and "exited 3" in err and "PRIVATE" not in err
```

- [ ] **Step 2: Run it to see it fail**

Run: `uv run pytest tests/test_up.py -q`
Expected: FAIL, `up` has no attribute `PEERS_PATH` (the stub only has `main`).

- [ ] **Step 3: Implement `up.py`**

```python
"""The startup script `wireguard-hub-up` (hub design 3.4).

Runs once as root before the app with the full environment (section 7): brings `wg0` up from the
peer table and applies the firewall rules, then exits. Every command is an argument list; the
private key only ever travels on stdin, so no command line and no error message can carry it.
"""

# Standard library imports
import os
import subprocess
import sys
from pathlib import Path

# First party imports
from wireguard_hub import peers

PEERS_PATH = peers.default_path()
RULES_PATH = peers.rules_path()
IP_FORWARD = Path("/proc/sys/net/ipv4/ip_forward")


def main() -> None:
  """Steps 1 to 8 of 3.4, in order; exit 1 on the first failure."""
  try:
    table = peers.load(PEERS_PATH)
  except (peers.PeersError, OSError) as e:
    _fail(f"{PEERS_PATH.name}: {e}")
  key = os.environ.get("WG_HUB_PRIVATE_KEY", "").strip()
  if not key:
    _fail("WG_HUB_PRIVATE_KEY is not set")
  if IP_FORWARD.read_text(encoding="utf-8").strip() != "1":
    _fail("net.ipv4.ip_forward is 0: the compose file needs sysctls net.ipv4.ip_forward=1")
  hub = table.hub
  _run(["ip", "link", "add", "dev", "wg0", "type", "wireguard"])
  _run(["wg", "set", "wg0", "listen-port", str(hub.listen_port), "private-key", "/dev/stdin"], stdin=key + "\n")
  for peer in table.peers:
    _run(["wg", "set", "wg0", "peer", peer.public_key, "allowed-ips", peer.address])
  _run(["ip", "address", "add", hub.address, "dev", "wg0"])
  _run(["ip", "link", "set", "up", "dev", "wg0"])
  _run(["iptables-restore"], stdin=RULES_PATH.read_text(encoding="utf-8"))
  public = _run(["wg", "pubkey"], stdin=key + "\n").strip()
  print(f"wireguard-hub-up: wg0 up, public key {public}, {len(table.peers)} peer(s)", file=sys.stderr)


def _run(args: list[str], *, stdin: str | None = None) -> str:
  """Run `args`; a nonzero exit names the command (never its stdin) and ends the script."""
  done = subprocess.run(args, input=stdin, capture_output=True, text=True, check=False)
  if done.returncode != 0:
    _fail(f"`{' '.join(args)}` exited {done.returncode}: {done.stderr.strip()}")
  return done.stdout


def _fail(message: str) -> None:
  print(f"wireguard-hub-up: {message}", file=sys.stderr)
  sys.exit(1)
```

`_fail` is 2 lines used 5 times and `_run` 5 lines used 7 times: both clear the repository's helper rule by reuse.

- [ ] **Step 4: Run the tests**

Run: `uv run pytest tests/test_up.py -q`
Expected: 2 passed.

- [ ] **Step 5: Lint, tick, commit**

```bash
uv run ruff format && uv run ruff check && uv run pyright
# tick Task 2 in the plan copy
git add src/wireguard_hub/up.py tests/test_up.py docs/superpowers/plans/2026-09-15-wireguard-hub.md
git commit -m "feat(up): wireguard-hub-up brings wg0 up from the peer table and applies rules.v4

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 3: the app `run-app-wireguard-hub`

**Files:**
- Modify: `src/wireguard_hub/__main__.py` (replace the stub)
- Create: `tests/test_app.py`

**Interfaces:**
- Produces: `run_app() -> None` (the console script); `serve(host: str, port: int) -> ThreadingHTTPServer` (started on a daemon thread, returned for the tests to stop); `beat() -> bool` (one heartbeat, `False` when the interface is absent); module constants `HEARTBEAT_FILE`, `INTERFACE`, `BEAT_SECS`, `PORT`.

- [ ] **Step 1: Write the failing tests**

`tests/test_app.py`:

```python
# Standard library imports
import http.client
from datetime import datetime
from importlib.metadata import version
from pathlib import Path

# Third party imports
import pytest

# First party imports
import wireguard_hub.__main__ as app


@pytest.fixture
def server():
  srv = app.serve("127.0.0.1", 0)
  yield srv
  srv.shutdown()
  srv.server_close()


def test_version_answers_the_tag_and_every_other_path_is_404(server):
  conn = http.client.HTTPConnection("127.0.0.1", server.server_address[1], timeout=5)
  conn.request("GET", "/version")
  r = conn.getresponse()
  assert r.status == 200
  assert r.getheader("Content-Type") == "text/plain; charset=utf-8"
  assert r.read() == f"v{version('wireguard-hub')}\n".encode()
  for path in ["/", "/version/", "/versions", "/health"]:
    conn.request("GET", path)
    r = conn.getresponse()
    assert r.status == 404, path
    r.read()


def test_the_heartbeat_is_written_only_while_the_interface_exists(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
  beat_file = tmp_path / "logs" / "heartbeat.txt"
  iface = tmp_path / "wg0"
  monkeypatch.setattr(app, "HEARTBEAT_FILE", beat_file)
  monkeypatch.setattr(app, "INTERFACE", iface)
  monkeypatch.delenv("PINGKEY", raising=False)
  monkeypatch.delenv("ALERTS_HEALTHCHECK_PING_URL", raising=False)
  assert app.beat() is False
  assert not beat_file.exists()
  iface.mkdir()
  assert app.beat() is True
  datetime.fromisoformat(beat_file.read_text(encoding="utf-8").strip())


def test_the_ping_key_and_slug_go_only_when_no_supervisor_owns_the_ping(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
  seen: list[dict[str, object]] = []
  monkeypatch.setattr(app, "send_heartbeat", lambda file, **kw: seen.append({"file": file, **kw}))
  monkeypatch.setattr(app, "HEARTBEAT_FILE", tmp_path / "heartbeat.txt")
  monkeypatch.setattr(app, "INTERFACE", tmp_path)
  monkeypatch.setenv("PINGKEY", "k")
  monkeypatch.setenv("HEARTBEAT_SLUG", "wireguard-hub")
  monkeypatch.delenv("DEVKIT_SUPERVISED_PING", raising=False)
  app.beat()
  assert seen[-1]["file"] == tmp_path / "heartbeat.txt"
  assert seen[-1]["pingkey"] is not None and seen[-1]["pingkey"].get_secret_value() == "k"
  assert seen[-1]["slug"] == "wireguard-hub"
  monkeypatch.setenv("DEVKIT_SUPERVISED_PING", "1")
  app.beat()
  assert seen[-1]["pingkey"] is None and seen[-1]["slug"] is None and seen[-1]["ping_url"] is None
```

- [ ] **Step 2: Run them to see them fail**

Run: `uv run pytest tests/test_app.py -q`
Expected: FAIL, `app` has no attribute `serve`.

- [ ] **Step 3: Implement `__main__.py`**

```python
"""The app `run-app-wireguard-hub` (hub design 3.5): the version endpoint and the heartbeat, nothing else.

Unprivileged. The heartbeat is `aeth_ext`'s one-shot call from this module's own loop because the
scheduled helpers cannot pause while the interface is gone, and the loop passes the ping key and
slug only when no supervisor owns the ping (`DEVKIT_SUPERVISED_PING` unset; always, with
`supervise = false`). An interface that vanished stops the beats, the standard healthcheck turns
unhealthy and healthchecks.io alerts.
"""

# Standard library imports
import os
import signal
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from importlib.metadata import version
from pathlib import Path

# Third party imports
from aeth_ext.monitoring.heartbeat import send_heartbeat
from pydantic import SecretStr

HOST = "0.0.0.0"  # noqa: S104 - the container's port, published by Coolify's Traefik route only
PORT = 8000
BEAT_SECS = 60
HEARTBEAT_FILE = Path("/app/persisted_data/logs/heartbeat.txt")
INTERFACE = Path("/sys/class/net/wg0")


class _Handler(BaseHTTPRequestHandler):
  """`GET /version` → `v<package version>\\n`; every other path 404 (4.1)."""

  def do_GET(self) -> None:  # noqa: N802 - the handler API's name
    if self.path != "/version":
      self.send_error(404)
      return
    body = f"v{version('wireguard-hub')}\n".encode()
    self.send_response(200)
    self.send_header("Content-Type", "text/plain; charset=utf-8")
    self.send_header("Content-Length", str(len(body)))
    self.end_headers()
    self.wfile.write(body)

  def log_message(self, format: str, *args: object) -> None:  # noqa: A002 - the handler API's name
    """Silence per-request lines: a 30 s route probe would fill the container log."""


def serve(host: str, port: int) -> ThreadingHTTPServer:
  """Start the HTTP server on a daemon thread and return it."""
  server = ThreadingHTTPServer((host, port), _Handler)
  threading.Thread(target=server.serve_forever, name="http", daemon=True).start()
  return server


def beat() -> bool:
  """One heartbeat, only while the interface exists; `False` when it does not."""
  if not INTERFACE.exists():
    return False
  supervised = bool(os.environ.get("DEVKIT_SUPERVISED_PING", "").strip())
  url = os.environ.get("ALERTS_HEALTHCHECK_PING_URL", "").strip()
  key = os.environ.get("PINGKEY", "").strip()
  send_heartbeat(
    HEARTBEAT_FILE,
    ping_url=SecretStr(url) if url and not supervised else None,
    pingkey=SecretStr(key) if key and not supervised else None,
    slug=os.environ.get("HEARTBEAT_SLUG") if not supervised else None,
  )
  return True


def run_app() -> None:
  """Serve until SIGINT or SIGTERM, beating every `BEAT_SECS`."""
  stop = threading.Event()
  signal.signal(signal.SIGINT, lambda *_: stop.set())
  signal.signal(signal.SIGTERM, lambda *_: stop.set())
  HEARTBEAT_FILE.parent.mkdir(parents=True, exist_ok=True)
  server = serve(HOST, PORT)
  print(f"run-app-wireguard-hub: serving /version on {HOST}:{PORT}", file=sys.stderr)
  while not stop.is_set():
    beat()
    stop.wait(BEAT_SECS)
  server.shutdown()
  server.server_close()


if __name__ == "__main__":
  run_app()
```

- [ ] **Step 4: Run the tests**

Run: `uv run pytest tests/test_app.py -q`
Expected: 3 passed.

- [ ] **Step 5: Lint, tick, commit**

```bash
uv run ruff format && uv run ruff check && uv run pyright
# tick Task 3 in the plan copy
git add src/wireguard_hub/__main__.py tests/test_app.py docs/superpowers/plans/2026-09-15-wireguard-hub.md
git commit -m "feat(app): run-app-wireguard-hub serves /version and beats while wg0 exists

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 4: the bundle module, the `peers` release job and `ci.yml`

**Files:**
- Create: `src/wireguard_hub/bundle.py`, `tests/test_bundle.py`, `.github/workflows/ci.yml`
- Modify: `.github/workflows/release.yml` (append the `peers` job under `jobs`)

**Interfaces:**
- Consumes: `peers.load`, `peers.stamp`, `peers.render_conf`, `peers.default_path` (Task 1).
- Produces: `bundle.main(argv: list[str]) -> int`; `python -m wireguard_hub.bundle <tag> <out-dir>` writes `<out-dir>/peers.toml` and `<out-dir>/<peer name>.conf` and exits 0, or prints the validation error and exits 1.

- [ ] **Step 1: Write the failing test**

`tests/test_bundle.py`:

```python
# Standard library imports
import base64
from pathlib import Path

# Third party imports
import pytest

# First party imports
from wireguard_hub import bundle, peers

HUB_KEY = base64.b64encode(bytes(range(32))).decode()
KEY_A = base64.b64encode(bytes([1] * 32)).decode()
TABLE = f"""schema = 1

[hub]
name = "wireguard-hub"
public_key = "{HUB_KEY}"
address = "10.8.0.1/24"
listen_port = 51820
endpoint = "tunnels.example:51820"
allowed_ips = ["10.8.0.0/24"]
persistent_keepalive = 25

[[peers]]
name = "office-db-pc"
public_key = "{KEY_A}"
address = "10.8.0.10/32"
"""


def test_the_bundle_is_the_stamped_table_and_one_conf_per_peer(tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]):
  src = tmp_path / "peers.toml"
  src.write_text(TABLE, encoding="utf-8")
  monkeypatch.setattr(bundle, "SOURCE", src)
  out = tmp_path / "out"
  assert bundle.main(["v1.2.3", str(out)]) == 0
  assert (out / "peers.toml").read_text(encoding="utf-8") == peers.stamp(TABLE, "v1.2.3")
  assert sorted(p.name for p in out.iterdir()) == ["office-db-pc.conf", "peers.toml"]
  assert (out / "office-db-pc.conf").read_text(encoding="utf-8").startswith("[Interface]\n")
  assert "office-db-pc.conf" in capsys.readouterr().err


def test_invalid_input_exits_1_naming_the_field(tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]):
  src = tmp_path / "peers.toml"
  src.write_text(TABLE.replace("listen_port = 51820", "listen_port = 0"), encoding="utf-8")
  monkeypatch.setattr(bundle, "SOURCE", src)
  assert bundle.main(["v1.2.3", str(tmp_path / "out")]) == 1
  assert "hub.listen_port" in capsys.readouterr().err
  src.write_text(TABLE, encoding="utf-8")
  assert bundle.main(["1.2.3", str(tmp_path / "out")]) == 1
  assert bundle.main(["v1.2.3"]) == 2
```

- [ ] **Step 2: Run it to see it fail**

Run: `uv run pytest tests/test_bundle.py -q`
Expected: collection error, `cannot import name 'bundle'`.

- [ ] **Step 3: Implement `bundle.py`**

```python
"""The release job's entry (hub design 3.7): `python -m wireguard_hub.bundle <tag> <out-dir>`.

Validates the shipped table, writes the stamped `peers.toml` and one `<peer name>.conf` per peer
into `<out-dir>` for the job to attach. A module rather than a third console script: 3.1 names two
programs. CI runs it with a placeholder tag on every push so a broken table is caught before a
release is attempted.
"""

# Standard library imports
import sys
from pathlib import Path

# First party imports
from wireguard_hub import peers

SOURCE = peers.default_path()


def main(argv: list[str]) -> int:
  """Write the bundle; 0 on success, 1 on a validation error, 2 on bad arguments."""
  if len(argv) != 2:
    print("usage: python -m wireguard_hub.bundle <tag> <out-dir>", file=sys.stderr)
    return 2
  tag, out = argv[0], Path(argv[1])
  try:
    text = SOURCE.read_text(encoding="utf-8")
    table = peers.parse(text)
    stamped = peers.stamp(text, tag)
  except (peers.PeersError, OSError) as e:
    print(f"bundle: {e}", file=sys.stderr)
    return 1
  out.mkdir(parents=True, exist_ok=True)
  (out / "peers.toml").write_text(stamped, encoding="utf-8")
  for peer in table.peers:
    (out / f"{peer.name}.conf").write_text(peers.render_conf(table, peer), encoding="utf-8")
  names = ", ".join(["peers.toml", *(f"{p.name}.conf" for p in table.peers)])
  print(f"bundle: {tag}, {len(table.peers)} peer(s): {names}", file=sys.stderr)
  return 0


if __name__ == "__main__":
  sys.exit(main(sys.argv[1:]))
```

- [ ] **Step 4: Run the tests**

Run: `uv run pytest tests/test_bundle.py -q`
Expected: 2 passed.

- [ ] **Step 5: Append the `peers` job to `release.yml`**

Append to the end of `.github/workflows/release.yml` (after the `publish` job's last step, one blank line between):

```yaml

  # The hub's own job (hub design 3.7), kept through every setup-project re-render by
  # [tool.devkit].release-workflow-jobs. Same guards as publish: the commit the tag pointed at
  # when the event fired, and the release that triggered this run must still own the tag.
  peers:
    name: "Peers: validate peers.toml, stamp it, render a conf per peer, attach them"
    runs-on: ubuntu-latest
    permissions:
      contents: write
    steps:
      - uses: actions/checkout@v4
        with:
          ref: ${{ github.sha }}
          persist-credentials: false

      - uses: astral-sh/setup-uv@v5
        with:
          python-version: "3.14"

      - name: The tag must name the committed version
        shell: bash
        run: |
          v="$(uv version --short)"
          [ "v$v" = "$TAG" ] || { echo "tag $TAG does not match pyproject.toml version $v" >&2; exit 1; }

      - name: uv sync
        env:
          UV_INDEX_SFTPYPI_USERNAME: ${{ secrets.UV_INDEX_SFTPYPI_USERNAME }}
          UV_INDEX_SFTPYPI_PASSWORD: ${{ secrets.UV_INDEX_SFTPYPI_PASSWORD }}
        run: uv sync

      - name: python -m wireguard_hub.bundle "$TAG" bundle
        run: uv run python -m wireguard_hub.bundle "$TAG" bundle && ls -l bundle

      - name: The release must still be the one that triggered this run
        env:
          GH_TOKEN: ${{ github.token }}
          RELEASE_ID: ${{ github.event.release.id }}
        run: |
          current="$(gh release view "$TAG" --json databaseId --jq .databaseId)"
          [ "$current" = "$RELEASE_ID" ] || { echo "release $TAG is now #$current, not #$RELEASE_ID which triggered this run; not attaching" >&2; exit 1; }

      - name: gh release upload the bundle
        env:
          GH_TOKEN: ${{ github.token }}
        run: gh release upload "$TAG" bundle/* --clobber
```

Then prove it survives a re-render:

```bash
cd "/d/SFT Software Projects/SFT Workspace/wireguard-hub"
set -a; . ./.env; set +a
uv run poe setup-project --dry-run 2>&1 | grep -A3 "release.yml"
```

Expected: `.github/workflows/release.yml` is either absent from "Changed:" or listed with `kept job peers` and no other detail; in both cases the file on disk still ends with the `peers` job (`grep -c "^  peers:" .github/workflows/release.yml` prints `1`).

- [ ] **Step 6: Write `ci.yml`**

`.github/workflows/ci.yml`:

```yaml
name: "Verify: tests, the peer table validates and renders"

on:
  push:
    branches: [main]
  pull_request:

concurrency:
  group: ci-${{ github.ref }}
  cancel-in-progress: true

permissions:
  contents: read

jobs:
  python:
    name: "Python: pytest, ruff, pyright, peers.toml renders into a bundle"
    runs-on: ubuntu-latest
    env:
      UV_INDEX_SFTPYPI_USERNAME: ${{ secrets.UV_INDEX_SFTPYPI_USERNAME }}
      UV_INDEX_SFTPYPI_PASSWORD: ${{ secrets.UV_INDEX_SFTPYPI_PASSWORD }}
    steps:
      - uses: actions/checkout@v4

      - uses: astral-sh/setup-uv@v5
        with:
          python-version: "3.14"

      - name: uv sync
        run: uv sync

      - name: uv run pytest
        run: uv run pytest

      - name: uv run ruff check, uv run ruff format --check
        run: uv run ruff check && uv run ruff format --check

      - name: uv run pyright
        run: uv run pyright

      # The same validation the release job runs (hub design 3.7), on every push, so a broken
      # table never reaches a release attempt.
      - name: python -m wireguard_hub.bundle v0.0.0 bundle
        run: uv run python -m wireguard_hub.bundle v0.0.0 bundle && ls -l bundle
```

- [ ] **Step 7: Lint, tick, commit**

```bash
uv run ruff format && uv run ruff check && uv run pyright
# tick Task 4 in the plan copy
git add src/wireguard_hub/bundle.py tests/test_bundle.py .github/workflows/release.yml .github/workflows/ci.yml \
  docs/superpowers/plans/2026-09-15-wireguard-hub.md
git commit -m "feat(release): the peers job validates, stamps and attaches the bundle; ci.yml validates every push

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 5: the hand-written Docker additions

**Files:**
- Modify: `docker/compose.yaml`, `docker/Dockerfile`

- [ ] **Step 1: Re-render for the `aeth-ext` environment block, then add the hub's lines**

Adding `aeth-ext` (Task 1) makes the compose template's `environment` block render (`HEARTBEAT_SLUG`, the `ALERTS_*` lines). Let `setup-project` write it, then add the hub's four lines by hand:

```bash
cd "/d/SFT Software Projects/SFT Workspace/wireguard-hub"
set -a; . ./.env; set +a
uv run poe setup-project --yes
grep -n "environment:\|HEARTBEAT_SLUG\|ALERTS_" docker/compose.yaml
```

Expected: an `environment:` block under `wireguard-hub:` with `- HEARTBEAT_SLUG=wireguard-hub` and the three `ALERTS_*` lines (setup-project commits that render itself).

Edit `docker/compose.yaml` so the service reads, with the rendered keys left as they are and these added (the private-key line as the last `environment` entry; the three mappings directly after `environment`'s block, before `networks:`):

```yaml
    environment:
      - HEARTBEAT_SLUG=wireguard-hub
      - ALERTS_EMAIL=info@sweetfiretobacco.com
      - ALERTS_EMAIL_PWD=${ALERTS_EMAIL_PWD:?}
      - ALERTS_RECIPIENTS=["jacob.ogden@sweetfiretobacco.com"]
      - WG_HUB_PRIVATE_KEY=${WG_HUB_PRIVATE_KEY:?}
    # The hub's own (hub design 3.6): setup-project never removes a key it does not annotate
    # and only appends to environment, so these survive every re-render.
    cap_add: [NET_ADMIN]
    sysctls: [net.ipv4.ip_forward=1]
    ports: ["51820:51820/udp"]
```

Edit `docker/Dockerfile`: inside the `final` window, between `# !window final:` and `# !end final`, insert:

```dockerfile
RUN apt-get update && apt-get install -y --no-install-recommends wireguard-tools iproute2 iptables && rm -rf /var/lib/apt/lists/*
```

- [ ] **Step 2: Prove both survive a re-render**

```bash
uv run poe setup-project --dry-run 2>&1 | grep -B1 -A4 "docker/"
```

Expected: neither `docker/compose.yaml` nor `docker/Dockerfile` appears under "Changed:" (or the Dockerfile appears only with `kept 1 line(s) in window final` and no diff). `docker compose -f docker/compose.yaml config --quiet` exits 0 if Docker is available locally.

- [ ] **Step 3: Build the image and run the startup script's checks in it (Docker available locally)**

```bash
cd "/d/SFT Software Projects/SFT Workspace/wireguard-hub"
set -a; . ./.env; set +a
docker build -f docker/Dockerfile --build-arg GIT_REPO=https://github.com/AetherBreaker/wireguard-hub.git --build-arg GIT_TAG="$(git rev-parse HEAD)" --secret id=uv_username,env=UV_INDEX_SFTPYPI_USERNAME --secret id=uv_password,env=UV_INDEX_SFTPYPI_PASSWORD -t wireguard-hub:dev . 2>&1 | tail -3
docker run --rm wireguard-hub:dev /app/.venv/bin/wireguard-hub-up; echo "exit=$?"
docker run --rm --cap-add NET_ADMIN --sysctl net.ipv4.ip_forward=1 -e WG_HUB_PRIVATE_KEY="$(docker run --rm wireguard-hub:dev wg genkey)" wireguard-hub:dev /app/.venv/bin/wireguard-hub-up; echo "exit=$?"
```

Expected: the first run exits 1 with `WG_HUB_PRIVATE_KEY is not set`; the second exits 0 and logs `wg0 up, public key …, <n> peer(s)`. (If the image's build args or secrets differ from the rendered Dockerfile's `ARG`s, take them from the file; the build args and secret ids are the Dockerfile's, not this plan's. If the build needs the release tag to exist on GitHub, the `GIT_TAG` build arg can name the pushed branch's commit as above.)

- [ ] **Step 4: Tick, commit**

```bash
# tick Task 5 in the plan copy
git add docker/compose.yaml docker/Dockerfile docs/superpowers/plans/2026-09-15-wireguard-hub.md
git commit -m "feat(docker): NET_ADMIN, forwarding, the UDP port, the private key and the wireguard tools

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 6: README and TODO entries

**Files:**
- Modify: `README.md`; create `TODO.md`

- [ ] **Step 1: Write the README**

Replace `README.md` with:

```markdown
# wireguard-hub

The WireGuard hub of the tunnel (design: `docs/superpowers/specs/2026-09-14-hub-fetched-peer-config-design.md`, section 3). One package, two programs:

- `wireguard-hub-up`: runs once as root before the app (the entrypoint's `startup_scripts`). Validates `src/wireguard_hub/peers.toml`, brings `wg0` up from it with the private key from `WG_HUB_PRIVATE_KEY` (stdin only; scrubbed from the app's environment by `scrub_env`), applies `src/wireguard_hub/rules.v4`, logs the hub's public key and the peer count, exits.
- `run-app-wireguard-hub`: unprivileged. `GET /version` on port 8000 answers `v<version>`; the heartbeat is written every 60 s while `/sys/class/net/wg0` exists, so a vanished interface turns the standard healthcheck unhealthy.

## The peer table

`peers.toml` is the single hand-edited source: the `[hub]` section and one `[[peers]]` row per enrolled spoke (name, public key, `/32` address; optional `endpoint`, `allowed_ips`, `persistent_keepalive` overrides). Addresses: subnet `10.8.0.0/24`, hub `10.8.0.1`, office database PC `10.8.0.10`, ScheduledReportAggregator `10.8.0.20`, `tunnel-probe` `10.8.0.21`, further peers from `.22`. Every rule of the table is checked by the startup script, by `python -m wireguard_hub.bundle`, and by the tests (`uv run pytest`); a failure names the field.

## Releases

`poe release` is the devkit release, unchanged. The `peers` job in `.github/workflows/release.yml` (kept through every `setup-project` run by `[tool.devkit].release-workflow-jobs`) validates the table, writes it with `hub_version = "<tag>"` stamped as the first key, renders `<peer name>.conf` for every peer, and attaches all of them to the GitHub release. Spokes fetch `peers.toml` from that release. `ci.yml` runs the same validation on every push.

## Enrolling a peer

Take the public key from the spoke's `not enrolled` log line, add a `[[peers]]` row with the next address, release the hub, redeploy the spoke. A running, enrolled spoke picks up a later change to its row within its version poll interval.

## Deploying

Coolify: the compose file publishes UDP 51820 directly; the domain `tunnels.sweetfiretobacco.com` is attached to the `wireguard-hub` service on container port 8000 in the Coolify UI. Environment: `WG_HUB_PRIVATE_KEY`, `PINGKEY`, `ALERTS_EMAIL_PWD`. The host needs the WireGuard kernel module and the netfilter modules `iptables-nft` uses; the first-deploy checklist is section 16 of the design.
```

- [ ] **Step 2: Record the TODO entries of spec 15 that are the hub's**

Create `TODO.md`:

```markdown
# TODO

- Generate `rules.v4` from a per-peer `allow` list in `peers.toml`, removing the duplicated addresses (hub design 15; the cross-check test in `tests/test_rules.py` is the guard until then).
- The per-flow `ACCEPT` lines and the office PC's firewall rule, once the database engine, port and protocol are decided (hub design 3.3, 11).
- Preshared keys in fetched mode need a per-peer secret on the hub side (hub design 15).
```

- [ ] **Step 3: Tick, commit**

```bash
# tick Task 6 in the plan copy
git add README.md TODO.md docs/superpowers/plans/2026-09-15-wireguard-hub.md
git commit -m "docs: the hub's README and its TODO entries

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 7: verification, PR, CI, merge

- [ ] **Step 1: The full local suite**

```bash
cd "/d/SFT Software Projects/SFT Workspace/wireguard-hub"
uv run pytest
uv run ruff check && uv run ruff format --check && uv run pyright
uv run python -m wireguard_hub.bundle v0.0.0 "$TMPDIR/hub-bundle" && ls "$TMPDIR/hub-bundle"
```

Expected: every command exits 0; the bundle folder holds `peers.toml` and one conf per row.

- [ ] **Step 2: Push, open the PR, watch CI**

```bash
git push -u origin feat/hub
gh pr create --base main --title "feat: the WireGuard hub" --body-file - <<'EOF'
## Summary

Release-order step 3 of the WireGuard hub design (`docs/superpowers/specs/2026-09-14-hub-fetched-peer-config-design.md`, section 3): the peer table and its validation, `rules.v4`, `wireguard-hub-up`, `run-app-wireguard-hub`, the `peers` release job with the bundle module, `ci.yml`, and the hand-written compose and Dockerfile lines that the devkit features of step 1 keep through re-renders.

## Test plan

- [ ] `uv run pytest`, `ruff`, `pyright` locally
- [ ] CI green: the tests and the bundle render

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
gh pr checks --watch
```

Expected: the one CI job green.

- [ ] **Step 3: Merge (owner's call), tick, commit**

The owner merges on GitHub or says to. Then:

```bash
git checkout main && git pull --ff-only
# tick Task 7 in the plan copy
git add docs/superpowers/plans/2026-09-15-wireguard-hub.md
git commit -m "docs(plans): tick task 7

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
git push
```

---

### Task 8: the first release, the pin, the deployment

- [ ] **Step 1: The owner's peer rows (owner input 2)**

If the owner supplies rows, add them to `src/wireguard_hub/peers.toml` on `main` in the 3.2 shape, run `uv run pytest`, commit as `feat(peers): enrol <names>` and push. Otherwise the first release ships the hub section alone.

- [ ] **Step 2: Release 1.0.0 (owner's go-ahead first)**

```bash
cd "/d/SFT Software Projects/SFT Workspace/wireguard-hub"
set -a; . ./.env; set +a
uv run devkit release --dry-run major "The WireGuard hub: peer table, startup script, version endpoint, the peers bundle"
```

Show the owner the plan and ask; then:

```bash
uv run devkit release major "The WireGuard hub: peer table, startup script, version endpoint, the peers bundle"
gh release view v1.0.0 --json assets -q '.assets[].name'
```

Expected: exit 0; the assets list holds the wheel, the sdist, `peers.toml` and one `.conf` per row. Then confirm the bundle is the stamped table:

```bash
gh release download v1.0.0 --pattern peers.toml --dir "$TMPDIR/v1" --clobber
head -1 "$TMPDIR/v1/peers.toml"      # hub_version = "v1.0.0"
```

- [ ] **Step 3: Pin the compose file to the release**

```bash
uv run poe docker-pin
git log --oneline -1
grep -n "GIT_TAG\|PACKAGE_VERSION" docker/compose.yaml
```

Expected: `GIT_TAG: v1.0.0` committed and pushed.

- [ ] **Step 4: Tick, commit**

```bash
# tick Task 8 steps 1 to 3 in the plan copy
git add docs/superpowers/plans/2026-09-15-wireguard-hub.md
git commit -m "docs(plans): tick task 8, released 1.0.0

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
git push
```

- [ ] **Step 5: Deploy in Coolify and run section 16 steps 1 to 4 (the owner)**

The owner: creates the Coolify service from `docker/compose.yaml`, sets `WG_HUB_PRIVATE_KEY`, `PINGKEY`, `ALERTS_EMAIL_PWD`, attaches `tunnels.sweetfiretobacco.com` to the service on port 8000, deploys, then in order:

1. Neither the office LAN nor the VPS uses `10.8.0.0/24` (`ip route` on both).
2. `modprobe wireguard` succeeds on the VPS host; `lsmod | grep -E "nf_tables|xt_conntrack"` lists both.
3. `docker inspect wireguard-hub` shows `NET_ADMIN` under `CapAdd`, `net.ipv4.ip_forward=1` under `Sysctls`, `51820/udp` under `PortBindings`; `curl https://tunnels.sweetfiretobacco.com/version` prints `v1.0.0`; the heartbeat file under the mount is fresh and the healthchecks.io check is up.
4. The hub and the office PC handshake (`wg show` on the office PC shows a recent handshake) once the office PC's row is enrolled.

The executor records the owner's results as a note under this step; a failing check is a stop.

---

### Task 9: sync the plan copies and report

- [ ] **Step 1: Mirror the fully ticked plan to the other three repositories**

```bash
ws="/d/SFT Software Projects/SFT Workspace"
plan=docs/superpowers/plans/2026-09-15-wireguard-hub.md
for r in aeth_devkit devkit-templates devkit-container; do
  cd "$ws/$r" && git checkout main && git pull --ff-only
  cp "$ws/wireguard-hub/$plan" "$plan"
  git add "$plan"
  git commit -m "docs(plans): the wireguard-hub plan fully ticked, synced across the repositories

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
  git push
done
```

- [ ] **Step 2: Report to the owner**

In this order: the release and its assets; the deployment checks' results as the owner reported them; what is enrolled; the spec's next release-order step (14 step 4: the spokes re-rendered and migrated per 10.1, `tunnel-probe` created the same way as the hub, the office PC per 10.2), which gets its own plan; and that the spec and plan copies stay in every repository until the whole multi-stage change has landed (owner's instruction, 2026-09-15).

---

## Self-review

**Spec coverage.** 3.1: two console scripts, package data in `src/` (Tasks 1 to 3). 3.2: the format (the shipped file), every rule with a failing case each and "any failure names the field" (Task 1's parametrised test), the stamp as the only difference (Task 1, `stamp`; Task 4 writes it). 3.3: the policy-only `rules.v4`, `INPUT` untouched, the cross-check test (Task 1). 3.4: the eight steps, the exact commands, the key on stdin, "naming the command, never the key", argument lists (Task 2). 3.5: the endpoint's status, content type and body, 404 elsewhere, the heartbeat every 60 s gated on the interface and on `DEVKIT_SUPERVISED_PING`, SIGINT/SIGTERM (Task 3). 3.6: the tables (already in `pyproject.toml`), the four compose lines and the window line, surviving re-renders (Task 5), Coolify (Task 8 step 5). 3.7: the `peers` job's guards, validation, stamping, confs in the given shape, the attach, `ci.yml` on every push (Task 4). 3.8: enrolment as the README's procedure and Task 8 step 1. 11: names, addresses, port, endpoint (the shipped table, README). 13 `wireguard-hub`: validation one test per rule, the `rules.v4` cross-check, the startup script with mocked `subprocess` asserting exact command lines and the key on stdin, the `/version` response, the heartbeat gating on the interface path and on `DEVKIT_SUPERVISED_PING` (Tasks 1 to 3). 14 step 3: created (done before this plan), `setup-project` against the step 1 and 2 releases (done: 15.1.0, 1.3.0, 2.1.0 are locked), the job, the compose additions and the window content, the first release, the deployment (Tasks 4, 5, 8). 15: the hub's TODO entries (Task 6). 16: the host requirements in the README and steps 1 to 4 as the owner's checklist (Task 8).

**Placeholders.** `<HUB PUBLIC KEY>` in Task 1's `peers.toml` is an owner input named as such, not a plan gap; the plan states the stop it causes. Every other step carries its content and expected result.

**Type consistency.** `peers.Table{hub: Hub, peers: tuple[Peer, ...], hub_version: str | None}`, `Peer.allowed_ips: tuple[str, ...] | None`, `Hub.allowed_ips: tuple[str, ...]` are used that way in `render_conf`, `up.py` (`table.hub.listen_port`, `peer.public_key`, `peer.address`) and `bundle.py`; `peers.stamp(text, tag) -> str` and `peers.render_conf(table, peer) -> str` are called with those argument orders in Task 4 and its tests; `up.PEERS_PATH`, `up.RULES_PATH`, `up.IP_FORWARD`, `app.HEARTBEAT_FILE`, `app.INTERFACE`, `bundle.SOURCE` are the module constants the tests monkeypatch and the modules define.
