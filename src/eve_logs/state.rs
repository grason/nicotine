use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::parse::AlertKind;
use crate::config::AlertClear;

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
    /// Side bars. Missing key means both sides are idle.
    pub meters: HashMap<String, CombatMeter>,
}

/// Discrete steps on each side bar.
pub const METER_SEGMENTS: u8 = 8;

/// What the thumbnail bars draw. `incoming` is recent damage taken.
/// `outgoing` is turret application when a gun has fired in the window.
/// With no turret sample it is outgoing DPS on the same scale as
/// `incoming`. `crit` is a wrecking turret hit still inside that window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CombatMeter {
    pub incoming: u8,
    pub outgoing: u8,
    pub incoming_live: bool,
    pub outgoing_live: bool,
    pub crit: bool,
}

impl CombatMeter {
    /// Compact form for the Windows overlay repaint key. Linux compares
    /// the struct itself, so this stays out of the Linux build.
    #[cfg(windows)]
    pub fn sig(self) -> String {
        format!(
            "{}:{}:{}:{}:{}",
            self.incoming,
            self.outgoing,
            u8::from(self.incoming_live),
            u8::from(self.outgoing_live),
            u8::from(self.crit)
        )
    }
}

/// Map incoming DPS onto [`METER_SEGMENTS`]. Under 10 DPS stays empty.
/// Each doubling fills another segment: 10 → 1, 20 → 1, 40 → 2, …, 2560 → 8.
pub fn incoming_segments(dps: f64) -> u8 {
    if !dps.is_finite() || dps < 10.0 {
        return 0;
    }
    let n = (dps / 10.0).log2();
    if !n.is_finite() {
        return 0;
    }
    let rounded = n.round();
    if rounded < 1.0 {
        1
    } else {
        rounded.min(f64::from(METER_SEGMENTS)) as u8
    }
}

/// Four turret cycles. A 4 s gun watches 16 s; the clamp keeps a
/// hand-edited cycle time from making the bar stick or flicker.
pub fn application_window(cycle: Duration) -> Duration {
    let secs = cycle.as_secs_f64().clamp(0.5, 20.0) * 4.0;
    Duration::from_secs_f64(secs.clamp(2.0, 80.0))
}

/// Shots closer than this are one volley. A rack of guns lands inside
/// a fraction of the cycle; the cap keeps a slow gun from merging two
/// real cycles.
fn volley_gap(cycle: Duration) -> Duration {
    let secs = (cycle.as_secs_f64().clamp(0.5, 20.0) * 0.45).clamp(0.25, 1.5);
    Duration::from_secs_f64(secs)
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

    pub fn alert_for(
        &self,
        character: &str,
        now: Instant,
        clear: AlertClear,
    ) -> Option<&ActiveAlert> {
        let alert = self.alerts.get(character).or_else(|| {
            let needle = character.to_lowercase();
            self.alerts
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(&needle))
                .map(|(_, v)| v)
        })?;
        // Until-focus ignores the timestamp. Switching to the timeout
        // then hides anything older than `alert_secs`, because the
        // stamp was set when the line was read.
        if clear == AlertClear::Timeout && alert.expired(now) {
            None
        } else {
            Some(alert)
        }
    }

    pub fn prune_expired(&mut self, now: Instant, clear: AlertClear) {
        if clear == AlertClear::UntilFocus {
            return;
        }
        self.alerts.retain(|_, a| !a.expired(now));
    }

    pub fn meter_for(&self, character: &str) -> Option<CombatMeter> {
        if let Some(m) = self.meters.get(character) {
            return Some(*m);
        }
        let needle = character.to_lowercase();
        self.meters
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(&needle))
            .map(|(_, v)| *v)
    }

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

    /// Drop the alert for a character (any case). Used when that client
    /// is focused and `alerts_on_inactive_only` is on.
    pub fn drop_alert(&mut self, character: &str) {
        if self.alerts.remove(character).is_some() {
            return;
        }
        let needle = character.to_lowercase();
        self.alerts.retain(|k, _| !k.eq_ignore_ascii_case(&needle));
    }
}

struct Shot {
    at: Instant,
    weight: f64,
    crit: bool,
}

