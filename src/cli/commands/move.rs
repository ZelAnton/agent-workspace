// ===========================================================================
// ws mv - Rename worktree branch
// ===========================================================================

use std::path::Path;

use clap::Args;
use clap_complete::engine::ArgValueCompleter;

use crate::cli::{write_path_file, Error, Result};
use crate::complete;
use crate::config::Config;
use crate::vcs;

#[derive(Args)]
pub struct MoveArgs {
    /// Current branch name (use '.' for current worktree)
    #[arg(add = ArgValueCompleter::new(complete::complete_worktrees))]
    old_branch: String,

    /// New branch name
    new_branch: String,
}

pub fn run(args: MoveArgs, config: &Config, path_file: Option<&Path>) -> Result<()> {
    let workspace_id = vcs::workspace_id()?;
    let wt_dir = config.project_dir_for(&workspace_id);

    // Resolve '.' to current branch
    let old_branch = if args.old_branch == "." {
        vcs::current_branch()?
    } else {
        args.old_branch
    };

    let old_path = wt_dir.join(&old_branch);
    let new_path = wt_dir.join(&args.new_branch);

    if !old_path.exists() {
        return Err(Error::Git(vcs::Error::WorktreeNotFound(old_branch.clone())));
    }

    if new_path.exists() {
        return Err(Error::Git(vcs::Error::WorktreeExists(
            args.new_branch.clone(),
        )));
    }

    // Check if we're inside the worktree being renamed
    let inside_target = vcs::is_cwd_inside(&old_path);

    // Same guard `ws rm` enforces: relocating the worktree the user is standing
    // in needs shell integration to rescue the parent shell to the new path.
    // Without it the shell is stranded in the old (now-nonexistent) path. The
    // shell wrapper always passes `--path-file`; this only bites a direct
    // `ws mv` of the current worktree.
    if inside_target && path_file.is_none() {
        return Err(Error::Other(
            "Refusing to move the current worktree without shell integration.\n\
             Run 'ws setup' first, or 'cd' to the main repo and retry."
                .into(),
        ));
    }

    // On Windows the OS locks the process cwd, so `git worktree move` would fail
    // to relocate a directory we're standing in. Step the process out to the
    // workspaces parent first (the shell is rescued to `new_path` via the
    // path-file afterwards). Harmless on Unix.
    if inside_target {
        std::env::set_current_dir(&wt_dir).map_err(|e| Error::Other(e.to_string()))?;
    }

    // Move worktree to new path (updates git's internal tracking)
    vcs::move_worktree(&old_path, &new_path)?;

    // Rename branch
    vcs::rename_branch(&old_branch, &args.new_branch)?;

    // Rename metadata file (find old with fallback, write new format)
    let old_meta = crate::meta::meta_path_with_fallback(&wt_dir, &old_branch);
    let new_meta = crate::meta::meta_path(&wt_dir, &args.new_branch);
    if old_meta.exists() {
        std::fs::rename(&old_meta, &new_meta).map_err(|e| {
            Error::Other(format!(
                "Failed to rename metadata {}: {e}",
                old_meta.display()
            ))
        })?;
    }

    eprintln!("Renamed {} -> {}", old_branch, args.new_branch);

    // If we were inside the renamed worktree, write new path for shell to cd
    if path_file.is_some() && inside_target {
        write_path_file(path_file, &new_path)?;
    }

    Ok(())
}
