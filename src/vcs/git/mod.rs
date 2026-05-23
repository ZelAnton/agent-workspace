// ===========================================================================
// vcs/git - Git backend (the only real implementation today)
// ===========================================================================
//
// `GitBackend` wraps an `Arc<dyn procpilot::Runner>`. Production code uses
// `DefaultRunner` (spawns real subprocesses); tests can inject `MockRunner`
// to drive parser logic without a real git binary.
//
// The actual per-method implementations live in the submodules (`repo`,
// `branch`, `worktree`, `ops`). This file is the assembly — it builds the
// trait impl by delegating each method to a submodule function that takes
// `&dyn Runner` explicitly. That keeps the bodies testable in isolation
// and lets us swap runners without touching call sites.

mod errmap;
mod repo;
mod branch;
mod worktree;
mod ops;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use vcs_runner::{Cmd, DefaultRunner, Runner};

use super::backend::VcsBackend;
use super::common::{DiffStat, WorktreeInfo};
use super::error::Result;

// Re-export pure-function helpers used by tests and (post-Phase B) by any
// code path that wants to parse fixture text without a backend instance.
pub use branch::parse_shortstat;
pub use errmap::clean_git_error;
pub use repo::is_cwd_inside;
pub use worktree::parse_worktree_list;

/// Git-backed [`VcsBackend`]. Stores the subprocess runner so tests can
/// swap in a `MockRunner`.
pub struct GitBackend {
    runner: Arc<dyn Runner>,
}

impl GitBackend {
    /// Default constructor — uses the real subprocess runner.
    pub fn new() -> Self {
        Self { runner: Arc::new(DefaultRunner) }
    }

    /// Construct with a caller-supplied runner. Exposed (not gated on `cfg(test)`)
    /// because integration tests under `tests/` and downstream callers may
    /// want to substitute their own runner — e.g. to record subprocess args
    /// for debugging.
    pub fn with_runner(runner: Arc<dyn Runner>) -> Self {
        Self { runner }
    }
}

impl Default for GitBackend {
    fn default() -> Self {
        Self::new()
    }
}

/// Run a void git command (no output parsing). Used by simple mutating ops
/// that just need to bubble up the exit status. Kept `pub(super)` so the
/// submodule files can call it without re-implementing the `Cmd` builder
/// boilerplate in every function.
pub(super) fn exec(runner: &dyn Runner, args: &[&str]) -> Result<()> {
    let cwd = std::env::current_dir()?;
    runner
        .run(Cmd::new("git").in_dir(&cwd).args(args))
        .map(|_| ())
        .map_err(errmap::map_run_err)
}

