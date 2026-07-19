//! Snapshot → spaces, worktree grouping, and CPU/RAM measurement
//! (mirrors `index.js` lines 222-341).
//!
//! [`collect_spaces`] turns `workspace.list` + per-workspace `pane.list` (plus
//! per-pane `process_info`) into [`Space`]s. [`group_worktree_families`] and
//! [`aggregate_families`] fold worktree-child workspaces into their parent.
//! [`measure`] samples `/proc` CPU jiffies over a window and fills cpu/ram/proc
//! counts. [`snapshot`] is the top-level `collect → group → measure → aggregate`
//! pipeline.

use std::collections::{HashMap, HashSet};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::herdr::Herdr;
use crate::model::{ClaudePane, Space};
use crate::proc;

/// Pseudo-agent label used to mark our agents-panel entries (agents-panel mode)
/// and to recognise / clean them up in sidebar mode.
pub const PSEUDO_AGENT: &str = "usage";

/// Enumerate spaces and the root shell PID of each of their panes, classifying
/// panes into agent / spare / pseudo buckets.
///
/// Mirrors the JS exactly (index.js:222-258): one `workspace.list`, then one
/// `pane.list` per workspace whose panes arrive already ordered — so the "first
/// pane's cwd" (used for the git branch) matches without any regrouping.
/// `pane.process_info` is per-pane (no bulk form), and a pane that closed
/// mid-scan simply contributes no root.
pub fn collect_spaces(client: &mut Herdr) -> crate::Result<Vec<Space>> {
    let workspaces = client.workspace_list()?;

    let mut spaces = Vec::with_capacity(workspaces.len());
    for ws in workspaces {
        let panes = client.pane_list(&ws.workspace_id)?;

        let mut roots = Vec::new();
        let mut agent_panes = Vec::new(); // panes with a real agent — sidebar rows
        let mut spare_panes = Vec::new(); // plain shell panes — pseudo-agent hosts
        let mut pseudo_panes = Vec::new(); // panes already carrying our "usage" agent
        let mut masked_pseudo_panes = Vec::new(); // our "usage" claim over a real agent
        let mut claude_panes = Vec::new(); // claude agent panes + status (cache timer)
        let mut cwd: Option<&str> = None;

        for pane in &panes {
            // First pane with a non-empty cwd wins (JS `if (!cwd && pane.cwd)`).
            if cwd.is_none() {
                if let Some(c) = pane.cwd.as_deref().filter(|c| !c.is_empty()) {
                    cwd = Some(c);
                }
            }
            // A herdr agent glyph in the title means a real agent runs here even
            // when our own "usage" claim (or detection lag) hides the `agent`.
            let real_agent_glyph = pane_has_agent_glyph(
                pane.terminal_title.as_deref(),
                pane.terminal_title_stripped.as_deref(),
            );
            // Classify: our pseudo-agent, then any real (non-empty) agent, else a
            // plain shell pane. An empty-string agent is falsy in JS → spare.
            match pane.agent.as_deref() {
                // Our "usage" claim, but a real agent is under it → release it and
                // never reuse it as a host (would keep masking the agent).
                Some(PSEUDO_AGENT) if real_agent_glyph => {
                    masked_pseudo_panes.push(pane.pane_id.clone())
                }
                Some(PSEUDO_AGENT) => pseudo_panes.push(pane.pane_id.clone()),
                Some(agent) if !agent.is_empty() => agent_panes.push(pane.pane_id.clone()),
                // No reported agent, but a glyph says one is there (detection lag):
                // treat as an agent pane so we never claim it as a usage host.
                _ if real_agent_glyph => agent_panes.push(pane.pane_id.clone()),
                _ => spare_panes.push(pane.pane_id.clone()),
            }
            // Track claude agents for the per-agent cache countdown timer.
            if pane.agent.as_deref() == Some("claude") {
                claude_panes.push(ClaudePane {
                    pane_id: pane.pane_id.clone(),
                    status: pane.agent_status.clone(),
                });
            }
            // Best-effort shell PID; a pane that just closed errors and is skipped.
            if let Ok(info) = client.process_info(&pane.pane_id) {
                if let Some(pid) = info.shell_pid.filter(|&p| p != 0) {
                    roots.push(pid);
                }
            }
        }

        let label = if ws.label.is_empty() {
            ws.workspace_id.clone()
        } else {
            ws.label.clone()
        };
        let branch = git_branch(cwd);

        spaces.push(Space {
            id: ws.workspace_id,
            label,
            focused: ws.focused,
            pane_count: panes.len(),
            branch,
            roots,
            agent_panes,
            spare_panes,
            pseudo_panes,
            masked_pseudo_panes,
            claude_panes,
            ..Default::default()
        });
    }
    Ok(spaces)
}

