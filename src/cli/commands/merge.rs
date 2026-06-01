// ===========================================================================
// ws merge - Merge current worktree to trunk
// ===========================================================================

use std::path::Path;

use clap::Args;
use clap_complete::engine::ArgValueCompleter;
use serde::Serialize;

use crate::cli::output::{self, OutputFormat};
use crate::cli::{write_path_file, Error, Result};
use crate::complete;
use crate::config::{Config, MergeStrategy};
use crate::vcs;
use crate::meta;
use crate::process;

/// Machine-facing `ws merge` result (json mode).
#[derive(Serialize)]
struct MergeResult {
    merged: bool,
    branch: String,
    target: String,
    commits: usize,
    deleted: bool,
}

#[derive(Args)]
pub struct MergeArgs {
    /// Merge strategy (default: squash)
    #[arg(short, long, value_enum)]
    strategy: Option<MergeStrategy>,

    /// Target branch to merge into (default: trunk)
    #[arg(long, value_name = "BRANCH", add = ArgValueCompleter::new(complete::complete_branches))]
    into: Option<String>,

    /// Delete worktree after merge (default: keep)
    #[arg(short = 'd', long)]
    delete: bool,

    /// Skip pre-merge hooks
    #[arg(short = 'H', long)]
    skip_hooks: bool,
}

pub async fn run(
    args: MergeArgs,
    config: &Config,
    path_file: Option<&Path>,
    format: OutputFormat,
    repo: &vcs::Repo,
) -> Result<()> {
    let main_repo = repo.repo_root().await?;
    run_merge(args, config, path_file, &main_repo, format, repo).await
}

