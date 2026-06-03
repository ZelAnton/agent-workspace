// ===========================================================================
// vcs/git - Git helper functions (driven by the vcs-core facade wrapper)
// ===========================================================================
//
// These submodules hold the git-specific implementations behind
// [`crate::vcs::Repo`]: each function takes the typed `vcs_git::Git` client +
// an explicit cwd, returning `ws`'s `Result`/DTOs. The facade wrapper
// (`src/vcs/repo.rs`) dispatches to them via the `repo.git()` escape hatch when
// the backend is git. The ones the typed client doesn't model (CoW worktree
// creation, `log --oneline`, `checkout --detach`, the index plumbing) drop to a
// raw `processkit::Command` via the `exec`/`capture` helpers below — all honour
// an explicit working directory, so nothing here mutates the process cwd.

mod errmap;
pub(crate) mod repo;
pub(crate) mod branch;
pub(crate) mod worktree;
pub(crate) mod ops;

use std::path::Path;

use processkit::{Command, JobRunner, ProcessResult};
use vcs_git::Git;

use super::error::{Error, Result};

// Re-export pure-function helpers used by tests and by any code path that wants
// to parse fixture text without a backend instance.
pub use branch::parse_shortstat;
pub use errmap::clean_git_error;
pub use repo::is_cwd_inside;
pub use worktree::parse_worktree_list;

/// The concrete vcs-git client type used in production (real job-backed runner).
/// Matches `vcs_core::Repo::git()`'s `Git<JobRunner>` so the facade's escape
/// hatch hands the helpers exactly this type.
pub(crate) type GitClient = Git<JobRunner>;

// ---------------------------------------------------------------------------
// Raw-command helpers (used by ops the vcs-git client doesn't model)
// ---------------------------------------------------------------------------

/// Build a `git <args>` command pinned to `cwd`.
pub(crate) fn git_cmd<'a>(cwd: &Path, args: impl IntoIterator<Item = &'a str>) -> Command {
    Command::new("git").current_dir(cwd).args(args)
}

/// Run a void git command, erroring on a non-zero exit. We capture the full
/// `ProcessResult` (rather than using `Command::run`) so a failure can build
/// its message from BOTH streams — git writes `CONFLICT (content): …` to
/// stdout, which `processkit::Error::Exit` would otherwise drop.
pub(crate) async fn exec<'a>(cwd: &Path, args: impl IntoIterator<Item = &'a str>) -> Result<()> {
    let out = git_cmd(cwd, args).output_string().await.map_err(errmap::map_pk_err)?;
    if out.is_success() {
        Ok(())
    } else {
        Err(Error::Command(errmap::extract_message(out.stderr(), out.stdout().as_bytes())))
    }
}

/// Capture a git command's result without erroring on a non-zero exit — for
/// exit-code-as-answer probes (`diff --quiet`, `show-ref --verify`).
pub(crate) async fn capture<'a>(
    cwd: &Path,
    args: impl IntoIterator<Item = &'a str>,
) -> Result<ProcessResult<String>> {
    git_cmd(cwd, args).output_string().await.map_err(errmap::map_pk_err)
}

#[cfg(test)]
mod tests;
