//! Sidebar status updater daemon and its enable/disable/toggle controls
//! (mirrors `index.js` lines 343-616).
//!
//! The daemon refreshes each space's usage on a cadence, surfacing it either as
//! a "usage" pseudo-agent (agents-panel mode) or as TTL'd display-only metadata
//! (sidebar mode). A pid file under the state dir enforces a single instance;
//! statuses self-clear via their TTL if the daemon dies. `enable`/`disable`/
//! `toggle` spawn or signal that daemon and sweep leftover statuses.

use std::collections::HashSet;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[cfg(unix)]
use signal_hook::consts::{SIGINT, SIGTERM};
#[cfg(unix)]
use signal_hook::iterator::Signals;

use crate::collect::{self, PSEUDO_AGENT};
use crate::config::{self, Config, Labels, Mode};
use crate::herdr::{self, Herdr};
use crate::model::Space;
use crate::proc;

/// Panes we have pushed status onto this run, so shutdown can clear them.
#[derive(Debug, Default)]
pub struct Tracked {
    /// Panes carrying our pseudo-agent (released, not TTL'd).
    pub pseudo: HashSet<String>,
    /// Panes carrying TTL'd metadata statuses.
    pub metadata: HashSet<String>,
}

/// PID of a live updater daemon, or `None` (missing pid file / dead process).
///
/// Reads `<state_dir>/updater.pid` and probes the process for liveness (Unix:
/// `kill(pid, 0)`, signal 0 checks existence only; Windows: open + exit-code
/// check). Any failure — no file, unparsable content, a non-positive pid, or a
/// dead/unprobeable process — reads as `None`, exactly as the JS `try/catch`
/// around `process.kill(pid, 0)` did.
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

/// Unix liveness probe: signal 0 performs no delivery, existence check only.
#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    // SAFETY: kill signal 0 is a liveness probe only.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

/// Windows liveness probe: open the process and check it hasn't exited.
#[cfg(windows)]
fn pid_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, FALSE, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    // SAFETY: handle checked before use; code is caller-owned; handle closed once.
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

/// `--daemon`: run the updater loop until signalled, then clear and exit.
///
/// Single-instance via the pid file; a signal-hook thread performs the SIGINT/
/// SIGTERM shutdown (clear tracked statuses + title, unlink pid, `exit(0)`) over
/// its own socket connection so it need not wait on the main loop's sample sleep.
/// The loop samples with a quick first window, then the configured interval, and
/// shuts down after five consecutive failures (herdr server likely gone).
pub fn run_daemon() -> crate::Result<()> {
    if daemon_pid().is_some() {
        return Ok(()); // another updater is already live
    }
    std::fs::create_dir_all(config::state_dir())?;
    std::fs::write(config::pid_file(), format!("{}\n", std::process::id()))?;

    let config = config::load_config();
    let labels = config::load_herdr_labels();

    let mut client = match herdr::connect() {
        Ok(client) => client,
        Err(err) => {
            // Nothing to run without a host connection — don't leave a pid file
            // pointing at a process that is about to exit.
            let _ = std::fs::remove_file(config::pid_file());
            return Err(err);
        }
    };

    let stopping = Arc::new(AtomicBool::new(false));
    let tracked = Arc::new(Mutex::new(Tracked::default()));

    // Signal thread: on the first SIGINT/SIGTERM, win the shutdown race and clear
    // everything via a fresh connection, then exit. The main loop must not
    // re-report after this runs, so it parks once it observes `stopping`.
    #[cfg(unix)]
    {
        let mut signals = Signals::new([SIGINT, SIGTERM])?;
        let stopping = Arc::clone(&stopping);
        let tracked = Arc::clone(&tracked);
        thread::spawn(move || {
            if signals.forever().next().is_some() && !stopping.swap(true, Ordering::SeqCst) {
                shutdown(herdr::connect().ok().as_mut(), &tracked);
            }
        });
    }

    // Windows equivalent: wait on the named stop-event that `disable_updater`
    // sets, then flip the same `stopping` flag the main loop already checks each
    // iteration — the loop's existing shutdown path (clear tracked statuses +
    // title, unlink pid, exit) takes it from there, matching the Unix signal path.
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
        use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject, INFINITE};
        let stopping = Arc::clone(&stopping);
        let tracked = Arc::clone(&tracked);
        let name = stop_event_name();
        thread::spawn(move || {
            // SAFETY: name is NUL-terminated; handle is waited on then closed once.
            unsafe {
                let h = CreateEventW(std::ptr::null(), 1, 0, name.as_ptr());
                if h.is_null() {
                    return;
                }
                if WaitForSingleObject(h, INFINITE) == WAIT_OBJECT_0
                    && !stopping.swap(true, Ordering::SeqCst)
                {
                    shutdown(herdr::connect().ok().as_mut(), &tracked);
                }
                CloseHandle(h);
            }
        });
    }

    let daemon_interval_ms = config.interval_seconds * 1000;
    let mut window_ms: u64 = 500; // quick first sample so the sidebar updates immediately
    let mut failures: u32 = 0;
    loop {
        match collect::snapshot(&mut client, window_ms) {
            Ok(spaces) => {
                if stopping.load(Ordering::SeqCst) {
                    park(); // shutdown ran during the sample window — do not re-report
                }
                {
                    let mut guard = tracked.lock().expect("tracked mutex poisoned");
                    push_statuses(&mut client, &spaces, &config, &labels, &mut guard);
                }
                if config.window_title_totals {
                    set_title_totals(&mut client, &spaces, &labels);
                }
                failures = 0;
            }
            Err(_) => {
                failures += 1;
                if failures >= 5 && !stopping.swap(true, Ordering::SeqCst) {
                    shutdown(Some(&mut client), &tracked); // herdr server likely gone
                }
                thread::sleep(Duration::from_secs(1));
                if stopping.load(Ordering::SeqCst) {
                    park();
                }
            }
        }
        window_ms = daemon_interval_ms;
    }
}

