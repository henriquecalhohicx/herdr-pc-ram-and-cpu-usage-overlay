# Agent Attention / Cache Countdown Timer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show a per-`claude`-agent 60-minute countdown timer (`$cache` token) in the herdr agents panel that resets to full while the agent works and ticks to `0m` (alert) once it goes idle/blocked/needs-input.

**Architecture:** A pure, unit-tested `timer` module owns the countdown math and the working/stopped decision. The daemon loop keeps an in-memory `HashMap<pane_id, TimerState>`, samples each `claude` pane's herdr `agent_status` every interval, and pushes a TTL'd `$cache` pane-metadata token (same mechanism as the existing `$usage` token). While the agent is actively working the token is suppressed and its reset instant is pinned to now; the moment it stops, the countdown runs from a full 60m.

**Tech Stack:** Rust (edition already set), herdr JSON-RPC over a named pipe (Windows) / unix socket (Linux), `std::time::Instant`. No new crates.

## Global Constraints

- **Per-agent, not per-space.** One timer per `claude` agent pane. Non-`claude` agents get no timer.
- **Count only when idle.** While actively working: suppress the token and pin the reset. On stop (idle/blocked/needs-input): reset to full 60m and count down.
- **At zero: alert marker.** Render `⚠ 0m` (icon/text — herdr token color is static per config, so the icon carries the alert). No `$cache_alert` swap.
- **Format:** whole minutes `"42m"` … `"1m"`, then `"⚠ 0m"` at expiry.
- **Cache window configurable:** `cache_minutes` in the plugin `config.toml`, default `60`, numeric `>= 1` (mirror `interval_seconds` parsing exactly).
- **Persistence:** in-memory only. A daemon restart resets all timers to full. Do NOT write reset instants to disk.
- **CI matrix (ubuntu + windows) must stay green; keep Linux `#[cfg]`-clean.** The timer module is platform-agnostic — do not add `cfg` gates to it.
- **Claude agent detection:** `pane.agent == "claude"` (confirmed live).
- **herdr `agent_status` values** (confirmed live 2026-07-19): idle `claude` agent reports `"idle"`; plain shell pane reports `"unknown"`. The exact *working* string is confirmed in Task 6 — the working-set default is `["working", "running", "busy", "active", "thinking"]`; everything else counts down.

### Build / run environment gotchas (apply to every build & E2E step)

- **cargo PATH is stale in tool shells** — prepend `$env:USERPROFILE\.cargo\bin` before `cargo`.
- **Disable/kill the daemon before `cargo build`** — a running daemon locks the `.exe` (Access denied). Run `target\debug\space-usage.exe --disable` (or kill the process) first.
- **herdr 0.7.4 must be running for live E2E.** Named pipe = `\\.\pipe\` + `HERDR_SOCKET_PATH`; that env var is only injected into herdr-spawned panes. Probe from a tool shell by enumerating the pipe (see Task 6).
- **herdr socket protocol:** newline-delimited JSON-RPC `{"id":..,"method":..,"params":{..}}\n`.

---

### Task 1: Pure timer module

**Files:**
- Create: `src/timer.rs`
- Modify: `src/main.rs:19-25` (add `mod timer;`)
- Test: `src/timer.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `std::time::Instant`.
- Produces:
  - `pub const DEFAULT_CACHE_MINUTES: u64 = 60`
  - `pub const WORKING_STATES: &[&str]`
  - `pub struct TimerState { pub reset_at: Instant }`
  - `pub fn is_working(status: Option<&str>) -> bool`
  - `pub fn on_sample(state: &mut TimerState, working: bool, now: Instant)`
  - `pub fn remaining_minutes(reset_at: Instant, now: Instant, total_minutes: u64) -> u64`
  - `pub fn cache_token(working: bool, reset_at: Instant, now: Instant, total_minutes: u64) -> Option<String>`

- [ ] **Step 1: Write the failing tests**

Create `src/timer.rs` with the test module first (module body empty for now so it fails to compile → fails):

