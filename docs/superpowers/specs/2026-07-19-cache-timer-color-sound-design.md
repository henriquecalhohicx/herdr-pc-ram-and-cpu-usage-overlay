# Cache Timer — Layout, Tiered Color, Alert Sound — Design

**Date:** 2026-07-19
**Status:** Approved in brainstorm; ready for implementation plan.
**Builds on:** the per-agent cache countdown timer
(`docs/superpowers/specs/2026-07-19-agent-attention-timer-design.md`, shipped in
`src/timer.rs` + `push_cache_tokens` in `src/daemon.rs`). Read that first.

## Goal

Three enhancements to the shipped `$cache` countdown:

1. **Layout:** show cpu/ram *and* cache on the claude agent's own entry
   (`claude · cpu 1% · ram 2% · cache 60m`), instead of cpu/ram on a separate
   `usage` pseudo-agent entry.
2. **Tiered color:** the cache value turns yellow at `<= warn_minutes` and red at
   `<= alert_minutes`.
3. **Alert sound:** crossing into the alert tier plays a configurable `.wav`
   once (Windows).

Thresholds and the sound path are configurable in the plugin `config.toml`;
colors live in herdr's config (herdr owns rendering).

## Context this builds on

- The daemon polls each space every `interval_seconds` and calls
  `push_cache_tokens` (`src/daemon.rs`), which pushes a TTL'd `cache` pane
  metadata token onto each claude pane. Per-pane `timer::TimerState { reset_at }`
  lives in an in-memory `HashMap<pane_id, TimerState>` owned by `run_daemon`,
  pruned each loop.
- `timer.rs` is pure and unit-tested: `is_working`, `on_sample`,
  `remaining_minutes`, `cache_token`, `WORKING_STATES = ["working"]`,
  `DEFAULT_CACHE_MINUTES = 60`.
- The space's cpu/ram text is `status_line(sp, labels)` → `"cpu X% · ram Y%"`.
- herdr **agent detection is process-based** (`agent=claude` while `claude` is
  the pane's live foreground process). herdr **rejects** a `report_agent` claim
  over a pane with a detected agent. The daemon guard
  (`collect::pane_has_agent_glyph`, `Space.masked_pseudo_panes`) already releases
  any stale `usage` claim masking a real agent.
- herdr token color is **static per config**: a plugin sets only a token's
  *value*, never its color. herdr supports **inline token styles** in sidebar
  rows: `{ token = "$cache", fg = "#a6e3a1", bold = true, dim = false }` (fg is
  strict `#RGB`/`#RRGGBB`; bold/dim booleans). Verified via `herdr --default-config`.

## Decisions (settled in brainstorm)

1. **Merge onto the agent line.** herdr row
   `["agent","$usage","$cache","$cache_warn","$cache_alert"]` renders
   `claude · cpu 1% · ram 2% · cache 60m` on one line.
2. **Suppress the `usage` pseudo-agent for spaces that have a claude agent.**
   cpu/ram rides the claude line; spaces without a claude agent keep the
   pseudo-agent exactly as today.
3. **Two color tiers via token-swap.** `Normal` → `cache`, `Warn` → `cache_warn`,
   `Alert` → `cache_alert`. The active tier's token carries the value; the other
   two are cleared so herdr elides them.
4. **Sound once per expiry episode.** Play on the transition into the alert tier;
   re-arm only when the agent works again (remaining rises out of the alert tier).
   Windows only; Linux no-op. Daemon-restart-safe (a newly-seen pane starts at
   full → `Normal`, so no spurious sound).
5. **Drop the `⚠` glyph at zero** — red color now carries the alert; value is a
   plain `cache 0m`.
6. **Colors in herdr config, thresholds + wav path in plugin `config.toml`.**

## Design

### Token value + label

- The cache value is labeled: `format!("cache {m}m")` → `"cache 42m"`,
  `"cache 1m"`, `"cache 0m"`. No `⚠`.
- While the agent works, all three cache tokens are suppressed (cleared), exactly
  as `cache_token` returns `None` today.

### Tiers (pure, `src/timer.rs`)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier { Normal, Warn, Alert }

/// Tier from whole minutes remaining and the (already-clamped) thresholds.
/// `<= alert` → Alert; else `<= warn` → Warn; else Normal.
pub fn tier(remaining_minutes: u64, warn_minutes: u64, alert_minutes: u64) -> Tier;

/// herdr token key for a tier: Normal→"cache", Warn→"cache_warn",
/// Alert→"cache_alert".
pub fn tier_token_key(tier: Tier) -> &'static str;

/// All three cache token keys (for clearing the inactive tiers + cleanup).
pub const CACHE_TOKEN_KEYS: &[&str] = &["cache", "cache_warn", "cache_alert"];
```

- `cache_token` keeps returning the value string; a new pure helper decides the
  tier. The daemon maps tier → which key gets the value and which get cleared.
- Threshold clamping is pure and lives with config resolution (see Config):
  the daemon passes already-valid `warn >= alert >= 1`, `warn <= cache_minutes`.

### Sound arming (pure decision, `src/timer.rs`)

```rust
/// Whether to play the alert sound on this sample, updating the debounce flag.
/// Returns true exactly on the transition into the Alert tier; false otherwise.
/// Any non-Alert tier re-arms (sets alerted = false).
pub fn should_alert(tier: Tier, alerted: &mut bool) -> bool {
    match tier {
        Tier::Alert if !*alerted => { *alerted = true; true }
        Tier::Alert => false,
        _ => { *alerted = false; false }
    }
}
```

- `TimerState` gains `pub alerted: bool` (init `false`).

### Sound playback (`src/sound.rs`, new)

```rust
/// Play a .wav asynchronously, best-effort. Empty path or a missing file is a
/// no-op. Windows: winmm PlaySoundW(SND_FILENAME | SND_ASYNC). Other OSes: no-op.
pub fn play_wav(path: &str);
```

- `#[cfg(windows)]` implementation calls `PlaySoundW` (async, non-blocking, no
  console needed — the daemon runs detached).
