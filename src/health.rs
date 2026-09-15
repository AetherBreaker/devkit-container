//! The tunnel's health (spec 5.3 to 5.6, 6.1, 6.3) as a pure state machine with an injected
//! clock: Connected or Disconnected with a reason, the continuous-disconnected clock, the
//! alternating repair, and the consent asks up to the give-up. Every shell-out and network call
//! is the supervisor's; this decides what to do at each poll.
#![allow(dead_code)] // until run and the supervisor use it (task 11)

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
  NoHandshake,
  EndpointUnresolvable,
  /// Only under the tolerate switch: the boot got no bundle and no usable cache.
  ConfigUnavailable,
  /// Only under the tolerate switch: the hub's bundle is valid and lacks this key.
  NotEnrolled,
}

impl Reason {
  pub fn as_str(self) -> &'static str {
    match self {
      Reason::NoHandshake => "no handshake",
      Reason::EndpointUnresolvable => "endpoint unresolvable",
      Reason::ConfigUnavailable => "config unavailable",
      Reason::NotEnrolled => "not enrolled",
    }
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
  Connected,
  Disconnected(Reason),
}

impl State {
  pub fn describe(self) -> String {
    match self {
      State::Connected => "Connected".to_string(),
      State::Disconnected(r) => format!("Disconnected: {}", r.as_str()),
    }
  }
}

/// The interface as the supervisor observed it this poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interface {
  /// No configuration applied: only under the tolerate switch, after a boot that got none.
  Unconfigured(Reason),
  /// A configuration is applied; `endpoint_ok` is false after the endpoint command failed.
  Configured { endpoint_ok: bool },
}

