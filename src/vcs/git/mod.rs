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
    /// Explicit working directory for every git invocation. `None` means
    /// "read the live process cwd" — preserving the historical behaviour
    /// where helpers called `std::env::current_dir()` directly (including
    /// following any `set_current_dir` a command performs). Stage 1 wires
    /// this through every helper but keeps the constructors producing
    /// `None`, so nothing changes until a later stage sets it explicitly.
    cwd: Option<PathBuf>,
}

impl GitBackend {
    /// Default constructor — uses the real subprocess runner.
    pub fn new() -> Self {
        Self { runner: Arc::new(DefaultRunner), cwd: None }
    }

    /// Construct with a caller-supplied runner. Exposed (not gated on `cfg(test)`)
    /// because integration tests under `tests/` and downstream callers may
    /// want to substitute their own runner — e.g. to record subprocess args
    /// for debugging.
    pub fn with_runner(runner: Arc<dyn Runner>) -> Self {
        Self { runner, cwd: None }
    }

    /// Construct pinned to an explicit working directory. Unused in Stage 1
    /// (kept for the follow-up stage that stops relying on the process cwd)
    /// but exposed now so call sites can migrate without touching this file.
    #[allow(dead_code)]
    pub fn at(cwd: PathBuf) -> Self {
        Self { runner: Arc::new(DefaultRunner), cwd: Some(cwd) }
    }

    /// As [`with_runner`](Self::with_runner) but pinned to an explicit cwd.
    #[allow(dead_code)]
    pub fn with_runner_at(runner: Arc<dyn Runner>, cwd: PathBuf) -> Self {
        Self { runner, cwd: Some(cwd) }
    }

