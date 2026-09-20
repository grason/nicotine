use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::parse::AlertKind;

/// Rolling window used to turn combat hits into a rate. Full-window
/// divisor so a single volley does not print as thousands of DPS.
pub const DPS_WINDOW: Duration = Duration::from_secs(10);

/// How long the preview chrome stays lit after an incoming hit.
/// Long enough that a volley reads as one pulse, short enough that
/// it goes dark when the incoming stops.
pub const INCOMING_FLASH: Duration = Duration::from_millis(400);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DpsRates {
    pub incoming: f64,
    pub outgoing: f64,
}

impl DpsRates {
    pub fn is_idle(&self) -> bool {
        self.incoming < 1.0 && self.outgoing < 1.0
    }
}

struct Hit {
    at: Instant,
    amount: f64,
    incoming: bool,
}

/// Per-character combat hits. Lives on the tailer thread; flush rates
/// into `LogLiveState.dps` after each poll.
#[derive(Default)]
pub struct DpsTracker {
    hits: HashMap<String, VecDeque<Hit>>,
}

impl DpsTracker {
    pub fn add(&mut self, character: &str, incoming: bool, amount: f64, now: Instant) {
        self.hits
            .entry(character.to_string())
            .or_default()
            .push_back(Hit {
                at: now,
                amount,
                incoming,
            });
    }

    pub fn flush_into(&mut self, live: &mut LogLiveState, now: Instant) {
        let cutoff = now.checked_sub(DPS_WINDOW).unwrap_or(now);
        let window_secs = DPS_WINDOW.as_secs_f64();
        live.dps.clear();
        self.hits.retain(|name, q| {
            while q.front().is_some_and(|h| h.at < cutoff) {
                q.pop_front();
            }
            if q.is_empty() {
                return false;
            }
            let mut incoming = 0.0;
            let mut outgoing = 0.0;
            for h in q.iter() {
                if h.incoming {
                    incoming += h.amount;
                } else {
                    outgoing += h.amount;
                }
            }
            let rates = DpsRates {
                incoming: incoming / window_secs,
                outgoing: outgoing / window_secs,
            };
            if !rates.is_idle() {
                live.dps.insert(name.clone(), rates);
            }
            !q.is_empty()
        });
    }
}

#[derive(Debug, Clone)]
pub struct ActiveAlert {
    #[allow(dead_code)]
    pub kind: AlertKind,
    pub text: String,
    pub expires_at: Instant,
}

impl ActiveAlert {
    pub fn expired(&self, now: Instant) -> bool {
        now >= self.expires_at
    }
}

/// Live view the preview managers (and debug logs) read. Written by
/// the tailer thread.
#[derive(Debug, Default, Clone)]
pub struct LogLiveState {
    /// Character name → current solar system. Keys match `EveWindow.title`
    /// (the `EVE - ` prefix already stripped), compared case-insensitively
    /// by the consumers.
    pub systems: HashMap<String, String>,
    /// Character → currently displayed alert. Newest wins; expired
    /// entries are pruned by the tailer and should be ignored by
    /// painters if they linger.
    pub alerts: HashMap<String, ActiveAlert>,
    pub resolved_chatlog_dir: Option<PathBuf>,
    pub resolved_gamelog_dir: Option<PathBuf>,
    /// Incoming / outgoing DPS over the last `DPS_WINDOW`. Missing key
    /// means idle (both rates below 1).
    pub dps: HashMap<String, DpsRates>,
    /// Last incoming-hit time per character. Preview chrome flashes
    /// while `now - stamp < INCOMING_FLASH`.
    pub last_incoming: HashMap<String, Instant>,
}

