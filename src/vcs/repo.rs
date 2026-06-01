// ===========================================================================
// vcs/repo - explicit Repo context
// ===========================================================================
//
// `Repo` is the explicit-cwd successor to the (now-removed) thread-local
// facade. It pins a `VcsBackend` to a specific directory (no reliance on the
// process cwd) and exposes thin wrappers over the backend methods commands
// need. It is the ONLY way the CLI reaches a backend.
//
// Wrappers here mirror the `VcsBackend` trait; new trait methods get a
// matching wrapper added when a command needs them.

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

    /// A GitHub CLI (`gh`) client, built on the same `processkit` job-backed
    /// runner as the git/jj backends. Wired and ready for future `gh`-backed
    /// features (PR creation, release management, …) — `vcs-github` is a
    /// dependency now, but no `ws` command drives it yet. Its methods take a
    /// `dir: &Path`; pass [`Repo::cwd`] for repo-scoped calls.
    pub fn github(&self) -> vcs_github::GitHub {
        vcs_github::GitHub::new()
    }

    // ----- Identity -------------------------------------------------------
    pub async fn repo_root(&self) -> Result<PathBuf> {
        self.backend.repo_root().await
    }
    pub async fn repo_name(&self) -> Result<String> {
        self.backend.repo_name().await
    }
    pub async fn workspace_id(&self) -> Result<String> {
        self.backend.workspace_id().await
    }
    pub async fn current_branch(&self) -> Result<String> {
        self.backend.current_branch().await
    }
    pub async fn current_commit(&self) -> Result<String> {
        self.backend.current_commit().await
    }
    pub async fn detect_trunk(&self) -> Result<String> {
        self.backend.detect_trunk().await
    }

    // ----- Branches -------------------------------------------------------
    pub async fn local_branches(&self) -> Result<Vec<String>> {
        self.backend.local_branches().await
    }
    pub async fn branch_exists(&self, name: &str) -> Result<bool> {
        self.backend.branch_exists(name).await
    }
    pub async fn remote_branch_exists(&self, name: &str) -> Result<bool> {
        self.backend.remote_branch_exists(name).await
    }
    pub async fn has_diff_from(&self, branch: &str, target: &str) -> Result<bool> {
        self.backend.has_diff_from(branch, target).await
    }
    pub async fn delete_branch(&self, name: &str, force: bool) -> Result<()> {
        self.backend.delete_branch(name, force).await
    }
    pub async fn rename_branch(&self, old: &str, new: &str) -> Result<()> {
        self.backend.rename_branch(old, new).await
    }
    pub async fn log_oneline(&self, from: &str, to: &str) -> Result<String> {
        self.backend.log_oneline(from, to).await
    }
    pub async fn commit_count(&self, from: &str, to: &str) -> Result<usize> {
        self.backend.commit_count(from, to).await
    }
    pub async fn diff_shortstat(&self, from: &str, to: &str) -> Result<DiffStat> {
        self.backend.diff_shortstat(from, to).await
    }
    pub async fn diff_shortstat_in(&self, path: &Path) -> Result<DiffStat> {
        self.backend.diff_shortstat_in(path).await
    }

    // ----- Working-copy state --------------------------------------------
    pub async fn has_uncommitted_changes(&self) -> Result<bool> {
        self.backend.has_uncommitted_changes().await
    }
    pub async fn uncommitted_count_in(&self, path: &Path) -> Result<usize> {
        self.backend.uncommitted_count_in(path).await
    }
    pub async fn is_rebase_in_progress(&self) -> bool {
        self.backend.is_rebase_in_progress().await
    }
    pub async fn is_merge_in_progress(&self) -> bool {
        self.backend.is_merge_in_progress().await
    }

    // ----- Mutations ------------------------------------------------------
    pub async fn merge(
        &self,
        branch: &str,
        dest_bookmark: &str,
        squash: bool,
        no_ff: bool,
        message: Option<&str>,
    ) -> Result<()> {
        self.backend.merge(branch, dest_bookmark, squash, no_ff, message).await
    }
    pub async fn dry_run_merge(&self, branch: &str, squash: bool) -> Result<bool> {
        self.backend.dry_run_merge(branch, squash).await
    }
    pub async fn checkout(&self, branch: &str) -> Result<()> {
        self.backend.checkout(branch).await
    }
    pub async fn reset_merge(&self) -> Result<()> {
        self.backend.reset_merge().await
    }
    pub async fn rebase(&self, onto: &str) -> Result<()> {
        self.backend.rebase(onto).await
    }
    pub async fn rebase_abort(&self) -> Result<()> {
        self.backend.rebase_abort().await
    }
    pub async fn rebase_continue(&self) -> Result<()> {
        self.backend.rebase_continue().await
    }
    pub async fn merge_abort(&self) -> Result<()> {
        self.backend.merge_abort().await
    }
    pub async fn merge_continue(&self) -> Result<()> {
        self.backend.merge_continue().await
    }

    // ----- Worktrees ------------------------------------------------------
    pub async fn list_worktrees(&self) -> Result<Vec<WorktreeInfo>> {
        self.backend.list_worktrees().await
    }
    pub async fn create_worktree(
        &self,
        path: &Path,
        branch: &str,
        base: &str,
    ) -> Result<CreateOutcome> {
        self.backend.create_worktree(path, branch, base).await
    }
    pub async fn create_worktree_from_remote(
        &self,
        path: &Path,
        branch: &str,
    ) -> Result<CreateOutcome> {
        self.backend.create_worktree_from_remote(path, branch).await
    }
    pub async fn remove_worktree(&self, path: &Path, force: bool) -> Result<()> {
        self.backend.remove_worktree(path, force).await
    }
    /// Synchronously force-remove a partial worktree — used by
    /// [`WorktreeGuard`](super::guard::WorktreeGuard)'s `Drop`, which can't
    /// `.await`. Delegates to the backend's blocking cleanup.
    pub fn cleanup_worktree_blocking(&self, path: &Path) -> Result<()> {
        self.backend.cleanup_worktree_blocking(path)
    }
    /// Arm a [`WorktreeGuard`](super::guard::WorktreeGuard) over `path`: it
    /// force-removes the worktree on drop until `keep()` is called. Use it to
    /// make the partial-failure window of worktree creation panic- and
    /// early-return-safe.
    pub fn guard_worktree(&self, path: impl Into<PathBuf>) -> super::guard::WorktreeGuard<'_> {
        super::guard::WorktreeGuard::new(self, path)
    }
    pub async fn move_worktree(&self, old: &Path, new: &Path) -> Result<()> {
        self.backend.move_worktree(old, new).await
    }
}
