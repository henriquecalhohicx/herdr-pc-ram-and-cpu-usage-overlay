# Windows Port Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add Windows support to the `space-usage` herdr plugin, keeping the Linux build byte-identical.

**Architecture:** Single cross-platform codebase, `#[cfg]`-gated. `proc.rs` becomes a `proc/` module with a shared pure front and per-OS backends (`linux.rs` moved verbatim, new `windows.rs` using Win32 via `windows-sys`). `herdr.rs` gains a `transport` shim: Unix keeps `UnixStream`; Windows opens herdr's named pipe. `daemon.rs`/`render.rs` cfg-gate signals, process liveness, detachment, and local-time formatting. `collect.rs`, `config.rs`, `model.rs` are untouched — the Windows CPU backend reports `jiffies` in 100ns ticks with `clk_tck()=10_000_000`, so `collect.rs`'s CPU math is identical on both platforms.

**Tech Stack:** Rust (edition 2021), `serde`/`serde_json`, `libc`+`signal-hook` (unix only), `windows-sys` 0.59 (windows only).

## Global Constraints

- `min_herdr_version = "0.7.0"`; herdr on Windows exposes its plugin socket as a **named pipe** whose name is in `HERDR_SOCKET_PATH`. Protocol: newline-delimited JSON-RPC, identical to Unix.
- **Linux code paths must not change behavior.** All Windows code is additive, behind `#[cfg(windows)]`; all current Linux code moves behind `#[cfg(unix)]`/`#[cfg(target_os = "linux")]` without edits.
- `ProcEntry { ppid: u32, jiffies: u64 }` contract is fixed — `collect.rs` depends on it. Windows `jiffies` = kernel+user CPU time in 100ns ticks; Windows `clk_tck()` = `10_000_000`.
- CLI flags unchanged: `--once`, `--interval N`, `--json`, `--enable`, `--disable`, `--toggle`, `--daemon`.
- Release profile unchanged: `opt-level="z"`, `lto=true`, `codegen-units=1`, `strip=true`, `panic="abort"`.
- **Execution environment is Windows** (user's machine). Each task's `cargo build`/`cargo test` runs the `windows` cfg branch on the `*-pc-windows-msvc` target. Prerequisite: `rustup` + MSVC toolchain installed. Linux is verified by CI only (Task 8).
- **Import-path caveat:** exact `windows-sys` module paths for a symbol occasionally differ from what is written here across minor versions. When a step's `cargo build` fails with an unresolved import, locate the symbol (`cargo doc -p windows-sys --open`, or search the crate source) and fix the `use` path. This is expected, not a plan defect.

---

## File Structure

- `Cargo.toml` — split deps into `[target.'cfg(unix)']` and `[target.'cfg(windows)']`.
- `src/proc/mod.rs` — shared pure code (`ProcEntry`, `children_map`, `subtree`, `pct_string`, `ram_pct`) + their tests; selects and re-exports a backend.
- `src/proc/linux.rs` — current `proc.rs` platform code, moved verbatim.
- `src/proc/windows.rs` — Win32 metrics backend.
- `src/herdr/mod.rs` — current `herdr.rs` (JSON-RPC client), retyped over `transport::Transport`.
- `src/herdr/transport.rs` — Unix `UnixStream` alias + Windows named-pipe stream.
- `src/render.rs` — cfg-gated `local_time_string` + Ctrl-C handling.
- `src/daemon.rs` — cfg-gated liveness / detach / stop mechanism.
- `herdr-plugin.toml` — per-platform build/panes/actions.
- `.github/workflows/ci.yml` — add `windows-latest` leg.

---

## Task 1: Restructure `proc.rs` into a `proc/` module + platform deps

**Files:**
- Modify: `Cargo.toml`
- Delete: `src/proc.rs`
- Create: `src/proc/mod.rs`, `src/proc/linux.rs`, `src/proc/windows.rs`

**Interfaces:**
- Produces (public API of `crate::proc`, unchanged from today): `ProcEntry`, `scan_proc() -> HashMap<u32, ProcEntry>`, `children_map(&HashMap<u32,ProcEntry>) -> HashMap<u32,Vec<u32>>`, `subtree(u32, &HashMap<u32,Vec<u32>>) -> HashSet<u32>`, `clk_tck() -> u64`, `nproc() -> u64`, `mem_total_mb() -> f64`, `ram_pct(f64) -> String`, `rss_mb(&HashSet<u32>) -> f64`.

- [ ] **Step 1: Cargo.toml — platform-gate dependencies**

Replace the `[dependencies]` block:

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
  "Win32_System_SystemServices",
  "Win32_System_Pipes",
  "Win32_System_Console",
  "Win32_Storage_FileSystem",
] }
```

Leave `[profile.release]` untouched.

- [ ] **Step 2: Move the platform code into `src/proc/linux.rs`**

Create `src/proc/linux.rs`. Move, verbatim from the current `src/proc.rs`, everything that touches `/proc` or `sysconf`: the `sysconf` helper, `clk_tck`, `page_size`, `nproc`, `scan_proc`, `parse_stat`, `parse_mem_total_mb`, `mem_total_mb`, `rss_mb`, and the tests `parse_stat_handles_comm_with_spaces_and_parens`, `parse_stat_plain_comm`, `parse_mem_total_extracts_kb_and_divides`. Add at the top:

```rust
//! Linux `/proc` + `sysconf` metrics backend (moved verbatim from proc.rs).
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use super::ProcEntry;
```

Change `pub fn` to `pub(super) fn` for the six re-exported functions (`scan_proc`, `clk_tck`, `nproc`, `mem_total_mb`, `rss_mb`; keep `page_size` `pub(super)` too). `parse_stat` and `parse_mem_total_mb` stay module-private (`fn`), with their tests alongside.

- [ ] **Step 3: Write `src/proc/mod.rs` (shared front + backend selection)**

```rust
//! Process/memory sampling. Pure tree/percentage logic lives here; the
//! host-specific sampling (`scan_proc`, `clk_tck`, `nproc`, `mem_total_mb`,
//! `rss_mb`) comes from a per-OS backend.

