// ===========================================================================
// terminal/gnome_terminal - GNOME Terminal (Linux) backend
// ===========================================================================
//
// Detection: `GNOME_TERMINAL_SERVICE` env var is set by GNOME Terminal
// since ~3.36 (the systemd-managed daemon variant). Older releases used
// `GNOME_TERMINAL_SCREEN`; we accept both.
//
// Spawn: `gnome-terminal --tab --title <T> --working-directory <D> --
//        bash -c "<script>"`. The `--` separator tells gnome-terminal
// that everything after it is the command, not more flags.
//
// **Untested from Windows dev environment** — review-only. Logic mirrors
// iTerm2's POSIX-shell flow.

use std::process::Command;

use super::{script, Error, Result, TabSpec, TerminalIntegration};

pub fn detect() -> Option<Box<dyn TerminalIntegration>> {
    let has_service = std::env::var("GNOME_TERMINAL_SERVICE")
        .ok()
        .is_some_and(|s| !s.is_empty());
    let has_screen = std::env::var("GNOME_TERMINAL_SCREEN")
        .ok()
        .is_some_and(|s| !s.is_empty());
    if has_service || has_screen {
        Some(Box::new(GnomeTerminal))
    } else {
        None
    }
}

pub struct GnomeTerminal;

impl TerminalIntegration for GnomeTerminal {
    fn name(&self) -> &'static str {
        "gnome-terminal"
    }

    fn open_tab(&self, spec: &TabSpec) -> Result<()> {
        let script_body = script::build_posix(spec);
        let cwd_str = spec.cwd.to_string_lossy().to_string();

        // GNOME Terminal's `--title` sets the initial title; we ALSO emit
        // an OSC 0 sequence at script start so the title sticks past the
        // first prompt repaint.
        let title_osc = format!(
            "printf '\\033]0;{}\\007'; ",
            spec.title.replace('\\', "\\\\").replace('\'', r"'\''")
        );
        let full_script = format!("{title_osc}{script_body}");

        let status = Command::new("gnome-terminal")
            .args([
                "--tab",
                "--title",
                &spec.title,
                "--working-directory",
                &cwd_str,
                "--",
                "bash",
                "-c",
                &full_script,
            ])
            .status()
            .map_err(|e| Error::Spawn(format!("gnome-terminal: {e}")))?;

        if !status.success() {
            return Err(Error::Spawn(format!(
                "gnome-terminal exit code {}",
                status.code().unwrap_or(-1)
            )));
        }
        Ok(())
    }
}
