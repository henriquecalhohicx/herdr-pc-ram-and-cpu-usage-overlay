# Agent Attention / Cache Countdown Timer — Design

**Date:** 2026-07-19
**Status:** Brainstorm captured; a few open items to confirm before writing the plan.
**Handoff:** Written so a fresh session can resume cheaply. Read this + the repo
(`src/daemon.rs`, `src/herdr/mod.rs`, `src/collect.rs`, `herdr-plugin.toml`) and
the prior spec `2026-07-18-windows-port-design.md` for herdr-integration context.

## Goal

Show a per-agent **countdown timer** next to the cpu/ram figures that starts at
60 minutes and ticks down to 0. Purpose: track herdr agents that need attention
and the Claude Code ~1h prompt-cache window. When a `claude` agent **finishes a
task, is blocked, or needs user action** (i.e. it stops working), its timer
resets to 60m and counts down; at 0 the cache is likely cold / the agent has
been idle an hour.

## Decisions (settled in brainstorm)

1. **Scope: per-agent.** Each `claude` agent has its own 60m timer, shown on its
   agents-panel entry. Matches the per-session cache and "which agent needs
   attention". A space with N agents shows N timers. (Not per-space.)
2. **Count only when idle.** While an agent is actively working, no countdown
   (its cache is being refreshed each turn) — show nothing (or a "working"
   marker). The moment it stops (finish/block/needs-input) the timer resets to
   60m and ticks down.
3. **At zero: red / alert.** Keep the entry; render `0m` (or `expired`) with an
   alert color/icon so it stands out as needing attention.

## Existing surface this builds on

- The daemon (`--daemon`, `run_daemon` in `src/daemon.rs`) already runs a loop
  every `interval_seconds` (default 5s), calling `collect::snapshot` which reads
  every pane and its `agent` / `agent_status` via the herdr socket, and pushes
  per-space `usage` tokens (`workspace.report_metadata`) + per-pane `usage`
  tokens on the pseudo-agent pane (`pane.report_metadata`, `pane_report_tokens`).
- herdr renders custom `$name` tokens in the agents panel
  (`[ui.sidebar.agents] rows`) and spaces card (`[ui.sidebar.spaces] rows`).
- herdr emits `pane.agent_status_changed` events and exposes `agent_status` per
  pane in `pane.list`. `EventKind` includes `pane.agent_status_changed`,
  `pane.agent_detected`, etc. (see herdr `src/api/schema/events.rs`).

## Proposed design

### Reset trigger

The daemon already polls every pane's `agent_status` each loop. Track per-pane
`{ last_status, reset_at }` in memory in the daemon:

- On a transition **into a "stopped" status** (agent went from working →
  idle/blocked/waiting-for-input), set `reset_at = now`.
- While the agent is in a working/active status, the timer is **not counting**
  (display suppressed or shown full); `reset_at` is refreshed to `now` so that
  the moment it stops, the countdown starts from a full 60m.
- `remaining = max(0, 60min - (now - reset_at))`, rendered as whole minutes.

Time source: the daemon is a long-running native process — use
`std::time::Instant`/`SystemTime` directly (unlike the workflow sandbox, real
time is available here).

Only applies to panes whose detected agent is a Claude agent (the cache concept
is Claude-specific); other agents get no timer. Confirm how herdr labels the
agent (`agent == "claude"`) — see open item 1.

### Display

- Push a new per-pane token (working name **`cache`**, config `$cache`) on each
  Claude agent's pane alongside/after the existing usage tokens, e.g. the agents
  panel row becomes `cpu 1% · ram 4% · 42m`.
- Format: whole minutes `"42m"` … `"1m"`, then `"0m"` at expiry (open item 4:
  `Nm` vs `MM:SS`).
- Working state: suppress the token (herdr elides the empty row/segment) or emit
  a marker like `"working"`.
- **At-zero alert coloring:** herdr token color is *static per config*, so the
  plugin cannot dynamically turn a value red. Two viable approaches (open item
  3): (a) emit an icon/text marker at 0 (e.g. `"⚠ 0m"`) that reads as alert
  without color; or (b) the token-swap trick — populate `$cache` normally but at
  0 populate a separate `$cache_alert` token instead, and the user's config
  styles `$cache_alert` red; the plugin switches which token carries the value.

### Data flow / state

- Per-pane timer state lives in the daemon loop (in-memory `HashMap<pane_id,
  TimerState>`). Prune entries for panes that no longer exist each loop.
- On disable/shutdown, clear the `cache`/`cache_alert` tokens like the existing
  `clear_pane_token` cleanup (TTL also self-clears).
- Persistence across daemon restart: **not persisted** by default (a restart
  resets all timers to full). Open item 2 if durable timers are wanted.

### Config (user side)

Extend `[ui.sidebar.agents]` rows with the timer token, e.g.:

```toml
[ui.sidebar.agents]
rows = [
    ["state_icon", "workspace", "tab"],
    ["agent"],
    ["$usage", "$cache"],   # cpu/ram + countdown
]
```

Document in the README next to the existing token recipe.

## Open items to confirm (fresh session)

1. **herdr agent states.** Confirm the exact `AgentState` variants herdr
   exposes and which count as "stopped" (idle, and any waiting/blocked/
   needs-input state). If herdr only exposes idle/working, "finished" and
   "blocked/needs-input" both collapse to idle — reset on working→idle is
   sufficient. Verify whether a permission/input-wait shows as idle or stays
   working (affects whether "needs user action" resets promptly). Grep herdr
   `src/api/schema/events.rs` / the agent-state enum and `detect_state_from_api`.
2. **Persistence** of `reset_at` across daemon restarts — in-memory (simple,
   default) vs a small state-dir file (survives restart). Recommend in-memory.
3. **At-zero coloring** — icon/text marker (simple) vs the `$cache_alert`
   token-swap trick (real red, needs a second config token). Recommend starting
   with the icon/text marker; add the swap if the user wants color.
4. **Format** — `Nm` whole minutes (recommended; the row is narrow) vs `MM:SS`.
5. **Should the 60m be configurable** (`cache_minutes` in the plugin
   `config.toml`, default 60)? Likely yes, cheap to add.

## Testing / verification

- Unit: a pure `remaining_minutes(reset_at, now, total)` helper (clamped 0..=60)
  + the transition logic (working→idle sets reset_at; staying working refreshes
  it) — testable without herdr.
- Live (herdr 0.7.4 on Windows): run a claude agent, watch its agents-panel
  entry show `~60m` when it goes idle and tick down; confirm reset when it
  resumes then re-idles; confirm `0m` alert after the window.
- CI matrix (ubuntu + windows) stays green; keep Linux paths cfg-clean.
