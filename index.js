#!/usr/bin/env node
// @ts-check
'use strict';

/**
 * @typedef {object} Space
 * @property {string} id            herdr workspace id
 * @property {string} label
 * @property {boolean} focused
 * @property {number} paneCount
 * @property {string} branch        git branch of the first pane's cwd ('' if none)
 * @property {number[]} roots       shell PIDs of each pane (process-tree roots)
 * @property {string[]} agentPanes  panes with a real agent
 * @property {string[]} sparePanes  plain shell panes
 * @property {string[]} pseudoPanes panes carrying our "usage" pseudo-agent
 * @property {number} cpu           CPU % of the whole machine (all cores), filled by measure()
 * @property {number} ramMb         RSS MB, filled by measure()
 * @property {number} procCount     processes counted, filled by measure()
 * @property {string} [familyParent]      workspace id of the worktree-group parent
 * @property {string[]} [worktreeLabels]  labels of folded worktree children
 */

/*
 * Space Usage — CPU / RAM per herdr space (workspace).
 *
 * For every workspace herdr reports, we find each pane's shell process
 * (via `herdr pane process-info`), walk that PID's /proc subtree, and sum
 * CPU% (from utime+stime deltas over a sample window, normalized across all
 * CPU cores so it reads as a share of the whole machine, 0..100 — matching the
 * RAM percentage and the sidebar's system header) and RSS memory.
 * Results are printed grouped by space, with usage shown under the branch name.
 *
 * Modes:
 *   --once            print a single snapshot and exit (used by the action)
 *   --interval N      live watch, refreshing every N seconds (used by the pane)
 *   --json            emit machine-readable JSON and exit
 *   --enable          start the sidebar status updater daemon
 *   --disable         stop the daemon and clear statuses
 *   --toggle          enable/disable depending on daemon state
 *   --daemon          internal: run the updater loop (spawned by --enable)
 *
 * Linux-only: relies on /proc. herdr injects HERDR_BIN_PATH / HERDR_PLUGIN_ROOT.
 */

const { execFileSync, spawn } = require('node:child_process');
const fs = require('node:fs');
const net = require('node:net');
const os = require('node:os');

const HERDR = process.env.HERDR_BIN_PATH || 'herdr';

// Probed via getconf for portability (e.g. 16K pages on some aarch64 distros).
/** @param {string} name @param {number} fallback @returns {number} */
function sysconf(name, fallback) {
  try {
    return Number(execFileSync('getconf', [name], { encoding: 'utf8' }).trim()) || fallback;
  } catch {
    return fallback;
  }
}
const CLK_TCK = sysconf('CLK_TCK', 100); // /proc stat times are in these ticks
const PAGE_SIZE = sysconf('PAGESIZE', 4096); // bytes per page
const NPROC = os.cpus().length || 1; // logical CPUs — normalizes CPU% to the whole machine

// ---- CLI --------------------------------------------------------------------

const argv = process.argv.slice(2);
const MODE_ONCE = argv.includes('--once');
const MODE_JSON = argv.includes('--json');
const MODE_ENABLE = argv.includes('--enable');
const MODE_DISABLE = argv.includes('--disable');
const MODE_TOGGLE = argv.includes('--toggle');
const MODE_DAEMON = argv.includes('--daemon');
const intervalArg = Number(argv[argv.indexOf('--interval') + 1]);
const INTERVAL_MS = Number.isFinite(intervalArg) && intervalArg > 0 ? intervalArg * 1000 : 2000;

/** @param {number} ms @returns {Promise<void>} */
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// ---- herdr / git ------------------------------------------------------------

/** @param {string[]} args @returns {any} */
function herdr(args) {
  const out = execFileSync(HERDR, args, { encoding: 'utf8' });
  if (!out.trim()) return {}; // some commands (e.g. report-metadata) print nothing on success
  const parsed = JSON.parse(out);
  if (parsed.error) throw new Error(parsed.error.message || 'herdr error');
  return parsed.result || {};
}

