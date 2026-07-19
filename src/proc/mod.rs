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
        let procs: HashMap<u32, ProcEntry> = [(1, 0), (2, 1), (3, 1), (4, 2), (5, 4), (6, 999)]
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
