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
pub fn clk_tck() -> u64 {
    10_000_000
}

/// Online logical processors, min 1.
pub fn nproc() -> u64 {
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
pub fn scan_proc() -> HashMap<u32, ProcEntry> {
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
pub fn rss_mb(pids: &HashSet<u32>) -> f64 {
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
pub fn mem_total_mb() -> f64 {
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
