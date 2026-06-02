// ===========================================================================
// ws snap-continue - Continue snap mode after agent exits
// ===========================================================================

use std::path::{Path, PathBuf};

// Exit codes consumed by the shell wrapper's snap loop.
// Keep in sync with the `case $continue_status` blocks in src/shell/mod.rs.
pub const EXIT_DONE: i32 = 0;
pub const EXIT_REOPEN: i32 = 2;
pub const EXIT_PRESERVE: i32 = 3;

use crate::cli::{write_path_file, Error, Result};
use crate::config::Config;
use crate::vcs;
use crate::meta::{self, WorktreeMeta};
use crate::process;
use crate::prompt::{self, SnapExitChoice, SnapMergeChoice};

// ===========================================================================
// Public Types
// ===========================================================================

/// Action to take after snap mode agent exits
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapAction {
    /// Cleanup and return to main (no changes)
    CleanupNoChanges,
    /// Merge changes and cleanup
    MergeAndCleanup,
    /// Exit snap mode but preserve worktree for manual handling
    ExitPreserve,
    /// Reopen the agent
    Reopen,
}

/// Context for snap-continue operation
#[derive(Debug)]
pub struct SnapContext {
    pub cwd: PathBuf,
    pub branch: String,
    pub merge_target: String,
    pub repo_root: PathBuf,
    pub has_uncommitted: bool,
    pub has_commits_ahead: bool,
}

// ===========================================================================
// Entry Point
// ===========================================================================

/// Run snap-continue command.
pub async fn run(config: &Config, path_file: Option<&Path>, repo: &vcs::Repo) -> Result<()> {
    let ctx = gather_context(config, repo).await?;
    let action = determine_action(&ctx)?;
    execute_action(&ctx, &action, config, path_file, repo).await
}

// ===========================================================================
// Pure Logic (Testable)
// ===========================================================================

/// Gather context from git state
pub async fn gather_context(config: &Config, repo: &vcs::Repo) -> Result<SnapContext> {
    // The snap loop cd'd the process INTO the worktree before running the
    // agent, so the worktree IS the Repo's anchor — read it from `repo.cwd()`
    // instead of the process cwd (no `std::env::current_dir()` here).
    let cwd = repo.cwd().to_path_buf();
    let branch = repo.current_branch().await?;
    let workspace_id = repo.workspace_id().await?;
    let repo_root = repo.repo_root().await?;

    // Load metadata to get base_branch (fallback to legacy .status.toml).
    let wt_dir = config.project_dir_for(&workspace_id);
    let meta_path = meta::meta_path_with_fallback(&wt_dir, &branch);
    let loaded_meta = WorktreeMeta::load(&meta_path).ok();

    // Resolve trunk lazily — `resolve_trunk` shells out when not configured,
    // and is only needed when meta is missing.
    //
    // If the worktree was created from a real base branch that has since
    // been deleted, refuse rather than silently merging into trunk —
    // landing commits on the wrong branch is a worse failure mode than an
    // explicit error that points the user at `ws merge --into <branch>`.
    let merge_target = match loaded_meta.as_ref().map(|m| m.base_branch.clone()) {
        Some(bb) => {
            if repo.branch_exists(&bb).await.unwrap_or(false) {
                bb
            } else {
                return Err(Error::Other(format!(
                    "Base branch '{bb}' no longer exists.\n\
                     Resolve manually with: ws merge --into <branch>"
                )));
            }
        }
        None => config.resolve_trunk(repo).await,
    };

    let has_uncommitted = repo.has_uncommitted_changes().await.unwrap_or(false);
    let has_commits_ahead = repo.commit_count(&merge_target, "HEAD").await.unwrap_or(0) > 0;

    Ok(SnapContext {
        cwd,
        branch,
        merge_target,
        repo_root,
        has_uncommitted,
        has_commits_ahead,
    })
}

/// Determine action based on context and user choice
pub fn determine_action(ctx: &SnapContext) -> Result<SnapAction> {
    // No changes at all → cleanup
    if !ctx.has_uncommitted && !ctx.has_commits_ahead {
        return Ok(SnapAction::CleanupNoChanges);
    }

    // Only committed changes → prompt merge or exit
    if !ctx.has_uncommitted && ctx.has_commits_ahead {
        return match prompt::snap_merge_prompt() {
            Ok(SnapMergeChoice::Merge) => Ok(SnapAction::MergeAndCleanup),
            Ok(SnapMergeChoice::Exit) | Err(_) => Ok(SnapAction::ExitPreserve),
        };
    }

    // Has uncommitted changes → prompt reopen or exit
    match prompt::snap_exit_prompt() {
        Ok(SnapExitChoice::Reopen) => Ok(SnapAction::Reopen),
        Ok(SnapExitChoice::Exit) | Err(_) => Ok(SnapAction::ExitPreserve),
    }
}

