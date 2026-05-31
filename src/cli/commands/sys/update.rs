// ===========================================================================
// cli/commands/update - Self-update Command (channel-aware)
// ===========================================================================
//
// `ws update` reads the install_channel marker in the base dir and dispatches:
//   - Channel::Npm   → `npm install -g @zelanton/agent-workspace@latest`
//   - Channel::Shell → download from GitHub Releases + atomic self-replace
//                      + re-run `ws setup` for any wrapper template changes
//
// Marker is missing for legacy installs (defaults to Npm). New installs stamp
// the marker explicitly: npm via install.js postinstall, shell via install.sh
// / install.ps1.

use crate::cli;
use crate::config::Config;
use crate::update::{self, Channel};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 更新行为：纯逻辑，不涉及 IO
#[derive(Debug)]
pub enum UpdateAction {
    AlreadyUpToDate,
    UpdateAvailable(String),
}

/// 根据版本检查结果决定行为
pub fn determine_action(check_result: update::Result<Option<String>>) -> cli::Result<UpdateAction> {
    match check_result {
        Ok(Some(latest)) => Ok(UpdateAction::UpdateAvailable(latest)),
        Ok(None) => Ok(UpdateAction::AlreadyUpToDate),
        Err(e) => Err(cli::Error::Other(format!(
            "failed to check for updates: {e}"
        ))),
    }
}

/// 构造 npm install 命令参数
///
/// The published package is SCOPED (`@zelanton/agent-workspace`) — see
/// `npm/agent-workspace/package.json`. An unscoped `agent-workspace@latest`
/// would target the wrong (or a non-existent) package, so the npm-channel
/// update path must use the full scoped name.
pub fn npm_install_args() -> Vec<&'static str> {
    vec!["install", "-g", "@zelanton/agent-workspace@latest"]
}

pub fn run() -> cli::Result<()> {
    eprintln!("Checking for updates...");

    let action = determine_action(update::check_update(VERSION))?;

    let latest = match action {
        UpdateAction::AlreadyUpToDate => {
            eprintln!("Already up to date ({})", VERSION);
            return Ok(());
        }
        UpdateAction::UpdateAvailable(v) => v,
    };

    eprintln!("Updating agent-workspace: {} -> {}", VERSION, latest);

    let base_dir = Config::base_dir().map_err(|e| cli::Error::Other(e.to_string()))?;
    let channel = update::detect_channel(&base_dir);

    match channel {
        Channel::Npm => run_npm_update()?,
        Channel::Shell => run_shell_update(&latest)?,
    }

    eprintln!("Updated successfully!");
    Ok(())
}

fn run_npm_update() -> cli::Result<()> {
    let status = std::process::Command::new("npm")
        .args(npm_install_args())
        .status()
        .map_err(|e| cli::Error::Other(format!("failed to run npm: {e}")))?;

    if !status.success() {
        return Err(cli::Error::Other("npm install failed".into()));
    }
    Ok(())
}

fn run_shell_update(latest: &str) -> cli::Result<()> {
    // self_update atomically replaces the currently running binary.
    update::self_update(latest)
        .map_err(|e| cli::Error::Other(format!("self-update failed: {e}")))?;

    // Re-run `ws setup` from the new binary so any wrapper template changes
    // propagate. The current process is the *old* binary — spawn a subprocess
    // pointing at the path of the running exe (which now contains the new
    // bytes after self_replace).
    if let Ok(current_exe) = std::env::current_exe() {
        let _ = std::process::Command::new(&current_exe)
            .arg("setup")
            .status();
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_determine_action_no_update() {
        let result = determine_action(Ok(None));
        assert!(matches!(result, Ok(UpdateAction::AlreadyUpToDate)));
    }

    #[test]
    fn test_determine_action_has_update() {
        let result = determine_action(Ok(Some("1.0.0".to_string())));
        match result {
            Ok(UpdateAction::UpdateAvailable(v)) => assert_eq!(v, "1.0.0"),
            _ => panic!("expected UpdateAvailable"),
        }
    }

    #[test]
    fn test_determine_action_network_error() {
        let result = determine_action(Err(update::Error::Network("timeout".into())));
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("failed to check for updates"));
    }

    #[test]
    fn test_npm_install_args() {
        let args = npm_install_args();
        assert_eq!(
            args,
            vec!["install", "-g", "@zelanton/agent-workspace@latest"]
        );
    }
}