use std::collections::{HashMap, HashSet};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{clk_tck, mem_total_mb, nproc, rss_mb, scan_proc};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{clk_tck, mem_total_mb, nproc, rss_mb, scan_proc};

/// Per-process sample: parent PID and cumulative CPU ticks.
/// On Linux, ticks are `_SC_CLK_TCK` jiffies (utime+stime). On Windows, ticks
/// are 100ns units (kernel+user) with `clk_tck() == 10_000_000`.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcEntry {
    pub ppid: u32,
    pub jiffies: u64,
}

/// Invert a proc map into `ppid -> [child pid, ..]`.
pub fn children_map(procs: &HashMap<u32, ProcEntry>) -> HashMap<u32, Vec<u32>> {
    let mut kids: HashMap<u32, Vec<u32>> = HashMap::new();
    for (&pid, p) in procs {
        kids.entry(p.ppid).or_default().push(pid);
    }
    kids
}

/// Every PID in `root`'s process subtree (inclusive). Iterative DFS with a
/// visited set, so shared parents and cycles terminate.
pub fn subtree(root: u32, kids: &HashMap<u32, Vec<u32>>) -> HashSet<u32> {
    let mut out = HashSet::new();
    let mut stack = vec![root];
    while let Some(pid) = stack.pop() {
        if !out.insert(pid) {
            continue;
        }
        if let Some(children) = kids.get(&pid) {
            stack.extend(children.iter().copied());
        }
    }
    out
}

/// Render `mb` as a whole-percent-of-`total_mb` string, or `""` when unknown.
fn pct_string(mb: f64, total_mb: f64) -> String {
    if total_mb > 0.0 {
        format!("{}%", (100.0 * mb / total_mb).round() as i64)
    } else {
        String::new()
    }
}

/// `"<n>%"` of total system RAM for `mb`, or `""` if the total is unknown.
pub fn ram_pct(mb: f64) -> String {
    pct_string(mb, mem_total_mb())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subtree_walks_descendants_with_dedup_and_cycle_safety() {
        let mut kids: HashMap<u32, Vec<u32>> = HashMap::new();
        kids.insert(1, vec![2, 3]);
        kids.insert(2, vec![4]);
        kids.insert(3, vec![4]);
        kids.insert(4, vec![1]);
        kids.insert(999, vec![6]);
        assert_eq!(subtree(1, &kids), HashSet::from([1, 2, 3, 4]));
    }

    #[test]
    fn children_map_then_subtree_over_synthetic_procs() {
        let procs: HashMap<u32, ProcEntry> = [
            (1, 0), (2, 1), (3, 1), (4, 2), (5, 4), (6, 999),
        ]
        .into_iter()
        .map(|(pid, ppid)| (pid, ProcEntry { ppid, jiffies: 0 }))
        .collect();
        let kids = children_map(&procs);
        assert_eq!(subtree(1, &kids), HashSet::from([1, 2, 3, 4, 5]));
    }

    #[test]
    fn ram_pct_math_rounds_and_guards_zero_total() {
        assert_eq!(pct_string(1024.0, 16384.0), "6%");
        assert_eq!(pct_string(250.0, 10000.0), "3%");
        assert_eq!(pct_string(16384.0, 16384.0), "100%");
        assert_eq!(pct_string(512.0, 0.0), "");
    }
}
```

- [ ] **Step 4: Write a compiling `src/proc/windows.rs` stub**

Minimal so the workspace builds on Windows; real logic lands in Task 2.

```rust
//! Windows Win32 metrics backend (stub — implemented in Task 2).
use std::collections::{HashMap, HashSet};

use super::ProcEntry;

