//! In-memory history of each server: memory and CPU of its process tree
//! sampled every pass, lifecycle events, growth over the last 10 minutes and
//! the health colour built on them. Nothing here is persisted.

use crate::agents::AgentKind;
use crate::config::Config;
use crate::model::ServerKey;
use crate::platform::ProcResources;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

/// How long samples and events are kept.
pub const HISTORY_MS: u64 = 7_200_000;
/// The window growth is measured over.
pub const GROWTH_WINDOW_MS: u64 = 600_000;
pub const MB: u64 = 1_048_576;

/// Memory from which a server is amber (red is a leak).
const AMBER_MEMORY_MB: u64 = 500;
/// An ignored leak alert comes back once memory grew this much more.
pub const LEAK_REARM_MB: u64 = 500;

/// One process of a tree: (pid, start time), so a reused pid is a new process.
pub type ProcId = (i32, u64);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub at_ms: u64,
    pub rss_bytes: u64,
    /// 100 = one core fully used by the whole tree.
    pub cpu_pct: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertKind {
    Idle,
    Leak,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventKind {
    Started { agent: Option<AgentKind> },
    BrowserConnected,
    BrowserClosed,
    SessionEnded,
    Alert { alert: AlertKind },
    Restarted,
    Postponed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServerEvent {
    pub at_ms: u64,
    pub kind: EventKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Green,
    Amber,
    Red,
}

#[derive(Default)]
struct Series {
    samples: Vec<Sample>,
    events: Vec<ServerEvent>,
    /// CPU time of each process at the previous pass.
    cpu_ns: HashMap<ProcId, u64>,
    last_at_ms: Option<u64>,
    established: u32,
    session_ended: bool,
    /// Alert kinds already pushed, so pruning never lets one repeat.
    alerted: HashSet<AlertKind>,
}

#[derive(Default)]
pub struct History {
    servers: HashMap<ServerKey, Series>,
}

impl History {
    /// Registers a server; true the first time it is seen.
    pub fn track(&mut self, key: ServerKey) -> bool {
        let new = !self.servers.contains_key(&key);
        self.servers.entry(key).or_default();
        new
    }

    /// Adds one sample: the summed rss of the tree, and its CPU over the wall
    /// time since the previous pass. Only processes present in both passes
    /// count towards CPU, so a worker that exits or appears never makes a
    /// negative or inflated delta.
    pub fn record(&mut self, key: ServerKey, at_ms: u64, tree: &[(ProcId, ProcResources)]) {
        let s = self.servers.entry(key).or_default();
        let rss_bytes = tree.iter().map(|(_, r)| r.rss_bytes).sum();
        let cpu_pct = match s.last_at_ms {
            Some(prev) if at_ms > prev => {
                // A process new this pass has no earlier counter: it adds no CPU yet.
                let delta: u64 = tree
                    .iter()
                    .filter_map(|(id, r)| {
                        s.cpu_ns
                            .get(id)
                            .map(|&before| r.cpu_time_ns.saturating_sub(before))
                    })
                    .sum();
                (delta as f64 / ((at_ms - prev) as f64 * 1_000_000.0) * 100.0) as f32
            }
            _ => 0.0,
        };
        s.cpu_ns = tree.iter().map(|&(id, r)| (id, r.cpu_time_ns)).collect();
        s.last_at_ms = Some(at_ms);
        s.samples.push(Sample {
            at_ms,
            rss_bytes,
            cpu_pct,
        });
    }

    /// Emits `BrowserConnected` / `BrowserClosed` when the count crosses zero.
    pub fn note_connections(&mut self, key: ServerKey, at_ms: u64, established: u32) {
        let s = self.servers.entry(key).or_default();
        let kind = match (s.established, established) {
            (0, n) if n > 0 => Some(EventKind::BrowserConnected),
            (n, 0) if n > 0 => Some(EventKind::BrowserClosed),
            _ => None,
        };
        s.established = established;
        if let Some(kind) = kind {
            s.events.push(ServerEvent { at_ms, kind });
        }
    }

    pub fn push_event(&mut self, key: ServerKey, ev: ServerEvent) {
        self.servers.entry(key).or_default().events.push(ev);
    }

    /// Records `SessionEnded` the first time only; true when it was pushed.
    pub fn note_session_ended(&mut self, key: ServerKey, at_ms: u64) -> bool {
        let s = self.servers.entry(key).or_default();
        if s.session_ended {
            return false;
        }
        s.session_ended = true;
        s.events.push(ServerEvent {
            at_ms,
            kind: EventKind::SessionEnded,
        });
        true
    }

    /// Records an alert event once per kind; true when it was pushed. Call
    /// `reset_alert` when the alert legitimately re-arms.
    pub fn note_alert(&mut self, key: ServerKey, at_ms: u64, kind: AlertKind) -> bool {
        let s = self.servers.entry(key).or_default();
        if !s.alerted.insert(kind) {
            return false;
        }
        s.events.push(ServerEvent {
            at_ms,
            kind: EventKind::Alert { alert: kind },
        });
        true
    }

    pub fn reset_alert(&mut self, key: ServerKey, kind: AlertKind) {
        if let Some(s) = self.servers.get_mut(&key) {
            s.alerted.remove(&kind);
        }
    }

    pub fn samples(&self, key: ServerKey) -> &[Sample] {
        self.servers.get(&key).map_or(&[], |s| &s.samples)
    }

    pub fn events(&self, key: ServerKey) -> &[ServerEvent] {
        self.servers.get(&key).map_or(&[], |s| &s.events)
    }

    /// Latest rss minus the oldest sample inside the growth window; 0 with
    /// fewer than two samples.
    pub fn growth_bytes(&self, key: ServerKey, now_ms: u64) -> i64 {
        let samples = self.samples(key);
        let from = now_ms.saturating_sub(GROWTH_WINDOW_MS);
        let Some(last) = samples.last() else {
            return 0;
        };
        match samples.iter().find(|s| s.at_ms >= from) {
            Some(first) if first.at_ms < last.at_ms => {
                last.rss_bytes as i64 - first.rss_bytes as i64
            }
            _ => 0,
        }
    }

    /// Drops servers no longer live, and samples and events past the horizon.
    pub fn retain(&mut self, live: &HashSet<ServerKey>, now_ms: u64) {
        self.servers.retain(|k, _| live.contains(k));
        let from = now_ms.saturating_sub(HISTORY_MS);
        for s in self.servers.values_mut() {
            s.samples.retain(|x| x.at_ms >= from);
            s.events.retain(|e| e.at_ms >= from);
        }
    }
}

/// Red exactly when the server leaks (`is_leak`), amber from 500 MB, green
/// below.
pub fn health(rss_bytes: u64, growth_bytes: i64, cfg: &Config) -> Health {
    if is_leak(rss_bytes, growth_bytes, cfg) {
        Health::Red
    } else if rss_bytes >= AMBER_MEMORY_MB * MB {
        Health::Amber
    } else {
        Health::Green
    }
}

pub fn is_leak(rss_bytes: u64, growth_bytes: i64, cfg: &Config) -> bool {
    leak(rss_bytes, growth_bytes, cfg).is_some()
}

/// What makes a server leak.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LeakCause {
    /// Above `leak_memory_mb`.
    Size,
    /// Grew by more than `leak_growth_mb` in 10 minutes (wins over `Size`).
    Growth,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Leak {
    pub rss_bytes: u64,
    pub growth_bytes: i64,
    pub cause: LeakCause,
}

/// The leak of a server with this memory and growth, if it leaks.
pub fn leak(rss_bytes: u64, growth_bytes: i64, cfg: &Config) -> Option<Leak> {
    let cause = if growth_bytes > cfg.leak_growth_mb.saturating_mul(MB) as i64 {
        LeakCause::Growth
    } else if rss_bytes > cfg.leak_memory_mb.saturating_mul(MB) {
        LeakCause::Size
    } else {
        return None;
    };
    Some(Leak {
        rss_bytes,
        growth_bytes,
        cause,
    })
}

/// The leak alert of one server.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LeakState {
    /// The notification of the current leak went out.
    pub notified: bool,
    /// "Ignore this alert": the memory at that moment. The alert stays
    /// dismissed until memory grows `LEAK_REARM_MB` more.
    pub ignored_at_rss: Option<u64>,
}

/// What one pass decided about a server's leak alert.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LeakUpdate {
    /// The leak its card shows (a leak that is not ignored).
    pub shown: Option<Leak>,
    /// Report it for a notification (shown and not notified yet).
    pub notify: bool,
    /// An ignored alert came back this pass.
    pub rearmed: bool,
}

