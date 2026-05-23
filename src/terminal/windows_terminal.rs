// ===========================================================================
// terminal/windows_terminal - Windows Terminal (Microsoft Store) backend
// ===========================================================================
//
// Detection: `WT_SESSION` env var is set by Windows Terminal in every
// shell it spawns. Non-empty value = inside WT.
//
// Spawn: invoke `wt.exe new-tab` (the WT command). The argument layout:
//
//   wt new-tab \
//       --title <title> \
//       --suppressApplicationTitle \         (stops apps from changing it)
//       -d <starting-directory> \
//       pwsh -NoExit -Command <script>
//
// The `pwsh -NoExit -Command ...` keeps the tab open after the script
// finishes. `-NoExit` is critical — without it, the tab closes the moment
// the creation script returns, dropping the user back to ground.

use std::path::PathBuf;
use std::process::Command;

use super::{script, Error, Result, TabSpec, TerminalIntegration};

pub fn detect() -> Option<Box<dyn TerminalIntegration>> {
    if std::env::var("WT_SESSION")
        .ok()
        .filter(|s| !s.is_empty())
        .is_some()
    {
        Some(Box::new(WindowsTerminal))
    } else {
        None
    }
}

pub struct WindowsTerminal;

impl TerminalIntegration for WindowsTerminal {
    fn name(&self) -> &'static str {
        "windows-terminal"
    }

    fn open_tab(&self, spec: &TabSpec) -> Result<()> {
        let ps_command = script::build_pwsh(spec);
        let cwd_str = spec.cwd.to_string_lossy().to_string();

        // Find `wt.exe`. It SHOULD be on PATH inside a Windows Terminal
        // session, but the Microsoft Store WindowsApps directory has
        // unusual permissions and the `where` lookup occasionally misses
        // it. Try a few known locations.
        let wt_bin = locate_wt_binary().ok_or_else(|| {
            Error::Spawn(
                "wt.exe (Windows Terminal) not found on PATH or in WindowsApps".into(),
            )
        })?;

        let status = Command::new(&wt_bin)
            .args([
                "new-tab",
                "--title",
                &spec.title,
                "--suppressApplicationTitle",
                "-d",
                &cwd_str,
                "pwsh",
                "-NoExit",
                "-Command",
                &ps_command,
            ])
            .status()
            .map_err(|e| Error::Spawn(format!("wt.exe new-tab: {e}")))?;

        if !status.success() {
            return Err(Error::Spawn(format!(
                "wt.exe new-tab exit code {}",
                status.code().unwrap_or(-1)
            )));
        }
        Ok(())
    }
}

/// Locate Windows Terminal's `wt.exe`.
///
/// On a healthy Microsoft Store install, `wt.exe` is symlinked into
/// `%LOCALAPPDATA%\Microsoft\WindowsApps`, which is usually first in PATH
/// — so `Command::new("wt")` would find it. But we deliberately avoid the
/// bare `wt` lookup because **our own binary is named `wt.exe`** and may
/// also be on PATH (installed by `wt setup` / npm). The collision is
/// real: if our binary is found first, `Command::new("wt")` would
/// recursively invoke us, not Windows Terminal.
///
/// Strategy:
///   1. Walk PATH, find an entry whose `wt.exe` is *not* our own binary.
///   2. Fall back to the WindowsApps default location.
fn locate_wt_binary() -> Option<PathBuf> {
    let our_exe = std::env::current_exe().ok().and_then(|p| p.canonicalize().ok());

    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join("wt.exe");
            if !candidate.is_file() {
                continue;
            }
            let canonical = candidate.canonicalize().ok();
            if canonical.is_some() && canonical == our_exe {
                continue; // skip our own binary
            }
            return Some(candidate);
        }
    }

    // Fall back: %LOCALAPPDATA%\Microsoft\WindowsApps\wt.exe
    if let Ok(local_appdata) = std::env::var("LOCALAPPDATA") {
        let fallback = PathBuf::from(local_appdata)
            .join("Microsoft")
            .join("WindowsApps")
            .join("wt.exe");
        if fallback.is_file() {
            return Some(fallback);
        }
    }

    None
}