// Raw socket call for API methods without a CLI wrapper (client.window_title.*).
/** @param {string} method @param {object} params @returns {Promise<any>} */
function herdrApi(method, params) {
  const socketPath =
    process.env.HERDR_SOCKET_PATH || `${os.homedir()}/.config/herdr/herdr.sock`;
  return new Promise((resolve, reject) => {
    const sock = net.createConnection(socketPath);
    const timer = setTimeout(() => {
      sock.destroy();
      reject(new Error('herdr socket timeout'));
    }, 2000);
    let buf = '';
    sock.on('connect', () =>
      sock.write(JSON.stringify({ id: `ezcorp.space-usage:${method}`, method, params }) + '\n'),
    );
    sock.on('data', (/** @type {Buffer} */ chunk) => {
      buf += chunk;
      if (!buf.includes('\n')) return;
      clearTimeout(timer);
      sock.end();
      try {
        const parsed = JSON.parse(buf.trim());
        if (parsed.error) reject(new Error(parsed.error.message || 'herdr error'));
        else resolve(parsed.result || {});
      } catch (err) {
        reject(err);
      }
    });
    sock.on('error', (/** @type {Error} */ err) => {
      clearTimeout(timer);
      reject(err);
    });
  });
}

/** @param {string|null} cwd @returns {string} */
function gitBranch(cwd) {
  if (!cwd) return '';
  try {
    return execFileSync('git', ['-C', cwd, 'rev-parse', '--abbrev-ref', 'HEAD'], {
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
    }).trim();
  } catch {
    return '';
  }
}

// ---- /proc ------------------------------------------------------------------

// pid -> { ppid, jiffies }.  Reads only /proc/<pid>/stat.
/** @returns {Map<number, {ppid: number, jiffies: number}>} */
function scanProc() {
  const procs = new Map();
  for (const name of fs.readdirSync('/proc')) {
    if (name.charCodeAt(0) < 48 || name.charCodeAt(0) > 57) continue; // digits only
    const pid = Number(name);
    try {
      const stat = fs.readFileSync(`/proc/${pid}/stat`, 'utf8');
      // comm (field 2) may contain spaces/parens; everything after the last
      // ')' is space-delimited starting at field 3 (state).
      const rest = stat.slice(stat.lastIndexOf(')') + 2).split(' ');
      procs.set(pid, {
        ppid: Number(rest[1]), // field 4
        jiffies: Number(rest[11]) + Number(rest[12]), // utime(14) + stime(15)
      });
    } catch {
      // process vanished mid-scan — ignore
    }
  }
  return procs;
}

/** @param {Map<number, {ppid: number, jiffies: number}>} procs @returns {Map<number, number[]>} */
function childrenMap(procs) {
  const kids = new Map();
  for (const [pid, p] of procs) {
    const bucket = kids.get(p.ppid);
    if (bucket) bucket.push(pid);
    else kids.set(p.ppid, [pid]);
  }
  return kids;
}

/** @param {number} rootPid @param {Map<number, number[]>} kids @returns {Set<number>} */
function subtree(rootPid, kids) {
  const out = new Set();
  const stack = [rootPid];
  while (stack.length) {
    const pid = stack.pop();
    if (pid === undefined || out.has(pid)) continue;
    out.add(pid);
    for (const c of kids.get(pid) || []) stack.push(c);
  }
  return out;
}

let MEM_TOTAL_MB = 0;
function memTotalMb() {
  if (!MEM_TOTAL_MB) {
    const m = /^MemTotal:\s+(\d+) kB/m.exec(fs.readFileSync('/proc/meminfo', 'utf8'));
    MEM_TOTAL_MB = m ? Number(m[1]) / 1024 : 0;
  }
  return MEM_TOTAL_MB;
}

// "4%" of total system RAM, or "" if /proc/meminfo is unreadable.
/** @param {number} mb @returns {string} */
function ramPct(mb) {
  const total = memTotalMb();
  return total ? `${Math.round((100 * mb) / total)}%` : '';
}

/** @param {Iterable<number>} pids @returns {number} */
function rssMb(pids) {
  let bytes = 0;
  for (const pid of pids) {
    try {
      const resident = Number(fs.readFileSync(`/proc/${pid}/statm`, 'utf8').split(' ')[1]);
      bytes += resident * PAGE_SIZE;
    } catch {
      // exited — ignore
    }
  }
  return bytes / (1024 * 1024);
}

