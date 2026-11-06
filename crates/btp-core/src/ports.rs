//! Alert state machine for machine-wide ephemeral port exhaustion.
//!
//! Pure logic, no I/O: the caller feeds the latest [`PortPressure`] and a
//! monotonic clock, and gets back what to show and whether to notify.

use crate::config::Config;
use crate::platform::PortPressure;

/// How long usage must stay below the clear line before the alert re-arms.
pub const PORT_CLEAR_MS: u64 = 60_000;
/// The alert clears below `warn - PORT_HYSTERESIS_PCT`.
pub const PORT_HYSTERESIS_PCT: u64 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PortLevel {
    Warning,
    Critical,
}

/// Result of one [`PortAlert::update`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortUpdate {
    /// Level to display, `None` when there is no alert or it was ignored.
    pub shown: Option<PortLevel>,
    /// True when a notification should be sent now.
    pub notify: bool,
}

/// Tracks one port exhaustion alert from first crossing to re-arm.
///
/// The alert starts at `used_pct >= warn` and ends once `used_pct` stays below
/// `warn - 10` for [`PORT_CLEAR_MS`]. While it lasts, the level follows the
/// current percentage (Critical at `>= critical`, else Warning), so dropping
/// from Critical back between the two thresholds shows Warning again. While
/// the clear timer runs, the last level stays shown. Notification bookkeeping
/// uses the highest level already notified: reaching a higher level notifies
/// once more and lifts an Ignore.
#[derive(Debug, Default)]
pub struct PortAlert {
    active: bool,
    level: Option<PortLevel>,
    notified_level: Option<PortLevel>,
    ignored: bool,
    clear_since_ms: Option<u64>,
}

impl PortAlert {
    pub fn update(&mut self, p: Option<&PortPressure>, cfg: &Config, now_ms: u64) -> PortUpdate {
        let pct = p.map_or(0, |p| u64::from(p.used_pct()));
        let clear_below = cfg
            .port_pressure_warn_pct
            .saturating_sub(PORT_HYSTERESIS_PCT);

        if !self.active {
            if pct < cfg.port_pressure_warn_pct {
                return PortUpdate {
                    shown: None,
                    notify: false,
                };
            }
            self.active = true;
            self.clear_since_ms = None;
        }

        if pct < clear_below {
            let since = *self.clear_since_ms.get_or_insert(now_ms);
            if now_ms.saturating_sub(since) >= PORT_CLEAR_MS {
                *self = Self::default();
                return PortUpdate {
                    shown: None,
                    notify: false,
                };
            }
        } else {
            self.clear_since_ms = None;
            self.level = Some(if pct >= cfg.port_pressure_critical_pct {
                PortLevel::Critical
            } else {
                PortLevel::Warning
            });
        }

        let level = self.level;
        if level > self.notified_level {
            // A higher level than any notified so far lifts an Ignore.
            if self.notified_level.is_some() {
                self.ignored = false;
            }
        }
        if self.ignored {
            return PortUpdate {
                shown: None,
                notify: false,
            };
        }
        PortUpdate {
            shown: level,
            notify: level > self.notified_level,
        }
    }

    /// The notification for the current level was delivered.
    pub fn mark_notified(&mut self) {
        self.notified_level = self.notified_level.max(self.level);
    }

    /// Hide the alert until it clears or reaches a higher level. Without an
    /// active alert (a stale card), nothing changes.
    pub fn ignore(&mut self) {
        if !self.active {
            return;
        }
        self.notified_level = self.notified_level.max(self.level);
        self.ignored = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pct: u32) -> PortPressure {
        PortPressure {
            range_size: 100,
            in_use: pct,
            time_wait: 0,
            top: None,
        }
    }

    fn up(a: &mut PortAlert, pct: u32, t_s: u64) -> PortUpdate {
        a.update(Some(&p(pct)), &Config::default(), t_s * 1000)
    }

    #[test]
    fn starts_at_warn_and_notifies_once() {
        let mut a = PortAlert::default();
        assert_eq!(up(&mut a, 49, 0).shown, None);
        let u = up(&mut a, 50, 5);
        assert_eq!((u.shown, u.notify), (Some(PortLevel::Warning), true));
        a.mark_notified();
        assert!(!up(&mut a, 60, 10).notify);
    }

    #[test]
    fn hysteresis_one_notification() {
        let mut a = PortAlert::default();
        let mut notes = 0;
        for (i, pct) in [52, 48, 51, 45, 52].into_iter().enumerate() {
            let u = up(&mut a, pct, i as u64 * 5);
            assert!(u.shown.is_some());
            if u.notify {
                notes += 1;
                a.mark_notified();
            }
        }
        assert_eq!(notes, 1);
    }

    #[test]
    fn clears_after_60s_below_40() {
        let mut a = PortAlert::default();
        up(&mut a, 50, 0);
        a.mark_notified();
        assert!(up(&mut a, 39, 100).shown.is_some());
        assert!(up(&mut a, 39, 159).shown.is_some());
        assert_eq!(up(&mut a, 39, 160).shown, None);
        let u = up(&mut a, 50, 170);
        assert_eq!((u.shown, u.notify), (Some(PortLevel::Warning), true));
    }

    #[test]
    fn clear_timer_resets_on_rebound() {
        let mut a = PortAlert::default();
        up(&mut a, 50, 0);
        up(&mut a, 39, 10);
        up(&mut a, 45, 50);
        up(&mut a, 39, 60);
        assert!(up(&mut a, 39, 119).shown.is_some());
        assert_eq!(up(&mut a, 39, 120).shown, None);
    }

    #[test]
    fn critical_renotifies_and_lifts_ignore() {
        let mut a = PortAlert::default();
        up(&mut a, 50, 0);
        a.mark_notified();
        a.ignore();
        assert_eq!(up(&mut a, 55, 5).shown, None);
        let u = up(&mut a, 86, 10);
        assert_eq!((u.shown, u.notify), (Some(PortLevel::Critical), true));
    }

    #[test]
    fn critical_drop_shows_warning_without_renotify() {
        let mut a = PortAlert::default();
        up(&mut a, 90, 0);
        a.mark_notified();
        let u = up(&mut a, 60, 5);
        assert_eq!((u.shown, u.notify), (Some(PortLevel::Warning), false));
    }

    #[test]
    fn ignore_holds_until_clear() {
        let mut a = PortAlert::default();
        up(&mut a, 55, 0);
        a.ignore();
        assert_eq!(up(&mut a, 70, 5).shown, None);
        up(&mut a, 10, 10);
        assert_eq!(up(&mut a, 10, 70).shown, None);
        let u = up(&mut a, 50, 80);
        assert_eq!((u.shown, u.notify), (Some(PortLevel::Warning), true));
    }

    #[test]
    fn ignore_when_inactive_is_noop() {
        // An Ignore on a stale card, after the alert cleared, must not
        // suppress the next one.
        let mut a = PortAlert::default();
        a.ignore();
        let u = up(&mut a, 50, 0);
        assert_eq!((u.shown, u.notify), (Some(PortLevel::Warning), true));
    }

    #[test]
    fn none_pressure_counts_as_clear() {
        let mut a = PortAlert::default();
        let cfg = Config::default();
        a.update(Some(&p(60)), &cfg, 0);
        assert!(a.update(None, &cfg, 1000).shown.is_some());
        assert_eq!(a.update(None, &cfg, 61_000).shown, None);
        assert_eq!(a.update(None, &cfg, 62_000).shown, None);
    }
}
