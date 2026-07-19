//! Data types shared across the plugin.
//!
//! [`Space`] is the internal per-workspace aggregate (mirrors the `Space`
//! typedef in `index.js` lines 6-21). The remaining types are `serde` views
//! over the `result` payloads of the herdr socket read methods we call
//! (`workspace.list`, `pane.list`, `pane.process_info`, `worktree.list`). Each
//! only declares the fields we consume; serde ignores the rest of herdr's
//! payload.

use serde::Deserialize;

/// CPU / RAM aggregate for one herdr space (workspace).
///
/// Fields correspond 1:1 to the JS `Space` typedef:
/// - `roots` are the shell PIDs of each pane (process-tree roots).
/// - `cpu` / `ram_mb` / `proc_count` are filled in by the measure step.
/// - `family_parent` / `worktree_labels` are set when a worktree child is
///   folded into its parent space.
#[derive(Debug, Clone, Default)]
pub struct Space {
    /// herdr workspace id.
    pub id: String,
    pub label: String,
    pub focused: bool,
    pub pane_count: usize,
    /// git branch of the first pane's cwd (empty if none).
    pub branch: String,
    /// shell PIDs of each pane (process-tree roots).
    pub roots: Vec<u32>,
    /// panes with a real agent.
    pub agent_panes: Vec<String>,
    /// plain shell panes.
    pub spare_panes: Vec<String>,
    /// panes carrying our "usage" pseudo-agent.
    pub pseudo_panes: Vec<String>,
    /// panes we hold a "usage" claim on that actually run a real agent (herdr
    /// glyph present) — our claim is masking it, so release rather than reuse.
    pub masked_pseudo_panes: Vec<String>,
    /// `claude` agent panes and their herdr agent_status (for the cache timer).
    pub claude_panes: Vec<ClaudePane>,
    /// CPU % of the whole machine (all cores), filled by measure.
    pub cpu: f64,
    /// RSS MB, filled by measure.
    pub ram_mb: f64,
    /// processes counted, filled by measure.
    pub proc_count: usize,
    /// workspace id of the worktree-group parent.
    pub family_parent: Option<String>,
    /// labels of folded worktree children.
    pub worktree_labels: Option<Vec<String>>,
}

/// A `claude` agent pane plus its herdr `agent_status`, used by the per-agent
/// cache countdown timer. Collected in `collect_spaces` and folded upward in
/// `aggregate_families` so worktree-child agents keep their timer.
#[derive(Debug, Clone, Default)]
pub struct ClaudePane {
    pub pane_id: String,
    pub status: Option<String>,
}

// ---- workspace.list ---------------------------------------------------------
//
// result = { "type": "workspace_list", "workspaces": [ { workspace_id, label,
//            focused, .. } ] }

/// `result` payload of `workspace.list`.
#[derive(Debug, Clone, Deserialize)]
pub struct WorkspaceListResult {
    #[serde(default)]
    pub workspaces: Vec<WorkspaceInfo>,
}

/// One entry of `workspaces`; only the fields we consume are modelled.
#[derive(Debug, Clone, Deserialize)]
pub struct WorkspaceInfo {
    pub workspace_id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub focused: bool,
}

// ---- pane.list --------------------------------------------------------------
//
// result = { "type": "pane_list", "panes": [ { pane_id, cwd?, agent?, .. } ] }

/// `result` payload of `pane.list`.
#[derive(Debug, Clone, Deserialize)]
pub struct PaneListResult {
    #[serde(default)]
    pub panes: Vec<PaneInfo>,
}

/// One entry of `panes`; only the fields we consume are modelled.
#[derive(Debug, Clone, Deserialize)]
pub struct PaneInfo {
    pub pane_id: String,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub agent_status: Option<String>,
    /// Raw terminal title (herdr prefixes a glyph for a detected agent).
    #[serde(default)]
    pub terminal_title: Option<String>,
    /// De-glyphed terminal title. A raw title that differs from this means herdr
    /// detects a real agent in the pane — even when our own `usage` pseudo-agent
    /// claim is masking the `agent` field.
    #[serde(default)]
    pub terminal_title_stripped: Option<String>,
}

// ---- pane.process_info ------------------------------------------------------
//
// result = { "type": "pane_process_info", "process_info": { shell_pid?, .. } }

/// `result` payload of `pane.process_info`.
#[derive(Debug, Clone, Deserialize)]
pub struct ProcessInfoResult {
    pub process_info: ProcessInfo,
}

/// The `process_info` object; we only need the shell PID.
#[derive(Debug, Clone, Deserialize)]
pub struct ProcessInfo {
    #[serde(default)]
    pub shell_pid: Option<u32>,
}

// ---- worktree.list ----------------------------------------------------------
//
// result = { "type": "worktree_list", "source": { .. }, "worktrees": [ .. ] }
// (this method ERRORS when the workspace is not a git repo)

/// `result` payload of `worktree.list`.
#[derive(Debug, Clone, Deserialize)]
pub struct WorktreeListResult {
    pub source: WorktreeSource,
    #[serde(default)]
    pub worktrees: Vec<WorktreeEntry>,
}

/// The `source` object identifying the repo and its main checkout's workspace.
#[derive(Debug, Clone, Deserialize)]
pub struct WorktreeSource {
    pub repo_key: String,
    #[serde(default)]
    pub source_workspace_id: Option<String>,
}

/// One entry of `worktrees`; only the open workspace id matters for grouping.
#[derive(Debug, Clone, Deserialize)]
pub struct WorktreeEntry {
    #[serde(default)]
    pub open_workspace_id: Option<String>,
}