// ---- collection -------------------------------------------------------------

// Enumerate spaces and the root shell PID of each of their panes.
/** @returns {Space[]} */
function collectSpaces() {
  const workspaces = herdr(['workspace', 'list']).workspaces || [];
  return workspaces.map((/** @type {any} */ ws) => {
    const panes = herdr(['pane', 'list', '--workspace', ws.workspace_id]).panes || [];
    const roots = [];
    const agentPanes = []; // panes with a real agent — these get sidebar rows
    const sparePanes = []; // plain shell panes — candidates for our pseudo-agent entry
    const pseudoPanes = []; // panes already carrying our "usage" pseudo-agent
    let cwd = null;
    for (const pane of panes) {
      if (!cwd && pane.cwd) cwd = pane.cwd;
      if (pane.agent === PSEUDO_AGENT) pseudoPanes.push(pane.pane_id);
      else if (pane.agent) agentPanes.push(pane.pane_id);
      else sparePanes.push(pane.pane_id);
      try {
        const info = herdr(['pane', 'process-info', '--pane', pane.pane_id]).process_info;
        if (info && info.shell_pid) roots.push(info.shell_pid);
      } catch {
        // pane may have just closed — skip it
      }
    }
    return {
      id: ws.workspace_id,
      label: ws.label || ws.workspace_id,
      focused: !!ws.focused,
      paneCount: panes.length,
      branch: gitBranch(cwd),
      roots,
      agentPanes,
      sparePanes,
      pseudoPanes,
      cpu: 0,
      ramMb: 0,
      procCount: 0,
    };
  });
}

// Sample CPU over `windowMs`, then fill cpu / ramMb / procCount on each space.
/** @param {Space[]} spaces @param {number} windowMs @returns {Promise<Space[]>} */
async function measure(spaces, windowMs) {
  const before = scanProc();
  const t0 = process.hrtime.bigint();
  await sleep(windowMs);
  const after = scanProc();
  const elapsed = Number(process.hrtime.bigint() - t0) / 1e9;
  const kids = childrenMap(after);

  for (const sp of spaces) {
    const pids = new Set();
    for (const root of sp.roots) for (const pid of subtree(root, kids)) pids.add(pid);

    let deltaJiffies = 0;
    for (const pid of pids) {
      const a = after.get(pid);
      const b = before.get(pid);
      if (a && b) deltaJiffies += Math.max(0, a.jiffies - b.jiffies);
    }
    // Divide by NPROC so a fully-busy core reads as 100/NPROC rather than 100:
    // CPU% is a share of the whole machine (0..100), consistent with the RAM
    // percentage and the patched sidebar's system-usage header.
    sp.cpu = elapsed > 0 ? (100 * (deltaJiffies / CLK_TCK)) / elapsed / NPROC : 0;
    sp.ramMb = rssMb(pids);
    sp.procCount = pids.size;
  }
  return spaces;
}

// Mark worktree-child workspaces with their group parent (one `herdr worktree
// list` call per unique repo). Children of a repo whose main checkout is open
// as a workspace belong to that parent's family.
/** @param {Space[]} spaces */
function groupWorktreeFamilies(spaces) {
  const byId = new Map(spaces.map((s) => [s.id, s]));
  const seenRepos = new Set();
  for (const sp of spaces) {
    let res;
    try {
      res = herdr(['worktree', 'list', '--workspace', sp.id]);
    } catch {
      continue; // workspace isn't in a git repo
    }
    const repoKey = res.source && res.source.repo_key;
    if (!repoKey || seenRepos.has(repoKey)) continue;
    seenRepos.add(repoKey);
    const parent = res.source.source_workspace_id && byId.get(res.source.source_workspace_id);
    if (!parent) continue; // main checkout isn't open — children stay standalone
    for (const wt of res.worktrees || []) {
      const child = wt.open_workspace_id && byId.get(wt.open_workspace_id);
      if (child && child.id !== parent.id) child.familyParent = parent.id;
    }
  }
}