/// What the app answered, with every failure and timeout already folded into `Ok` by the
/// consent client (spec 6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
  Ok,
  Hold,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Repair {
  None,
  /// Under the switch with no configuration: fetch one (5.6 step 1).
  Obtain,
  /// Re-set the endpoint, re-resolving the hub's name; the interface keeps running.
  ResetEndpoint,
  /// The interface down and up again with the applied configuration.
  DownUp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GiveUp {
  /// Continuous Disconnected seconds, for `gave up after <n> s`.
  pub elapsed: u64,
  /// `WG_HOLD_LIMIT_SECS` ran out; proceeded without asking again.
  pub hold_limit: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
  pub state: State,
  /// The state differs from the previous poll's (or this is the first poll): log it.
  pub changed: bool,
  pub repair: Repair,
  /// Fetched mode, Disconnected, a configuration applied: start a version check if none is in flight.
  pub check_version: bool,
  /// Start a consent ask with `wireguard-disconnected <n>s`, `n` being the seconds carried here.
  pub ask: Option<u64>,
  pub give_up: Option<GiveUp>,
}

/// Seconds between asks, measured from the previous reply or timeout (6.3).
pub const ASK_INTERVAL_SECS: u64 = 60;

#[derive(Debug)]
struct Pending {
  first_ask: u64,
  outstanding: bool,
  next_ask: u64,
}

#[derive(Debug)]
pub struct Health {
  tolerate: bool,
  fetched: bool,
  limit_secs: u64,
  hold_limit_secs: u64,
  last_state: Option<State>,
  disconnected_since: Option<u64>,
  next_repair: Repair,
  pending: Option<Pending>,
}

impl Health {
  pub fn new(tolerate: bool, fetched: bool, limit_secs: u64, hold_limit_secs: u64) -> Health {
    Health {
      tolerate,
      fetched,
      limit_secs,
      hold_limit_secs,
      last_state: None,
      disconnected_since: None,
      next_repair: Repair::ResetEndpoint,
      pending: None,
    }
  }

  /// One poll. `now` is any monotonic clock in seconds; `fresh` is a handshake younger than
  /// `WG_STALE_SECS`; `reply` is a consent reply that arrived since the previous poll.
  pub fn poll(&mut self, now: u64, iface: Interface, fresh: bool, reply: Option<Reply>) -> Decision {
    let state = if fresh {
      State::Connected
    } else {
      State::Disconnected(match iface {
        Interface::Unconfigured(r) => r,
        Interface::Configured { endpoint_ok: false } => Reason::EndpointUnresolvable,
        Interface::Configured { endpoint_ok: true } => Reason::NoHandshake,
      })
    };
    let changed = self.last_state != Some(state);
    self.last_state = Some(state);
    let mut d = Decision {
      state,
      changed,
      repair: Repair::None,
      check_version: false,
      ask: None,
      give_up: None,
    };
    if fresh {
      // Connected resets the clock, cancels a pending shutdown and restarts the alternation.
      self.disconnected_since = None;
      self.pending = None;
      self.next_repair = Repair::ResetEndpoint;
      return d;
    }
    let since = *self.disconnected_since.get_or_insert(now);
    let elapsed = now.saturating_sub(since);
    match iface {
      Interface::Unconfigured(_) => d.repair = Repair::Obtain,
      Interface::Configured { .. } => {
        d.repair = self.next_repair;
        self.next_repair = match self.next_repair {
          Repair::ResetEndpoint => Repair::DownUp,
          _ => Repair::ResetEndpoint,
        };
        d.check_version = self.fetched;
      }
    }
    if self.tolerate || elapsed < self.limit_secs {
      return d;
    }
    match &mut self.pending {
      None => {
        self.pending = Some(Pending {
          first_ask: now,
          outstanding: true,
          next_ask: now,
        });
        d.ask = Some(elapsed);
      }
      Some(p) => {
        if let Some(r) = reply {
          p.outstanding = false;
          match r {
            Reply::Ok => {
              d.give_up = Some(GiveUp {
                elapsed,
                hold_limit: false,
              })
            }
            Reply::Hold => p.next_ask = now + ASK_INTERVAL_SECS,
          }
        }
        if d.give_up.is_none() && self.hold_limit_secs > 0 && now.saturating_sub(p.first_ask) >= self.hold_limit_secs {
          d.give_up = Some(GiveUp { elapsed, hold_limit: true });
        } else if d.give_up.is_none() && !p.outstanding && now >= p.next_ask {
          p.outstanding = true;
          d.ask = Some(elapsed);
        }
      }
    }
    d
  }

  /// A configuration was applied by a repair or a re-apply: the alternation starts over (5.6).
  pub fn config_applied(&mut self) {
    self.next_repair = Repair::ResetEndpoint;
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const OK: Interface = Interface::Configured { endpoint_ok: true };

  fn default() -> Health {
    Health::new(false, true, 1800, 0)
  }

  #[test]
  fn connected_polls_decide_nothing_and_log_the_first_transition() {
    let mut h = default();
    let d = h.poll(0, OK, true, None);
    assert_eq!(d.state, State::Connected);
    assert!(d.changed, "the first poll is a transition from nothing");
    assert_eq!((d.repair, d.check_version, d.ask, d.give_up), (Repair::None, false, None, None));
    let d = h.poll(30, OK, true, None);
    assert!(!d.changed);
  }

  #[test]
  fn the_reason_follows_the_interface() {
    let mut h = default();
    assert_eq!(h.poll(0, OK, false, None).state, State::Disconnected(Reason::NoHandshake));
    assert_eq!(
      h.poll(30, Interface::Configured { endpoint_ok: false }, false, None).state,
      State::Disconnected(Reason::EndpointUnresolvable)
    );
    let mut t = Health::new(true, true, 1800, 0);
    assert_eq!(
      t.poll(0, Interface::Unconfigured(Reason::ConfigUnavailable), false, None).state,
      State::Disconnected(Reason::ConfigUnavailable)
    );
    assert_eq!(
      t.poll(30, Interface::Unconfigured(Reason::NotEnrolled), false, None).state,
      State::Disconnected(Reason::NotEnrolled)
    );
    assert_eq!(Reason::EndpointUnresolvable.as_str(), "endpoint unresolvable");
    assert_eq!(State::Disconnected(Reason::NoHandshake).describe(), "Disconnected: no handshake");
    assert_eq!(State::Connected.describe(), "Connected");
  }

  #[test]
  fn the_repair_alternates_and_restarts_after_connected_or_an_apply() {
    let mut h = default();
    h.poll(0, OK, true, None);
    assert_eq!(h.poll(30, OK, false, None).repair, Repair::ResetEndpoint);
    assert_eq!(h.poll(60, OK, false, None).repair, Repair::DownUp);
    assert_eq!(h.poll(90, OK, false, None).repair, Repair::ResetEndpoint);
    assert_eq!(h.poll(120, OK, false, None).repair, Repair::DownUp);
    assert_eq!(h.poll(150, OK, true, None).repair, Repair::None, "Connected repairs nothing");
    assert_eq!(h.poll(180, OK, false, None).repair, Repair::ResetEndpoint, "starts over");
    assert_eq!(h.poll(210, OK, false, None).repair, Repair::DownUp);
    h.config_applied();
    assert_eq!(
      h.poll(240, OK, false, None).repair,
      Repair::ResetEndpoint,
      "starts over after an apply"
    );
  }

  #[test]
  fn an_unconfigured_interface_is_obtained_and_never_version_checked() {
    let mut t = Health::new(true, true, 1800, 0);
    let d = t.poll(0, Interface::Unconfigured(Reason::ConfigUnavailable), false, None);
    assert_eq!((d.repair, d.check_version), (Repair::Obtain, false));
    let d = t.poll(30, Interface::Unconfigured(Reason::ConfigUnavailable), false, None);
    assert_eq!(d.repair, Repair::Obtain, "every poll");
    t.config_applied();
    let d = t.poll(60, OK, false, None);
    assert_eq!((d.repair, d.check_version), (Repair::ResetEndpoint, true));
    let mut env_mode = Health::new(false, false, 1800, 0);
    assert!(
      !env_mode.poll(0, OK, false, None).check_version,
      "no version check in environment mode"
    );
  }

  #[test]
  fn the_first_ask_comes_when_the_clock_crosses_the_limit_then_every_60_s_after_a_reply() {
    let mut h = default();
    h.poll(0, OK, true, None);
    assert_eq!(h.poll(10, OK, false, None).ask, None, "the clock starts here");
    assert_eq!(h.poll(1790, OK, false, None).ask, None, "1780 s in");
    let d = h.poll(1810, OK, false, None);
    assert_eq!(d.ask, Some(1800), "crossed: ask with the elapsed seconds");
    assert_eq!(h.poll(1840, OK, false, None).ask, None, "one ask outstanding");
    let d = h.poll(1870, OK, false, Some(Reply::Hold));
    assert_eq!((d.ask, d.give_up), (None, None), "hold postpones");
    assert_eq!(h.poll(1900, OK, false, None).ask, None, "not 60 s yet");
    assert_eq!(h.poll(1930, OK, false, None).ask, Some(1920), "60 s after the reply");
    let d = h.poll(1960, OK, false, Some(Reply::Ok));
    assert_eq!(
      d.give_up,
      Some(GiveUp {
        elapsed: 1950,
        hold_limit: false
      })
    );
  }

  #[test]
  fn a_timeout_counts_as_ok_by_the_client_and_connected_cancels_everything() {
    let mut h = default();
    h.poll(0, OK, false, None);
    assert!(h.poll(1800, OK, false, None).ask.is_some());
    let d = h.poll(1830, OK, true, None);
    assert_eq!((d.state, d.ask, d.give_up), (State::Connected, None, None));
    let d = h.poll(1860, OK, false, Some(Reply::Ok));
    assert_eq!(d.give_up, None, "a late reply after Connected is ignored");
    assert_eq!(h.poll(3659, OK, false, None).ask, None, "the clock restarted at 1860");
    assert_eq!(h.poll(3660, OK, false, None).ask, Some(1800));
  }

  #[test]
  fn the_hold_limit_proceeds_without_asking_again() {
    let mut h = Health::new(false, true, 1800, 100);
    h.poll(0, OK, false, None);
    assert_eq!(h.poll(1800, OK, false, None).ask, Some(1800));
    h.poll(1830, OK, false, Some(Reply::Hold));
    assert_eq!(h.poll(1890, OK, false, None).ask, Some(1890));
    let d = h.poll(1900, OK, false, None);
    assert_eq!(
      d.give_up,
      Some(GiveUp {
        elapsed: 1900,
        hold_limit: true
      }),
      "100 s after the first ask"
    );
  }

  #[test]
  fn under_the_switch_there_is_no_ask_and_no_give_up() {
    let mut t = Health::new(true, true, 1800, 100);
    t.poll(0, OK, false, None);
    let d = t.poll(10_000, OK, false, None);
    assert_eq!((d.ask, d.give_up), (None, None));
    assert_eq!(d.repair, Repair::DownUp, "repairs go on");
  }
}
