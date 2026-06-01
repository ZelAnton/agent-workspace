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

use crate::vcs::error::Error;

/// Map any `processkit` subprocess error to our domain [`Error`].
///
/// `Exit` gets the `clean_git_error` treatment on its captured stderr;
/// everything else (spawn failures, timeouts, parse, IO) is stringified
/// through its `Display` impl — the same user-facing shape the old
/// `vcs_runner::RunError` path produced.
pub(super) fn map_pk_err(err: PkError) -> Error {
    match err {
        PkError::Exit { stderr, .. } => Error::Command(clean_git_error(&stderr)),
        // Spawn / Timeout / Parse / Io — pass through the Display message.
        other => Error::Command(other.to_string()),
    }
}

/// Build a user-friendly message from the split stderr/stdout streams of a
/// captured [`ProcessResult`](processkit::ProcessResult). Prefers stderr;
/// falls back to stdout when stderr is empty or whitespace — that's where
/// `git merge` puts `CONFLICT (content): …` and `git commit` puts
/// `nothing to commit, working tree clean`.
pub(super) fn extract_message(stderr: &str, stdout: &[u8]) -> String {
    if stderr.trim().is_empty() {
        String::from_utf8_lossy(stdout).trim().to_string()
    } else {
        clean_git_error(stderr)
    }
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
