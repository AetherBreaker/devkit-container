# devkit-container wireguard mode

Date: 2026-09-08. Status: design approved in discussion except the decisions in section 10, which
must be answered before the implementation plan is written. Written in `aeth-devkit` and moved
here with the crate on 2026-09-09.

Prerequisite: step 1 of `aeth-devkit/docs/superpowers/specs/2026-09-08-devkit-split-design.md`
has shipped, so `devkit-container` is its own repo, distributed as a wheel, with the Dockerfile
template as package data and `setup-project` reading it from the venv. Consuming project's view:
`ScheduledReportAggregator/docs/superpowers/specs/2026-09-08-wireguard-db-access-design.md`,
which decided that the tunnel lives inside the app container. This spec is the container side of
that decision.

## 1. Boundary

What changes, and where:

- `devkit-container`: the `run` entrypoint gains the mode (section 3), the status file
  (section 4), the environment contract (section 5), and the `if-wireguard` block in its
  Dockerfile template. Two new tests (section 8).
- `setup` in `aeth-devkit`: a `wireguard` context flag read from `[tool.docker]`, the
  `if-wireguard` gate enabled from it, and three compose rules (section 6).
- The compose template, wherever it lives at the time: the `if-wireguard` block (section 6).

Nothing else. The hub, the office PC and the test project are the companion spec's.

## 2. Switch

