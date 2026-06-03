// ===========================================================================
// vcs/git/ops - Merge / rebase / checkout / fetch + in-progress probes
// ===========================================================================

use std::path::Path;

use vcs_git::GitApi;

use super::GitClient;
use super::errmap::map_pk_err;
use crate::vcs::error::Result;

/// How many times a transient `git fetch` failure is retried before giving up.
const FETCH_MAX_ATTEMPTS: usize = 3;

/// Run `git merge` with the configured strategy.
///
/// **Squash + message handling**: `git merge --squash` stages but doesn't
/// commit, and the `-m` flag is incompatible with `--squash`. When called
/// with `squash=true` AND `message=Some(_)`, this does both steps: stage via
/// `git merge --squash`, then `git commit -m msg` (skipped if nothing was
/// actually staged, matching "already up to date" semantics).
pub(crate) async fn merge(
    cwd: &Path,
    branch: &str,
    squash: bool,
    no_ff: bool,
    message: Option<&str>,
) -> Result<()> {
    if squash {
        super::exec(cwd, ["merge", "--squash", branch]).await?;
        // If a message is provided, finalise the squash with a real commit.
        // The post-squash index may be empty when the branch was already
        // merged ("already up to date") — `git commit` would error with
        // "nothing to commit" in that case, so probe the index first.
        if let Some(msg) = message {
            // `diff --cached --quiet` exits 1 when there ARE staged changes
            // (`code()` is `Option` in processkit 0.5 — `None` if killed/timed
            // out; only an explicit exit 1 means "staged changes present").
            let probe = super::capture(cwd, ["diff", "--cached", "--quiet"]).await?;
            if probe.code() == Some(1) {
                super::exec(cwd, ["commit", "-m", msg]).await?;
            }
        }
        return Ok(());
    }

    // Non-squash path. `-m` is valid alongside `--no-ff`.
    let mut args: Vec<&str> = vec!["merge"];
    if no_ff {
        args.push("--no-ff");
    }
    if let Some(msg) = message {
        args.push("-m");
        args.push(msg);
    }
    args.push(branch);
    super::exec(cwd, args).await
}

/// Dry-run a merge to check for conflicts without leaving state.
///
/// Mirrors the real merge strategy: a `--no-ff` dry-run that passes can
/// still fail under `--squash` (different three-way ancestor). So we dry-run
/// with the **actual** flag. Returns `Ok(true)` if the merge would be clean.
pub(crate) async fn dry_run_merge(cwd: &Path, branch: &str, squash: bool) -> Result<bool> {
    let merge_args: Vec<&str> = if squash {
        vec!["merge", "--squash", "--no-commit", branch]
    } else {
        vec!["merge", "--no-commit", "--no-ff", branch]
    };

    let clean = super::capture(cwd, merge_args).await?.is_success();

    // Best-effort cleanup. `git merge --squash` never sets MERGE_HEAD so
    // `--abort` errors; `reset --hard HEAD` restores the index in that case.
    if squash {
        let _ = super::capture(cwd, ["reset", "--hard", "HEAD"]).await;
    } else {
        let _ = super::capture(cwd, ["merge", "--abort"]).await;
    }

    Ok(clean)
}

/// Run `git rebase <onto>`.
pub(crate) async fn rebase(cwd: &Path, onto: &str) -> Result<()> {
    super::exec(cwd, ["rebase", onto]).await
}

/// Run `git checkout <branch>`.
pub(crate) async fn checkout(cwd: &Path, branch: &str) -> Result<()> {
    super::exec(cwd, ["checkout", branch]).await
}

/// Fetch a SINGLE branch from `origin` into its remote-tracking ref.
///
/// **Targeted** fetch that **hard-fails** on error: callers reach it only after
/// `remote_branch_exists` already confirmed the branch is there, so a failure
/// here is real and actionable. Uses the typed client's `fetch_remote_branch`
/// (same refspec + `GIT_TERMINAL_PROMPT=0`); transient network blips — classified
/// by `vcs_git::is_transient_fetch_error` (DNS / connection / EOF / timeout) —
/// retry first.
pub(crate) async fn fetch_remote_branch(git: &GitClient, cwd: &Path, branch: &str) -> Result<()> {
    let mut attempt = 0;
    loop {
        attempt += 1;
        match git.fetch_remote_branch(cwd, branch).await {
            Ok(()) => return Ok(()),
            Err(e) if attempt < FETCH_MAX_ATTEMPTS && vcs_git::is_transient_fetch_error(&e) => {
                continue;
            }
            Err(e) => return Err(map_pk_err(e)),
        }
    }
}

/// Abort an in-progress rebase.
pub(crate) async fn rebase_abort(cwd: &Path) -> Result<()> {
    super::exec(cwd, ["rebase", "--abort"]).await
}

/// Continue an in-progress rebase after conflict resolution.
pub(crate) async fn rebase_continue(cwd: &Path) -> Result<()> {
    super::exec(cwd, ["rebase", "--continue"]).await
}

/// Abort an in-progress merge.
pub(crate) async fn merge_abort(cwd: &Path) -> Result<()> {
    super::exec(cwd, ["merge", "--abort"]).await
}

/// Reset index to HEAD, clearing any merge/squash conflict state. Unlike
/// `merge --abort`, this also works for `--squash` conflicts which don't
/// create MERGE_HEAD.
pub(crate) async fn reset_merge(cwd: &Path) -> Result<()> {
    super::exec(cwd, ["reset", "--merge"]).await
}

/// Continue an in-progress merge (after conflict resolution).
pub(crate) async fn merge_continue(cwd: &Path) -> Result<()> {
    super::exec(cwd, ["commit", "--no-edit"]).await
}

/// True iff a rebase is currently in progress (`.git/rebase-merge` or
/// `.git/rebase-apply`). Best-effort: a probe failure (outside a repo) reads
/// as "no".
pub(crate) async fn is_rebase_in_progress(git: &GitClient, cwd: &Path) -> bool {
    git.is_rebase_in_progress(cwd).await.unwrap_or(false)
}

/// True iff a merge is currently in progress (`.git/MERGE_HEAD`). Best-effort.
pub(crate) async fn is_merge_in_progress(git: &GitClient, cwd: &Path) -> bool {
    git.is_merge_in_progress(cwd).await.unwrap_or(false)
}
