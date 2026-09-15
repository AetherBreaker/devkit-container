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
