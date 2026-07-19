//! Per-agent cache countdown timer: pure math + the working/stopped decision.
//!
//! No herdr or platform types — unit-tested in isolation. The daemon owns the
//! per-pane `TimerState` map and calls these on each sample.

use std::time::Instant;

/// Default cache/attention window in minutes.
pub const DEFAULT_CACHE_MINUTES: u64 = 60;

/// herdr `agent_status` values that mean the agent is ACTIVELY working — its
/// prompt cache is refreshed each turn, so no countdown is shown. Every other
/// value (idle, blocked, waiting-for-input, done, unknown) counts down.
/// Confirmed live: an idle `claude` agent reports "idle"; the working string is
/// confirmed during E2E (Task 6) and this set adjusted if herdr differs.
pub const WORKING_STATES: &[&str] = &["working", "running", "busy", "active", "thinking"];

/// Per-pane countdown state. `reset_at` is the instant the current 60m window
/// began (refreshed to "now" every sample the agent is working).
pub struct TimerState {
    pub reset_at: Instant,
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
    ((remaining + 59) / 60).min(total_minutes)
}

/// The `$cache` token text for a `claude` pane, or `None` to suppress it (the
/// agent is working). `"42m"` while counting down; `"⚠ 0m"` at expiry — the
/// icon carries the alert since herdr token colour is static per config.
pub fn cache_token(
    working: bool,
    reset_at: Instant,
    now: Instant,
    total_minutes: u64,
) -> Option<String> {
    if working {
        return None;
    }
    let m = remaining_minutes(reset_at, now, total_minutes);
    Some(if m == 0 {
        "⚠ 0m".to_string()
    } else {
        format!("{m}m")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn is_working_true_only_for_working_states() {
        assert!(is_working(Some("working")));
        assert!(is_working(Some("running")));
        assert!(!is_working(Some("idle")));
        assert!(!is_working(Some("unknown")));
        assert!(!is_working(Some("blocked")));
        assert!(!is_working(None));
    }

    #[test]
    fn remaining_is_full_at_reset_and_within_first_minute() {
        let now = Instant::now();
        assert_eq!(remaining_minutes(now, now, 60), 60);
        // 30s in still reads a full 60 (ceil to whole minutes).
        assert_eq!(remaining_minutes(now - Duration::from_secs(30), now, 60), 60);
    }

    #[test]
    fn remaining_ceils_to_whole_minutes() {
        let now = Instant::now();
        // 60s elapsed -> 3540s left -> 59m.
        assert_eq!(remaining_minutes(now - Duration::from_secs(60), now, 60), 59);
        // 59m elapsed -> 60s left -> 1m.
        assert_eq!(remaining_minutes(now - Duration::from_secs(3540), now, 60), 1);
    }

    #[test]
    fn remaining_clamps_to_zero_past_expiry() {
        let now = Instant::now();
        assert_eq!(remaining_minutes(now - Duration::from_secs(3600), now, 60), 0);
        assert_eq!(remaining_minutes(now - Duration::from_secs(9999), now, 60), 0);
    }

    #[test]
    fn on_sample_pins_reset_while_working_and_holds_while_stopped() {
        let now = Instant::now();
        let mut state = TimerState { reset_at: now - Duration::from_secs(100) };
        // Working: reset_at snaps forward to now.
        on_sample(&mut state, true, now);
        assert_eq!(state.reset_at, now);
        // Stopped: reset_at is left where it is (countdown keeps running).
        let later = now + Duration::from_secs(10);
        on_sample(&mut state, false, later);
        assert_eq!(state.reset_at, now);
    }

    #[test]
    fn cache_token_suppresses_while_working() {
        let now = Instant::now();
        assert_eq!(cache_token(true, now, now, 60), None);
    }

    #[test]
    fn cache_token_shows_minutes_then_alert_at_zero() {
        let now = Instant::now();
        assert_eq!(cache_token(false, now, now, 60), Some("60m".to_string()));
        assert_eq!(
            cache_token(false, now - Duration::from_secs(3600), now, 60),
            Some("⚠ 0m".to_string())
        );
    }

    #[test]
    fn cache_token_honours_a_custom_total() {
        let now = Instant::now();
        assert_eq!(cache_token(false, now, now, 30), Some("30m".to_string()));
    }
}
