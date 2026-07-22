//! Per-agent cache countdown timer: pure math + the working/stopped decision.
//!
//! No herdr or platform types — unit-tested in isolation. The daemon owns the
//! per-pane `TimerState` map and calls these on each sample.

use std::time::Instant;

/// Default cache/attention window in minutes.
pub const DEFAULT_CACHE_MINUTES: u64 = 60;

/// herdr `agent_status` values that mean the agent is ACTIVELY working — its
/// prompt cache is refreshed each turn, so no countdown is shown. Every other
/// value counts down. The full herdr `AgentStatus` enum is
/// idle/working/blocked/done/unknown (confirmed via `herdr api schema` on
/// 0.7.4), so `working` is the only active state; idle/blocked/done/unknown all
/// count down.
pub const WORKING_STATES: &[&str] = &["working"];

/// herdr `agent_status` values that mean the agent is WAITING on the user —
/// `blocked` (stuck, needs input) and `done` (finished, awaiting the next
/// prompt). Surfaced as the window-title "N waiting" count. `idle`/`unknown`
/// are not counted: the agent isn't demanding attention.
pub const ATTENTION_STATES: &[&str] = &["blocked", "done"];

/// Whether `status` means the agent is waiting on the user (counted by the
/// window-title "waiting" tally). An absent status counts as not waiting.
pub fn is_waiting(status: Option<&str>) -> bool {
    match status {
        Some(s) => ATTENTION_STATES.contains(&s),
        None => false,
    }
}

/// Per-pane countdown state. `reset_at` is the instant the current window began
/// (refreshed every sample the agent is working). `alerted` debounces the alert
/// sound: set when it fires, cleared whenever the tier leaves Alert.
pub struct TimerState {
    pub reset_at: Instant,
    pub alerted: bool,
}

/// Whether `status` means the agent is actively working (countdown suppressed).
/// An absent status counts as stopped.
pub fn is_working(status: Option<&str>) -> bool {
    match status {
        Some(s) => WORKING_STATES.contains(&s),
        None => false,
    }
}

/// Update on each sample. While working, pin `reset_at` to `now` so the window
/// stays full; while stopped, leave it so the countdown keeps ticking. This
/// means the instant the agent stops, `reset_at` is ≈ now → a full window.
pub fn on_sample(state: &mut TimerState, working: bool, now: Instant) {
    if working {
        state.reset_at = now;
    }
}

/// Whole minutes left, ceil-divided so a fresh timer reads `total_minutes` and
/// only reaches 0 at true expiry; clamped to `0..=total_minutes`.
pub fn remaining_minutes(reset_at: Instant, now: Instant, total_minutes: u64) -> u64 {
    let elapsed = now.saturating_duration_since(reset_at).as_secs();
    let total_secs = total_minutes.saturating_mul(60);
    let remaining = total_secs.saturating_sub(elapsed);
    // Ceil to whole minutes: (remaining + 59) / 60.
    remaining.div_ceil(60).min(total_minutes)
}

/// Color/urgency tier of a countdown, driving which token name carries the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Normal,
    Warn,
    Alert,
}

/// The herdr token keys the cache value can be published under, one per tier —
/// used to clear the inactive tiers and to sweep every key on cleanup.
pub const CACHE_TOKEN_KEYS: &[&str] = &["cache", "cache_warn", "cache_alert"];

/// Tier from whole minutes remaining and the (already-clamped, warn >= alert)
/// thresholds: `<= alert_minutes` → Alert, else `<= warn_minutes` → Warn, else
/// Normal.
pub fn tier(remaining_minutes: u64, warn_minutes: u64, alert_minutes: u64) -> Tier {
    if remaining_minutes <= alert_minutes {
        Tier::Alert
    } else if remaining_minutes <= warn_minutes {
        Tier::Warn
    } else {
        Tier::Normal
    }
}

/// herdr token key for a tier. The user styles each key's colour in herdr config.
pub fn tier_token_key(t: Tier) -> &'static str {
    match t {
        Tier::Normal => "cache",
        Tier::Warn => "cache_warn",
        Tier::Alert => "cache_alert",
    }
}

/// The displayed token value: `label` prefix + minutes, e.g. `"cache 42m"` or
/// `"⏳ 42m"`. An empty `label` renders just `"42m"`. Colour (not an icon)
/// carries the alert, so there is no `⚠`.
pub fn cache_label(label: &str, remaining_minutes: u64) -> String {
    if label.is_empty() {
        format!("{remaining_minutes}m")
    } else {
        format!("{label} {remaining_minutes}m")
    }
}

