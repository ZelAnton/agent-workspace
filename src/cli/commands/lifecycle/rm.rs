// ===========================================================================
// wt rm - Remove a worktree
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

pub fn run(args: RmArgs, config: &Config, path_file: Option<&Path>) -> Result<()> {
    // Get main repo path BEFORE any destructive operations
    let main_path = vcs::repo_root()?;
    let workspace_id = vcs::workspace_id()?;
    let wt_dir = config.workspaces_dir.join(&workspace_id);

    // Resolve '.' to current branch
    let branch = if args.branch == "." {
        vcs::current_branch()?
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
             Run 'wt setup' first, or 'cd' to the main repo and retry."
                .into(),
        ));
    }

    // On Windows the OS holds an exclusive handle on every process's cwd —
    // git's directory delete would fail with "Permission denied" if we sit
    // on the worktree while removing it. Switch to the main repo *before*
    // the remove, not after. Harmless on Unix (lazy-unlink semantics).
    if inside_target {
        std::env::set_current_dir(&main_path).ok();
    }

    // Remove worktree
    vcs::remove_worktree(&wt_path, args.force)?;

    // Switch to main repo before deleting branch (avoid "not in repo" error)
    std::env::set_current_dir(&main_path).ok();

    // Delete branch — best-effort, failure doesn't block worktree cleanup
    let _ = vcs::delete_branch(&branch, args.force);

    // Remove metadata
    crate::meta::remove_meta(&wt_dir, &branch);

    eprintln!("Removed worktree: {branch}");

    // If we were inside the removed worktree, write main repo path for shell to cd
    if path_file.is_some() && inside_target {
        write_path_file(path_file, &main_path)?;
    }

    Ok(())
}
