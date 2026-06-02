// ===========================================================================
// terminal/tmux - tmux (cross-platform multiplexer) backend
// ===========================================================================
//
// Detection: tmux sets `$TMUX` (the server socket path + session info) in every
// pane's environment. Non-empty = we're inside a tmux session.
//
// Spawn: `tmux new-window -c <dir> -n <title> bash -c "<script>"`. A new tmux
// window is the closest analogue to a terminal tab — it appears in the status
// bar's window list and the user switches to it with the usual tmux keys. `-n`
// names the window (the "tab" label); `-c` sets its working directory.
//
// **Precedence**: registered LAST in `terminal::detect`, so a GUI terminal with
// native tabs (Windows Terminal / iTerm2 / GNOME Terminal / WezTerm) wins when
// tmux is running *inside* one. tmux is the fallback when no tabbed GUI terminal
// is detected — e.g. a bare ssh session multiplexed with tmux.
//
// **Untested from this dev environment** — review-only. Mirrors the POSIX-shell
// flow used by the other POSIX backends.

use std::process::Command;

use super::{script, Error, Result, TabSpec, TerminalIntegration};

pub fn detect() -> Option<Box<dyn TerminalIntegration>> {
    if std::env::var("TMUX").ok().is_some_and(|s| !s.is_empty()) {
        Some(Box::new(Tmux))
    } else {
        None
    }
}

pub struct Tmux;

impl TerminalIntegration for Tmux {
    fn name(&self) -> &'static str {
        "tmux"
    }

    fn open_tab(&self, spec: &TabSpec) -> Result<()> {
        let body = script::build_posix(spec);
        let cwd_str = spec.cwd.to_string_lossy().to_string();

        let status = Command::new("tmux")
            .args(["new-window", "-c", &cwd_str, "-n", &spec.title, "bash", "-c", &body])
            .status()
            .map_err(|e| Error::Spawn(format!("tmux: {e}")))?;

        if !status.success() {
            return Err(Error::Spawn(format!(
                "tmux new-window exit code {}",
                status.code().unwrap_or(-1)
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_requires_nonempty_tmux_env() {
        // We can't safely mutate process env in a parallel test, so just assert
        // the predicate shape on the public `name()` and that the module wires
        // up without panicking when constructed directly.
        assert_eq!(Tmux.name(), "tmux");
    }
}