/// The leak alerts of every server, in memory only. One notification per
/// leak: a leak stays notified while its server lives, unless it was
/// ignored and memory then grew `LEAK_REARM_MB` more.
#[derive(Debug, Default)]
pub struct LeakAlerts {
    states: HashMap<ServerKey, LeakState>,
    shown: HashSet<ServerKey>,
}

impl LeakAlerts {
    /// One pass for a server with memory `rss_bytes`, leaking or not.
    pub fn update(&mut self, key: ServerKey, rss_bytes: u64, leak: Option<Leak>) -> LeakUpdate {
        let st = self.states.entry(key).or_default();
        let mut rearmed = false;
        if let Some(at) = st.ignored_at_rss {
            if rss_bytes >= at.saturating_add(LEAK_REARM_MB * MB) {
                *st = LeakState::default();
                rearmed = true;
            }
        }
        let shown = leak.filter(|_| st.ignored_at_rss.is_none());
        if shown.is_some() {
            self.shown.insert(key);
        } else {
            self.shown.remove(&key);
        }
        LeakUpdate {
            shown,
            notify: shown.is_some() && !st.notified,
            rearmed,
        }
    }

    pub fn state(&self, key: ServerKey) -> LeakState {
        self.states.get(&key).copied().unwrap_or_default()
    }

