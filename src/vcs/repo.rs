// ===========================================================================
// vcs/repo - the ws-facing Repo, built on the vcs-core facade
// ===========================================================================
//
// `Repo` is the single handle every command reaches a backend through. It wraps
// a [`vcs_core::Repo`] (which owns the detected `vcs_git::Git` / `vcs_jj::Jj`
// client + the bound cwd) and dispatches each operation, by backend kind, to
// `ws`'s git/jj helper functions via the `inner.git()` / `inner.jj()` escape
// hatches. Those helpers carry all of `ws`'s policy (error swallowing, the jj
// "no bookmark on @" error, the detect-trunk fallback chain, CoW worktree
// creation, the merge/rebase state machines), so the wrapper is a thin,
// behaviour-preserving dispatch layer — no semantic mapping happens here.
//
// The common detect-and-dispatch boilerplate (the old `VcsBackend` trait +
// `BackendState` + `Box<dyn>`) collapses into `vcs_core::Repo`; only the
// per-call routing lives here.

use std::path::{Path, PathBuf};

use vcs_core::{BackendKind, OperationState, Repo as CoreRepo};

use super::common::{CreateOutcome, DiffStat, WorktreeInfo};
use super::error::Result;
use super::{git, jj};

/// A VCS repository handle pinned to an explicit working directory.
pub struct Repo {
    inner: CoreRepo,
}

impl Repo {
    /// Wrap an already-built [`vcs_core::Repo`]. Built once at the CLI edge by
    /// [`crate::vcs::resolve_backend`].
    pub(crate) fn from_core(inner: CoreRepo) -> Self {
        Repo { inner }
    }

    /// A git-backed handle anchored at `path`, using the real runner. Test-only:
    /// lets unit tests point a `Repo` at a fresh temp repo without process-cwd
    /// mutation, so the suites run in parallel.
    #[cfg(test)]
    pub(crate) fn git_at(path: &Path) -> Self {
        Repo { inner: CoreRepo::from_git(path, path, vcs_git::Git::new()) }
    }

    /// Re-anchor at another directory (e.g. the main repo from a worktree),
    /// sharing the same underlying client.
    pub fn at(&self, dir: impl Into<PathBuf>) -> Repo {
        Repo { inner: self.inner.at(dir) }
    }

    /// The directory this Repo is anchored at.
    pub fn cwd(&self) -> &Path {
        self.inner.cwd()
    }