impl LogLiveState {
    pub fn system_for(&self, character: &str) -> Option<&str> {
        if let Some(s) = self.systems.get(character) {
            return Some(s.as_str());
        }
        let needle = character.to_lowercase();
        self.systems
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(&needle))
            .map(|(_, v)| v.as_str())
    }

    pub fn alert_for(&self, character: &str, now: Instant) -> Option<&ActiveAlert> {
        let alert = self.alerts.get(character).or_else(|| {
            let needle = character.to_lowercase();
            self.alerts
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(&needle))
                .map(|(_, v)| v)
        })?;
        if alert.expired(now) {
            None
        } else {
            Some(alert)
        }
    }

    pub fn prune_expired(&mut self, now: Instant) {
        self.alerts.retain(|_, a| !a.expired(now));
    }

    /// Drop the alert for a character (any case). Used when that client
    /// is focused and `alerts_on_inactive_only` is on.
    pub fn dps_for(&self, character: &str) -> Option<DpsRates> {
        if let Some(r) = self.dps.get(character) {
            return Some(*r);
        }
        let needle = character.to_lowercase();
        self.dps
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(&needle))
            .map(|(_, v)| *v)
    }

    pub fn stamp_incoming(&mut self, character: &str, now: Instant) {
        self.last_incoming.insert(character.to_string(), now);
    }

    pub fn taking_damage(&self, character: &str, now: Instant) -> bool {
        let at = self.last_incoming.get(character).cloned().or_else(|| {
            let needle = character.to_lowercase();
            self.last_incoming
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(&needle))
                .map(|(_, v)| *v)
        });
        at.is_some_and(|t| now.saturating_duration_since(t) < INCOMING_FLASH)
    }

    pub fn drop_alert(&mut self, character: &str) {
        if self.alerts.remove(character).is_some() {
            return;
        }
        let needle = character.to_lowercase();
        self.alerts.retain(|k, _| !k.eq_ignore_ascii_case(&needle));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn lookup_is_case_insensitive() {
        let mut s = LogLiveState::default();
        s.systems.insert("Alpha".into(), "Jita".into());
        assert_eq!(s.system_for("alpha"), Some("Jita"));
        assert_eq!(s.system_for("ALPHA"), Some("Jita"));
    }

    #[test]
    fn expired_alert_hidden() {
        let mut s = LogLiveState::default();
        let now = Instant::now();
        s.alerts.insert(
            "Alpha".into(),
            ActiveAlert {
                kind: AlertKind::FleetInvite,
                text: "Fleet invite from Bob".into(),
                expires_at: now - Duration::from_secs(1),
            },
        );
        assert!(s.alert_for("Alpha", now).is_none());
        s.prune_expired(now);
        assert!(s.alerts.is_empty());
    }

    #[test]
    fn drop_alert_is_case_insensitive() {
        let mut s = LogLiveState::default();
        s.alerts.insert(
            "Alpha".into(),
            ActiveAlert {
                kind: AlertKind::FleetInvite,
                text: "Fleet invite from Bob".into(),
                expires_at: Instant::now() + Duration::from_secs(8),
            },
        );
        s.drop_alert("alpha");
        assert!(s.alerts.is_empty());
    }

    #[test]
    fn dps_window_full_divisor() {
        let mut t = DpsTracker::default();
        let t0 = Instant::now();
        t.add("Alpha", true, 1000.0, t0);
        t.add("Alpha", false, 500.0, t0);
        let mut live = LogLiveState::default();
        t.flush_into(&mut live, t0);
        let r = live.dps_for("alpha").expect("rates");
        assert!((r.incoming - 100.0).abs() < 1e-6, "{}", r.incoming);
        assert!((r.outgoing - 50.0).abs() < 1e-6, "{}", r.outgoing);
    }

    #[test]
    fn dps_expires_after_window() {
        let mut t = DpsTracker::default();
        let t0 = Instant::now();
        t.add("Alpha", true, 1000.0, t0);
        let mut live = LogLiveState::default();
        t.flush_into(&mut live, t0 + DPS_WINDOW + Duration::from_millis(1));
        assert!(live.dps_for("Alpha").is_none());
    }

    #[test]
    fn incoming_flash_coalesces_then_expires() {
        let mut s = LogLiveState::default();
        let t0 = Instant::now();
        s.stamp_incoming("Alpha", t0);
        assert!(s.taking_damage("alpha", t0 + Duration::from_millis(100)));
        s.stamp_incoming("Alpha", t0 + Duration::from_millis(300));
        assert!(s.taking_damage("Alpha", t0 + Duration::from_millis(600)));
        assert!(!s.taking_damage("Alpha", t0 + Duration::from_millis(800)));
    }
}
