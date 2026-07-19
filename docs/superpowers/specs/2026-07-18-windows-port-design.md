# Windows Port — Design

**Date:** 2026-07-18
**Status:** Approved for planning
**Scope:** Add Windows support to the `space-usage` herdr plugin while keeping the
Linux build byte-identical. Cross-platform single codebase, `#[cfg]`-gated.

## Goal

The plugin (`ez-corp.space-usage`) shows CPU/RAM per herdr space. Today it is
Linux-only: it reads `/proc` + `sysconf`, talks to herdr over an `AF_UNIX`
socket, and daemonizes with `setsid`/POSIX signals. herdr itself is one
cross-platform Rust binary and on Windows exposes its plugin socket as a
**named pipe** (`HERDR_SOCKET_PATH` carries the pipe name); the newline-delimited
JSON-RPC protocol is identical across platforms.

Port the plugin to Windows using direct Win32 APIs (`windows-sys`) for metrics
and a cfg-gated named-pipe transport shim, leaving all Linux code paths unchanged.

## Non-goals

- macOS (POSIX but no `/proc`; out of scope).
- Changing the JSON-RPC protocol, CPU%/RAM% semantics, or CLI flags.
- Refactoring unrelated code.

## OS-specific surface (current)

| File | Linux dependency |
|------|------------------|
| `proc.rs` | `/proc/<pid>/stat`, `/proc/<pid>/statm`, `/proc/meminfo`, `sysconf` |
| `herdr.rs` | `std::os::unix::net::UnixStream` to herdr socket |
| `daemon.rs` | `setsid` pre_exec, `signal-hook`, `kill(pid,0)`, `kill(pid,SIGTERM)` |
| `render.rs` | `signal-hook`, `libc::localtime_r` |

`collect.rs`, `config.rs`, `model.rs`, `main.rs` are platform-neutral and stay
unchanged (except `main.rs` module wiring if `proc.rs` becomes a directory module).

## Design

### 1. `proc.rs` → module split

Split into a shared front + two backends behind one public API:

- `proc/mod.rs` — shared pure fns and re-exports: `parse_stat`, `children_map`,
  `subtree`, `pct_string`, `ram_pct`, `parse_mem_total_mb`, and their tests.
  Re-exports the platform backend's `scan_proc`, `clk_tck`, `nproc`,
  `mem_total_mb`, `rss_mb`.
- `proc/linux.rs` — current implementation, moved verbatim (`/proc` + `sysconf`).
- `proc/windows.rs` — Win32 via `windows-sys`:
  - `scan_proc() -> HashMap<u32, ProcEntry>`: `CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS)`
    + `Process32FirstW`/`Process32NextW` yields pid + `th32ParentProcessID`.
    For CPU: `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)` +
    `GetProcessTimes`; sum kernel+user `FILETIME` as 100ns ticks → `jiffies`.
    Denied/vanished pids are skipped (contribute `jiffies=0`, ppid still recorded).
  - `clk_tck() -> u64` = `10_000_000` (100ns tick rate). This makes
    `collect.rs`'s `jiffies / clk_tck` delta-over-window CPU math produce the
    exact same seconds-of-CPU value it computes on Linux — **`collect.rs` is not
    touched**.
  - `nproc() -> u64` = `GetSystemInfo().dwNumberOfProcessors`, min 1.
  - `rss_mb(&HashSet<u32>) -> f64`: per pid `OpenProcess` +
    `GetProcessMemoryInfo` (`PROCESS_MEMORY_COUNTERS.WorkingSetSize`), summed,
    bytes → MB. This is the RSS analogue.
  - `mem_total_mb() -> f64`: `GlobalMemoryStatusEx().ullTotalPhys` / 1 MiB,
    cached in a `OnceLock` (matches the Linux memo).
  - `page_size` is not needed on Windows (WorkingSetSize is already bytes); keep
    it unix-only.

`ProcEntry { ppid: u32, jiffies: u64 }` is unchanged, so `collect.rs` and the
subtree/children logic work identically.