`[tool.docker].wireguard = true` in `pyproject.toml`. Read by the container binary at `run` (the
image carries the project's `pyproject.toml`) and by `setup-project` for template gating. Off is
today's behaviour exactly: `exec`, the app is PID 1. The smoke test's `pid_1` assertion applies to
off mode only.

## 3. Entrypoint, mode on

Root phase, in this order:

1. Read the `WG_*` environment. Render `wg0.conf` to a root-only tmpfs path, mode `0600`.
   Remove `WG_PRIVATE_KEY` and `WG_PEER_PRESHARED_KEY` from the environment that will be handed to
   the app. A failure here names the variable, never the value.
2. Bring up `wg0`: interface, key, address, peer, routes for the peer's allowed IPs. Whether via
   `wg-quick` or `ip` + `wg setconf` is the plan's call; both need `CAP_NET_ADMIN`. Kernel
   WireGuard needs no `/dev/net/tun`; only a userspace implementation does.
3. Wait for the first handshake, up to `WG_HANDSHAKE_TIMEOUT_SECS` (default 60). No handshake is
   a refused start, with the endpoint named in the message.
4. The existing steps: mount check, `prepare` (mkdir and chown).
5. Spawn, not exec, `/app/.venv/bin/<run-app-*>` as 999:999 with empty supplementary groups (a
   `pre_exec` doing what `run.rs` does today). The entrypoint stays PID 1 as root because re-upping
   the tunnel needs `NET_ADMIN`. The child's capability sets are empty; the smoke test asserts
   `CapEff`, `CapPrm` and `CapAmb` are zero in the child's `/proc/self/status`.
6. Supervise: forward `SIGTERM`, `SIGINT` and `SIGHUP` to the child; reap zombies; every
   `WG_POLL_SECS` (default 30) read the latest handshake and, if older than `WG_STALE_SECS`
   (default 180), re-up the tunnel and log it; write the status file after every poll.
7. On child exit: bring `wg0` down, exit with the child's code (signal death as 128+n).

## 4. Status file

`/run/devkit/wireguard.json`, world-readable, rewritten atomically, fields: `state`
(`up` | `stale` | `reupping` | `down`), `latest_handshake_epoch`, `endpoint`, `rx_bytes`,
`tx_bytes`, `reups`, `updated_at`. This is the only tunnel information the app can see; reading it
needs no capabilities. `aeth_ext` reading it and alerting on `stale` is follow-on work in that
project. The path and fields are a contract owned here and consumed there.

## 5. Environment contract

Names are a proposal; the plan fixes them and the `[tool.docker]` schema doc records them.

| Variable | Meaning | Required |
|---|---|---|
| `WG_PRIVATE_KEY` | this peer's private key; secret | yes |
| `WG_ADDRESS` | this peer's tunnel address, CIDR (`10.8.0.20/32`) | yes |
| `WG_PEER_PUBLIC_KEY` | the hub's public key | yes |
| `WG_PEER_ENDPOINT` | `host:port` of the hub | yes |
| `WG_PEER_ALLOWED_IPS` | comma-separated CIDRs routed through the hub | yes |
| `WG_PEER_PRESHARED_KEY` | secret | no |
| `WG_PERSISTENT_KEEPALIVE` | seconds; default 25 | no |
| `WG_HANDSHAKE_TIMEOUT_SECS`, `WG_POLL_SECS`, `WG_STALE_SECS` | defaults 60, 30, 180 | no |

## 6. Rendering

Both templates gain an `if-wireguard` block using the existing `# setup-project: if-<name>` gate,
enabled from the new context flag.

**Compose.** The block on the app service adds `cap_add: [NET_ADMIN]`,
`devices: [/dev/net/tun:/dev/net/tun]`, `sysctls: net.ipv4.conf.all.src_valid_mark=1`, and the
`WG_*` environment lines, so the compose file is identical across projects and all values live in
the deploy environment (Coolify): required values as `${NAME:?}`, optional ones as `${NAME:-}`
with empty read as unset. The `environment:` key appears once in the template, with the
`if-aeth-ext` and `if-wireguard` lists as separate gated blocks nested under it, because `gate`
does not nest and two gated blocks each carrying `environment:` would duplicate the key when both
are on. `/dev/net/tun` and the sysctl are not needed by kernel WireGuard with a fixed tunnel
subnet; they are kept because the companion spec's hub image and a future userspace fallback need
them, and they are harmless where the device exists.

**The compose rule engine needs three additions,** not none: `cap_add`, `devices` and `sysctls`
as `Presence` rules in the `RULES` table, so an existing compose file gains them. Rules whose path
the scaffold lacks are skipped, so projects with the mode off are untouched. `EnvKeys` already
inserts the `WG_*` lines.

**Dockerfile.** The block installs `wireguard-tools` and `iproute2` in the final stage. It lives
in the template inside the `devkit-container` wheel, so the binary and the packages it needs
version together.

## 7. Host requirements

A Docker host kernel with the wireguard module (Linux 5.6 or later), and a deploy platform that
passes `cap_add`, `devices` and `sysctls` through. Both are verified on the first deploy, not
assumed (the companion spec's section 8 is the checklist). The CI runner for the smoke test needs
the module loaded too (`sudo modprobe wireguard` in the workflow).

## 8. Tests

Unit: config rendering, environment scrubbing, status serialisation, stale-handshake logic with an
injected clock.

Smoke, in CI beside the existing one: a hub container with generated keys and an image built from
the template with the mode on, on one Docker network. Asserts: handshake within the timeout;
child uid 999 with empty capability sets; `WG_PRIVATE_KEY` absent from the child's environment;
status file present, readable by 999, `state: up`; `SIGTERM` reaches the child and its exit code
passes through; removing the peer on the hub and restoring it produces `stale` then `up` with
`reups` incremented. The poll and stale intervals are set short through the environment for the
test.

## 9. Done means

- The existing smoke test is unchanged and green with the mode off.
- The new smoke test is green in the container repo's CI.
- `ScheduledReportAggregator` builds and starts in both modes; with the mode on, the companion
  spec's first-deploy checks pass.
- A project with the mode off renders no wireguard line anywhere; turning it on changes only the
  gated blocks and the three compose keys.

## 10. Open decisions

Each must be answered before the plan is written. Recommendations are noted.

- **Private key handling.** Section 3 renders `wg0.conf` to tmpfs. The alternative is to pass the
  key to `wg set wg0 private-key /dev/stdin` (and the preshared key the same way) so it never
  touches a filesystem. Recommended: stdin.
- **What "re-up" means.** Re-set the peer endpoint so its DNS is re-resolved, escalating to a
  full down-and-up only if the next poll is still stale; or always down-and-up. The companion spec
  allows the hub to move hosts, which the first covers directly. Recommended: the first.
- **Keepalive.** Stale detection depends on it: with `WG_PERSISTENT_KEEPALIVE=0` and an idle app,
  WireGuard does not re-handshake, and every poll after `WG_STALE_SECS` reads as stale. Either the
  mode requires a nonzero keepalive (refused start otherwise), or the stale logic is suspended when
  keepalive is zero. Recommended: require it.
- **Healthcheck start period.** The compose block can raise `healthcheck.start_period` above
  `WG_HANDSHAKE_TIMEOUT_SECS`, or a slow first handshake can mark the container unhealthy before
  the app has written its first heartbeat. Recommended: raise it in the gated block.
- **Public key at startup.** Log the public key derived from `WG_PRIVATE_KEY` on every start. Not
  a secret, and it is what the hub operator needs to enrol the peer. Recommended: yes.
- **The switch and uncommitted edits.** `setup-project` refuses to commit when
  `[tool.docker].services` differs between `HEAD` and the working copy, because the switch is read
  from the working copy while the merge runs against `HEAD`. `wireguard` gates rendering the same
  way. Recommended: the same refusal.
- **Optional variables.** Section 6 renders them as `${NAME:-}`. The alternative is to omit them
  from the compose block and let the binary's defaults apply, which keeps the file shorter but
  hides which knobs exist. Recommended: keep them, as written.
- **`/dev/net/tun` and the sysctl.** Kept in section 6 for the reasons given there; drop them if
  the userspace fallback is ruled out for good.