```rust
//! Per-agent cache countdown timer: pure math + the working/stopped decision.
//!
//! No herdr or platform types — unit-tested in isolation. The daemon owns the
//! per-pane `TimerState` map and calls these on each sample.

use std::time::Instant;

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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `$env:USERPROFILE\.cargo\bin\cargo test --lib timer`
Expected: FAIL — `cannot find function is_working` / `cannot find type TimerState`.

- [ ] **Step 3: Write the minimal implementation**

Insert this above the `#[cfg(test)]` block in `src/timer.rs`:

```rust
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
```

Add the module declaration to `src/main.rs` in the `mod` block (alphabetical, after `mod render;` is fine — match existing order; insert `mod timer;`):

```rust
mod collect;
mod config;
mod daemon;
mod herdr;
mod model;
mod proc;
mod render;
mod timer;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `$env:USERPROFILE\.cargo\bin\cargo test --lib timer`
Expected: PASS — 8 tests in `timer::tests`.

- [ ] **Step 5: Commit**

```bash
git add src/timer.rs src/main.rs
git commit -m "feat: pure cache-countdown timer module"
```

---

### Task 2: Configurable cache window

**Files:**
- Modify: `src/config.rs:22-38` (`Config` struct + `Default`), `src/config.rs:138-164` (`parse_config`)
- Test: `src/config.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `crate::timer::DEFAULT_CACHE_MINUTES`.
- Produces: `Config.cache_minutes: u64` (read by the daemon in Task 4).

- [ ] **Step 1: Write the failing tests**

Add to the `parse_config` test group in `src/config.rs` (after `config_interval_seconds_gates_on_ge_one`):

```rust
    #[test]
    fn config_cache_minutes_defaults_to_60() {
        assert_eq!(parse_config("").cache_minutes, 60);
    }

    #[test]
    fn config_cache_minutes_gates_on_ge_one() {
        assert_eq!(parse_config("cache_minutes = 90").cache_minutes, 90);
        assert_eq!(parse_config("cache_minutes = \"45\"").cache_minutes, 45);
        // Below 1, zero, and non-numeric keep the default 60.
        assert_eq!(parse_config("cache_minutes = 0").cache_minutes, 60);
        assert_eq!(parse_config("cache_minutes = -5").cache_minutes, 60);
        assert_eq!(parse_config("cache_minutes = soon").cache_minutes, 60);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `$env:USERPROFILE\.cargo\bin\cargo test --lib config::tests::config_cache`
Expected: FAIL — `no field cache_minutes on type Config`.

- [ ] **Step 3: Write the minimal implementation**

Add the field to `Config` (`src/config.rs:24-28`):

```rust
pub struct Config {
    pub mode: Mode,
    pub interval_seconds: u64,
    pub window_title_totals: bool,
    pub cache_minutes: u64,
}
```

Add it to the `Default` impl (`src/config.rs:31-37`):

```rust
        Self {
            mode: Mode::AgentsPanel,
            interval_seconds: 5,
            window_title_totals: true,
            cache_minutes: crate::timer::DEFAULT_CACHE_MINUTES,
        }
```

Add the parse arm in `parse_config`'s `match key` (`src/config.rs`, alongside `interval_seconds`):

```rust
            "cache_minutes" => {
                if let Ok(n) = value.parse::<f64>() {
                    if n >= 1.0 {
                        cfg.cache_minutes = n as u64;
                    }
                }
            }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `$env:USERPROFILE\.cargo\bin\cargo test --lib config`
Expected: PASS — including the two new `config_cache_minutes_*` tests.

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "feat: add configurable cache_minutes (default 60)"
```

---

### Task 3: Surface each claude pane's agent_status

**Files:**
- Modify: `src/model.rs:19-46` (`Space` — add `claude_panes`), `src/model.rs:81-89` (`PaneInfo` — add `agent_status`); add a `ClaudePane` struct
- Modify: `src/collect.rs:31-87` (`collect_spaces` — populate `claude_panes`), `src/collect.rs:218-258` (`aggregate_families` — fold child `claude_panes` into parent)
- Test: `src/collect.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: nothing new.
- Produces:
  - `pub struct ClaudePane { pub pane_id: String, pub status: Option<String> }`
  - `Space.claude_panes: Vec<ClaudePane>` — every `claude` agent pane in the space (and, post-aggregation, in its folded worktree children), with its herdr `agent_status`.
  - `PaneInfo.agent_status: Option<String>`.

