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

/// True iff `git ls-remote --heads origin <name>` output advertises a branch
/// named EXACTLY `<name>`.
///
/// `ls-remote` prints one `"<sha>\t<refname>"` line per matching ref. We require
/// `refname == "refs/heads/<name>"` — an exact match, never a substring/suffix
/// test: `git ls-remote --heads origin foo` matches refs by trailing path
/// component, so probing `foo` would otherwise falsely succeed on a remote
/// `refs/heads/x/foo`. Pure + backend-agnostic so both `GitBackend` and the
/// colocated `JjBackend` (which also shells out to `git ls-remote`) share it.
pub(crate) fn ls_remote_has_branch(stdout: &str, name: &str) -> bool {
    let target = format!("refs/heads/{name}");
    stdout.lines().any(|line| {
        line.split('\t').nth(1).map(|refname| refname.trim() == target).unwrap_or(false)
    })
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

#[cfg(test)]
mod tests {
    use super::ls_remote_has_branch;

    fn line(sha: &str, refname: &str) -> String {
        format!("{sha}\t{refname}\n")
    }

    #[test]
    fn exact_match_is_true() {
        assert!(ls_remote_has_branch(&line("abc", "refs/heads/feature"), "feature"));
    }

    #[test]
    fn suffix_neighbour_is_false() {
        // `--heads foo` can advertise `x/foo`; an exact-refname test must not match.
        assert!(!ls_remote_has_branch(&line("a", "refs/heads/x/foo"), "foo"));
        assert!(!ls_remote_has_branch(&line("a", "refs/heads/feat/foo"), "foo"));
        assert!(!ls_remote_has_branch(&line("a", "refs/heads/feat/foo"), "feat"));
    }

    #[test]
    fn slash_name_matches_exactly() {
        assert!(ls_remote_has_branch(&line("a", "refs/heads/feat/foo"), "feat/foo"));
    }

    #[test]
    fn picks_the_right_line_among_many() {
        let out = format!(
            "{}{}{}",
            line("a", "refs/heads/main"),
            line("b", "refs/heads/feature"),
            line("c", "refs/heads/dev"),
        );
        assert!(ls_remote_has_branch(&out, "feature"));
        assert!(!ls_remote_has_branch(&out, "nope"));
    }

    #[test]
    fn empty_or_malformed_is_false() {
        assert!(!ls_remote_has_branch("", "feature"));
        assert!(!ls_remote_has_branch("refs/heads/feature\n", "feature")); // no tab
    }
}