/// Whether a pane carries a herdr agent-title glyph — i.e. herdr detects a real
/// agent in it. herdr prefixes a detected agent's title with a marker glyph and
/// exposes the de-glyphed form as `terminal_title_stripped`, so a raw title that
/// differs from its stripped form means an agent is present — even when our own
/// `usage` pseudo-agent claim is masking the pane's `agent` field. Agent-agnostic
/// (works for claude, codex, …); both titles absent/equal ⇒ no agent.
pub fn pane_has_agent_glyph(raw: Option<&str>, stripped: Option<&str>) -> bool {
    match (raw, stripped) {
        (Some(r), Some(s)) => !r.is_empty() && !s.is_empty() && r != s,
        _ => false,
    }
}

/// git branch of `cwd` via `git -C <cwd> rev-parse --abbrev-ref HEAD`
/// (empty string if `cwd` is `None`/empty or not a repo — the git call exiting
/// non-zero is swallowed exactly as the JS `try/catch` did).
pub fn git_branch(cwd: Option<&str>) -> String {
    let cwd = match cwd {
        Some(c) if !c.is_empty() => c,
        _ => return String::new(),
    };
    let mut cmd = Command::new("git");
    cmd.args(["-C", cwd, "rev-parse", "--abbrev-ref", "HEAD"])
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    // On Windows the daemon runs without a console (DETACHED_PROCESS), so each
    // `git` child would otherwise pop a conhost window that flashes on screen
    // every sample. CREATE_NO_WINDOW suppresses it. No-op / unneeded on Unix.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let output = cmd.output();
    match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        _ => String::new(),
    }
}

/// Tag worktree-child spaces with their group parent, one `worktree.list` per
/// unique repo. Children whose repo's main checkout is open get `family_parent`.
///
/// `worktree.list` errors for non-repo workspaces; that error is folded into
/// "leave it standalone". Parent/child resolution is done against an id→index
/// map and applied after the query loop so we never hold a `&mut` into `spaces`
/// while borrowing `client`.
pub fn group_worktree_families(client: &mut Herdr, spaces: &mut [Space]) {
    let index_of: HashMap<String, usize> = spaces
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id.clone(), i))
        .collect();
    let ids: Vec<String> = spaces.iter().map(|s| s.id.clone()).collect();

    let mut seen_repos: HashSet<String> = HashSet::new();
    let mut assignments: Vec<(usize, String)> = Vec::new(); // (child index, parent id)

    for ws_id in &ids {
        let res = match client.worktree_list(ws_id) {
            Ok(res) => res,
            Err(_) => continue, // workspace isn't in a git repo
        };
        let repo_key = res.source.repo_key;
        if repo_key.is_empty() || seen_repos.contains(&repo_key) {
            continue;
        }
        seen_repos.insert(repo_key);

        // The family only forms when the repo's main checkout is itself open.
        let parent_id = match res.source.source_workspace_id {
            Some(id) if index_of.contains_key(&id) => id,
            _ => continue, // main checkout isn't open — children stay standalone
        };
        for wt in res.worktrees {
            if let Some(child_id) = wt.open_workspace_id {
                if let Some(&child_idx) = index_of.get(&child_id) {
                    if child_id != parent_id {
                        assignments.push((child_idx, parent_id.clone()));
                    }
                }
            }
        }
    }

    for (child_idx, parent_id) in assignments {
        spaces[child_idx].family_parent = Some(parent_id);
    }
}

