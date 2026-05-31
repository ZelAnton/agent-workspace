// ===========================================================================
// cli - Command Line Interface
// ===========================================================================

mod commands;
pub mod output;

use std::path::Path;

use clap::{CommandFactory, Parser, Subcommand};

use crate::config::Config;
pub use output::OutputFormat;

/// Write path to file for shell integration
pub fn write_path_file(path_file: Option<&Path>, path: &Path) -> Result<()> {
    if let Some(file) = path_file {
        std::fs::write(file, path.display().to_string())
            .map_err(|e| Error::Other(format!("failed to write path file: {}", e)))?;
    }
    Ok(())
}

/// Write multiple lines to path file (for snap mode)
pub fn write_path_file_lines(path_file: Option<&Path>, lines: &[&str]) -> Result<()> {
    if let Some(file) = path_file {
        std::fs::write(file, lines.join("\n"))
            .map_err(|e| Error::Other(format!("failed to write path file: {}", e)))?;
    }
    Ok(())
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("config error: {0}")]
    Config(#[from] crate::config::Error),

    #[error("{0}")]
    Git(#[from] crate::vcs::Error),

    #[error("not in a git repository")]
    NotInRepo,

    #[error("{0}")]
    Other(String),
}

#[derive(Parser)]
#[command(
    name = "ws",
    version,
    about = "Git worktree workflow tool for AI agents",
    after_help = "Run 'ws setup' to install shell integration for cd/new commands."
)]
pub struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Write target path to file (for shell integration)
    #[arg(long, global = true, hide = true, value_name = "FILE")]
    path_file: Option<std::path::PathBuf>,

    /// Force a specific VCS backend (overrides config and auto-detection).
    /// `auto` (default) detects from `.git`/`.jj` presence — colocated
    /// repos prefer jj. Hidden from short help; rarely needed outside
    /// debugging.
    #[arg(long, global = true, hide_short_help = true, value_enum, default_value = "auto")]
    vcs: crate::vcs::VcsChoice,

    /// Output format. `human` (default) is the aligned/labelled text; `json`
    /// emits a single machine-readable object on stdout (progress/notices stay
    /// on stderr). Honored by `ls`, `status`, `repo-info`, `new`, and `merge`.
    #[arg(long, global = true, value_enum, default_value_t = OutputFormat::Human)]
    format: OutputFormat,
}

#[derive(Subcommand)]
enum Command {
    /// Create a new worktree and switch to it
    New(commands::NewArgs),

    /// List all worktrees for this project
    Ls(commands::LsArgs),

    /// Switch to a worktree directory (no args = return to main repo)
    Cd(commands::CdArgs),

    /// Remove a worktree and its branch
    Rm(commands::RmArgs),

    /// Remove worktrees with no diff from trunk
    Clean(commands::CleanArgs),

    /// Merge current worktree into trunk
    Merge(commands::MergeArgs),

    /// Show current worktree information
    Status,

    /// Show or refresh the per-repo metadata cache (file count, size,
    /// origin URL, GitHub slug). Used by `ws new` to skip the Phase-1
    /// scan; auto-refreshed every 30 days. Pass `--refresh` to force.
    RepoInfo(commands::RepoInfoArgs),

    /// Get / set / unset / list per-repo settings in the local
    /// `.workspace.toml` (legacy `.agent-workspace.toml` read as a fallback).
    /// Currently supported keys: `workspace.alias` (string),
    /// `workspace.use_path_hash` (bool).
    /// Run `ws config list` for the up-to-date roster + descriptions.
    Config(commands::ConfigArgs),

    /// Manage `[copy] exclude` patterns — gitignore-style entries the
    /// CoW path skips when copying the source repo into a new
    /// worktree (`target/`, `node_modules/`, `**/*.iso`, etc).
    /// Positional args ADD; `--remove` drops; `--list` shows; `--clear`
    /// wipes. No args = TUI tree picker.
    Exclude(commands::ExcludeArgs),

