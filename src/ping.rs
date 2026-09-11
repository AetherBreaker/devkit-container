//! The healthchecks.io ping, in `aeth_ext.monitoring.ping`'s exact shape (spec 7): a fixed
//! URL, else `https://hc-ping.com/<PINGKEY>/<HEARTBEAT_SLUG>` with `?create=1`; `/start`
//! once, plain while healthy, `/fail` with a body on a stale transition or a bad exit. The
//! request is made in-process by `ureq` over rustls with Mozilla's roots, off the supervisor's
//! thread; the URL carries the key, so it never reaches argv, logs or error text.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
  Start,
  Plain,
  Fail,
}

#[derive(Debug, Clone)]
pub struct Ping {
  base: String,
  autoprovision: bool,
}

fn set(v: Option<&str>) -> Option<&str> {
  v.map(str::trim).filter(|s| !s.is_empty())
}

impl Ping {
  /// A fixed URL, else key and slug; `None` when neither is configured (empty is unset).
  pub fn configure(url: Option<&str>, pingkey: Option<&str>, slug: Option<&str>) -> Option<Ping> {
    if let Some(url) = set(url) {
      return Some(Ping {
        base: url.to_string(),
        autoprovision: false,
      });
    }
    match (set(pingkey), set(slug)) {
      (Some(key), Some(slug)) => Some(Ping {
        base: format!("https://hc-ping.com/{key}/{slug}"),
        autoprovision: true,
      }),
      _ => None,
    }
  }

  pub fn url(&self, kind: Kind) -> String {
    let suffix = match kind {
      Kind::Start => "/start",
      Kind::Plain => "",
      Kind::Fail => "/fail",
    };
    let query = if self.autoprovision { "?create=1" } else { "" };
    format!("{}{suffix}{query}", self.base)
  }
}

/// `HEARTBEAT_SLUG`, else the single `[tool.docker].services` entry; two services would be
/// two containers pinging one check, so that case pings nothing (spec 7).
pub fn slug(env_slug: Option<&str>, services: &[String]) -> Option<String> {
  if let Some(s) = set(env_slug) {
    return Some(s.to_string());
  }
  match services {
    [one] => Some(one.clone()),
    _ => None,
  }
}

/// One agent for the process: the 10 s timeout `aeth_ext`'s `urlopen` uses, as a global bound.
#[cfg(unix)]
pub fn agent() -> ureq::Agent {
  ureq::Agent::config_builder()
    .timeout_global(Some(std::time::Duration::from_secs(10)))
    .build()
    .into()
}

/// The request, as `aeth_ext` makes it: GET without a body, POST with one; any non-2xx is a
/// failure and the response body is never read. The error text is ureq's, which names hosts,
/// status codes and timeouts but never the URL, so the caller may log it.
#[cfg(unix)]
pub fn send(agent: &ureq::Agent, url: &str, body: &str) -> Result<(), String> {
  let result = if body.is_empty() {
    agent.get(url).call()
  } else {
    agent.post(url).send(body)
  };
  result.map(drop).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_fixed_url_wins_and_never_autoprovisions() {
    let p = Ping::configure(Some("https://hc-ping.com/fixed-uuid"), Some("key"), Some("app")).unwrap();
    assert_eq!(p.url(Kind::Plain), "https://hc-ping.com/fixed-uuid");
    assert_eq!(p.url(Kind::Start), "https://hc-ping.com/fixed-uuid/start");
    assert_eq!(p.url(Kind::Fail), "https://hc-ping.com/fixed-uuid/fail");
  }

  #[test]
  fn key_and_slug_build_the_autoprovisioning_url() {
    let p = Ping::configure(None, Some("my-key"), Some("my-app")).unwrap();
    assert_eq!(p.url(Kind::Plain), "https://hc-ping.com/my-key/my-app?create=1");
    assert_eq!(p.url(Kind::Start), "https://hc-ping.com/my-key/my-app/start?create=1");
    assert_eq!(p.url(Kind::Fail), "https://hc-ping.com/my-key/my-app/fail?create=1");
  }

  #[test]
  fn nothing_pings_without_a_url_or_a_key_with_a_slug_and_empty_is_unset() {
    assert!(Ping::configure(None, None, Some("app")).is_none());
    assert!(Ping::configure(None, Some("key"), None).is_none());
    assert!(Ping::configure(Some(""), Some(""), Some("app")).is_none());
    assert!(Ping::configure(None, Some("key"), Some("")).is_none());
    assert!(Ping::configure(Some("  "), None, None).is_none());
  }

  #[test]
  fn the_slug_is_the_environment_else_the_single_service() {
    assert_eq!(slug(Some("svc"), &["a".into(), "b".into()]).as_deref(), Some("svc"));
    assert_eq!(slug(None, &["only".into()]).as_deref(), Some("only"));
    assert_eq!(slug(Some(""), &["only".into()]).as_deref(), Some("only"));
    assert_eq!(slug(None, &["a".into(), "b".into()]), None, "two services: no guessing");
    assert_eq!(slug(None, &[]), None);
  }

  #[cfg(unix)]
  #[test]
  fn a_failed_request_reports_without_the_url() {
    // Port 1 on loopback refuses at once, no network needed. The URL holds the key, so the
    // text the supervisor logs must not echo it.
    let err = send(&agent(), "http://127.0.0.1:1/secret-key/app", "").unwrap_err();
    assert!(!err.contains("secret-key"), "{err}");
  }
}
