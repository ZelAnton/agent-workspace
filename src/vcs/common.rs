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

/// How `create_worktree` materialised the new worktree's contents.
///
/// Some downstream steps (notably the per-file `copy_files` patterns from
/// `[general] copy_files`) are redundant when the worktree was cloned in
/// bulk via reflink — the source repo's files are already there. The
/// caller in `src/cli/commands/lifecycle/new.rs` switches on this to
/// avoid duplicate work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateOutcome {
    /// Standard backend creation (git's `worktree add` checkout, or jj's
    /// `workspace add` materialisation). Caller must still run
    /// `copy_files` for any patterns specified in config.
    Plain,
    /// Worktree was created via `--no-checkout` and the source repo's
    /// files were cloned in bulk via reflink. `copy_files` is redundant
    /// — every file the source repo had (sans `.git/`) is already in the
    /// new worktree.
    CowCloned,
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