- [ ] **Step 1: Write the failing test**

Add to `src/collect.rs` tests. First extend the `space()` test helper is not needed (uses `..Default::default()`). Add:

```rust
    #[test]
    fn aggregate_folds_child_claude_panes_into_parent() {
        use crate::model::ClaudePane;
        let mut parent = space("p", 0.0, 0.0, 0, 1);
        parent.claude_panes = vec![ClaudePane {
            pane_id: "p:p1".to_string(),
            status: Some("idle".to_string()),
        }];
        let mut child = space("c", 0.0, 0.0, 0, 1);
        child.family_parent = Some("p".to_string());
        child.claude_panes = vec![ClaudePane {
            pane_id: "c:p1".to_string(),
            status: Some("working".to_string()),
        }];

        let out = aggregate_families(vec![parent, child]);

        assert_eq!(out.len(), 1);
        let ids: Vec<&str> = out[0].claude_panes.iter().map(|c| c.pane_id.as_str()).collect();
        assert_eq!(ids, vec!["p:p1", "c:p1"], "child claude panes fold into parent");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `$env:USERPROFILE\.cargo\bin\cargo test --lib collect::tests::aggregate_folds_child_claude`
Expected: FAIL — `no field claude_panes on type Space` / `no type ClaudePane`.

- [ ] **Step 3: Write the minimal implementation**

Add `ClaudePane` and the `Space` field in `src/model.rs`. After the `Space` struct docs, add the field (inside `Space`, after `pseudo_panes`):

```rust
    /// panes carrying our "usage" pseudo-agent.
    pub pseudo_panes: Vec<String>,
    /// `claude` agent panes and their herdr agent_status (for the cache timer).
    pub claude_panes: Vec<ClaudePane>,
```

Add the struct just below `Space` (before the `workspace.list` section comment):

```rust
/// A `claude` agent pane plus its herdr `agent_status`, used by the per-agent
/// cache countdown timer. Collected in `collect_spaces` and folded upward in
/// `aggregate_families` so worktree-child agents keep their timer.
#[derive(Debug, Clone, Default)]
pub struct ClaudePane {
    pub pane_id: String,
    pub status: Option<String>,
}
```

Add `agent_status` to `PaneInfo` (`src/model.rs:83-89`):

```rust
pub struct PaneInfo {
    pub pane_id: String,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub agent_status: Option<String>,
}
```

In `src/collect.rs`, import `ClaudePane` (extend the existing model import at line 16):

```rust
use crate::model::{ClaudePane, Space};
```

In `collect_spaces`, add the accumulator next to the others (near `src/collect.rs:41`):

```rust
        let mut pseudo_panes = Vec::new(); // panes already carrying our "usage" agent
        let mut claude_panes = Vec::new(); // claude agent panes + status (cache timer)
```

Inside the `for pane in &panes` loop, right after the `match pane.agent.as_deref() { .. }` block, add:

```rust
            // Track claude agents for the per-agent cache countdown timer.
            if pane.agent.as_deref() == Some("claude") {
                claude_panes.push(ClaudePane {
                    pane_id: pane.pane_id.clone(),
                    status: pane.agent_status.clone(),
                });
            }
```

Add the field when constructing `Space` (near `src/collect.rs:80`, after `pseudo_panes,`):

```rust
            pseudo_panes,
            claude_panes,
```

In `aggregate_families`, fold the child's claude panes into the parent. Extend the snapshot tuple and the parent mutation (`src/collect.rs:235-253`):

```rust
        let (cpu, ram_mb, proc_count, pane_count, label, claude_panes) = {
            let child = &spaces[i];
            (
                child.cpu,
                child.ram_mb,
                child.proc_count,
                child.pane_count,
                child.label.clone(),
                child.claude_panes.clone(),
            )
        };
        let parent = &mut spaces[parent_idx];
        parent.cpu += cpu;
        parent.ram_mb += ram_mb;
        parent.proc_count += proc_count;
        parent.pane_count += pane_count;
        parent.claude_panes.extend(claude_panes);
        parent
            .worktree_labels
            .get_or_insert_with(Vec::new)
            .push(label);
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `$env:USERPROFILE\.cargo\bin\cargo test --lib collect`
Expected: PASS — including `aggregate_folds_child_claude_panes_into_parent` and all pre-existing aggregate tests.

