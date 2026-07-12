//! Sidebar status updater daemon and its enable/disable/toggle controls
//! (mirrors `index.js` lines 343-616).
//!
//! The daemon refreshes each space's usage on a cadence, surfacing it either as
//! a "usage" pseudo-agent (agents-panel mode) or as TTL'd display-only metadata
//! (sidebar mode). A pid file under the state dir enforces a single instance;
//! statuses self-clear via their TTL if the daemon dies. `enable`/`disable`/
//! `toggle` spawn or signal that daemon and sweep leftover statuses.

use std::collections::HashSet;

use crate::config::{Config, Labels};
use crate::herdr::Herdr;
use crate::model::Space;

/// Panes we have pushed status onto this run, so shutdown can clear them.
#[derive(Debug, Default)]
pub struct Tracked {
    /// Panes carrying our pseudo-agent (released, not TTL'd).
    pub pseudo: HashSet<String>,
    /// Panes carrying TTL'd metadata statuses.
    pub metadata: HashSet<String>,
}

/// PID of a live updater daemon, or `None` (missing pid file / dead process).
pub fn daemon_pid() -> Option<u32> {
    todo!()
}

/// `--daemon`: run the updater loop until signalled, then clear and exit.
pub fn run_daemon() -> crate::Result<()> {
    todo!()
}

/// `--enable`: spawn a detached `--daemon` process (no-op if already running).
pub fn enable_updater() -> crate::Result<()> {
    todo!()
}

/// `--disable`: signal the daemon and sweep any leftover statuses / title.
pub fn disable_updater() -> crate::Result<()> {
    todo!()
}

/// `--toggle`: disable if a daemon is live, else enable.
pub fn toggle_updater() -> crate::Result<()> {
    todo!()
}

/// Push each space's usage status onto a pane, mode-dependent, recording the
/// touched panes in `tracked`.
pub fn push_statuses(
    client: &mut Herdr,
    spaces: &[Space],
    config: &Config,
    labels: &Labels,
    tracked: &mut Tracked,
) {
    todo!()
}

/// Release every pseudo-agent and clear every metadata status in `tracked`.
pub fn clear_all(client: &mut Herdr, tracked: &Tracked) {
    todo!()
}

/// Write the all-space CPU/RAM totals to the client window title.
pub fn set_title_totals(client: &mut Herdr, spaces: &[Space], labels: &Labels) {
    todo!()
}
