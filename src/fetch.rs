//! Linux only: the version endpoint (spec 4.1) and the bundle fetch from the hub's GitHub
//! release (4.2) over `ureq`, and `obtain`, the version → bundle → entry sequence the boot and
//! the worker thread run. Errors name the step and the HTTP status, never the token or a body.
//! The two GitHub hosts are ordinary inputs, so the tests point them at a local listener.
#![allow(dead_code)] // until run and the supervisor use it (task 11)

use anyhow::{Result, anyhow, bail};

use crate::bundle::{self, Effective};

const API_VERSION: &str = "2022-11-28";
/// The cap on the release listing and on the bundle body (4.2).
const LIMIT: u64 = 1024 * 1024;
const USER_AGENT: &str = concat!("devkit-container/", env!("CARGO_PKG_VERSION"));

/// The two GitHub hosts. Ordinary inputs, so a test can point them at a local listener; there
/// is no environment variable for them.
#[derive(Debug, Clone)]
pub struct Hosts {
  pub api: String,
  pub web: String,
}

impl Default for Hosts {
  fn default() -> Hosts {
    Hosts {
      api: "https://api.github.com".into(),
      web: "https://github.com".into(),
    }
  }
}

#[derive(Clone)]
pub struct Fetcher {
  agent: ureq::Agent,
  hub_url: String,
  repo: String,
  token: Option<String>,
  hosts: Hosts,
}

impl Fetcher {
  pub fn new(hub_url: String, repo: String, token: Option<String>, hosts: Hosts) -> Fetcher {
    Fetcher {
      agent: crate::ping::agent(),
      hub_url,
      repo,
      token,
      hosts,
    }
  }

  /// `GET <hub>/version` (4.1): a strict tag, else an error carrying the status or the body's
  /// first 64 bytes.
  pub fn version(&self) -> Result<String> {
    let url = format!("{}/version", self.hub_url);
    let mut resp = self
      .agent
      .get(&url)
      .header("User-Agent", USER_AGENT)
      .call()
      .map_err(|e| anyhow!("version endpoint: {}", describe(&e)))?;
    let body = resp
      .body_mut()
      .with_config()
      .limit(1024)
      .read_to_string()
      .map_err(|e| anyhow!("version endpoint: reading the body: {}", describe(&e)))?;
    let text = body.strip_suffix('\n').unwrap_or(&body);
    bundle::validate_tag(text).map_err(|_| anyhow!("version endpoint: the body is not a tag: {:?}", truncate(text, 64)))
  }

  /// The bundle's text at `tag` (4.2), capped at 1 MiB.
  pub fn bundle(&self, tag: &str) -> Result<String> {
    let Some(token) = &self.token else {
      return self.public(tag);
    };
    let bearer = format!("Bearer {token}");
    let listing_url = format!("{}/repos/{}/releases/tags/{tag}", self.hosts.api, self.repo);
    let mut resp = self
      .agent
      .get(&listing_url)
      .header("Authorization", &bearer)
      .header("Accept", "application/vnd.github+json")
      .header("X-GitHub-Api-Version", API_VERSION)
      .header("User-Agent", USER_AGENT)
      .call()
      .map_err(|e| anyhow!("release listing for {tag}: {}", describe(&e)))?;
    let listing = resp
      .body_mut()
      .with_config()
      .limit(LIMIT)
      .read_to_string()
      .map_err(|e| anyhow!("release listing for {tag}: reading the body: {}", describe(&e)))?;
    let json: serde_json::Value =
      serde_json::from_str(&listing).map_err(|_| anyhow!("release listing for {tag}: the body is not JSON"))?;
    let asset_url = json
      .get("assets")
      .and_then(|a| a.as_array())
      .and_then(|assets| assets.iter().find(|a| a.get("name").and_then(|n| n.as_str()) == Some("peers.toml")))
      .and_then(|a| a.get("url"))
      .and_then(|u| u.as_str())
      .ok_or_else(|| anyhow!("release {tag} has no peers.toml asset"))?
      .to_string();
    // Redirects off: the 302 comes back as a response, and its Location is fetched without the
    // token (the client would strip it on a followed redirect too; this keeps it testable).
    let resp = self
      .agent
      .get(&asset_url)
      .header("Authorization", &bearer)
      .header("Accept", "application/octet-stream")
      .header("X-GitHub-Api-Version", API_VERSION)
      .header("User-Agent", USER_AGENT)
      .config()
      .max_redirects(0)
      .build()
      .call()
      .map_err(|e| anyhow!("asset request for {tag}: {}", describe(&e)))?;
    if resp.status() != 302 {
      bail!("asset request for {tag}: expected 302, got {}", resp.status());
    }
    let location = resp
      .headers()
      .get("location")
      .and_then(|v| v.to_str().ok())
      .ok_or_else(|| anyhow!("asset request for {tag}: 302 without a Location"))?
      .to_string();
    let mut resp = self
      .agent
      .get(&location)
      .header("Accept", "application/octet-stream")
      .header("User-Agent", USER_AGENT)
      .call()
      .map_err(|e| anyhow!("asset download for {tag}: {}", describe(&e)))?;
    resp
      .body_mut()
      .with_config()
      .limit(LIMIT)
      .read_to_string()
      .map_err(|e| anyhow!("asset download for {tag}: reading the body: {}", describe(&e)))
  }