- [ ] **Step 5: Commit**

```bash
git add src/model.rs src/collect.rs
git commit -m "feat: collect claude panes + agent_status, fold across worktrees"
```

---

### Task 4: Push the $cache token from the daemon loop

**Files:**
- Modify: `src/daemon.rs:10` (import `HashMap`), add `use std::time::Instant;`, `use crate::timer;`
- Modify: `src/daemon.rs:30-39` (`Tracked` — add `cache`)
- Modify: `src/daemon.rs:104-213` (`run_daemon` — own the timers map, call the new push fn)
- Modify: `src/daemon.rs:375-387` (`clear_all` — clear cache tokens)
- Modify: `src/daemon.rs:271-287` (`disable_updater` sweep — include cache panes)
- Add: `push_cache_tokens` fn in `src/daemon.rs`

**Interfaces:**
- Consumes: `Space.claude_panes` (Task 3), `Config.cache_minutes` (Task 2), `timer::{TimerState, is_working, on_sample, cache_token}` (Task 1).
- Produces: `pub fn push_cache_tokens(client: &mut Herdr, spaces: &[Space], config: &Config, timers: &mut HashMap<String, timer::TimerState>, tracked: &mut Tracked)`.

> No new unit test: `push_cache_tokens` requires a live `Herdr` client. Its decision logic is fully covered by `timer` tests (Task 1); its end-to-end behaviour is verified in Task 6. The test cycle for this task is `cargo build` + `cargo test` (nothing regresses) + `cargo clippy`.

- [ ] **Step 1: Add imports and the `cache` tracked set**

Change the collections import (`src/daemon.rs:10`):

```rust
use std::collections::{HashMap, HashSet};
```

Add to the time import (`src/daemon.rs:17`) so both are present:

```rust
use std::time::{Duration, Instant};
```

Add the timer import near the other `use crate::` lines (`src/daemon.rs:24-28`):

```rust
use crate::timer;
```

Add the field to `Tracked` (`src/daemon.rs:32-39`):

```rust
pub struct Tracked {
    /// Panes carrying our pseudo-agent (released, not TTL'd).
    pub pseudo: HashSet<String>,
    /// Panes carrying TTL'd metadata statuses.
    pub metadata: HashSet<String>,
    /// Workspaces carrying our TTL'd `usage` spaces-card token.
    pub workspaces: HashSet<String>,
    /// Claude panes carrying our TTL'd `cache` countdown token.
    pub cache: HashSet<String>,
}
```

- [ ] **Step 2: Add the `push_cache_tokens` function**

Add after `push_space_tokens` (`src/daemon.rs:428`):

```rust
/// Push each `claude` agent pane's cache-countdown as a TTL'd `$cache` pane
/// metadata token. Keeps a per-pane [`timer::TimerState`] across loop iterations
/// in `timers`: while the agent works the token is suppressed and the window is
/// pinned full; while stopped it ticks down to `⚠ 0m`. Prunes timer state for
/// panes that no longer exist. Best-effort per pane; the TTL self-clears if the
/// daemon dies. Renders wherever the user adds a `$cache` row to
/// `[ui.sidebar.agents]`; herdr elides the segment for a suppressed/absent token.
pub fn push_cache_tokens(
    client: &mut Herdr,
    spaces: &[Space],
    config: &Config,
    timers: &mut HashMap<String, timer::TimerState>,
    tracked: &mut Tracked,
) {
    let source = config::plugin_id();
    let ttl_ms = config.interval_seconds * 1000 * 3;
    let now = Instant::now();

    let mut present: HashSet<String> = HashSet::new();
    for sp in spaces {
        for cp in &sp.claude_panes {
            present.insert(cp.pane_id.clone());
            let working = timer::is_working(cp.status.as_deref());
            let state = timers
                .entry(cp.pane_id.clone())
                .or_insert_with(|| timer::TimerState { reset_at: now });
            timer::on_sample(state, working, now);

            match timer::cache_token(working, state.reset_at, now, config.cache_minutes) {
                Some(text) => {
                    if client
                        .pane_report_tokens(&cp.pane_id, &source, &[("cache", &text)], ttl_ms)
                        .is_ok()
                    {
                        tracked.cache.insert(cp.pane_id.clone());
                    }
                }
                None => {
                    // Working — suppress so herdr elides the segment.
                    let _ = client.clear_pane_token(&cp.pane_id, &source, "cache");
                    tracked.cache.remove(&cp.pane_id);
                }
            }
        }
    }
    // Drop state for panes that closed since the last sample.
    timers.retain(|pane_id, _| present.contains(pane_id));
}
```

