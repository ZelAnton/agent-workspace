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

use vcs_core::{BackendKind, Repo as CoreRepo};

use super::common::{CreateOutcome, DiffStat, WorktreeInfo};
use super::error::{Error, Result};
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

    // ----- Identity -------------------------------------------------------
    pub async fn repo_root(&self) -> Result<PathBuf> {
        let cwd = self.inner.cwd();
        if self.is_git() {
            git::repo::repo_root(self.git(), cwd).await
        } else {
            jj::repo::repo_root(self.jj(), cwd).await
        }
    }
    pub async fn repo_name(&self) -> Result<String> {
        let cwd = self.inner.cwd();
        if self.is_git() {
            git::repo::repo_name(self.git(), cwd).await
        } else {
            jj::repo::repo_name(self.jj(), cwd).await
        }
    }
    pub async fn workspace_id(&self) -> Result<String> {
        let cwd = self.inner.cwd();
        if self.is_git() {
            git::repo::workspace_id(self.git(), cwd).await
        } else {
            jj::repo::workspace_id(self.jj(), cwd).await
        }
    }
    pub async fn current_branch(&self) -> Result<String> {
        let cwd = self.inner.cwd();
        if self.is_git() {
            git::repo::current_branch(self.git(), cwd).await
        } else {
            jj::repo::current_branch(self.jj(), cwd).await
        }
    }
    pub async fn current_commit(&self) -> Result<String> {
        let cwd = self.inner.cwd();
        if self.is_git() {
            git::repo::current_commit(self.git(), cwd).await
        } else {
            jj::repo::current_commit(self.jj(), cwd).await
        }
    }
    pub async fn detect_trunk(&self) -> Result<String> {
        let cwd = self.inner.cwd();
        if self.is_git() {
            git::repo::detect_trunk(self.git(), cwd).await
        } else {
            jj::repo::detect_trunk(self.jj(), cwd).await
        }
    }

    // ----- Branches -------------------------------------------------------
    pub async fn local_branches(&self) -> Result<Vec<String>> {
        let cwd = self.inner.cwd();
        if self.is_git() {
            git::repo::local_branches(self.git(), cwd).await
        } else {
            jj::repo::local_branches(self.jj(), cwd).await
        }
    }
    pub async fn branch_exists(&self, name: &str) -> Result<bool> {
        let cwd = self.inner.cwd();
        if self.is_git() {
            git::repo::branch_exists(self.git(), cwd, name).await
        } else {
            jj::repo::branch_exists(self.jj(), cwd, name).await
        }
    }
    pub async fn remote_branch_exists(&self, name: &str) -> Result<bool> {
        let cwd = self.inner.cwd();
        if self.is_git() {
            git::repo::remote_branch_exists(self.git(), cwd, name).await
        } else {
            jj::repo::remote_branch_exists(self.jj(), cwd, name).await
        }
    }
    pub async fn has_diff_from(&self, branch: &str, target: &str) -> Result<bool> {
        let cwd = self.inner.cwd();
        if self.is_git() {
            git::branch::has_diff_from(self.git(), cwd, branch, target).await
        } else {
            jj::branch::has_diff_from(self.jj(), cwd, branch, target).await
        }
    }
    pub async fn delete_branch(&self, name: &str, force: bool) -> Result<()> {
        let cwd = self.inner.cwd();
        if self.is_git() {
            git::branch::delete_branch(self.git(), cwd, name, force).await
        } else {
            jj::repo::delete_branch(self.jj(), cwd, name, force).await
        }
    }
    pub async fn rename_branch(&self, old: &str, new: &str) -> Result<()> {
        let cwd = self.inner.cwd();
        if self.is_git() {
            git::branch::rename_branch(self.git(), cwd, old, new).await
        } else {
            jj::repo::rename_branch(self.jj(), cwd, old, new).await
        }
    }
    pub async fn log_oneline(&self, from: &str, to: &str) -> Result<String> {
        let cwd = self.inner.cwd();
        if self.is_git() {
            git::branch::log_oneline(cwd, from, to).await
        } else {
            jj::branch::log_oneline(self.jj(), cwd, from, to).await
        }
    }
    pub async fn commit_count(&self, from: &str, to: &str) -> Result<usize> {
        let cwd = self.inner.cwd();
        if self.is_git() {
            git::branch::commit_count(self.git(), cwd, from, to).await
        } else {
            jj::branch::commit_count(self.jj(), cwd, from, to).await
        }
    }
    pub async fn diff_shortstat(&self, from: &str, to: &str) -> Result<DiffStat> {
        let cwd = self.inner.cwd();
        if self.is_git() {
            git::branch::diff_shortstat(self.git(), cwd, from, to).await
        } else {
            jj::branch::diff_shortstat(self.jj(), cwd, from, to).await
        }
    }
    pub async fn diff_shortstat_in(&self, path: &Path) -> Result<DiffStat> {
        if self.is_git() {
            git::branch::diff_shortstat_in(self.git(), path).await
        } else {
            jj::branch::diff_shortstat_in(self.jj(), path).await
        }
    }

    // ----- Working-copy state --------------------------------------------
    pub async fn has_uncommitted_changes(&self) -> Result<bool> {
        let cwd = self.inner.cwd();
        if self.is_git() {
            git::branch::has_uncommitted_changes(self.git(), cwd).await
        } else {
            jj::branch::has_uncommitted_changes(self.jj(), cwd).await
        }
    }
    pub async fn uncommitted_count_in(&self, path: &Path) -> Result<usize> {
        if self.is_git() {
            git::branch::uncommitted_count_in(self.git(), path).await
        } else {
            jj::branch::uncommitted_count_in(self.jj(), path).await
        }
    }
    pub async fn is_rebase_in_progress(&self) -> bool {
        let cwd = self.inner.cwd();
        if self.is_git() {
            git::ops::is_rebase_in_progress(self.git(), cwd).await
        } else {
            // jj operations are atomic — there is no in-progress rebase state.
            false
        }
    }
    pub async fn is_merge_in_progress(&self) -> bool {
        let cwd = self.inner.cwd();
        if self.is_git() {
            git::ops::is_merge_in_progress(self.git(), cwd).await
        } else {
            jj::branch::is_merge_in_progress(self.jj(), cwd).await
        }
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
    /// `.await`. Dispatches to a blocking subprocess per backend.
    pub fn cleanup_worktree_blocking(&self, path: &Path) -> Result<()> {
        let cwd = self.inner.cwd();
        if self.is_git() {
            // Force is required: a half-set-up worktree has its freshly-created
            // branch checked out, which a non-forced removal refuses.
            let path_arg = crate::vcs::common::path_str(path)?;
            let status = std::process::Command::new("git")
                .current_dir(cwd)
                .args(["worktree", "remove", "--force", path_arg])
                .status()
                .map_err(|e| Error::Command(format!("spawn git worktree remove: {e}")))?;
            if status.success() {
                Ok(())
            } else {
                Err(Error::Command(format!(
                    "git worktree remove exited with {}",
                    status.code().unwrap_or(-1)
                )))
            }
        } else {
            // Delete the on-disk workspace dir + forget it from jj. Resolve the
            // workspace name BEFORE removing the dir (the lookup needs the live
            // `jj workspace root`). Forget only when positively identified —
            // never guess a name (could forget an unrelated workspace).
            let ws_name = jj::worktree::workspace_name_for_path_blocking(cwd, path);
            if path.exists() {
                std::fs::remove_dir_all(path).map_err(|e| Error::Command(e.to_string()))?;
            }
            if let Some(ws_name) = ws_name {
                let _ = std::process::Command::new("jj")
                    .current_dir(cwd)
                    .args(["workspace", "forget", &ws_name])
                    .status();
            }
            Ok(())
        }
    }

    /// Arm a [`WorktreeGuard`](super::guard::WorktreeGuard) over `path`: it
    /// force-removes the worktree on drop until `keep()` is called. Makes the
    /// partial-failure window of worktree creation panic- and early-return-safe.
    pub fn guard_worktree(&self, path: impl Into<PathBuf>) -> super::guard::WorktreeGuard<'_> {
        super::guard::WorktreeGuard::new(self, path)
    }
}
