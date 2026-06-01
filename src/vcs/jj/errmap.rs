// ===========================================================================
// vcs/jj/errmap - processkit::Error → vcs::Error mapping for jj
// ===========================================================================
//
// jj's error output is more structured than git's (it prefixes fatal messages
// with `Error: ` rather than git's `fatal:`/`error:`). As with the git backend,
// `processkit::Error::Exit` carries only stderr, so the stderr/stdout fallback
// extraction can only be applied where WE capture the raw `ProcessResult`
// (the `exec` helper in `mod.rs`); `map_pk_err` is the fallback for typed
// `vcs-jj` calls.

use processkit::Error as PkError;

use crate::vcs::error::Error;

/// Map any `processkit` subprocess error to our domain [`Error`].
pub(super) fn map_pk_err(err: PkError) -> Error {
    match err {
        PkError::Exit { stderr, .. } => Error::Command(clean_jj_error(&stderr)),
        other => Error::Command(other.to_string()),
    }
}

/// Build a user-friendly message from jj's stderr/stdout streams. Prefers
/// stderr; falls back to stdout when stderr is empty (some jj commands print
/// informational text to stdout even on error).
pub(super) fn extract_message(stderr: &str, stdout: &[u8]) -> String {
    let trimmed = stderr.trim();
    if trimmed.is_empty() {
        String::from_utf8_lossy(stdout).trim().to_string()
    } else {
        clean_jj_error(trimmed)
    }
}

/// Strip jj's `Error: ` prefix for cleaner UX. Keeps `Warning:` (non-fatal,
/// worth surfacing).
fn clean_jj_error(stderr: &str) -> String {
    let trimmed = stderr.trim();
    trimmed.strip_prefix("Error: ").unwrap_or(trimmed).to_string()
}