impl VcsBackend for GitBackend {
    fn name(&self) -> &'static str {
        "git"
    }

    // ----- Identity -------------------------------------------------------
    fn repo_root(&self) -> Result<PathBuf> { repo::repo_root(self.runner.as_ref()) }
    fn repo_name(&self) -> Result<String> { repo::repo_name(self.runner.as_ref()) }
    fn workspace_id(&self) -> Result<String> { repo::workspace_id(self.runner.as_ref()) }
    fn current_branch(&self) -> Result<String> { repo::current_branch(self.runner.as_ref()) }
    fn current_commit(&self) -> Result<String> { repo::current_commit(self.runner.as_ref()) }
    fn detect_trunk(&self) -> Result<String> { repo::detect_trunk(self.runner.as_ref()) }

    // ----- Branches -------------------------------------------------------
    fn local_branches(&self) -> Result<Vec<String>> { repo::local_branches(self.runner.as_ref()) }
    fn branch_exists(&self, name: &str) -> Result<bool> {
        repo::branch_exists(self.runner.as_ref(), name)
    }
    fn is_merged(&self, branch: &str, target: &str) -> Result<bool> {
        branch::is_merged(self.runner.as_ref(), branch, target)
    }
    fn has_diff_from(&self, branch: &str, target: &str) -> Result<bool> {
        branch::has_diff_from(self.runner.as_ref(), branch, target)
    }
    fn delete_branch(&self, name: &str, force: bool) -> Result<()> {
        branch::delete_branch(self.runner.as_ref(), name, force)
    }
    fn rename_branch(&self, old: &str, new: &str) -> Result<()> {
        branch::rename_branch(self.runner.as_ref(), old, new)
    }
    fn log_oneline(&self, from: &str, to: &str) -> Result<String> {
        branch::log_oneline(self.runner.as_ref(), from, to)
    }
    fn commit_count(&self, from: &str, to: &str) -> Result<usize> {
        branch::commit_count(self.runner.as_ref(), from, to)
    }
    fn diff_shortstat(&self, from: &str, to: &str) -> Result<DiffStat> {
        branch::diff_shortstat(self.runner.as_ref(), from, to)
    }
    fn diff_shortstat_in(&self, path: &Path) -> Result<DiffStat> {
        branch::diff_shortstat_in(self.runner.as_ref(), path)
    }

    // ----- Working-copy state --------------------------------------------
    fn has_uncommitted_changes(&self) -> Result<bool> {
        branch::has_uncommitted_changes(self.runner.as_ref())
    }
    fn uncommitted_count_in(&self, path: &Path) -> Result<usize> {
        branch::uncommitted_count_in(self.runner.as_ref(), path)
    }
    fn has_staged_changes(&self) -> Result<bool> {
        branch::has_staged_changes(self.runner.as_ref())
    }
    fn has_changes_from_trunk(&self, trunk: &str) -> Result<bool> {
        branch::has_changes_from_trunk(self.runner.as_ref(), trunk)
    }
    fn is_rebase_in_progress(&self) -> bool {
        ops::is_rebase_in_progress(self.runner.as_ref())
    }
    fn is_merge_in_progress(&self) -> bool {
        ops::is_merge_in_progress(self.runner.as_ref())
    }

    // ----- Mutations ------------------------------------------------------
    fn merge(&self, branch: &str, squash: bool, no_ff: bool, message: Option<&str>) -> Result<()> {
        ops::merge(self.runner.as_ref(), branch, squash, no_ff, message)
    }
    fn dry_run_merge(&self, branch: &str, squash: bool) -> Result<bool> {
        ops::dry_run_merge(self.runner.as_ref(), branch, squash)
    }
    fn rebase(&self, onto: &str) -> Result<()> { ops::rebase(self.runner.as_ref(), onto) }
    fn checkout(&self, branch: &str) -> Result<()> { ops::checkout(self.runner.as_ref(), branch) }
    fn commit(&self, message: &str) -> Result<()> { ops::commit(self.runner.as_ref(), message) }
    fn fetch(&self) -> Result<()> { ops::fetch(self.runner.as_ref()) }
    fn rebase_abort(&self) -> Result<()> { ops::rebase_abort(self.runner.as_ref()) }
    fn rebase_continue(&self) -> Result<()> { ops::rebase_continue(self.runner.as_ref()) }
    fn merge_abort(&self) -> Result<()> { ops::merge_abort(self.runner.as_ref()) }
    fn merge_continue(&self) -> Result<()> { ops::merge_continue(self.runner.as_ref()) }
    fn reset_merge(&self) -> Result<()> { ops::reset_merge(self.runner.as_ref()) }

    // ----- Worktrees ------------------------------------------------------
    fn list_worktrees(&self) -> Result<Vec<WorktreeInfo>> {
        worktree::list_worktrees(self.runner.as_ref())
    }
    fn create_worktree(&self, path: &Path, branch: &str, base: &str) -> Result<()> {
        worktree::create_worktree(self.runner.as_ref(), path, branch, base)
    }
    fn remove_worktree(&self, path: &Path, force: bool) -> Result<()> {
        worktree::remove_worktree(self.runner.as_ref(), path, force)
    }
    fn move_worktree(&self, old: &Path, new: &Path) -> Result<()> {
        worktree::move_worktree(self.runner.as_ref(), old, new)
    }
}

#[cfg(test)]
mod tests;