pub(super) fn clk_tck() -> u64 { 10_000_000 }
pub(super) fn nproc() -> u64 { 1 }
pub(super) fn scan_proc() -> HashMap<u32, ProcEntry> { HashMap::new() }
pub(super) fn mem_total_mb() -> f64 { 0.0 }
pub(super) fn rss_mb(_pids: &HashSet<u32>) -> f64 { 0.0 }
```

- [ ] **Step 5: Delete the old file**

```bash
git rm src/proc.rs
```

(`main.rs` already declares `mod proc;`, which now resolves to the directory — no change needed there.)

- [ ] **Step 6: Build and test**

Run: `cargo test`
Expected: compiles on Windows; the three `proc::tests` pass (`subtree_*`, `children_map_*`, `ram_pct_*`). The `linux` backend and its tests are `#[cfg(target_os = "linux")]`, so they are absent on this host — that is correct.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml src/proc/
git commit -m "refactor: split proc.rs into proc/ module with per-OS backends"
```

---

## Task 2: Windows metrics backend

**Files:**
- Modify: `src/proc/windows.rs`

**Interfaces:**
- Consumes: `super::ProcEntry`.
- Produces: real `scan_proc`, `clk_tck`, `nproc`, `mem_total_mb`, `rss_mb` (signatures as in Task 1 Step 4).

- [ ] **Step 1: Write the failing test for the FILETIME→ticks helper**

Add to `src/proc/windows.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filetime_pair_sums_to_100ns_ticks() {
        // kernel = 0x0000_0001_0000_0000 (1<<32) ticks, user = 5 ticks.
        let kernel = ft(0x0000_0001, 0x0000_0000);
        let user = ft(0, 5);
        assert_eq!(ticks(kernel, user), (1u64 << 32) + 5);
    }

    fn ft(high: u32, low: u32) -> FILETIME {
        FILETIME { dwHighDateTime: high, dwLowDateTime: low }
    }
}
```

- [ ] **Step 2: Run it — verify it fails**

Run: `cargo test -p space-usage windows::tests::filetime_pair_sums_to_100ns_ticks`
Expected: FAIL — `ticks` / `FILETIME` not found.

- [ ] **Step 3: Implement the backend**

Replace the whole non-test body of `src/proc/windows.rs`:

```rust
//! Windows Win32 metrics backend.
//!
//! ppid tree via a Toolhelp process snapshot; per-process CPU time via
//! `GetProcessTimes` (kernel+user, 100ns ticks → `jiffies`); RSS via
//! `GetProcessMemoryInfo` WorkingSetSize; total RAM via `GlobalMemoryStatusEx`.
//! `clk_tck()` is the 100ns tick rate so `collect.rs`'s CPU math is unchanged.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, FALSE, HANDLE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows_sys::Win32::System::SystemInformation::{
    GetSystemInfo, GlobalMemoryStatusEx, MEMORYSTATUSEX, SYSTEM_INFO,
};
use windows_sys::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};

use super::ProcEntry;

/// 100ns ticks per second — the unit of `GetProcessTimes`.
pub(super) fn clk_tck() -> u64 {
    10_000_000
}

/// Online logical processors, min 1.
pub(super) fn nproc() -> u64 {
    // SAFETY: fills a caller-owned SYSTEM_INFO.
    let mut si: SYSTEM_INFO = unsafe { std::mem::zeroed() };
    unsafe { GetSystemInfo(&mut si) };
    (si.dwNumberOfProcessors as u64).max(1)
}

/// Combine kernel+user FILETIMEs into a single 100ns tick count.
fn ticks(kernel: FILETIME, user: FILETIME) -> u64 {
    let k = ((kernel.dwHighDateTime as u64) << 32) | kernel.dwLowDateTime as u64;
    let u = ((user.dwHighDateTime as u64) << 32) | user.dwLowDateTime as u64;
    k + u
}

/// RAII wrapper so every early return closes the process handle.
struct OwnedHandle(HANDLE);
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: handle came from OpenProcess and is closed exactly once.
        unsafe { CloseHandle(self.0) };
    }
}

/// Open a process for metric queries, or `None` if access is denied / it died.
fn open(pid: u32) -> Option<OwnedHandle> {
    // SAFETY: OpenProcess returns null on failure, which we check.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid) };
    if h.is_null() {
        None
    } else {
        Some(OwnedHandle(h))
    }
}

/// CPU ticks (kernel+user) for one process, 0 if the times cannot be read.
fn proc_ticks(h: HANDLE) -> u64 {
    // SAFETY: all four FILETIMEs are caller-owned; return value is checked.
    let mut creation: FILETIME = unsafe { std::mem::zeroed() };
    let mut exit: FILETIME = unsafe { std::mem::zeroed() };
    let mut kernel: FILETIME = unsafe { std::mem::zeroed() };
    let mut user: FILETIME = unsafe { std::mem::zeroed() };
    let ok = unsafe { GetProcessTimes(h, &mut creation, &mut exit, &mut kernel, &mut user) };
    if ok == 0 {
        0
    } else {
        ticks(kernel, user)
    }
}

/// Snapshot every process: pid -> {ppid, cumulative CPU ticks}. Denied or
/// vanished processes contribute `jiffies = 0` but keep their ppid (so the
/// tree stays connected), mirroring how Linux skips unreadable `/proc/<pid>`.
pub(super) fn scan_proc() -> HashMap<u32, ProcEntry> {
    let mut procs = HashMap::new();
    // SAFETY: snapshot handle checked; PROCESSENTRY32W is zeroed with dwSize set
    // as the API requires before Process32FirstW.
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snap.is_null() || snap == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
        return procs;
    }
    let _snap = OwnedHandle(snap);

    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;

    let mut ok = unsafe { Process32FirstW(snap, &mut entry) };
    while ok != 0 {
        let pid = entry.th32ProcessID;
        let ppid = entry.th32ParentProcessID;
        let jiffies = open(pid).map(|h| proc_ticks(h.0)).unwrap_or(0);
        procs.insert(pid, ProcEntry { ppid, jiffies });
        ok = unsafe { Process32NextW(snap, &mut entry) };
    }
    procs
}

