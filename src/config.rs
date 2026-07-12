//! Plugin config + herdr `[ui]` label loading, and env/state path resolution
//! (mirrors `index.js` lines 350-417).
//!
//! - [`load_config`] parses `$HERDR_PLUGIN_CONFIG_DIR/config.toml` (flat
//!   `key = value` lines).
//! - [`load_herdr_labels`] reads `cpu_label` / `ram_label` from herdr's OWN
//!   `[ui]` section so per-space rows match the patched sidebar header.
//! - The path helpers resolve the herdr-injected env (`HERDR_PLUGIN_*`) with the
//!   same `<tmpdir>/<id>` fallbacks the runtime uses.

use std::path::PathBuf;

/// Status-surfacing strategy (plugin `config.toml` `mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Stock herdr: a "usage" pseudo-agent per space in the agents panel.
    AgentsPanel,
    /// Patched herdr: display-only metadata rendered inside the spaces card.
    Sidebar,
}

/// Plugin user config from `$HERDR_PLUGIN_CONFIG_DIR/config.toml`.
#[derive(Debug, Clone)]
pub struct Config {
    pub mode: Mode,
    pub interval_seconds: u64,
    pub window_title_totals: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mode: Mode::AgentsPanel,
            interval_seconds: 5,
            window_title_totals: true,
        }
    }
}

/// CPU / RAM label tokens sourced from herdr's `[ui]` config (default cpu/ram).
#[derive(Debug, Clone)]
pub struct Labels {
    pub cpu: String,
    pub ram: String,
}

impl Default for Labels {
    fn default() -> Self {
        Self {
            cpu: "cpu".to_string(),
            ram: "ram".to_string(),
        }
    }
}

/// Load the plugin's own `config.toml`, returning defaults if it is absent.
pub fn load_config() -> Config {
    todo!()
}

/// Load `cpu_label` / `ram_label` from herdr's `[ui]` config section.
pub fn load_herdr_labels() -> Labels {
    todo!()
}

/// Plugin id (`HERDR_PLUGIN_ID`, else `ez-corp.space-usage`).
pub fn plugin_id() -> String {
    todo!()
}

/// Durable state dir (`HERDR_PLUGIN_STATE_DIR`, else `<tmpdir>/<id>`).
pub fn state_dir() -> PathBuf {
    todo!()
}

/// User config dir (`HERDR_PLUGIN_CONFIG_DIR`, else `<tmpdir>/<id>-config`).
pub fn config_dir() -> PathBuf {
    todo!()
}

/// Updater single-instance pid file (`<state_dir>/updater.pid`).
pub fn pid_file() -> PathBuf {
    todo!()
}
