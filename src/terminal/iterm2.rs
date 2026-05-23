// ===========================================================================
// terminal/iterm2 - iTerm2 (macOS) backend
// ===========================================================================
//
// Detection: `TERM_PROGRAM=iTerm.app` (set by iTerm2 in every spawned
// shell; the canonical, stable identifier).
//
// Spawn: iTerm2 doesn't have a CLI subcommand for new-tab, so we drive
// it via AppleScript through `osascript`. The script:
//   1. Creates a new tab in the current iTerm2 window.
//   2. Sends two `write text` commands to the new tab's session:
//      first sets the tab title via the OSC 0/2 escape, then runs our
//      shell script (the same one bash/zsh would run for GNOME Terminal).
//
// **Untested from Windows dev environment** — review-only. Logic mirrors
// GNOME Terminal's POSIX-shell script flow.

use std::process::Command;

use super::{script, Error, Result, TabSpec, TerminalIntegration};

pub fn detect() -> Option<Box<dyn TerminalIntegration>> {
    let prog = std::env::var("TERM_PROGRAM").ok()?;
    if prog == "iTerm.app" {
        Some(Box::new(ITerm2))
    } else {
        None
    }
}

pub struct ITerm2;

impl TerminalIntegration for ITerm2 {
    fn name(&self) -> &'static str {
        "iterm2"
    }

    fn open_tab(&self, spec: &TabSpec) -> Result<()> {
        let script_body = script::build_posix(spec);
        let applescript = build_applescript(&spec.title, &spec.cwd.to_string_lossy(), &script_body);

        let status = Command::new("osascript")
            .arg("-e")
            .arg(&applescript)
            .status()
            .map_err(|e| Error::Spawn(format!("osascript: {e}")))?;

        if !status.success() {
            return Err(Error::Spawn(format!(
                "osascript exit code {}",
                status.code().unwrap_or(-1)
            )));
        }
        Ok(())
    }
}

/// Build the AppleScript driving iTerm2.
///
/// Two layers of escaping:
///   1. `as_escape` for AppleScript string literals (handles `\` and `"`).
///   2. The shell script ITSELF is the inner payload — it's already
///      shell-quoted by the caller, but its body becomes an AppleScript
///      string literal here, so we double-escape backslashes / quotes.
fn build_applescript(title: &str, cwd: &str, shell_script: &str) -> String {
    fn as_escape(s: &str) -> String {
        s.replace('\\', "\\\\").replace('"', "\\\"")
    }

    let title_esc = as_escape(title);
    let cwd_esc = as_escape(cwd);
    let script_esc = as_escape(shell_script);

    // The OSC 0 sequence (`ESC ] 0 ; <title> BEL`) sets both icon and
    // window title. iTerm2 honours it. We emit it via `printf` so the
    // user-visible tab title matches the requested branch name.
    format!(
        "tell application \"iTerm\"\n\
         \ttell current window\n\
         \t\tset newTab to (create tab with default profile)\n\
         \t\ttell current session of newTab\n\
         \t\t\twrite text \"cd \\\"{cwd_esc}\\\" && printf '\\\\033]0;{title_esc}\\\\007' && {script_esc}\"\n\
         \t\tend tell\n\
         \tend tell\n\
         end tell"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applescript_escapes_quotes_and_backslashes() {
        let s = build_applescript("ti\"tle", "/tmp", "echo hi");
        // Title's `"` becomes `\"` in the AppleScript string literal.
        assert!(s.contains(r#"ti\"tle"#));
    }

    #[test]
    fn applescript_includes_cd_and_osc_title() {
        let s = build_applescript("feat-x", "/repo", "echo go");
        assert!(s.contains(r#"cd \"/repo\""#));
        // OSC 0 ... BEL sequence: octal escape \033 → bel \007.
        assert!(s.contains("\\\\033]0;feat-x\\\\007"));
    }
}
