// ===========================================================================
// vcs/jj/ops - Mutations (merge/rebase/checkout/commit/fetch)
// ===========================================================================
//
// **Semantic deltas vs git** (locked decisions + jj's model):
//   - `merge` is atomic in jj: every `jj new` records conflicts into the
//     resulting commit rather than failing. We materialise the merge, then
//     `jj op restore <pre-op-id>` on conflict to keep the same "main repo
//     never silently changes on failure" invariant `ws merge` relies on.
//   - `*_abort` / `*_continue` return `Error::Unsupported` per locked
//     decision: jj has no in-progress state to abort or continue from.
//   - `commit(message)` is `jj describe -m` (sets description on `@`, which
//     is always "the commit"). There's no separate "create commit" step.
//   - `fetch` uses `jj git fetch` with retry on transient errors (DNS /
//     connection / EOF) via `vcs_runner::is_transient_error`.

use std::path::Path;

use vcs_runner::{is_transient_error, Cmd, RetryPolicy, RunError, Runner};

use super::errmap::map_run_err;
use crate::vcs::error::{Error, Result};

/// `jj rebase -d <onto>` — rebase `@` onto the given revision.
///
/// jj's rebase is atomic — conflicts are recorded into the rebased
/// commits rather than blocking the operation. Caller should call
/// `is_merge_in_progress()` (== "jj st shows conflicts") afterward to
/// detect that case.
pub(super) fn rebase(runner: &dyn Runner, cwd: &Path, onto: &str) -> Result<()> {
    super::exec(runner, cwd, &["rebase", "-d", onto])
}

/// `jj edit <branch>` — move `@` to the named bookmark's commit.
///
/// Pre-checks that the bookmark exists (jj's `edit` accepts any revset
/// that resolves to a commit; we want the stricter "named bookmark"
/// semantic that matches git's `checkout` behaviour with our locked
/// decision that managed worktrees always have bookmarks).
pub(super) fn checkout(runner: &dyn Runner, cwd: &Path, branch: &str) -> Result<()> {
    if !super::repo::branch_exists(runner, cwd, branch)? {
        return Err(Error::BranchNotFound(branch.to_string()));
    }
    super::exec(runner, cwd, &["edit", branch])
}

/// `jj describe -m <message>` — set description on `@`.
///
/// In jj's model, this is the closest analogue to git's `commit -m`: the
/// working-copy commit always exists, and `describe` is how you attach a
/// message to it. Use after a squash-merge atomicity primitive that
/// materialises a commit without a description.
pub(super) fn commit(runner: &dyn Runner, cwd: &Path, message: &str) -> Result<()> {
    super::exec(runner, cwd, &["describe", "-m", message])
}

/// `jj git fetch` with retry on transient network errors.
///
/// Uses `vcs_runner::is_transient_error` as the retry predicate — same
/// shape as `GitBackend::fetch` would after the suggested unification in
/// the plan. Non-zero exits that don't match the transient predicate are
/// **silently swallowed** to match git's "fetch failing is often not
/// critical" behaviour. Spawn errors still propagate.
pub(super) fn fetch(runner: &dyn Runner, cwd: &Path) -> Result<()> {
    let policy = RetryPolicy::default().when(is_transient_error);
    match runner.run(
        Cmd::new("jj")
            .in_dir(cwd)
            .args(["git", "fetch"])
            .retry(policy),
    ) {
        Ok(_) | Err(RunError::NonZeroExit { .. }) => Ok(()),
        Err(e) => Err(map_run_err(e)),
    }
}

/// Fetch a SINGLE bookmark from `origin` (`jj git fetch --remote origin -b
/// <branch>`). Targeted — not the whole remote — so it stays cheap on
/// large repos. **Hard-fails** on error (unlike best-effort [`fetch`]):
/// callers reach it only after `remote_branch_exists` confirmed the
/// bookmark is there, so a failure here is real. Transient network blips
/// retry first.
pub(super) fn fetch_remote_branch(runner: &dyn Runner, cwd: &Path, branch: &str) -> Result<()> {
    let policy = RetryPolicy::default().when(is_transient_error);
    runner
        .run(
            Cmd::new("jj")
                .in_dir(cwd)
                .args(["git", "fetch", "--remote", "origin", "-b", branch])
                .retry(policy),
        )
        .map(|_| ())
        .map_err(map_run_err)
}

