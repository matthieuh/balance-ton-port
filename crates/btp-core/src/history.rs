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

/// Memory above which a server is red whatever its growth.
const RED_MEMORY_MB: u64 = 2048;
const AMBER_MEMORY_MB: u64 = 500;

/// One process of a tree: (pid, start time), so a reused pid is a new process.
pub type ProcId = (i32, u64);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub at_ms: u64,
    pub rss_bytes: u64,
    /// 100 = one core fully used by the whole tree.
    pub cpu_pct: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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

pub fn health(rss_bytes: u64, growth_bytes: i64, cfg: &Config) -> Health {
    if rss_bytes > RED_MEMORY_MB * MB || growth_bytes > (cfg.leak_growth_mb * MB) as i64 {
        Health::Red
    } else if rss_bytes >= AMBER_MEMORY_MB * MB {
        Health::Amber
    } else {
        Health::Green
    }
}

pub fn is_leak(rss_bytes: u64, growth_bytes: i64, cfg: &Config) -> bool {
    rss_bytes > cfg.leak_memory_mb * MB || growth_bytes > (cfg.leak_growth_mb * MB) as i64
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
    fn session_ended_once() {
        let mut h = History::default();
        assert!(h.note_session_ended(key(1), 5));
        assert!(!h.note_session_ended(key(1), 6));
        assert_eq!(h.events(key(1)).len(), 1);
    }
}
