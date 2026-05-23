// ===========================================================================
// vcs/git/errmap - vcs_runner::RunError → vcs::Error mapping
// ===========================================================================
//
// vcs-runner returns `RunError::NonZeroExit { stderr: String, stdout: Vec<u8>, .. }`
// — the split that the original `std::process::Output`-based `extract_error` glued
// back together. The "prefer stderr, fall back to stdout" logic is load-bearing
// for friendly merge/commit error messages (git puts `CONFLICT (content): …`
// on stdout) so it's preserved here.

use vcs_runner::RunError;

use crate::vcs::error::Error;

/// Map any vcs-runner subprocess error to our domain [`Error`].
///
/// `NonZeroExit` gets the stderr/stdout extraction + `clean_git_error`
/// treatment; everything else (spawn failures, timeouts, cancellation) is
/// stringified through its `Display` impl.
pub(super) fn map_run_err(err: RunError) -> Error {
    match err {
        RunError::NonZeroExit { stderr, stdout, .. } => {
            Error::Command(extract_message(&stderr, &stdout))
        }
        // Spawn / Timeout / Cancelled — pass through the Display message.
        // The user-facing impact is identical to the old `output()?`
        // path which surfaced `std::io::Error`s via `Error::Io(...)`.
        other => Error::Command(other.to_string()),
    }
}

/// Build a user-friendly message from the split stderr/stdout streams
/// captured by vcs-runner. Prefers stderr; falls back to stdout when stderr
/// is empty or whitespace — that's where `git merge` puts `CONFLICT (content): …`
/// and `git commit` puts `nothing to commit, working tree clean`.
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
