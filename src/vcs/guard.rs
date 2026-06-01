// ===========================================================================
// vcs/guard - RAII cleanup for freshly-created worktrees
// ===========================================================================
//
// `ws new` creates a worktree, then runs several fallible setup steps (meta
// write, copy_files). If one of those returns early — or the process panics —
// the partially-created worktree would leak: a directory on disk and a git
// worktree registration with no usable metadata. Manual rollback at each `?`
// is easy to forget and impossible to cover for panics.
//
// `WorktreeGuard` replaces that with a scope guard: arm it right after creation,
// and the worktree is removed on drop until `keep()` is called once it's
// committed to. This is the RAII analogue of the explicit-rollback closures the
// CoW paths already use, extended to cover the whole setup window (and panics).

use std::path::{Path, PathBuf};

use super::repo::Repo;

/// Removes a freshly-created worktree on drop unless [`keep`](Self::keep)-ed.
///
/// Arm it immediately after [`Repo::create_worktree`](super::repo::Repo::create_worktree)
/// and call [`keep`](Self::keep) once the worktree is fully set up and should
/// survive. Any early return — or a panic — before `keep()` removes the partial
/// worktree instead of leaking it.
///
/// ```ignore
/// let outcome = repo.create_worktree(&path, &branch, &base)?;
/// let guard = repo.guard_worktree(&path); // armed
/// meta.save(&meta_path)?;                  // on error → worktree removed
/// copy_files(&repo_root, &path, config)?;  // on error → worktree removed
/// guard.keep();                            // committed: keep it from here on
/// ```
#[must_use = "bind the guard for the setup window, or call keep() to defuse it"]
pub struct WorktreeGuard<'a> {
    repo: &'a Repo,
    path: PathBuf,
    armed: bool,
}

impl<'a> WorktreeGuard<'a> {
    /// Arm a guard over `path`. Prefer [`Repo::guard_worktree`](super::repo::Repo::guard_worktree).
    pub(crate) fn new(repo: &'a Repo, path: impl Into<PathBuf>) -> Self {
        Self {
            repo,
            path: path.into(),
            armed: true,
        }
    }

    /// The worktree path under guard.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Defuse the guard: the worktree is kept when the guard drops.
    pub fn keep(mut self) {
        self.armed = false;
        // `self` drops here with `armed == false`, so [`Drop`] is a no-op.
    }
}

impl Drop for WorktreeGuard<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // Force-remove: a half-set-up worktree has the freshly-created branch
        // checked out, which a non-forced removal would refuse. `Drop` can't
        // `.await`, so this goes through the backend's SYNCHRONOUS blocking
        // cleanup (a plain subprocess), not the async `remove_worktree`. A
        // failure is surfaced as a warning rather than swallowed — the user may
        // need to `ws rm` or `git worktree prune`.
        if let Err(e) = self.repo.cleanup_worktree_blocking(&self.path) {
            eprintln!(
                "warning: could not clean up partial worktree at {}: {e}",
                self.path.display()
            );
        }
    }
}
