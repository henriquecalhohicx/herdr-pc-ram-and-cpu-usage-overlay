# herdr-pc-ram-and-cpu-usage-overlay
<img width="393" height="243" alt="Screenshot 2026-07-11 at 2 41 03 PM" src="https://github.com/user-attachments/assets/720aace3-4aa2-4474-a8ae-3197b84f4f79" />

A [herdr](https://herdr.dev) plugin that shows **live CPU and RAM usage per
space (workspace)** — so when you're running a herd of agents, you can see at
a glance which space is eating your machine.

```
● web-app
    main
    cpu 26% · ram 8%          ← spaces card (sidebar mode, patched herdr)

⚡ web-app
    idle · usage · cpu 26% · ram 8%   ← agents panel (default mode, stock herdr)
```

- Per-space CPU% and RAM%, both a share of the **whole machine** (0–100%, so a
  busy space reads e.g. `cpu 4%` — not a per-core figure that can exceed 100%),
  refreshed every 5s
- **Worktree-aware**: workspaces opened as worktree children are folded into
  their parent space's total
- All-space totals in your terminal's window title: `spaces · cpu 39% · ram 8%`
- A live dashboard pane and one-shot report/JSON actions
- A small static Rust binary (~2–5 MB resident) that talks to herdr over its
  unix socket — no per-sample subprocess spawns, no Node runtime

## Install

```sh
herdr plugin install ezcorp-org/herdr-pc-ram-and-cpu-usage-overlay
```

Requirements: Linux (reads `/proc`) or Windows, and the **Rust toolchain**
(`cargo`) on the box hosting the herdr server — herdr compiles the plugin at
install time via `cargo build --release`. Plugins run on the machine hosting
the herdr server, so remote setups need these on the server box only. `node`
is no longer required. macOS is not supported — the CPU/RAM sampling relies on
`/proc`, which macOS doesn't have.

## Usage

Toggle the background updater (statuses appear in the sidebar within ~5s):

```sh
herdr plugin action invoke status-toggle --plugin ez-corp.space-usage
```

Other entrypoints:

```sh
herdr plugin pane open --plugin ez-corp.space-usage --entrypoint dashboard  # live dashboard
herdr plugin action invoke report --plugin ez-corp.space-usage             # one-shot snapshot
./target/release/space-usage --json                                        # machine-readable
```

Statuses carry a TTL and self-clear if the updater dies; disabling clears
everything immediately.

On Windows, herdr registers `-win`-suffixed entrypoint/action ids and builds a
`.exe`, so use `status-toggle-win`, `dashboard-win`, `report-win`,
`status-enable-win`, `status-disable-win`, and `.\target\release\space-usage.exe`
in place of the ids and path above.

## Modes

Configure in `$HERDR_PLUGIN_CONFIG_DIR/config.toml`
(herdr prints the config dir via `herdr plugin config-dir ez-corp.space-usage`):

```toml
mode = "agents-panel"       # default — works on stock herdr
# mode = "sidebar"          # for herdr builds with the sidebar patch (below)
interval_seconds = 5
window_title_totals = true
```

- **agents-panel** (default): each space gets its own two-row entry in the
  sidebar agents panel via a `usage` pseudo-agent on a spare shell pane.
- **sidebar**: renders usage as a third line inside each spaces card, under the
  branch name.

Switching modes cleans up after the other mode automatically.

### Spaces card (native, no patch — herdr with metadata tokens)

Recent herdr (the metadata-token feature, ex-discussion #713) can render the
usage line **in the spaces card on a stock build** — no patch. The daemon
pushes each space's usage as a TTL'd `usage` workspace metadata token via
`workspace.report_metadata`; you surface it by adding a `$usage` token row to
your herdr `config.toml`:

```toml
[ui.sidebar.spaces]
rows = [
    ["state_icon", "workspace"],   # herdr defaults
    ["branch", "git_status"],      #
    ["$usage"],                    # ← this plugin's per-space cpu/ram
]
```

Enable the updater (`status-toggle` / `status-toggle-win` on Windows) and
`herdr config` reload. The token self-clears (TTL) if the daemon stops.

To show it in the **agents panel** instead (on the `usage` pseudo-agent entry),
the daemon also pushes `$usage` as a pane token in agents-panel mode; add it to
`[ui.sidebar.agents]`:

```toml
[ui.sidebar.agents]
rows = [
    ["state_icon", "workspace", "tab"],   # herdr defaults
    ["agent"],                            #
    ["$usage"],                           # ← usage entry's cpu/ram
]
```

Other agents carry no `$usage` token, so herdr elides that row for them.

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
- `cpu_label` / `ram_label` — override the cpu/ram labels (default: herdr's
  `[ui]` `cpu_label`/`ram_label`). Set short text or an icon to fit a narrow
  sidebar, e.g. `cpu_label = "⚡"`.
- `cache_label` — prefix on the countdown value (default `"cache"`), e.g.
  `cache_label = "⏳"` renders `⏳ 60m`; empty renders just `60m`.

> The older note here said the spaces card "requires a patched herdr". That
> predates herdr's native metadata-token surface; on builds that have it, the
> config above is all you need. The AGPL patch is no longer necessary.

## Labels

The `cpu`/`ram` tokens are read from herdr's own `config.toml` `[ui]`
(`cpu_label` / `ram_label`, default `cpu`/`ram`) — set them to nerd-font icons to
taste. On a patched build this also matches the sidebar's system-usage header,
which reads the same two keys. Restart the updater to pick up a change.

## How it works

The binary opens one persistent connection to the herdr unix socket and speaks
its newline-delimited JSON-RPC. Per refresh: `session.snapshot` returns every
workspace and pane in a single call → `pane.process_info` yields each pane's
`shell_pid` → the process walks that PID's `/proc` subtree, summing CPU
(utime+stime jiffie deltas over a sample window) and RSS. Branch comes from the
pane cwd's git checkout, and worktree families from `worktree.list`. Clock ticks
(`_SC_CLK_TCK`) and page size (`_SC_PAGESIZE`) are probed via `sysconf`.

## Development

```sh
git clone <this repo>
cd herdr-pc-ram-and-cpu-usage-overlay
cargo build --release
herdr plugin link .
```

`herdr plugin link` references the directory in place and does **not** run the
build step, so run `cargo build --release` first — the linked commands invoke
`./target/release/space-usage`. (`herdr plugin install` builds automatically.)

## License

MIT — see [LICENSE](LICENSE).
