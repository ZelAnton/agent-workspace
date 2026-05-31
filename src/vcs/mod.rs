// ===========================================================================
// vcs - VCS abstraction layer (git + jj)
// ===========================================================================
//
// **Module layout**:
//   - `backend.rs` — the `VcsBackend` trait every implementation satisfies.
//   - `common.rs`  — backend-agnostic DTOs (`WorktreeInfo`, `DiffStat`) +
//                    the `path_str` UTF-8 helper.
//   - `error.rs`   — the shared `Error`/`Result` type.
//   - `git/`       — `GitBackend`, the only real implementation today.
//   - `jj/`        — `JjBackend`, currently `unimplemented!()` stubs.
//
// **How callers use this module**:
// Don't import `Backend`/`VcsBackend` directly unless you're constructing
// a backend (e.g. in tests). The intended surface is the [`Repo`] context
// (`src/vcs/repo.rs`): `Cli::run` resolves a backend once via
// [`resolve_backend`], wraps it in a `Repo` pinned to the process cwd, and
// passes `&Repo` into every command. That keeps call sites in
// `src/cli/commands/` from caring which backend is in use, with no reliance
// on the process cwd or any thread-local state.

pub mod backend;
pub mod common;
pub mod error;
pub mod git;
pub mod jj;
pub mod repo;

use std::path::Path;

pub use backend::VcsBackend;
pub use repo::Repo;
pub use common::{path_str, CreateOutcome, DiffStat, WorktreeInfo};
pub use error::{Error, Result};
// Pure git helpers re-exported at this level so call sites can write
// `vcs::is_cwd_inside(...)`, `vcs::parse_worktree_list(...)`, etc. without
// reaching into `vcs::git`. They're git-specific in implementation but
// don't actually shell out — they operate on already-captured text or the
// local filesystem.
pub use git::{clean_git_error, is_cwd_inside, parse_shortstat, parse_worktree_list};

// ---------------------------------------------------------------------------
// Backend tag + dispatch
// ---------------------------------------------------------------------------

/// Which VCS implementation is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Git,
    Jj,
}

impl Backend {
    /// Short string form — `"git"` or `"jj"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::Git => "git",
            Backend::Jj => "jj",
        }
    }

    /// Map a vcs-runner detection result to our `Backend`. **Colocated maps
    /// to `Jj`** by policy decision — the user installed jj for a reason,
    /// and the jj impl will eventually drive the colocated git layer where
    /// needed. Override via `--vcs=git` or `[general] vcs = "git"`.
    fn from_detected(detected: vcs_runner::VcsBackend) -> Self {
        if detected.is_jj() { Backend::Jj } else { Backend::Git }
    }
}

/// User-facing VCS selection — `--vcs <choice>` CLI flag + `[general] vcs`
/// config field. `Auto` triggers detection; the others force the choice
/// regardless of what's on disk (handy for testing or when detection picks
/// the "wrong" backend in an unusual repo layout).
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize, clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
pub enum VcsChoice {
    #[default]
    Auto,
    Git,
    Jj,
}

impl VcsChoice {
    /// Resolve to a concrete `Backend`, or `None` for `Auto` (caller
    /// continues down the precedence chain).
    fn resolve(self) -> Option<Backend> {
        match self {
            VcsChoice::Auto => None,
            VcsChoice::Git => Some(Backend::Git),
            VcsChoice::Jj => Some(Backend::Jj),
        }
    }
}

/// Resolve which backend to install given the configured choices.
///
/// **Precedence** (first non-`Auto` wins):
///   1. `cli_choice` — explicit `--vcs=...` on the command line.
///   2. `project_choice` — `[general] vcs` in `.workspace.toml` (legacy
///      `.agent-workspace.toml` as a fallback).
///   3. `global_choice` — `[general] vcs` in `~/.agent-workspace/config.toml`.
///   4. `vcs_runner::detect_vcs(cwd)` — colocated → `Jj`, jj-only → `Jj`,
///      git-only → `Git`.
///   5. Hard fallback: `Git`. Preserves behaviour for repos that
///      `detect_vcs` can't classify (e.g. running `ws setup` outside any
///      repo) — the resulting `GitBackend` will surface `NotInRepo` when
///      it tries to actually do anything.
pub fn resolve_backend(
    cli_choice: VcsChoice,
    project_choice: Option<VcsChoice>,
    global_choice: Option<VcsChoice>,
) -> Box<dyn VcsBackend> {
    let backend = cli_choice
        .resolve()
        .or_else(|| project_choice.and_then(|c| c.resolve()))
        .or_else(|| global_choice.and_then(|c| c.resolve()))
        .or_else(|| {
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            vcs_runner::detect_vcs(&cwd).ok().map(|(b, _)| Backend::from_detected(b))
        })
        .unwrap_or(Backend::Git);

    match backend {
        Backend::Git => Box::new(git::GitBackend::new()),
        Backend::Jj => Box::new(jj::JjBackend::new()),
    }
}

/// Step the ws PROCESS out of `doomed` (into `escape_to`) before deleting or
/// moving `doomed`. REQUIRED on Windows: the OS holds an exclusive handle on
/// every process's current directory, so `git worktree remove`/`move` of the
/// dir we're standing in fails "Permission denied" regardless of the git
/// subprocess's own cwd. No-op when we're not inside `doomed`. This is the
/// ONE sanctioned process-cwd mutation — it is NOT steering (which is now
/// done with explicit `Repo` handles).
pub fn step_out_of(doomed: &Path, escape_to: &Path) -> std::io::Result<()> {
    if is_cwd_inside(doomed) {
        std::env::set_current_dir(escape_to)?;
    }
    Ok(())
}

/// Shared cwd-serialization mutex for backend test suites.
///
/// `std::env::current_dir()` is process-global; any test that calls
/// `set_current_dir` must hold this mutex for the duration. Both
/// `src/vcs/git/tests` and `src/vcs/jj/tests` lock the **same** mutex so
/// concurrent jj + git tests don't trample each other.
#[cfg(test)]
pub(crate) static CWD_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod resolve_tests {
    //! Precedence tests for `resolve_backend`. These check the priority
    //! chain only — they don't validate `detect_vcs` behaviour (that's
    //! tested in the vcs-runner crate itself).

    use super::*;

    #[test]
    fn cli_choice_wins_over_project() {
        // Explicit CLI override beats project config.
        let backend = resolve_backend(VcsChoice::Git, Some(VcsChoice::Jj), None);
        assert_eq!(backend.name(), "git");
    }

    #[test]
    fn project_wins_over_global() {
        // Project config beats global config when CLI is auto.
        let backend = resolve_backend(VcsChoice::Auto, Some(VcsChoice::Jj), Some(VcsChoice::Git));
        assert_eq!(backend.name(), "jj");
    }

    #[test]
    fn global_wins_when_project_absent() {
        let backend = resolve_backend(VcsChoice::Auto, None, Some(VcsChoice::Jj));
        assert_eq!(backend.name(), "jj");
    }

    #[test]
    fn project_auto_falls_through_to_global() {
        // Project value of `Auto` is the same as `None` from the precedence
        // chain's point of view — it doesn't satisfy a non-Auto choice.
        let backend = resolve_backend(
            VcsChoice::Auto,
            Some(VcsChoice::Auto),
            Some(VcsChoice::Jj),
        );
        assert_eq!(backend.name(), "jj");
    }

    #[test]
    fn explicit_jj_installs_jj_backend() {
        let backend = resolve_backend(VcsChoice::Jj, None, None);
        assert_eq!(backend.name(), "jj");
    }
}
