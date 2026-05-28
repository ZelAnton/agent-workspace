// ===========================================================================
// ws sync - Sync current worktree with trunk
// ===========================================================================

use clap::Args;
use clap_complete::engine::ArgValueCompleter;

use crate::cli::{Error, Result};
use crate::complete;
use crate::config::{Config, SyncStrategy};
use crate::vcs;
use crate::meta;

#[derive(Args)]
pub struct SyncArgs {
    /// Sync strategy (default: rebase)
    #[arg(short, long, value_enum)]
    strategy: Option<SyncStrategy>,

    /// Source branch to sync from (default: base branch or trunk)
    #[arg(long, value_name = "BRANCH", add = ArgValueCompleter::new(complete::complete_branches))]
    from: Option<String>,

    /// Continue sync after resolving conflicts
    #[arg(long)]
    r#continue: bool,

    /// Abort sync and restore previous state
    #[arg(long)]
    abort: bool,
}

pub fn run(args: SyncArgs, config: &Config) -> Result<()> {
    // jj has no "in progress" state — conflicts are recorded into commits
    // and resolved by editing files directly. --abort / --continue have no
    // direct analog. We detect "conflicts present" via is_merge_in_progress
    // (which jj implements as "jj st shows unresolved conflicts") and
    // surface a clear hint instead of silently failing.
    let is_jj = vcs::backend_name() == "jj";

    if args.abort {
        if vcs::is_rebase_in_progress() {
            eprintln!("Aborting rebase...");
            vcs::rebase_abort()?;
            eprintln!("Rebase aborted.");
        } else if vcs::is_merge_in_progress() {
            if is_jj {
                return Err(Error::Other(
                    "jj records conflicts in commits — there's no in-progress merge to abort.\n\
                     Resolve the conflict markers in your files, then re-run `ws sync` if needed.\n\
                     To discard the conflicted change entirely, use `jj abandon @` (advanced)."
                        .into(),
                ));
            }
            eprintln!("Aborting merge...");
            vcs::merge_abort()?;
            eprintln!("Merge aborted.");
        } else if is_jj {
            return Err(Error::Other(
                "No conflicted commit to abort. jj records conflicts in commits — \
                 if you expected a rebase to be in progress, jj's rebase is atomic and \
                 either succeeded or recorded conflicts into the resulting commit."
                    .into(),
            ));
        } else {
            return Err(Error::Other("No sync in progress to abort".into()));
        }
        return Ok(());
    }

    if args.r#continue {
        if vcs::is_rebase_in_progress() {
            eprintln!("Continuing rebase...");
            vcs::rebase_continue()?;
            eprintln!("Rebase continued.");
        } else if vcs::is_merge_in_progress() {
            if is_jj {
                return Err(Error::Other(
                    "jj records conflicts in commits — there's no in-progress merge to continue.\n\
                     Resolve the conflict markers in your files; jj snapshots the resolution \
                     into `@` on the next command. No explicit `--continue` is needed.".into(),
                ));
            }
            eprintln!("Continuing merge...");
            vcs::merge_continue()?;
            eprintln!("Merge continued.");
        } else if is_jj {
            return Err(Error::Other(
                "No conflicted commit to continue from. jj operations are atomic; \
                 conflicts are recorded into commits and resolved by editing files directly."
                    .into(),
            ));
        } else {
            return Err(Error::Other("No sync in progress to continue".into()));
        }
        return Ok(());
    }

    let current = vcs::current_branch()?;

    if let Some(ref branch) = args.from {
        if !vcs::branch_exists(branch)? {
            return Err(Error::Other(format!("Branch '{branch}' does not exist")));
        }
        eprintln!(
            "Note: --from '{branch}' applies to this sync only. \
             The worktree's base branch is unchanged."
        );
    }

    let target = {
        let workspace_id = vcs::workspace_id()?;
        let wt_dir = config.project_dir_for(&workspace_id);
        meta::resolve_effective_target(
            &wt_dir,
            &current,
            args.from.as_deref(),
            |b| vcs::branch_exists(b).unwrap_or(false),
            &config.resolve_trunk(),
        )
    };

    if current == target {
        return Err(Error::Other(format!("Cannot sync {current} with itself")));
    }

    let strategy = args.strategy.unwrap_or(config.sync_strategy);

    eprintln!("Syncing {current} with {target} ({strategy:?})...");

    match strategy {
        SyncStrategy::Rebase => {
            vcs::rebase(&target)?;
            eprintln!("Rebased onto {target}");
        }
        SyncStrategy::Merge => {
            vcs::merge(&target, false, false, None)?;
            eprintln!("Merged {target} into {current}");
        }
    }

    Ok(())
}
