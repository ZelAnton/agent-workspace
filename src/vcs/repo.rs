// ===========================================================================
// vcs/repo - explicit Repo context
// ===========================================================================
//
// `Repo` is the explicit-cwd successor to the thread-local facade. It pins a
// `VcsBackend` to a specific directory (no reliance on the process cwd) and
// exposes thin wrappers over the backend methods the migrated, non-steering
// commands need.
//
// The thread-local facade (`vcs::repo_root()` etc.) still exists for the
// cwd-STEERING commands (merge/rm/clean/move/snap) that call
// `std::env::set_current_dir`; those are out of scope for this migration.
// Wrappers here are added lazily — only the methods used by the migrated
// commands are covered today; the rest can be added as more commands move
// off the facade.

use std::path::{Path, PathBuf};

use super::backend::VcsBackend;
use super::common::{CreateOutcome, DiffStat, WorktreeInfo};
use super::error::Result;

/// A VCS repository pinned to an explicit working directory.
///
/// Built once at the CLI edge from a freshly-resolved backend; passed by
/// reference into the commands that have moved off the thread-local facade.
pub struct Repo {
    backend: Box<dyn VcsBackend>,
    cwd: PathBuf,
}

impl Repo {
    /// Build a Repo anchored at `cwd` from a freshly-resolved backend.
    pub fn discover(cwd: impl Into<PathBuf>, backend: Box<dyn VcsBackend>) -> Self {
        let cwd = cwd.into();
        Repo { backend: backend.at_cwd(cwd.clone()), cwd }
    }

    /// Re-anchor at another directory (e.g. the main repo from a worktree).
    pub fn at(&self, dir: impl Into<PathBuf>) -> Repo {
        let dir = dir.into();
        Repo { backend: self.backend.at_cwd(dir.clone()), cwd: dir }
    }

    /// The directory this Repo is anchored at.
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// Short identifier of the underlying backend — `"git"` or `"jj"`.
    pub fn backend_name(&self) -> &'static str {
        self.backend.name()
    }

    // ----- Identity -------------------------------------------------------
    pub fn repo_root(&self) -> Result<PathBuf> {
        self.backend.repo_root()
    }
    pub fn repo_name(&self) -> Result<String> {
        self.backend.repo_name()
    }
    pub fn workspace_id(&self) -> Result<String> {
        self.backend.workspace_id()
    }
    pub fn current_branch(&self) -> Result<String> {
        self.backend.current_branch()
    }

    // ----- Branches -------------------------------------------------------
    pub fn local_branches(&self) -> Result<Vec<String>> {
        self.backend.local_branches()
    }
    pub fn branch_exists(&self, name: &str) -> Result<bool> {
        self.backend.branch_exists(name)
    }
    pub fn remote_branch_exists(&self, name: &str) -> Result<bool> {
        self.backend.remote_branch_exists(name)
    }
    pub fn commit_count(&self, from: &str, to: &str) -> Result<usize> {
        self.backend.commit_count(from, to)
    }
    pub fn diff_shortstat(&self, from: &str, to: &str) -> Result<DiffStat> {
        self.backend.diff_shortstat(from, to)
    }
    pub fn diff_shortstat_in(&self, path: &Path) -> Result<DiffStat> {
        self.backend.diff_shortstat_in(path)
    }

    // ----- Working-copy state --------------------------------------------
    pub fn uncommitted_count_in(&self, path: &Path) -> Result<usize> {
        self.backend.uncommitted_count_in(path)
    }
    pub fn is_rebase_in_progress(&self) -> bool {
        self.backend.is_rebase_in_progress()
    }
    pub fn is_merge_in_progress(&self) -> bool {
        self.backend.is_merge_in_progress()
    }

    // ----- Mutations ------------------------------------------------------
    pub fn merge(
        &self,
        branch: &str,
        dest_bookmark: &str,
        squash: bool,
        no_ff: bool,
        message: Option<&str>,
    ) -> Result<()> {
        self.backend.merge(branch, dest_bookmark, squash, no_ff, message)
    }
    pub fn rebase(&self, onto: &str) -> Result<()> {
        self.backend.rebase(onto)
    }
    pub fn rebase_abort(&self) -> Result<()> {
        self.backend.rebase_abort()
    }
    pub fn rebase_continue(&self) -> Result<()> {
        self.backend.rebase_continue()
    }
    pub fn merge_abort(&self) -> Result<()> {
        self.backend.merge_abort()
    }
    pub fn merge_continue(&self) -> Result<()> {
        self.backend.merge_continue()
    }

    // ----- Worktrees ------------------------------------------------------
    pub fn list_worktrees(&self) -> Result<Vec<WorktreeInfo>> {
        self.backend.list_worktrees()
    }
    pub fn create_worktree(
        &self,
        path: &Path,
        branch: &str,
        base: &str,
    ) -> Result<CreateOutcome> {
        self.backend.create_worktree(path, branch, base)
    }
    pub fn create_worktree_from_remote(
        &self,
        path: &Path,
        branch: &str,
    ) -> Result<CreateOutcome> {
        self.backend.create_worktree_from_remote(path, branch)
    }
}