/// `--enable`: spawn a detached `--daemon` process (no-op if already running).
pub fn enable_updater() -> crate::Result<()> {
    if daemon_pid().is_some() {
        notify("sidebar usage already enabled");
        return Ok(());
    }

    // Re-exec ourselves as the daemon, fully detached: a new session (setsid) so
    // it survives the controlling terminal, and null stdio — mirrors Node's
    // `spawn(.., { detached: true, stdio: 'ignore' })` + `child.unref()`.
    let exe = std::env::current_exe()?;
    let mut cmd = Command::new(exe);
    cmd.arg("--daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: `setsid` is async-signal-safe and the only action taken in the
    // forked child before exec; it starts a new session, detaching the daemon.
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(|| match libc::setsid() {
            -1 => Err(std::io::Error::last_os_error()),
            _ => Ok(()),
        });
    }
    // Windows: no controlling terminal/session to detach from, but spawn fully
    // background — no console window and not tied to this process's job/console.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW);
    }
    cmd.spawn()?; // do not wait — the child outlives us and is reaped by init

    notify("sidebar usage enabled");
    Ok(())
}

/// `--disable`: signal the daemon and sweep any leftover statuses / title.
pub fn disable_updater() -> crate::Result<()> {
    if let Some(pid) = daemon_pid() {
        // The daemon clears its own statuses + title on shutdown; best-effort.
        #[cfg(unix)]
        // SAFETY: `kill` merely posts SIGTERM to the pid; failure is ignored.
        unsafe {
            libc::kill(pid as i32, SIGTERM);
        }
        #[cfg(windows)]
        {
            let _ = pid; // no per-pid signal on Windows; the named event covers it
            let _ = signal_stop_event(); // best-effort; sweep below still runs
        }
    }

    // Belt and braces: sweep every current pane in case the daemon died — release
    // pseudo-agents (no TTL) and clear metadata statuses — then clear the title.
    // If herdr is unavailable, metadata TTLs expire the statuses anyway.
    if let Ok(mut client) = herdr::connect() {
        if let Ok(spaces) = collect::collect_spaces(&mut client) {
            let mut sweep = Tracked::default();
            for sp in &spaces {
                sweep.pseudo.extend(sp.pseudo_panes.iter().cloned());
                sweep.metadata.extend(sp.agent_panes.iter().cloned());
                sweep.metadata.extend(sp.spare_panes.iter().cloned());
            }
            clear_all(&mut client, &sweep);
        }
        let _ = client.window_title_clear();
    }

    notify("sidebar usage disabled");
    Ok(())
}