- [ ] **Step 3: Wire it into `run_daemon`**

Add the timers map before the loop (`src/daemon.rs`, just before `let daemon_interval_ms = ...` at line 170):

```rust
    let mut timers: HashMap<String, timer::TimerState> = HashMap::new();
```

Inside the guarded block, after the `push_space_tokens(..)` call (`src/daemon.rs:187-193`), add:

```rust
                    push_space_tokens(
                        &mut client,
                        &spaces,
                        &labels,
                        config.interval_seconds * 1000 * 3,
                        &mut guard,
                    );
                    // Per-agent cache countdown on each claude pane.
                    push_cache_tokens(&mut client, &spaces, &config, &mut timers, &mut guard);
```

- [ ] **Step 4: Clear cache tokens on shutdown and disable**

In `clear_all` (`src/daemon.rs:375-387`), add a loop after the `workspaces` loop:

```rust
    for pane_id in &tracked.cache {
        let _ = client.clear_pane_token(pane_id, &source, "cache");
    }
```

In `disable_updater`'s sweep loop (`src/daemon.rs:278-283`), add the claude panes to the sweep so a dead daemon's tokens are cleared promptly:

```rust
            for sp in &spaces {
                let _ = client.clear_workspace_token(&sp.id, &source, "usage");
                sweep.pseudo.extend(sp.pseudo_panes.iter().cloned());
                sweep.metadata.extend(sp.agent_panes.iter().cloned());
                sweep.metadata.extend(sp.spare_panes.iter().cloned());
                sweep.cache.extend(sp.claude_panes.iter().map(|c| c.pane_id.clone()));
            }
```

- [ ] **Step 5: Build, test, and lint**

Run:
```
target\debug\space-usage.exe --disable
$env:USERPROFILE\.cargo\bin\cargo build
$env:USERPROFILE\.cargo\bin\cargo test
$env:USERPROFILE\.cargo\bin\cargo clippy --all-targets -- -D warnings
```
Expected: build succeeds; all tests pass; clippy clean.

- [ ] **Step 6: Commit**

```bash
git add src/daemon.rs
git commit -m "feat: push per-agent \$cache countdown token from the daemon"
```

---

### Task 5: Document the $cache token and cache_minutes

**Files:**
- Modify: `README.md` (agents-panel token recipe, ~`README.md:107-116`)
- Modify: `herdr-plugin.toml` (config comment header, ~`herdr-plugin.toml:17-21`)

> No test — docs only. Verify by re-reading the rendered section.

- [ ] **Step 1: Extend the agents-panel recipe in `README.md`**

After the existing `[ui.sidebar.agents]` example block (the one ending with the `$usage` row and the "Other agents carry no `$usage`" note), add:

````markdown
To also show the per-agent **cache countdown timer**, add a `$cache` token. Each
`claude` agent entry then shows minutes until its ~1h prompt cache goes cold /
the agent has been idle an hour — resetting to `60m` while it works and counting
down once it goes idle, blocked, or needs input, ending at `⚠ 0m`:

```toml
[ui.sidebar.agents]
rows = [
    ["state_icon", "workspace", "tab"],   # herdr defaults
    ["agent"],                            #
    ["$usage", "$cache"],                 # ← cpu/ram + attention/cache countdown
]
```

Only `claude` agents carry `$cache`; herdr elides the segment for other agents
and while an agent is actively working. The window defaults to 60 minutes; set
`cache_minutes` in the plugin `config.toml` to change it.
````