// Fold measured children into their parent: the sidebar renders indented
// worktree children as single-line rows (no status row), so the parent's
// line carries the family total. Returns spaces without the folded children.
/** @param {Space[]} spaces @returns {Space[]} */
function aggregateFamilies(spaces) {
  const byId = new Map(spaces.map((s) => [s.id, s]));
  for (const sp of spaces) {
    if (!sp.familyParent) continue;
    const parent = byId.get(sp.familyParent);
    if (!parent) continue;
    parent.cpu += sp.cpu;
    parent.ramMb += sp.ramMb;
    parent.procCount += sp.procCount;
    parent.paneCount += sp.paneCount;
    (parent.worktreeLabels ||= []).push(sp.label);
  }
  return spaces.filter((s) => !s.familyParent);
}

/** @param {number} windowMs @returns {Promise<Space[]>} */
async function snapshot(windowMs) {
  const spaces = collectSpaces();
  groupWorktreeFamilies(spaces);
  await measure(spaces, windowMs);
  return aggregateFamilies(spaces);
}

// ---- sidebar status updater -------------------------------------------------
//
// Pushes each space's usage as display-only `custom_status` metadata (TTL'd)
// onto a spare shell pane, which the locally patched herdr renders as a third
// row in the sidebar spaces card (the patch prefers non-agent pane statuses).
// Statuses self-clear via TTL if the updater dies.

const PLUGIN_ID = process.env.HERDR_PLUGIN_ID || 'ezcorp.space-usage';
const PSEUDO_AGENT = 'usage'; // agents-panel mode label; also cleaned up in sidebar mode
const STATE_DIR = process.env.HERDR_PLUGIN_STATE_DIR || `${os.tmpdir()}/${PLUGIN_ID}`;
const CONFIG_DIR = process.env.HERDR_PLUGIN_CONFIG_DIR || `${os.tmpdir()}/${PLUGIN_ID}-config`;
const PID_FILE = `${STATE_DIR}/updater.pid`;

