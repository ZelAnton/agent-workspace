// ===========================================================================
// vcs/jj/branch - Working-copy state + diff + log (jj)
// ===========================================================================
//
// Mirrors `src/vcs/git/branch.rs` + the state probes from `git/ops.rs`. Methods
// here are about working-copy state + revset queries that touch bookmarks
// (jj's branches), on the typed `vcs-jj` client.

use std::path::Path;

use processkit::Error as PkError;
use vcs_jj::JjApi;

use super::JjClient;
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
pub(crate) async fn has_uncommitted_changes(jj: &JjClient, cwd: &Path) -> Result<bool> {
    Ok(!jj.diff_summary(cwd, "@-", "@").await.map_err(map_pk_err)?.is_empty())
}

/// Count of files in the working-copy diff scoped to `path`.
pub(crate) async fn uncommitted_count_in(jj: &JjClient, path: &Path) -> Result<usize> {
    Ok(jj.diff_summary(path, "@-", "@").await.map_err(map_pk_err)?.len())
}

/// True iff `@` is a conflicted commit (jj's first-class conflict state). jj
/// operations are atomic — there is no transient "merge in progress" state like
/// git's `MERGE_HEAD`; conflicts get recorded into the resulting commit.
/// Best-effort: a probe failure reads as "no conflict".
pub(crate) async fn is_merge_in_progress(jj: &JjClient, cwd: &Path) -> bool {
    jj.has_workingcopy_conflict(cwd).await.unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Branch / range queries
// ---------------------------------------------------------------------------

/// True iff there are any commits divergent between `branch` and `target` (jj
/// has no working-copy/index split, so the answer is symmetric).
pub(crate) async fn has_diff_from(jj: &JjClient, cwd: &Path, branch: &str, target: &str) -> Result<bool> {
    Ok(commit_count(jj, cwd, target, branch).await? > 0
        || commit_count(jj, cwd, branch, target).await? > 0)
}

/// Number of commits in `to` not reachable from `from` — the revset
/// `to ~ ancestors(from)`, symmetric with git's `from..to`.
pub(crate) async fn commit_count(jj: &JjClient, cwd: &Path, from: &str, to: &str) -> Result<usize> {
    commit_count_via_revset(jj, cwd, &format!("{to} ~ ancestors({from})")).await
}

/// Count commits matching an arbitrary revset. Returns 0 on revset-resolution
/// errors (mirrors git's "outside repo / bad ref" tolerance).
pub(crate) async fn commit_count_via_revset(jj: &JjClient, cwd: &Path, revset: &str) -> Result<usize> {
    match jj.commit_count(cwd, revset).await {
        Ok(n) => Ok(n),
        Err(PkError::Exit { .. }) => Ok(0),
        Err(e) => Err(map_pk_err(e)),
    }
}

/// Short log of commits in `to` not reachable from `from`. Template matches
/// git's `--oneline`: `<short-id> <description-first-line>`.
pub(crate) async fn log_oneline(jj: &JjClient, cwd: &Path, from: &str, to: &str) -> Result<String> {
    let revset = format!("{to} ~ ancestors({from})");
    let template = r#"commit_id.short() ++ " " ++ description.first_line() ++ "\n""#;
    match jj.template_query(cwd, &revset, template, None).await {
        Ok(out) => Ok(out),
        Err(PkError::Exit { .. }) => Ok(String::new()),
        Err(e) => Err(map_pk_err(e)),
    }
}

// ---------------------------------------------------------------------------
// Diff stats
// ---------------------------------------------------------------------------

/// Aggregated insertion/deletion counts between two refs.
pub(crate) async fn diff_shortstat(jj: &JjClient, cwd: &Path, from: &str, to: &str) -> Result<DiffStat> {
    let range = format!("{from}..{to}");
    match jj.diff_stat(cwd, &range).await {
        Ok(stat) => Ok(conv(stat)),
        Err(PkError::Exit { .. }) => Ok(DiffStat::default()),
        Err(e) => Err(map_pk_err(e)),
    }
}

/// Working-copy diff stats for the workspace at `path`.
pub(crate) async fn diff_shortstat_in(jj: &JjClient, path: &Path) -> Result<DiffStat> {
    match jj.diff_stat(path, "@-..@").await {
        Ok(stat) => Ok(conv(stat)),
        Err(PkError::Exit { .. }) => Ok(DiffStat::default()),
        Err(e) => Err(map_pk_err(e)),
    }
}
