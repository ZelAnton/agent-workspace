// ===========================================================================
// terminal - Multi-tab terminal integration for `wt new`
// ===========================================================================
//
// When the user runs `wt new` inside a terminal that supports tabs (Windows
// Terminal, iTerm2, GNOME Terminal), open a fresh tab titled with the
// branch name and run the worktree-creation flow there instead of in the
// originating shell. The originating shell prints a one-line confirmation
// and returns immediately.
//
// Detection is env-var-based — no subprocess calls — and silent: if no
// supported terminal is detected, callers fall through to the standard
// in-place creation flow.
//
// **Recursion guard**: the spawned tab inherits/sets `WT_SPAWNED_IN_TAB=1`
// before re-invoking `wt new`. Subsequent invocations see the guard and
// skip the spawn, doing the actual worktree creation. Without this, every
// `wt new` inside a spawned tab would open *another* tab ad infinitum.

use std::path::PathBuf;

mod gnome_terminal;
mod iterm2;
mod script;
mod windows_terminal;

#[cfg(test)]
mod tests;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to spawn terminal tab: {0}")]
    Spawn(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// What the spawned tab should run.
///
/// The script generated for the spawned tab does (in order):
///   1. set `WT_SPAWNED_IN_TAB=1` (the recursion guard)
///   2. invoke `binary new <args> --path-file <tmp>` with the given args
///   3. if `tmp` exists and is non-empty, `cd` to the first line
///   4. if a second line exists (snap mode), run the snap-resume loop
///
/// The binary path is captured at spawn time via [`std::env::current_exe`]
/// — on Windows this is critical because the system PATH may surface
/// Microsoft Store's `wt.exe` (Windows Terminal itself) before ours.
pub struct TabSpec {
    /// Title to set on the new tab (typically the branch/bookmark name).
    pub title: String,

    /// Working directory the new tab starts in. The worktree-cd happens
    /// AFTER creation (so the new tab ends up in the worktree); this is
    /// just the starting directory before our binary runs.
    pub cwd: PathBuf,

    /// Absolute path to our `wt` binary. Always use the absolute path —
    /// see the note above about `wt.exe` ambiguity on Windows.
    pub binary: PathBuf,

    /// CLI args to pass to the binary after `new` (e.g. `["feat-x"]` or
    /// `["feat-x", "--base", "main", "--snap", "claude"]`).
    pub args: Vec<String>,

    /// Whether the args include `--snap` (or `-s`). The spawned-tab
    /// script then includes the snap-resume loop so the agent flow runs
    /// natively in the new tab.
    pub is_snap: bool,
}

pub trait TerminalIntegration: Send + Sync {
    /// Stable identifier (used for logging / `--tab` flag debug output).
    fn name(&self) -> &'static str;

    /// Open a new tab and return immediately. The binary's `wt new` flow
    /// runs INSIDE the new tab; the caller (the originating shell) just
    /// gets back the spawn result.
    fn open_tab(&self, spec: &TabSpec) -> Result<()>;
}

/// Env var marking a process already spawned via [`TerminalIntegration::open_tab`].
/// Set inside the spawned shell script; checked by the binary's `wt new`
/// dispatch to skip re-spawning.
pub const SPAWNED_IN_TAB_ENV: &str = "WT_SPAWNED_IN_TAB";

/// True if this process is running inside an already-spawned terminal tab
/// (the spawn-loop recursion guard is set).
pub fn is_spawned_in_tab() -> bool {
    std::env::var(SPAWNED_IN_TAB_ENV)
        .ok()
        .is_some_and(|v| !v.is_empty())
}

/// Detect which terminal-with-tabs (if any) the current process is running
/// inside. Returns `None` when no supported terminal is detected — caller
/// should fall through to in-place creation.
///
/// Detection order: Windows Terminal → iTerm2 → GNOME Terminal. The order
/// only matters if multiple env vars are present (rare); the practical
/// effect is that the first match wins.
pub fn detect() -> Option<Box<dyn TerminalIntegration>> {
    if let Some(t) = windows_terminal::detect() {
        return Some(t);
    }
    if let Some(t) = iterm2::detect() {
        return Some(t);
    }
    if let Some(t) = gnome_terminal::detect() {
        return Some(t);
    }
    None
}
