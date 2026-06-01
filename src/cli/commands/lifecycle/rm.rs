// ===========================================================================
// ws rm - Remove a worktree
// ===========================================================================

use std::path::Path;

use clap::Args;
use clap_complete::engine::ArgValueCompleter;

use crate::cli::{write_path_file, Error, Result};
use crate::complete;
use crate::config::Config;
use crate::vcs;

#[derive(Args)]
pub struct RmArgs {
    /// Branch name to remove (use '.' for current worktree)
    #[arg(add = ArgValueCompleter::new(complete::complete_worktrees))]
    branch: String,

    /// Force removal even with uncommitted changes
    #[arg(short, long)]
    force: bool,
}

pub async fn run(args: RmArgs, config: &Config, path_file: Option<&Path>, repo: &vcs::Repo) -> Result<()> {
    // Get main repo path BEFORE any destructive operations
    let main_path = repo.repo_root().await?;
    let workspace_id = repo.workspace_id().await?;
    let wt_dir = config.project_dir_for(&workspace_id);

    // Re-anchor at the main repo so worktree removal + branch deletion run
    // against it explicitly — no process chdir steering.
    let main = repo.at(&main_path);

    // Resolve '.' to current branch
    let branch = if args.branch == "." {
        repo.current_branch().await?
    } else {
        args.branch
    };

    let wt_path = wt_dir.join(&branch);

    if !wt_path.exists() {
        return Err(Error::Git(vcs::Error::WorktreeNotFound(branch.clone())));
    }

    // Check if we're inside the worktree being removed
    let inside_target = vcs::is_cwd_inside(&wt_path);

    // Without the shell wrapper, removing the current worktree leaves the
    // parent shell stranded in a deleted directory (every subsequent `pwd`
    // / `ls` then errors). Refuse instead of producing a broken shell.
    if inside_target && path_file.is_none() {
        return Err(Error::Other(
            "Refusing to remove the current worktree without shell integration.\n\
             Run 'ws setup' first, or 'cd' to the main repo and retry."
                .into(),
        ));
    }

    // On Windows the OS holds an exclusive handle on every process's cwd —
    // git's directory delete would fail with "Permission denied" if we sit
    // on the worktree while removing it. Step the process out to the main
    // repo *before* the remove, not after. Harmless on Unix (lazy-unlink).
    vcs::step_out_of(&wt_path, &main_path).ok();

    // Remove worktree (via the main-repo handle).
    main.remove_worktree(&wt_path, args.force).await?;

    // Delete branch — best-effort, failure doesn't block worktree cleanup
    let _ = main.delete_branch(&branch, args.force).await;

    // Remove metadata
    crate::meta::remove_meta(&wt_dir, &branch);

    eprintln!("Removed worktree: {branch}");

    // If we were inside the removed worktree, write main repo path for shell to cd
    if path_file.is_some() && inside_target {
        write_path_file(path_file, &main_path)?;
    }

    Ok(())
}