// ---------------------------------------------------------------------------
// Merge primitives
// ---------------------------------------------------------------------------

/// Capture the current operation id for later `jj op restore`-based rollback.
///
/// Returns the short id of the latest operation. Used by `dry_run_merge`
/// and `merge` to pin the rollback target precisely (more reliable than
/// `jj op undo`, which only walks back one op and can lose track if a
/// snapshot op slipped in).
pub(super) fn capture_op_id(runner: &dyn Runner, cwd: &Path) -> Result<String> {
    let out = runner
        .run(
            Cmd::new("jj").in_dir(cwd).args([
                "op",
                "log",
                "-T",
                r#"self.id().short()"#,
                "--no-graph",
                "--limit",
                "1",
            ]),
        )
        .map_err(map_run_err)?;
    Ok(out.stdout_lossy().trim().to_string())
}

/// True iff the commit at `revset` has conflicts.
fn has_conflict_at(runner: &dyn Runner, cwd: &Path, revset: &str) -> bool {
    let Ok(out) = runner.run(Cmd::new("jj").in_dir(cwd).args([
        "log",
        "-r",
        revset,
        "-T",
        r#"if(conflict, "1", "0")"#,
        "--no-graph",
        "--limit",
        "1",
    ])) else {
        return false;
    };
    out.stdout_lossy().trim() == "1"
}

/// **Dry-run merge.** Materialise the merge, check for conflicts, restore
/// to the pre-merge op state.
///
/// **Approach** (Option A from the plan, with op-id capture for precise
/// rollback):
///   1. Capture pre-op id via `jj op log -T self.id().short() --limit 1`.
///   2. Run the real merge (`jj new -m DRYRUN @ <branch>`).
///   3. Check `@` (and `@-` for squash) for conflicts.
///   4. `jj op restore <pre-op-id>` — restores to exact pre-merge state
///      including any concurrent snapshots that snuck in.
///
/// **Transient state window**: between step 2 and step 4 (typically <10ms),
/// a concurrent reader sees the merge commit on `@`. Acceptable per the
/// plan — documented in AGENTS.md.
///
/// Returns `Ok(true)` if merge would be clean, `Ok(false)` on conflicts.
pub(super) fn dry_run_merge(runner: &dyn Runner, cwd: &Path, branch: &str, squash: bool) -> Result<bool> {
    let pre_op = capture_op_id(runner, cwd)?;

    // Pre-flight: already up to date?
    if super::branch::commit_count_via_revset(
        runner,
        cwd,
        &format!("({branch}) ~ ancestors(@)"),
    )? == 0
    {
        return Ok(true); // clean, no-op merge
    }

    // Materialise the merge. Use a sentinel description so we can identify
    // the dry-run op in audits if someone reads `jj op log`.
    let new_result = super::exec(runner, cwd, &["new", "-m", "WT-DRY-RUN", "@", branch]);

    // Whether or not `jj new` itself errored, attempt rollback before
    // returning. If the new command produced a conflicted commit, jj returns
    // 0 exit but `jj log -r @ -T conflict` says "true".
    let conflicted_at = has_conflict_at(runner, cwd, "@");
    let conflicted_parent = squash && has_conflict_at(runner, cwd, "@-");
    let clean = !conflicted_at && !conflicted_parent && new_result.is_ok();

    // Roll back precisely. `jj op restore` errors are best-effort logged.
    let _ = super::exec(runner, cwd, &["op", "restore", &pre_op]);

    match new_result {
        Ok(_) => Ok(clean),
        // If jj new failed for a non-conflict reason, surface the error.
        Err(e) => Err(e),
    }
}

