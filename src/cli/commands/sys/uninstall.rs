// ===========================================================================
// wt uninstall - Remove shell integration
// ===========================================================================
//
// Symmetric inverse of `wt setup`: strips the `# === agent-workspace
// BEGIN/END ===` block from the user's shell rc file. Does NOT touch the
// install directory (`~/.agent-workspace/`), the binary itself, or any
// worktrees the user created — that's a deliberate scope limit so an
// accidental `wt uninstall` is recoverable (just re-run `wt setup`).
//
// For a full nuke (binary + install dir + PATH), use the standalone
// `uninstall.ps1` / `uninstall.sh` scripts shipped with each release.

use clap::Args;

use crate::cli::{Error, Result};
use crate::cli::commands::sys::setup::ShellArg;
use crate::shell::{self, Shell, UninstallOutcome};

#[derive(Args)]
pub struct UninstallArgs {
    /// Shell type (auto-detected if not specified)
    #[arg(long, value_enum)]
    shell: Option<ShellArg>,
}

pub fn run(args: UninstallArgs) -> Result<()> {
    let shell: Shell = if let Some(shell_arg) = args.shell {
        shell_arg.into()
    } else {
        Shell::detect()
            .ok_or_else(|| Error::Other("Cannot detect shell. Use --shell to specify.".into()))?
    };

    let config_path = shell
        .config_file()
        .map_err(|e| Error::Other(e.to_string()))?;

    let outcome = shell::uninstall(shell).map_err(|e| Error::Other(e.to_string()))?;

    match outcome {
        UninstallOutcome::Removed => {
            eprintln!("Shell integration removed from {}.", config_path.display());
            eprintln!();
            eprintln!("Restart your shell to drop the `wt` function. The binary itself");
            eprintln!("(and ~/.agent-workspace/) is untouched — re-run `wt setup` to");
            eprintln!("reinstate the wrapper. For a full uninstall, use the standalone");
            eprintln!("uninstall.ps1 / uninstall.sh from the GitHub release page.");
        }
        UninstallOutcome::NotInstalled => {
            eprintln!(
                "No agent-workspace shell integration found in {}.",
                config_path.display()
            );
            eprintln!("Nothing to remove.");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_uninstall_args_parse_with_shell() {
        use clap::Parser;
        #[derive(Parser)]
        struct TestCli {
            #[command(flatten)]
            args: UninstallArgs,
        }
        let cli = TestCli::try_parse_from(["test", "--shell", "bash"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_uninstall_args_parse_default() {
        use clap::Parser;
        #[derive(Parser)]
        struct TestCli {
            #[command(flatten)]
            args: UninstallArgs,
        }
        let cli = TestCli::try_parse_from(["test"]);
        assert!(cli.is_ok());
    }
}