/// Sum WorkingSetSize (MB) across `pids`. Denied/vanished pids contribute 0.
pub(super) fn rss_mb(pids: &HashSet<u32>) -> f64 {
    let mut bytes: u64 = 0;
    for &pid in pids {
        if let Some(h) = open(pid) {
            let mut counters: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
            let cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
            // SAFETY: counters is caller-owned; cb is its size.
            let ok = unsafe { GetProcessMemoryInfo(h.0, &mut counters, cb) };
            if ok != 0 {
                bytes += counters.WorkingSetSize as u64;
            }
        }
    }
    bytes as f64 / (1024.0 * 1024.0)
}

/// Total physical RAM in MB, cached (matches the Linux memo). 0 if unreadable.
pub(super) fn mem_total_mb() -> f64 {
    static MEM_TOTAL_MB: OnceLock<f64> = OnceLock::new();
    *MEM_TOTAL_MB.get_or_init(|| {
        let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
        status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
        // SAFETY: status is caller-owned with dwLength set as required.
        let ok = unsafe { GlobalMemoryStatusEx(&mut status) };
        if ok == 0 {
            0.0
        } else {
            status.ullTotalPhys as f64 / (1024.0 * 1024.0)
        }
    })
}
```

- [ ] **Step 4: Run the helper test — verify it passes**

Run: `cargo test windows::tests::filetime_pair_sums_to_100ns_ticks`
Expected: PASS.

- [ ] **Step 5: Smoke-check the live syscalls**

Run: `cargo run --release -- --json` (does not need herdr yet if it fails at connect; to check metrics alone, temporarily add `eprintln!("{} procs, {} cores, {} MB total", crate::proc::scan_proc().len(), crate::proc::nproc(), crate::proc::mem_total_mb());` at the top of `run()` in `main.rs`, run `cargo run --release`, confirm proc count > 50, cores matches your CPU, total MB matches installed RAM, then remove the line).
Expected: plausible non-zero values.

- [ ] **Step 6: Commit**

```bash
git add src/proc/windows.rs
git commit -m "feat(windows): Win32 process CPU/RAM metrics backend"
```

---

## Task 3: Named-pipe transport shim

**Files:**
- Create: `src/herdr/transport.rs`
- Move: `src/herdr.rs` → `src/herdr/mod.rs`, retyped over `transport::Transport`

**Interfaces:**
- Produces `crate::herdr::transport`:
  - `pub type Transport` — a value implementing `std::io::Read + Write`.
  - `pub fn connect(path: &Path) -> io::Result<Transport>`
  - `pub fn try_clone(t: &Transport) -> io::Result<Transport>`
  - `pub fn configure(t: &Transport) -> io::Result<()>` (Unix sets timeouts; Windows is a no-op — see limitation note).
- Consumes: `crate::herdr::mod` calls the four items above in `open`/`reconnect`.

- [ ] **Step 1: Move `herdr.rs` into the module directory**

```bash
git mv src/herdr.rs src/herdr/mod.rs
```

- [ ] **Step 2: Write the Unix transport (behavior-preserving) in `src/herdr/transport.rs`**

```rust
//! Byte-stream transport to the herdr host. Unix: a Unix-domain socket.
//! Windows: herdr's named pipe (name from `HERDR_SOCKET_PATH`).

use std::io;
use std::path::Path;
use std::time::Duration;

/// Round-trip timeout guard against a wedged host (Unix only; see the Windows
/// note in this module).
const IO_TIMEOUT: Duration = Duration::from_secs(15);

#[cfg(unix)]
mod imp {
    use super::*;
    use std::os::unix::net::UnixStream;

    pub type Transport = UnixStream;

    pub fn connect(path: &Path) -> io::Result<Transport> {
        UnixStream::connect(path)
    }

    pub fn try_clone(t: &Transport) -> io::Result<Transport> {
        t.try_clone()
    }

    pub fn configure(t: &Transport) -> io::Result<()> {
        t.set_read_timeout(Some(IO_TIMEOUT))?;
        t.set_write_timeout(Some(IO_TIMEOUT))?;
        Ok(())
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::io::{Read, Write};
    use windows_sys::Win32::Foundation::{
        CloseHandle, DuplicateHandle, GetCurrentProcess, DUPLICATE_SAME_ACCESS, ERROR_PIPE_BUSY,
        GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, ReadFile, WriteFile, OPEN_EXISTING};
    use windows_sys::Win32::System::Pipes::WaitNamedPipeW;

    /// Connect timeout while the pipe is momentarily busy (ms).
    const PIPE_WAIT_MS: u32 = 15_000;

    /// A synchronous, byte-mode named-pipe client handle.
    ///
    /// Limitation vs the Unix path: no per-read/write timeout is applied. herdr
    /// answers in milliseconds, so the wedge-guard the Unix socket gets from
    /// `set_read_timeout` is omitted here; a truly hung host would block a
    /// round-trip. Acceptable for v1 (see plan §Task 3).
    pub struct PipeStream(HANDLE);

    // SAFETY: the handle is only used from the thread that owns the struct (and
    // its clone); no concurrent access to a single handle.
    unsafe impl Send for PipeStream {}

    pub type Transport = PipeStream;

    fn to_wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn connect(path: &Path) -> io::Result<Transport> {
        let name = to_wide(&path.to_string_lossy());
        loop {
            // SAFETY: name is NUL-terminated; return value checked.
            let h = unsafe {
                CreateFileW(
                    name.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    0,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    0,
                    std::ptr::null_mut(),
                )
            };
            if h != INVALID_HANDLE_VALUE && !h.is_null() {
                return Ok(PipeStream(h));
            }
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) {
                // SAFETY: name is NUL-terminated.
                let waited = unsafe { WaitNamedPipeW(name.as_ptr(), PIPE_WAIT_MS) };
                if waited == 0 {
                    return Err(io::Error::last_os_error());
                }
                continue;
            }
            return Err(err);
        }
    }