/// Determine action without prompt (for testing)
#[cfg(test)]
pub fn determine_action_with_choice(
    has_uncommitted: bool,
    has_commits_ahead: bool,
    exit_choice: Option<SnapExitChoice>,
    merge_choice: Option<SnapMergeChoice>,
) -> SnapAction {
    // No changes at all → cleanup
    if !has_uncommitted && !has_commits_ahead {
        return SnapAction::CleanupNoChanges;
    }

    // Only committed changes → use merge choice
    if !has_uncommitted && has_commits_ahead {
        return match merge_choice {
            Some(SnapMergeChoice::Merge) => SnapAction::MergeAndCleanup,
            Some(SnapMergeChoice::Exit) | None => SnapAction::ExitPreserve,
        };
    }

    // Has uncommitted changes → use exit choice
    match exit_choice {
        Some(SnapExitChoice::Reopen) => SnapAction::Reopen,
        Some(SnapExitChoice::Exit) | None => SnapAction::ExitPreserve,
    }
}

/// Remove worktree, branch, and metadata.
///
/// Uses non-force removal so that any untracked files left in the worktree
/// (build artifacts, .env, agent-generated scratch) cause the cleanup to
/// fail loudly instead of silently deleting work.
pub async fn cleanup_worktree(
    main: &vcs::Repo,
    wt_path: &Path,
    branch: &str,
    config: &Config,
) -> Result<()> {
    main.remove_worktree(wt_path, false).await?;
    main.delete_branch(branch, true).await.ok();

    // Remove metadata
    if let Ok(workspace_id) = main.workspace_id().await {
        let wt_dir = config.project_dir_for(&workspace_id);
        meta::remove_meta(&wt_dir, branch);
    }

    Ok(())
}

// ===========================================================================
// Side Effects (Hard to Test)
// ===========================================================================

