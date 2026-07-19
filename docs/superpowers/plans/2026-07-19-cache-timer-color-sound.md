# Cache Timer — Layout, Tiered Color, Alert Sound — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Put cpu/ram + a labeled countdown on the claude agent's own line, color the countdown by threshold (green→yellow→red) via herdr token-swap, and play a configurable `.wav` once when it enters the alert tier (Windows).

**Architecture:** All decisions stay in the pure `timer` module (tier + sound-arming) and a new `sound` module (winmm playback, cfg-gated). The daemon maps a claude pane's remaining minutes to one of three cache token names (`cache`/`cache_warn`/`cache_alert`), pushes the space's cpu/ram as `$usage` on the same pane, suppresses the separate `usage` pseudo-agent for spaces that have a claude agent, and fires the sound on the alert transition. Colors are set by the user in herdr config; thresholds + wav path in the plugin `config.toml`.

**Tech Stack:** Rust, herdr JSON-RPC over named pipe / unix socket, `windows-sys` winmm `PlaySoundW`. No new crates.

**Task order note:** Config (Task 1) and Sound (Task 2) are additive and compile on their own. The `timer` API change (removing `cache_token`, adding the `alerted` field) only compiles alongside its daemon consumer, so Task 3 makes that change **and** rewrites the daemon in one commit — every commit builds.

## Global Constraints

- **Layout:** claude entry shows `claude · cpu 1% · ram 2% · cache 60m` (herdr row `["agent","$usage","$cache","$cache_warn","$cache_alert"]`).
- **Suppress** the `usage` pseudo-agent for any space that has a claude agent; spaces without one are unchanged.
- **Cache value is labeled:** `"cache {m}m"` (e.g. `"cache 42m"`, `"cache 0m"`). No `⚠`.
- **Two color tiers via token-swap:** `Normal`→`cache`, `Warn` (`<= warn_minutes`)→`cache_warn`, `Alert` (`<= alert_minutes`)→`cache_alert`. Exactly one carries the value; the others are cleared.
- **Sound once per expiry episode:** fire on the transition into Alert; re-arm on any non-Alert sample. Windows only; Linux no-op. Skip if path empty or file missing.
- **Config (`config.toml`):** `cache_warn_minutes` (default 30), `cache_alert_minutes` (default 10), `cache_alert_sound` (default `""` = off). Clamp: `1 ≤ alert ≤ warn ≤ cache_minutes`.
- **Colors are herdr-side**, not in `config.toml`.
- **CI matrix (ubuntu + windows) green:** `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, `cargo build`. `timer.rs` stays `#[cfg]`-clean; `sound.rs` playback is `cfg(windows)` with a non-windows no-op.

### Build / run environment gotchas

