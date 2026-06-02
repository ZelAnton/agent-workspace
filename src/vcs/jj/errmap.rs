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

use crate::vcs::error::{self, Error};

/// Map any `processkit` subprocess error to our domain [`Error`], stripping jj's
/// `Error: ` prefix off the captured stderr. Thin wrapper over the shared
/// [`error::map_pk_err`].
pub(super) fn map_pk_err(err: PkError) -> Error {
    error::map_pk_err(err, clean_jj_error)
}

/// Build a user-friendly message from jj's stderr/stdout streams — prefers
/// stderr, falls back to stdout (some jj commands print info to stdout even on
/// error). Thin wrapper over the shared [`error::extract_message`].
pub(super) fn extract_message(stderr: &str, stdout: &[u8]) -> String {
    error::extract_message(stderr, stdout, clean_jj_error)
}

/// Strip jj's `Error: ` prefix for cleaner UX. Keeps `Warning:` (non-fatal,
/// worth surfacing).
fn clean_jj_error(stderr: &str) -> String {
    let trimmed = stderr.trim();
    trimmed.strip_prefix("Error: ").unwrap_or(trimmed).to_string()
}