    /// The last pass showed a leak card for this server.
    pub fn is_shown(&self, key: ServerKey) -> bool {
        self.shown.contains(&key)
    }

    pub fn mark_notified(&mut self, key: ServerKey) {
        self.states.entry(key).or_default().notified = true;
    }

    /// Dismisses the current alert at memory `rss_bytes`.
    pub fn ignore(&mut self, key: ServerKey, rss_bytes: u64) {
        self.states.entry(key).or_default().ignored_at_rss = Some(rss_bytes);
        self.shown.remove(&key);
    }

    pub fn retain(&mut self, live: &HashSet<ServerKey>) {
        self.states.retain(|k, _| live.contains(k));
        self.shown.retain(|k| live.contains(k));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(pid: i32) -> ServerKey {
        ServerKey { pid, start_time: 1 }
    }

    fn res(rss: u64, cpu_ns: u64) -> ProcResources {
        ProcResources {
            rss_bytes: rss,
            cpu_time_ns: cpu_ns,
        }
    }

    fn mb(n: u64) -> u64 {
        n * MB
    }

    #[test]
    fn health_thresholds() {
        let c = Config::default();
        assert_eq!(health(mb(499), 0, &c), Health::Green);
        assert_eq!(health(mb(500), 0, &c), Health::Amber);
        assert_eq!(health(mb(2048), 0, &c), Health::Amber);
        assert_eq!(health(mb(2049), 0, &c), Health::Red);
        assert_eq!(health(mb(100), mb(501) as i64, &c), Health::Red);
        assert_eq!(health(mb(100), mb(500) as i64, &c), Health::Green);
        // Red follows the configured leak size.
        let c = Config {
            leak_memory_mb: 1024,
            ..Config::default()
        };
        assert_eq!(health(mb(1025), 0, &c), Health::Red);
        assert_eq!(health(mb(1024), 0, &c), Health::Amber);
    }

    #[test]
    fn leak_cause_growth_wins() {
        let c = Config::default();
        assert_eq!(leak(mb(100), 0, &c), None);
        assert_eq!(leak(mb(3000), 0, &c).unwrap().cause, LeakCause::Size);
        assert_eq!(
            leak(mb(100), mb(600) as i64, &c).unwrap().cause,
            LeakCause::Growth
        );
        assert_eq!(
            leak(mb(3000), mb(600) as i64, &c).unwrap().cause,
            LeakCause::Growth
        );
    }

    #[test]
    fn leak_alert_notified_once_ignored_then_rearmed() {
        let c = Config::default();
        let mut a = LeakAlerts::default();
        let k = key(1);
        let big = |r| leak(r, 0, &c);
        let u = a.update(k, mb(3000), big(mb(3000)));
        assert!(u.notify && u.shown.is_some() && !u.rearmed);
        assert!(a.update(k, mb(3000), big(mb(3000))).notify);
        a.mark_notified(k);
        let u = a.update(k, mb(3000), big(mb(3000)));
        assert!(!u.notify && u.shown.is_some());
        a.ignore(k, mb(3000));
        assert!(!a.is_shown(k));
        let u = a.update(k, mb(3499), big(mb(3499)));
        assert_eq!(u, LeakUpdate::default());
        let u = a.update(k, mb(3500), big(mb(3500)));
        assert!(u.rearmed && u.notify && u.shown.is_some());
        assert_eq!(a.state(k), LeakState::default());
        a.retain(&HashSet::new());
        assert!(!a.is_shown(k));
    }

    #[test]
    fn leak_boundaries() {
        let mut c = Config::default();
        assert!(!is_leak(mb(2048), 0, &c));
        assert!(is_leak(mb(2048) + 1, 0, &c));
        assert!(!is_leak(0, mb(500) as i64, &c));
        assert!(is_leak(0, mb(500) as i64 + 1, &c));
        c.leak_memory_mb = 4096;
        assert!(!is_leak(mb(3000), 0, &c));
        assert!(is_leak(mb(4097), 0, &c));
    }

    #[test]
    fn growth_over_window_with_sparse_samples() {
        let mut h = History::default();
        let k = key(1);
        let min = 60_000;
        for (t, rss) in [(0, 1024), (5 * min, 1229), (12 * min, 1741)] {
            h.record(k, t, &[((1, 1), res(mb(rss), 0))]);
        }
        // Window starts at 2 min: oldest sample at or after it is t=5 min.
        assert_eq!(h.growth_bytes(k, 12 * min), mb(1741 - 1229) as i64);
        assert_eq!(h.growth_bytes(key(9), 12 * min), 0);
    }

    #[test]
    fn growth_needs_two_samples() {
        let mut h = History::default();
        h.record(key(1), 1000, &[((1, 1), res(mb(10), 0))]);
        assert_eq!(h.growth_bytes(key(1), 1000), 0);
    }

    #[test]
    fn history_pruned_after_two_hours() {
        let mut h = History::default();
        let k = key(1);
        h.record(k, 0, &[((1, 1), res(1, 0))]);
        h.push_event(
            k,
            ServerEvent {
                at_ms: 0,
                kind: EventKind::Restarted,
            },
        );
        h.record(k, HISTORY_MS, &[((1, 1), res(2, 0))]);
        let live = HashSet::from([k]);
        h.retain(&live, HISTORY_MS);
        assert_eq!(h.samples(k).len(), 2);
        h.retain(&live, HISTORY_MS + 1);
        assert_eq!(h.samples(k).len(), 1);
        assert!(h.events(k).is_empty());
    }

    #[test]
    fn retain_drops_dead_keys() {
        let mut h = History::default();
        h.record(key(1), 0, &[]);
        h.record(key(2), 0, &[]);
        h.retain(&HashSet::from([key(2)]), 0);
        assert!(h.samples(key(1)).is_empty());
        assert_eq!(h.samples(key(2)).len(), 1);
        assert!(h.track(key(1)));
        assert!(!h.track(key(2)));
    }

    #[test]
    fn cpu_pct_from_tree_time_delta() {
        let mut h = History::default();
        let k = key(1);
        let tree = |cpu| vec![((1, 1), res(10, cpu)), ((2, 1), res(10, cpu))];
        h.record(k, 0, &tree(1_000_000_000));
        h.record(k, 1000, &tree(1_500_000_000));
        assert_eq!(h.samples(k)[0].cpu_pct, 0.0);
        assert_eq!(h.samples(k)[1].cpu_pct, 100.0);
    }

    #[test]
    fn tree_membership_change_keeps_history() {
        let mut h = History::default();
        let k = key(1);
        h.record(
            k,
            0,
            &[
                ((1, 1), res(mb(100), 5_000_000_000)),
                ((2, 1), res(mb(50), 9_000_000_000)),
            ],
        );
        // Worker 2 left (its CPU time is gone), worker 3 joined with a big
        // CPU counter: neither may change the CPU figure.
        h.record(
            k,
            1000,
            &[
                ((1, 1), res(mb(100), 5_100_000_000)),
                ((3, 1), res(mb(70), 8_000_000_000)),
            ],
        );
        let s = h.samples(k);
        assert_eq!(s.len(), 2);
        assert!((s[1].cpu_pct - 10.0).abs() < 0.01, "{}", s[1].cpu_pct);
        assert!(s[1].cpu_pct >= 0.0);
        assert_eq!(h.growth_bytes(k, 1000), mb(170) as i64 - mb(150) as i64);
    }

    #[test]
    fn connection_transitions_emit_events() {
        let mut h = History::default();
        let k = key(1);
        for (t, n) in [(0, 0), (1, 2), (2, 3), (3, 0)] {
            h.note_connections(k, t, n);
        }
        let kinds: Vec<_> = h.events(k).iter().map(|e| e.kind.clone()).collect();
        assert_eq!(
            kinds,
            vec![EventKind::BrowserConnected, EventKind::BrowserClosed]
        );
    }

    #[test]
    fn alert_pushed_once_even_after_pruning() {
        let mut h = History::default();
        let k = key(1);
        assert!(h.note_alert(k, 0, AlertKind::Idle));
        assert!(!h.note_alert(k, 1, AlertKind::Idle));
        assert!(h.note_alert(k, 2, AlertKind::Leak));
        h.record(k, HISTORY_MS + 5, &[]);
        h.retain(&HashSet::from([k]), HISTORY_MS + 5);
        assert!(h.events(k).is_empty());
        assert!(!h.note_alert(k, HISTORY_MS + 6, AlertKind::Idle));
        assert!(h.events(k).is_empty());
        h.reset_alert(k, AlertKind::Idle);
        assert!(h.note_alert(k, HISTORY_MS + 7, AlertKind::Idle));
    }

    #[test]
    fn same_instant_and_new_process_add_no_cpu() {
        let mut h = History::default();
        let k = key(1);
        h.record(k, 1000, &[((1, 1), res(1, 1_000_000_000))]);
        h.record(k, 1000, &[((1, 1), res(1, 2_000_000_000))]);
        assert_eq!(h.samples(k)[1].cpu_pct, 0.0);
        // Process 2 was absent last pass: only process 1's delta counts.
        h.record(
            k,
            2000,
            &[
                ((1, 1), res(1, 2_100_000_000)),
                ((2, 1), res(1, 9_000_000_000)),
            ],
        );
        assert!((h.samples(k)[2].cpu_pct - 10.0).abs() < 0.01);
    }

    #[test]
    fn session_ended_once() {
        let mut h = History::default();
        assert!(h.note_session_ended(key(1), 5));
        assert!(!h.note_session_ended(key(1), 6));
        assert_eq!(h.events(key(1)).len(), 1);
    }
}