- cargo PATH is stale — prepend `$env:USERPROFILE\.cargo\bin` before `cargo` (PowerShell) or `export PATH="$HOME/.cargo/bin:$PATH"` (bash).
- **Disable the daemon before `cargo build`** (it locks the `.exe`): `herdr plugin action invoke status-disable-win --plugin ez-corp.space-usage`, then `Get-Process space-usage | Stop-Process -Force`.
- Tests run under the bin crate — `cargo test <filter>` (NOT `cargo test --lib`, there is no lib target).
- herdr running for E2E. Named pipe = `\\.\pipe\` + `HERDR_SOCKET_PATH`; discover via `[System.IO.Directory]::GetFiles("\\.\pipe\")` filtered on `herdr`.
- Re-enable via `herdr plugin action invoke status-enable-win --plugin ez-corp.space-usage`; apply herdr config edits with `herdr server reload-config`.

---

### Task 1: Threshold + sound config in `config.rs`

**Files:**
- Modify: `src/config.rs` (`Config`, `Default`, `parse_config`)
- Test: `src/config.rs` tests

**Interfaces:**
- Produces: `Config.cache_warn_minutes: u64`, `Config.cache_alert_minutes: u64`, `Config.cache_alert_sound: String`, parsed + clamped in `parse_config`.

- [ ] **Step 1: Write the failing tests**

Add to `src/config.rs` tests (after the `config_cache_minutes_*` tests):

```rust
    #[test]
    fn config_cache_tiers_and_sound_defaults() {
        let cfg = parse_config("");
        assert_eq!(cfg.cache_warn_minutes, 30);
        assert_eq!(cfg.cache_alert_minutes, 10);
        assert_eq!(cfg.cache_alert_sound, "");
    }

    #[test]
    fn config_cache_sound_path_is_read_verbatim() {
        let cfg = parse_config("cache_alert_sound = \"C:\\\\stuff\\\\sounds\\\\wav\\\\droid.wav\"");
        assert_eq!(cfg.cache_alert_sound, "C:\\stuff\\sounds\\wav\\droid.wav");
    }

    #[test]
    fn config_cache_tiers_clamp_to_valid_ordering() {
        // alert above the window clamps down to cache_minutes; warn follows.
        let cfg = parse_config("cache_minutes = 60\ncache_alert_minutes = 100\ncache_warn_minutes = 5");
        assert_eq!(cfg.cache_alert_minutes, 60);
        assert_eq!(cfg.cache_warn_minutes, 60); // warn never below alert
        // warn below alert is bumped up to alert.
        let cfg = parse_config("cache_alert_minutes = 20\ncache_warn_minutes = 5");
        assert_eq!(cfg.cache_alert_minutes, 20);
        assert_eq!(cfg.cache_warn_minutes, 20);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `$env:USERPROFILE\.cargo\bin\cargo test config::tests::config_cache`
Expected: FAIL — `no field cache_warn_minutes on type Config`.

- [ ] **Step 3: Write the implementation**

Add fields to `Config`:

```rust
pub struct Config {
    pub mode: Mode,
    pub interval_seconds: u64,
    pub window_title_totals: bool,
    pub cache_minutes: u64,
    pub cache_warn_minutes: u64,
    pub cache_alert_minutes: u64,
    pub cache_alert_sound: String,
}
```

Add to `Default` (after `cache_minutes`):

```rust
            cache_minutes: crate::timer::DEFAULT_CACHE_MINUTES,
            cache_warn_minutes: 30,
            cache_alert_minutes: 10,
            cache_alert_sound: String::new(),
```

Add parse arms in `parse_config`'s `match key` (alongside `cache_minutes`):

```rust
            "cache_warn_minutes" => {
                if let Ok(n) = value.parse::<f64>() {
                    if n >= 1.0 {
                        cfg.cache_warn_minutes = n as u64;
                    }
                }
            }
            "cache_alert_minutes" => {
                if let Ok(n) = value.parse::<f64>() {
                    if n >= 1.0 {
                        cfg.cache_alert_minutes = n as u64;
                    }
                }
            }
            "cache_alert_sound" => cfg.cache_alert_sound = value.to_string(),
```

At the END of `parse_config`, replace the bare trailing `cfg` with the clamp block:

```rust
    // Keep 1 <= alert <= warn <= cache_minutes so the tiers are well-ordered even
    // if the user misconfigures them.
    cfg.cache_alert_minutes = cfg.cache_alert_minutes.clamp(1, cfg.cache_minutes);
    cfg.cache_warn_minutes = cfg
        .cache_warn_minutes
        .clamp(cfg.cache_alert_minutes, cfg.cache_minutes);
    cfg
```

- [ ] **Step 4: Run tests + gates**

Run:
```
$env:USERPROFILE\.cargo\bin\cargo test config
$env:USERPROFILE\.cargo\bin\cargo fmt --all --check
$env:USERPROFILE\.cargo\bin\cargo clippy --all-targets -- -D warnings
```
Expected: config tests PASS; fmt/clippy clean (crate still builds — this change is additive).

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "feat: cache warn/alert thresholds and alert-sound path config"
```

---

### Task 2: Alert sound module `sound.rs`

**Files:**
- Create: `src/sound.rs`
- Modify: `src/main.rs` (add `mod sound;`), `Cargo.toml` (add `Win32_Media_Audio` feature)
- Test: `src/sound.rs` tests

**Interfaces:**
- Produces: `pub fn play_wav(path: &str)` — best-effort async playback; no-op on empty path, missing file, or non-Windows.

- [ ] **Step 1: Write the failing tests**

Create `src/sound.rs`:

```rust
//! Best-effort alert sound. Windows: winmm `PlaySoundW` (async, fire-and-forget).
//! Every other OS: a no-op, so the Linux CI leg stays clean.

use std::path::Path;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_path_is_noop() {
        play_wav(""); // must not panic
    }

    #[test]
    fn missing_file_is_noop() {
        play_wav("this/path/does/not/exist/nope.wav"); // must not panic
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `$env:USERPROFILE\.cargo\bin\cargo test sound`
Expected: FAIL — `cannot find function play_wav`.

- [ ] **Step 3: Write the implementation**

Insert above the `#[cfg(test)]` block in `src/sound.rs`:

```rust
/// Play a `.wav` asynchronously, best-effort. Empty path or a missing file is a
/// no-op; a playback failure is ignored. On non-Windows this does nothing.
pub fn play_wav(path: &str) {
    if path.is_empty() || !Path::new(path).exists() {
        return;
    }
    play(path);
}

#[cfg(windows)]
fn play(path: &str) {
    use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_FILENAME};
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` is NUL-terminated and outlives the call; SND_ASYNC returns
    // immediately (winmm copies the name); null module handle = load from file.
    unsafe {
        PlaySoundW(wide.as_ptr(), std::ptr::null_mut(), SND_FILENAME | SND_ASYNC);
    }
}

