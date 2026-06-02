// ===========================================================================
// ws clean - Clean up worktrees with no diff from trunk
// ===========================================================================

use std::collections::HashSet;
use std::path::Path;

use clap::Args;
use serde::Serialize;

use crate::cli::output::{self, OutputFormat};
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

/// Machine-facing `ws clean` result (json mode). `cleaned` lists branches
/// removed (or, under `dry_run`, that would be removed); `skipped_dirty`
/// lists branches kept because of uncommitted changes.
#[derive(Serialize)]
struct CleanResult {
    dry_run: bool,
    cleaned: Vec<String>,
    skipped_dirty: Vec<String>,
    returned_to: Option<String>,
}

pub async fn run(args: CleanArgs, config: &Config, path_file: Option<&Path>, format: OutputFormat, repo: &vcs::Repo) -> Result<()> {
    // Get main repo path before any operations
    let main_path = repo.repo_root().await?;
    let workspace_id = repo.workspace_id().await?;
    let wt_dir = config.project_dir_for(&workspace_id);

    if !wt_dir.exists() {
        eprintln!("No worktrees to clean.");
        output::emit_json(
            &CleanResult {
                dry_run: args.dry_run,
                cleaned: Vec::new(),
                skipped_dirty: Vec::new(),
                returned_to: None,
            },
            format,
        );
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
    let mut cleaned: Vec<String> = Vec::new();
    let mut checked = 0;
    let mut skipped_dirty: Vec<String> = Vec::new();
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
            skipped_dirty.push(branch.clone());
            continue;
        }

        if args.dry_run {
            eprintln!("Would clean (no diff from {target}): {branch}");
            cleaned.push(branch.clone());
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

        cleaned.push(branch.clone());

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
    } else if cleaned.is_empty() {
        eprintln!("No worktrees to clean (all have changes).");
    } else {
        eprintln!("{} worktree(s) {verb}.", cleaned.len());
    }
    if !skipped_dirty.is_empty() {
        eprintln!("{} worktree(s) skipped due to uncommitted changes.", skipped_dirty.len());
    }

    // Write main repo path for shell to cd if we were inside a cleaned worktree
    let returned_to = if !args.dry_run && path_file.is_some() && cleaned_current {
        write_path_file(path_file, &main_path)?;
        Some(main_path.display().to_string())
    } else {
        None
    };

    output::emit_json(
        &CleanResult {
            dry_run: args.dry_run,
            cleaned,
            skipped_dirty,
            returned_to,
        },
        format,
    );
    Ok(())
}