/// Whether to play the alert sound on this sample, updating the debounce flag.
/// True exactly on the transition into Alert; any non-Alert tier re-arms.
pub fn should_alert(t: Tier, alerted: &mut bool) -> bool {
    match t {
        Tier::Alert if !*alerted => {
            *alerted = true;
            true
        }
        Tier::Alert => false,
        _ => {
            *alerted = false;
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn is_working_true_only_for_working_states() {
        // herdr AgentStatus enum: only "working" is active; the rest count down.
        assert!(is_working(Some("working")));
        assert!(!is_working(Some("idle")));
        assert!(!is_working(Some("blocked")));
        assert!(!is_working(Some("done")));
        assert!(!is_working(Some("unknown")));
        // A value outside herdr's enum is treated as stopped, not working.
        assert!(!is_working(Some("running")));
        assert!(!is_working(None));
    }

    #[test]
    fn is_waiting_true_only_for_blocked_and_done() {
        assert!(is_waiting(Some("blocked")));
        assert!(is_waiting(Some("done")));
        assert!(!is_waiting(Some("working")));
        assert!(!is_waiting(Some("idle")));
        assert!(!is_waiting(Some("unknown")));
        assert!(!is_waiting(Some("running")));
        assert!(!is_waiting(None));
    }

    #[test]
    fn remaining_is_full_at_reset_and_within_first_minute() {
        let now = Instant::now();
        assert_eq!(remaining_minutes(now, now, 60), 60);
        // 30s in still reads a full 60 (ceil to whole minutes).
        assert_eq!(
            remaining_minutes(now - Duration::from_secs(30), now, 60),
            60
        );
    }

    #[test]
    fn remaining_ceils_to_whole_minutes() {
        let now = Instant::now();
        // 60s elapsed -> 3540s left -> 59m.
        assert_eq!(
            remaining_minutes(now - Duration::from_secs(60), now, 60),
            59
        );
        // 59m elapsed -> 60s left -> 1m.
        assert_eq!(
            remaining_minutes(now - Duration::from_secs(3540), now, 60),
            1
        );
    }

    #[test]
    fn remaining_clamps_to_zero_past_expiry() {
        let now = Instant::now();
        assert_eq!(
            remaining_minutes(now - Duration::from_secs(3600), now, 60),
            0
        );
        assert_eq!(
            remaining_minutes(now - Duration::from_secs(9999), now, 60),
            0
        );
    }

    #[test]
    fn on_sample_pins_reset_while_working_and_holds_while_stopped() {
        let now = Instant::now();
        let mut state = TimerState {
            reset_at: now - Duration::from_secs(100),
            alerted: false,
        };
        // Working: reset_at snaps forward to now.
        on_sample(&mut state, true, now);
        assert_eq!(state.reset_at, now);
        // Stopped: reset_at is left where it is (countdown keeps running).
        let later = now + Duration::from_secs(10);
        on_sample(&mut state, false, later);
        assert_eq!(state.reset_at, now);
    }

    #[test]
    fn tier_thresholds_are_inclusive_lower_bounds() {
        assert_eq!(tier(31, 30, 10), Tier::Normal);
        assert_eq!(tier(30, 30, 10), Tier::Warn);
        assert_eq!(tier(11, 30, 10), Tier::Warn);
        assert_eq!(tier(10, 30, 10), Tier::Alert);
        assert_eq!(tier(0, 30, 10), Tier::Alert);
    }

    #[test]
    fn tier_handles_warn_equal_alert() {
        assert_eq!(tier(11, 10, 10), Tier::Normal);
        assert_eq!(tier(10, 10, 10), Tier::Alert);
    }

    #[test]
    fn tier_token_key_maps_each_tier() {
        assert_eq!(tier_token_key(Tier::Normal), "cache");
        assert_eq!(tier_token_key(Tier::Warn), "cache_warn");
        assert_eq!(tier_token_key(Tier::Alert), "cache_alert");
        for t in [Tier::Normal, Tier::Warn, Tier::Alert] {
            assert!(CACHE_TOKEN_KEYS.contains(&tier_token_key(t)));
        }
    }

    #[test]
    fn cache_label_prefixes_label_and_covers_zero() {
        assert_eq!(cache_label("cache", 42), "cache 42m");
        assert_eq!(cache_label("cache", 0), "cache 0m");
        assert_eq!(cache_label("⏳", 42), "⏳ 42m");
        // Empty label → bare minutes.
        assert_eq!(cache_label("", 42), "42m");
        assert_eq!(cache_label("", 0), "0m");
    }

    #[test]
    fn should_alert_fires_once_per_episode_and_rearms() {
        let mut alerted = false;
        assert!(should_alert(Tier::Alert, &mut alerted));
        assert!(!should_alert(Tier::Alert, &mut alerted));
        assert!(!should_alert(Tier::Warn, &mut alerted));
        assert!(!should_alert(Tier::Normal, &mut alerted));
        assert!(should_alert(Tier::Alert, &mut alerted));
    }
}