async fn run_merge(
    args: MergeArgs,
    config: &Config,
    path_file: Option<&Path>,
    main_repo: &Path,
    format: OutputFormat,
    repo: &vcs::Repo,
) -> Result<()> {
    let current = repo.current_branch().await?;
    let workspace_id = repo.workspace_id().await?;
    let wt_dir = config.project_dir_for(&workspace_id);

    // --into target must exist AND not be checked out elsewhere.
    // git refuses to checkout a branch that another worktree owns; without
    // the second check, merge would fail mid-flight with a confusing
    // low-level git error instead of a clear upfront message.
    if let Some(ref branch) = args.into {
        if !repo.branch_exists(branch).await? {
            return Err(Error::Other(format!("Branch '{branch}' does not exist")));
        }
        let main_canon = main_repo
            .canonicalize()
            .unwrap_or_else(|_| main_repo.to_path_buf());
        let conflict = repo.list_worktrees().await?.into_iter().find(|wt| {
            wt.branch.as_deref() == Some(branch.as_str())
                && wt.path.canonicalize().unwrap_or_else(|_| wt.path.clone()) != main_canon
        });
        if let Some(wt) = conflict {
            return Err(Error::Other(format!(
                "Branch '{branch}' is checked out in another worktree at {}.\n\
                 Switch that worktree off the branch, or merge from there directly.",
                wt.path.display()
            )));
        }
    }

    // Refuse to silently retarget to trunk when the worktree's recorded base
    // branch was deleted and the user gave no `--into` override — landing
    // commits on the wrong branch is a worse failure mode than an explicit
    // error. Mirrors snap-continue (`resume::gather_context`); read-only
    // callers (`ws status`/`ws ls`) keep the silent trunk fallback in
    // `resolve_effective_target` because refusing there would be unhelpful.
    if args.into.is_none() {
        let meta_path = meta::meta_path_with_fallback(&wt_dir, &current);
        if let Ok(m) = meta::WorktreeMeta::load(&meta_path)
            && !repo.branch_exists(&m.base_branch).await.unwrap_or(false)
        {
            return Err(Error::Other(format!(
                "Base branch '{}' no longer exists.\n\
                 Resolve manually with: ws merge --into <branch>",
                m.base_branch
            )));
        }
    }

    // Pre-resolve branch existence into a set so the resolver's sync predicate
    // doesn't need the async `branch_exists`.
    let known: std::collections::HashSet<String> =
        repo.local_branches().await.unwrap_or_default().into_iter().collect();
    let trunk = config.resolve_trunk(repo).await;
    let target = meta::resolve_effective_target(
        &wt_dir,
        &current,
        args.into.as_deref(),
        |b| known.contains(b),
        &trunk,
    );

    if current == target {
        return Err(Error::Other(format!("Cannot merge {current} into itself")));
    }

    if repo.has_uncommitted_changes().await? {
        return Err(Error::Other(format!(
            "Worktree '{current}' has uncommitted changes. Commit or stash first."
        )));
    }

    let wt_path = wt_dir.join(&current);
    let inside_worktree = vcs::is_cwd_inside(&wt_path);

    // Same guard `ws rm` enforces: deleting the worktree the user is standing
    // in requires shell integration to rescue the parent shell back to the
    // main repo (via the path-file). Without it, `merge -d` would leave the
    // shell stranded in a deleted directory. The shell wrapper always passes
    // `--path-file`; this only bites a direct-binary `ws merge -d`.
    if args.delete && inside_worktree && path_file.is_none() {
        return Err(Error::Other(
            "Refusing to delete the current worktree without shell integration.\n\
             Run 'ws setup' first, or 'cd' to the main repo and retry."
                .into(),
        ));
    }

    let strategy = args.strategy.unwrap_or(config.merge_strategy);

    if !args.skip_hooks && !config.hooks.pre_merge.is_empty() {
        eprintln!("Running pre-merge hooks...");
        // CWD = worktree so pre_merge and post_merge see the same context.
        process::run_hooks(&config.hooks.pre_merge, &wt_path)
            .map_err(|e| Error::Other(e.to_string()))?;
    }

    let commit_count = repo.commit_count(&target, &current).await.unwrap_or(0);
    eprintln!("Merging {current} into {target} ({commit_count} commits, {strategy:?})");

    // Re-anchor at the main repo for every main-repo operation that follows —
    // replaces the old `set_current_dir(main_repo)` steering with an explicit
    // handle. No process cwd mutation: the worktree stays the process cwd (so
    // post-merge hooks below still see it), and the main-repo ops run via
    // `main`, anchored at `main_repo` through `backend.at_cwd`.
    let main = repo.at(main_repo);

    if main.has_uncommitted_changes().await? {
        return Err(Error::Other(
            "Main repo has uncommitted changes. Commit or stash before merging.".into(),
        ));
    }
    if main.is_merge_in_progress().await {
        return Err(Error::Other("Main repo has a merge in progress.".into()));
    }
    if main.is_rebase_in_progress().await {
        return Err(Error::Other("Main repo has a rebase in progress.".into()));
    }

    // Capture main repo's current branch *before* we move HEAD, so we can
    // restore it if any subsequent step fails.
    let original_main_branch = main.current_branch().await.ok();

    main.checkout(&target).await?;

    if !main.dry_run_merge(&current, strategy.is_squash()).await? {
        if let Some(orig) = &original_main_branch {
            let _ = main.checkout(orig).await;
        }
        print_conflict_hint();
        return Err(Error::Other("Merge aborted due to conflicts".into()));
    }

    match execute_merge(&main, &current, &target, strategy).await {
        Ok(false) => {
            eprintln!("Nothing to merge: {current} is already up to date with {target}");
            // Restore main repo to its prior branch — moving HEAD is a side
            // effect of the dry-run + checkout sequence; the user didn't
            // ask for it.
            if let Some(orig) = &original_main_branch {
                let _ = main.checkout(orig).await;
            }
            output::emit_json(
                &MergeResult {
                    merged: false,
                    branch: current.clone(),
                    target: target.clone(),
                    commits: commit_count,
                    deleted: false,
                },
                format,
            );
            return Ok(());
        }
        Err(e) => {
            // Roll back any squash staging, then return HEAD to where it was.
            let _ = main.reset_merge().await;
            if let Some(orig) = &original_main_branch {
                let _ = main.checkout(orig).await;
            }
            return Err(e);
        }
        Ok(true) => {}
    }

    if !config.hooks.post_merge.is_empty() {
        eprintln!("Running post-merge hooks...");
        // Match pre_merge: CWD = worktree (still on disk, since cleanup
        // happens after this block).
        process::run_hooks(&config.hooks.post_merge, &wt_path)
            .map_err(|e| Error::Other(e.to_string()))?;
    }

    if args.delete {
        // On Windows the OS holds an exclusive handle on the process cwd. The
        // process is still standing in the worktree here (post-merge hooks ran
        // with CWD = worktree), so step out to the main repo before removing
        // it, or `git worktree remove` fails "Permission denied". Harmless on
        // Unix. Previously implicit because the flow had chdir'd to main.
        vcs::step_out_of(&wt_path, main_repo).ok();
        cleanup_worktree(&main, &current, config).await?;
        if inside_worktree {
            write_path_file(path_file, main_repo)?;
        }
    }

    eprintln!("Merge complete: {current} into {target}.");

    output::emit_json(
        &MergeResult {
            merged: true,
            branch: current.clone(),
            target: target.clone(),
            commits: commit_count,
            deleted: args.delete,
        },
        format,
    );

    Ok(())
}

pub fn print_conflict_hint() {
    eprintln!("Merge would conflict. Sync first to resolve:");
    eprintln!("  ws sync");
    eprintln!("  ws merge");
}