    pub fn try_clone(t: &Transport) -> io::Result<Transport> {
        let mut dup: HANDLE = std::ptr::null_mut();
        // SAFETY: duplicates our own handle into our own process.
        let ok = unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                t.0,
                GetCurrentProcess(),
                &mut dup,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        };
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(PipeStream(dup))
        }
    }

    pub fn configure(_t: &Transport) -> io::Result<()> {
        Ok(()) // see PipeStream limitation note
    }

    impl Read for PipeStream {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let mut read: u32 = 0;
            // SAFETY: buf is valid for buf.len() bytes; read is caller-owned.
            let ok = unsafe {
                ReadFile(
                    self.0,
                    buf.as_mut_ptr(),
                    buf.len() as u32,
                    &mut read,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(read as usize)
            }
        }
    }

    impl Write for PipeStream {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let mut written: u32 = 0;
            // SAFETY: buf is valid for buf.len() bytes; written is caller-owned.
            let ok = unsafe {
                WriteFile(
                    self.0,
                    buf.as_ptr(),
                    buf.len() as u32,
                    &mut written,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(written as usize)
            }
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Drop for PipeStream {
        fn drop(&mut self) {
            // SAFETY: handle from CreateFileW/DuplicateHandle, closed once.
            unsafe { CloseHandle(self.0) };
        }
    }
}

pub use imp::{connect, configure, try_clone, Transport};
```

- [ ] **Step 3: Retype `herdr/mod.rs` over `Transport`**

In `src/herdr/mod.rs`:

1. Add module declaration near the top (after the doc comment): `mod transport;`
2. Delete `use std::os::unix::net::UnixStream;`.
3. In `struct Herdr`, change field types: `stream: transport::Transport,` and `reader: BufReader<transport::Transport>,`.
4. In `Herdr::open`, replace the connect/configure/clone lines with:

```rust
    fn open(path: PathBuf) -> crate::Result<Herdr> {
        let stream = transport::connect(&path)
            .map_err(|e| format!("cannot connect to herdr socket {}: {e}", path.display()))?;
        transport::configure(&stream)?;
        let reader = BufReader::new(transport::try_clone(&stream)?);
        Ok(Herdr { stream, reader, path, next_id: 1 })
    }
```

5. In `Herdr::reconnect`, replace its `UnixStream::connect` + configure + clone with `transport::connect(&self.path)` / `transport::configure(&stream)?` / `transport::try_clone(&stream)?` (keep the existing error message and field assignments).
6. If a standalone `fn configure(stream: &UnixStream)` exists in this file, delete it (its body moved into `transport::configure`).

- [ ] **Step 4: Write a named-pipe round-trip integration test**

Create `src/herdr/transport.rs` test module at the end of the file:

```rust
#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::thread;
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_ACCESS_DUPLEX, PIPE_READMODE_BYTE,
        PIPE_TYPE_BYTE, PIPE_WAIT,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    #[test]
    fn pipe_round_trips_newline_framed_json() {
        let name = r"\\.\pipe\space-usage-test-roundtrip";
        let wname = wide(name);
        // Server: create the pipe, wait for the client, echo one line back.
        let server = thread::spawn(move || {
            let h = unsafe {
                CreateNamedPipeW(
                    wname.as_ptr(),
                    PIPE_ACCESS_DUPLEX,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                    1, 512, 512, 0, std::ptr::null(),
                )
            };
            assert!(h as isize != -1);
            unsafe { ConnectNamedPipe(h, std::ptr::null_mut()) };
            let mut srv = super::imp::PipeStream_from_raw_for_test(h);
            let mut r = BufReader::new(super::try_clone(&srv).unwrap());
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            srv.write_all(line.as_bytes()).unwrap();
        };
        // Give the server a moment to create the pipe before connecting.
        std::thread::sleep(std::time::Duration::from_millis(100));
        let mut client = connect(std::path::Path::new(name)).unwrap();
        client.write_all(b"{\"id\":\"1\"}\n").unwrap();
        let mut r = BufReader::new(try_clone(&client).unwrap());
        let mut got = String::new();
        r.read_line(&mut got).unwrap();
        assert_eq!(got, "{\"id\":\"1\"}\n");
        server.join().unwrap();
    }
}
```

Then add, inside the `#[cfg(windows)] mod imp` block, a test-only constructor so the server side can wrap its handle:

```rust
    #[cfg(test)]
    pub fn PipeStream_from_raw_for_test(h: HANDLE) -> PipeStream {
        PipeStream(h)
    }
```

- [ ] **Step 5: Run the transport test — verify it passes**

Run: `cargo test herdr::transport::tests::pipe_round_trips_newline_framed_json -- --nocapture`
Expected: PASS (a real pipe is created, the client connects, one JSON line echoes back unchanged).

- [ ] **Step 6: Build the whole crate**

Run: `cargo build`
Expected: compiles; `herdr/mod.rs` uses `transport::Transport` throughout with no `UnixStream` reference.

- [ ] **Step 7: Commit**

```bash
git add src/herdr/
git commit -m "feat(windows): named-pipe transport shim for the herdr socket"
```

---

## Task 4: `socket_path()` — Windows resolution

**Files:**
- Modify: `src/herdr/mod.rs` (the `socket_path` fn)

**Interfaces:**
- Consumes: `std::env::var("HERDR_SOCKET_PATH")`.
- Produces: unchanged signature `fn socket_path() -> crate::Result<PathBuf>`.

- [ ] **Step 1: cfg-gate the resolution**

Locate `fn socket_path()` in `src/herdr/mod.rs`. Wrap the body so Unix keeps its `HERDR_SOCKET_PATH` → XDG → `~/.config/herdr/herdr.sock` chain, and Windows requires the env var:

```rust
fn socket_path() -> crate::Result<PathBuf> {
    if let Ok(p) = std::env::var("HERDR_SOCKET_PATH") {
        return Ok(PathBuf::from(p));
    }
    #[cfg(windows)]
    {
        Err("HERDR_SOCKET_PATH not set (herdr injects the named-pipe name)".into())
    }
    #[cfg(unix)]
    {
        // existing XDG / ~/.config/herdr/herdr.sock fallback — keep verbatim
        // <existing code>
    }
}
```

Move the current XDG/home fallback code verbatim into the `#[cfg(unix)]` block. Keep whatever helper it calls in `crate::config`.

- [ ] **Step 2: Build**

Run: `cargo build`
Expected: compiles.

- [ ] **Step 3: Commit**

```bash
git add src/herdr/mod.rs
git commit -m "feat(windows): require HERDR_SOCKET_PATH for the named pipe"
```

---

## Task 5: `render.rs` — local time + Ctrl-C, cfg-gated

**Files:**
- Modify: `src/render.rs`

**Interfaces:**
- Produces: unchanged `fn local_time_string() -> String`; cfg-gated signal setup used by `run_interval`.

- [ ] **Step 1: Write the failing test for the time formatter**

Extract the format step into a pure helper so it is testable. Add to `src/render.rs` tests:

```rust
    #[test]
    fn fmt_hms_zero_pads() {
        assert_eq!(fmt_hms(9, 5, 3), "09:05:03");
        assert_eq!(fmt_hms(23, 59, 59), "23:59:59");
    }
```

- [ ] **Step 2: Run it — verify it fails**

Run: `cargo test render::tests::fmt_hms_zero_pads`
Expected: FAIL — `fmt_hms` not found.

- [ ] **Step 3: Implement `fmt_hms` + cfg-gated `local_time_string`**

Add the pure helper and rewrite `local_time_string`:

```rust
/// Zero-padded `HH:MM:SS`.
fn fmt_hms(h: u32, m: u32, s: u32) -> String {
    format!("{h:02}:{m:02}:{s:02}")
}

#[cfg(unix)]
fn local_time_string() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as libc::time_t;
    // SAFETY: localtime_r fills the caller-owned tm; secs is a valid time_t.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&secs, &mut tm) };
    fmt_hms(tm.tm_hour as u32, tm.tm_min as u32, tm.tm_sec as u32)
}

#[cfg(windows)]
fn local_time_string() -> String {
    use windows_sys::Win32::System::SystemInformation::{GetLocalTime, SYSTEMTIME};
    // SAFETY: GetLocalTime fills a caller-owned SYSTEMTIME.
    let mut st: SYSTEMTIME = unsafe { std::mem::zeroed() };
    unsafe { GetLocalTime(&mut st) };
    fmt_hms(st.wHour as u32, st.wMinute as u32, st.wSecond as u32)
}
```

- [ ] **Step 4: Run the test — verify it passes**

Run: `cargo test render::tests::fmt_hms_zero_pads`
Expected: PASS.

- [ ] **Step 5: cfg-gate the Ctrl-C signal setup**

Find the `signal-hook` usage in `run_interval` (the `Signals::new([SIGINT, SIGTERM])` thread that restores the terminal / clears and exits). Wrap the existing block in `#[cfg(unix)]`. Add the Windows equivalent that runs the same clean-exit path:

```rust
#[cfg(windows)]
{
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
    // SAFETY: registers a static handler; no captured state.
    unsafe extern "system" fn on_ctrl(_ctrl_type: u32) -> windows_sys::Win32::Foundation::BOOL {
        // Restore cursor/screen the same way the Unix path does, then exit.
        print!("\x1b[?25h\x1b[2J\x1b[H");
        use std::io::Write;
        let _ = std::io::stdout().flush();
        std::process::exit(0);
    }
    unsafe { SetConsoleCtrlHandler(Some(on_ctrl), 1) };
}
```

(Match the exact terminal-restore escape sequence the existing Unix handler emits; copy it into `on_ctrl` so both platforms leave the terminal in the same state.)

- [ ] **Step 6: Build and test**

Run: `cargo test` then `cargo build --release`
Expected: compiles; `fmt_hms` test passes.

- [ ] **Step 7: Commit**

```bash
git add src/render.rs
git commit -m "feat(windows): GetLocalTime footer + console Ctrl-C handler"
```

---

## Task 6: `daemon.rs` — liveness, detach, stop mechanism (cfg-gated)

**Files:**
- Modify: `src/daemon.rs`

**Interfaces:**
- Produces: unchanged `daemon_pid`, `enable_updater`, `disable_updater`, `toggle_updater`, `run_daemon` signatures.

- [ ] **Step 1: cfg-gate `daemon_pid` liveness**

Replace the `libc::kill(pid, 0)` probe. Keep the pid-file read shared; gate only the liveness check:

```rust
pub fn daemon_pid() -> Option<u32> {
    let text = std::fs::read_to_string(config::pid_file()).ok()?;
    let pid: i32 = text.trim().parse().ok()?;
    if pid <= 0 {
        return None;
    }
    if pid_alive(pid as u32) {
        Some(pid as u32)
    } else {
        None
    }
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    // SAFETY: kill signal 0 is a liveness probe only.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(windows)]
fn pid_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, FALSE, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    // SAFETY: handle checked; code is caller-owned; handle closed once.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid);
        if h.is_null() {
            return false;
        }
        let mut code: u32 = 0;
        let ok = GetExitCodeProcess(h, &mut code);
        CloseHandle(h);
        ok != 0 && code == STILL_ACTIVE as u32
    }
}
```

- [ ] **Step 2: cfg-gate detachment in `enable_updater`**

Wrap the existing `unsafe { cmd.pre_exec(...setsid...) }` block in `#[cfg(unix)]`. Add:

```rust
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW);
    }
```

(Place it right before `cmd.spawn()?;`. The `stdin/stdout/stderr(Stdio::null())` calls stay shared.)

- [ ] **Step 3: cfg-gate the stop signal in `disable_updater`**

Wrap the `unsafe { libc::kill(pid as i32, SIGTERM); }` in `#[cfg(unix)]`. Add the Windows stop-event set:

```rust
    #[cfg(windows)]
    {
        let _ = signal_stop_event(); // best-effort; sweep below still runs
    }
```

Add the stop-event helper (shared name derived from the plugin id):

```rust
#[cfg(windows)]
fn stop_event_name() -> Vec<u16> {
    let name = format!(r"Local\herdr-space-usage-stop-{}", config::plugin_id());
    name.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn signal_stop_event() -> std::io::Result<()> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{CreateEventW, SetEvent};
    let name = stop_event_name();
    // SAFETY: name is NUL-terminated; handle checked and closed once.
    unsafe {
        let h = CreateEventW(std::ptr::null(), 1, 0, name.as_ptr());
        if h.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        SetEvent(h);
        CloseHandle(h);
    }
    Ok(())
}
```

- [ ] **Step 4: cfg-gate the daemon's shutdown wait in `run_daemon`**

Wrap the existing `signal-hook` `Signals::new([SIGINT, SIGTERM])` thread (the one that flips the `AtomicBool` and runs cleanup) in `#[cfg(unix)]`. Add a Windows thread that waits on the stop event and flips the same `AtomicBool`, then performs (or triggers) the same cleanup the Unix signal thread does:

```rust
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
        use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject, INFINITE};
        let flag = Arc::clone(&shutdown); // reuse the loop's existing AtomicBool
        let name = stop_event_name();
        thread::spawn(move || {
            // SAFETY: name NUL-terminated; handle waited on then closed once.
            unsafe {
                let h = CreateEventW(std::ptr::null(), 1, 0, name.as_ptr());
                if h.is_null() {
                    return;
                }
                if WaitForSingleObject(h, INFINITE) == WAIT_OBJECT_0 {
                    flag.store(true, Ordering::SeqCst);
                }
                CloseHandle(h);
            }
        });
    }
```

Match the exact name of the existing shutdown `AtomicBool`/`Arc` used by the Unix branch (the loop already checks it each iteration — reuse it, do not add a second flag). The loop's existing "on shutdown: clear tracked statuses + title, unlink pid, exit" path stays shared and runs when the flag flips.

Note: the daemon does its own status cleanup when the flag flips. Because Windows sets the flag via the event, the effect matches the Unix `SIGTERM` path. The belt-and-braces sweep in `disable_updater` still covers a dead/unresponsive daemon (metadata TTL + explicit pseudo release).

- [ ] **Step 5: cfg-gate remaining unix imports in daemon.rs**

At the top of `src/daemon.rs`, wrap the unix-only imports so Windows builds:

```rust
#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(unix)]
use signal_hook::consts::{SIGINT, SIGTERM};
#[cfg(unix)]
use signal_hook::iterator::Signals;
```

- [ ] **Step 6: Build**

Run: `cargo build --release`
Expected: compiles on Windows with no `libc`/`signal-hook` references outside `#[cfg(unix)]`.

- [ ] **Step 7: Smoke-test the daemon lifecycle**

Run (against a live herdr, or expect a clean connect error): `cargo run --release -- --enable` then `cargo run --release -- --disable`.
Expected: `--enable` spawns a detached process (visible in Task Manager as `space-usage.exe`, no console window); `--disable` makes it exit within one refresh interval and reports "sidebar usage disabled".

- [ ] **Step 8: Commit**

```bash
git add src/daemon.rs
git commit -m "feat(windows): daemon liveness, detached spawn, stop-event shutdown"
```

---

## Task 7: Manifest + CI + README

**Files:**
- Modify: `herdr-plugin.toml`, `.github/workflows/ci.yml`, `README.md`

**Interfaces:** none (packaging).

- [ ] **Step 1: Per-platform manifest entries**

In `herdr-plugin.toml`: set `platforms = ["linux", "windows"]`. Add a `platforms = ["linux"]` key to the existing `[[build]]`, and duplicate each `[[build]]`/`[[panes]]`/`[[actions]]` command entry for Windows with `.exe` and `platforms = ["windows"]`. Example for the build and the dashboard pane:

```toml
[[build]]
command = ["cargo", "build", "--release"]
platforms = ["linux"]

[[build]]
command = ["cargo", "build", "--release"]
platforms = ["windows"]

[[panes]]
id = "dashboard"
title = "Space usage"
placement = "overlay"
command = ["./target/release/space-usage", "--interval", "2"]
platforms = ["linux"]

[[panes]]
id = "dashboard"
title = "Space usage"
placement = "overlay"
command = ["./target/release/space-usage.exe", "--interval", "2"]
platforms = ["windows"]
```

Apply the same linux/windows duplication to the `report`, `status-toggle`, `status-enable`, and `status-disable` actions (windows command = same argv with `space-usage.exe`).

- [ ] **Step 2: Verify herdr accepts duplicate ids across platforms**

Run: `herdr plugin link .` (or `herdr plugin install` from a local path) on the Windows host.
Expected: herdr registers the plugin without an id-collision error and selects the `windows` entries. If herdr rejects duplicate ids, fall back to distinct ids (`dashboard-win`, etc.) with `platforms = ["windows"]` and note it in the commit.

- [ ] **Step 3: Add the Windows CI leg**

In `.github/workflows/ci.yml`, convert the build/test job to a matrix (keep whatever steps exist — checkout, toolchain, `cargo build`, `cargo test`, `cargo clippy`):

```yaml
    strategy:
      matrix:
        os: [ubuntu-latest, windows-latest]
    runs-on: ${{ matrix.os }}
```

Ensure `cargo clippy --all-targets -- -D warnings` and `cargo test` run on both legs. This is the mechanism that keeps the Linux build honest without a local Linux host.

- [ ] **Step 4: README platform note**

In `README.md`, change the platform line to state Linux **and** Windows are supported; note macOS is unsupported (no `/proc`). Keep install commands as-is.

- [ ] **Step 5: Commit**

```bash
git add herdr-plugin.toml .github/workflows/ci.yml README.md
git commit -m "build(windows): manifest entries, CI matrix, README"
```

---

## Task 8: End-to-end verification on Windows

**Files:** none (verification only).

- [ ] **Step 1: Full release build**

Run: `cargo build --release`
Expected: `target/release/space-usage.exe` exists.

- [ ] **Step 2: JSON snapshot against live herdr**

With herdr running on Windows and `HERDR_SOCKET_PATH` set by herdr for plugin commands, trigger the report action (or run the binary in a herdr pane): `space-usage.exe --json`.
Expected: valid JSON with one entry per space; CPU% and RAM% are non-empty and plausible vs Task Manager (within a few %).

- [ ] **Step 3: Live pane**

Open the "Space usage" pane in herdr (or run `space-usage.exe --interval 2`).
Expected: refreshes every 2s; footer shows local `HH:MM:SS`; Ctrl-C exits and restores the terminal.

- [ ] **Step 4: Daemon toggle**

Run the "Toggle sidebar usage status" action twice.
Expected: first enables (statuses appear per space), second disables (statuses clear); killing the daemon process directly leaves statuses that self-clear within the TTL (`interval * 3`).

- [ ] **Step 5: Confirm Linux is green**

Push the branch; confirm the `ubuntu-latest` CI leg passes `cargo build`, `cargo test`, `cargo clippy`.
Expected: green — proves the Linux paths are unchanged.

- [ ] **Step 6: Final verification via the `verify` skill**

Invoke the `verify` skill to drive the plugin end-to-end and confirm observed behavior matches the spec's Verification section before declaring done.

---

## Self-Review

**Spec coverage:**
- §1 proc split → Tasks 1, 2. ✓
- §2 transport shim → Task 3. ✓
- §2 socket_path Windows → Task 4. ✓
- §3 daemon liveness/detach/stop → Task 6. ✓
- §4 render time/signals → Task 5. ✓
- §5 Cargo.toml → Task 1 Step 1. ✓
- §6 manifest → Task 7 (+ open item verified: per-entry `platforms` supported; Windows needs `.exe`). ✓
- Testing (pipe framing, CI matrix) → Tasks 3, 7. ✓
- Verification → Task 8. ✓

**Deviations from spec (intentional):**
- Spec §2 named `SetCommTimeouts` for a per-IO timeout. Plan uses blocking pipe IO + `WaitNamedPipeW` connect timeout and drops the per-read/write wedge-guard (documented in the `PipeStream` doc comment). Reason: a true sync-IO timeout needs overlapped IO; out of proportion for a host that answers in ms. Revisit if a hang is ever observed.

**Type consistency:** `Transport`, `connect`, `configure`, `try_clone` names match across Tasks 3–4. `ProcEntry{ppid,jiffies}`, `scan_proc`/`clk_tck`/`nproc`/`mem_total_mb`/`rss_mb`/`ram_pct` names match Task 1 exports and their Task 2 impls. `pid_alive`, `stop_event_name`, `signal_stop_event` consistent within Task 6.

**Placeholder scan:** no TBD/TODO; each code step carries full code. The two "match the existing X" instructions (terminal-restore escape in Task 5 Step 5; shutdown `AtomicBool` name in Task 6 Step 4) point at concrete existing code the implementer reads in-file, not invented names.
