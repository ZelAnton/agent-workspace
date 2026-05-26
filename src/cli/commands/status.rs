// ===========================================================================
// ws status - Show current worktree information
// ===========================================================================

use crate::cli::{Error, Result};
use crate::config::Config;
use crate::vcs;
use crate::meta::{self, WorktreeMeta};

pub fn run(config: &Config) -> Result<()> {
    let current = vcs::current_branch()?;
    let workspace_id = vcs::workspace_id()?;
    let wt_dir = config.workspaces_dir.join(&workspace_id);
    let wt_path = wt_dir.join(&current);

    if !wt_path.exists() {
        return Err(Error::Other(format!(
            "Not in a managed worktree (branch: {current})"
        )));
    }

    let trunk = config.resolve_trunk();

    let meta_path = meta::meta_path_with_fallback(&wt_dir, &current);
    let loaded = WorktreeMeta::load(&meta_path).ok();

    let base_branch = loaded.as_ref().map(|m| m.base_branch.as_str());
    let effective_target = meta::resolve_target_branch(
        None,
        base_branch,
        |b| vcs::branch_exists(b).unwrap_or(false),
        &trunk,
    );

    let uncommitted = vcs::uncommitted_count_in(&wt_path).unwrap_or(0);
    let commits = vcs::commit_count(&effective_target, &current).unwrap_or(0);

    let diff = vcs::diff_shortstat(&effective_target, &current).unwrap_or(vcs::DiffStat {
        insertions: 0,
        deletions: 0,
    });
    let unstaged = vcs::diff_shortstat_in(&wt_path).unwrap_or(vcs::DiffStat {
        insertions: 0,
        deletions: 0,
    });

    println!("Branch:       {current}");

    if let Some(bb) = base_branch {
        println!("Base branch:  {bb}");
    }

    println!("Trunk:        {trunk}");
    println!("Merge target: {effective_target}");

    if let Some(ref m) = loaded {
        println!(
            "Created:      {}",
            m.created_at.format("%Y-%m-%d %H:%M:%S UTC")
        );
    }

    println!("Commits:      {commits}");
    println!("Uncommitted:  {uncommitted}");

    let total_ins = diff.insertions + unstaged.insertions;
    let total_del = diff.deletions + unstaged.deletions;
    if total_ins > 0 || total_del > 0 {
        println!("Diff:         +{total_ins} -{total_del}");
    } else {
        println!("Diff:         -");
    }

    println!("Path:         {}", wt_path.display());

    // Show in-progress sync state (git-native only, no WT_MERGE_BRANCH)
    print_in_progress_state();

    Ok(())
}

/// Detect and display sync in-progress state.
///
/// jj has no "in progress" transient state — conflicts are recorded into
/// commits. When the active backend is jj and `is_merge_in_progress` is
/// true, that means `jj st` shows unresolved conflicts in the working
/// copy commit. The guidance is different: edit the files; jj snapshots
/// the resolution automatically. No `--continue`/`--abort` apply.
fn print_in_progress_state() {
    if vcs::is_rebase_in_progress() {
        // Only reachable on git — jj always returns false here.
        println!();
        println!("State:        REBASE IN PROGRESS (sync)");
        println!("  Resolve conflicts, then: ws sync --continue");
        println!("  Or abort: ws sync --abort");
    } else if vcs::is_merge_in_progress() {
        println!();
        if vcs::backend_name() == "jj" {
            println!("State:        CONFLICTS IN COMMIT");
            println!("  jj records conflicts in `@`. Resolve the markers in your files;");
            println!("  jj snapshots the resolution into `@` on the next command.");
            println!("  (No `ws sync --continue/--abort` — those are git-only.)");
        } else {
            println!("State:        MERGE IN PROGRESS (sync)");
            println!("  Resolve conflicts, then: ws sync --continue");
            println!("  Or abort: ws sync --abort");
        }
    }
}
