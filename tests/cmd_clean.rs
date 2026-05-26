// ===========================================================================
// Integration Tests - Clean Command
// ===========================================================================

mod common;

use std::process::Command;
use tempfile::tempdir;

use common::*;

#[test]
fn test_clean_no_worktrees() {
    let dir = tempdir().unwrap();
    setup_git_repo(dir.path());

    let output = Command::new(ws_binary())
        .env("WS_SPAWNED_IN_TAB", "1")
        .arg("clean")
        .current_dir(dir.path())
        .output()
        .expect("ws clean failed");

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("No") || stderr.is_empty());
}

#[test]
fn test_clean_with_path_file() {
    let dir = tempdir().unwrap();
    setup_git_repo(dir.path());

    let path_file = create_path_file(dir.path());
    let output = Command::new(ws_binary())
        .env("WS_SPAWNED_IN_TAB", "1")
        .args(["clean", "--path-file", path_file.to_str().unwrap()])
        .current_dir(dir.path())
        .output()
        .expect("ws clean failed");

    assert!(output.status.success());
}

#[test]
fn test_clean_after_merge() {
    let (_dir, repo, home) = setup_worktree_test_env();

    let output = Command::new(ws_binary())
        .env("WS_SPAWNED_IN_TAB", "1")
        .args(["new", "clean-test"])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("AGENT_WORKSPACE_DIR", home.join(".agent-workspace"))
        .output()
        .expect("ws new failed");

    assert!(
        output.status.success(),
        "Command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    Command::new("git")
        .args(["merge", "clean-test", "--no-edit"])
        .current_dir(&repo)
        .output()
        .ok();

    let output = Command::new(ws_binary())
        .env("WS_SPAWNED_IN_TAB", "1")
        .arg("clean")
        .current_dir(&repo)
        .env("HOME", &home)
        .env("AGENT_WORKSPACE_DIR", home.join(".agent-workspace"))
        .output()
        .expect("ws clean failed");

    assert!(output.status.success());
}

#[test]
fn test_clean_remvs_merged_worktree() {
    let (_dir, repo, home) = setup_worktree_test_env();

    let output = Command::new(ws_binary())
        .env("WS_SPAWNED_IN_TAB", "1")
        .args(["new", "clean-merged"])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("AGENT_WORKSPACE_DIR", home.join(".agent-workspace"))
        .output()
        .expect("ws new failed");

    assert!(
        output.status.success(),
        "Command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    Command::new("git")
        .args(["merge", "clean-merged", "--no-edit"])
        .current_dir(&repo)
        .output()
        .ok();

    let output = Command::new(ws_binary())
        .env("WS_SPAWNED_IN_TAB", "1")
        .arg("clean")
        .current_dir(&repo)
        .env("HOME", &home)
        .env("AGENT_WORKSPACE_DIR", home.join(".agent-workspace"))
        .output()
        .expect("ws clean failed");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success()
            || stderr.contains("cleaned")
            || stderr.contains("No")
            || stderr.contains("merged")
    );
}

#[test]
fn test_clean_dry_run() {
    let (_dir, repo, home) = setup_worktree_test_env();

    // Create a worktree with no changes (will match trunk)
    let output = Command::new(ws_binary())
        .env("WS_SPAWNED_IN_TAB", "1")
        .args(["new", "clean-dry"])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("AGENT_WORKSPACE_DIR", home.join(".agent-workspace"))
        .output()
        .expect("ws new failed");
    assert!(output.status.success());

    // dry-run should not remove anything
    let output = Command::new(ws_binary())
        .env("WS_SPAWNED_IN_TAB", "1")
        .args(["clean", "--dry-run"])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("AGENT_WORKSPACE_DIR", home.join(".agent-workspace"))
        .output()
        .expect("ws clean --dry-run failed");

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Worktree should still exist after dry-run
    let ls_output = Command::new(ws_binary())
        .env("WS_SPAWNED_IN_TAB", "1")
        .arg("ls")
        .current_dir(&repo)
        .env("HOME", &home)
        .env("AGENT_WORKSPACE_DIR", home.join(".agent-workspace"))
        .output()
        .expect("ws ls failed");
    let stdout = String::from_utf8_lossy(&ls_output.stdout);

    // If worktree had no diff, dry-run would show "Would clean" but worktree survives
    if stderr.contains("Would clean") {
        assert!(stdout.contains("clean-dry"));
    }
}

#[test]
fn test_clean_skips_dirty_worktree() {
    // A worktree with no committed diff but uncommitted edits is NOT eligible
    // for clean — git would refuse non-force removal anyway, and silently
    // discarding in-flight work would be a footgun.
    let (dir, repo, home) = setup_worktree_test_env();

    // Use --path-file so we get the worktree path directly without parsing
    // `ws ls -l` output (whose `~`-prefix shortening on hosts where the
    // tempdir happens to live under $HOME breaks naive Path::is_absolute
    // detection).
    let path_file = create_path_file(dir.path());
    let output = Command::new(ws_binary())
        .env("WS_SPAWNED_IN_TAB", "1")
        .args([
            "new",
            "dirty-clean",
            "--path-file",
            path_file.to_str().unwrap(),
        ])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("AGENT_WORKSPACE_DIR", home.join(".agent-workspace"))
        .output()
        .expect("ws new failed");
    assert!(output.status.success());
    let wt_path = read_path_file(&path_file).trim().to_string();
    std::fs::write(format!("{wt_path}/scratch.tmp"), "in-flight\n").unwrap();

    // Dry-run should report the dirty skip, not "Would clean"
    let output = Command::new(ws_binary())
        .env("WS_SPAWNED_IN_TAB", "1")
        .args(["clean", "--dry-run"])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("AGENT_WORKSPACE_DIR", home.join(".agent-workspace"))
        .output()
        .expect("ws clean --dry-run failed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success());
    assert!(
        stderr.contains("Skipping dirty-clean") || stderr.contains("uncommitted"),
        "stderr should mention skipping dirty worktree: {stderr}"
    );
    assert!(
        !stderr.contains("Would clean (no diff from main): dirty-clean"),
        "dry-run must not promise to clean a dirty worktree: {stderr}"
    );
}