  /// The public-repo path: supported, not the decided configuration (4.2).
  fn public(&self, tag: &str) -> Result<String> {
    let url = format!("{}/{}/releases/download/{tag}/peers.toml", self.hosts.web, self.repo);
    let mut resp = self
      .agent
      .get(&url)
      .header("User-Agent", USER_AGENT)
      .call()
      .map_err(|e| anyhow!("public download for {tag}: {}", describe(&e)))?;
    resp
      .body_mut()
      .with_config()
      .limit(LIMIT)
      .read_to_string()
      .map_err(|e| anyhow!("public download for {tag}: reading the body: {}", describe(&e)))
  }
}

/// ureq's error text names hosts, statuses and timeouts, never a URL or a header value.
fn describe(e: &ureq::Error) -> String {
  match e {
    ureq::Error::StatusCode(code) => format!("HTTP {code}"),
    other => other.to_string(),
  }
}

fn truncate(s: &str, n: usize) -> &str {
  match s.char_indices().nth(n) {
    Some((i, _)) => &s[..i],
    None => s,
  }
}

/// The result of one version → bundle → entry attempt (4.1 to 4.3).
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
  /// The hub reports the tag that is already applied.
  Unchanged(String),
  /// A valid bundle at another tag, with this spoke's entry.
  New { tag: String, text: String, effective: Effective },
  /// A valid bundle at another tag, without this spoke's entry.
  NotEnrolled { tag: String, text: String },
  /// The version endpoint, the fetch or the validation failed; the message names the step.
  Unavailable(String),
}

pub fn obtain(f: &Fetcher, public_key: &str, applied_tag: Option<&str>) -> Outcome {
  let tag = match f.version() {
    Ok(t) => t,
    Err(e) => return Outcome::Unavailable(format!("{e:#}")),
  };
  if applied_tag == Some(tag.as_str()) {
    return Outcome::Unchanged(tag);
  }
  let text = match f.bundle(&tag) {
    Ok(t) => t,
    Err(e) => return Outcome::Unavailable(format!("{e:#}")),
  };
  let parsed = match bundle::parse(&text).and_then(|b| bundle::require_version(&b, &tag).map(|()| b)) {
    Ok(b) => b,
    Err(e) => return Outcome::Unavailable(format!("bundle {tag}: {e:#}")),
  };
  match bundle::select(&parsed, public_key) {
    Some(effective) => Outcome::New { tag, text, effective },
    None => Outcome::NotEnrolled { tag, text },
  }
}

#[cfg(test)]
mod tests {
  use std::io::{Read as _, Write as _};
  use std::net::TcpListener;
  use std::sync::{Arc, Mutex};

  use super::*;

  #[derive(Debug, Clone)]
  struct Route {
    path: String,
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
  }

  fn route(path: &str, status: u16, headers: &[(&str, &str)], body: &str) -> Route {
    Route {
      path: path.into(),
      status,
      headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
      body: body.into(),
    }
  }

  type Request = (String, Vec<(String, String)>);

