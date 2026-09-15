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
