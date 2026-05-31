// ===========================================================================
// ws status - Show current worktree information
// ===========================================================================

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::cli::output::{self, OutputFormat, Render};
use crate::cli::{Error, Result};
use crate::config::Config;
use crate::meta::{self, WorktreeMeta};
use crate::vcs;

pub fn run(config: &Config, format: OutputFormat, repo: &vcs::Repo) -> Result<()> {
    let current = repo.current_branch()?;
    let workspace_id = repo.workspace_id()?;
    let wt_dir = config.project_dir_for(&workspace_id);
    let wt_path = wt_dir.join(&current);

    if !wt_path.exists() {
        return Err(Error::Other(format!(
            "Not in a managed worktree (branch: {current})"
        )));
    }

    let trunk = config.resolve_trunk();

    let meta_path = meta::meta_path_with_fallback(&wt_dir, &current);
    let loaded = WorktreeMeta::load(&meta_path).ok();

    let base_branch = loaded.as_ref().map(|m| m.base_branch.clone());
    let effective_target = meta::resolve_target_branch(
        None,
        base_branch.as_deref(),
        |b| repo.branch_exists(b).unwrap_or(false),
        &trunk,
    );

    let uncommitted = repo.uncommitted_count_in(&wt_path).unwrap_or(0);
    let commits = repo.commit_count(&effective_target, &current).unwrap_or(0);

    let diff = repo.diff_shortstat(&effective_target, &current).unwrap_or(vcs::DiffStat {
        insertions: 0,
        deletions: 0,
    });
    let unstaged = repo.diff_shortstat_in(&wt_path).unwrap_or(vcs::DiffStat {
        insertions: 0,
        deletions: 0,
    });

    let view = StatusView {
        branch: current,
        base_branch,
        trunk,
        merge_target: effective_target,
        created_at: loaded.as_ref().map(|m| m.created_at),
        commits,
        uncommitted,
        insertions: diff.insertions + unstaged.insertions,
        deletions: diff.deletions + unstaged.deletions,
        path: wt_path.display().to_string(),
        in_progress: detect_in_progress_state(repo),
    };

    output::emit(&view, format);
    Ok(())
}

/// Machine-facing `ws status` payload. `in_progress` exposes the sync state
/// machine-readably (agents no longer scrape "REBASE IN PROGRESS").
#[derive(Serialize)]
struct StatusView {
    branch: String,
    base_branch: Option<String>,
    trunk: String,
    merge_target: String,
    created_at: Option<DateTime<Utc>>,
    commits: usize,
    uncommitted: usize,
    insertions: usize,
    deletions: usize,
    path: String,
    in_progress: Option<InProgressState>,
}

/// In-progress `ws sync` state. jj has no transient in-progress state —
/// conflicts are recorded into the working-copy commit — hence the distinct
/// `jj_conflicts` variant with different recovery guidance.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum InProgressState {
    RebaseSync,
    MergeSync,
    JjConflicts,
}

/// Detect the in-progress sync state (git-native; jj records conflicts in `@`).
fn detect_in_progress_state(repo: &vcs::Repo) -> Option<InProgressState> {
    if repo.is_rebase_in_progress() {
        // Only reachable on git — jj always returns false here.
        Some(InProgressState::RebaseSync)
    } else if repo.is_merge_in_progress() {
        if repo.backend_name() == "jj" {
            Some(InProgressState::JjConflicts)
        } else {
            Some(InProgressState::MergeSync)
        }
    } else {
        None
    }
}

impl Render for StatusView {
    fn render_human(&self) {
        println!("Branch:       {}", self.branch);
        if let Some(bb) = &self.base_branch {
            println!("Base branch:  {bb}");
        }
        println!("Trunk:        {}", self.trunk);
        println!("Merge target: {}", self.merge_target);
        if let Some(created) = self.created_at {
            println!("Created:      {}", created.format("%Y-%m-%d %H:%M:%S UTC"));
        }
        println!("Commits:      {}", self.commits);
        println!("Uncommitted:  {}", self.uncommitted);
        if self.insertions > 0 || self.deletions > 0 {
            println!("Diff:         +{} -{}", self.insertions, self.deletions);
        } else {
            println!("Diff:         -");
        }
        println!("Path:         {}", self.path);

        match self.in_progress {
            Some(InProgressState::RebaseSync) => {
                println!();
                println!("State:        REBASE IN PROGRESS (sync)");
                println!("  Resolve conflicts, then: ws sync --continue");
                println!("  Or abort: ws sync --abort");
            }
            Some(InProgressState::MergeSync) => {
                println!();
                println!("State:        MERGE IN PROGRESS (sync)");
                println!("  Resolve conflicts, then: ws sync --continue");
                println!("  Or abort: ws sync --abort");
            }
            Some(InProgressState::JjConflicts) => {
                println!();
                println!("State:        CONFLICTS IN COMMIT");
                println!("  jj records conflicts in `@`. Resolve the markers in your files;");
                println!("  jj snapshots the resolution into `@` on the next command.");
                println!("  (No `ws sync --continue/--abort` — those are git-only.)");
            }
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_progress_serializes_snake_case() {
        assert_eq!(
            serde_json::to_value(InProgressState::JjConflicts).unwrap(),
            serde_json::json!("jj_conflicts")
        );
        assert_eq!(
            serde_json::to_value(InProgressState::RebaseSync).unwrap(),
            serde_json::json!("rebase_sync")
        );
    }

    #[test]
    fn status_view_json_shape() {
        let view = StatusView {
            branch: "b".into(),
            base_branch: None,
            trunk: "main".into(),
            merge_target: "main".into(),
            created_at: None,
            commits: 1,
            uncommitted: 0,
            insertions: 0,
            deletions: 0,
            path: "/x".into(),
            in_progress: None,
        };
        let v = serde_json::to_value(&view).unwrap();
        assert_eq!(v["branch"], "b");
        assert_eq!(v["merge_target"], "main");
        assert!(v["in_progress"].is_null());
        assert!(v["base_branch"].is_null());
    }
}
