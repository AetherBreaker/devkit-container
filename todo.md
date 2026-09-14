# todo

- Hook the binary's log lines into aeth_ext's logging. Today `run` writes plain lines to stderr
  with no levels, so the container log is the only sink and nothing reaches the central log
  server. The likely shape is a Rust library in aeth_ext that connects to the central log server
  under the app's own identity and sends simple levelled messages following the server's
  protocol, as the Python client does; the binary then logs through it beside the app. Decided
  2026-09-14 as later work, outside the hub design.
