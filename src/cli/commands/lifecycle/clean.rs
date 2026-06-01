// ===========================================================================
// ws clean - Clean up worktrees with no diff from trunk
// ===========================================================================

use std::collections::HashSet;
use std::path::Path;

use clap::Args;

use crate::cli::{write_path_file, Result};
use crate::config::Config;
use crate::vcs;
use crate::meta;

#[derive(Args)]
pub struct CleanArgs {
    /// Preview which worktrees would be cleaned without removing them
    #[arg(long)]
    pub dry_run: bool,
}

pub async fn run(args: CleanArgs, config: &Config, path_file: Option<&Path>, repo: &vcs::Repo) -> Result<()> {
    // Get main repo path before any operations
    let main_path = repo.repo_root().await?;
    let workspace_id = repo.workspace_id().await?;
    let wt_dir = config.project_dir_for(&workspace_id);

    if !wt_dir.exists() {
        eprintln!("No worktrees to clean.");
        return Ok(());
    }

    // Re-anchor at the main repo for branch deletion — no process chdir.
    let main = repo.at(&main_path);

    let trunk = config.resolve_trunk(repo).await;
    let known_branches: HashSet<String> = repo
        .local_branches()
        .await
        .unwrap_or_default()
        .into_iter()
        .collect();

    let worktrees = repo.list_worktrees().await?;
    let mut cleaned = 0;
    let mut checked = 0;
    let mut skipped_dirty = 0;
    let mut cleaned_current = false;

    for wt in worktrees {
        if !wt.path.starts_with(&wt_dir) {
            continue;
        }

        let Some(branch) = wt.branch.as_ref() else {
            continue;
        };

        // Skip trunk
        if branch == &trunk {
            continue;
        }

        checked += 1;

        let target = meta::resolve_effective_target(
            &wt_dir,
            branch,
            None,
            |b| known_branches.contains(b),
            &trunk,
        );

        // Skip worktrees that still differ from target — committed diff is
        // the cheap check, run it before the per-worktree dirty status call.
        if repo.has_diff_from(branch, &target).await.unwrap_or(true) {
            continue;
        }

        // Dirty worktrees aren't clean even with no committed diff: git
        // refuses non-force removal anyway, and silently discarding
        // in-flight work would be a footgun.
        let dirty = repo.uncommitted_count_in(&wt.path).await.unwrap_or(0);
        if dirty > 0 {
            eprintln!("Skipping {branch}: {dirty} uncommitted change(s)");
            skipped_dirty += 1;
            continue;
        }

        if args.dry_run {
            eprintln!("Would clean (no diff from {target}): {branch}");
            cleaned += 1;
            continue;
        }

        let inside = vcs::is_cwd_inside(&wt.path);

        // Removing the worktree the user is standing in needs shell integration
        // to rescue the parent shell (via the path-file). Without it, SKIP this
        // one (with a hint) rather than strand the shell — the rest of the
        // sweep can still proceed. The shell wrapper always passes
        // `--path-file`, so this only bites a direct-binary `ws clean`.
        if inside && path_file.is_none() {
            eprintln!(
                "Skipping {branch}: it's the current worktree and shell integration \
                 is off (run 'ws setup', or 'cd' to the main repo first)."
            );
            continue;
        }

        eprintln!("Cleaning worktree (no diff from {target}): {branch}");

        if let Err(e) = repo.remove_worktree(&wt.path, false).await {
            eprintln!("Warning: failed to remove worktree {branch}: {e}");
            continue;
        }

        // Delete branch via the main-repo handle — git refuses to delete the
        // branch a worktree is on. (Was a `set_current_dir(main)` steer.)
        main.delete_branch(branch, false).await.ok();

        crate::meta::remove_meta(&wt_dir, branch);

        cleaned += 1;

        if inside {
            cleaned_current = true;
        }
    }

    let verb = if args.dry_run {
        "would be cleaned"
    } else {
        "cleaned"
    };

    if checked == 0 {
        eprintln!("No worktrees to clean.");
    } else if cleaned == 0 {
        eprintln!("No worktrees to clean (all have changes).");
    } else {
        eprintln!("{cleaned} worktree(s) {verb}.");
    }
    if skipped_dirty > 0 {
        eprintln!("{skipped_dirty} worktree(s) skipped due to uncommitted changes.");
    }

    // Write main repo path for shell to cd if we were inside a cleaned worktree
    if !args.dry_run && path_file.is_some() && cleaned_current {
        write_path_file(path_file, &main_path)?;
    }

    Ok(())
}