/// Build commit message for squash merge
///
/// - Single commit: use that commit's message directly
/// - Multiple commits: "Merge branch 'x'" + list all commits
/// - No commits: "Merge branch 'x'"
pub fn build_merge_message(branch: &str, log: &str) -> String {
    let lines: Vec<&str> = log.lines().filter(|l| !l.is_empty()).collect();

    match lines.len() {
        0 => format!("Merge branch '{branch}'"),
        1 => {
            // Single commit → strip hash prefix, use message directly
            let line = lines[0];
            line.split_once(' ')
                .map(|(_, msg)| msg.to_string())
                .unwrap_or_else(|| format!("Merge branch '{branch}'"))
        }
        _ => {
            let mut msg = format!("Merge branch '{branch}'\n\n");
            for line in &lines {
                msg.push_str(&format!("* {line}\n"));
            }
            msg.trim_end().to_string()
        }
    }
}

/// Execute squash/merge. `main` must already be on trunk (the caller checked
/// it out via this same handle).
///
/// Returns true if changes were merged, false if already up to date.
pub async fn execute_merge(
    main: &vcs::Repo,
    branch: &str,
    trunk: &str,
    strategy: MergeStrategy,
) -> Result<bool> {
    let log = main.log_oneline(trunk, branch).await.unwrap_or_default();
    let msg = build_merge_message(branch, &log);

    // No-op detection in two layers so cleanup paths only run when
    // something actually changed:
    //
    //   1. `commit_count(trunk, branch) == 0` — branch has no commits
    //      trunk lacks (sha-level). Cheap; catches the common case.
    //   2. **Post-merge `current_commit` comparison** — catches the
    //      cherry-pick scenario where branch has commits trunk lacks (by
    //      sha) but their content is already present in trunk under
    //      different shas. `git merge --squash` stages nothing in that
    //      case, the commit step is skipped, and HEAD doesn't advance.
    //      Comparing HEAD before/after gives a backend-agnostic signal.
    //
    // **Why not `diff_shortstat(trunk, branch) == 0/0` as the pre-check?**
    // The git impl uses `git diff --shortstat trunk...branch` (three-dot
    // / merge-base diff), which shows branch's accumulated work from the
    // merge base — non-zero in the cherry-pick case even though `merge
    // --squash` would stage nothing. The three-dot diff is the right
    // shape for "what does this branch add to its merge base" but the
    // wrong shape for "would a squash merge produce content changes".
    // A two-dot tree diff `git diff trunk branch` would be correct, but
    // we don't expose that primitive — and the post-merge check below
    // works for both backends without a new trait method.
    if main.commit_count(trunk, branch).await? == 0 {
        return Ok(false);
    }
    let pre_commit = main.current_commit().await.ok();
    // `trunk` is the merge DESTINATION (the caller checked it out); pass it so
    // the jj backend advances the right bookmark instead of guessing from `@`.
    match strategy {
        MergeStrategy::Squash => {
            main.merge(branch, trunk, true, false, Some(&msg)).await?;
        }
        MergeStrategy::Merge => {
            main.merge(branch, trunk, false, true, Some(&msg)).await?;
        }
    }
    let post_commit = main.current_commit().await.ok();
    // Git: HEAD stays put when `merge --squash` stages nothing and the
    //      commit step is skipped — pre == post → return false.
    // Jj:  `@` always advances on merge() (new change for squash, merge
    //      commit for non-squash), so pre != post → return true. Jj's
    //      pre-flight check inside `merge()` catches the "branch is
    //      ancestor of @" no-op upstream; the cherry-pick-where-content-
    //      already-applied case in jj is an accepted limitation.
    if pre_commit.is_some() && pre_commit == post_commit {
        return Ok(false);
    }
    Ok(true)
}

/// Clean up worktree after successful merge.
///
/// `main` is the main-repo `Repo` handle — worktree removal and branch
/// deletion run against it explicitly (the branch the worktree is on can't be
/// deleted from the worktree itself).
pub async fn cleanup_worktree(main: &vcs::Repo, branch: &str, config: &Config) -> Result<()> {
    let workspace_id = main.workspace_id().await?;
    let wt_dir = config.project_dir_for(&workspace_id);
    let wt_path = wt_dir.join(branch);

    eprintln!("Cleaning up worktree: {branch}");

    main.remove_worktree(&wt_path, false).await.ok();

    // Force delete: squash merge rewrites history so -d thinks
    // the branch is "not fully merged" even though changes are in trunk
    main.delete_branch(branch, true).await.ok();

    crate::meta::remove_meta(&wt_dir, branch);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_merge_message_with_commits() {
        let log = "abc1234 Add user authentication\ndef5678 Fix login edge case\n";
        let msg = build_merge_message("feature-auth", log);
        assert!(msg.starts_with("Merge branch 'feature-auth'\n"));
        assert!(msg.contains("abc1234 Add user authentication"));
        assert!(msg.contains("def5678 Fix login edge case"));
    }

    #[test]
    fn test_build_merge_message_single_commit() {
        let log = "abc1234 Initial implementation\n";
        let msg = build_merge_message("fix-bug", log);
        assert_eq!(msg, "Initial implementation");
    }

    #[test]
    fn test_build_merge_message_empty_log() {
        let msg = build_merge_message("my-branch", "");
        assert_eq!(msg, "Merge branch 'my-branch'");
    }
}