/// Sample CPU over `window_ms`, then fill `cpu` / `ram_mb` / `proc_count` on each
/// space by summing over each root's `/proc` subtree.
///
/// One `/proc` scan before, sleep, one after; per space the PID set is the union
/// of every root's subtree (built from the *after* children map). CPU% is
/// `Σ max(0, Δjiffies) / CLK_TCK / elapsed_s / NPROC * 100` — a share of the
/// whole machine (0..100). RSS and process count come from that same PID set.
pub fn measure(spaces: &mut [Space], window_ms: u64) {
    let before = proc::scan_proc();
    let start = Instant::now();
    std::thread::sleep(Duration::from_millis(window_ms));
    let after = proc::scan_proc();
    let elapsed = start.elapsed().as_secs_f64();
    let kids = proc::children_map(&after);

    let clk_tck = proc::clk_tck() as f64;
    let nproc = proc::nproc() as f64;

    for sp in spaces.iter_mut() {
        let mut pids: HashSet<u32> = HashSet::new();
        for &root in &sp.roots {
            pids.extend(proc::subtree(root, &kids));
        }

        let mut delta_jiffies: u64 = 0;
        for &pid in &pids {
            if let (Some(a), Some(b)) = (after.get(&pid), before.get(&pid)) {
                // `saturating_sub` is JS `Math.max(0, a - b)` — guards counter
                // resets / pid reuse.
                delta_jiffies += a.jiffies.saturating_sub(b.jiffies);
            }
        }

        sp.cpu = if elapsed > 0.0 {
            100.0 * (delta_jiffies as f64 / clk_tck) / elapsed / nproc
        } else {
            0.0
        };
        sp.ram_mb = proc::rss_mb(&pids);
        sp.proc_count = pids.len();
    }
}

/// Fold measured worktree children into their parent (summing cpu/ram/procs/
/// panes and collecting labels), returning the spaces without folded children.
///
/// Iterates by index and reads each child's *current* values at fold time, so a
/// child that is itself a parent accumulates before contributing upward —
/// matching the JS in-place mutation exactly. Every space carrying a
/// `family_parent` is dropped from the result, even if that parent was not
/// found (a missing parent means the child is not surfaced on its own).
pub fn aggregate_families(mut spaces: Vec<Space>) -> Vec<Space> {
    let index_of: HashMap<String, usize> = spaces
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id.clone(), i))
        .collect();

    for i in 0..spaces.len() {
        let Some(parent_id) = spaces[i].family_parent.clone() else {
            continue;
        };
        let Some(&parent_idx) = index_of.get(&parent_id) else {
            continue;
        };
        // Snapshot the child's contribution (immutable borrow ends here) before
        // taking a `&mut` to the parent — `parent_idx != i` always, but this also
        // sidesteps the borrow checker cleanly.
        let (cpu, ram_mb, proc_count, pane_count, label, claude_panes) = {
            let child = &spaces[i];
            (
                child.cpu,
                child.ram_mb,
                child.proc_count,
                child.pane_count,
                child.label.clone(),
                child.claude_panes.clone(),
            )
        };
        let parent = &mut spaces[parent_idx];
        parent.cpu += cpu;
        parent.ram_mb += ram_mb;
        parent.proc_count += proc_count;
        parent.pane_count += pane_count;
        parent.claude_panes.extend(claude_panes);
        parent
            .worktree_labels
            .get_or_insert_with(Vec::new)
            .push(label);
    }

    spaces.retain(|s| s.family_parent.is_none());
    spaces
}