### 2. `herdr.rs` → transport shim

New `transport` module abstracting the byte stream:

- unix: `pub type Transport = std::os::unix::net::UnixStream;` plus the existing
  `configure` (read/write timeouts) and `try_clone`. Unchanged behavior.
- windows: named-pipe client.
  - `connect(path)`: `CreateFileW(name, GENERIC_READ|GENERIC_WRITE, 0, null,
    OPEN_EXISTING, 0, null)`; on `ERROR_PIPE_BUSY`, `WaitNamedPipeW` then retry.
  - Wrap the `HANDLE` in a struct implementing `std::io::Read` + `Write` via
    `ReadFile`/`WriteFile`. Byte mode (`PIPE_READMODE_BYTE`) so
    `BufReader::read_line` frames JSON-RPC exactly as over the Unix socket.
  - `try_clone` via `DuplicateHandle` (the read/write half split in `herdr.rs`
    stays the same).
  - Timeouts via `SetCommTimeouts` on the pipe handle (guards a wedged host,
    mirroring the 15s `IO_TIMEOUT`).
  - `Drop` closes the handle (`CloseHandle`).

`Herdr` struct fields change from `UnixStream` to `transport::Transport`;
`open`/`reconnect`/`configure` route through the module. The JSON-RPC layer
(request framing, id counter, method calls) is untouched.

`socket_path()`: on Windows return `HERDR_SOCKET_PATH` (the pipe name herdr
injects) or error if unset — there is no default pipe name to fall back to. On
unix the existing `HERDR_SOCKET_PATH` → XDG → `~/.config/herdr/herdr.sock` chain
is unchanged.

### 3. `daemon.rs` — cfg-gated lifecycle

- `daemon_pid()` liveness: unix keeps `kill(pid, 0)`. windows:
  `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)`, then
  `GetExitCodeProcess`; alive iff the code is `STILL_ACTIVE` (259). No handle /
  failure → `None`.
- `enable_updater()` detach: unix keeps the `setsid` `pre_exec`. windows uses
  `std::os::windows::process::CommandExt::creation_flags(DETACHED_PROCESS |
  CREATE_NO_WINDOW)` with null stdio.
- Graceful stop: unix keeps `kill(pid, SIGTERM)`. windows uses a **named stop
  event** (`CreateEventW(Local\herdr-space-usage-stop-<plugin_id>)`): the daemon
  opens/creates it and a wait-thread blocks on it; `disable_updater()` opens and
  `SetEvent`s it, waking the daemon to clear tracked statuses + window title and
  `exit(0)`. The existing belt-and-braces sweep in `disable_updater` still runs,
  so a dead or unresponsive daemon is still cleaned up (metadata via TTL, pseudo
  agents via explicit release).
- In-daemon shutdown handler: unix keeps the `signal-hook` (SIGINT/SIGTERM)
  thread. windows runs a thread that waits on the stop event and performs the
  same cleanup path.

### 4. `render.rs` — cfg-gated

- Live-pane Ctrl-C exit: unix keeps the `signal-hook` thread. windows registers
  `SetConsoleCtrlHandler` catching `CTRL_C_EVENT`/`CTRL_CLOSE_EVENT` and runs the
  same clean-exit path.
- `local_time_string()`: unix keeps `libc::localtime_r`. windows uses
  `GetLocalTime()` (`SYSTEMTIME.wHour/wMinute/wSecond`). Output format identical
  (`HH:MM:SS`); the stamp is cosmetic, no parity contract.

### 5. `Cargo.toml`

```toml
[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"

[target.'cfg(unix)'.dependencies]
libc = "0.2"
signal-hook = "0.3"

[target.'cfg(windows)'.dependencies]
windows-sys = { version = "0.59", features = [
  "Win32_Foundation",
  "Win32_System_Threading",
  "Win32_System_ProcessStatus",
  "Win32_System_Diagnostics_ToolHelp",
  "Win32_System_SystemInformation",
  "Win32_System_Memory",
  "Win32_Storage_FileSystem",
  "Win32_System_Pipes",
  "Win32_System_Console",
  "Win32_System_SystemServices",
] }
```

