// ===========================================================================
// vcs - VCS abstraction layer (git + jj), built on the vcs-core facade
// ===========================================================================
//
// **Module layout**:
//   - `common.rs`  — `ws`'s DTOs (`WorktreeInfo`, `DiffStat`, `CreateOutcome`) +
//                    the `path_str` UTF-8 helper.
//   - `error.rs`   — the shared `Error`/`Result` type.
//   - `git/`, `jj/` — the per-backend helper functions. Pure query helpers take
//                    a cwd-bound view (`vcs_git::GitAt` / `vcs_jj::JjAt`); the
//                    remaining client+cwd ops (raw-command + state machines) use
//                    the escape hatches. The `vcs_core::Repo`-backed wrapper in
//                    `repo.rs` dispatches to them via `git_at`/`jj_at` and
//                    `git()`/`jj()`.
//   - `repo.rs`    — `Repo`, the single handle every command reaches a backend
//                    through.
//
// **How callers use this module**:
// `Cli::run` resolves a [`Repo`] once via [`resolve_backend`] (a `vcs_core::Repo`
// + `ws` policy veneer) and passes `&Repo` into every command, so call sites in
// `src/cli/commands/` don't care which backend is in use, with no reliance on
// the process cwd. Detection + the cwd-bound client come from `vcs-core`; the
// common-vs-divergent op routing lives in `repo.rs`.

pub mod common;
pub mod error;
pub mod git;
pub mod guard;
pub mod jj;
pub mod repo;

use std::path::Path;

use vcs_core::BackendKind;

pub use guard::WorktreeGuard;
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

/// Resolve and build the [`Repo`] handle for `cwd` given the configured choices.
///
/// **Precedence** (first non-`Auto` wins):
///   1. `cli_choice` — explicit `--vcs=...` on the command line.
///   2. `project_choice` — `[general] vcs` in `.workspace.toml` (legacy
///      `.agent-workspace.toml` as a fallback).
///   3. `global_choice` — `[general] vcs` in `~/.agent-workspace/config.toml`.
///   4. [`vcs_core::detect`] — a `.jj`/`.git` ancestor-walk: colocated → `Jj`,
///      jj-only → `Jj`, git-only → `Git`.
///   5. Hard fallback: `Git`. Preserves behaviour for repos that detection
///      can't classify (e.g. running `ws setup` outside any repo) — the
///      resulting git handle surfaces `NotInRepo` when it does anything.
///
/// A forced choice (1–3) bypasses detection and builds the requested backend
/// regardless of what's on disk. The `root` `ws` passes to the facade is the
/// detected root when available else `cwd` — `ws`'s own `repo_root()` re-resolves
/// it (via `--git-common-dir` / `jj workspace root`), so it's not load-bearing.
pub fn resolve_backend(
    cwd: &Path,
    cli_choice: VcsChoice,
    project_choice: Option<VcsChoice>,
    global_choice: Option<VcsChoice>,
) -> Repo {
    let forced = cli_choice
        .resolve()
        .or_else(|| project_choice.and_then(|c| c.resolve()))
        .or_else(|| global_choice.and_then(|c| c.resolve()));

    // Only probe the filesystem when no backend was forced — a forced choice
    // genuinely bypasses detection (and its `root` is inert; see below).
    let located = if forced.is_none() { vcs_core::detect(cwd) } else { None };
    let kind = forced
        .map(|b| match b {
            Backend::Git => BackendKind::Git,
            Backend::Jj => BackendKind::Jj,
        })
        .or_else(|| located.as_ref().map(|l| l.kind))
        .unwrap_or(BackendKind::Git);
    let root = located.map(|l| l.root).unwrap_or_else(|| cwd.to_path_buf());

    let inner = match kind {
        BackendKind::Jj => vcs_core::Repo::from_jj(root, cwd, vcs_jj::Jj::new()),
        // `BackendKind` is `#[non_exhaustive]`; anything not jj builds git.
        _ => vcs_core::Repo::from_git(root, cwd, vcs_git::Git::new()),
    };
    Repo::from_core(inner)
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

#[cfg(test)]
mod resolve_tests {
    //! Precedence tests for `resolve_backend`. These check the priority chain
    //! only — a forced (non-`Auto`) choice bypasses `vcs_core::detect`, so the
    //! `cwd` passed here need not be a real repo.

    use super::*;

    // Any path works: every test below forces a backend, so detection (which
    // would read this dir) is never consulted.
    fn cwd() -> &'static Path {
        Path::new(".")
    }

    #[test]
    fn cli_choice_wins_over_project() {
        // Explicit CLI override beats project config.
        let repo = resolve_backend(cwd(), VcsChoice::Git, Some(VcsChoice::Jj), None);
        assert_eq!(repo.backend_name(), "git");
    }

    #[test]
    fn project_wins_over_global() {
        // Project config beats global config when CLI is auto.
        let repo = resolve_backend(cwd(), VcsChoice::Auto, Some(VcsChoice::Jj), Some(VcsChoice::Git));
        assert_eq!(repo.backend_name(), "jj");
    }

    #[test]
    fn global_wins_when_project_absent() {
        let repo = resolve_backend(cwd(), VcsChoice::Auto, None, Some(VcsChoice::Jj));
        assert_eq!(repo.backend_name(), "jj");
    }

    #[test]
    fn project_auto_falls_through_to_global() {
        // Project value of `Auto` is the same as `None` from the precedence
        // chain's point of view — it doesn't satisfy a non-Auto choice.
        let repo = resolve_backend(cwd(), VcsChoice::Auto, Some(VcsChoice::Auto), Some(VcsChoice::Jj));
        assert_eq!(repo.backend_name(), "jj");
    }

    #[test]
    fn explicit_jj_installs_jj_backend() {
        let repo = resolve_backend(cwd(), VcsChoice::Jj, None, None);
        assert_eq!(repo.backend_name(), "jj");
    }
}
