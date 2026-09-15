//! The supervisor's consent client (spec 6.2): one request per connection to the app's Unix
//! socket, one reply line back. Everything that is not a literal `hold` within the timeout is
//! consent: a missing socket, a refused connection, an error, an empty stream, any other line.
#![allow(dead_code)] // until the supervisor uses it (task 11)

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use crate::health::Reply;

pub const DIR: &str = "/run/devkit";
pub const SOCKET: &str = "/run/devkit/consent.sock";
/// No reply within this long is consent (6.2).
pub const TIMEOUT: Duration = Duration::from_secs(60);

/// `may-shutdown <reason>`; `Hold` only for a literal `hold` line back within `timeout`.
pub fn ask(path: &Path, reason: &str, timeout: Duration) -> Reply {
  match reply_line(path, reason, timeout).as_deref() {
    Some("hold") => Reply::Hold,
    _ => Reply::Ok,
  }
}

fn reply_line(path: &Path, reason: &str, timeout: Duration) -> Option<String> {
  let mut stream = UnixStream::connect(path).ok()?;
  stream.set_read_timeout(Some(timeout)).ok()?;
  stream.set_write_timeout(Some(timeout)).ok()?;
  stream.write_all(format!("may-shutdown {reason}\n").as_bytes()).ok()?;
  // Bounded: a reply longer than this is not a protocol reply.
  let mut reader = BufReader::new(stream).take(256);
  let mut line = String::new();
  reader.read_line(&mut line).ok()?;
  let line = line.trim_end_matches(['\r', '\n']).to_string();
  (!line.is_empty()).then_some(line)
}

#[cfg(test)]
mod tests {
  use std::os::unix::net::UnixListener;
  use std::sync::mpsc;
  use std::time::Duration;

  use super::*;

  enum Server {
    Reply(&'static str),
    CloseAtOnce,
    Silent(Duration),
  }

  /// One connection served on a fresh socket; the request line is sent back through the channel.
  fn serve(dir: &std::path::Path, behaviour: Server) -> (std::path::PathBuf, mpsc::Receiver<String>) {
    let path = dir.join("consent.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
      use std::io::{BufRead as _, BufReader, Write as _};
      let (stream, _) = listener.accept().unwrap();
      let mut reader = BufReader::new(&stream);
      let mut line = String::new();
      reader.read_line(&mut line).unwrap();
      tx.send(line.trim_end().to_string()).unwrap();
      match behaviour {
        Server::Reply(text) => {
          (&stream).write_all(text.as_bytes()).unwrap();
        }
        Server::CloseAtOnce => {}
        Server::Silent(d) => std::thread::sleep(d),
      }
    });
    (path, rx)
  }

  #[test]
  fn only_a_literal_hold_holds() {
    let dir = tempfile::tempdir().unwrap();
    for (behaviour, want) in [
      (Server::Reply("hold\n"), Reply::Hold),
      (Server::Reply("ok\n"), Reply::Ok),
      (Server::Reply("HOLD\n"), Reply::Ok),
      (Server::Reply("hold please\n"), Reply::Ok),
      (Server::Reply("garbage"), Reply::Ok),
      (Server::CloseAtOnce, Reply::Ok),
      (Server::Silent(Duration::from_millis(600)), Reply::Ok),
    ] {
      let sub = tempfile::tempdir_in(dir.path()).unwrap();
      let (path, rx) = serve(sub.path(), behaviour);
      let got = ask(&path, "wireguard-disconnected 1800s", Duration::from_millis(200));
      assert_eq!(got, want);
      assert_eq!(rx.recv().unwrap(), "may-shutdown wireguard-disconnected 1800s");
    }
  }

  #[test]
  fn no_socket_and_a_refused_connection_are_consent() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(ask(&dir.path().join("absent.sock"), "x", Duration::from_millis(200)), Reply::Ok);
    let path = dir.path().join("dead.sock");
    drop(UnixListener::bind(&path).unwrap());
    assert!(path.exists(), "the socket file stays behind, with nobody listening");
    assert_eq!(ask(&path, "x", Duration::from_millis(200)), Reply::Ok);
  }
}
