// ===========================================================================
// wt sync - Sync current worktree with trunk
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
    if args.abort {
        if vcs::is_rebase_in_progress() {
            eprintln!("Aborting rebase...");
            vcs::rebase_abort()?;
            eprintln!("Rebase aborted.");
        } else if vcs::is_merge_in_progress() {
            eprintln!("Aborting merge...");
            vcs::merge_abort()?;
            eprintln!("Merge aborted.");
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
            eprintln!("Continuing merge...");
            vcs::merge_continue()?;
            eprintln!("Merge continued.");
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
        let wt_dir = config.workspaces_dir.join(&workspace_id);
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
