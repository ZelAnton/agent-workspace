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
//       pwsh -NoExit -File <temp-script.ps1>
//
// **Why `-File` instead of `-Command`?** Windows Terminal's CLI parser
// splits on `;` to chain subcommands (`wt new-tab cmd1 ; new-tab cmd2`)
// — and the split happens AT THE WT ARGV LEVEL, **even when `;` is
// inside a quoted argument** passed via Rust's `Command::args`. Our
// PowerShell script is full of `;` (statement separators). If we pass
// it inline via `-Command "<script>"`, WT silently truncates the script
// at the first `;`, then treats every subsequent chunk as an implicit
// `new-tab` whose first token (e.g. `try`, `if`, `$pf=...`) it tries to
// launch as an executable — producing the user-visible
// `[error 0x80070002 when launching ...]` errors. Writing the script to
// a temp .ps1 file and invoking `pwsh -File <path>` keeps WT's argv
// free of `;` and bullet-proofs the spawn against arbitrary script
// content (regression: 0.12.4 and earlier did inline `-Command`).
//
// The `-NoExit` flag keeps the tab open after the script finishes;
// without it the tab closes the moment the creation script returns,
// dropping the user back to ground. The temp script self-deletes at
// the end of execution (PowerShell loads `-File` into memory and
// releases the handle before user code runs, so removing the file is
// safe).

use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

use super::{script, Error, Result, TabSpec, TerminalIntegration};

pub fn detect() -> Option<Box<dyn TerminalIntegration>> {
    let in_wt = std::env::var("WT_SESSION")
        .ok()
        .filter(|s| !s.is_empty())
        .is_some();
    if !in_wt {
        return None;
    }
    // Soft-probe for `wt.exe` at DETECTION time, not spawn time. A stale
    // `WT_SESSION` can linger in the environment of a shell that's no longer
    // running under Windows Terminal (e.g. a detached process, or one re-exec'd
    // from a parent WT that has since exited). Without this, `detect()` would
    // claim WT support and `open_tab` would fail mid-`ws new` with a confusing
    // "wt.exe not found" error. Returning `None` here lets the caller fall
    // through to ordinary in-place creation instead.
    locate_wt_binary()?;
    Some(Box::new(WindowsTerminal))
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

        // Write the PowerShell script to a temp .ps1 file. See the
        // module-level comment for the full rationale — short version:
        // WT splits its own command line on `;` even inside quoted
        // args, so an inline `-Command "$env:X='1'; ..."` silently
        // truncates at the first `;`. `-File <path>` sidesteps the WT
        // argv parser entirely.
        let tmp_path = write_script_to_temp(&ps_command)?;
        let tmp_path_str = tmp_path.to_string_lossy().to_string();

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
                "-File",
                &tmp_path_str,
            ])
            .status()
            .map_err(|e| Error::Spawn(format!("wt.exe new-tab: {e}")))?;

        if !status.success() {
            // Best-effort temp cleanup: if WT failed to spawn, no shell
            // will run the self-delete tail, so unlink it here. Ignored
            // if it fails (already gone, or AV holds a handle).
            let _ = std::fs::remove_file(&tmp_path);
            return Err(Error::Spawn(format!(
                "wt.exe new-tab exit code {}",
                status.code().unwrap_or(-1)
            )));
        }
        Ok(())
    }
}

/// Persist the PowerShell script to a uniquely-named `.ps1` file and
/// append a self-delete tail so the file is cleaned up when the user
/// closes the spawned tab.
///
/// PowerShell's `-File` loads the script and closes the file handle
/// before user code runs, so `Remove-Item $PSCommandPath` at the bottom
/// is safe. If the script body itself errors out the tail still runs
/// because we wrap in `try { ... } finally { ... }`. If pwsh itself
/// crashes before reaching the finally, the file leaks to `%TEMP%` and
/// the OS reclaims it on the next cleanup pass — acceptable.
fn write_script_to_temp(body: &str) -> Result<PathBuf> {
    // `tempfile::Builder::tempfile()` creates the file with `O_EXCL`
    // semantics on Unix and `CREATE_NEW` on Windows — no race on the
    // unique name. We then `keep()` it so the file outlives this
    // process (the spawned tab needs to read it).
    let tmp = tempfile::Builder::new()
        .prefix("ws-tab-")
        .suffix(".ps1")
        .tempfile()
        .map_err(|e| Error::Spawn(format!("create temp script: {e}")))?;

    let (mut file, path) = tmp
        .keep()
        .map_err(|e| Error::Spawn(format!("persist temp script: {e}")))?;

    // Wrap the body in try/finally for the self-delete. The body
    // already contains its own try/finally (for the path-file dance)
    // — nesting is fine in PowerShell. `-LiteralPath` defeats any
    // wildcard interpretation of `$PSCommandPath`.
    let wrapped = format!(
        "try {{\n{body}\n}} finally {{ \
         Remove-Item -LiteralPath $PSCommandPath -ErrorAction SilentlyContinue \
         }}\n"
    );

    file.write_all(wrapped.as_bytes())
        .map_err(|e| Error::Spawn(format!("write temp script: {e}")))?;
    file.flush()
        .map_err(|e| Error::Spawn(format!("flush temp script: {e}")))?;
    drop(file);

    Ok(path)
}

/// Locate Microsoft Windows Terminal's `wt.exe`.
///
/// On a healthy Microsoft Store install, `wt.exe` is symlinked into
/// `%LOCALAPPDATA%\Microsoft\WindowsApps`, which is usually first in PATH.
/// Since v0.13.0 we renamed our binary to `ws.exe`, so there is no longer
/// any naming collision with Microsoft's `wt.exe` — the elaborate
/// "skip-our-own-binary" PATH-walk from earlier releases is gone.
///
/// Strategy:
///   1. Walk PATH for the first `wt.exe`.
///   2. Fall back to `%LOCALAPPDATA%\Microsoft\WindowsApps\wt.exe` if the
///      PATH lookup misses it (WindowsApps has unusual ACLs and the entry
///      occasionally drops out of PATH resolution).
fn locate_wt_binary() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join("wt.exe");
            if candidate.is_file() {
                return Some(candidate);
            }
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
