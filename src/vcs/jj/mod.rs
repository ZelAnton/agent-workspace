// ===========================================================================
// vcs/jj - Jujutsu backend
// ===========================================================================
//
// Mirrors the structure of `src/vcs/git/`. Implementation lives in
// submodules (`repo`, ...) — `mod.rs` is just struct + trait-impl assembly.
//
// **Implementation status** (track against AGENTS.md's "VCS backend
// compatibility" section):
//   - F-1 (identity + bookmarks): implemented
//   - F-2 (workspaces): stubs
//   - F-3 (state + diff): stubs
//   - F-4 (mutations + atomic_merge): stubs
//   - F-5 (sync hints): N/A — handled at caller level
//
// Methods that are intentionally `unimplemented!()` per locked semantic
// decisions surface as `Error::Unsupported("jj: <op> — <hint>")` once F-2..F-4
// land. Until then, the catch-all `nyi()` helper produces the same shape.

mod branch;
mod errmap;
mod ops;
mod repo;
mod worktree;

pub use branch::parse_jj_stat_footer;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use vcs_runner::{Cmd, DefaultRunner, Runner};

use super::backend::VcsBackend;
use super::common::{DiffStat, WorktreeInfo};
use super::error::{Error, Result};

/// jj-backed [`VcsBackend`]. Stores the subprocess runner so tests can
/// swap in a `MockRunner` (parser-heavy tests) or `DefaultRunner` (e2e).
pub struct JjBackend {
    runner: Arc<dyn Runner>,
}

impl JjBackend {
    pub fn new() -> Self {
        Self { runner: Arc::new(DefaultRunner) }
    }

    pub fn with_runner(runner: Arc<dyn Runner>) -> Self {
        Self { runner }
    }
}

impl Default for JjBackend {
    fn default() -> Self {
        Self::new()
    }
}

/// Run a void jj command (no output parsing). Mirrors `git::exec` from the
/// git backend. Used by simple mutating ops that only need exit status.
pub(super) fn exec(runner: &dyn Runner, args: &[&str]) -> Result<()> {
    let cwd = std::env::current_dir()?;
    runner
        .run(Cmd::new("jj").in_dir(&cwd).args(args))
        .map(|_| ())
        .map_err(errmap::map_run_err)
}

// All F-1..F-5 methods are implemented. The previous `nyi(...)` helper
// for stubs has been retired now that no `unimplemented!`-style returns
// remain. Methods that genuinely have no jj equivalent (locked semantic
// decisions: `move_worktree`, `*_abort/*_continue`) return inline
// `Error::Unsupported(...)` with a hint message.

impl VcsBackend for JjBackend {
    fn name(&self) -> &'static str { "jj" }

    // ===================================================================
    // F-1: Identity + bookmarks — IMPLEMENTED
    // ===================================================================
    fn repo_root(&self) -> Result<PathBuf> { repo::repo_root(self.runner.as_ref()) }
    fn repo_name(&self) -> Result<String> { repo::repo_name(self.runner.as_ref()) }
    fn workspace_id(&self) -> Result<String> { repo::workspace_id(self.runner.as_ref()) }
    fn current_branch(&self) -> Result<String> { repo::current_branch(self.runner.as_ref()) }
    fn current_commit(&self) -> Result<String> { repo::current_commit(self.runner.as_ref()) }
    fn detect_trunk(&self) -> Result<String> { repo::detect_trunk(self.runner.as_ref()) }

    fn local_branches(&self) -> Result<Vec<String>> { repo::local_branches(self.runner.as_ref()) }
    fn branch_exists(&self, name: &str) -> Result<bool> {
        repo::branch_exists(self.runner.as_ref(), name)
    }
    fn rename_branch(&self, old: &str, new: &str) -> Result<()> {
        repo::rename_branch(self.runner.as_ref(), old, new)
    }
    fn delete_branch(&self, name: &str, force: bool) -> Result<()> {
        repo::delete_branch(self.runner.as_ref(), name, force)
    }