The `[profile.release]` block (opt-level z, lto, strip, panic=abort) is unchanged.
Exact `windows-sys` feature list to be finalized against the crate's module tree
during implementation (symbols may live under slightly different feature gates).

### 6. `herdr-plugin.toml`

- `platforms = ["linux", "windows"]`.
- Add a windows `[[build]]`: `command = ["cargo", "build", "--release"]`,
  `platforms = ["windows"]`.
- Windows cargo emits `space-usage.exe`; pane/action commands currently point at
  `./target/release/space-usage` (no extension). **Open item (resolve in
  planning):** confirm whether herdr's manifest schema (v0.7.x) supports a
  per-entry `platforms` key on `[[panes]]`/`[[actions]]` so we can provide
  windows variants pointing at `space-usage.exe`, or whether herdr resolves the
  `.exe` automatically. Fallback if neither: a post-build step that produces an
  extension-neutral binary name. Verify against herdr docs/source before coding.

## Testing

- Existing pure-fn unit tests (`parse_stat`, `subtree`, `children_map`,
  `pct_string`, `parse_mem_total_mb`) stay in `proc/mod.rs`, run on all platforms.
- Windows named-pipe framing: integration test spins up a real `CreateNamedPipe`
  server in a thread, has the transport connect, and asserts a JSON-RPC
  round-trip frames correctly over `read_line`.
- Syscall-heavy metrics code (`scan_proc`, `rss_mb`) is validated by a manual
  smoke run rather than unit tests.
- CI (`.github/workflows/ci.yml`): add a `windows-latest` matrix leg building +
  testing + clippy. Keep the existing linux leg.

## Verification (before "done")

On Windows, against a live herdr instance:
1. `cargo build --release` produces `space-usage.exe`.
2. `space-usage.exe --json` connects over the named pipe and emits populated
   per-space CPU%/RAM% (non-empty, plausible vs Task Manager).
3. `space-usage.exe --interval 2` renders the live pane; Ctrl-C exits cleanly.
4. `--enable` / `--disable` start/stop the detached daemon; statuses clear on
   disable and self-clear (TTL) if the daemon is killed.
5. Linux build still compiles and passes its tests unchanged (`cargo test`).

## E2E corrections (verified against live herdr 0.7.4 on Windows)

Two facts the pre-implementation research got wrong, found during end-to-end
testing and fixed:

1. **Named-pipe name.** `HERDR_SOCKET_PATH` is injected as a filesystem-style
   path (e.g. `C:\Users\..\herdr.sock`, with a placeholder file of that name),
   but the actual IPC endpoint is the named pipe `\\.\pipe\<that path>`. The
   Windows transport must prepend `\\.\pipe\` (opening the bare path lands on the
   placeholder file → `ERROR_SHARING_VIOLATION`). The transport mechanism itself
   (named pipe, `CreateFileW`) was correct — only the name derivation was wrong.
2. **Command spawn.** herdr cannot launch a `./`-relative executable on Windows:
   `CreateProcess` resolves a relative application path against the caller's
   (herdr's) cwd, not the child's working directory, so
   `./target/release/space-usage.exe` fails with "path not found". The Windows
   manifest entries go through `cmd /c "target\release\space-usage.exe <flag>"`,
   which inherits the plugin-root cwd herdr sets and resolves the path itself.

Both are covered by a full live E2E: report action prints per-space CPU/RAM over
the pipe; daemon enable/disable/toggle spawns detached and stops via the named
stop-event.

## Risks / open items

1. herdr manifest per-platform command / `.exe` resolution (§6) — verify first.
2. `windows-sys` feature-gate names (§5) — finalize during implementation.
3. `GetProcessTimes` requires `PROCESS_QUERY_LIMITED_INFORMATION`; some system
   pids will be denied. That is fine — they contribute `jiffies=0`, matching how
   Linux skips unreadable `/proc/<pid>/stat`. Shell subtrees the plugin cares
   about are owned by the user and readable.