/// `--toggle`: disable if a daemon is live, else enable.
pub fn toggle_updater() -> crate::Result<()> {
    if daemon_pid().is_some() {
        disable_updater()
    } else {
        enable_updater()
    }
}

/// Push each space's usage status onto a pane, mode-dependent, recording the
/// touched panes in `tracked`.
///
/// agents-panel mode: release any stale pseudo-claims beyond the first, then
/// report the "usage" pseudo-agent (state `idle`) on the space's first pseudo /
/// spare pane; on success that space is done. sidebar mode (and the agents-panel
/// fall-through when the pseudo report fails): release leftover pseudo-agents,
/// then report TTL'd metadata on the first spare pane (else the first agent pane).
pub fn push_statuses(
    client: &mut Herdr,
    spaces: &[Space],
    config: &Config,
    labels: &Labels,
    tracked: &mut Tracked,
) {
    let source = config::plugin_id();
    let ttl_ms = config.interval_seconds * 1000 * 3;

    for sp in spaces {
        let status = status_line(sp, labels);

        if config.mode == Mode::AgentsPanel {
            // Drop stale claims from earlier runs so a space keeps one entry.
            for extra in sp.pseudo_panes.iter().skip(1) {
                release_pseudo(client, extra, &source);
            }
            let pane = sp.pseudo_panes.first().or_else(|| sp.spare_panes.first());
            if let Some(pane) = pane {
                if client
                    .report_agent(pane, &source, PSEUDO_AGENT, "idle", &status)
                    .is_ok()
                {
                    tracked.pseudo.insert(pane.clone());
                    continue; // dedicated panel entry covers this space
                }
                // pane just closed — fall through to metadata
            }
        } else {
            // sidebar mode: release pseudo-agents left over from agents-panel mode
            // or pre-v0.5 versions (report-agent entries have no TTL).
            for pane_id in &sp.pseudo_panes {
                release_pseudo(client, pane_id, &source);
            }
        }

        let targets = if !sp.spare_panes.is_empty() {
            &sp.spare_panes[..1]
        } else if !sp.agent_panes.is_empty() {
            &sp.agent_panes[..1]
        } else {
            &[][..]
        };
        for pane_id in targets {
            if client
                .report_metadata_status(pane_id, &source, &status, ttl_ms)
                .is_ok()
            {
                tracked.metadata.insert(pane_id.clone());
            }
        }
    }
}

/// Release every pseudo-agent and clear every metadata status in `tracked`.
pub fn clear_all(client: &mut Herdr, tracked: &Tracked) {
    let source = config::plugin_id();
    for pane_id in &tracked.pseudo {
        release_pseudo(client, pane_id, &source);
    }
    for pane_id in &tracked.metadata {
        let _ = client.clear_metadata_status(pane_id, &source);
    }
}

/// Write the all-space CPU/RAM totals to the client window title.
pub fn set_title_totals(client: &mut Herdr, spaces: &[Space], labels: &Labels) {
    let mut cpu = 0.0;
    let mut ram_mb = 0.0;
    for sp in spaces {
        cpu += sp.cpu;
        ram_mb += sp.ram_mb;
    }
    let title = format!(
        "spaces · {} {}% · {} {}",
        labels.cpu,
        cpu.round() as i64,
        labels.ram,
        ram_display(ram_mb),
    );
    let _ = client.window_title_set(&title);
}

// ---- helpers ----------------------------------------------------------------

