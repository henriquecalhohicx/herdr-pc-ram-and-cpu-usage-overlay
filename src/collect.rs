//! Snapshot → spaces, worktree grouping, and CPU/RAM measurement
//! (mirrors `index.js` lines 222-341).
//!
//! [`collect_spaces`] turns one `session.snapshot` (plus per-pane
//! `process_info`) into [`Space`]s. [`group_worktree_families`] and
//! [`aggregate_families`] fold worktree-child workspaces into their parent.
//! [`measure`] samples `/proc` CPU jiffies over a window and fills cpu/ram/proc
//! counts. [`snapshot`] is the top-level `collect → group → measure → aggregate`
//! pipeline.

use crate::herdr::Herdr;
use crate::model::Space;

/// Pseudo-agent label used to mark our agents-panel entries (agents-panel mode)
/// and to recognise / clean them up in sidebar mode.
pub const PSEUDO_AGENT: &str = "usage";

/// Enumerate spaces and the root shell PID of each of their panes from a single
/// `session.snapshot`, classifying panes into agent / spare / pseudo buckets.
pub fn collect_spaces(client: &mut Herdr) -> crate::Result<Vec<Space>> {
    todo!()
}

/// git branch of `cwd` via `git -C <cwd> rev-parse --abbrev-ref HEAD`
/// (empty string if `cwd` is `None` or not a repo).
pub fn git_branch(cwd: Option<&str>) -> String {
    todo!()
}

/// Tag worktree-child spaces with their group parent, one `worktree.list` per
/// unique repo. Children whose repo's main checkout is open get `family_parent`.
pub fn group_worktree_families(client: &mut Herdr, spaces: &mut [Space]) {
    todo!()
}

/// Sample CPU over `window_ms`, then fill `cpu` / `ram_mb` / `proc_count` on each
/// space by summing over each root's `/proc` subtree.
pub fn measure(spaces: &mut [Space], window_ms: u64) {
    todo!()
}

/// Fold measured worktree children into their parent (summing cpu/ram/procs/
/// panes and collecting labels), returning the spaces without folded children.
pub fn aggregate_families(spaces: Vec<Space>) -> Vec<Space> {
    todo!()
}

/// Full pipeline: collect → group worktrees → measure (`window_ms`) → aggregate.
pub fn snapshot(client: &mut Herdr, window_ms: u64) -> crate::Result<Vec<Space>> {
    todo!()
}
