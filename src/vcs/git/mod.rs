// ===========================================================================
// vcs/git - Git backend (the only real implementation today)
// ===========================================================================
//
// `GitBackend` wraps an `Arc<vcs_git::Git<JobRunner>>` — the typed async Git
// client from the `vcs-git` crate, itself built on `processkit`'s job-backed
// process runner so a `git` subprocess is never orphaned. Operations the
// typed client models are delegated straight to it; the handful it doesn't
// (CoW worktree creation, `log --oneline`, `checkout --detach`, the index
// plumbing) drop to a raw `processkit::Command` via the `exec`/`capture`
// helpers below — both honour an explicit working directory, so nothing here
// mutates the process cwd.
//
// The actual per-method implementations live in the submodules (`repo`,
// `branch`, `worktree`, `ops`). This file is the assembly — it builds the
// trait impl by delegating each method to a submodule function that takes the
// client + cwd explicitly. That keeps the bodies testable in isolation and
// lets us swap in a `ScriptedRunner`-backed client without touching call sites.

mod errmap;
mod repo;
mod branch;
mod worktree;
mod ops;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use processkit::{Command, JobRunner, ProcessResult};
use vcs_git::Git;

use super::backend::VcsBackend;
use super::common::{DiffStat, WorktreeInfo};
use super::error::{Error, Result};

// Re-export pure-function helpers used by tests and by any code path that wants
// to parse fixture text without a backend instance.
pub use branch::parse_shortstat;
pub use errmap::clean_git_error;
pub use repo::is_cwd_inside;
pub use worktree::parse_worktree_list;

/// The concrete vcs-git client type used in production (real job-backed runner).
pub(super) type GitClient = Git<JobRunner>;

/// Git-backed [`VcsBackend`]. Holds the shared typed client plus an explicit
/// working directory for every invocation.
pub struct GitBackend {
    git: Arc<GitClient>,
    /// Explicit working directory for every git invocation. `None` means
    /// "read the live process cwd" — preserving the historical behaviour
    /// where helpers called `std::env::current_dir()` directly.
    cwd: Option<PathBuf>,
}

impl GitBackend {
    /// Default constructor — uses the real job-backed runner.
    pub fn new() -> Self {
        Self { git: Arc::new(Git::new()), cwd: None }
    }

    /// Construct with a caller-supplied client (e.g. a `ScriptedRunner`-backed
    /// one in tests). Exposed (not gated on `cfg(test)`) because integration
    /// tests under `tests/` and downstream callers may want to substitute their
    /// own runner.
    pub fn with_client(git: Arc<GitClient>) -> Self {
        Self { git, cwd: None }
    }

    /// Construct a backend anchored at an explicit `cwd` using the real runner.
    /// Test-only: lets each unit test point a `GitBackend` at its own `TempDir`
    /// without mutating the process current-directory, so the suites run in
    /// parallel.
    #[cfg(test)]
    pub(crate) fn at(cwd: PathBuf) -> Self {
        Self { git: Arc::new(Git::new()), cwd: Some(cwd) }
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

    /// The shared typed client.
    fn git(&self) -> &GitClient {
        &self.git
    }
}

impl Default for GitBackend {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Raw-command helpers (used by ops the vcs-git client doesn't model)
// ---------------------------------------------------------------------------

/// Build a `git <args>` command pinned to `cwd`.
pub(super) fn git_cmd<'a>(cwd: &Path, args: impl IntoIterator<Item = &'a str>) -> Command {
    Command::new("git").current_dir(cwd).args(args)
}

/// Run a void git command, erroring on a non-zero exit. We capture the full
/// `ProcessResult` (rather than using `Command::run`) so a failure can build
/// its message from BOTH streams — git writes `CONFLICT (content): …` to
/// stdout, which `processkit::Error::Exit` would otherwise drop.
pub(super) async fn exec<'a>(cwd: &Path, args: impl IntoIterator<Item = &'a str>) -> Result<()> {
    let out = git_cmd(cwd, args).output_string().await.map_err(errmap::map_pk_err)?;
    if out.is_success() {
        Ok(())
    } else {
        Err(Error::Command(errmap::extract_message(out.stderr(), out.stdout().as_bytes())))
    }
}

/// Capture a git command's result without erroring on a non-zero exit — for
/// exit-code-as-answer probes (`diff --quiet`, `show-ref --verify`).
pub(super) async fn capture<'a>(
    cwd: &Path,
    args: impl IntoIterator<Item = &'a str>,
) -> Result<ProcessResult<String>> {
    git_cmd(cwd, args).output_string().await.map_err(errmap::map_pk_err)
}

