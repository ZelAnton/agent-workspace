// ===========================================================================
// vcs/error - Shared error type across all backends
// ===========================================================================
//
// Lifted from the former `git::Error`. Renamed `NotInRepo` message to be
// backend-neutral ("version-controlled repository") so jj-only repos surface
// the same error variant when no backend can claim them.

/// Result alias used by every VCS-touching call.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors returned by any [`VcsBackend`](super::backend::VcsBackend) method.
///
/// `Command(String)` is the catch-all for backend CLI failures — the inner
/// string is the post-processed user-facing message (already stripped of
/// `fatal:`/`error:` prefixes for git).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// CLI returned non-zero; payload is already cleaned for display.
    #[error("{0}")]
    Command(String),

    /// `wt` was invoked outside any recognized repository.
    #[error("not in a version-controlled repository")]
    NotInRepo,

    /// Operation requires a backend that isn't implemented yet (e.g. jj
    /// methods that still live as stubs in `JjBackend`).
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
}
