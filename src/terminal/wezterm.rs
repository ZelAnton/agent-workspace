// ===========================================================================
// terminal/wezterm - WezTerm (cross-platform) backend
// ===========================================================================
//
// Detection: WezTerm sets `TERM_PROGRAM=WezTerm` in every spawned shell, and
// also exports `WEZTERM_PANE` (the pane id). We accept either — `TERM_PROGRAM`
// is the canonical identifier; `WEZTERM_PANE` is a robust fallback for shells
// that scrub `TERM_PROGRAM`.
//
// Spawn: `wezterm cli spawn --cwd <dir> -- bash -c "<script>"`. `wezterm cli`
// talks to the running GUI over its mux socket, so a new tab opens in the
// current window. The `--` separator hands the rest to the spawned program.
// Cross-platform (Windows / macOS / Linux): the `wezterm` binary is the same
// everywhere, so this one backend covers WezTerm on every OS.
//
// **Untested from this dev environment** — review-only. Mirrors the POSIX-shell
// flow used by the iTerm2 / GNOME Terminal backends.

use std::process::Command;

use super::{script, Error, Result, TabSpec, TerminalIntegration};

pub fn detect() -> Option<Box<dyn TerminalIntegration>> {
    let is_wezterm = std::env::var("TERM_PROGRAM")
        .ok()
        .is_some_and(|p| p == "WezTerm")
        || std::env::var("WEZTERM_PANE")
            .ok()
            .is_some_and(|s| !s.is_empty());
    if is_wezterm {
        Some(Box::new(WezTerm))
    } else {
        None
    }
}

pub struct WezTerm;

impl TerminalIntegration for WezTerm {
    fn name(&self) -> &'static str {
        "wezterm"
    }

    fn open_tab(&self, spec: &TabSpec) -> Result<()> {
        let full_script = build_titled_script(spec);
        let cwd_str = spec.cwd.to_string_lossy().to_string();

        let status = Command::new("wezterm")
            .args(["cli", "spawn", "--cwd", &cwd_str, "--", "bash", "-c", &full_script])
            .status()
            .map_err(|e| Error::Spawn(format!("wezterm: {e}")))?;

        if !status.success() {
            return Err(Error::Spawn(format!(
                "wezterm cli spawn exit code {}",
                status.code().unwrap_or(-1)
            )));
        }
        Ok(())
    }
}

/// Prefix the spawned-tab script with an OSC 0 title set, so the tab title
/// shows the branch name and sticks past the first prompt repaint. The title
/// is embedded in a bash single-quoted `printf` — escaped accordingly.
fn build_titled_script(spec: &TabSpec) -> String {
    let body = script::build_posix(spec);
    let title_osc = format!(
        "printf '\\033]0;{}\\007'; ",
        script::escape_for_printf_single_quoted(&spec.title)
    );
    format!("{title_osc}{body}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{TabMode, TabSpec};
    use std::path::PathBuf;

    fn spec() -> TabSpec {
        TabSpec {
            title: "feat-x".into(),
            cwd: PathBuf::from("/repo"),
            mode: TabMode::OpenAtCwd,
        }
    }

    #[test]
    fn titled_script_sets_osc_title() {
        let s = build_titled_script(&spec());
        // OSC 0 ; <title> BEL, emitted via printf octal escapes.
        assert!(s.contains("\\033]0;feat-x\\007"));
    }

    #[test]
    fn title_with_percent_is_neutralised() {
        let mut sp = spec();
        sp.title = "100%-done".into();
        let s = build_titled_script(&sp);
        assert!(s.contains("100%%-done"), "printf `%` must be doubled");
    }
}