#[cfg(not(windows))]
fn play(_path: &str) {}
```

> IMPLEMENTER NOTE: `PlaySoundW`'s module-handle parameter is `HMODULE`. In this crate's `windows-sys` version pass `std::ptr::null_mut()`; if that fails to compile because `HMODULE` is an integer alias, use `0`. Confirm `SND_FILENAME | SND_ASYNC` typecheck against the flags parameter (add `as _` only if the compiler demands it).

Add `mod sound;` to `src/main.rs` (alphabetical):

```rust
mod proc;
mod render;
mod sound;
mod timer;
```

In `Cargo.toml`, find the `[target.'cfg(windows)'.dependencies]` `windows-sys` entry and add `"Win32_Media_Audio"` to its `features` list (keep existing features).

- [ ] **Step 4: Run tests + build (Windows) + gates**

Run:
```
$env:USERPROFILE\.cargo\bin\cargo test sound
$env:USERPROFILE\.cargo\bin\cargo build
$env:USERPROFILE\.cargo\bin\cargo fmt --all --check
$env:USERPROFILE\.cargo\bin\cargo clippy --all-targets -- -D warnings
```
Expected: `sound` tests PASS; build succeeds on Windows (the `PlaySoundW` import resolves via the new feature); fmt/clippy clean. This change is additive — the crate still builds.

- [ ] **Step 5: Commit**

```bash
git add src/sound.rs src/main.rs Cargo.toml Cargo.lock
git commit -m "feat: winmm alert-sound module (cfg-gated, no-op off Windows)"
```

---

### Task 3: Timer tiers + daemon wiring (atomic)

**Files:**
- Modify: `src/timer.rs` (add tiers/label/arming, `TimerState.alerted`, remove `cache_token`), `src/daemon.rs` (rewrite `push_cache_tokens`, edit `push_statuses`, `clear_all`, `run_daemon` call site, add `use crate::sound;`)
- Test: `src/timer.rs` tests

**Interfaces:**
- Consumes: `config.cache_{minutes,warn_minutes,alert_minutes,alert_sound}` (Task 1), `sound::play_wav` (Task 2), `status_line`.
- Produces:
  - `TimerState { pub reset_at: Instant, pub alerted: bool }`
  - `pub enum Tier { Normal, Warn, Alert }`
  - `pub const CACHE_TOKEN_KEYS: &[&str] = &["cache", "cache_warn", "cache_alert"]`
  - `pub fn tier(remaining_minutes: u64, warn_minutes: u64, alert_minutes: u64) -> Tier`
  - `pub fn tier_token_key(t: Tier) -> &'static str`
  - `pub fn cache_label(remaining_minutes: u64) -> String`
  - `pub fn should_alert(t: Tier, alerted: &mut bool) -> bool`
  - `push_cache_tokens(client, spaces, config, labels, timers, tracked)` (adds `labels`).

