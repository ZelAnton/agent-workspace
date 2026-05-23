// ===========================================================================
// vcs/backend - VcsBackend trait
// ===========================================================================
//
// The contract every VCS implementation (`GitBackend`, future `JjBackend`)
// must satisfy. Methods are grouped by purpose; the surface mirrors the
// original `git::*` free-function API one-for-one so the facade can stay
// purely syntactic.
//
// **API drift guard.** Adding a method here requires landing both:
//   - a real `GitBackend` implementation in `src/vcs/git/`, and
//   - an `unimplemented!("jj: <name>")` stub in `src/vcs/jj/mod.rs`.
//
// The jj stub is non-negotiable: it prevents the two backends silently
// diverging in their surfaces and gives users a clear error message when
// they run an unimplemented op against a jj repo today.

use std::path::{Path, PathBuf};

use super::common::{DiffStat, WorktreeInfo};
use super::error::Result;

/// All VCS-touching operations the rest of the codebase needs.
///
/// `Send + Sync` is required so a `&dyn VcsBackend` can cross thread
/// boundaries — today only one runs (the daily update check runs in its
/// own thread but doesn't touch VCS), but locking in the bound costs us
/// nothing and unblocks future parallelism (e.g. concurrent worktree
/// status queries).
pub trait VcsBackend: Send + Sync {
    /// Short backend identifier — `"git"` or `"jj"`.
    fn name(&self) -> &'static str;

    // -------------------------------------------------------------------
    // Identity
    // -------------------------------------------------------------------
    fn repo_root(&self) -> Result<PathBuf>;
    fn repo_name(&self) -> Result<String>;
    fn workspace_id(&self) -> Result<String>;
    fn current_branch(&self) -> Result<String>;
    fn current_commit(&self) -> Result<String>;
    fn detect_trunk(&self) -> Result<String>;

    // -------------------------------------------------------------------
    // Branches / bookmarks
    // -------------------------------------------------------------------
    fn local_branches(&self) -> Result<Vec<String>>;
    fn branch_exists(&self, name: &str) -> Result<bool>;
    fn is_merged(&self, branch: &str, target: &str) -> Result<bool>;
    fn has_diff_from(&self, branch: &str, target: &str) -> Result<bool>;
    fn delete_branch(&self, name: &str, force: bool) -> Result<()>;
    fn rename_branch(&self, old: &str, new: &str) -> Result<()>;
    fn log_oneline(&self, from: &str, to: &str) -> Result<String>;
    fn commit_count(&self, from: &str, to: &str) -> Result<usize>;
    fn diff_shortstat(&self, from: &str, to: &str) -> Result<DiffStat>;
    fn diff_shortstat_in(&self, path: &Path) -> Result<DiffStat>;

    // -------------------------------------------------------------------
    // Working-copy state
    // -------------------------------------------------------------------
    fn has_uncommitted_changes(&self) -> Result<bool>;
    fn uncommitted_count_in(&self, path: &Path) -> Result<usize>;
    fn has_changes_from_trunk(&self, trunk: &str) -> Result<bool>;

    /// True iff a rebase is currently in progress (git: `.git/rebase-*`).
    /// jj has no equivalent state; `JjBackend` returns `false`.
    fn is_rebase_in_progress(&self) -> bool;

    /// True iff a merge is currently in progress (git: `.git/MERGE_HEAD`).
    /// jj treats unresolved conflicts as first-class committed state, so the
    /// jj impl will read `jj st` for unresolved conflicts.
    fn is_merge_in_progress(&self) -> bool;

    // -------------------------------------------------------------------
    // Mutations
    // -------------------------------------------------------------------
    fn merge(
        &self,
        branch: &str,
        squash: bool,
        no_ff: bool,
        message: Option<&str>,
    ) -> Result<()>;
    fn dry_run_merge(&self, branch: &str, squash: bool) -> Result<bool>;
    fn rebase(&self, onto: &str) -> Result<()>;
    fn checkout(&self, branch: &str) -> Result<()>;
    fn commit(&self, message: &str) -> Result<()>;
    fn fetch(&self) -> Result<()>;
    fn rebase_abort(&self) -> Result<()>;
    fn rebase_continue(&self) -> Result<()>;
    fn merge_abort(&self) -> Result<()>;
    fn merge_continue(&self) -> Result<()>;
    fn reset_merge(&self) -> Result<()>;

    // -------------------------------------------------------------------
    // Worktrees / workspaces
    // -------------------------------------------------------------------
    fn list_worktrees(&self) -> Result<Vec<WorktreeInfo>>;
    fn create_worktree(
        &self,
        path: &Path,
        branch: &str,
        base: &str,
    ) -> Result<crate::vcs::common::CreateOutcome>;
    fn remove_worktree(&self, path: &Path, force: bool) -> Result<()>;
    fn move_worktree(&self, old: &Path, new: &Path) -> Result<()>;
}
