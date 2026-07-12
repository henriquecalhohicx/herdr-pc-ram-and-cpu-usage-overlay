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
- Zero dependencies — a single `index.js`, no build step

## Install

```sh
herdr plugin install ezcorp-org/herdr-pc-ram-and-cpu-usage-overlay
```

Requirements: Linux (reads `/proc`), `node` ≥ 18 on PATH. Plugins run on the
machine hosting the herdr server, so remote setups need these on the server
box only.

## Usage

Toggle the background updater (statuses appear in the sidebar within ~5s):

```sh
herdr plugin action invoke status-toggle --plugin ez-corp.space-usage
```

Other entrypoints:

```sh
herdr plugin pane open --plugin ez-corp.space-usage --entrypoint dashboard  # live dashboard
herdr plugin action invoke report --plugin ez-corp.space-usage             # one-shot snapshot
node index.js --json                                                   # machine-readable
```

Statuses carry a TTL and self-clear if the updater dies; disabling clears
everything immediately.

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
  branch name. Requires a herdr build patched to add a spaces-card status row —
  a native plugin sidebar surface is an open upstream request (discussion #713).
  That patch modifies herdr itself (AGPL-3.0), so it is **not** distributed with
  this MIT plugin; agents-panel mode (above) works on stock herdr with no patch.

Switching modes cleans up after the other mode automatically.

## Labels

The `cpu`/`ram` tokens are read from herdr's own `config.toml` `[ui]`
(`cpu_label` / `ram_label`, default `cpu`/`ram`) — set them to nerd-font icons to
taste. On a patched build this also matches the sidebar's system-usage header,
which reads the same two keys. Restart the updater to pick up a change.

## How it works

Per space: panes → each pane's `shell_pid` (via `herdr pane process-info`) →
walk the `/proc` subtree, summing CPU (utime+stime deltas over a sample
window) and RSS. Branch comes from the pane cwd's git checkout. Worktree
families come from `herdr worktree list`. Page size and clock ticks are probed
with `getconf` for portability.

## Development

```sh
git clone <this repo> && herdr plugin link ./herdr-pc-ram-and-cpu-usage-overlay
```

The file is plain JavaScript with `// @ts-check` + JSDoc types — editors with
TypeScript check it as-is (`jsconfig.json` included); no build step.

## License

MIT — see [LICENSE](LICENSE).
