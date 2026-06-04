// ===========================================================================
// vcs/error - Shared error type across all backends
// ===========================================================================
//
// Lifted from the former `git::Error`. Renamed `NotInRepo` message to be
// backend-neutral ("version-controlled repository") so jj-only repos surface
// the same error variant when no backend can claim them.

/// Result alias used by every VCS-touching call.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors returned by any VCS backend operation (a [`Repo`](super::Repo) method
/// or the git/jj helpers behind it).
///
/// `Command(String)` is the catch-all for backend CLI failures — the inner
/// string is the post-processed user-facing message (already stripped of
/// `fatal:`/`error:` prefixes for git).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// CLI returned non-zero; payload is already cleaned for display.
    #[error("{0}")]
    Command(String),

    /// `ws` was invoked outside any recognized repository.
    #[error("not in a version-controlled repository")]
    NotInRepo,

    /// Operation has no analogue on the active backend (e.g. the jj
    /// `move_worktree` / `*_abort` / `*_continue` arms return this via
    /// `jj::unsupported`).
    #[error("operation not yet supported by this backend: {0}")]
    Unsupported(String),

    #[error("worktree '{0}' not found")]
    WorktreeNotFound(String),

    #[error("worktree '{0}' already exists")]
    WorktreeExists(String),

    #[error("branch '{0}' not found")]
    BranchNotFound(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// Copy-on-Write worktree creation failed during the reflink walk
    /// step. Preserves the structured `cow::Error` so callers and tests
    /// can match the underlying cause (io, walker) rather than parsing
    /// the formatted display string.
    #[error("cow copy: {0}")]
    Cow(#[from] crate::cow::Error),
}

// ---------------------------------------------------------------------------
// Shared processkit::Error → Error mapping
//
// Both backends fail with the same `processkit::Error`; only the prefix-
// cleanup function differs (`clean_git_error` vs `clean_jj_error`). These two
// helpers hold the identical fallback/dispatch logic so each `errmap.rs`
// shrinks to its cleanup fn + thin wrappers.
// ---------------------------------------------------------------------------

/// Build a user-friendly message from a captured subprocess's split streams.
/// Prefers stderr; falls back to stdout when stderr is blank — that's where
/// `git merge` puts `CONFLICT (content): …` and `git commit` puts `nothing to
/// commit`. `cleanup` is the backend's prefix stripper.
pub(crate) fn extract_message(stderr: &str, stdout: &[u8], cleanup: fn(&str) -> String) -> String {
    if stderr.trim().is_empty() {
        String::from_utf8_lossy(stdout).trim().to_string()
    } else {
        cleanup(stderr)
    }
}

/// Map a `processkit` subprocess error to [`Error::Command`]. `Exit` gets the
/// backend `cleanup` on its captured stderr (processkit drops stdout once a run
/// is judged a failure); every other variant is stringified via its `Display`.
pub(crate) fn map_pk_err(err: processkit::Error, cleanup: fn(&str) -> String) -> Error {
    match err {
        processkit::Error::Exit { stderr, .. } => Error::Command(cleanup(&stderr)),
        other => Error::Command(other.to_string()),
    }
}

/// Map a [`vcs_core`] facade error onto our domain [`Error`]. Used where the
/// `Repo` wrapper calls the facade's common-surface methods directly (trunk
/// resolution, blocking worktree cleanup) rather than going through a per-backend
/// helper. Repo-detection failures fold into [`Error::NotInRepo`]; the underlying
/// CLI error keeps its already-shaped `Display` (the facade itself strips noise).
impl From<vcs_core::Error> for Error {
    fn from(err: vcs_core::Error) -> Self {
        match err {
            vcs_core::Error::NotARepository(_) => Error::NotInRepo,
            vcs_core::Error::WorktreeNotFound(p) => {
                Error::WorktreeNotFound(p.display().to_string())
            }
            vcs_core::Error::Io(e) => Error::Io(e),
            other => Error::Command(other.to_string()),
        }
    }
}