  /// A one-thread HTTP/1.1 listener on localhost answering canned routes and recording every
  /// request's path and headers. `{base}` in a header value becomes the listener's base URL.
  fn serve(routes: Vec<Route>) -> (String, Arc<Mutex<Vec<Request>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen: Arc<Mutex<Vec<Request>>> = Arc::default();
    let record = Arc::clone(&seen);
    let routes: Vec<Route> = routes
      .into_iter()
      .map(|mut r| {
        for (_, v) in &mut r.headers {
          *v = v.replace("{base}", &base);
        }
        r.body = r.body.replace("{base}", &base);
        r
      })
      .collect();
    std::thread::spawn(move || {
      for stream in listener.incoming() {
        let Ok(mut stream) = stream else { break };
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        while !buf.ends_with(b"\r\n\r\n") && stream.read(&mut byte).is_ok_and(|n| n == 1) {
          buf.push(byte[0]);
        }
        let text = String::from_utf8_lossy(&buf).into_owned();
        let mut lines = text.lines();
        let path = lines.next().and_then(|l| l.split_whitespace().nth(1)).unwrap_or("").to_string();
        let headers: Vec<(String, String)> = lines
          .filter_map(|l| l.split_once(':'))
          .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
          .collect();
        record.lock().unwrap().push((path.clone(), headers));
        let (status, extra, body) = match routes.iter().find(|r| r.path == path) {
          Some(r) => (r.status, r.headers.clone(), r.body.clone()),
          None => (404, vec![], "not found".to_string()),
        };
        let mut response = format!("HTTP/1.1 {status} X\r\nConnection: close\r\nContent-Length: {}\r\n", body.len());
        for (k, v) in extra {
          response.push_str(&format!("{k}: {v}\r\n"));
        }
        response.push_str("\r\n");
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.write_all(body.as_bytes());
        let _ = stream.shutdown(std::net::Shutdown::Both);
      }
    });
    (base, seen)
  }

