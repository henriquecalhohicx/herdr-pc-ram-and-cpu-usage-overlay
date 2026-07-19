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