#[derive(Default)]
struct AppChar {
    target: String,
    shots: VecDeque<Shot>,
}

/// Outgoing turret samples. Missiles and drones never arrive here.
/// A target-name change drops the window so the bar restarts.
#[derive(Default)]
pub struct ApplicationTracker {
    chars: HashMap<String, AppChar>,
}

impl ApplicationTracker {
    pub fn add_outgoing(
        &mut self,
        character: &str,
        target: &str,
        weight: f64,
        crit: bool,
        now: Instant,
    ) {
        let entry = self.chars.entry(character.to_string()).or_default();
        if !target.is_empty()
            && !entry.target.is_empty()
            && !entry.target.eq_ignore_ascii_case(target)
        {
            entry.shots.clear();
        }
        if !target.is_empty() {
            entry.target = target.to_string();
        }
        entry.shots.push_back(Shot {
            at: now,
            weight,
            crit,
        });
    }

    /// Rebuild `live.meters` from turret samples plus the DPS rates
    /// already flushed this poll. Call after [`DpsTracker::flush_into`].
    /// Outgoing DPS fills the green bar only when that character has no
    /// turret sample, and it does not set `crit`.
    pub fn flush_into(&mut self, live: &mut LogLiveState, now: Instant, cycle: Duration) {
        let cutoff = now.checked_sub(application_window(cycle)).unwrap_or(now);
        self.chars.retain(|_, ch| {
            while ch.shots.front().is_some_and(|s| s.at < cutoff) {
                ch.shots.pop_front();
            }
            if ch.shots.is_empty() {
                ch.target.clear();
                return false;
            }
            true
        });

        let mut next: HashMap<String, CombatMeter> = HashMap::new();
        for (name, ch) in &self.chars {
            let (filled, crit) = score_shots(&ch.shots, cycle);
            next.insert(
                name.clone(),
                CombatMeter {
                    outgoing: filled,
                    outgoing_live: true,
                    crit,
                    ..CombatMeter::default()
                },
            );
        }
        for (name, rates) in &live.dps {
            let incoming_segs = incoming_segments(rates.incoming);
            let outgoing_segs = incoming_segments(rates.outgoing);
            let incoming_ok = incoming_segs > 0;
            let outgoing_ok = outgoing_segs > 0;
            if !incoming_ok && !outgoing_ok {
                continue;
            }
            if let Some((_, meter)) = next
                .iter_mut()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
            {
                if incoming_ok {
                    meter.incoming = incoming_segs;
                    meter.incoming_live = true;
                }
                // A turret sample already owns this side, including an
                // all-miss window that draws an empty track.
                if outgoing_ok && !meter.outgoing_live {
                    meter.outgoing = outgoing_segs;
                    meter.outgoing_live = true;
                }
            } else {
                next.insert(
                    name.clone(),
                    CombatMeter {
                        incoming: if incoming_ok { incoming_segs } else { 0 },
                        incoming_live: incoming_ok,
                        outgoing: if outgoing_ok { outgoing_segs } else { 0 },
                        outgoing_live: outgoing_ok,
                        crit: false,
                    },
                );
            }
        }
        live.meters = next;
    }
}