  fn header<'a>(req: &'a Request, name: &str) -> Option<&'a str> {
    req.1.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
  }

  fn key(c: char) -> String {
    format!("{}=", c.to_string().repeat(43))
  }

  fn bundle_text(tag: &str, spoke: char) -> String {
    format!(
      "hub_version = \"{tag}\"\nschema = 1\n\n[hub]\nname = \"wireguard-hub\"\npublic_key = \"{}\"\naddress = \"10.8.0.1/24\"\nlisten_port = 51820\nendpoint = \"wireguard-hub:51820\"\nallowed_ips = [\"10.8.0.0/24\"]\npersistent_keepalive = 25\n\n[[peers]]\nname = \"smoke-spoke\"\npublic_key = \"{}\"\naddress = \"10.8.0.20/32\"\n",
      key('A'),
      key(spoke)
    )
  }

  fn github_routes(tag: &str, spoke: char) -> Vec<Route> {
    vec![
      route("/version", 200, &[], &format!("{tag}\n")),
      route(
        &format!("/repos/o/r/releases/tags/{tag}"),
        200,
        &[("Content-Type", "application/json")],
        &format!(
          r#"{{"tag_name":"{tag}","assets":[{{"name":"other.conf","url":"{{base}}/assets/1"}},{{"name":"peers.toml","url":"{{base}}/assets/2"}}]}}"#
        ),
      ),
      route("/assets/2", 302, &[("Location", "{base}/download/peers.toml")], ""),
      route("/download/peers.toml", 200, &[], &bundle_text(tag, spoke)),
      route(
        &format!("/o/r/releases/download/{tag}/peers.toml"),
        200,
        &[],
        &bundle_text(tag, spoke),
      ),
    ]
  }

  fn fetcher(base: &str, token: Option<&str>) -> Fetcher {
    Fetcher::new(
      base.to_string(),
      "o/r".into(),
      token.map(str::to_string),
      Hosts {
        api: base.to_string(),
        web: base.to_string(),
      },
    )
  }

  #[test]
  fn the_version_is_a_strict_tag_and_failures_name_the_step() {
    let (base, _) = serve(vec![route("/version", 200, &[], "v0.1.0\n")]);
    assert_eq!(fetcher(&base, None).version().unwrap(), "v0.1.0");
    let (base, _) = serve(vec![route(
      "/version",
      200,
      &[],
      "<html>welcome to the wrong server, this page is a good deal longer than sixty-four bytes</html>",
    )]);
    let err = fetcher(&base, None).version().unwrap_err().to_string();
    assert!(
      err.contains("version endpoint") && err.contains("not a tag") && !err.contains("</html>"),
      "{err}"
    );
    let (base, _) = serve(vec![route("/version", 503, &[], "")]);
    let err = fetcher(&base, None).version().unwrap_err().to_string();
    assert!(err.contains("version endpoint") && err.contains("503"), "{err}");
  }

  #[test]
  fn the_token_goes_to_the_api_host_only() {
    let (base, seen) = serve(github_routes("v0.1.0", 'B'));
    let text = fetcher(&base, Some("tok")).bundle("v0.1.0").unwrap();
    assert!(text.contains("hub_version = \"v0.1.0\""));
    let seen = seen.lock().unwrap();
    let paths: Vec<&str> = seen.iter().map(|r| r.0.as_str()).collect();
    assert_eq!(paths, ["/repos/o/r/releases/tags/v0.1.0", "/assets/2", "/download/peers.toml"]);
    let listing = &seen[0];
    assert_eq!(header(listing, "authorization"), Some("Bearer tok"));
    assert_eq!(header(listing, "accept"), Some("application/vnd.github+json"));
    assert_eq!(header(listing, "x-github-api-version"), Some("2022-11-28"));
    assert!(header(listing, "user-agent").unwrap().starts_with("devkit-container/"));
    let asset = &seen[1];
    assert_eq!(header(asset, "authorization"), Some("Bearer tok"));
    assert_eq!(header(asset, "accept"), Some("application/octet-stream"));
    let download = &seen[2];
    assert_eq!(header(download, "authorization"), None, "the token never leaves the API host");
    assert_eq!(header(download, "accept"), Some("application/octet-stream"));
  }

  #[test]
  fn without_a_token_the_public_download_is_used() {
    let (base, seen) = serve(github_routes("v0.1.0", 'B'));
    fetcher(&base, None).bundle("v0.1.0").unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0].0, "/o/r/releases/download/v0.1.0/peers.toml");
    assert_eq!(header(&seen[0], "authorization"), None);
  }

  #[test]
  fn each_step_fails_with_its_name_and_never_the_token() {
    let missing = vec![route(
      "/repos/o/r/releases/tags/v0.1.0",
      200,
      &[],
      r#"{"assets":[{"name":"x.conf","url":"u"}]}"#,
    )];
    let (base, _) = serve(missing);
    let err = fetcher(&base, Some("tok")).bundle("v0.1.0").unwrap_err().to_string();
    assert!(
      err.contains("no peers.toml asset") && err.contains("v0.1.0") && !err.contains("tok"),
      "{err}"
    );
    let (base, _) = serve(vec![]);
    let err = fetcher(&base, Some("tok")).bundle("v0.1.0").unwrap_err().to_string();
    assert!(err.contains("release listing") && err.contains("404"), "{err}");
    let mut no_redirect = github_routes("v0.1.0", 'B');
    no_redirect[2] = route("/assets/2", 200, &[], "peers");
    let (base, _) = serve(no_redirect);
    let err = fetcher(&base, Some("tok")).bundle("v0.1.0").unwrap_err().to_string();
    assert!(err.contains("expected 302"), "{err}");
    let mut huge = github_routes("v0.1.0", 'B');
    huge[3] = route("/download/peers.toml", 200, &[], &"#".repeat(1024 * 1024 + 1));
    let (base, _) = serve(huge);
    assert!(
      fetcher(&base, Some("tok")).bundle("v0.1.0").is_err(),
      "a body over 1 MiB is refused"
    );
  }

  #[test]
  fn obtain_reports_unchanged_new_not_enrolled_and_unavailable() {
    let (base, _) = serve(github_routes("v0.1.0", 'B'));
    let f = fetcher(&base, Some("tok"));
    assert_eq!(obtain(&f, &key('B'), Some("v0.1.0")), Outcome::Unchanged("v0.1.0".into()));
    match obtain(&f, &key('B'), Some("v0.0.9")) {
      Outcome::New { tag, text, effective } => {
        assert_eq!(tag, "v0.1.0");
        assert!(text.contains("smoke-spoke"));
        assert_eq!(effective.address, "10.8.0.20/32");
        assert_eq!(effective.endpoint, "wireguard-hub:51820");
      }
      other => panic!("{other:?}"),
    }
    match obtain(&f, &key('Z'), None) {
      Outcome::NotEnrolled { tag, .. } => assert_eq!(tag, "v0.1.0"),
      other => panic!("{other:?}"),
    }
    let mut stale = github_routes("v0.2.0", 'B');
    stale[3] = route("/download/peers.toml", 200, &[], &bundle_text("v0.1.0", 'B'));
    let (base, _) = serve(stale);
    match obtain(&fetcher(&base, Some("tok")), &key('B'), None) {
      Outcome::Unavailable(msg) => assert!(msg.contains("v0.1.0") && msg.contains("v0.2.0"), "{msg}"),
      other => panic!("{other:?}"),
    }
    let (base, _) = serve(vec![]);
    match obtain(&fetcher(&base, Some("tok")), &key('B'), None) {
      Outcome::Unavailable(msg) => assert!(msg.contains("version endpoint"), "{msg}"),
      other => panic!("{other:?}"),
    }
  }
}