/// Execute action with side effects
async fn execute_action(
    ctx: &SnapContext,
    action: &SnapAction,
    config: &Config,
    path_file: Option<&Path>,
    repo: &vcs::Repo,
) -> Result<()> {
    // Main-repo handle for every main-repo op below — replaces the old
    // `set_current_dir(&ctx.repo_root)` steering. No process chdir for steering;
    // the only process-cwd move left is the Windows-lock `step_out_of` on the
    // cleanup paths (which `exit()` immediately after).
    let main = repo.at(&ctx.repo_root);

    match action {
        SnapAction::CleanupNoChanges => {
            eprintln!("No changes detected. Cleaning up...");
            // The snap loop cd'd us INTO the worktree before running the agent,
            // so `ctx.cwd` is the worktree. On Windows the OS holds an
            // exclusive handle on the process cwd, so `git worktree remove`
            // would fail to delete the directory we're standing in. Step out to
            // the main repo first — same guard the MergeAndCleanup arm uses.
            vcs::step_out_of(&ctx.cwd, &ctx.repo_root).map_err(|e| Error::Other(e.to_string()))?;
            cleanup_worktree(&main, &ctx.cwd, &ctx.branch, config).await?;
            write_path_file(path_file, &ctx.repo_root)?;
            std::process::exit(EXIT_DONE);
        }
        SnapAction::MergeAndCleanup => {
            // Run pre-merge hooks
            if !config.hooks.pre_merge.is_empty() {
                eprintln!("Running pre-merge hooks...");
                process::run_hooks(&config.hooks.pre_merge, &ctx.cwd)
                    .map_err(|e| Error::Other(e.to_string()))?;
            }

            eprintln!("Merging {} into {}...", ctx.branch, ctx.merge_target);

            main.checkout(&ctx.merge_target).await?;

            if !main.dry_run_merge(&ctx.branch, config.merge_strategy.is_squash()).await? {
                main.checkout(&ctx.merge_target).await.ok();
                super::super::merge::print_conflict_hint();
                eprintln!();
                eprintln!(
                    "Conflicts in worktree '{}'. Resolve there, then 'ws merge'.",
                    ctx.branch
                );
                std::process::exit(EXIT_PRESERVE);
            }

            if let Err(e) = super::super::merge::execute_merge(
                &main,
                &ctx.branch,
                &ctx.merge_target,
                config.merge_strategy,
            )
            .await
            {
                eprintln!("Merge failed: {e}");
                let _ = main.reset_merge().await;
                let _ = main.checkout(&ctx.merge_target).await;
                eprintln!(
                    "Worktree '{}' preserved. Inspect there and retry.",
                    ctx.branch
                );
                std::process::exit(EXIT_PRESERVE);
            }

            eprintln!("Merged {} into {}", ctx.branch, ctx.merge_target);

            // Match pre_merge CWD so hooks see the same context across phases.
            if !config.hooks.post_merge.is_empty() {
                eprintln!("Running post-merge hooks...");
                process::run_hooks(&config.hooks.post_merge, &ctx.cwd)
                    .map_err(|e| Error::Other(e.to_string()))?;
            }

            // Step out of the worktree (Windows lock) before removing it.
            vcs::step_out_of(&ctx.cwd, &ctx.repo_root).map_err(|e| Error::Other(e.to_string()))?;
            cleanup_worktree(&main, &ctx.cwd, &ctx.branch, config).await?;
            write_path_file(path_file, &ctx.repo_root)?;
            std::process::exit(EXIT_DONE);
        }
        SnapAction::Reopen => {
            eprintln!("Reopening agent...");
            std::process::exit(EXIT_REOPEN);
        }
        SnapAction::ExitPreserve => {
            eprintln!();
            eprintln!("Exiting snap mode. Worktree preserved.");
            eprintln!();
            eprintln!("Your changes are safe. To continue later:");
            eprintln!("  git add . && git commit -m 'your message'");
            eprintln!("  ws merge    # merge and cleanup");
            eprintln!();
            std::process::exit(EXIT_PRESERVE);
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Wire-protocol tripwire: the snap exit codes are a contract with the
    // snap-loop `case`/`if` blocks in EVERY shell wrapper. These tests derive
    // the expected branch labels from the constants and assert each wrapper
    // contains them, so changing a constant without updating the templates
    // (or vice versa) fails CI — making the "keep in sync" comment enforceable
    // rather than aspirational.
    // -----------------------------------------------------------------------

    use crate::shell::Shell;

    #[test]
    fn snap_exit_codes_match_posix_wrapper_case_labels() {
        // bash/zsh use POSIX `case` with `<code>)` labels; fish's `switch`
        // uses `case <code>`. Derive each from the constants so a renumbered
        // exit code that the template didn't follow trips this test.
        for shell in [Shell::Bash, Shell::Zsh] {
            let w = shell.wrapper_script();
            assert!(w.contains(&format!("{EXIT_REOPEN})")), "{shell:?} EXIT_REOPEN label");
            assert!(w.contains(&format!("{EXIT_PRESERVE})")), "{shell:?} EXIT_PRESERVE label");
        }
        let fish = Shell::Fish.wrapper_script();
        assert!(fish.contains(&format!("case {EXIT_REOPEN}")), "fish EXIT_REOPEN case");
        assert!(fish.contains(&format!("case {EXIT_PRESERVE}")), "fish EXIT_PRESERVE case");
    }

    #[test]
    fn snap_exit_codes_match_powershell_wrapper_comparisons() {
        // PowerShell branches with `-eq <code>`.
        let w = Shell::PowerShell.wrapper_script();
        assert!(
            w.contains(&format!("-eq {EXIT_REOPEN}")),
            "powershell wrapper must compare against EXIT_REOPEN"
        );
        assert!(
            w.contains(&format!("-eq {EXIT_PRESERVE}")),
            "powershell wrapper must compare against EXIT_PRESERVE"
        );
    }

    // -----------------------------------------------------------------------
    // determine_action_with_choice tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_determine_no_changes() {
        // No uncommitted, no commits ahead → cleanup
        let action = determine_action_with_choice(false, false, Some(SnapExitChoice::Exit), None);
        assert_eq!(action, SnapAction::CleanupNoChanges);
    }

    #[test]
    fn test_determine_only_commits_ahead_merge() {
        // No uncommitted but has commits ahead, user chooses merge
        let action = determine_action_with_choice(false, true, None, Some(SnapMergeChoice::Merge));
        assert_eq!(action, SnapAction::MergeAndCleanup);
    }

    #[test]
    fn test_determine_only_commits_ahead_exit() {
        // No uncommitted but has commits ahead, user chooses exit
        let action = determine_action_with_choice(false, true, None, Some(SnapMergeChoice::Exit));
        assert_eq!(action, SnapAction::ExitPreserve);
    }

    #[test]
    fn test_determine_only_commits_ahead_no_choice_defaults_to_exit() {
        // No uncommitted but has commits ahead, no choice → exit
        let action = determine_action_with_choice(false, true, None, None);
        assert_eq!(action, SnapAction::ExitPreserve);
    }

    #[test]
    fn test_determine_uncommitted_reopen() {
        // Has uncommitted, user chooses reopen → reopen
        let action = determine_action_with_choice(true, false, Some(SnapExitChoice::Reopen), None);
        assert_eq!(action, SnapAction::Reopen);
    }

    #[test]
    fn test_determine_uncommitted_exit() {
        // Has uncommitted, user chooses exit → preserve worktree
        let action = determine_action_with_choice(true, true, Some(SnapExitChoice::Exit), None);
        assert_eq!(action, SnapAction::ExitPreserve);
    }

    #[test]
    fn test_determine_uncommitted_none_defaults_to_exit() {
        // Has uncommitted, no choice → preserve worktree
        let action = determine_action_with_choice(true, false, None, None);
        assert_eq!(action, SnapAction::ExitPreserve);
    }
}
