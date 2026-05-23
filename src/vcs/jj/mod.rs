// ===========================================================================
// vcs/jj - jj backend (stub)
// ===========================================================================
//
// **Every** method of `VcsBackend` is mirrored here as `unimplemented!()`.
// The stub is mandatory — it makes the trait surface explicitly visible
// for the future jj implementation and prevents `GitBackend` from drifting
// ahead of jj by adding methods that have no jj equivalent.
//
// When a method gets a real jj implementation, replace the
// `unimplemented!()` body in place; the method name + signature must stay
// identical to the trait. The stub message must include the op name so
// users hitting it via `wt <cmd>` in a jj repo today get a clear "this op
// is not yet supported by the jj backend" message instead of a generic
// panic.

use std::path::{Path, PathBuf};

use super::backend::VcsBackend;
use super::common::{DiffStat, WorktreeInfo};
use super::error::{Error, Result};

/// jj-backed [`VcsBackend`] — currently every method returns
/// `Error::Unsupported`. See module docs for the stub-policy rationale.
pub struct JjBackend;

impl JjBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Default for JjBackend {
    fn default() -> Self {
        Self::new()
    }
}

/// Build the standard "jj backend: <op> not yet implemented" error.
#[inline]
fn nyi(op: &str) -> Error {
    Error::Unsupported(format!("jj: {op}"))
}

impl VcsBackend for JjBackend {
    fn name(&self) -> &'static str { "jj" }

    // ----- Identity -------------------------------------------------------
    fn repo_root(&self) -> Result<PathBuf> { Err(nyi("repo_root")) }
    fn repo_name(&self) -> Result<String> { Err(nyi("repo_name")) }
    fn workspace_id(&self) -> Result<String> { Err(nyi("workspace_id")) }
    fn current_branch(&self) -> Result<String> { Err(nyi("current_branch")) }
    fn current_commit(&self) -> Result<String> { Err(nyi("current_commit")) }
    fn detect_trunk(&self) -> Result<String> { Err(nyi("detect_trunk")) }

    // ----- Branches -------------------------------------------------------
    fn local_branches(&self) -> Result<Vec<String>> { Err(nyi("local_branches")) }
    fn branch_exists(&self, _name: &str) -> Result<bool> { Err(nyi("branch_exists")) }
    fn is_merged(&self, _branch: &str, _target: &str) -> Result<bool> { Err(nyi("is_merged")) }
    fn has_diff_from(&self, _branch: &str, _target: &str) -> Result<bool> {
        Err(nyi("has_diff_from"))
    }
    fn delete_branch(&self, _name: &str, _force: bool) -> Result<()> { Err(nyi("delete_branch")) }
    fn rename_branch(&self, _old: &str, _new: &str) -> Result<()> { Err(nyi("rename_branch")) }
    fn log_oneline(&self, _from: &str, _to: &str) -> Result<String> { Err(nyi("log_oneline")) }
    fn commit_count(&self, _from: &str, _to: &str) -> Result<usize> { Err(nyi("commit_count")) }
    fn diff_shortstat(&self, _from: &str, _to: &str) -> Result<DiffStat> {
        Err(nyi("diff_shortstat"))
    }
    fn diff_shortstat_in(&self, _path: &Path) -> Result<DiffStat> { Err(nyi("diff_shortstat_in")) }

    // ----- Working-copy state --------------------------------------------
    fn has_uncommitted_changes(&self) -> Result<bool> { Err(nyi("has_uncommitted_changes")) }
    fn uncommitted_count_in(&self, _path: &Path) -> Result<usize> {
        Err(nyi("uncommitted_count_in"))
    }
    fn has_staged_changes(&self) -> Result<bool> { Err(nyi("has_staged_changes")) }
    fn has_changes_from_trunk(&self, _trunk: &str) -> Result<bool> {
        Err(nyi("has_changes_from_trunk"))
    }
    // State probes are infallible by trait — return false until jj impls land.
    // (jj has no "rebase in progress" or "merge in progress" state in the
    // same shape as git; the future jj impl will check `jj st` for
    // unresolved conflicts.)
    fn is_rebase_in_progress(&self) -> bool { false }
    fn is_merge_in_progress(&self) -> bool { false }

    // ----- Mutations ------------------------------------------------------
    fn merge(
        &self,
        _branch: &str,
        _squash: bool,
        _no_ff: bool,
        _message: Option<&str>,
    ) -> Result<()> {
        Err(nyi("merge"))
    }
    fn dry_run_merge(&self, _branch: &str, _squash: bool) -> Result<bool> {
        Err(nyi("dry_run_merge"))
    }
    fn rebase(&self, _onto: &str) -> Result<()> { Err(nyi("rebase")) }
    fn checkout(&self, _branch: &str) -> Result<()> { Err(nyi("checkout")) }
    fn commit(&self, _message: &str) -> Result<()> { Err(nyi("commit")) }
    fn fetch(&self) -> Result<()> { Err(nyi("fetch")) }
    fn rebase_abort(&self) -> Result<()> { Err(nyi("rebase_abort")) }
    fn rebase_continue(&self) -> Result<()> { Err(nyi("rebase_continue")) }
    fn merge_abort(&self) -> Result<()> { Err(nyi("merge_abort")) }
    fn merge_continue(&self) -> Result<()> { Err(nyi("merge_continue")) }
    fn reset_merge(&self) -> Result<()> { Err(nyi("reset_merge")) }

    // ----- Worktrees ------------------------------------------------------
    fn list_worktrees(&self) -> Result<Vec<WorktreeInfo>> { Err(nyi("list_worktrees")) }
    fn create_worktree(&self, _path: &Path, _branch: &str, _base: &str) -> Result<()> {
        Err(nyi("create_worktree"))
    }
    fn remove_worktree(&self, _path: &Path, _force: bool) -> Result<()> {
        Err(nyi("remove_worktree"))
    }
    fn move_worktree(&self, _old: &Path, _new: &Path) -> Result<()> { Err(nyi("move_worktree")) }
}