    /// Resolve the working directory for a git invocation. `Some` returns the
    /// pinned path; `None` falls back to the live process cwd, exactly as the
    /// helpers used to do via `std::env::current_dir()`.
    fn dir(&self) -> std::io::Result<PathBuf> {
        match &self.cwd {
            Some(d) => Ok(d.clone()),
            None => std::env::current_dir(),
        }
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
pub(super) fn exec(runner: &dyn Runner, cwd: &Path, args: &[&str]) -> Result<()> {
    runner
        .run(Cmd::new("git").in_dir(cwd).args(args))
        .map(|_| ())
        .map_err(errmap::map_run_err)
}

impl VcsBackend for GitBackend {
    fn name(&self) -> &'static str {
        "git"
    }

    // ----- Identity -------------------------------------------------------
    fn repo_root(&self) -> Result<PathBuf> { repo::repo_root(self.runner.as_ref(), &self.dir()?) }
    fn repo_name(&self) -> Result<String> { repo::repo_name(self.runner.as_ref(), &self.dir()?) }
    fn workspace_id(&self) -> Result<String> { repo::workspace_id(self.runner.as_ref(), &self.dir()?) }
    fn current_branch(&self) -> Result<String> { repo::current_branch(self.runner.as_ref(), &self.dir()?) }
    fn current_commit(&self) -> Result<String> { repo::current_commit(self.runner.as_ref(), &self.dir()?) }
    fn detect_trunk(&self) -> Result<String> { repo::detect_trunk(self.runner.as_ref(), &self.dir()?) }

    // ----- Branches -------------------------------------------------------
    fn local_branches(&self) -> Result<Vec<String>> { repo::local_branches(self.runner.as_ref(), &self.dir()?) }
    fn branch_exists(&self, name: &str) -> Result<bool> {
        repo::branch_exists(self.runner.as_ref(), &self.dir()?, name)
    }
    fn remote_branch_exists(&self, name: &str) -> Result<bool> {
        repo::remote_branch_exists(self.runner.as_ref(), &self.dir()?, name)
    }
    fn is_merged(&self, branch: &str, target: &str) -> Result<bool> {
        branch::is_merged(self.runner.as_ref(), &self.dir()?, branch, target)
    }
    fn has_diff_from(&self, branch: &str, target: &str) -> Result<bool> {
        branch::has_diff_from(self.runner.as_ref(), &self.dir()?, branch, target)
    }
    fn delete_branch(&self, name: &str, force: bool) -> Result<()> {
        branch::delete_branch(self.runner.as_ref(), &self.dir()?, name, force)
    }
    fn rename_branch(&self, old: &str, new: &str) -> Result<()> {
        branch::rename_branch(self.runner.as_ref(), &self.dir()?, old, new)
    }
    fn log_oneline(&self, from: &str, to: &str) -> Result<String> {
        branch::log_oneline(self.runner.as_ref(), &self.dir()?, from, to)
    }
    fn commit_count(&self, from: &str, to: &str) -> Result<usize> {
        branch::commit_count(self.runner.as_ref(), &self.dir()?, from, to)
    }
    fn diff_shortstat(&self, from: &str, to: &str) -> Result<DiffStat> {
        branch::diff_shortstat(self.runner.as_ref(), &self.dir()?, from, to)
    }
    fn diff_shortstat_in(&self, path: &Path) -> Result<DiffStat> {
        branch::diff_shortstat_in(self.runner.as_ref(), path)
    }

    // ----- Working-copy state --------------------------------------------
    fn has_uncommitted_changes(&self) -> Result<bool> {
        branch::has_uncommitted_changes(self.runner.as_ref(), &self.dir()?)
    }
    fn uncommitted_count_in(&self, path: &Path) -> Result<usize> {
        branch::uncommitted_count_in(self.runner.as_ref(), path)
    }
    fn has_changes_from_trunk(&self, trunk: &str) -> Result<bool> {
        branch::has_changes_from_trunk(self.runner.as_ref(), &self.dir()?, trunk)
    }
    fn is_rebase_in_progress(&self) -> bool {
        ops::is_rebase_in_progress(self.runner.as_ref(), self.cwd.as_deref())
    }
    fn is_merge_in_progress(&self) -> bool {
        ops::is_merge_in_progress(self.runner.as_ref(), self.cwd.as_deref())
    }

    // ----- Mutations ------------------------------------------------------
    fn merge(
        &self,
        branch: &str,
        _dest_bookmark: &str, // git merges into the checked-out HEAD; no bookmark to advance
        squash: bool,
        no_ff: bool,
        message: Option<&str>,
    ) -> Result<()> {
        ops::merge(self.runner.as_ref(), &self.dir()?, branch, squash, no_ff, message)
    }
    fn dry_run_merge(&self, branch: &str, squash: bool) -> Result<bool> {
        ops::dry_run_merge(self.runner.as_ref(), &self.dir()?, branch, squash)
    }
    fn rebase(&self, onto: &str) -> Result<()> { ops::rebase(self.runner.as_ref(), &self.dir()?, onto) }
    fn checkout(&self, branch: &str) -> Result<()> { ops::checkout(self.runner.as_ref(), &self.dir()?, branch) }
    fn commit(&self, message: &str) -> Result<()> { ops::commit(self.runner.as_ref(), &self.dir()?, message) }
    fn fetch(&self) -> Result<()> { ops::fetch(self.runner.as_ref(), &self.dir()?) }
    fn rebase_abort(&self) -> Result<()> { ops::rebase_abort(self.runner.as_ref(), &self.dir()?) }
    fn rebase_continue(&self) -> Result<()> { ops::rebase_continue(self.runner.as_ref(), &self.dir()?) }
    fn merge_abort(&self) -> Result<()> { ops::merge_abort(self.runner.as_ref(), &self.dir()?) }
    fn merge_continue(&self) -> Result<()> { ops::merge_continue(self.runner.as_ref(), &self.dir()?) }
    fn reset_merge(&self) -> Result<()> { ops::reset_merge(self.runner.as_ref(), &self.dir()?) }

    // ----- Worktrees ------------------------------------------------------
    fn list_worktrees(&self) -> Result<Vec<WorktreeInfo>> {
        worktree::list_worktrees(self.runner.as_ref(), &self.dir()?)
    }
    fn create_worktree(
        &self,
        path: &Path,
        branch: &str,
        base: &str,
    ) -> Result<crate::vcs::common::CreateOutcome> {
        worktree::create_worktree(self.runner.as_ref(), &self.dir()?, path, branch, base)
    }
    fn create_worktree_from_remote(
        &self,
        path: &Path,
        branch: &str,
    ) -> Result<crate::vcs::common::CreateOutcome> {
        worktree::create_worktree_from_remote(self.runner.as_ref(), &self.dir()?, path, branch)
    }
    fn remove_worktree(&self, path: &Path, force: bool) -> Result<()> {
        worktree::remove_worktree(self.runner.as_ref(), &self.dir()?, path, force)
    }
    fn move_worktree(&self, old: &Path, new: &Path) -> Result<()> {
        worktree::move_worktree(self.runner.as_ref(), &self.dir()?, old, new)
    }
}

#[cfg(test)]
mod tests;
