// ===========================================================================
// vcs/git/branch - Branch ops + working-copy state
// ===========================================================================

use std::path::Path;

use vcs_runner::{Cmd, RunError, Runner};

use super::errmap::map_run_err;
use crate::vcs::common::DiffStat;
use crate::vcs::error::Result;

/// Check if branch is merged into target.
pub(super) fn is_merged(runner: &dyn Runner, branch: &str, target: &str) -> Result<bool> {
    let cwd = std::env::current_dir()?;
    match runner.run(Cmd::new("git").in_dir(&cwd).args(["branch", "--merged", target])) {
        Ok(out) => Ok(out
            .stdout_lossy()
            .lines()
            .any(|l| l.trim().trim_start_matches("* ") == branch)),
        Err(RunError::NonZeroExit { .. }) => Ok(false),
        Err(e) => Err(map_run_err(e)),
    }
}

/// True if `branch` has any committed-or-uncommitted diff from `target`.
pub(super) fn has_diff_from(runner: &dyn Runner, branch: &str, target: &str) -> Result<bool> {
    let cwd = std::env::current_dir()?;
    let range = format!("{target}...{branch}");
    // `git diff --quiet`: exit 0 = no diff, exit 1 = has diff. Exit 1 is a
    // signal, not an error.
    match runner.run(Cmd::new("git").in_dir(&cwd).args(["diff", "--quiet", &range])) {
        Ok(_) => {}
        Err(RunError::NonZeroExit { .. }) => return Ok(true),
        Err(e) => return Err(map_run_err(e)),
    }

    // No diff in the tree — check if branch has commits the target lacks.
    Ok(commit_count(runner, target, branch)? > 0)
}

/// Delete a branch (`-d` / `-D` for force).
pub(super) fn delete_branch(runner: &dyn Runner, name: &str, force: bool) -> Result<()> {
    let flag = if force { "-D" } else { "-d" };
    super::exec(runner, &["branch", flag, name])
}

/// Check for any uncommitted changes in the current working directory.
pub(super) fn has_uncommitted_changes(runner: &dyn Runner) -> Result<bool> {
    let cwd = std::env::current_dir()?;
    let out = runner
        .run(Cmd::new("git").in_dir(&cwd).args(["status", "--porcelain"]))
        .map_err(map_run_err)?;
    Ok(!out.stdout.is_empty())
}

/// Count uncommitted files in a specific worktree path.
pub(super) fn uncommitted_count_in(runner: &dyn Runner, path: &Path) -> Result<usize> {
    let out = runner
        .run(Cmd::new("git").in_dir(path).args(["status", "--porcelain"]))
        .map_err(map_run_err)?;
    Ok(out.stdout_lossy().lines().filter(|l| !l.is_empty()).count())
}

/// Get diff `--shortstat` between two refs (committed changes).
///
/// Output format: `" 3 files changed, 120 insertions(+), 30 deletions(-)"`
pub(super) fn diff_shortstat(runner: &dyn Runner, from: &str, to: &str) -> Result<DiffStat> {
    let cwd = std::env::current_dir()?;
    let range = format!("{from}...{to}");
    match runner.run(Cmd::new("git").in_dir(&cwd).args(["diff", "--shortstat", &range])) {
        Ok(out) => Ok(parse_shortstat(&out.stdout_lossy())),
        Err(RunError::NonZeroExit { .. }) => Ok(DiffStat::default()),
        Err(e) => Err(map_run_err(e)),
    }
}

/// Get `--shortstat` for uncommitted changes in a worktree.
pub(super) fn diff_shortstat_in(runner: &dyn Runner, path: &Path) -> Result<DiffStat> {
    match runner.run(Cmd::new("git").in_dir(path).args(["diff", "--shortstat", "HEAD"])) {
        Ok(out) => Ok(parse_shortstat(&out.stdout_lossy())),
        Err(RunError::NonZeroExit { .. }) => Ok(DiffStat::default()),
        Err(e) => Err(map_run_err(e)),
    }
}

/// Parse `git diff --shortstat` output into a [`DiffStat`].
///
/// `--shortstat` is **git-specific** — jj's `diff --summary` has a different
/// schema (per-file `M/A/D` lines, no aggregate counts). When `JjBackend`
/// lands it will need its own parser; don't try to reuse `parse_diff_summary`
/// from vcs-runner here.
pub fn parse_shortstat(output: &str) -> DiffStat {
    let line = output.trim();
    if line.is_empty() {
        return DiffStat::default();
    }

    let mut insertions = 0;
    let mut deletions = 0;

    for part in line.split(',') {
        let part = part.trim();
        if part.contains("insertion") {
            insertions = part
                .split_whitespace()
                .next()
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
        } else if part.contains("deletion") {
            deletions = part
                .split_whitespace()
                .next()
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
        }
    }

    DiffStat { insertions, deletions }
}

/// True if current branch has any uncommitted changes OR commits ahead of trunk.
pub(super) fn has_changes_from_trunk(runner: &dyn Runner, trunk: &str) -> Result<bool> {
    if has_uncommitted_changes(runner)? {
        return Ok(true);
    }
    Ok(commit_count(runner, trunk, "HEAD")? > 0)
}

/// Rename a branch in place.
pub(super) fn rename_branch(runner: &dyn Runner, old: &str, new: &str) -> Result<()> {
    super::exec(runner, &["branch", "-m", old, new])
}

/// Short log of commits between two refs (`git log --oneline from..to`).
pub(super) fn log_oneline(runner: &dyn Runner, from: &str, to: &str) -> Result<String> {
    let cwd = std::env::current_dir()?;
    let range = format!("{from}..{to}");
    match runner.run(Cmd::new("git").in_dir(&cwd).args(["log", "--oneline", &range])) {
        Ok(out) => Ok(out.stdout_lossy().to_string()),
        Err(RunError::NonZeroExit { .. }) => Ok(String::new()),
        Err(e) => Err(map_run_err(e)),
    }
}

/// Count commits in a range (`git rev-list --count from..to`).
pub(super) fn commit_count(runner: &dyn Runner, from: &str, to: &str) -> Result<usize> {
    let cwd = std::env::current_dir()?;
    let range = format!("{from}..{to}");
    match runner.run(Cmd::new("git").in_dir(&cwd).args(["rev-list", "--count", &range])) {
        Ok(out) => Ok(out.stdout_lossy().trim().parse().unwrap_or(0)),
        Err(RunError::NonZeroExit { .. }) => Ok(0),
        Err(e) => Err(map_run_err(e)),
    }
}
