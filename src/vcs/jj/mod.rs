// ===========================================================================
// vcs/jj - Jujutsu backend
// ===========================================================================
//
// `JjBackend` wraps an `Arc<vcs_jj::Jj<JobRunner>>` — the typed async jj client
// from the `vcs-jj` crate, built on `processkit`'s job-backed runner. Mirrors
// the structure of `src/vcs/git/`: per-method implementations live in the
// submodules (`repo`, `branch`, `ops`, `worktree`); this file is the assembly.
//
// Operations the typed client models are delegated to it; the few it doesn't
// (the CoW workspace dance — `workspace add --sparse-patterns empty`,
// `sparse set`) drop to a raw `processkit::Command` via `exec`/`capture`.

mod branch;
mod errmap;
mod ops;
mod repo;
mod worktree;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use processkit::{Command, JobRunner};
use vcs_jj::Jj;

use super::backend::VcsBackend;
use super::common::{DiffStat, WorktreeInfo};
use super::error::{Error, Result};

/// The concrete vcs-jj client type used in production (real job-backed runner).
pub(super) type JjClient = Jj<JobRunner>;

/// jj-backed [`VcsBackend`].
pub struct JjBackend {
    jj: Arc<JjClient>,
    cwd: Option<PathBuf>,
}

impl JjBackend {
    pub fn new() -> Self {
        Self { jj: Arc::new(Jj::new()), cwd: None }
    }

    pub fn with_client(jj: Arc<JjClient>) -> Self {
        Self { jj, cwd: None }
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn at(cwd: PathBuf) -> Self {
        Self { jj: Arc::new(Jj::new()), cwd: Some(cwd) }
    }

    fn dir(&self) -> std::io::Result<PathBuf> {
        match &self.cwd {
            Some(d) => Ok(d.clone()),
            None => std::env::current_dir(),
        }
    }

    fn jj(&self) -> &JjClient {
        &self.jj
    }
}

impl Default for JjBackend {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Raw-command helpers (used by ops the vcs-jj client doesn't model)
// ---------------------------------------------------------------------------

/// Build a `jj <args>` command pinned to `cwd`.
pub(super) fn jj_cmd<'a>(cwd: &Path, args: impl IntoIterator<Item = &'a str>) -> Command {
    Command::new("jj").current_dir(cwd).args(args)
}

/// Run a void jj command, erroring on a non-zero exit. Captures both streams so
/// the message survives (jj writes some informational text to stdout).
pub(super) async fn exec<'a>(cwd: &Path, args: impl IntoIterator<Item = &'a str>) -> Result<()> {
    let out = jj_cmd(cwd, args).output_string().await.map_err(errmap::map_pk_err)?;
    if out.is_success() {
        Ok(())
    } else {
        Err(Error::Command(errmap::extract_message(out.stderr(), out.stdout().as_bytes())))
    }
}

#[async_trait]
impl VcsBackend for JjBackend {
    fn name(&self) -> &'static str {
        "jj"
    }

    fn at_cwd(&self, cwd: PathBuf) -> Box<dyn VcsBackend> {
        Box::new(Self { jj: self.jj.clone(), cwd: Some(cwd) })
    }

    // ----- Identity -------------------------------------------------------
    async fn repo_root(&self) -> Result<PathBuf> {
        repo::repo_root(self.jj(), &self.dir()?).await
    }
    async fn repo_name(&self) -> Result<String> {
        repo::repo_name(self.jj(), &self.dir()?).await
    }
    async fn workspace_id(&self) -> Result<String> {
        repo::workspace_id(self.jj(), &self.dir()?).await
    }
    async fn current_branch(&self) -> Result<String> {
        repo::current_branch(self.jj(), &self.dir()?).await
    }
    async fn current_commit(&self) -> Result<String> {
        repo::current_commit(self.jj(), &self.dir()?).await
    }
    async fn detect_trunk(&self) -> Result<String> {
        repo::detect_trunk(self.jj(), &self.dir()?).await
    }

    // ----- Branches -------------------------------------------------------
    async fn local_branches(&self) -> Result<Vec<String>> {
        repo::local_branches(self.jj(), &self.dir()?).await
    }
    async fn branch_exists(&self, name: &str) -> Result<bool> {
        repo::branch_exists(self.jj(), &self.dir()?, name).await
    }
    async fn remote_branch_exists(&self, name: &str) -> Result<bool> {
        repo::remote_branch_exists(self.jj(), &self.dir()?, name).await
    }
    async fn is_merged(&self, branch: &str, target: &str) -> Result<bool> {
        branch::is_merged(self.jj(), &self.dir()?, branch, target).await
    }
    async fn has_diff_from(&self, branch: &str, target: &str) -> Result<bool> {
        branch::has_diff_from(self.jj(), &self.dir()?, branch, target).await
    }
    async fn delete_branch(&self, name: &str, force: bool) -> Result<()> {
        repo::delete_branch(self.jj(), &self.dir()?, name, force).await
    }
    async fn rename_branch(&self, old: &str, new: &str) -> Result<()> {
        repo::rename_branch(self.jj(), &self.dir()?, old, new).await
    }
    async fn log_oneline(&self, from: &str, to: &str) -> Result<String> {
        branch::log_oneline(self.jj(), &self.dir()?, from, to).await
    }
    async fn commit_count(&self, from: &str, to: &str) -> Result<usize> {
        branch::commit_count(self.jj(), &self.dir()?, from, to).await
    }
    async fn diff_shortstat(&self, from: &str, to: &str) -> Result<DiffStat> {
        branch::diff_shortstat(self.jj(), &self.dir()?, from, to).await
    }
    async fn diff_shortstat_in(&self, path: &Path) -> Result<DiffStat> {
        branch::diff_shortstat_in(self.jj(), path).await
    }

