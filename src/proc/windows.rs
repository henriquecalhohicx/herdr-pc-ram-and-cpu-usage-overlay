//! Windows Win32 metrics backend (stub — implemented in Task 2).
use std::collections::{HashMap, HashSet};

use super::ProcEntry;

pub fn clk_tck() -> u64 { 10_000_000 }
pub fn nproc() -> u64 { 1 }
pub fn scan_proc() -> HashMap<u32, ProcEntry> { HashMap::new() }
pub fn mem_total_mb() -> f64 { 0.0 }
pub fn rss_mb(_pids: &HashSet<u32>) -> f64 { 0.0 }
