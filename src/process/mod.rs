// ===========================================================================
// process - External Process Management (Agents & Hooks)
// ===========================================================================

use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to spawn process: {0}")]
    Spawn(#[from] std::io::Error),

    #[error("hook '{0}' failed")]
    HookFailed(String),
}

/// Run a command in the specified directory, inheriting stdio.
///
/// Hooks are an unsandboxed shell-string contract (see AGENTS.md) — the command
/// is handed to `sh -c` (Unix) / `cmd /C` (Windows) verbatim. On Windows we use
/// `raw_arg` rather than `.args(["/C", command])`: std's default argument
/// escaping is tuned for ordinary executables, not cmd.exe's own parser, so for
/// hook strings containing quotes, `%`, or `^` the default path would mangle
/// the command (backslash-escaped quotes that cmd.exe doesn't understand).
/// `raw_arg` passes the line through unaltered.
pub fn run_interactive(command: &str, cwd: &Path) -> Result<ExitStatus> {
    let mut cmd = build_shell_command(command);
    let status = cmd
        .current_dir(cwd)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;
    Ok(status)
}

#[cfg(windows)]
fn build_shell_command(command: &str) -> Command {
    use std::os::windows::process::CommandExt;
    let mut cmd = Command::new("cmd");
    // Single raw arg containing the whole `/C <command>` tail — no std re-quoting.
    cmd.raw_arg(format!("/C {command}"));
    cmd
}

#[cfg(not(windows))]
fn build_shell_command(command: &str) -> Command {
    let mut cmd = Command::new("sh");
    cmd.args(["-c", command]);
    cmd
}

/// Run a hook command
pub fn run_hook(command: &str, cwd: &Path) -> Result<()> {
    let status = run_interactive(command, cwd)?;

    if !status.success() {
        return Err(Error::HookFailed(command.to_string()));
    }

    Ok(())
}

/// Run multiple hooks in sequence
pub fn run_hooks(hooks: &[String], cwd: &Path) -> Result<()> {
    for hook in hooks {
        eprintln!("Running hook: {hook}...");
        run_hook(hook, cwd)?;
        eprintln!("Hook done: {hook}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // =========================================================================
    // Error tests
    // =========================================================================
    #[test]
    fn test_error_display() {
        let err = Error::HookFailed("npm install".to_string());
        assert_eq!(err.to_string(), "hook 'npm install' failed");
    }

    // =========================================================================
    // run_interactive tests
    // =========================================================================
    // Test commands are chosen to run identically under `cmd /C` (Windows) and
    // `sh -c` (Unix): `exit 0` / `exit 1` are builtins in both shells, and
    // `echo ... > file` creates a file on both. This keeps the suite green on a
    // clean Windows CI image that lacks Git's `true`/`touch`/`test` coreutils.

    #[test]
    fn test_run_interactive_success() {
        let dir = tempdir().unwrap();
        let result = run_interactive("exit 0", dir.path());
        assert!(result.is_ok());
        assert!(result.unwrap().success());
    }

    #[test]
    fn test_run_interactive_failure() {
        let dir = tempdir().unwrap();
        let result = run_interactive("exit 1", dir.path());
        assert!(result.is_ok());
        assert!(!result.unwrap().success());
    }

    #[test]
    fn test_run_interactive_echo() {
        let dir = tempdir().unwrap();
        let result = run_interactive("echo hello", dir.path());
        assert!(result.is_ok());
        assert!(result.unwrap().success());
    }

    #[test]
    fn test_run_interactive_with_cwd() {
        let dir = tempdir().unwrap();
        // Verify cwd is respected: the hook writes a marker into its cwd, and we
        // assert it lands in `dir`. Portable across cmd.exe and sh.
        let result = run_interactive("echo x > marker.txt", dir.path());
        assert!(result.is_ok());
        assert!(result.unwrap().success());
        assert!(dir.path().join("marker.txt").exists());
    }

    #[test]
    fn test_run_interactive_nonexistent_cwd() {
        let nonexistent = std::path::Path::new("/nonexistent/path/12345");
        let result = run_interactive("exit 0", nonexistent);
        assert!(result.is_err());
    }

    // =========================================================================
    // run_hook tests
    // =========================================================================
    #[test]
    fn test_run_hook_success() {
        let dir = tempdir().unwrap();
        let result = run_hook("exit 0", dir.path());
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_hook_failure() {
        let dir = tempdir().unwrap();
        let result = run_hook("exit 1", dir.path());
        assert!(result.is_err());
        match result.unwrap_err() {
            Error::HookFailed(cmd) => assert_eq!(cmd, "exit 1"),
            _ => panic!("Expected HookFailed error"),
        }
    }

    #[test]
    fn test_run_hook_creates_file() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("hook_created.txt");

        let cmd = format!("echo test > {}", file_path.display());
        let result = run_hook(&cmd, dir.path());
        assert!(result.is_ok());
        assert!(file_path.exists());
    }

    // =========================================================================
    // run_hooks tests
    // =========================================================================
    #[test]
    fn test_run_hooks_empty() {
        let dir = tempdir().unwrap();
        let hooks: Vec<String> = vec![];
        let result = run_hooks(&hooks, dir.path());
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_hooks_single() {
        let dir = tempdir().unwrap();
        let hooks = vec!["exit 0".to_string()];
        let result = run_hooks(&hooks, dir.path());
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_hooks_multiple() {
        let dir = tempdir().unwrap();
        let hooks = vec![
            "exit 0".to_string(),
            "echo hello".to_string(),
            "exit 0".to_string(),
        ];
        let result = run_hooks(&hooks, dir.path());
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_hooks_stops_on_failure() {
        let dir = tempdir().unwrap();
        let file1 = dir.path().join("file1.txt");
        let file2 = dir.path().join("file2.txt");

        // `echo x > file` creates the file portably (cmd.exe + sh); we only
        // assert existence, not content.
        let hooks = vec![
            format!("echo x > {}", file1.display()),
            "exit 1".to_string(), // This will fail
            format!("echo x > {}", file2.display()),
        ];

        let result = run_hooks(&hooks, dir.path());
        assert!(result.is_err());
        assert!(file1.exists()); // First hook ran
        assert!(!file2.exists()); // Third hook didn't run
    }

    #[test]
    fn test_run_hooks_sequential_order() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("order.txt");

        let hooks = vec![
            format!("echo one >> {}", file.display()),
            format!("echo two >> {}", file.display()),
            format!("echo three >> {}", file.display()),
        ];

        let result = run_hooks(&hooks, dir.path());
        assert!(result.is_ok());

        // Lines are trimmed because cmd.exe's `echo` preserves whitespace
        // between the argument and the redirect operator — `echo one >> f`
        // writes "one \n" on Windows, "one\n" on Unix.
        let content = std::fs::read_to_string(&file).unwrap();
        let lines: Vec<&str> = content.lines().map(str::trim).collect();
        assert_eq!(lines, vec!["one", "two", "three"]);
    }
}