> This task is atomic (timer API change + daemon fix together) so every commit compiles. No new *daemon* unit test — a live `Herdr` is required; the logic is covered by the `timer`/`config`/`sound` unit tests and Task 5 E2E. The daemon's gate is the full `build` + `clippy` + `test`.

- [ ] **Step 1: Write the failing timer tests**

In `src/timer.rs` tests, REMOVE the three `cache_token_*` tests and add:

```rust
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
    fn cache_label_is_prefixed_and_covers_zero() {
        assert_eq!(cache_label(42), "cache 42m");
        assert_eq!(cache_label(1), "cache 1m");
        assert_eq!(cache_label(0), "cache 0m");
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
```

- [ ] **Step 2: Run to verify they fail**

Run: `$env:USERPROFILE\.cargo\bin\cargo test timer`
Expected: FAIL — `cannot find type Tier`.

- [ ] **Step 3: Update `timer.rs`**

Change `TimerState`:

```rust
/// Per-pane countdown state. `reset_at` is the instant the current window began
/// (refreshed every sample the agent is working). `alerted` debounces the alert
/// sound: set when it fires, cleared whenever the tier leaves Alert.
pub struct TimerState {
    pub reset_at: Instant,
    pub alerted: bool,
}
```

Remove the `cache_token` function entirely. Add, after `remaining_minutes`:

```rust
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

/// The displayed token value, e.g. `"cache 42m"` … `"cache 0m"`. Colour (not an
/// icon) carries the alert, so there is no `⚠`.
pub fn cache_label(remaining_minutes: u64) -> String {
    format!("cache {remaining_minutes}m")
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
```

- [ ] **Step 4: Rewrite `push_cache_tokens` in `daemon.rs`**

Add near the other `use crate::` lines:

```rust
use crate::sound;
```

Replace the whole existing `push_cache_tokens` function with:

```rust
/// Push per-claude-agent tokens: the space's cpu/ram as `$usage` on the claude
/// pane (so its entry reads `claude · cpu · ram · cache`), plus a tiered `$cache*`
/// countdown token. Keeps in-memory `timer::TimerState` per pane across loops.
/// While the agent works the cache tokens are suppressed and the sound re-arms;
/// on the transition into the alert tier the configured `.wav` plays once.
/// Best-effort per pane; the TTL self-clears if the daemon dies. Prunes state
/// for panes that closed.
pub fn push_cache_tokens(
    client: &mut Herdr,
    spaces: &[Space],
    config: &Config,
    labels: &Labels,
    timers: &mut HashMap<String, timer::TimerState>,
    tracked: &mut Tracked,
) {
    let source = config::plugin_id();
    let ttl_ms = config.interval_seconds * 1000 * 3;
    let now = Instant::now();

    let mut present: HashSet<String> = HashSet::new();
    for sp in spaces {
        if sp.claude_panes.is_empty() {
            continue;
        }
        let usage = status_line(sp, labels);
        for cp in &sp.claude_panes {
            present.insert(cp.pane_id.clone());
            let working = timer::is_working(cp.status.as_deref());
            let state = timers
                .entry(cp.pane_id.clone())
                .or_insert_with(|| timer::TimerState {
                    reset_at: now,
                    alerted: false,
                });
            timer::on_sample(state, working, now);

            // cpu/ram always rides the claude line.
            let _ = client.pane_report_tokens(&cp.pane_id, &source, &[("usage", &usage)], ttl_ms);
            tracked.cache.insert(cp.pane_id.clone());

            if working {
                // Suppress the countdown and re-arm the alert sound.
                for key in timer::CACHE_TOKEN_KEYS {
                    let _ = client.clear_pane_token(&cp.pane_id, &source, key);
                }
                state.alerted = false;
                continue;
            }

            let remaining = timer::remaining_minutes(state.reset_at, now, config.cache_minutes);
            let t = timer::tier(remaining, config.cache_warn_minutes, config.cache_alert_minutes);
            let active = timer::tier_token_key(t);
            let label = timer::cache_label(remaining);
            let _ = client.pane_report_tokens(&cp.pane_id, &source, &[(active, &label)], ttl_ms);
            for key in timer::CACHE_TOKEN_KEYS.iter().filter(|k| **k != active) {
                let _ = client.clear_pane_token(&cp.pane_id, &source, key);
            }

            if timer::should_alert(t, &mut state.alerted) && !config.cache_alert_sound.is_empty() {
                sound::play_wav(&config.cache_alert_sound);
            }
        }
    }
    timers.retain(|pane_id, _| present.contains(pane_id));
}
```

