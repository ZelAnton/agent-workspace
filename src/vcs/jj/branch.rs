// ===========================================================================
// vcs/jj/branch - Working-copy state + diff + log (jj)
// ===========================================================================
//
// Mirrors the public surface of `src/vcs/git/branch.rs` + the state probes
// from `git/ops.rs`. The `branch` filename is kept for symmetry with the
// git backend's module layout — methods here are about working-copy state
// + revset queries that touch bookmarks (which are jj's branches).

use std::path::Path;

use vcs_runner::{parse_diff_summary, Cmd, RunError, Runner};

use super::errmap::map_run_err;
use crate::vcs::common::DiffStat;
use crate::vcs::error::Result;

// ---------------------------------------------------------------------------
// Working-copy state
// ---------------------------------------------------------------------------

/// True iff the working-copy commit (`@`) differs from its parent (`@-`).
///
/// jj snapshots untracked files into `@` automatically on every command,
/// so `@-..@` captures exactly what a user thinks of as "uncommitted
/// changes" — the modifications they've made since the last described
/// change.
pub(super) fn has_uncommitted_changes(runner: &dyn Runner) -> Result<bool> {
    let cwd = std::env::current_dir()?;
    let out = runner
        .run(
            Cmd::new("jj")
                .in_dir(&cwd)
                .args(["diff", "-r", "@-..@", "--summary"]),
        )
        .map_err(map_run_err)?;
    Ok(!out.stdout_lossy().trim().is_empty())
}

/// Count of files in the working-copy diff scoped to `path`.
///
/// Uses `vcs_runner::parse_diff_summary` to interpret the per-file
/// `M path` / `A path` / `D path` lines from `jj diff --summary`.
pub(super) fn uncommitted_count_in(runner: &dyn Runner, path: &Path) -> Result<usize> {
    let out = runner
        .run(
            Cmd::new("jj")
                .in_dir(path)
                .args(["diff", "-r", "@-..@", "--summary"]),
        )
        .map_err(map_run_err)?;
    Ok(parse_diff_summary(&out.stdout_lossy()).len())
}

/// True iff the working copy has changes OR commits ahead of `trunk`.
pub(super) fn has_changes_from_trunk(runner: &dyn Runner, trunk: &str) -> Result<bool> {
    if has_uncommitted_changes(runner)? {
        return Ok(true);
    }
    Ok(commit_count(runner, trunk, "@")? > 0)
}

/// True iff `@` is a conflicted commit (jj's first-class conflict state).
///
/// jj operations are atomic — there is no transient "merge in progress"
/// state like git's `MERGE_HEAD`. Conflicts get recorded into the resulting
/// commit. This probe scans `jj st` for the literal marker so callers
/// (`ws status`, `ws sync --continue`) can branch on it.
///
/// **Marker fragility note**: jj's wording has changed across versions.
/// The string `"There are unresolved conflicts"` has been stable since
/// jj 0.16. If we need to support older versions later, swap to a regex
/// over the parenthetical `(conflict)` flag in `jj st` output.
pub(super) fn is_merge_in_progress(runner: &dyn Runner) -> bool {
    let Ok(cwd) = std::env::current_dir() else {
        return false;
    };
    let Ok(out) = runner.run(Cmd::new("jj").in_dir(&cwd).args(["st"])) else {
        return false;
    };
    out.stdout_lossy()
        .contains("There are unresolved conflicts")
}

// ---------------------------------------------------------------------------
// Branch / range queries
// ---------------------------------------------------------------------------

/// True iff `branch`'s bookmark is reachable from `target`'s ancestors —
/// i.e. branch is fully merged into target.
pub(super) fn is_merged(runner: &dyn Runner, branch: &str, target: &str) -> Result<bool> {
    let revset = format!("ancestors({target}) & {branch}");
    Ok(commit_count_via_revset(runner, &revset)? > 0)
}

/// True iff there are any commits divergent between `branch` and `target`.
///
/// Returns true if either side has commits the other lacks. This unifies
/// git's "diff or commits-ahead" check into a single revset query — jj
/// has no working-copy/index split, so the answer is symmetric.
pub(super) fn has_diff_from(runner: &dyn Runner, branch: &str, target: &str) -> Result<bool> {
    Ok(commit_count(runner, target, branch)? > 0 || commit_count(runner, branch, target)? > 0)
}