// User config: flat `key = value` lines in $HERDR_PLUGIN_CONFIG_DIR/config.toml.
//   mode = "agents-panel" | "sidebar"
//     agents-panel (default): works on stock herdr — each space gets a "usage"
//       pseudo-agent entry in the sidebar agents panel.
//     sidebar: for herdr builds carrying the space-usage sidebar patch — pushes
//       display-only metadata that renders inside the spaces card itself.
//   interval_seconds = 5        daemon refresh cadence
//   window_title_totals = true  write all-space totals to the client window title
function loadConfig() {
  const cfg = { mode: 'agents-panel', interval_seconds: 5, window_title_totals: true };
  let text;
  try {
    text = fs.readFileSync(`${CONFIG_DIR}/config.toml`, 'utf8');
  } catch {
    return cfg; // no config file — defaults
  }
  for (const line of text.split('\n')) {
    if (line.trim().startsWith('#')) continue;
    const m = /^\s*([A-Za-z_]+)\s*=\s*(.+?)\s*$/.exec(line);
    if (!m) continue;
    const value = m[2].replace(/^["']|["']$/g, '');
    if (m[1] === 'mode' && (value === 'sidebar' || value === 'agents-panel')) cfg.mode = value;
    else if (m[1] === 'interval_seconds' && Number(value) >= 1) cfg.interval_seconds = Number(value);
    else if (m[1] === 'window_title_totals') cfg.window_title_totals = value !== 'false';
  }
  return cfg;
}
const CONFIG = loadConfig();

// The cpu/ram label tokens are read from herdr's OWN config.toml `[ui]` section
// (`cpu_label` / `ram_label`), so the plugin's per-space rows match the patched
// sidebar's system-usage header — one source of truth. Users can swap them for
// nerd-font icons. Falls back to "cpu"/"ram" if the file or keys are absent.
// Loaded once at start; restart the daemon (or reopen the pane) to pick up edits.
function loadHerdrLabels() {
  const labels = { cpu: 'cpu', ram: 'ram' };
  const configHome = process.env.XDG_CONFIG_HOME || `${os.homedir()}/.config`;
  let text;
  try {
    text = fs.readFileSync(`${configHome}/herdr/config.toml`, 'utf8');
  } catch {
    return labels; // no herdr config readable — defaults
  }
  let inUi = false;
  for (const raw of text.split('\n')) {
    const line = raw.trim();
    if (!line || line.startsWith('#')) continue;
    const section = /^\[([^\]]+)\]/.exec(line);
    if (section) {
      inUi = section[1].trim() === 'ui'; // [ui] only, not [ui.toast] etc.
      continue;
    }
    if (!inUi) continue;
    const m = /^(cpu_label|ram_label)\s*=\s*(.+?)\s*$/.exec(line);
    if (!m) continue;
    const value = m[2].replace(/^["']|["']$/g, '');
    if (m[1] === 'cpu_label') labels.cpu = value;
    else labels.ram = value;
  }
  return labels;
}
const LABELS = loadHerdrLabels();

const DAEMON_INTERVAL_MS = CONFIG.interval_seconds * 1000;
const STATUS_TTL_MS = DAEMON_INTERVAL_MS * 3;

/** @param {number} mb @returns {string} */
const compactRam = (mb) => (mb >= 1024 ? `${(mb / 1024).toFixed(1)}G` : `${Math.round(mb)}M`);

// PID of a live updater daemon, or null (missing pid file / dead process).
/** @returns {number|null} */
function daemonPid() {
  try {
    const pid = Number(fs.readFileSync(PID_FILE, 'utf8').trim());
    if (pid > 0) {
      process.kill(pid, 0);
      return pid;
    }
  } catch {
    // fall through
  }
  return null;
}

/** @param {string} body */
function notify(body) {
  try {
    execFileSync(HERDR, ['notification', 'show', 'Space usage', '--body', body], {
      stdio: 'ignore',
    });
  } catch {
    // toast is best-effort
  }
}

/** @param {string} paneId */
function releasePseudo(paneId) {
  try {
    herdr(['pane', 'release-agent', paneId, '--source', PLUGIN_ID, '--agent', PSEUDO_AGENT]);
  } catch {
    // pane gone — nothing to release
  }
}

// Give each space a status line, mode-dependent:
//  - sidebar mode (patched herdr): display-only metadata on a spare shell pane
//    (else the first agent pane). The patch renders it inside the spaces card
//    and its agents panel ignores statuses not reported by the agent's own
//    integration — our text never rides an agent's panel row.
//  - agents-panel mode (stock herdr): a "usage" pseudo-agent on a spare shell
//    pane gives each space its own 2-row entry in the agents panel; spaces
//    with no spare pane fall back to metadata on the agent's row.
/** @param {Space[]} spaces @param {{pseudo: Set<string>, metadata: Set<string>}} tracked */
function pushStatuses(spaces, tracked) {
  for (const sp of spaces) {
    const status = `${LABELS.cpu} ${sp.cpu.toFixed(0)}% · ${LABELS.ram} ${ramPct(sp.ramMb) || compactRam(sp.ramMb)}`;

    if (CONFIG.mode === 'agents-panel') {
      for (const extra of sp.pseudoPanes.slice(1)) releasePseudo(extra); // stale claims
      const pane = sp.pseudoPanes[0] || sp.sparePanes[0];
      if (pane) {
        try {
          herdr([
            'pane', 'report-agent', pane,
            '--source', PLUGIN_ID,
            '--agent', PSEUDO_AGENT,
            '--state', 'idle',
            '--custom-status', status,
          ]);
          tracked.pseudo.add(pane);
          continue; // dedicated panel entry covers this space
        } catch {
          // pane just closed — fall through to metadata
        }
      }
    } else {
      // sidebar mode: release pseudo-agents left over from agents-panel mode
      // or pre-v0.5 versions (report-agent entries have no TTL).
      for (const paneId of sp.pseudoPanes) releasePseudo(paneId);
    }

    const targets = sp.sparePanes.length ? [sp.sparePanes[0]] : sp.agentPanes.slice(0, 1);
    for (const paneId of targets) {
      try {
        herdr([
          'pane', 'report-metadata', paneId,
          '--source', PLUGIN_ID,
          '--custom-status', status,
          '--ttl-ms', String(STATUS_TTL_MS),
        ]);
        tracked.metadata.add(paneId);
      } catch {
        // pane just closed — skip
      }
    }
  }
}

/** @param {{pseudo: Iterable<string>, metadata: Iterable<string>}} tracked */
function clearAll(tracked) {
  for (const paneId of tracked.pseudo) releasePseudo(paneId);
  for (const paneId of tracked.metadata) {
    try {
      herdr(['pane', 'report-metadata', paneId, '--source', PLUGIN_ID, '--clear-custom-status']);
    } catch {
      // pane gone — nothing to clear
    }
  }
}

// Total across all spaces, shown in the client window title.
/** @param {Space[]} spaces @returns {Promise<any>} */
function setTitleTotals(spaces) {
  let cpu = 0;
  let ramMb = 0;
  for (const sp of spaces) {
    cpu += sp.cpu;
    ramMb += sp.ramMb;
  }
  const title = `spaces · ${LABELS.cpu} ${cpu.toFixed(0)}% · ${LABELS.ram} ${ramPct(ramMb) || compactRam(ramMb)}`;
  return herdrApi('client.window_title.set', { title }).catch(() => {});
}

const clearTitle = () => herdrApi('client.window_title.clear', {}).catch(() => {});

async function runDaemon() {
  if (daemonPid()) return; // another updater is already live
  fs.mkdirSync(STATE_DIR, { recursive: true });
  fs.writeFileSync(PID_FILE, `${process.pid}\n`);

  const tracked = { pseudo: new Set(), metadata: new Set() };
  let stopping = false;
  const shutdown = async () => {
    if (stopping) return;
    stopping = true; // parks the main loop — it must not re-report after clearAll
    clearAll(tracked);
    await clearTitle();
    try {
      fs.unlinkSync(PID_FILE);
    } catch {
      // already removed
    }
    process.exit(0);
  };
  process.on('SIGINT', shutdown);
  process.on('SIGTERM', shutdown);

  let windowMs = 500; // quick first sample so the sidebar updates immediately
  let failures = 0;
  for (;;) {
    try {
      const spaces = await snapshot(windowMs); // snapshot sleeps windowMs
      if (stopping) return; // shutdown ran during the sample window
      pushStatuses(spaces, tracked);
      if (CONFIG.window_title_totals) await setTitleTotals(spaces);
      failures = 0;
    } catch {
      if (++failures >= 5) await shutdown(); // herdr server likely gone
      await sleep(1000);
      if (stopping) return;
    }
    windowMs = DAEMON_INTERVAL_MS;
  }
}

function enableUpdater() {
  if (daemonPid()) {
    notify('sidebar usage already enabled');
    return;
  }
  const child = spawn(process.execPath, [__filename, '--daemon'], {
    detached: true,
    stdio: 'ignore',
  });
  child.unref();
  notify('sidebar usage enabled');
}

async function disableUpdater() {
  const pid = daemonPid();
  if (pid) {
    try {
      process.kill(pid, 'SIGTERM'); // daemon clears its statuses + title on shutdown
    } catch {
      // exited between check and kill
    }
  }
  // Belt and braces: sweep every current pane in case the daemon died —
  // release pseudo-agents (no TTL) and clear metadata statuses.
  try {
    const spaces = collectSpaces();
    clearAll({
      pseudo: spaces.flatMap((s) => s.pseudoPanes),
      metadata: spaces.flatMap((s) => [...s.agentPanes, ...s.sparePanes]),
    });
  } catch {
    // herdr unavailable — metadata TTL will expire the statuses anyway
  }
  await clearTitle();
  notify('sidebar usage disabled');
}

// ---- rendering --------------------------------------------------------------

const useColor = process.stdout.isTTY && !process.env.NO_COLOR;
/** @param {string} code @param {string} s @returns {string} */
const paint = (code, s) => (useColor ? `\x1b[${code}m${s}\x1b[0m` : s);
/** @param {string} s */
const dim = (s) => paint('2', s);
/** @param {string} s */
const bold = (s) => paint('1', s);
/** @param {string} s */
const green = (s) => paint('32', s);
/** @param {string} s */
const yellow = (s) => paint('33', s);
/** @param {string} s */
const red = (s) => paint('31', s);

/** @param {number} v */
function cpuColor(v) {
  if (v >= 80) return red;
  if (v >= 40) return yellow;
  return green;
}

/** @param {number} mb @returns {string} */
function fmtRam(mb) {
  return mb >= 1024 ? `${(mb / 1024).toFixed(2)} GB` : `${mb.toFixed(0)} MB`;
}

/** @param {Space[]} spaces @returns {string} */
function render(spaces) {
  const lines = [bold('  CPU / RAM per space'), ''];
  if (!spaces.length) {
    lines.push(dim('  No spaces open.'));
    return lines.join('\n');
  }

  let totalCpu = 0;
  let totalRam = 0;
  for (const sp of spaces) {
    totalCpu += sp.cpu;
    totalRam += sp.ramMb;
    const marker = sp.focused ? green('●') : dim('○');
    const branch = sp.branch ? sp.branch : '(no branch)';
    const cpuStr = cpuColor(sp.cpu)(`${sp.cpu.toFixed(1)}%`.padStart(6));
    const pct = ramPct(sp.ramMb);
    lines.push(`  ${marker} ${bold(sp.label)}`);
    lines.push(`      ${dim(branch)}`);
    const notes = [`· ${sp.paneCount} pane${sp.paneCount === 1 ? '' : 's'}`];
    if (sp.worktreeLabels) {
      notes.push(`· +${sp.worktreeLabels.length} worktree${sp.worktreeLabels.length === 1 ? '' : 's'}`);
    }
    lines.push(
      `      ${LABELS.cpu} ${cpuStr}   ${LABELS.ram} ${fmtRam(sp.ramMb).padStart(8)}${pct ? dim(` (${pct})`) : ''}   ${dim(notes.join(' '))}`,
    );
    lines.push('');
  }
  const totalPct = ramPct(totalRam);
  lines.push(
    dim(
      `  ── total   ${LABELS.cpu} ${totalCpu.toFixed(1)}%   ${LABELS.ram} ${fmtRam(totalRam)}${totalPct ? ` (${totalPct})` : ''}`,
    ),
  );
  return lines.join('\n');
}

// ---- main -------------------------------------------------------------------

async function main() {
  if (MODE_DAEMON) return runDaemon();
  if (MODE_ENABLE) return enableUpdater();
  if (MODE_DISABLE) return disableUpdater();
  if (MODE_TOGGLE) return daemonPid() ? disableUpdater() : enableUpdater();

  if (MODE_JSON) {
    const spaces = await snapshot(300);
    const payload = spaces.map((s) => ({
      workspace_id: s.id,
      label: s.label,
      branch: s.branch,
      focused: s.focused,
      panes: s.paneCount,
      processes: s.procCount,
      cpu_percent: Number(s.cpu.toFixed(1)),
      ram_mb: Number(s.ramMb.toFixed(1)),
      ram_percent: memTotalMb() ? Number(((100 * s.ramMb) / memTotalMb()).toFixed(1)) : null,
      ...(s.worktreeLabels ? { includes_worktrees: s.worktreeLabels } : {}),
    }));
    process.stdout.write(JSON.stringify(payload, null, 2) + '\n');
    return;
  }

  if (MODE_ONCE) {
    process.stdout.write(render(await snapshot(300)) + '\n');
    return;
  }

  // Live watch: first frame quick, then full-interval CPU windows.
  const restore = () => {
    process.stdout.write('\x1b[?25h'); // show cursor
    process.exit(0);
  };
  process.on('SIGINT', restore);
  process.on('SIGTERM', restore);
  process.stdout.write('\x1b[?25l'); // hide cursor

  let windowMs = 400;
  for (;;) {
    let body;
    try {
      body = render(await snapshot(windowMs));
    } catch (err) {
      body = `${red('  herdr unavailable:')} ${err instanceof Error ? err.message : String(err)}`;
    }
    const stamp = new Date().toLocaleTimeString();
    process.stdout.write(
      `\x1b[2J\x1b[H${body}\n\n${dim(`  refreshing every ${INTERVAL_MS / 1000}s · ${stamp} · ctrl-c to quit`)}\n`,
    );
    windowMs = INTERVAL_MS;
  }
}

main().catch((err) => {
  process.stderr.write(`space-usage: ${err.message}\n`);
  process.exit(1);
});