#[async_trait]
impl VcsBackend for GitBackend {
    fn name(&self) -> &'static str {
        "git"
    }

    fn at_cwd(&self, cwd: PathBuf) -> Box<dyn VcsBackend> {
        Box::new(Self { git: self.git.clone(), cwd: Some(cwd) })
    }

    // ----- Identity -------------------------------------------------------
    async fn repo_root(&self) -> Result<PathBuf> {
        repo::repo_root(self.git(), &self.dir()?).await
    }
    async fn repo_name(&self) -> Result<String> {
        repo::repo_name(self.git(), &self.dir()?).await
    }
    async fn workspace_id(&self) -> Result<String> {
        repo::workspace_id(self.git(), &self.dir()?).await
    }
    async fn current_branch(&self) -> Result<String> {
        repo::current_branch(self.git(), &self.dir()?).await
    }
    async fn current_commit(&self) -> Result<String> {
        repo::current_commit(self.git(), &self.dir()?).await
    }
    async fn detect_trunk(&self) -> Result<String> {
        repo::detect_trunk(self.git(), &self.dir()?).await
    }

    // ----- Branches -------------------------------------------------------
    async fn local_branches(&self) -> Result<Vec<String>> {
        repo::local_branches(self.git(), &self.dir()?).await
    }
    async fn branch_exists(&self, name: &str) -> Result<bool> {
        repo::branch_exists(self.git(), &self.dir()?, name).await
    }
    async fn remote_branch_exists(&self, name: &str) -> Result<bool> {
        repo::remote_branch_exists(self.git(), &self.dir()?, name).await
    }
    async fn is_merged(&self, branch: &str, target: &str) -> Result<bool> {
        branch::is_merged(self.git(), &self.dir()?, branch, target).await
    }
    async fn has_diff_from(&self, branch: &str, target: &str) -> Result<bool> {
        branch::has_diff_from(self.git(), &self.dir()?, branch, target).await
    }
    async fn delete_branch(&self, name: &str, force: bool) -> Result<()> {
        branch::delete_branch(self.git(), &self.dir()?, name, force).await
    }
    async fn rename_branch(&self, old: &str, new: &str) -> Result<()> {
        branch::rename_branch(self.git(), &self.dir()?, old, new).await
    }
    async fn log_oneline(&self, from: &str, to: &str) -> Result<String> {
        branch::log_oneline(&self.dir()?, from, to).await
    }
    async fn commit_count(&self, from: &str, to: &str) -> Result<usize> {
        branch::commit_count(self.git(), &self.dir()?, from, to).await
    }
    async fn diff_shortstat(&self, from: &str, to: &str) -> Result<DiffStat> {
        branch::diff_shortstat(self.git(), &self.dir()?, from, to).await
    }
    async fn diff_shortstat_in(&self, path: &Path) -> Result<DiffStat> {
        branch::diff_shortstat_in(self.git(), path).await
    }

    // ----- Working-copy state --------------------------------------------
    async fn has_uncommitted_changes(&self) -> Result<bool> {
        branch::has_uncommitted_changes(self.git(), &self.dir()?).await
    }
    async fn uncommitted_count_in(&self, path: &Path) -> Result<usize> {
        branch::uncommitted_count_in(self.git(), path).await
    }
    async fn has_changes_from_trunk(&self, trunk: &str) -> Result<bool> {
        branch::has_changes_from_trunk(self.git(), &self.dir()?, trunk).await
    }
    async fn is_rebase_in_progress(&self) -> bool {
        ops::is_rebase_in_progress(self.git(), &self.dir().unwrap_or_default()).await
    }
    async fn is_merge_in_progress(&self) -> bool {
        ops::is_merge_in_progress(self.git(), &self.dir().unwrap_or_default()).await
    }

    // ----- Mutations ------------------------------------------------------
    async fn merge(
        &self,
        branch: &str,
        _dest_bookmark: &str, // git merges into the checked-out HEAD; no bookmark to advance
        squash: bool,
        no_ff: bool,
        message: Option<&str>,
    ) -> Result<()> {
        ops::merge(&self.dir()?, branch, squash, no_ff, message).await
    }
    async fn dry_run_merge(&self, branch: &str, squash: bool) -> Result<bool> {
        ops::dry_run_merge(&self.dir()?, branch, squash).await
    }
    async fn rebase(&self, onto: &str) -> Result<()> {
        ops::rebase(&self.dir()?, onto).await
    }
    async fn checkout(&self, branch: &str) -> Result<()> {
        ops::checkout(&self.dir()?, branch).await
    }
    async fn commit(&self, message: &str) -> Result<()> {
        ops::commit(&self.dir()?, message).await
    }
    async fn fetch(&self) -> Result<()> {
        ops::fetch(&self.dir()?).await
    }
    async fn rebase_abort(&self) -> Result<()> {
        ops::rebase_abort(&self.dir()?).await
    }
    async fn rebase_continue(&self) -> Result<()> {
        ops::rebase_continue(&self.dir()?).await
    }
    async fn merge_abort(&self) -> Result<()> {
        ops::merge_abort(&self.dir()?).await
    }
    async fn merge_continue(&self) -> Result<()> {
        ops::merge_continue(&self.dir()?).await
    }
    async fn reset_merge(&self) -> Result<()> {
        ops::reset_merge(&self.dir()?).await
    }

    // ----- Worktrees ------------------------------------------------------
    async fn list_worktrees(&self) -> Result<Vec<WorktreeInfo>> {
        worktree::list_worktrees(self.git(), &self.dir()?).await
    }
    async fn create_worktree(
        &self,
        path: &Path,
        branch: &str,
        base: &str,
    ) -> Result<crate::vcs::common::CreateOutcome> {
        worktree::create_worktree(self.git(), &self.dir()?, path, branch, base).await
    }
    async fn create_worktree_from_remote(
        &self,
        path: &Path,
        branch: &str,
    ) -> Result<crate::vcs::common::CreateOutcome> {
        worktree::create_worktree_from_remote(self.git(), &self.dir()?, path, branch).await
    }
    async fn remove_worktree(&self, path: &Path, force: bool) -> Result<()> {
        worktree::remove_worktree(&self.dir()?, path, force).await
    }
    async fn move_worktree(&self, old: &Path, new: &Path) -> Result<()> {
        worktree::move_worktree(&self.dir()?, old, new).await
    }

    // ----- Synchronous cleanup (for WorktreeGuard::drop) ------------------
    fn cleanup_worktree_blocking(&self, path: &Path) -> Result<()> {
        // `Drop` can't await; run a plain blocking `git worktree remove
        // --force` via std::process here. Force is required: a half-set-up
        // worktree has its freshly-created branch checked out, which a
        // non-forced removal refuses.
        let cwd = self.dir().map_err(|e| Error::Command(e.to_string()))?;
        let path_arg = crate::vcs::common::path_str(path)?;
        let status = std::process::Command::new("git")
            .current_dir(&cwd)
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
    }
}

#[cfg(test)]
mod tests;
