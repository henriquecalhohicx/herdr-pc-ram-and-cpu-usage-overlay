//! Socket-native herdr JSON-RPC client (replaces the per-sample CLI shell-outs
//! of `index.js` lines 82-124, hardened).
//!
//! One persistent [`UnixStream`] speaks herdr's newline-delimited JSON-RPC:
//!   request : `{"id":"<unique>","method":"<name>","params":{...}}\n`
//!   success : `{"id":..,"result":{"type":"<snake>",...}}`
//!   failure : `{"id":..,"error":{"code":..,"message":..}}`
//! Any non-`error` envelope is treated as success (mutations return
//! `{"type":"ok"}`). Method param field names are verified against herdr 0.7.1
//! `src/api/schema/panes.rs` (`PaneReportAgentParams`, `PaneReleaseAgentParams`,
//! `PaneReportMetadataParams`).
//!
//! The socket path is resolved `HERDR_SOCKET_PATH` → `$XDG_CONFIG_HOME/herdr` →
//! `~/.config/herdr/herdr.sock`. [`bin_path`] exposes the `HERDR_BIN_PATH` CLI as
//! a degraded fallback path.

use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use serde_json::Value;

use crate::model::{ProcessInfo, Snapshot, WorktreeListResult};

/// A live JSON-RPC connection to the herdr host.
pub struct Herdr {
    stream: UnixStream,
    next_id: u64,
}

/// Open a connection to the herdr socket.
pub fn connect() -> crate::Result<Herdr> {
    todo!()
}

impl Herdr {
    /// Send `method`/`params`, read exactly one reply line, and return its
    /// `result` object (or surface `error.message` as an `Err`). Reconnects once
    /// on a broken pipe.
    fn call(&mut self, method: &str, params: Value) -> crate::Result<Value> {
        todo!()
    }

    // ---- read methods -------------------------------------------------------

    /// `session.snapshot` — all workspaces + panes in one call.
    pub fn session_snapshot(&mut self) -> crate::Result<Snapshot> {
        todo!()
    }

    /// `pane.process_info` — the pane's shell PID (no bulk form exists).
    pub fn process_info(&mut self, pane_id: &str) -> crate::Result<ProcessInfo> {
        todo!()
    }

    /// `worktree.list` — errors if `workspace_id` is not a git repo.
    pub fn worktree_list(&mut self, workspace_id: &str) -> crate::Result<WorktreeListResult> {
        todo!()
    }

    // ---- mutation methods ---------------------------------------------------

    /// `pane.report_agent` — claim a pseudo-agent row (agents-panel mode).
    pub fn report_agent(
        &mut self,
        pane_id: &str,
        source: &str,
        agent: &str,
        state: &str,
        custom_status: &str,
    ) -> crate::Result<()> {
        todo!()
    }

    /// `pane.release_agent` — drop a previously reported pseudo-agent.
    pub fn release_agent(&mut self, pane_id: &str, source: &str, agent: &str) -> crate::Result<()> {
        todo!()
    }

    /// `pane.report_metadata` with a TTL'd `custom_status` (sidebar mode).
    pub fn report_metadata_status(
        &mut self,
        pane_id: &str,
        source: &str,
        custom_status: &str,
        ttl_ms: u64,
    ) -> crate::Result<()> {
        todo!()
    }

    /// `pane.report_metadata` with `clear_custom_status`.
    pub fn clear_metadata_status(&mut self, pane_id: &str, source: &str) -> crate::Result<()> {
        todo!()
    }

    /// `client.window_title.set`.
    pub fn window_title_set(&mut self, title: &str) -> crate::Result<()> {
        todo!()
    }

    /// `client.window_title.clear`.
    pub fn window_title_clear(&mut self) -> crate::Result<()> {
        todo!()
    }

    /// `notification.show` — best-effort toast.
    pub fn notification_show(&mut self, title: &str, body: &str) -> crate::Result<()> {
        todo!()
    }
}

/// Resolve the herdr socket path (`HERDR_SOCKET_PATH` → XDG → `~/.config`).
pub fn socket_path() -> crate::Result<PathBuf> {
    todo!()
}

/// The herdr CLI binary (`HERDR_BIN_PATH`, else `herdr`) for the fallback path.
pub fn bin_path() -> String {
    todo!()
}
