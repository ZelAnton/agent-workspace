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
// or replacing the active backend. The intended surface is the free
// functions at the bottom of this file — `vcs::repo_root()`,
// `vcs::create_worktree(...)`, etc. — which forward to the active backend
// via a thread-local. That keeps call sites in `src/cli/commands/` from
// caring which backend is in use, and matches the pre-refactor `git::*`
// shape so the migration is a pure rename.

pub mod backend;
pub mod common;
pub mod error;
pub mod git;
pub mod jj;

use std::cell::RefCell;
use std::path::{Path, PathBuf};

pub use backend::VcsBackend;
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
///   2. `project_choice` — `[general] vcs` in `.agent-workspace.toml`.
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

thread_local! {
    static BACKEND: RefCell<Option<Box<dyn VcsBackend>>> = const { RefCell::new(None) };
}

/// Install a backend for this thread. Production code calls this once
/// from `Cli::run` after argument parsing + `resolve_backend(...)`; tests
/// call it inside their cwd helper. Replaces any previously installed
/// backend on this thread.
pub fn set_backend(b: Box<dyn VcsBackend>) {
    BACKEND.with(|c| {
        *c.borrow_mut() = Some(b);
    });
}

/// Short identifier of the active backend — `"git"` or `"jj"`.
///
/// Intended for **UI-only branching** of help text and hint messages
/// (e.g. `ws status` prints a jj-specific "conflicts in commits" line
/// instead of git's "merge in progress"). Don't use this for behavioural
/// switches in command logic — the trait abstraction is the right place
/// for those. The free function is a deliberate, narrow leak of the
/// backend tag for human-readable output.
pub fn backend_name() -> &'static str {
    with_backend(|b| b.name())
}

/// Run a closure against the active backend.
///
/// **Lazy default**: if no backend has been installed on this thread,
/// constructs a `GitBackend::new()` on demand. That keeps simple test
/// setups (and any early-init code path that runs before `Cli::run`)
/// working without an explicit `set_backend` call. Production code paths
/// reach this *after* `Cli::run` has set the backend explicitly, so the
/// lazy default never fires for real users.
pub(crate) fn with_backend<R>(f: impl FnOnce(&dyn VcsBackend) -> R) -> R {
    BACKEND.with(|c| {
        let mut borrow = c.borrow_mut();
        if borrow.is_none() {
            *borrow = Some(Box::new(git::GitBackend::new()));
        }
        // Safe: we just guaranteed Some above.
        f(borrow.as_deref().expect("backend must be set"))
    })
}

// ---------------------------------------------------------------------------
// Free-function facade — one wrapper per `VcsBackend` method.
//
// The wrappers exist so call sites read like the pre-refactor code
// (`vcs::repo_root()`, `vcs::create_worktree(...)`) and don't have to
// fish out the backend explicitly. The list mirrors the trait exactly —
// every new trait method needs a matching wrapper added here.
// ---------------------------------------------------------------------------

// ----- Identity -----------------------------------------------------------
pub fn repo_root() -> Result<PathBuf> { with_backend(|b| b.repo_root()) }
pub fn repo_name() -> Result<String> { with_backend(|b| b.repo_name()) }
pub fn workspace_id() -> Result<String> { with_backend(|b| b.workspace_id()) }
pub fn current_branch() -> Result<String> { with_backend(|b| b.current_branch()) }
pub fn current_commit() -> Result<String> { with_backend(|b| b.current_commit()) }
pub fn detect_trunk() -> Result<String> { with_backend(|b| b.detect_trunk()) }

// ----- Branches -----------------------------------------------------------
pub fn local_branches() -> Result<Vec<String>> { with_backend(|b| b.local_branches()) }
pub fn branch_exists(name: &str) -> Result<bool> { with_backend(|b| b.branch_exists(name)) }
pub fn is_merged(branch: &str, target: &str) -> Result<bool> {
    with_backend(|b| b.is_merged(branch, target))
}
pub fn has_diff_from(branch: &str, target: &str) -> Result<bool> {
    with_backend(|b| b.has_diff_from(branch, target))
}
pub fn delete_branch(name: &str, force: bool) -> Result<()> {
    with_backend(|b| b.delete_branch(name, force))
}
pub fn rename_branch(old: &str, new: &str) -> Result<()> {
    with_backend(|b| b.rename_branch(old, new))
}
pub fn log_oneline(from: &str, to: &str) -> Result<String> {
    with_backend(|b| b.log_oneline(from, to))
}
pub fn commit_count(from: &str, to: &str) -> Result<usize> {
    with_backend(|b| b.commit_count(from, to))
}
pub fn diff_shortstat(from: &str, to: &str) -> Result<DiffStat> {
    with_backend(|b| b.diff_shortstat(from, to))
}
pub fn diff_shortstat_in(path: &Path) -> Result<DiffStat> {
    with_backend(|b| b.diff_shortstat_in(path))
}

// ----- Working-copy state -------------------------------------------------
pub fn has_uncommitted_changes() -> Result<bool> { with_backend(|b| b.has_uncommitted_changes()) }
pub fn uncommitted_count_in(path: &Path) -> Result<usize> {
    with_backend(|b| b.uncommitted_count_in(path))
}
pub fn has_changes_from_trunk(trunk: &str) -> Result<bool> {
    with_backend(|b| b.has_changes_from_trunk(trunk))
}
pub fn is_rebase_in_progress() -> bool { with_backend(|b| b.is_rebase_in_progress()) }
pub fn is_merge_in_progress() -> bool { with_backend(|b| b.is_merge_in_progress()) }

// ----- Mutations ----------------------------------------------------------
pub fn merge(branch: &str, squash: bool, no_ff: bool, message: Option<&str>) -> Result<()> {
    with_backend(|b| b.merge(branch, squash, no_ff, message))
}
pub fn dry_run_merge(branch: &str, squash: bool) -> Result<bool> {
    with_backend(|b| b.dry_run_merge(branch, squash))
}
pub fn rebase(onto: &str) -> Result<()> { with_backend(|b| b.rebase(onto)) }
pub fn checkout(branch: &str) -> Result<()> { with_backend(|b| b.checkout(branch)) }
pub fn commit(message: &str) -> Result<()> { with_backend(|b| b.commit(message)) }
pub fn fetch() -> Result<()> { with_backend(|b| b.fetch()) }
pub fn rebase_abort() -> Result<()> { with_backend(|b| b.rebase_abort()) }
pub fn rebase_continue() -> Result<()> { with_backend(|b| b.rebase_continue()) }
pub fn merge_abort() -> Result<()> { with_backend(|b| b.merge_abort()) }
pub fn merge_continue() -> Result<()> { with_backend(|b| b.merge_continue()) }
pub fn reset_merge() -> Result<()> { with_backend(|b| b.reset_merge()) }

// ----- Worktrees ----------------------------------------------------------
pub fn list_worktrees() -> Result<Vec<WorktreeInfo>> { with_backend(|b| b.list_worktrees()) }
pub fn create_worktree(path: &Path, branch: &str, base: &str) -> Result<CreateOutcome> {
    with_backend(|b| b.create_worktree(path, branch, base))
}
pub fn remove_worktree(path: &Path, force: bool) -> Result<()> {
    with_backend(|b| b.remove_worktree(path, force))
}
pub fn move_worktree(old: &Path, new: &Path) -> Result<()> {
    with_backend(|b| b.move_worktree(old, new))
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