/// Number of commits in `to` that aren't reachable from `from`.
///
/// Uses the revset `to ~ ancestors(from)` — symmetric with git's
/// `from..to` range. Returns 0 on revset-resolution errors (mirrors git's
/// "outside repo / bad ref" tolerance).
pub(super) fn commit_count(runner: &dyn Runner, from: &str, to: &str) -> Result<usize> {
    let revset = format!("{to} ~ ancestors({from})");
    commit_count_via_revset(runner, &revset)
}

/// Lower-level helper that counts commits matching an arbitrary revset.
pub(super) fn commit_count_via_revset(runner: &dyn Runner, revset: &str) -> Result<usize> {
    let cwd = std::env::current_dir()?;
    match runner.run(
        Cmd::new("jj").in_dir(&cwd).args([
            "log",
            "-r",
            revset,
            "-T",
            r#"commit_id ++ "\n""#,
            "--no-graph",
        ]),
    ) {
        Ok(out) => Ok(out.stdout_lossy().lines().filter(|l| !l.trim().is_empty()).count()),
        Err(RunError::NonZeroExit { .. }) => Ok(0),
        Err(e) => Err(map_run_err(e)),
    }
}

/// Short log of commits in `to` that aren't reachable from `from`.
/// Template matches git's `--oneline`: `<short-id> <description-first-line>`.
pub(super) fn log_oneline(runner: &dyn Runner, from: &str, to: &str) -> Result<String> {
    let cwd = std::env::current_dir()?;
    let revset = format!("{to} ~ ancestors({from})");
    match runner.run(Cmd::new("jj").in_dir(&cwd).args([
        "log",
        "-r",
        &revset,
        "-T",
        r#"commit_id.short() ++ " " ++ description.first_line() ++ "\n""#,
        "--no-graph",
    ])) {
        Ok(out) => Ok(out.stdout_lossy().to_string()),
        Err(RunError::NonZeroExit { .. }) => Ok(String::new()),
        Err(e) => Err(map_run_err(e)),
    }
}

// ---------------------------------------------------------------------------
// Diff stats
// ---------------------------------------------------------------------------

/// Aggregated insertion/deletion counts between two refs.
///
/// jj's `diff --stat` prints per-file lines plus a trailing footer in the
/// **same shape as git's `--shortstat`**:
/// `" 3 files changed, 120 insertions(+), 30 deletions(-)"`. We pluck the
/// last non-empty line and reuse a parser that's structurally identical
/// to the git one (insertion/deletion keyword match).
pub(super) fn diff_shortstat(runner: &dyn Runner, from: &str, to: &str) -> Result<DiffStat> {
    let cwd = std::env::current_dir()?;
    let range = format!("{from}..{to}");
    match runner.run(Cmd::new("jj").in_dir(&cwd).args(["diff", "-r", &range, "--stat"])) {
        Ok(out) => Ok(parse_jj_stat_footer(&out.stdout_lossy())),
        Err(RunError::NonZeroExit { .. }) => Ok(DiffStat::default()),
        Err(e) => Err(map_run_err(e)),
    }
}

/// Working-copy diff stats for the workspace at `path`. Same parser as
/// [`diff_shortstat`].
pub(super) fn diff_shortstat_in(runner: &dyn Runner, path: &Path) -> Result<DiffStat> {
    match runner.run(Cmd::new("jj").in_dir(path).args(["diff", "-r", "@-..@", "--stat"])) {
        Ok(out) => Ok(parse_jj_stat_footer(&out.stdout_lossy())),
        Err(RunError::NonZeroExit { .. }) => Ok(DiffStat::default()),
        Err(e) => Err(map_run_err(e)),
    }
}

/// Parse the trailing footer of `jj diff --stat`.
///
/// jj's output starts with per-file lines (one per changed file) and
/// concludes with the same `N files changed, X insertions(+), Y deletions(-)`
/// summary git's `--shortstat` produces. We grab the **last non-empty line**
/// and apply the same keyword extraction logic.
///
/// **Divergence from git's exact counts**: jj counts logical change lines
/// against `@`'s parent in the revset, while git's `--shortstat` compares
/// against the index. For our use case (display in `ws ls`/`ws status`)
/// the difference is cosmetic — both give the user a sense of magnitude.
pub fn parse_jj_stat_footer(output: &str) -> DiffStat {
    let footer = output
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    if footer.is_empty() || !footer.contains("changed") {
        return DiffStat::default();
    }

    let mut insertions = 0;
    let mut deletions = 0;
    for part in footer.split(',') {
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
