// ===========================================================================
// tests/common - Shared Test Helpers
// ===========================================================================

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

pub fn ws_binary() -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("target/debug/ws");
    path
}

/// Build a `Command` for the `ws` binary with test-isolation env vars applied.
///
/// Setting `HOME` is enough on Unix, where `directories::BaseDirs` reads it
/// directly. On Windows that crate reads the user profile via the
/// `SHGetKnownFolderPath` WinAPI — env vars don't influence it — so we also
/// set `AGENT_WORKSPACE_DIR`, which `Config::base_dir()` (in
/// `src/config/mod.rs`) checks before falling back to `BaseDirs`. This makes
/// tests isolate cross-platform.
pub fn ws_command(home: &Path) -> Command {
    let mut cmd = Command::new(ws_binary());
    cmd.env("HOME", home);
    cmd.env("AGENT_WORKSPACE_DIR", home.join(".agent-workspace"));
    // Hard-disable the new-tab feature in integration tests. Without
    // this, running `cargo test` from inside Windows Terminal (or
    // iTerm2 / GNOME Terminal) inherits `WT_SESSION` etc. into the
    // child process, `terminal::detect()` returns Some, and `ws new`
    // attempts to spawn a tab — which hangs or pollutes the developer's
    // window. Setting the recursion guard short-circuits the spawn
    // dispatch regardless of which terminal env vars leaked in.
    cmd.env("WS_SPAWNED_IN_TAB", "1");
    cmd
}

pub fn create_path_file(dir: &Path) -> PathBuf {
    dir.join(".ws-path")
}

pub fn read_path_file(path_file: &Path) -> String {
    std::fs::read_to_string(path_file).unwrap_or_default()
}

pub fn setup_git_repo(dir: &Path) {
    Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .expect("git init failed");

    Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(dir)
        .output()
        .expect("git config email failed");

    Command::new("git")
        .args(["config", "user.name", "Test User"])
        .current_dir(dir)
        .output()
        .expect("git config name failed");

    std::fs::write(dir.join("README.md"), "# Test Repo\n").unwrap();

    Command::new("git")
        .args(["add", "."])
        .current_dir(dir)
        .output()
        .expect("git add failed");

    Command::new("git")
        .args(["commit", "-m", "Initial commit"])
        .current_dir(dir)
        .output()
        .expect("git commit failed");

    Command::new("git")
        .args(["branch", "-M", "main"])
        .current_dir(dir)
        .output()
        .ok();
}

pub fn setup_git_repo_with_home(dir: &Path) -> PathBuf {
    setup_git_repo(dir);

    let home = dir.join("fake_home");
    std::fs::create_dir_all(&home).unwrap();

    let wt_dir = home.join(".agent-workspace");
    std::fs::create_dir_all(&wt_dir).unwrap();

    let config = r#"
[worktree]
default_base = "main"
"#;
    std::fs::write(wt_dir.join("config.toml"), config).unwrap();

    home
}

pub fn setup_worktree_test_env() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();

    setup_git_repo(&repo);

    let home = dir.path().join("home");
    let wt_dir = home.join(".agent-workspace");
    std::fs::create_dir_all(&wt_dir).unwrap();

    let config = r#"
[worktree]
default_base = "main"
"#;
    std::fs::write(wt_dir.join("config.toml"), config).unwrap();

    (dir, repo, home)
}