    /// Short identifier of the underlying backend — `"git"` or `"jj"`.
    pub fn backend_name(&self) -> &'static str {
        self.inner.kind().as_str()
    }

    /// A GitHub CLI (`gh`) client, built on the same `processkit` job-backed
    /// runner as the git/jj backends. Wired and ready for future `gh`-backed
    /// features; its methods take a `dir: &Path` — pass [`Repo::cwd`].
    pub fn github(&self) -> vcs_github::GitHub {
        vcs_github::GitHub::new()
    }

    // -- escape-hatch accessors (the wrapper's internal dispatch helpers) -----

    fn is_git(&self) -> bool {
        matches!(self.inner.kind(), BackendKind::Git)
    }
    /// The git client. Only call from a git arm — panics on a jj handle.
    fn git(&self) -> &git::GitClient {
        self.inner.git().expect("git backend resolved to a git client")
    }
    /// The jj client. Only call from a jj arm — panics on a git handle.
    fn jj(&self) -> &jj::JjClient {
        self.inner.jj().expect("jj backend resolved to a jj client")
    }
    /// A `cwd`-bound git view (the toolkit `GitAt`), anchored at this Repo's cwd.
    /// Only call from a git arm — panics on a jj handle.
    fn git_view(&self) -> vcs_git::GitAt<'_> {
        self.inner.git_at().expect("git backend resolved to a git client")
    }
    /// A `cwd`-bound jj view (the toolkit `JjAt`), anchored at this Repo's cwd.
    /// Only call from a jj arm — panics on a git handle.
    fn jj_view(&self) -> vcs_jj::JjAt<'_> {
        self.inner.jj_at().expect("jj backend resolved to a jj client")
    }

    // ----- Identity -------------------------------------------------------
    pub async fn repo_root(&self) -> Result<PathBuf> {
        if self.is_git() {
            git::repo::repo_root(self.git(), self.inner.cwd()).await
        } else {
            jj::repo::repo_root(self.jj_view()).await
        }
    }
    pub async fn repo_name(&self) -> Result<String> {
        if self.is_git() {
            git::repo::repo_name(self.git(), self.inner.cwd()).await
        } else {
            jj::repo::repo_name(self.jj_view()).await
        }
    }
    pub async fn workspace_id(&self) -> Result<String> {
        if self.is_git() {
            git::repo::workspace_id(self.git(), self.inner.cwd()).await
        } else {
            jj::repo::workspace_id(self.jj_view()).await
        }
    }
    pub async fn current_branch(&self) -> Result<String> {
        if self.is_git() {
            git::repo::current_branch(self.git_view()).await
        } else {
            jj::repo::current_branch(self.jj_view()).await
        }
    }
    pub async fn current_commit(&self) -> Result<String> {
        if self.is_git() {
            git::repo::current_commit(self.git_view()).await
        } else {
            jj::repo::current_commit(self.jj_view()).await
        }
    }
    /// Detect the trunk branch/bookmark. The facade resolves the backend-native
    /// default (git `origin/HEAD`, jj `trunk()`) then falls back `main` →
    /// `master`; we keep `ws`'s non-optional contract by defaulting to `"main"`
    /// when nothing resolves.
    pub async fn detect_trunk(&self) -> Result<String> {
        Ok(self.inner.trunk().await?.unwrap_or_else(|| "main".to_string()))
    }

    // ----- Branches -------------------------------------------------------
    pub async fn local_branches(&self) -> Result<Vec<String>> {
        if self.is_git() {
            git::repo::local_branches(self.git_view()).await
        } else {
            jj::repo::local_branches(self.jj_view()).await
        }
    }
    pub async fn branch_exists(&self, name: &str) -> Result<bool> {
        if self.is_git() {
            git::repo::branch_exists(self.git_view(), name).await
        } else {
            jj::repo::branch_exists(self.jj_view(), name).await
        }
    }
    pub async fn remote_branch_exists(&self, name: &str) -> Result<bool> {
        if self.is_git() {
            git::repo::remote_branch_exists(self.git_view(), name).await
        } else {
            // jj keeps the client+cwd form: it shells out to a git client at cwd
            // for the colocated `ls-remote` probe.
            jj::repo::remote_branch_exists(self.jj(), self.inner.cwd(), name).await
        }
    }
    pub async fn has_diff_from(&self, branch: &str, target: &str) -> Result<bool> {
        if self.is_git() {
            git::branch::has_diff_from(self.git_view(), branch, target).await
        } else {
            jj::branch::has_diff_from(self.jj_view(), branch, target).await
        }
    }
    pub async fn delete_branch(&self, name: &str, force: bool) -> Result<()> {
        if self.is_git() {
            git::branch::delete_branch(self.git_view(), name, force).await
        } else {
            jj::repo::delete_branch(self.jj_view(), name, force).await
        }
    }
    pub async fn rename_branch(&self, old: &str, new: &str) -> Result<()> {
        if self.is_git() {
            git::branch::rename_branch(self.git_view(), old, new).await
        } else {
            jj::repo::rename_branch(self.jj_view(), old, new).await
        }
    }
    pub async fn log_oneline(&self, from: &str, to: &str) -> Result<String> {
        if self.is_git() {
            // git log_oneline runs a raw command, so it still takes cwd.
            git::branch::log_oneline(self.inner.cwd(), from, to).await
        } else {
            jj::branch::log_oneline(self.jj_view(), from, to).await
        }
    }
    pub async fn commit_count(&self, from: &str, to: &str) -> Result<usize> {
        if self.is_git() {
            git::branch::commit_count(self.git_view(), from, to).await
        } else {
            jj::branch::commit_count(self.jj_view(), from, to).await
        }
    }
    pub async fn diff_shortstat(&self, from: &str, to: &str) -> Result<DiffStat> {
        if self.is_git() {
            git::branch::diff_shortstat(self.git_view(), from, to).await
        } else {
            jj::branch::diff_shortstat(self.jj_view(), from, to).await
        }
    }
    pub async fn diff_shortstat_in(&self, path: &Path) -> Result<DiffStat> {
        let wt = self.inner.at(path);
        if self.is_git() {
            git::branch::diff_shortstat_in(wt.git_at().expect("git backend")).await
        } else {
            jj::branch::diff_shortstat_in(wt.jj_at().expect("jj backend")).await
        }
    }

    // ----- Working-copy state --------------------------------------------
    pub async fn has_uncommitted_changes(&self) -> Result<bool> {
        if self.is_git() {
            git::branch::has_uncommitted_changes(self.git_view()).await
        } else {
            jj::branch::has_uncommitted_changes(self.jj_view()).await
        }
    }
    pub async fn uncommitted_count_in(&self, path: &Path) -> Result<usize> {
        let wt = self.inner.at(path);
        if self.is_git() {
            git::branch::uncommitted_count_in(wt.git_at().expect("git backend")).await
        } else {
            jj::branch::uncommitted_count_in(wt.jj_at().expect("jj backend")).await
        }
    }
    /// Backend-agnostic in-progress state, via the facade. Best-effort: a probe
    /// failure (e.g. outside a repo) reads as [`OperationState::Clear`].
    async fn in_progress_state(&self) -> OperationState {
        self.inner.in_progress_state().await.unwrap_or(OperationState::Clear)
    }
    pub async fn is_rebase_in_progress(&self) -> bool {
        // git reports `Rebase`; jj is atomic and never has an in-progress rebase.
        matches!(self.in_progress_state().await, OperationState::Rebase)
    }
    pub async fn is_merge_in_progress(&self) -> bool {
        // git reports `Merge` (MERGE_HEAD present); jj records the conflict on the
        // change and surfaces it as `Conflict` — both mean "resolve, then continue".
        matches!(self.in_progress_state().await, OperationState::Merge | OperationState::Conflict)
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
        let cwd = self.inner.cwd();
        if self.is_git() {
            // git merges into the checked-out HEAD; no bookmark to advance.
            git::ops::merge(cwd, branch, squash, no_ff, message).await
        } else {
            jj::ops::merge(self.jj(), cwd, branch, dest_bookmark, squash, no_ff, message).await
        }
    }
    pub async fn dry_run_merge(&self, branch: &str, squash: bool) -> Result<bool> {
        let cwd = self.inner.cwd();
        if self.is_git() {
            git::ops::dry_run_merge(cwd, branch, squash).await
        } else {
            jj::ops::dry_run_merge(self.jj(), cwd, branch, squash).await
        }
    }
    pub async fn checkout(&self, branch: &str) -> Result<()> {
        let cwd = self.inner.cwd();
        if self.is_git() {
            git::ops::checkout(cwd, branch).await
        } else {
            jj::ops::checkout(self.jj(), cwd, branch).await
        }
    }
    pub async fn reset_merge(&self) -> Result<()> {
        let cwd = self.inner.cwd();
        if self.is_git() {
            git::ops::reset_merge(cwd).await
        } else {
            jj::ops::reset_merge(cwd).await
        }
    }
    pub async fn rebase(&self, onto: &str) -> Result<()> {
        let cwd = self.inner.cwd();
        if self.is_git() {
            git::ops::rebase(cwd, onto).await
        } else {
            jj::ops::rebase(cwd, onto).await
        }
    }
    pub async fn rebase_abort(&self) -> Result<()> {
        if self.is_git() {
            git::ops::rebase_abort(self.inner.cwd()).await
        } else {
            Err(jj::unsupported(
                "rebase_abort",
                "jj records conflicts in commits; resolve files and re-run",
            ))
        }
    }
    pub async fn rebase_continue(&self) -> Result<()> {
        if self.is_git() {
            git::ops::rebase_continue(self.inner.cwd()).await
        } else {
            Err(jj::unsupported(
                "rebase_continue",
                "jj records conflicts in commits; resolve files and re-run",
            ))
        }
    }
    pub async fn merge_abort(&self) -> Result<()> {
        if self.is_git() {
            git::ops::merge_abort(self.inner.cwd()).await
        } else {
            Err(jj::unsupported(
                "merge_abort",
                "jj records conflicts in commits; resolve files and re-run",
            ))
        }
    }
    pub async fn merge_continue(&self) -> Result<()> {
        if self.is_git() {
            git::ops::merge_continue(self.inner.cwd()).await
        } else {
            Err(jj::unsupported(
                "merge_continue",
                "jj records conflicts in commits; resolve files and re-run",
            ))
        }
    }

    // ----- Worktrees ------------------------------------------------------
    pub async fn list_worktrees(&self) -> Result<Vec<WorktreeInfo>> {
        let cwd = self.inner.cwd();
        if self.is_git() {
            git::worktree::list_worktrees(self.git(), cwd).await
        } else {
            jj::worktree::list_worktrees(self.jj(), cwd).await
        }
    }
    pub async fn create_worktree(
        &self,
        path: &Path,
        branch: &str,
        base: &str,
    ) -> Result<CreateOutcome> {
        let cwd = self.inner.cwd();
        if self.is_git() {
            git::worktree::create_worktree(self.git(), cwd, path, branch, base).await
        } else {
            jj::worktree::create_worktree(self.jj(), cwd, path, branch, base).await
        }
    }
    pub async fn create_worktree_from_remote(
        &self,
        path: &Path,
        branch: &str,
    ) -> Result<CreateOutcome> {
        let cwd = self.inner.cwd();
        if self.is_git() {
            git::worktree::create_worktree_from_remote(self.git(), cwd, path, branch).await
        } else {
            jj::worktree::create_worktree_from_remote(self.jj(), cwd, path, branch).await
        }
    }
    pub async fn remove_worktree(&self, path: &Path, force: bool) -> Result<()> {
        let cwd = self.inner.cwd();
        if self.is_git() {
            git::worktree::remove_worktree(cwd, path, force).await
        } else {
            jj::worktree::remove_worktree(self.jj(), cwd, path, force).await
        }
    }
    pub async fn move_worktree(&self, old: &Path, new: &Path) -> Result<()> {
        if self.is_git() {
            git::worktree::move_worktree(self.inner.cwd(), old, new).await
        } else {
            Err(jj::unsupported(
                "move_worktree",
                "remove and re-create the workspace",
            ))
        }
    }

    /// Synchronously force-remove a partial worktree — used by
    /// [`WorktreeGuard`](super::guard::WorktreeGuard)'s `Drop`, which can't
    /// `.await`. Delegates to the facade's blocking cleanup, which per backend
    /// does exactly what `ws` needs: git `worktree remove --force` (a half-set-up
    /// worktree has its new branch checked out, so a non-forced removal would
    /// refuse); jj resolve-the-workspace-name-by-path → remove the dir → `workspace
    /// forget`, and a no-match path is a safe no-op (never guesses a name).
    pub fn cleanup_worktree_blocking(&self, path: &Path) -> Result<()> {
        Ok(self.inner.cleanup_worktree_blocking(path)?)
    }

    /// Arm a [`WorktreeGuard`](super::guard::WorktreeGuard) over `path`: it
    /// force-removes the worktree on drop until `keep()` is called. Makes the
    /// partial-failure window of worktree creation panic- and early-return-safe.
    pub fn guard_worktree(&self, path: impl Into<PathBuf>) -> super::guard::WorktreeGuard<'_> {
        super::guard::WorktreeGuard::new(self, path)
    }
}
