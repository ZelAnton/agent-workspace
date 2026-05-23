// ===========================================================================
// vcs/git/worktree - Worktree CRUD + porcelain parser
// ===========================================================================

use std::path::{Path, PathBuf};

use vcs_runner::{Cmd, RunError, Runner};

use super::errmap::map_run_err;
use crate::vcs::common::{path_str, WorktreeInfo};
use crate::vcs::error::{Error, Result};

/// Create a new worktree.
///
/// If `branch` already exists locally, check it out into the new worktree;
/// otherwise create the branch from `base` and check it out.
pub(super) fn create_worktree(
    runner: &dyn Runner,
    path: &Path,
    branch: &str,
    base: &str,
) -> Result<()> {
    let path_arg = path_str(path)?;

    if super::repo::branch_exists(runner, branch)? {
        let worktrees = list_worktrees(runner)?;
        if worktrees.iter().any(|wt| wt.branch.as_deref() == Some(branch)) {
            return Err(Error::WorktreeExists(branch.to_string()));
        }
        super::exec(runner, &["worktree", "add", path_arg, branch])
    } else {
        super::exec(runner, &["worktree", "add", "-b", branch, path_arg, base])
    }
}

/// Remove a worktree.
pub(super) fn remove_worktree(runner: &dyn Runner, path: &Path, force: bool) -> Result<()> {
    let path_arg = path_str(path)?;
    if force {
        super::exec(runner, &["worktree", "remove", "--force", path_arg])
    } else {
        super::exec(runner, &["worktree", "remove", path_arg])
    }
}

/// Move a worktree to a new path.
pub(super) fn move_worktree(runner: &dyn Runner, old_path: &Path, new_path: &Path) -> Result<()> {
    super::exec(
        runner,
        &["worktree", "move", path_str(old_path)?, path_str(new_path)?],
    )
}

/// List all worktrees attached to the repo.
pub(super) fn list_worktrees(runner: &dyn Runner) -> Result<Vec<WorktreeInfo>> {
    let cwd = std::env::current_dir()?;
    let out = runner
        .run(Cmd::new("git").in_dir(&cwd).args(["worktree", "list", "--porcelain"]))
        .map_err(|e| match e {
            RunError::NonZeroExit { .. } => Error::NotInRepo,
            other => map_run_err(other),
        })?;
    Ok(parse_worktree_list(&out.stdout_lossy()))
}

/// Parse `git worktree list --porcelain` output.
///
/// Kept `pub(crate)` so tests can hit it directly with literal fixtures.
///
/// Git's porcelain schema:
/// ```text
/// worktree /path/to/main
/// HEAD <sha>
/// branch refs/heads/main
///
/// worktree /path/to/feature
/// HEAD <sha>
/// branch refs/heads/feature
///
/// worktree /path/to/detached
/// HEAD <sha>
/// detached
/// ```
///
/// **This parser is git-specific** — jj's `jj workspace list` uses an
/// entirely different schema. `JjBackend` will need its own normalizer
/// that emits `WorktreeInfo` from `jj workspace list -T <template>`.
pub fn parse_worktree_list(content: &str) -> Vec<WorktreeInfo> {
    let mut worktrees = Vec::new();
    let mut current: Option<WorktreeInfo> = None;

    for line in content.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            if let Some(wt) = current.take() {
                worktrees.push(wt);
            }
            current = Some(WorktreeInfo {
                path: PathBuf::from(path),
                branch: None,
                commit: None,
                is_bare: false,
            });
        } else if let Some(ref mut wt) = current {
            if let Some(branch) = line.strip_prefix("branch refs/heads/") {
                wt.branch = Some(branch.to_string());
            } else if let Some(commit) = line.strip_prefix("HEAD ") {
                wt.commit = Some(commit.to_string());
            } else if line == "bare" {
                wt.is_bare = true;
            }
        }
    }

    if let Some(wt) = current {
        worktrees.push(wt);
    }

    worktrees
}