    // ===================================================================
    // F-2: Workspaces — IMPLEMENTED
    // ===================================================================
    fn list_worktrees(&self) -> Result<Vec<WorktreeInfo>> {
        worktree::list_worktrees(self.runner.as_ref())
    }
    fn create_worktree(
        &self,
        path: &Path,
        branch: &str,
        base: &str,
    ) -> Result<crate::vcs::common::CreateOutcome> {
        worktree::create_worktree(self.runner.as_ref(), path, branch, base)
    }
    fn remove_worktree(&self, path: &Path, force: bool) -> Result<()> {
        worktree::remove_worktree(self.runner.as_ref(), path, force)
    }
    /// **Per locked decision**: `wt mv` on jj workspaces is not supported.
    /// jj has no `workspace move` primitive; the manual recipe is "remove
    /// and re-create the workspace" — surface that to the user as an error.
    fn move_worktree(&self, _old: &Path, _new: &Path) -> Result<()> {
        Err(Error::Unsupported(
            "jj: move_worktree — remove and re-create the workspace".into(),
        ))
    }

    // ===================================================================
    // F-3: Working-copy state + diff — IMPLEMENTED
    // ===================================================================
    fn is_merged(&self, branch: &str, target: &str) -> Result<bool> {
        branch::is_merged(self.runner.as_ref(), branch, target)
    }
    fn has_diff_from(&self, branch: &str, target: &str) -> Result<bool> {
        branch::has_diff_from(self.runner.as_ref(), branch, target)
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
    fn has_uncommitted_changes(&self) -> Result<bool> {
        branch::has_uncommitted_changes(self.runner.as_ref())
    }
    fn uncommitted_count_in(&self, path: &Path) -> Result<usize> {
        branch::uncommitted_count_in(self.runner.as_ref(), path)
    }
    fn has_changes_from_trunk(&self, trunk: &str) -> Result<bool> {
        branch::has_changes_from_trunk(self.runner.as_ref(), trunk)
    }
    /// **Per locked decision**: jj operations are atomic; there is no
    /// "rebase in progress" state. Always false.
    fn is_rebase_in_progress(&self) -> bool { false }
    /// **Per locked decision**: jj treats conflicts as committed state.
    /// Implemented by scanning `jj st` for the unresolved-conflicts marker.
    fn is_merge_in_progress(&self) -> bool {
        branch::is_merge_in_progress(self.runner.as_ref())
    }

    // ===================================================================
    // F-4: Mutations — IMPLEMENTED
    // ===================================================================
    fn merge(
        &self,
        branch: &str,
        squash: bool,
        no_ff: bool,
        message: Option<&str>,
    ) -> Result<()> {
        ops::merge(self.runner.as_ref(), branch, squash, no_ff, message)
    }
    fn dry_run_merge(&self, branch: &str, squash: bool) -> Result<bool> {
        ops::dry_run_merge(self.runner.as_ref(), branch, squash)
    }
    fn rebase(&self, onto: &str) -> Result<()> { ops::rebase(self.runner.as_ref(), onto) }
    fn checkout(&self, branch: &str) -> Result<()> {
        ops::checkout(self.runner.as_ref(), branch)
    }
    fn commit(&self, message: &str) -> Result<()> {
        ops::commit(self.runner.as_ref(), message)
    }
    fn fetch(&self) -> Result<()> { ops::fetch(self.runner.as_ref()) }

    /// **Per locked decision**: jj has no in-progress state. Return
    /// `Unsupported` with a guidance message; `wt sync` will surface this
    /// directly until F-5 adds backend-aware hints at the caller layer.
    fn rebase_abort(&self) -> Result<()> {
        Err(Error::Unsupported(
            "jj: rebase_abort — jj records conflicts in commits; resolve files and re-run".into(),
        ))
    }
    fn rebase_continue(&self) -> Result<()> {
        Err(Error::Unsupported(
            "jj: rebase_continue — jj records conflicts in commits; resolve files and re-run".into(),
        ))
    }
    fn merge_abort(&self) -> Result<()> {
        Err(Error::Unsupported(
            "jj: merge_abort — jj records conflicts in commits; resolve files and re-run".into(),
        ))
    }
    fn merge_continue(&self) -> Result<()> {
        Err(Error::Unsupported(
            "jj: merge_continue — jj records conflicts in commits; resolve files and re-run".into(),
        ))
    }
    fn reset_merge(&self) -> Result<()> { ops::reset_merge(self.runner.as_ref()) }
}

#[cfg(test)]
mod tests;