- [ ] **Step 5: Suppress the `usage` pseudo-agent for claude spaces**

In `push_statuses`, the `for sp in spaces` loop currently starts with `let status = status_line(sp, labels);` followed by the masked-pane release block (added earlier) and then the `if config.mode == Mode::AgentsPanel {` branch. Insert the claude-space skip AFTER the masked-release block and BEFORE the mode branch:

```rust
        // A space with a claude agent shows cpu/ram on the claude line (see
        // push_cache_tokens), so it needs no separate `usage` pseudo-agent entry.
        // Release any pseudo-agent we still hold there and skip the usage push.
        if !sp.claude_panes.is_empty() {
            for pane_id in &sp.pseudo_panes {
                release_pseudo(client, pane_id, &source);
                let _ = client.clear_pane_token(pane_id, &source, "usage");
            }
            continue;
        }
```

- [ ] **Step 6: Clear all cache keys on cleanup**

In `clear_all`, replace the single-`"cache"` clear loop over `tracked.cache` with:

```rust
    for pane_id in &tracked.cache {
        for key in timer::CACHE_TOKEN_KEYS {
            let _ = client.clear_pane_token(pane_id, &source, key);
        }
        let _ = client.clear_pane_token(pane_id, &source, "usage");
    }
```

- [ ] **Step 7: Pass `labels` at the call site**

In `run_daemon`, update the `push_cache_tokens` call:

```rust
                    push_cache_tokens(&mut client, &spaces, &config, &labels, &mut timers, &mut guard);
```

- [ ] **Step 8: Build, lint, test**

Run:
```
target\debug\space-usage.exe --disable
$env:USERPROFILE\.cargo\bin\cargo build
$env:USERPROFILE\.cargo\bin\cargo fmt --all --check
$env:USERPROFILE\.cargo\bin\cargo clippy --all-targets -- -D warnings
$env:USERPROFILE\.cargo\bin\cargo test
```
Expected: build succeeds; fmt/clippy clean; all tests pass (timer + config + sound + existing).

- [ ] **Step 9: Commit**

```bash
git add src/timer.rs src/daemon.rs
git commit -m "feat: claude-line cpu/ram + tiered colored cache token + alert sound"
```

---

### Task 4: Docs — README recipe + herdr-plugin.toml

**Files:**
- Modify: `README.md`, `herdr-plugin.toml`

> Docs only; no test. Verify by re-reading.

- [ ] **Step 1: Update the `$cache` recipe in `README.md`**

Replace the existing `$cache` agents-panel example block with:

````markdown
The per-agent cache timer shares the claude agent's line with cpu/ram and colours
by urgency. Put cpu/ram (`$usage`) and the three tiered cache tokens on the agent
row, styling each cache token's colour inline (herdr token colour is static per
config, so the plugin swaps which token carries the value as time runs down):

