// ===========================================================================
// vcs/git/errmap - processkit::Error → vcs::Error mapping
// ===========================================================================
//
// The vcs-git typed client and our own raw `processkit::Command` calls both
// fail with `processkit::Error`. `Exit { stderr, .. }` carries only stderr
// (processkit drops stdout once a run is judged a failure), so the
// "prefer stderr, fall back to stdout" extraction that's load-bearing for
// friendly merge/commit errors (git puts `CONFLICT (content): …` on stdout)
// can only be applied where WE capture the raw `ProcessResult` ourselves —
// see `exec`/`run_checked` in `mod.rs`, which pass both streams to
// [`extract_message`]. This module's [`map_pk_err`] is the fallback used for
// typed vcs-git calls, where only the structured `Exit` is available.

use processkit::Error as PkError;

use crate::vcs::error::{self, Error};

/// Map any `processkit` subprocess error to our domain [`Error`], cleaning git's
/// `fatal:`/`error:` prefixes off the captured stderr. Thin wrapper over the
/// shared [`error::map_pk_err`].
pub(super) fn map_pk_err(err: PkError) -> Error {
    error::map_pk_err(err, clean_git_error)
}

/// Build a user-friendly message from the split stderr/stdout streams of a
/// captured [`ProcessResult`](processkit::ProcessResult) — prefers stderr, falls
/// back to stdout (where `git merge` puts `CONFLICT (content): …`). Thin wrapper
/// over the shared [`error::extract_message`].
pub(super) fn extract_message(stderr: &str, stdout: &[u8]) -> String {
    error::extract_message(stderr, stdout, clean_git_error)
}

/// Strip git's `fatal:`/`error:` prefixes and rewrite the
/// "worktree contains modified or untracked files" message into a shorter
/// "worktree '<branch>' has uncommitted changes, use --force" form.
///
/// Kept `pub` so the unit tests in `src/vcs/git/tests/` can assert
/// against it directly without going through the trait, and so external
/// debugging callers can pre-clean a stderr string.
pub fn clean_git_error(stderr: &str) -> String {
    let msg = stderr.trim();

    let msg = msg
        .strip_prefix("fatal: ")
        .or_else(|| msg.strip_prefix("error: "))
        .unwrap_or(msg);

    // "'/path/to/branch' contains modified or untracked files, use --force to delete it"
    if msg.contains("contains modified or untracked files")
        && let Some(start) = msg.find('\'')
        && let Some(end) = msg[start + 1..].find('\'')
    {
        let path = &msg[start + 1..start + 1 + end];
        let branch = path.rsplit('/').next().unwrap_or(path);
        return format!("worktree '{branch}' has uncommitted changes, use --force");
    }

    msg.to_string()
}