/// **Atomic merge** (jj implementation).
///
/// Caller has already moved `@` to the target via `vcs::checkout(target)`.
/// We need `branch`'s content incorporated into `@` per the squash flag,
/// and the target bookmark advanced to reflect the new commit.
///
/// For squash:
///   1. `jj new -m <msg> @ <branch>` — create merge commit `@'`
///   2. `jj squash --into @-` — collapse `@'`'s content into `@-` (target)
///   3. `jj bookmark move <target> --to @-` — advance bookmark to the
///      now-combined commit. (jj sometimes auto-advances via change_id,
///      sometimes doesn't; explicit move is always safe.)
///
/// For non-squash (merge commit):
///   1. `jj new -m <msg> @ <branch>` — create merge commit `@'` with both
///      target and branch as parents
///   2. `jj bookmark move <target> --to @` — advance bookmark to the merge
///      commit
///
/// **Self-cleaning** (review fix #2): captures the op id before any
/// mutation and `jj op restore`s on any internal step failure. Without
/// this, a failure between steps 1 and 2 (squash) would leave the merge
/// commit on `@` with no bookmark advance — and merge.rs's `reset_merge`
/// fallback is `jj op undo`, which only walks back one op and can't fully
/// recover. Self-cleaning makes merge() atomic from the caller's view.
///
/// **No-op detection** (review fix #4): if `branch` has no commits the
/// current `@` lacks AND the content diff is empty, returns Ok(()) without
/// creating a degenerate merge commit. This is the symmetric guard to
/// `execute_merge`'s pre-check — needed here too because `ws sync
/// --strategy=merge` and other callers reach `merge()` without the
/// command-layer pre-check.
pub(super) fn merge(
    runner: &dyn Runner,
    cwd: &Path,
    branch: &str,
    dest_bookmark: &str,
    squash: bool,
    _no_ff: bool, // jj has no FF/non-FF distinction — always explicit merge commits
    message: Option<&str>,
) -> Result<()> {
    // No-op pre-flight: if branch has no commits @ lacks, nothing to merge.
    // Avoids degenerate "merge commit with already-ancestor parent" output
    // when `ws sync` calls merge() on an up-to-date worktree.
    if super::branch::commit_count_via_revset(
        runner,
        cwd,
        &format!("({branch}) ~ ancestors(@)"),
    )? == 0
    {
        return Ok(());
    }

    // The destination bookmark to advance is passed EXPLICITLY by the caller
    // (the branch it checked out / is standing in). We must NOT re-derive it
    // from `@` via current_branch(): a commit can carry multiple bookmarks
    // (e.g. trunk co-located as `main` + `master`), and picking the
    // lexicographically smallest could move the WRONG bookmark — and, with
    // `--allow-backwards` below, drag it backward, losing its reference.
    let target_bookmark = dest_bookmark;

    // Capture pre-merge op id for precise rollback on internal failure.
    // Critical: op_id capture goes here, BEFORE any mutation, so a failure
    // at any subsequent step can restore the full pre-merge state in one
    // op_restore — far more reliable than jj op undo's "last op only".
    let pre_op = capture_op_id(runner, cwd)?;

    let msg = message.unwrap_or("(merge)");

    let attempt: Result<()> = (|| {
        if squash {
            super::exec(runner, cwd, &["new", "-m", msg, "@", branch])?;
            super::exec(runner, cwd, &["squash", "--into", "@-"])?;
            // After squash, the description on @- is the user's <msg>; @ is a
            // new empty change above it. Move the bookmark forward.
            super::exec(
                runner,
                cwd,
                &["bookmark", "move", target_bookmark, "--to", "@-", "--allow-backwards"],
            )?;
        } else {
            super::exec(runner, cwd, &["new", "-m", msg, "@", branch])?;
            super::exec(
                runner,
                cwd,
                &["bookmark", "move", target_bookmark, "--to", "@", "--allow-backwards"],
            )?;
        }
        Ok(())
    })();

    if let Err(e) = attempt {
        // Best-effort rollback to pre-merge state. Errors from op restore
        // are intentionally ignored — bubbling them would mask the original
        // error the user needs to see.
        let _ = super::exec(runner, cwd, &["op", "restore", &pre_op]);
        return Err(e);
    }
    Ok(())
}

/// `reset_merge` in jj terms — restore to before the most recent merge.
///
/// jj has no in-progress merge state, so the closest analogue is undoing
/// the last operation. We use `jj op undo` which is precisely "undo the
/// most recent op". Less precise than the op-id capture in [`dry_run_merge`]
/// but adequate for `merge.rs` cleanup paths that haven't captured an id.
pub(super) fn reset_merge(runner: &dyn Runner, cwd: &Path) -> Result<()> {
    super::exec(runner, cwd, &["op", "undo"])
}
