// ===========================================================================
// vcs/jj - Jujutsu helper functions (driven by the vcs-core facade wrapper)
// ===========================================================================
//
// These submodules (`repo`, `branch`, `ops`, `worktree`) hold the jj-specific
// implementations behind [`crate::vcs::Repo`]: each takes the typed
// `vcs_jj::Jj` client (from `vcs_core::Repo::jj()`) + an explicit cwd and
// returns `ws`'s `Result`/DTOs. The facade wrapper (`src/vcs/repo.rs`)
// dispatches to them when the backend is jj. Operations the typed client
// models are delegated to it; the few it doesn't (the CoW workspace dance —
// `workspace add --sparse-patterns empty`, `sparse set`) drop to a raw
// `processkit::Command` via `exec`.

pub(crate) mod branch;
mod errmap;
pub(crate) mod ops;
pub(crate) mod repo;
pub(crate) mod worktree;

use std::path::Path;

use processkit::{Command, JobRunner};
use vcs_jj::Jj;

use super::error::{Error, Result};

/// The concrete vcs-jj client type used in production (real job-backed runner).
/// Matches `vcs_core::Repo::jj()`'s `Jj<JobRunner>`.
pub(crate) type JjClient = Jj<JobRunner>;

// ---------------------------------------------------------------------------
// Raw-command helpers (used by ops the vcs-jj client doesn't model)
// ---------------------------------------------------------------------------

/// Build a `jj <args>` command pinned to `cwd`.
pub(crate) fn jj_cmd<'a>(cwd: &Path, args: impl IntoIterator<Item = &'a str>) -> Command {
    Command::new("jj").current_dir(cwd).args(args)
}

/// Run a void jj command, erroring on a non-zero exit. Captures both streams so
/// the message survives (jj writes some informational text to stdout).
pub(crate) async fn exec<'a>(cwd: &Path, args: impl IntoIterator<Item = &'a str>) -> Result<()> {
    let out = jj_cmd(cwd, args).output_string().await.map_err(errmap::map_pk_err)?;
    if out.is_success() {
        Ok(())
    } else {
        Err(Error::Command(errmap::extract_message(out.stderr(), out.stdout().as_bytes())))
    }
}

/// Shared `Error::Unsupported` for the jj operations with no analogue (the
/// abort/continue state machine and `workspace move`). The facade wrapper
/// returns these from its jj arms.
pub(crate) fn unsupported(op: &str, hint: &str) -> Error {
    Error::Unsupported(format!("jj: {op} — {hint}"))
}
