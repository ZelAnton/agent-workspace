// ===========================================================================
// terminal - Multi-tab terminal integration for `ws new`
// ===========================================================================
//
// When the user runs `ws new` inside a terminal that supports tabs (Windows
// Terminal, iTerm2, GNOME Terminal), open a fresh tab titled with the
// branch name and run the worktree-creation flow there instead of in the
// originating shell. The originating shell prints a one-line confirmation
// and returns immediately.
//
// Detection is env-var-based — no subprocess calls — and silent: if no
// supported terminal is detected, callers fall through to the standard
// in-place creation flow.
//
// **Recursion guard**: the spawned tab inherits/sets `WS_SPAWNED_IN_TAB=1`
// before re-invoking `ws new`. Subsequent invocations see the guard and
// skip the spawn, doing the actual worktree creation. Without this, every
// `ws new` inside a spawned tab would open *another* tab ad infinitum.

use std::path::PathBuf;

mod gnome_terminal;
mod iterm2;
mod script;
mod tmux;
mod wezterm;
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
/// Two distinct flows live behind one type:
///   - [`TabMode::WtNew`] — `ws new` creation: spawned tab re-invokes our
///     binary with the original args + a fresh `--path-file`, optionally
///     enters the snap-resume loop. The script body is substantial.
///   - [`TabMode::OpenAtCwd`] — `ws cd` navigation: spawned tab just
///     opens a shell at the target directory. The terminal's native cwd
///     flag (`wt.exe new-tab -d`, `gnome-terminal --working-directory`,
///     iTerm2's AppleScript) does the work; the script body only sets
///     the recursion-guard env and locks the tab title via OSC 0.
pub struct TabSpec {
    /// Title to set on the new tab (typically the branch/bookmark name).
    pub title: String,

    /// Working directory the new tab starts in. For `OpenAtCwd` this IS
    /// where the user ends up. For `WtNew` it's just the starting dir
    /// before the binary runs — the worktree-cd happens AFTER creation
    /// inside the spawned binary's `--path-file` dance.
    pub cwd: PathBuf,

    /// What the spawned tab actually does.
    pub mode: TabMode,
}

/// Distinguishes the two spawn flows. The script generator and each
/// terminal backend branch on this.
pub enum TabMode {
    /// Re-invoke `<binary> new <args> --path-file <tmp>` in the new tab.
    /// Includes the snap-resume loop when `is_snap` is true. The binary
    /// path is captured at spawn time via [`std::env::current_exe`] — on
    /// Windows this is critical because the system PATH may surface
    /// Microsoft Store's `wt.exe` (Windows Terminal itself) before ours.
    WtNew {
        binary: PathBuf,
        args: Vec<String>,
        is_snap: bool,
    },
    /// Just open a shell at [`TabSpec::cwd`]. No binary re-invocation, no
    /// `--path-file` dance — the terminal's native cwd flag handles it.
    /// Used by `ws cd <branch>`.
    OpenAtCwd,
}

pub trait TerminalIntegration: Send + Sync {
    /// Stable identifier (used for logging / `--tab` flag debug output).
    fn name(&self) -> &'static str;

    /// Open a new tab and return immediately. The binary's `ws new` flow
    /// runs INSIDE the new tab; the caller (the originating shell) just
    /// gets back the spawn result.
    fn open_tab(&self, spec: &TabSpec) -> Result<()>;
}

/// Env var marking a process already spawned via [`TerminalIntegration::open_tab`].
/// Set inside the spawned shell script; checked by the binary's `ws new`
/// dispatch to skip re-spawning.
pub const SPAWNED_IN_TAB_ENV: &str = "WS_SPAWNED_IN_TAB";

/// True if this process is running inside an already-spawned terminal tab
/// (the spawn-loop recursion guard is set).
pub fn is_spawned_in_tab() -> bool {
    std::env::var(SPAWNED_IN_TAB_ENV)
        .ok()
        .is_some_and(|v| !v.is_empty())
}

/// Shared precedence resolver for the `--in-new-tab` / `--no-tab` flags +
/// config toggle. Used by both `ws new` and `ws cd`:
///
///   1. `--no-tab` flag → false (user explicitly disabled)
///   2. Already running inside a spawned tab (recursion guard) → false
///   3. `--in-new-tab` flag → true (user explicitly enabled)
///   4. `config_value` (`[ui] open_in_new_tab` resolved project/global)
///
/// The recursion guard is checked BEFORE `--in-new-tab` (not after): once
/// `WS_SPAWNED_IN_TAB` is set, spawning is hard-disabled regardless of any
/// flag that leaked into the child's argv or env. This matches the
/// architectural invariant (AGENTS.md) and the integration-test helper, which
/// relies on the guard to unconditionally suppress tab spawning. In normal
/// flow the tab-control flags are stripped from the spawned tab's re-invocation
/// anyway (see `lifecycle::new::spawn_in_new_tab`), so this reordering is pure
/// defense-in-depth — it can't change behaviour for a non-guarded invocation.
pub fn should_open_in_new_tab(no_tab: bool, in_new_tab: bool, config_value: bool) -> bool {
    if no_tab {
        return false;
    }
    if is_spawned_in_tab() {
        return false;
    }
    if in_new_tab {
        return true;
    }
    config_value
}

/// Detect which terminal-with-tabs (if any) the current process is running
/// inside. Returns `None` when no supported terminal is detected — caller
/// should fall through to in-place creation.
///
/// Detection order: Windows Terminal → iTerm2 → GNOME Terminal → WezTerm →
/// tmux. Native-tab GUI terminals come first; tmux is LAST so that when it runs
/// inside one of those, the GUI tab wins (tmux is only chosen when no tabbed GUI
/// terminal is detected). The order otherwise only matters if multiple env vars
/// are present (rare) — first match wins. To add a terminal, drop a module with
/// a `detect()` and splice one line into this chain.
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
    if let Some(t) = wezterm::detect() {
        return Some(t);
    }
    if let Some(t) = tmux::detect() {
        return Some(t);
    }
    None
}