/// Full pipeline: collect → group worktrees → measure (`window_ms`) → aggregate.
pub fn snapshot(client: &mut Herdr, window_ms: u64) -> crate::Result<Vec<Space>> {
    let mut spaces = collect_spaces(client)?;
    group_worktree_families(client, &mut spaces);
    measure(&mut spaces, window_ms);
    Ok(aggregate_families(spaces))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Space;

    /// Build a measured [`Space`] with just the aggregate-relevant fields set.
    fn space(id: &str, cpu: f64, ram_mb: f64, proc_count: usize, pane_count: usize) -> Space {
        Space {
            id: id.to_string(),
            label: id.to_string(),
            cpu,
            ram_mb,
            proc_count,
            pane_count,
            ..Default::default()
        }
    }

    #[test]
    fn aggregate_folds_child_into_parent_and_drops_it() {
        let parent = space("p", 10.0, 100.0, 3, 2);
        let mut child = space("c", 5.0, 50.0, 2, 1);
        child.label = "child".to_string();
        child.family_parent = Some("p".to_string());

        let out = aggregate_families(vec![parent, child]);

        assert_eq!(out.len(), 1, "the folded child is removed");
        let p = &out[0];
        assert_eq!(p.id, "p");
        assert_eq!(p.cpu, 15.0);
        assert_eq!(p.ram_mb, 150.0);
        assert_eq!(p.proc_count, 5);
        assert_eq!(p.pane_count, 3);
        assert_eq!(p.worktree_labels, Some(vec!["child".to_string()]));
    }

    #[test]
    fn aggregate_folds_multiple_children_preserving_label_order() {
        let parent = space("p", 0.0, 0.0, 0, 1);
        let mut c1 = space("c1", 1.0, 10.0, 1, 1);
        c1.label = "one".to_string();
        c1.family_parent = Some("p".to_string());
        let mut c2 = space("c2", 2.0, 20.0, 1, 1);
        c2.label = "two".to_string();
        c2.family_parent = Some("p".to_string());

        let out = aggregate_families(vec![parent, c1, c2]);

        assert_eq!(out.len(), 1);
        assert_eq!(out[0].cpu, 3.0);
        assert_eq!(out[0].ram_mb, 30.0);
        assert_eq!(out[0].pane_count, 3);
        assert_eq!(
            out[0].worktree_labels,
            Some(vec!["one".to_string(), "two".to_string()]),
        );
    }

    #[test]
    fn aggregate_drops_child_even_when_parent_is_missing() {
        // JS filters purely on `familyParent` being set, so a child whose parent
        // isn't present is still not surfaced standalone.
        let standalone = space("a", 1.0, 1.0, 1, 1);
        let mut orphan = space("o", 2.0, 2.0, 1, 1);
        orphan.family_parent = Some("ghost".to_string());

        let out = aggregate_families(vec![standalone, orphan]);

        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "a");
    }

    #[test]
    fn aggregate_leaves_standalone_spaces_untouched() {
        let out = aggregate_families(vec![space("a", 4.0, 8.0, 2, 2)]);
        assert_eq!(out.len(), 1);
        assert!(out[0].worktree_labels.is_none());
        assert_eq!(out[0].cpu, 4.0);
    }

    #[test]
    fn git_branch_empty_for_none_or_blank_cwd() {
        assert_eq!(git_branch(None), "");
        assert_eq!(git_branch(Some("")), "");
    }

    #[test]
    fn agent_glyph_detected_only_when_raw_differs_from_stripped() {
        // Real agent: herdr prefixes a glyph, stripped removes it.
        assert!(pane_has_agent_glyph(
            Some("✳ General conversation and check-in"),
            Some("General conversation and check-in"),
        ));
        // Plain shell: raw == stripped.
        let sh = "C:\\WINDOWS\\System32\\WindowsPowerShell\\v1.0\\powershell.exe";
        assert!(!pane_has_agent_glyph(Some(sh), Some(sh)));
        // Missing either title ⇒ no agent claimed.
        assert!(!pane_has_agent_glyph(Some("✳ X"), None));
        assert!(!pane_has_agent_glyph(None, Some("X")));
        assert!(!pane_has_agent_glyph(None, None));
        // Empty strings ⇒ no agent.
        assert!(!pane_has_agent_glyph(Some(""), Some("")));
    }

    #[test]
    fn aggregate_folds_child_claude_panes_into_parent() {
        use crate::model::ClaudePane;
        let mut parent = space("p", 0.0, 0.0, 0, 1);
        parent.claude_panes = vec![ClaudePane {
            pane_id: "p:p1".to_string(),
            status: Some("idle".to_string()),
        }];
        let mut child = space("c", 0.0, 0.0, 0, 1);
        child.family_parent = Some("p".to_string());
        child.claude_panes = vec![ClaudePane {
            pane_id: "c:p1".to_string(),
            status: Some("working".to_string()),
        }];

        let out = aggregate_families(vec![parent, child]);

        assert_eq!(out.len(), 1);
        let ids: Vec<&str> = out[0]
            .claude_panes
            .iter()
            .map(|c| c.pane_id.as_str())
            .collect();
        assert_eq!(
            ids,
            vec!["p:p1", "c:p1"],
            "child claude panes fold into parent"
        );
    }
}
