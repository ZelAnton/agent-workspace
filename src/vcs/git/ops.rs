// ===========================================================================
// vcs/git/ops - Merge / rebase / checkout / fetch + in-progress probes
// ===========================================================================

use vcs_runner::{Cmd, RunError, Runner};

use super::errmap::map_run_err;
use crate::vcs::error::Result;

/// Run `git merge` with the configured strategy.
pub(super) fn merge(
    runner: &dyn Runner,
    branch: &str,
    squash: bool,
    no_ff: bool,
    message: Option<&str>,
) -> Result<()> {
    let mut args: Vec<&str> = vec!["merge"];
    if squash {
        args.push("--squash");
    }
    if no_ff {
        args.push("--no-ff");
    }
    if let Some(msg) = message {
        args.push("-m");
        args.push(msg);
    }
    args.push(branch);
    super::exec(runner, &args)
}

/// Dry-run a merge to check for conflicts without leaving state.
///
/// Mirrors the real merge strategy: a `--no-ff` dry-run that passes can
/// still fail under `--squash` (different three-way ancestor), silently
/// leaving the repo half-merged. So we dry-run with the **actual** flag.
///
/// Returns `Ok(true)` if the merge would be clean, `Ok(false)` on conflict.
pub(super) fn dry_run_merge(runner: &dyn Runner, branch: &str, squash: bool) -> Result<bool> {
    let cwd = std::env::current_dir()?;
    let merge_args: &[&str] = if squash {
        &["merge", "--squash", "--no-commit", branch]
    } else {
        &["merge", "--no-commit", "--no-ff", branch]
    };

    let clean = match runner.run(Cmd::new("git").in_dir(&cwd).args(merge_args)) {
        Ok(_) => true,
        Err(RunError::NonZeroExit { .. }) => false,
        Err(e) => return Err(map_run_err(e)),
    };

    // Best-effort cleanup. `git merge --squash` never sets MERGE_HEAD so
    // `--abort` errors; reset --hard HEAD restores the index in that case.
    if squash {
        let _ = runner.run(Cmd::new("git").in_dir(&cwd).args(["reset", "--hard", "HEAD"]));
    } else {
        let _ = runner.run(Cmd::new("git").in_dir(&cwd).args(["merge", "--abort"]));
    }

    Ok(clean)
}

/// Run `git rebase <onto>`.
pub(super) fn rebase(runner: &dyn Runner, onto: &str) -> Result<()> {
    super::exec(runner, &["rebase", onto])
}

/// Run `git checkout <branch>`.
pub(super) fn checkout(runner: &dyn Runner, branch: &str) -> Result<()> {
    super::exec(runner, &["checkout", branch])
}

/// Commit currently staged changes.
pub(super) fn commit(runner: &dyn Runner, message: &str) -> Result<()> {
    super::exec(runner, &["commit", "-m", message])
}

/// Fetch from origin with retry on transient network errors.
///
/// **Non-zero exits are still silently swallowed** — fetch failing
/// (after all retries) is not critical for downstream commands. We retry
/// on a custom transient predicate matching DNS / connection / EOF
/// patterns: `vcs_runner::RetryPolicy::default()` ships a predicate that
/// only matches `"stale"`/`".lock"` (index-lock contention), which is the
/// wrong shape for `git fetch` failures. See [`is_transient_fetch_err`].
pub(super) fn fetch(runner: &dyn Runner) -> Result<()> {
    use vcs_runner::RetryPolicy;
    let cwd = std::env::current_dir()?;
    let policy = RetryPolicy::default().when(is_transient_fetch_err);
    match runner.run(
        Cmd::new("git")
            .in_dir(&cwd)
            .args(["fetch", "--quiet"])
            .retry(policy),
    ) {
        Ok(_) | Err(RunError::NonZeroExit { .. }) => Ok(()),
        Err(e) => Err(map_run_err(e)),
    }
}

/// True if `err` looks like a transient network failure that's worth
/// retrying. Matches stderr text against patterns git emits for DNS
/// failures, connection resets, and protocol-level early EOFs.
///
/// Kept `pub(crate)` so the unit test in `tests::mock_runner` can hit
/// the predicate directly without spinning up a real failed fetch.
pub(crate) fn is_transient_fetch_err(err: &RunError) -> bool {
    let RunError::NonZeroExit { stderr, .. } = err else {
        return false;
    };
    const TRANSIENT_MARKERS: &[&str] = &[
        "Could not resolve",        // DNS failure
        "Could not read from remote", // common pre-protocol failure
        "Connection",                // refused / reset / timed out
        "timed out",
        "early EOF",                // protocol cut short mid-transfer
        "the remote end hung up",
    ];
    TRANSIENT_MARKERS.iter().any(|m| stderr.contains(m))
}

/// Abort an in-progress rebase.
pub(super) fn rebase_abort(runner: &dyn Runner) -> Result<()> {
    super::exec(runner, &["rebase", "--abort"])
}

/// Continue an in-progress rebase after conflict resolution.
pub(super) fn rebase_continue(runner: &dyn Runner) -> Result<()> {
    super::exec(runner, &["rebase", "--continue"])
}

/// Abort an in-progress merge.
pub(super) fn merge_abort(runner: &dyn Runner) -> Result<()> {
    super::exec(runner, &["merge", "--abort"])
}

/// Reset index to HEAD, clearing any merge/squash conflict state.
///
/// Unlike `merge --abort`, this also works for `--squash` conflicts which
/// don't create MERGE_HEAD.
pub(super) fn reset_merge(runner: &dyn Runner) -> Result<()> {
    super::exec(runner, &["reset", "--merge"])
}

/// Continue an in-progress merge (after conflict resolution).
pub(super) fn merge_continue(runner: &dyn Runner) -> Result<()> {
    super::exec(runner, &["commit", "--no-edit"])
}

/// Get the git dir (`.git` or `.git/worktrees/<branch>`) for probing
/// in-progress state files. Returns `None` outside a repo so the
/// `is_*_in_progress` probes silently say "no" rather than erroring.
fn git_dir(runner: &dyn Runner) -> Option<std::path::PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    runner
        .run(Cmd::new("git").in_dir(&cwd).args(["rev-parse", "--git-dir"]))
        .ok()
        .map(|out| std::path::PathBuf::from(out.stdout_lossy().trim()))
}

/// True iff a rebase is currently in progress.
///
/// Git stashes rebase state in `.git/rebase-merge/` (interactive / merge
/// strategy) or `.git/rebase-apply/` (am-based rebase). jj has no
/// equivalent concept — `JjBackend::is_rebase_in_progress` returns `false`.
pub(super) fn is_rebase_in_progress(runner: &dyn Runner) -> bool {
    git_dir(runner).is_some_and(|d| {
        d.join("rebase-merge").exists() || d.join("rebase-apply").exists()
    })
}

/// True iff a merge is currently in progress (`.git/MERGE_HEAD` exists).
///
/// jj treats unresolved conflicts as committed state, not a transient
/// "merge in progress" status — the jj impl will read `jj st` for
/// unresolved conflicts instead.
pub(super) fn is_merge_in_progress(runner: &dyn Runner) -> bool {
    git_dir(runner).is_some_and(|d| d.join("MERGE_HEAD").exists())
}
