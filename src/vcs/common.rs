// ===========================================================================
// vcs/common - Backend-agnostic DTOs and helpers
// ===========================================================================
//
// Types in this module are shared across all backends. A jj backend that
// normalizes its "workspace" output into `WorktreeInfo` keeps the same DTO
// shape git produces — callers in `src/cli/` don't need to know which
// backend was used.

use std::path::{Path, PathBuf};

use super::error::{Error, Result};

/// Snapshot of one worktree (git) or workspace (jj, when implemented).
///
/// `branch` is `None` for detached-HEAD git worktrees and for bare worktrees;
/// `commit` is `None` for bare worktrees that have no checkout.
#[derive(Debug, Clone)]
pub struct WorktreeInfo {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub commit: Option<String>,
    pub is_bare: bool,
}

/// Aggregated insertion/deletion counts from `git diff --shortstat`
/// (or the jj equivalent, once `JjBackend` lands).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiffStat {
    pub insertions: usize,
    pub deletions: usize,
}

/// Safely convert a [`Path`] to `&str`, surfacing the bad path via
/// [`Error::Command`] instead of panicking on `to_str().unwrap()`.
///
/// Used when a path needs to be embedded inside an argument string for the
/// underlying CLI (e.g. `git worktree add <path>`). For setting the child's
/// cwd, prefer passing `&Path` to `Cmd::in_dir` directly.
pub fn path_str(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| Error::Command(format!("path contains invalid UTF-8: {}", path.display())))
}