```toml
[ui.sidebar.agents]
rows = [
    ["state_icon", "workspace", "tab"],
    [
        "agent",
        "$usage",                                                # cpu/ram
        { token = "$cache",       fg = "#a6e3a1" },              # > warn: green
        { token = "$cache_warn",  fg = "#f9e2af" },              # <= warn: yellow
        { token = "$cache_alert", fg = "#f38ba8", bold = true }, # <= alert: red
    ],
]
```

This renders `claude · cpu 1% · ram 2% · cache 60m`, the countdown turning yellow
at `cache_warn_minutes` and red at `cache_alert_minutes`. Only `claude` agents
carry these tokens; a space with a claude agent shows cpu/ram on that line
instead of a separate `usage` entry. Plugin `config.toml` keys:

- `cache_minutes` (default 60) — the full window.
- `cache_warn_minutes` (default 30) — yellow at/under this.
- `cache_alert_minutes` (default 10) — red at/under this, and plays the sound.
- `cache_alert_sound` — path to a `.wav` played once (Windows) when the timer
  enters the alert tier; empty disables it.
````

- [ ] **Step 2: Update the config comment in `herdr-plugin.toml`**

Replace the existing `cache_minutes` comment line with:

```toml
# The per-agent cache countdown ($cache/$cache_warn/$cache_alert tokens in
# [ui.sidebar.agents]) resets to `cache_minutes` (default 60) when a claude agent
# goes idle and ticks down. It turns yellow at `cache_warn_minutes` (30) and red
# at `cache_alert_minutes` (10); `cache_alert_sound` plays a .wav once (Windows)
# on entering the alert tier. Colours are set per token in the herdr config.
```

- [ ] **Step 3: Commit**

```bash
git add README.md herdr-plugin.toml
git commit -m "docs: tiered colored cache token + sound recipe"
```

---

### Task 5: Live E2E (herdr 0.7.4, Windows)

**Files:** none (verification only).

- [ ] **Step 1: Build + deploy the new binary**

```
herdr plugin action invoke status-disable-win --plugin ez-corp.space-usage
$env:USERPROFILE\.cargo\bin\cargo build --release
herdr plugin action invoke status-enable-win --plugin ez-corp.space-usage
```

- [ ] **Step 2: Apply the herdr config + reload**

Edit `%APPDATA%\herdr\config.toml` `[ui.sidebar.agents]` rows per Task 4 (merged line + three colored cache tokens), then `herdr server reload-config`. Expected: `status: applied`, no diagnostics.

- [ ] **Step 3: Watch a claude agent idle**

Start a `claude` session in a herdr pane and leave it idle. Confirm its entry reads `claude · cpu X% · ram Y% · cache 60m` (green) — one entry for the space, no separate `usage` entry. Set `cache_warn_minutes`/`cache_alert_minutes` high (e.g. 59/58) + `cache_alert_sound = "C:\\stuff\\sounds\\wav\\droid.wav"`, restart the daemon, and confirm within ~2 min: yellow (<= warn), then red (<= alert) with the `.wav` playing once. Probe token values over the socket if the visual is ambiguous (`pane.list` → `tokens.cache*`).

- [ ] **Step 4: Confirm reset re-arms the sound**

Send the agent a prompt so it works (cache suppressed), then let it idle again → back to green `cache 60m`; drive it back under the alert threshold → the sound plays again (re-armed). Confirm it does NOT replay every loop while sitting under the threshold.

- [ ] **Step 5: Confirm a non-claude space still shows usage**

Confirm a space without a claude agent still shows its `usage` cpu/ram entry unchanged.

- [ ] **Step 6: CI-parity check + restore config**

```
$env:USERPROFILE\.cargo\bin\cargo fmt --all --check
$env:USERPROFILE\.cargo\bin\cargo clippy --all-targets -- -D warnings
$env:USERPROFILE\.cargo\bin\cargo test
```
Expected: all clean/green. Restore sane threshold values in `config.toml` after testing.
