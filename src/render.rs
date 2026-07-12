//! Human and JSON rendering plus the `--once` / `--json` / `--interval` run modes
//! (mirrors `index.js` lines 618-681 and 691-706).
//!
//! [`render`] builds the coloured multi-line terminal report; [`render_json`]
//! builds the machine-readable payload. The `run_*` helpers drive a
//! [`collect::snapshot`](crate::collect::snapshot) and print the result, with
//! `run_interval` clearing and redrawing each frame.

use crate::config::Labels;
use crate::herdr::Herdr;
use crate::model::Space;

/// Format the per-space CPU/RAM report as a coloured, multi-line string.
pub fn render(spaces: &[Space], labels: &Labels) -> String {
    todo!()
}

/// Serialize spaces to the `--json` payload (array of per-space objects).
pub fn render_json(spaces: &[Space]) -> String {
    todo!()
}

/// `--once`: print a single rendered snapshot and return.
pub fn run_once(client: &mut Herdr, labels: &Labels) -> crate::Result<()> {
    todo!()
}

/// `--json`: print one JSON snapshot and return.
pub fn run_json(client: &mut Herdr) -> crate::Result<()> {
    todo!()
}

/// `--interval`: live watch, redrawing every `interval_ms` (first frame quick).
pub fn run_interval(client: &mut Herdr, labels: &Labels, interval_ms: u64) -> crate::Result<()> {
    todo!()
}