/// Name of the named event used to signal the daemon to stop on Windows
/// (Unix uses SIGTERM instead). Scoped to the plugin id so multiple herdr
/// instances/configs don't collide.
#[cfg(windows)]
fn stop_event_name() -> Vec<u16> {
    let name = format!(r"Local\herdr-space-usage-stop-{}", config::plugin_id());
    name.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Open-or-create the stop event and set it; the daemon's wait-thread (see
/// `run_daemon`) wakes on this and flips the shared `stopping` flag. Best-effort:
/// errors are swallowed by the caller since the belt-and-braces sweep in
/// `disable_updater` still covers a dead/unresponsive daemon.
#[cfg(windows)]
fn signal_stop_event() -> std::io::Result<()> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{CreateEventW, SetEvent};
    let name = stop_event_name();
    // SAFETY: name is NUL-terminated; handle is checked and closed once.
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

/// Clear tracked statuses + title, unlink the pid file, and `exit(0)`.
///
/// Shared by the signal thread (own connection) and the five-failure path (main
/// connection). `client` is `None` only when no socket could be opened, in which
/// case the pid file is still removed before exiting. Never returns.
fn shutdown(client: Option<&mut Herdr>, tracked: &Mutex<Tracked>) -> ! {
    if let Some(client) = client {
        if let Ok(tracked) = tracked.lock() {
            clear_all(client, &tracked);
        }
        let _ = client.window_title_clear();
    }
    let _ = std::fs::remove_file(config::pid_file());
    std::process::exit(0);
}

/// Idle forever while the signal thread completes its shutdown and `exit(0)`s the
/// whole process; keeps the main loop from re-reporting or racing that exit.
fn park() -> ! {
    loop {
        thread::sleep(Duration::from_secs(3600));
    }
}

/// The per-space status text: `"<cpu> <n>% · <ram> <pct-or-compact>"`.
fn status_line(sp: &Space, labels: &Labels) -> String {
    format!(
        "{} {}% · {} {}",
        labels.cpu,
        sp.cpu.round() as i64,
        labels.ram,
        ram_display(sp.ram_mb),
    )
}

/// RAM as a percent-of-total string, falling back to the compact absolute form
/// when `/proc/meminfo` is unreadable (JS `ramPct(mb) || compactRam(mb)`).
fn ram_display(mb: f64) -> String {
    let pct = proc::ram_pct(mb);
    if pct.is_empty() {
        compact_ram(mb)
    } else {
        pct
    }
}

/// Compact absolute RAM: `"<x.x>G"` at/above 1024 MB, else `"<n>M"`
/// (JS `compactRam`).
fn compact_ram(mb: f64) -> String {
    if mb >= 1024.0 {
        format!("{:.1}G", mb / 1024.0)
    } else {
        format!("{}M", mb.round() as i64)
    }
}

/// Best-effort release of our pseudo-agent on `pane_id` (a closed pane errors and
/// is ignored — nothing to release).
fn release_pseudo(client: &mut Herdr, pane_id: &str, source: &str) {
    let _ = client.release_agent(pane_id, source, PSEUDO_AGENT);
}

/// Best-effort "Space usage" toast over a throwaway connection (JS `notify`).
fn notify(body: &str) {
    if let Ok(mut client) = herdr::connect() {
        let _ = client.notification_show("Space usage", body);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Labels;

    fn space(cpu: f64, ram_mb: f64) -> Space {
        Space {
            cpu,
            ram_mb,
            ..Default::default()
        }
    }

    #[test]
    fn compact_ram_switches_unit_at_1024() {
        assert_eq!(compact_ram(0.0), "0M");
        assert_eq!(compact_ram(512.6), "513M"); // Math.round
        assert_eq!(compact_ram(1023.4), "1023M"); // still MB below the gate
        assert_eq!(compact_ram(1024.0), "1.0G");
        assert_eq!(compact_ram(1536.0), "1.5G");
    }

    #[test]
    fn status_line_uses_labels_and_rounds_cpu() {
        let labels = Labels {
            cpu: "CPU".to_string(),
            ram: "MEM".to_string(),
        };
        // No /proc/meminfo total in most CI: ram_display falls back to compact.
        // Assert the CPU rounding + label layout, which are total-independent.
        let line = status_line(&space(5.6, 0.0), &labels);
        assert!(line.starts_with("CPU 6% · MEM "), "got: {line}");
    }

    #[test]
    fn status_line_rounds_cpu_half_away_from_zero() {
        let labels = Labels::default();
        assert!(status_line(&space(2.5, 0.0), &labels).starts_with("cpu 3%"));
        assert!(status_line(&space(2.4, 0.0), &labels).starts_with("cpu 2%"));
    }

    #[test]
    fn pid_alive_true_for_own_pid() {
        assert!(pid_alive(std::process::id()));
    }

    #[test]
    fn pid_alive_false_for_implausible_pid() {
        // Vanishingly unlikely to be a real pid on any platform under test.
        assert!(!pid_alive(u32::MAX));
    }
}