- `#[cfg(not(windows))]` is an empty no-op so the Linux CI leg stays clean.
- Path checked non-empty and `Path::exists()` before calling; failures ignored.
- `Cargo.toml`: add `Win32_Media_Audio` to the `[target.'cfg(windows)']`
  `windows-sys` features (for `PlaySoundW`, `SND_FILENAME`, `SND_ASYNC`).

### Daemon wiring (`src/daemon.rs`)

`push_cache_tokens` (renamed conceptually to "push per-claude tokens"):

For each claude pane in each space:
- `working = is_working(status)`; update `TimerState.reset_at` via `on_sample`.
- If working → clear all `CACHE_TOKEN_KEYS` (suppress); leave `$usage` cleared too
  (the space still surfaces cpu/ram elsewhere only if no claude — see below).
  Re-arm handled by `should_alert` seeing a non-Alert tier next idle sample.
- Else (idle/stopped):
  - `remaining = remaining_minutes(reset_at, now, config.cache_minutes)`.
  - `t = tier(remaining, warn, alert)`.
  - Push the space's cpu/ram as the `usage` token on this pane
    (`status_line(sp, labels)`), so the claude line shows cpu/ram.
  - Push `"cache {remaining}m"` under `tier_token_key(t)`; clear the other two
    cache keys.
  - `if should_alert(t, &mut state.alerted) && !config.cache_alert_sound.is_empty()
    { sound::play_wav(&config.cache_alert_sound); }`
- Record touched panes in `tracked` for cleanup; prune `timers` to present panes.

`push_statuses` (the `usage` pseudo-agent / metadata path):
- **Skip a space that has any claude pane** — its cpu/ram is already on the claude
  line. Spaces with no claude pane behave exactly as today.

Cleanup (`clear_all`, `disable_updater` sweep):
- Clear every key in `CACHE_TOKEN_KEYS` (not just `"cache"`) plus `"usage"` on
  each tracked cache pane. TTL self-clears otherwise.

### Config (`src/config.rs`)

New `Config` fields (all `u64` / `String`), parsed like `interval_seconds`:

- `cache_warn_minutes: u64` — default `30`.
- `cache_alert_minutes: u64` — default `10`.
- `cache_alert_sound: String` — default `""` (empty = sound disabled).

Clamping (pure, applied after parse):
- `cache_alert_minutes = clamp(alert, 1, cache_minutes)`.
- `cache_warn_minutes = clamp(warn, alert, cache_minutes)` — warn never below
  alert, never above the full window. A misordered config self-corrects rather
  than erroring.

`config.toml` keys: `cache_warn_minutes`, `cache_alert_minutes`,
`cache_alert_sound`. Numeric keys reuse the `>= 1` numeric gate; the string key
is taken verbatim (quotes stripped, like other string values).

### herdr config (user side, documented in README)

```toml
[ui.sidebar.agents]
rows = [
    ["state_icon", "workspace", "tab"],
    [
        "agent",
        "$usage",
        { token = "$cache",       fg = "#a6e3a1" },            # green
        { token = "$cache_warn",  fg = "#f9e2af" },            # yellow
        { token = "$cache_alert", fg = "#f38ba8", bold = true }, # red
    ],
]
```

Only one of the three cache tokens is non-empty at a time, so herdr shows a
single colored `cache Nm`. Exact fg hex is the user's choice; document sensible
defaults.

## Testing / verification

- **Unit (`timer.rs`):** `tier` boundaries (`remaining > warn` → Normal;
  `== warn` → Warn; `== alert` → Alert; `0` → Alert; `warn == alert` edge);
  `tier_token_key` mapping; `should_alert` transitions (Normal→Alert fires once,
  stays quiet while Alert, re-arms on leaving Alert).
- **Unit (`config.rs`):** defaults (30/10/""), parsing, and the clamp
  (alert > cache_minutes clamps down; warn < alert bumps up to alert; warn >
  cache_minutes clamps down).
- **`sound.rs`:** empty-path and missing-file are no-ops (testable without audio
  on any OS); Windows playback verified live.
- **Live E2E (herdr 0.7.4, Windows):** claude agent idle → `claude · cpu · ram ·
  cache 60m` green; wait/adjust thresholds so it crosses `warn` (yellow) and
  `alert` (red) and the `.wav` plays once; confirm reset on work re-arms the
  sound; confirm a space without a claude agent still shows the `usage` entry.
- **CI matrix (ubuntu + windows) stays green;** `sound.rs` is `cfg`-gated so the
  Linux leg has only the no-op. `timer.rs` stays platform-agnostic.
