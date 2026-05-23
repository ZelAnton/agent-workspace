// ===========================================================================
// vcs/jj/errmap - vcs_runner::RunError → vcs::Error mapping for jj
// ===========================================================================
//
// jj's error output is more structured than git's (no "fatal:"/"error:"
// prefixes to strip) but we still want to surface a clean single-line
// message to the user. Inner stderr trimmed; stdout fallback preserved
// for symmetry with git (some jj commands print informational text to
// stdout even on error — e.g. `jj edit` on a missing revset).

use vcs_runner::RunError;

use crate::vcs::error::Error;

/// Map a vcs-runner subprocess error to our domain [`Error`].
pub(super) fn map_run_err(err: RunError) -> Error {
    match err {
        RunError::NonZeroExit { stderr, stdout, .. } => {
            Error::Command(extract_message(&stderr, &stdout))
        }
        // Spawn / Timeout / Cancelled — pass through the Display message.
        other => Error::Command(other.to_string()),
    }
}

/// Build a user-friendly message from jj's stderr/stdout streams.
/// Prefers stderr; falls back to stdout when stderr is empty.
pub(super) fn extract_message(stderr: &str, stdout: &[u8]) -> String {
    let trimmed = stderr.trim();
    if trimmed.is_empty() {
        String::from_utf8_lossy(stdout).trim().to_string()
    } else {
        // jj sometimes prefixes with "Error: " — strip for cleaner UX.
        // Don't strip "Warning:" — those are non-fatal and worth keeping.
        trimmed
            .strip_prefix("Error: ")
            .unwrap_or(trimmed)
            .to_string()
    }
}