    /// Sync current worktree from trunk
    Sync(commands::SyncArgs),

    /// Rename a worktree branch
    Mv(commands::MoveArgs),

    /// Install shell integration (bash/zsh/fish)
    Setup(commands::SetupArgs),

    /// Remove shell integration (inverse of `ws setup`)
    Uninstall(commands::UninstallArgs),

    /// Create .agent-workspace.toml config file
    Init(commands::InitArgs),

    /// Update to the latest version
    Update,

    /// Continue snap mode after agent exits (internal use)
    #[command(hide = true)]
    SnapContinue,
}

/// Build clap Command for CompleteEnv (completion generation)
pub fn build_command() -> clap::Command {
    Cli::command()
}

impl Cli {
    /// Whether the passive "a new version is available" background check
    /// should run for this invocation.
    ///
    /// Suppressed for the commands that *are* the update flow: `ws update`
    /// re-execs `ws setup` from the freshly-replaced binary, so without
    /// this guard a single `ws update` prints the upgrade notice up to
    /// twice (once from the update process, once from the spawned setup
    /// child) on top of the command's own output.
    pub fn should_check_for_updates(&self) -> bool {
        !matches!(self.command, Command::Update | Command::Setup(_))
    }

    /// The selected output format. `main` reads this before `run` (which
    /// consumes `self`) so the top-level error path can render JSON errors.
    pub fn format(&self) -> OutputFormat {
        self.format
    }

