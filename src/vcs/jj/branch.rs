// ===========================================================================
// vcs/jj/branch - Working-copy state + diff + log (jj)
// ===========================================================================
//
// Mirrors `src/vcs/git/branch.rs` + the state probes from `git/ops.rs`. Methods
// here are about working-copy state + revset queries that touch bookmarks
// (jj's branches), on the typed `vcs-jj` client.

use processkit::Error as PkError;
use vcs_jj::JjAt;

use super::errmap::map_pk_err;
use crate::vcs::common::DiffStat;
use crate::vcs::error::Result;

/// Convert the typed client's `DiffStat` into ours (we don't track
/// `files_changed`).
fn conv(s: vcs_jj::DiffStat) -> DiffStat {
    DiffStat { insertions: s.insertions, deletions: s.deletions }
}

// ---------------------------------------------------------------------------
// Working-copy state
// ---------------------------------------------------------------------------

/// True iff the working-copy commit (`@`) differs from its parent (`@-`).
/// jj auto-snapshots untracked files into `@` on every command, so `@-..@`
/// captures exactly what a user thinks of as "uncommitted changes".
pub(crate) async fn has_uncommitted_changes(j: JjAt<'_>) -> Result<bool> {
    Ok(!j.diff_summary("@-", "@").await.map_err(map_pk_err)?.is_empty())
}

/// Count of files in the working-copy diff (at the view's anchored path).
pub(crate) async fn uncommitted_count_in(j: JjAt<'_>) -> Result<usize> {
    Ok(j.diff_summary("@-", "@").await.map_err(map_pk_err)?.len())
}

// ---------------------------------------------------------------------------
// Branch / range queries
// ---------------------------------------------------------------------------

/// True iff there are any commits divergent between `branch` and `target` (jj
/// has no working-copy/index split, so the answer is symmetric).
pub(crate) async fn has_diff_from(j: JjAt<'_>, branch: &str, target: &str) -> Result<bool> {
    Ok(commit_count(j, target, branch).await? > 0
        || commit_count(j, branch, target).await? > 0)
}

/// Number of commits in `to` not reachable from `from` — the revset
/// `to ~ ancestors(from)`, symmetric with git's `from..to`.
pub(crate) async fn commit_count(j: JjAt<'_>, from: &str, to: &str) -> Result<usize> {
    commit_count_via_revset(j, &format!("{to} ~ ancestors({from})")).await
}

/// Count commits matching an arbitrary revset. Returns 0 on revset-resolution
/// errors (mirrors git's "outside repo / bad ref" tolerance).
pub(crate) async fn commit_count_via_revset(j: JjAt<'_>, revset: &str) -> Result<usize> {
    match j.commit_count(revset).await {
        Ok(n) => Ok(n),
        Err(PkError::Exit { .. }) => Ok(0),
        Err(e) => Err(map_pk_err(e)),
    }
}

/// Short log of commits in `to` not reachable from `from`. Template matches
/// git's `--oneline`: `<short-id> <description-first-line>`.
pub(crate) async fn log_oneline(j: JjAt<'_>, from: &str, to: &str) -> Result<String> {
    let revset = format!("{to} ~ ancestors({from})");
    let template = r#"commit_id.short() ++ " " ++ description.first_line() ++ "\n""#;
    match j.template_query(&revset, template, None).await {
        Ok(out) => Ok(out),
        Err(PkError::Exit { .. }) => Ok(String::new()),
        Err(e) => Err(map_pk_err(e)),
    }
}

// ---------------------------------------------------------------------------
// Diff stats
// ---------------------------------------------------------------------------

/// Aggregated insertion/deletion counts between two refs.
pub(crate) async fn diff_shortstat(j: JjAt<'_>, from: &str, to: &str) -> Result<DiffStat> {
    let range = format!("{from}..{to}");
    match j.diff_stat(&range).await {
        Ok(stat) => Ok(conv(stat)),
        Err(PkError::Exit { .. }) => Ok(DiffStat::default()),
        Err(e) => Err(map_pk_err(e)),
    }
}

/// Working-copy diff stats for the workspace (at the view's anchored path).
pub(crate) async fn diff_shortstat_in(j: JjAt<'_>) -> Result<DiffStat> {
    match j.diff_stat("@-..@").await {
        Ok(stat) => Ok(conv(stat)),
        Err(PkError::Exit { .. }) => Ok(DiffStat::default()),
        Err(e) => Err(map_pk_err(e)),
    }
}
