// ===========================================================================
// vcs/backend_state - Shared state for the per-tool VcsBackend wrappers
// ===========================================================================
//
// `GitBackend` and `JjBackend` are identical in shape: each holds an
// `Arc<TypedClient>` plus an optional pinned working directory, and each
// repeats the same `new`/`with_client`/`at`/`dir`/`client` boilerplate. This
// generic factors that out so each backend keeps only its tool-specific bits
// (the concrete client type + the trait method delegations). The accessors
// (`client()`, `dir()`) are kept thin so the backends can expose their own
// `git()`/`jj()` wrappers and the ~30 trait-method call sites stay untouched.

use std::path::PathBuf;
use std::sync::Arc;

/// Shared client + working-directory state for a [`VcsBackend`](super::backend::VcsBackend)
/// implementation. `C` is the concrete typed CLI client (`vcs_git::Git<_>` /
/// `vcs_jj::Jj<_>`); the `Arc` is shared cheaply when a backend is re-anchored
/// at a different directory via [`pinned`](Self::pinned).
pub(super) struct BackendState<C> {
    client: Arc<C>,
    /// Explicit working directory for every invocation. `None` means "read the
    /// live process cwd" — preserving the behaviour where helpers called
    /// `std::env::current_dir()` directly.
    cwd: Option<PathBuf>,
}

impl<C> BackendState<C> {
    /// New state owning `client`, reading the live process cwd.
    pub(super) fn new(client: Arc<C>) -> Self {
        Self { client, cwd: None }
    }

    /// New state owning `client`, pinned to an explicit `cwd`. Only the
    /// test-only `at()` backend constructors use this.
    #[cfg(test)]
    pub(super) fn with_cwd(client: Arc<C>, cwd: PathBuf) -> Self {
        Self { client, cwd: Some(cwd) }
    }

    /// A clone of this state sharing the SAME `Arc<C>` (no new subprocess /
    /// runner) but pinned to `cwd`. Backs `VcsBackend::at_cwd`.
    pub(super) fn pinned(&self, cwd: PathBuf) -> Self {
        Self { client: self.client.clone(), cwd: Some(cwd) }
    }

    /// The shared typed client.
    pub(super) fn client(&self) -> &C {
        &self.client
    }

    /// Resolve the working directory for an invocation: the pinned path, or the
    /// live process cwd when unpinned.
    pub(super) fn dir(&self) -> std::io::Result<PathBuf> {
        match &self.cwd {
            Some(d) => Ok(d.clone()),
            None => std::env::current_dir(),
        }
    }
}
