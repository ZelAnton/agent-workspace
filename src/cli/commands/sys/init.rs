// ===========================================================================
// ws init - Initialize project configuration
// ===========================================================================

use clap::Args;
use clap_complete::engine::ArgValueCompleter;

use crate::cli::{Error, Result};
use crate::complete;
use crate::config::{MergeStrategy, ProjectConfig, SyncStrategy};

#[derive(Args)]
pub struct InitArgs {
    /// Main branch name (auto-detected: main > master)
    #[arg(long, value_name = "BRANCH", add = ArgValueCompleter::new(complete::complete_branches))]
    trunk: Option<String>,

    /// Default merge strategy
    #[arg(long, value_enum, value_name = "STRATEGY")]
    merge_strategy: Option<MergeStrategy>,

    /// Default sync strategy
    #[arg(long, value_enum, value_name = "STRATEGY")]
    sync_strategy: Option<SyncStrategy>,

    /// Files to copy from main repo to new worktrees (can be repeated)
    #[arg(long, value_name = "PATTERN")]
    copy_files: Vec<String>,
}

pub async fn run(args: InitArgs, repo: &crate::vcs::Repo) -> Result<()> {
    // Anchor the config at the MAIN repo root — never the process cwd. Run from
    // a subdirectory, a cwd-relative write would land the file where
    // `Config::load` (which resolves the repo root independently) never reads
    // it. `ws init` deliberately writes the *committed, team-shared*
    // `.agent-workspace.toml` (this repo ships one), distinct from the local,
    // git-excluded `.workspace.toml` that `ws config`/`ws exclude` manage — so
    // it is NOT auto-excluded here.
    let repo_root = repo.repo_root().await.map_err(|e| Error::Other(e.to_string()))?;
    let config_path = repo_root.join(crate::config::LEGACY_PROJECT_CONFIG_FILENAME);

    if config_path.exists() {
        return Err(Error::Other("Config file already exists".into()));
    }

    // Detect trunk if not specified
    let trunk = match args.trunk {
        Some(t) => t,
        None => repo.detect_trunk().await.unwrap_or_else(|_| "main".into()),
    };

    let mut config = ProjectConfig::default();
    config.general.trunk = Some(trunk.clone());
    config.general.merge_strategy = args.merge_strategy;
    config.general.sync_strategy = args.sync_strategy;
    if !args.copy_files.is_empty() {
        config.general.copy_files = args.copy_files;
    }

    let content = toml::to_string_pretty(&config).map_err(|e| Error::Other(e.to_string()))?;

    std::fs::write(&config_path, content).map_err(|e| Error::Other(e.to_string()))?;

    eprintln!("Created {}", config_path.display());
    eprintln!("Trunk branch: {trunk}");
    if let Some(ref strategy) = config.general.merge_strategy {
        eprintln!("Merge strategy: {strategy:?}");
    }
    if let Some(ref strategy) = config.general.sync_strategy {
        eprintln!("Sync strategy: {strategy:?}");
    }
    if !config.general.copy_files.is_empty() {
        eprintln!("Copy files: {}", config.general.copy_files.join(", "));
    }

    Ok(())
}
