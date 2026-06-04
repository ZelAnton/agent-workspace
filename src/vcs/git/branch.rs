// ===========================================================================
// vcs/git/branch - Branch ops + working-copy state
// ===========================================================================

use std::path::Path;

use processkit::Error as PkError;
use vcs_git::GitAt;

use super::errmap::map_pk_err;
use crate::vcs::common::DiffStat;
use crate::vcs::error::Result;

/// Convert the typed client's `DiffStat` into ours (we don't track
/// `files_changed`).
fn conv(s: vcs_git::DiffStat) -> DiffStat {
    DiffStat { insertions: s.insertions, deletions: s.deletions }
}

/// True if `branch` has any committed-or-uncommitted diff from `target`.
pub(crate) async fn has_diff_from(g: GitAt<'_>, branch: &str, target: &str) -> Result<bool> {
    let range = format!("{target}...{branch}");
    // A non-empty tree diff means "has diff" outright.
    if !g.diff_range_is_empty(&range).await.map_err(map_pk_err)? {
        return Ok(true);
    }
    // No diff in the tree — check if branch has commits the target lacks.
    Ok(commit_count(g, target, branch).await? > 0)
}

/// Delete a branch (`-d` / `-D` for force).
pub(crate) async fn delete_branch(g: GitAt<'_>, name: &str, force: bool) -> Result<()> {
    g.delete_branch(name, force).await.map_err(map_pk_err)
}

/// Check for any uncommitted changes in the current working directory.
pub(crate) async fn has_uncommitted_changes(g: GitAt<'_>) -> Result<bool> {
    Ok(!g.status().await.map_err(map_pk_err)?.is_empty())
}

/// Count uncommitted files in a specific worktree (the view's anchored path).
pub(crate) async fn uncommitted_count_in(g: GitAt<'_>) -> Result<usize> {
    Ok(g.status().await.map_err(map_pk_err)?.len())
}

/// Get diff `--shortstat` between two refs (committed changes).
pub(crate) async fn diff_shortstat(g: GitAt<'_>, from: &str, to: &str) -> Result<DiffStat> {
    let range = format!("{from}...{to}");
    match g.diff_stat(&range).await {
        Ok(stat) => Ok(conv(stat)),
        Err(PkError::Exit { .. }) => Ok(DiffStat::default()),
        Err(e) => Err(map_pk_err(e)),
    }
}

/// Get `--shortstat` for uncommitted changes in a worktree (`git diff
/// --shortstat HEAD`, at the view's anchored path).
pub(crate) async fn diff_shortstat_in(g: GitAt<'_>) -> Result<DiffStat> {
    match g.diff_stat("HEAD").await {
        Ok(stat) => Ok(conv(stat)),
        Err(PkError::Exit { .. }) => Ok(DiffStat::default()),
        Err(e) => Err(map_pk_err(e)),
    }
}

/// Parse `git diff --shortstat` output into a [`DiffStat`].
///
/// Kept as a pure helper (re-exported for tests and fixture parsing) even
/// though the typed client now does the shortstat parsing internally.
///
/// `--shortstat` is **git-specific** — jj's `diff --summary` has a different
/// schema (per-file `M/A/D` lines, no aggregate counts).
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

/// Rename a branch in place.
pub(crate) async fn rename_branch(g: GitAt<'_>, old: &str, new: &str) -> Result<()> {
    g.rename_branch(old, new).await.map_err(map_pk_err)
}

/// Short log of commits between two refs (`git log --oneline from..to`).
///
/// Not modelled by the typed client, so it runs through a raw command. A
/// non-zero exit (e.g. an unknown ref) yields an empty string, matching the
/// original best-effort behaviour.
pub(crate) async fn log_oneline(cwd: &Path, from: &str, to: &str) -> Result<String> {
    let range = format!("{from}..{to}");
    let out = super::capture(cwd, ["log", "--oneline", &range]).await?;
    if out.is_success() {
        Ok(out.into_stdout())
    } else {
        Ok(String::new())
    }
}

/// Count commits in a range (`git rev-list --count from..to`).
pub(crate) async fn commit_count(g: GitAt<'_>, from: &str, to: &str) -> Result<usize> {
    let range = format!("{from}..{to}");
    match g.rev_list_count(&range).await {
        Ok(n) => Ok(n),
        Err(PkError::Exit { .. }) => Ok(0),
        Err(e) => Err(map_pk_err(e)),
    }
}