- [ ] **Step 2: Note `cache_minutes` in `herdr-plugin.toml`**

Add a line to the config comment block near the top (after the existing `$usage` note, ~`herdr-plugin.toml:21`):

```toml
# The per-agent cache countdown ($cache token in [ui.sidebar.agents]) resets to
# `cache_minutes` (default 60) when a claude agent goes idle and ticks to 0.
```

- [ ] **Step 3: Commit**

```bash
git add README.md herdr-plugin.toml
git commit -m "docs: document \$cache token and cache_minutes"
```

---

### Task 6: Live E2E verification (herdr 0.7.4 on Windows)

**Files:** none (verification only). This task confirms open item 1 (the exact herdr *working* status string) and the end-to-end render.

> If the observed working status is NOT in `WORKING_STATES`, add it to the const in `src/timer.rs` (Task 1), re-run `cargo test`, rebuild, and amend/extend the Task 1 commit before finishing.

- [ ] **Step 1: Build a fresh binary (daemon disabled first)**

Run:
```
target\debug\space-usage.exe --disable
$env:USERPROFILE\.cargo\bin\cargo build
```
Expected: build succeeds (no "Access denied" on the `.exe`).

- [ ] **Step 2: Enable the daemon and confirm the token on an idle claude pane**

Run: `target\debug\space-usage.exe --enable`

Add `$cache` to `~/.config/herdr/config.toml` (or the Windows herdr config at `%APPDATA%\herdr\config.toml`) `[ui.sidebar.agents]` rows per Task 5, reload herdr config, and confirm an **idle** `claude` agent's entry shows `~60m` and ticks down over subsequent samples.

- [ ] **Step 3: Probe the live working status string**

While a `claude` agent is actively working, probe `pane.list` and read `agent_status`. From a tool shell (PowerShell):

```powershell
$pipeName = 'C:\Users\HenriqueCalhó\AppData\Roaming\herdr\herdr.sock'
function Invoke-Herdr($method, $params) {
  $c = New-Object System.IO.Pipes.NamedPipeClientStream('.', $pipeName, [System.IO.Pipes.PipeDirection]::InOut)
  $c.Connect(3000)
  $sw = New-Object System.IO.StreamWriter($c); $sw.AutoFlush = $true
  $sr = New-Object System.IO.StreamReader($c)
  ($sw).WriteLine((@{ id='probe:1'; method=$method; params=$params } | ConvertTo-Json -Compress))
  $line = $sr.ReadLine(); $c.Dispose(); $line
}
$ws = (Invoke-Herdr 'workspace.list' @{} | ConvertFrom-Json).result.workspaces
foreach ($w in $ws) { Invoke-Herdr 'pane.list' @{ workspace_id = $w.workspace_id } }
```

Expected: the working `claude` pane's `agent_status` is one of `WORKING_STATES` (confirm the token is suppressed for it). If not, update `WORKING_STATES` and rebuild.

- [ ] **Step 4: Confirm reset and expiry behaviour**

- Let a `claude` agent finish → its entry shows `~60m`.
- Make it work again → the `$cache` segment disappears (suppressed).
- Let it go idle again → back to `~60m`, ticking down.
- (Optional, faster) set `cache_minutes = 1` in the plugin config, restart the daemon, and confirm the entry reaches `⚠ 0m` after ~1 minute idle. Restore `cache_minutes` after.

- [ ] **Step 5: Confirm cleanup**

Run: `target\debug\space-usage.exe --disable`
Expected: the `$cache` segments clear from all `claude` entries (or self-clear within one TTL, ≈ 3× interval).

- [ ] **Step 6: Full CI-parity check**

Run:
```
$env:USERPROFILE\.cargo\bin\cargo test
$env:USERPROFILE\.cargo\bin\cargo clippy --all-targets -- -D warnings
```
Expected: all pass, clippy clean. (Linux leg stays green — the timer module has no `cfg` gates.)

- [ ] **Step 7: Commit any working-set adjustment**

If `WORKING_STATES` was changed in Step 3:
```bash
git add src/timer.rs
git commit -m "fix: match herdr working agent_status string"
```