    // ----- Working-copy state --------------------------------------------
    async fn has_uncommitted_changes(&self) -> Result<bool> {
        branch::has_uncommitted_changes(self.jj(), &self.dir()?).await
    }
    async fn uncommitted_count_in(&self, path: &Path) -> Result<usize> {
        branch::uncommitted_count_in(self.jj(), path).await
    }
    async fn has_changes_from_trunk(&self, trunk: &str) -> Result<bool> {
        branch::has_changes_from_trunk(self.jj(), &self.dir()?, trunk).await
    }
    /// **Locked decision**: jj operations are atomic; there is no "rebase in
    /// progress" state. Always false.
    async fn is_rebase_in_progress(&self) -> bool {
        false
    }
    /// **Locked decision**: jj treats conflicts as committed state — implemented
    /// by querying whether `@` is conflicted.
    async fn is_merge_in_progress(&self) -> bool {
        branch::is_merge_in_progress(self.jj(), &self.dir().unwrap_or_default()).await
    }

    // ----- Mutations ------------------------------------------------------
    async fn merge(
        &self,
        branch: &str,
        dest_bookmark: &str,
        squash: bool,
        no_ff: bool,
        message: Option<&str>,
    ) -> Result<()> {
        ops::merge(self.jj(), &self.dir()?, branch, dest_bookmark, squash, no_ff, message).await
    }
    async fn dry_run_merge(&self, branch: &str, squash: bool) -> Result<bool> {
        ops::dry_run_merge(self.jj(), &self.dir()?, branch, squash).await
    }
    async fn rebase(&self, onto: &str) -> Result<()> {
        ops::rebase(&self.dir()?, onto).await
    }
    async fn checkout(&self, branch: &str) -> Result<()> {
        ops::checkout(self.jj(), &self.dir()?, branch).await
    }
    async fn commit(&self, message: &str) -> Result<()> {
        ops::commit(&self.dir()?, message).await
    }
    async fn fetch(&self) -> Result<()> {
        ops::fetch(self.jj(), &self.dir()?).await
    }
    /// **Locked decision**: jj has no in-progress state to abort/continue.
    async fn rebase_abort(&self) -> Result<()> {
        Err(Error::Unsupported(
            "jj: rebase_abort — jj records conflicts in commits; resolve files and re-run".into(),
        ))
    }
    async fn rebase_continue(&self) -> Result<()> {
        Err(Error::Unsupported(
            "jj: rebase_continue — jj records conflicts in commits; resolve files and re-run".into(),
        ))
    }
    async fn merge_abort(&self) -> Result<()> {
        Err(Error::Unsupported(
            "jj: merge_abort — jj records conflicts in commits; resolve files and re-run".into(),
        ))
    }
    async fn merge_continue(&self) -> Result<()> {
        Err(Error::Unsupported(
            "jj: merge_continue — jj records conflicts in commits; resolve files and re-run".into(),
        ))
    }
    async fn reset_merge(&self) -> Result<()> {
        ops::reset_merge(&self.dir()?).await
    }

    // ----- Worktrees / workspaces ----------------------------------------
    async fn list_worktrees(&self) -> Result<Vec<WorktreeInfo>> {
        worktree::list_worktrees(self.jj(), &self.dir()?).await
    }
    async fn create_worktree(
        &self,
        path: &Path,
        branch: &str,
        base: &str,
    ) -> Result<crate::vcs::common::CreateOutcome> {
        worktree::create_worktree(self.jj(), &self.dir()?, path, branch, base).await
    }
    async fn create_worktree_from_remote(
        &self,
        path: &Path,
        branch: &str,
    ) -> Result<crate::vcs::common::CreateOutcome> {
        worktree::create_worktree_from_remote(self.jj(), &self.dir()?, path, branch).await
    }
    async fn remove_worktree(&self, path: &Path, force: bool) -> Result<()> {
        worktree::remove_worktree(self.jj(), &self.dir()?, path, force).await
    }
    /// **Locked decision**: `ws mv` on jj workspaces is not supported — jj has
    /// no `workspace move` primitive.
    async fn move_worktree(&self, _old: &Path, _new: &Path) -> Result<()> {
        Err(Error::Unsupported(
            "jj: move_worktree — remove and re-create the workspace".into(),
        ))
    }

    // ----- Synchronous cleanup (for WorktreeGuard::drop) ------------------
    fn cleanup_worktree_blocking(&self, path: &Path) -> Result<()> {
        // `Drop` can't await; remove the on-disk workspace dir + forget it from
        // jj synchronously. Order mirrors the async `remove_worktree`: delete
        // the dir first (an orphan dir is worse than a still-attached ws), then
        // best-effort `jj workspace forget`. Resolve the workspace name BEFORE
        // removing the dir (the lookup needs the live `jj workspace root`).
        let cwd = self.dir().map_err(|e| Error::Command(e.to_string()))?;
        let ws_name = worktree::workspace_name_for_path_blocking(&cwd, path);
        if path.exists() {
            std::fs::remove_dir_all(path).map_err(|e| Error::Command(e.to_string()))?;
        }
        // Forget only when we positively identified the workspace — never guess
        // a name (which could forget an unrelated workspace). Best-effort: jj
        // happily forgets an already-deleted ws dir.
        if let Some(ws_name) = ws_name {
            let _ = std::process::Command::new("jj")
                .current_dir(&cwd)
                .args(["workspace", "forget", &ws_name])
                .status();
        }
        Ok(())
    }
}
