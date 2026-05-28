// ===========================================================================
// ws repo-info - Show or refresh the per-repo metadata cache
// ===========================================================================
//
// Prints the contents of the per-repo metadata TOML file (the one that
// `ws new` uses to skip the slow Phase-1 file-count scan). On first
// invocation against a repo — or whenever the cache is >30 days old —
// the file is created/refreshed by walking the working copy. Pass
// `--refresh` to force regeneration even when the existing cache is
// still inside its freshness window.
//
// Cache file location:
//   $AGENT_WORKSPACE_DIR/<project>-<hash>/.repo-meta.toml
//
// See `src/repo_meta.rs` for the schema and refresh semantics.

use clap::Args;

use crate::cli::{Error, Result};
use crate::config::Config;
use crate::repo_meta;
use crate::vcs;

#[derive(Args)]
pub struct RepoInfoArgs {
    /// Force regeneration even if the cache is still within its 30-day
    /// freshness window. Useful after a large rebase, merge, or pull
    /// when you want the `total_files` / `total_bytes` numbers (which
    /// drive `ws new` progress) to reflect the up-to-date repo state
    /// immediately rather than waiting for the next monthly refresh.
    #[arg(long)]
    refresh: bool,
}

pub fn run(args: RepoInfoArgs, config: &Config) -> Result<()> {
    // Resolve which repo we're in. Same lookup as every other `ws`
    // subcommand: the active VCS backend's `repo_root()` follows
    // git/jj worktree gitlinks back to the main repo.
    let repo_root = vcs::repo_root().map_err(|e| Error::Other(e.to_string()))?;
    let workspace_id = vcs::workspace_id().map_err(|e| Error::Other(e.to_string()))?;
    let project_dir = config.project_dir_for(&workspace_id);

    // The cache walks the *same* set of files that the cow-copy step
    // walks — top-level `.git/` always excluded; colocated repos also
    // exclude `.jj/` so the numbers match what `ws new` would actually
    // copy. Mirrors the exclude list in `vcs::git::worktree` and
    // `vcs::jj::worktree`.
    let is_colocated = repo_root.join(".jj").is_dir() && repo_root.join(".git").exists();
    let excludes: &[&str] = if is_colocated {
        &[".git", ".jj"]
    } else {
        &[".git"]
    };

    // Force-refresh: nuke the existing cache file so `load_or_refresh`
    // takes the recompute path even when the file would otherwise be
    // fresh. Cheaper than threading a `force` bool through the cache
    // API just for this one caller.
    let cache_path = project_dir.join(repo_meta::CACHE_FILENAME);
    if args.refresh && cache_path.exists() {
        std::fs::remove_file(&cache_path).map_err(|e| {
            Error::Other(format!(
                "failed to remove existing cache at {}: {e}",
                cache_path.display()
            ))
        })?;
    }

    // Make sure the project dir exists (first-time `ws repo-info` may
    // run before any `ws new` has materialised it).
    std::fs::create_dir_all(&project_dir).map_err(|e| {
        Error::Other(format!(
            "failed to create {}: {e}",
            project_dir.display()
        ))
    })?;

    eprintln!("Computing repository metadata...");
    eprintln!("  Source: {}", repo_root.display());
    eprintln!("  Cache:  {}", cache_path.display());

    let result =
        repo_meta::load_or_refresh(&project_dir, &repo_root, excludes).map_err(|e| {
            Error::Other(format!("repo-meta load/refresh failed: {e}"))
        })?;

    eprintln!();
    if result.from_cache {
        eprintln!("(loaded from cache — use --refresh to regenerate)");
    } else {
        eprintln!("Regenerated cache.");
    }
    eprintln!();

    // Pretty-print the TOML we just wrote (or read) so the user sees
    // exactly what's persisted, without needing to `cat` the file.
    print_meta(&result.meta);
    Ok(())
}

fn print_meta(meta: &repo_meta::RepoMeta) {
    use indicatif::HumanBytes;
    println!("[repository]");
    println!("name         = {:?}", meta.repo_name);
    println!(
        "origin       = {}",
        meta.origin
            .as_deref()
            .map(|s| format!("{s:?}"))
            .unwrap_or_else(|| "<none>".to_string())
    );
    println!(
        "github_repo  = {}",
        meta.github_repo
            .as_deref()
            .map(|s| format!("{s:?}"))
            .unwrap_or_else(|| "<none>".to_string())
    );
    println!("total_files  = {}", meta.total_files);
    println!(
        "total_bytes  = {}  # {}",
        meta.total_bytes,
        HumanBytes(meta.total_bytes)
    );
    println!("last_refresh = {}  # unix seconds", meta.last_refresh);
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        args: RepoInfoArgs,
    }

    #[test]
    fn parses_default() {
        let cli = TestCli::try_parse_from(["test"]);
        assert!(cli.is_ok());
        assert!(!cli.unwrap().args.refresh);
    }

    #[test]
    fn parses_refresh_flag() {
        let cli = TestCli::try_parse_from(["test", "--refresh"]);
        assert!(cli.is_ok());
        assert!(cli.unwrap().args.refresh);
    }
}
