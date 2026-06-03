// ===========================================================================
// vcs/jj/ops - Mutations (merge / rebase / checkout / fetch)
// ===========================================================================
//
// **Semantic deltas vs git** (locked decisions + jj's model):
//   - `merge` is atomic in jj: every `jj new` records conflicts into the
//     resulting commit rather than failing. We materialise the merge, then
//     `jj op restore <pre-op-id>` on internal failure to keep the "main repo
//     never silently changes on failure" invariant `ws merge` relies on.
//   - `*_abort` / `*_continue` are `Unsupported` (returned by the facade
//     wrapper's jj arm): jj has no in-progress state to abort or continue from.

use std::path::Path;

use vcs_jj::JjApi;

use super::JjClient;
use super::errmap::map_pk_err;
use crate::vcs::error::{Error, Result};

/// How many times a transient `jj git fetch` failure is retried before giving up.
const FETCH_MAX_ATTEMPTS: usize = 3;

/// `jj rebase -d <onto>` — rebase `@` onto the given revision. jj's rebase is
/// atomic; conflicts are recorded into the rebased commits. Callers probe
/// `is_merge_in_progress()` afterward to detect that.
pub(crate) async fn rebase(cwd: &Path, onto: &str) -> Result<()> {
    super::exec(cwd, ["rebase", "-d", onto]).await
}

/// `jj edit <branch>` — move `@` to the named bookmark's commit. Pre-checks the
/// bookmark exists (managed worktrees always have bookmarks, matching git's
/// `checkout` semantics).
pub(crate) async fn checkout(jj: &JjClient, cwd: &Path, branch: &str) -> Result<()> {
    if !super::repo::branch_exists(jj, cwd, branch).await? {
        return Err(Error::BranchNotFound(branch.to_string()));
    }
    jj.edit(cwd, branch).await.map_err(map_pk_err)
}

/// Fetch a SINGLE bookmark from `origin` (`jj git fetch --remote origin -b
/// <branch>`). Targeted and **hard-fails** on error — callers reach it only
/// after `remote_branch_exists` confirmed the bookmark is there. Transient
/// network blips retry first.
pub(crate) async fn fetch_remote_branch(jj: &JjClient, cwd: &Path, branch: &str) -> Result<()> {
    let mut attempt = 0;
    loop {
        attempt += 1;
        match jj.git_fetch_branch(cwd, branch).await {
            Ok(()) => return Ok(()),
            Err(e) if attempt < FETCH_MAX_ATTEMPTS && vcs_jj::is_transient_fetch_error(&e) => {
                continue;
            }
            Err(e) => return Err(map_pk_err(e)),
        }
    }
}

/// Capture the current operation id for later `jj op restore`-based rollback.
pub(crate) async fn capture_op_id(jj: &JjClient, cwd: &Path) -> Result<String> {
    jj.op_head(cwd).await.map_err(map_pk_err)
}

/// True iff the commit at `revset` has conflicts. Best-effort.
async fn has_conflict_at(jj: &JjClient, cwd: &Path, revset: &str) -> bool {
    jj.is_conflicted(cwd, revset).await.unwrap_or(false)
}

/// **Dry-run merge.** Materialise the merge, check for conflicts, restore to the
/// pre-merge op state via `jj op restore <pre-op-id>`. Returns `Ok(true)` if the
/// merge would be clean.
pub(crate) async fn dry_run_merge(jj: &JjClient, cwd: &Path, branch: &str, squash: bool) -> Result<bool> {
    let pre_op = capture_op_id(jj, cwd).await?;

    // Pre-flight: already up to date?
    if super::branch::commit_count_via_revset(jj, cwd, &format!("({branch}) ~ ancestors(@)")).await?
        == 0
    {
        return Ok(true);
    }

    // Materialise the merge with a sentinel description (visible in `jj op log`).
    let new_result = jj.new_merge(cwd, "WT-DRY-RUN", vec!["@".into(), branch.into()]).await;

    // jj can return a 0 exit with a conflicted commit, so probe `@` (and `@-`
    // for squash) regardless of the exit status.
    let conflicted_at = has_conflict_at(jj, cwd, "@").await;
    let conflicted_parent = squash && has_conflict_at(jj, cwd, "@-").await;
    let clean = !conflicted_at && !conflicted_parent && new_result.is_ok();

    // Roll back precisely; op-restore errors are best-effort.
    let _ = jj.op_restore(cwd, &pre_op).await;

    match new_result {
        Ok(_) => Ok(clean),
        Err(e) => Err(map_pk_err(e)),
    }
}

/// **Atomic merge** (jj implementation).
///
/// Caller has already moved `@` to the target via `checkout(target)`. We
/// incorporate `branch`'s content into `@` per the squash flag, and advance the
/// destination bookmark. Self-cleaning: captures the op id before any mutation
/// and `jj op restore`s on any internal step failure, so `merge()` is atomic
/// from the caller's view.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn merge(
    jj: &JjClient,
    cwd: &Path,
    branch: &str,
    dest_bookmark: &str,
    squash: bool,
    _no_ff: bool, // jj has no FF/non-FF distinction — always explicit merge commits
    message: Option<&str>,
) -> Result<()> {
    // No-op pre-flight: if branch has no commits @ lacks, nothing to merge.
    if super::branch::commit_count_via_revset(jj, cwd, &format!("({branch}) ~ ancestors(@)")).await?
        == 0
    {
        return Ok(());
    }

    // The destination bookmark is passed EXPLICITLY by the caller — we must NOT
    // re-derive it from `@` (a commit can carry several bookmarks; picking one
    // and `--allow-backwards`-moving it could drag the wrong bookmark backward).
    let pre_op = capture_op_id(jj, cwd).await?;
    let msg = message.unwrap_or("(merge)");

    let attempt: Result<()> = async {
        if squash {
            jj.new_merge(cwd, msg, vec!["@".into(), branch.into()]).await.map_err(map_pk_err)?;
            jj.squash_into(cwd, "@-").await.map_err(map_pk_err)?;
            // After squash, the description on @- is <msg>; @ is a new empty
            // change above it. Advance the bookmark forward.
            jj.bookmark_move(cwd, dest_bookmark, "@-", true).await.map_err(map_pk_err)?;
        } else {
            jj.new_merge(cwd, msg, vec!["@".into(), branch.into()]).await.map_err(map_pk_err)?;
            jj.bookmark_move(cwd, dest_bookmark, "@", true).await.map_err(map_pk_err)?;
        }
        Ok(())
    }
    .await;

    if let Err(e) = attempt {
        // Best-effort rollback to pre-merge state; surface the original error.
        let _ = jj.op_restore(cwd, &pre_op).await;
        return Err(e);
    }
    Ok(())
}

/// `reset_merge` in jj terms — undo the most recent operation (`jj op undo`). jj
/// has no in-progress merge state; this is the cleanup analogue for `merge.rs`
/// paths that haven't captured an op id.
pub(crate) async fn reset_merge(cwd: &Path) -> Result<()> {
    super::exec(cwd, ["op", "undo"]).await
}
