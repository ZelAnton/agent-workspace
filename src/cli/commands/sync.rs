// ===========================================================================
// ws sync - Sync current worktree with trunk
// ===========================================================================

use std::collections::HashSet;

use clap::Args;
use clap_complete::engine::ArgValueCompleter;
use serde::Serialize;

use crate::cli::output::{self, OutputFormat};
use crate::cli::{Error, Result};
use crate::complete;
use crate::config::{Config, SyncStrategy};
use crate::vcs;
use crate::meta;

/// Machine-facing `ws sync` result (json mode). `action` is one of
/// `rebased` / `merged` / `aborted` / `continued`.
#[derive(Serialize)]
struct SyncResult {
    action: &'static str,
    branch: String,
    target: Option<String>,
    strategy: Option<&'static str>,
}

#[derive(Args)]
pub struct SyncArgs {
    /// Sync strategy (default: rebase)
    #[arg(short, long, value_enum, add = ArgValueCompleter::new(complete::complete_sync_strategies))]
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

pub async fn run(args: SyncArgs, config: &Config, format: OutputFormat, repo: &vcs::Repo) -> Result<()> {
    // jj has no "in progress" state — conflicts are recorded into commits
    // and resolved by editing files directly. --abort / --continue have no
    // direct analog. We detect "conflicts present" via is_merge_in_progress
    // (which jj implements as "jj st shows unresolved conflicts") and
    // surface a clear hint instead of silently failing.
    let is_jj = repo.backend_name() == "jj";

    if args.abort {
        if repo.is_rebase_in_progress().await {
            eprintln!("Aborting rebase...");
            repo.rebase_abort().await?;
            eprintln!("Rebase aborted.");
        } else if repo.is_merge_in_progress().await {
            if is_jj {
                return Err(Error::Other(
                    "jj records conflicts in commits — there's no in-progress merge to abort.\n\
                     Resolve the conflict markers in your files, then re-run `ws sync` if needed.\n\
                     To discard the conflicted change entirely, use `jj abandon @` (advanced)."
                        .into(),
                ));
            }
            eprintln!("Aborting merge...");
            repo.merge_abort().await?;
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
        output::emit_json(&sync_result("aborted", repo, None, None).await, format);
        return Ok(());
    }

    if args.r#continue {
        if repo.is_rebase_in_progress().await {
            eprintln!("Continuing rebase...");
            repo.rebase_continue().await?;
            eprintln!("Rebase continued.");
        } else if repo.is_merge_in_progress().await {
            if is_jj {
                return Err(Error::Other(
                    "jj records conflicts in commits — there's no in-progress merge to continue.\n\
                     Resolve the conflict markers in your files; jj snapshots the resolution \
                     into `@` on the next command. No explicit `--continue` is needed.".into(),
                ));
            }
            eprintln!("Continuing merge...");
            repo.merge_continue().await?;
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
        output::emit_json(&sync_result("continued", repo, None, None).await, format);
        return Ok(());
    }

    let current = repo.current_branch().await?;

    if let Some(ref branch) = args.from {
        if !repo.branch_exists(branch).await? {
            return Err(Error::Other(format!("Branch '{branch}' does not exist")));
        }
        eprintln!(
            "Note: --from '{branch}' applies to this sync only. \
             The worktree's base branch is unchanged."
        );
    }

    // Pre-resolve branch existence into a set: the resolver predicate must be
    // synchronous, but `branch_exists` is async. Local-branch membership is the
    // same check `branch_exists` performs.
    let known: HashSet<String> =
        repo.local_branches().await.unwrap_or_default().into_iter().collect();
    let trunk = config.resolve_trunk(repo).await;
    let target = {
        let workspace_id = repo.workspace_id().await?;
        let wt_dir = config.project_dir_for(&workspace_id);
        meta::resolve_effective_target(
            &wt_dir,
            &current,
            args.from.as_deref(),
            |b| known.contains(b),
            &trunk,
        )
    };

    if current == target {
        return Err(Error::Other(format!("Cannot sync {current} with itself")));
    }

    let strategy = args.strategy.unwrap_or(config.sync_strategy);

    output::note(format, format_args!("Syncing {current} with {target} ({strategy})..."));

    let action = match strategy {
        SyncStrategy::Rebase => {
            repo.rebase(&target).await?;
            output::success(format, format_args!("Rebased onto {target}"));
            "rebased"
        }
        SyncStrategy::Merge => {
            // Sync merges `target` (trunk) INTO the worktree, so the
            // destination bookmark to advance is the worktree's own branch
            // (`current`), not `target`.
            repo.merge(&target, &current, false, false, None).await?;
            output::success(format, format_args!("Merged {target} into {current}"));
            "merged"
        }
    };

    output::emit_json(
        &SyncResult {
            action,
            branch: current,
            target: Some(target),
            strategy: Some(strategy.as_str()),
        },
        format,
    );
    Ok(())
}

/// Build a [`SyncResult`] for the abort/continue paths, best-effort resolving
/// the current branch (these run in possibly-conflicted states where
/// `current_branch` may fail — an empty string is fine for the machine view).
async fn sync_result(
    action: &'static str,
    repo: &vcs::Repo,
    target: Option<String>,
    strategy: Option<&'static str>,
) -> SyncResult {
    SyncResult {
        action,
        branch: repo.current_branch().await.unwrap_or_default(),
        target,
        strategy,
    }
}
