# CLAUDE.md

Guidance for working in this repo (a Rust herdr plugin: CPU/RAM per space).

## Build / test (Windows dev)

- Tool shells often have a **stale PATH** — prepend cargo before any cargo call:
  - PowerShell: `$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"; cargo ...`
  - bash: `export PATH="$HOME/.cargo/bin:$PATH"; cargo ...`
- **Kill the running `space-usage` daemon before `cargo build`.** A live daemon
  holds `target/release/space-usage.exe` open, so a build fails with
  `Access is denied. (os error 5)`. Disable it (`herdr plugin action invoke
  status-disable-win`) and/or `Get-Process space-usage | Stop-Process -Force`
  first, then build, then re-enable.
- CI gate (must pass on **both** ubuntu + windows legs): `cargo fmt --all --check`,
  `cargo clippy --all-targets -- -D warnings`, `cargo test --all`.

## Cross-platform rules

- **Linux behavior stays byte-identical.** All Windows code is additive behind
  `#[cfg(windows)]`; Linux code behind `#[cfg(unix)]` / `#[cfg(target_os =
  "linux")]`. `libc`/`signal-hook` are `[target.'cfg(unix)']` deps; `windows-sys`
  is `[target.'cfg(windows)']`.
- Watch for cfg-only-dead-code: a symbol live on one OS but dead on the other
  fails `clippy -D warnings` on that leg (e.g. gate consts/fns with
  `#[cfg(any(unix, test))]` or move them into the platform module).
- Windows CPU metric: `jiffies` = kernel+user 100ns ticks with
  `clk_tck() = 10_000_000`, so `collect.rs`'s CPU math is unchanged vs Linux.

## herdr integration (learned the hard way — verified on herdr 0.7.4)

- **Socket transport:** `HERDR_SOCKET_PATH` is a filesystem-style path, but the
  actual Windows endpoint is the named pipe `\\.\pipe\` + that path. Opening the
  bare path hits a placeholder `.sock` file (sharing violation). See
  `herdr/transport.rs::pipe_name`.
- **Manifest command spawn (Windows):** herdr runs commands with an
  extended-length `\\?\C:\..` cwd, and `CreateProcessW` can't resolve a relative
  `./target/..` exe against it (os error 3). Windows manifest entries use a
  PowerShell launcher that builds an absolute path from `HERDR_PLUGIN_ROOT`
  (stripping `\\?\`). `cmd /c` works for one-shot actions but dies for panes.
- **API field drift:** `pane.report_agent` status goes in `message` (the old
  `custom_status` name is silently ignored by 0.7.4+).
- **Sidebar cpu/ram, no patch:** push custom tokens via
  `workspace.report_metadata` (spaces card) / `pane.report_metadata` (agents
  panel) `tokens` maps (TTL'd); users render them with `$name` tokens in
  `[ui.sidebar.spaces]` / `[ui.sidebar.agents]` `rows`. herdr token color is
  static per config — the plugin can't dynamically color a value.
- herdr rejects duplicate pane/action ids across platform variants, so Windows
  entries use `-win`-suffixed ids.
- Event hooks: `[[events]]` with `on` (an `EventKind`, e.g. `workspace.focused`)
  + `command`. Event commands get the plugin env incl. `HERDR_PLUGIN_ROOT`.
- **`AgentStatus` enum** (authoritative via `herdr api schema`, 0.7.4):
  `idle / working / blocked / done / unknown`. The cache timer treats only
  `working` as actively-working (`WORKING_STATES = ["working"]` in `src/timer.rs`);
  every other value counts down. Claude agents are `agent == "claude"`.
- **Window title** (`window_title_totals`, gated, default on) shows
  `herdr · cpu · ram` plus `· N waiting` = count of `claude` panes in
  `blocked`/`done` (`timer::ATTENTION_STATES`); the tail is omitted at zero to
  avoid jitter. Only visible windowed — fullscreen hides the window title.
- **Claude detection is process-based, not title-based.** herdr sets
  `agent=Some(Claude)` while `claude` is the pane's live foreground process and
  `None` when it exits (see herdr-server.log `agent changed ... process=claude`).
  A summarised tab title (e.g. `✳ General conversation…`) does NOT lose detection.
  Reliable state needs the integration hook installed:
  `herdr integration install claude` (writes `~/.claude/hooks/herdr-agent-state.ps1`
  + registers it) — check with `herdr integration status`.
- **herdr rejects a `report_agent` claim over a pane with a detected agent**
  (log `method=pane.report_agent outcome=error`). But if our daemon claims a
  pane as the `usage` pseudo-agent *before* its agent is detected (a startup
  race), the claim masks the agent indefinitely. The guard
  (`collect::classify_pane` + `pane_has_agent_glyph`: raw `terminal_title` ≠
  `terminal_title_stripped` ⇒ a real agent's glyph is present even under our
  mask) routes such panes to `MaskedPseudo`, and the daemon releases the claim so
  herdr re-surfaces the agent next loop.
- **Token colour is static per config; use the token-swap pattern for dynamic
  colour.** Sidebar rows accept inline token styles:
  `{ token = "$cache", fg = "#RRGGBB", bold = true, dim = false }` (fg strict
  `#RGB`/`#RRGGBB`; omitting fg inherits the default). The cache timer emits its
  value under one of `cache`/`cache_warn`/`cache_alert` per tier and clears the
  others, so the user styles three token names and only the active one shows.
- **Probe / drive herdr from a tool shell** (`HERDR_SOCKET_PATH` is only injected
  into herdr-spawned panes): the server pipe is discoverable —
  `[System.IO.Directory]::GetFiles("\\.\pipe\")` filtered on `herdr` gives
  `...\AppData\Roaming\herdr\herdr.sock`; connect a `NamedPipeClientStream` and
  write one newline-terminated `{"id":..,"method":"pane.list","params":{..}}`.
  CLI levers: `herdr plugin action invoke <id> --plugin <pid>` (e.g.
  `status-enable-win`/`status-disable-win` to start/stop the daemon canonically),
  `herdr server reload-config` (apply sidebar-row edits), `herdr api schema`,
  `herdr plugin config-dir <pid>`.

## Layout

- `proc/` — per-OS metrics backends (`linux.rs` `/proc`+`sysconf`, `windows.rs`
  Win32) behind one API. `herdr/` — JSON-RPC client + `transport` shim.
  `collect.rs`/`config.rs`/`model.rs` are platform-neutral.
- Design/plan docs live in `docs/superpowers/`.