    pub fn run(self) -> Result<()> {
        let config = Config::load()?;
        let path_file = self.path_file.as_deref();
        let format = self.format;

        // Install the VCS backend once, before any command dispatches.
        // Precedence: CLI flag > project config > global config > detect.
        // Subsequent calls to `crate::vcs::*` resolve through the thread-local.
        crate::vcs::set_backend(crate::vcs::resolve_backend(
            self.vcs,
            config.vcs,
            config.vcs_global,
        ));

        match self.command {
            Command::New(args) => commands::lifecycle::new::run(args, &config, path_file, format),
            Command::Ls(args) => commands::ls::run(args, &config, format),
            Command::Cd(args) => commands::nav::cd::run(args, &config, path_file),
            Command::Rm(args) => commands::lifecycle::rm::run(args, &config, path_file),
            Command::Clean(args) => commands::lifecycle::clean::run(args, &config, path_file),
            Command::Merge(args) => commands::merge::run(args, &config, path_file, format),
            Command::Status => commands::status::run(&config, format),
            Command::RepoInfo(args) => commands::repo_info::run(args, &config, format),
            Command::Config(args) => commands::config::run(args),
            Command::Exclude(args) => commands::exclude::run(args),
            Command::Sync(args) => commands::sync::run(args, &config),
            Command::Mv(args) => commands::r#move::run(args, &config, path_file),
            Command::Setup(args) => commands::sys::setup::run(args),
            Command::Uninstall(args) => commands::sys::uninstall::run(args),
            Command::Init(args) => commands::sys::init::run(args),
            Command::Update => commands::sys::update::run(),
            Command::SnapContinue => commands::snap::resume::run(&config, path_file),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let err = Error::NotInRepo;
        assert_eq!(err.to_string(), "not in a git repository");

        let err = Error::Other("custom error".to_string());
        assert_eq!(err.to_string(), "custom error");
    }

    #[test]
    fn test_cli_parse_help() {
        // Verify CLI can parse --help without panicking
        let result = Cli::try_parse_from(["ws", "--help"]);
        assert!(result.is_err()); // --help causes early exit
    }

    #[test]
    fn test_cli_parse_version() {
        let result = Cli::try_parse_from(["ws", "--version"]);
        assert!(result.is_err()); // --version causes early exit
    }

    #[test]
    fn test_cli_parse_new() {
        let cli = Cli::try_parse_from(["ws", "new", "feature-branch"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_new_with_base() {
        let cli = Cli::try_parse_from(["ws", "new", "feature", "--base", "develop"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_ls() {
        let cli = Cli::try_parse_from(["ws", "ls"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_ls_long() {
        let cli = Cli::try_parse_from(["ws", "ls", "-l"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_ls_long_full() {
        let cli = Cli::try_parse_from(["ws", "ls", "--long"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_cd() {
        let cli = Cli::try_parse_from(["ws", "cd", "branch-name"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_cd_no_args() {
        let cli = Cli::try_parse_from(["ws", "cd"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_rm() {
        let cli = Cli::try_parse_from(["ws", "rm", "branch"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_rm_force() {
        let cli = Cli::try_parse_from(["ws", "rm", "branch", "--force"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_clean() {
        let cli = Cli::try_parse_from(["ws", "clean"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_clean_dry_run() {
        let cli = Cli::try_parse_from(["ws", "clean", "--dry-run"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_merge() {
        let cli = Cli::try_parse_from(["ws", "merge"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_merge_with_strategy() {
        let cli = Cli::try_parse_from(["ws", "merge", "--strategy", "squash"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_status() {
        let cli = Cli::try_parse_from(["ws", "status"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_sync() {
        let cli = Cli::try_parse_from(["ws", "sync"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_sync_from() {
        let cli = Cli::try_parse_from(["ws", "sync", "--from", "develop"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_mv() {
        let cli = Cli::try_parse_from(["ws", "mv", "old", "new"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_setup() {
        let cli = Cli::try_parse_from(["ws", "setup"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_setup_with_shell() {
        let cli = Cli::try_parse_from(["ws", "setup", "--shell", "bash"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_uninstall() {
        let cli = Cli::try_parse_from(["ws", "uninstall"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_uninstall_with_shell() {
        let cli = Cli::try_parse_from(["ws", "uninstall", "--shell", "powershell"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_init() {
        let cli = Cli::try_parse_from(["ws", "init"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_init_with_trunk() {
        let cli = Cli::try_parse_from(["ws", "init", "--trunk", "develop"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_init_with_merge_strategy() {
        let cli = Cli::try_parse_from(["ws", "init", "--merge-strategy", "merge"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_init_with_copy_files() {
        let cli = Cli::try_parse_from([
            "ws",
            "init",
            "--copy-files",
            ".env",
            "--copy-files",
            ".env.*",
        ]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_with_path_file() {
        let cli = Cli::try_parse_from(["ws", "--path-file", "/tmp/test", "cd"]);
        assert!(cli.is_ok());
        let cli = cli.unwrap();
        assert_eq!(cli.path_file, Some(std::path::PathBuf::from("/tmp/test")));
    }

    #[test]
    fn test_cli_parse_new_with_snap() {
        let cli = Cli::try_parse_from(["ws", "new", "-s", "claude"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_new_with_snap_long() {
        let cli = Cli::try_parse_from(["ws", "new", "--snap", "claude"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_new_with_snap_and_branch() {
        let cli = Cli::try_parse_from(["ws", "new", "my-branch", "-s", "agent"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_update() {
        let cli = Cli::try_parse_from(["ws", "update"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_update_flow_commands_skip_update_check() {
        // The passive "new version available" notice must not fire for the
        // commands that make up the update flow, or `ws update` prints it
        // twice (once here, once from the re-execed `ws setup`).
        let update = Cli::try_parse_from(["ws", "update"]).unwrap();
        assert!(!update.should_check_for_updates());

        let setup = Cli::try_parse_from(["ws", "setup"]).unwrap();
        assert!(!setup.should_check_for_updates());

        // Ordinary commands still get the daily nudge.
        let ls = Cli::try_parse_from(["ws", "ls"]).unwrap();
        assert!(ls.should_check_for_updates());
    }

    #[test]
    fn test_cli_parse_snap_continue() {
        let cli = Cli::try_parse_from(["ws", "snap-continue"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_snap_continue_is_hidden() {
        // snap-continue should not appear in help
        let result = Cli::try_parse_from(["ws", "--help"]);
        // --help causes early exit but the command is still valid
        assert!(result.is_err());
    }
}