fn score_shots(shots: &VecDeque<Shot>, cycle: Duration) -> (u8, bool) {
    if shots.is_empty() {
        return (0, false);
    }
    let gap = volley_gap(cycle);
    let mut cycles: Vec<(f64, usize)> = Vec::new();
    let mut crit = false;
    let mut last: Option<Instant> = None;
    for shot in shots {
        crit |= shot.crit;
        let split = last.is_some_and(|t| shot.at.saturating_duration_since(t) > gap);
        if last.is_none() || split {
            cycles.push((0.0, 0));
        }
        let Some(bucket) = cycles.last_mut() else {
            continue;
        };
        bucket.0 += shot.weight;
        bucket.1 += 1;
        last = Some(shot.at);
    }
    if cycles.is_empty() {
        return (0, crit);
    }
    let mut sum = 0.0;
    for (total, count) in &cycles {
        if *count > 0 {
            sum += total / *count as f64;
        }
    }
    let mean = sum / cycles.len() as f64;
    let filled = (mean * f64::from(METER_SEGMENTS))
        .round()
        .clamp(0.0, f64::from(METER_SEGMENTS)) as u8;
    (filled, crit)
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
        assert!(s.alert_for("Alpha", now, AlertClear::Timeout).is_none());
        s.prune_expired(now, AlertClear::Timeout);
        assert!(s.alerts.is_empty());
    }

    #[test]
    fn until_focus_keeps_an_expired_stamp() {
        let mut s = LogLiveState::default();
        let now = Instant::now();
        s.alerts.insert(
            "Alpha".into(),
            ActiveAlert {
                kind: AlertKind::Scramble,
                text: "scramble".into(),
                expires_at: now - Duration::from_secs(30),
            },
        );
        assert!(s.alert_for("Alpha", now, AlertClear::UntilFocus).is_some());
        s.prune_expired(now, AlertClear::UntilFocus);
        assert_eq!(s.alerts.len(), 1);
        assert!(s.alert_for("Alpha", now, AlertClear::Timeout).is_none());
        s.prune_expired(now, AlertClear::Timeout);
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
    fn incoming_segments_double_each_step() {
        assert_eq!(incoming_segments(0.0), 0);
        assert_eq!(incoming_segments(9.0), 0);
        assert_eq!(incoming_segments(10.0), 1);
        assert_eq!(incoming_segments(20.0), 1);
        assert_eq!(incoming_segments(40.0), 2);
        assert_eq!(incoming_segments(80.0), 3);
        assert_eq!(incoming_segments(2560.0), 8);
        assert_eq!(incoming_segments(20_000.0), 8);
    }

    fn cycle() -> Duration {
        Duration::from_secs(4)
    }

    #[test]
    fn wreck_volley_fills_the_bar_and_flags_crit() {
        let mut t = ApplicationTracker::default();
        let t0 = Instant::now();
        for i in 0..4 {
            t.add_outgoing(
                "Alpha",
                "Rat",
                1.0,
                true,
                t0 + Duration::from_millis(20 * i),
            );
        }
        let mut live = LogLiveState::default();
        t.flush_into(&mut live, t0 + Duration::from_millis(80), cycle());
        let m = live.meter_for("alpha").expect("meter");
        assert!(m.outgoing_live);
        assert_eq!(m.outgoing, METER_SEGMENTS);
        assert!(m.crit);
        assert!(!m.incoming_live);
    }

    #[test]
    fn one_miss_in_a_volley_drops_a_segment() {
        let mut t = ApplicationTracker::default();
        let t0 = Instant::now();
        for i in 0..7 {
            t.add_outgoing(
                "Alpha",
                "Rat",
                1.0,
                true,
                t0 + Duration::from_millis(10 * i),
            );
        }
        t.add_outgoing("Alpha", "Rat", 0.0, false, t0 + Duration::from_millis(80));
        let mut live = LogLiveState::default();
        t.flush_into(&mut live, t0 + Duration::from_millis(80), cycle());
        let m = live.meter_for("Alpha").expect("meter");
        // 7/8 = 0.875 → 7 segments. Still one volley, still a crit.
        assert_eq!(m.outgoing, 7);
        assert!(m.crit);
    }

    #[test]
    fn all_misses_keep_an_empty_live_bar() {
        let mut t = ApplicationTracker::default();
        let t0 = Instant::now();
        t.add_outgoing("Alpha", "Rat", 0.0, false, t0);
        let mut live = LogLiveState::default();
        t.flush_into(&mut live, t0, cycle());
        let m = live.meter_for("Alpha").expect("meter");
        assert!(m.outgoing_live);
        assert_eq!(m.outgoing, 0);
        assert!(!m.crit);
    }

    #[test]
    fn target_switch_drops_the_previous_window() {
        let mut t = ApplicationTracker::default();
        let t0 = Instant::now();
        t.add_outgoing("Alpha", "Rat", 1.0, true, t0);
        t.add_outgoing("Alpha", "Other", 0.0, false, t0 + Duration::from_millis(50));
        let mut live = LogLiveState::default();
        t.flush_into(&mut live, t0 + Duration::from_millis(50), cycle());
        let m = live.meter_for("Alpha").expect("meter");
        assert_eq!(m.outgoing, 0);
        assert!(!m.crit);
        assert!(m.outgoing_live);
    }

    #[test]
    fn same_target_different_case_does_not_reset() {
        let mut t = ApplicationTracker::default();
        let t0 = Instant::now();
        t.add_outgoing("Alpha", "Rat", 1.0, true, t0);
        t.add_outgoing("Alpha", "rat", 1.0, true, t0 + Duration::from_millis(30));
        let mut live = LogLiveState::default();
        t.flush_into(&mut live, t0 + Duration::from_millis(30), cycle());
        assert_eq!(live.meter_for("Alpha").unwrap().outgoing, METER_SEGMENTS);
    }

    #[test]
    fn two_cycles_average() {
        let mut t = ApplicationTracker::default();
        let t0 = Instant::now();
        t.add_outgoing("Alpha", "Rat", 1.0, true, t0);
        // Volley gap at 4 s is 1.5 s, so 2 s is a second cycle.
        t.add_outgoing("Alpha", "Rat", 0.0, false, t0 + Duration::from_secs(2));
        let mut live = LogLiveState::default();
        t.flush_into(&mut live, t0 + Duration::from_secs(2), cycle());
        let m = live.meter_for("Alpha").expect("meter");
        assert_eq!(m.outgoing, 4);
        assert!(m.crit);
    }

    #[test]
    fn application_expires_with_the_window() {
        let mut t = ApplicationTracker::default();
        let t0 = Instant::now();
        t.add_outgoing("Alpha", "Rat", 1.0, true, t0);
        let mut live = LogLiveState::default();
        let later = t0 + application_window(cycle()) + Duration::from_millis(1);
        t.flush_into(&mut live, later, cycle());
        assert!(live.meter_for("Alpha").is_none());
    }

    #[test]
    fn incoming_bar_uses_dps_and_survives_without_turrets() {
        let mut dps = DpsTracker::default();
        let t0 = Instant::now();
        dps.add("Alpha", true, 800.0, t0);
        let mut live = LogLiveState::default();
        dps.flush_into(&mut live, t0);
        let mut guns = ApplicationTracker::default();
        guns.flush_into(&mut live, t0, cycle());
        let m = live.meter_for("Alpha").expect("meter");
        assert!(m.incoming_live);
        assert!(!m.outgoing_live);
        assert_eq!(m.incoming, incoming_segments(80.0));
    }

    #[test]
    fn missile_outgoing_lights_the_green_bar_without_turrets() {
        let mut dps = DpsTracker::default();
        let t0 = Instant::now();
        // 800 damage over the 10 s window is 80 DPS.
        dps.add("Alpha", false, 800.0, t0);
        let mut live = LogLiveState::default();
        dps.flush_into(&mut live, t0);
        let mut guns = ApplicationTracker::default();
        guns.flush_into(&mut live, t0, cycle());
        let m = live.meter_for("Alpha").expect("meter");
        assert!(m.outgoing_live);
        assert!(!m.incoming_live);
        assert!(!m.crit);
        assert_eq!(m.outgoing, incoming_segments(80.0));
    }

    #[test]
    fn outgoing_under_ten_dps_stays_dark() {
        let mut dps = DpsTracker::default();
        let t0 = Instant::now();
        dps.add("Alpha", false, 50.0, t0);
        let mut live = LogLiveState::default();
        dps.flush_into(&mut live, t0);
        let mut guns = ApplicationTracker::default();
        guns.flush_into(&mut live, t0, cycle());
        assert!(live.meter_for("Alpha").is_none());
    }

    #[test]
    fn turret_application_is_not_replaced_by_outgoing_dps() {
        let mut guns = ApplicationTracker::default();
        let t0 = Instant::now();
        guns.add_outgoing("Alpha", "Rat", 0.0, false, t0);
        let mut dps = DpsTracker::default();
        dps.add("Alpha", false, 8000.0, t0);
        let mut live = LogLiveState::default();
        dps.flush_into(&mut live, t0);
        guns.flush_into(&mut live, t0, cycle());
        let m = live.meter_for("Alpha").expect("meter");
        assert!(m.outgoing_live);
        assert_eq!(m.outgoing, 0);
        assert!(!m.crit);
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
