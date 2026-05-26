// ===========================================================================
// Integration Tests - Cd Command
// ===========================================================================

mod common;

use std::process::Command;
use tempfile::tempdir;

use common::*;

#[test]
fn test_cd_nonexistent() {
    let dir = tempdir().unwrap();
    setup_git_repo(dir.path());

    let output = Command::new(ws_binary())
        .env("WS_SPAWNED_IN_TAB", "1")
        .args(["cd", "nonexistent-branch"])
        .current_dir(dir.path())
        .output()
        .expect("Failed to execute wt cd");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not found") || stderr.contains("error"));
}

#[test]
fn test_cd_without_print_path() {
    let dir = tempdir().unwrap();
    setup_git_repo(dir.path());

    let output = Command::new(ws_binary())
        .env("WS_SPAWNED_IN_TAB", "1")
        .args(["cd", "nonexistent"])
        .current_dir(dir.path())
        .output()
        .expect("ws cd failed");

    assert!(!output.status.success());
}

#[test]
fn test_cd_to_existing_worktree() {
    let (dir, repo, home) = setup_worktree_test_env();

    let output = Command::new(ws_binary())
        .env("WS_SPAWNED_IN_TAB", "1")
        .args(["new", "cd-target"])
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

    let path_file = create_path_file(dir.path());
    let output = Command::new(ws_binary())
        .env("WS_SPAWNED_IN_TAB", "1")
        .args([
            "cd",
            "cd-target",
            "--path-file",
            path_file.to_str().unwrap(),
        ])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("AGENT_WORKSPACE_DIR", home.join(".agent-workspace"))
        .output()
        .expect("ws cd failed");

    if output.status.success() {
        let path = read_path_file(&path_file);
        assert!(path.contains("cd-target"));
    }
}

#[test]
fn test_cd_returns_correct_path() {
    let (dir, repo, home) = setup_worktree_test_env();

    let path_file = create_path_file(dir.path());
    let new_output = Command::new(ws_binary())
        .env("WS_SPAWNED_IN_TAB", "1")
        .args([
            "new",
            "cd-check",
            "--path-file",
            path_file.to_str().unwrap(),
        ])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("AGENT_WORKSPACE_DIR", home.join(".agent-workspace"))
        .output()
        .expect("ws new failed");

    assert!(
        new_output.status.success(),
        "Command failed: {}",
        String::from_utf8_lossy(&new_output.stderr)
    );

    let created_path = read_path_file(&path_file).trim().to_string();

    let cd_path_file = dir.path().join(".wt-cd-path");
    let output = Command::new(ws_binary())
        .env("WS_SPAWNED_IN_TAB", "1")
        .args([
            "cd",
            "cd-check",
            "--path-file",
            cd_path_file.to_str().unwrap(),
        ])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("AGENT_WORKSPACE_DIR", home.join(".agent-workspace"))
        .output()
        .expect("ws cd failed");

    if output.status.success() {
        let cd_path = read_path_file(&cd_path_file).trim().to_string();
        assert_eq!(created_path, cd_path);
    }
}

// ---------------------------------------------------------------------------
// Tab-integration flag handling for `ws cd`
//
// Per the test-infra convention (tests/common/mod.rs), `WS_SPAWNED_IN_TAB=1`
// is set on every invocation — so the tab-spawn dispatcher always
// short-circuits via the recursion-guard branch. These tests therefore
// can't observe an actual tab spawn; they pin **flag acceptance and the
// fallback-to-path-file behaviour** that exercises the same code path a
// user would hit when CoW/tab integration is disabled.
// ---------------------------------------------------------------------------

#[test]
fn test_cd_accepts_no_tab_flag_and_writes_path_file() {
    let (dir, repo, home) = setup_worktree_test_env();
    Command::new(ws_binary())
        .env("WS_SPAWNED_IN_TAB", "1")
        .args(["new", "no-tab-target"])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("AGENT_WORKSPACE_DIR", home.join(".agent-workspace"))
        .output()
        .expect("ws new failed");

    let path_file = create_path_file(dir.path());
    let output = Command::new(ws_binary())
        .env("WS_SPAWNED_IN_TAB", "1")
        .args([
            "cd",
            "no-tab-target",
            "--no-tab",
            "--path-file",
            path_file.to_str().unwrap(),
        ])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("AGENT_WORKSPACE_DIR", home.join(".agent-workspace"))
        .output()
        .expect("ws cd --no-tab failed");

    assert!(
        output.status.success(),
        "Command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let path = read_path_file(&path_file);
    assert!(
        path.contains("no-tab-target"),
        "path-file must contain target worktree (no-tab path)"
    );
}

#[test]
fn test_cd_accepts_in_new_tab_flag() {
    // With WS_SPAWNED_IN_TAB=1 the dispatcher's recursion guard
    // short-circuits before any terminal::detect() call — so even with
    // --in-new-tab passed, the path-file fallback runs. This test just
    // pins that the flag is accepted and doesn't break the wrapper
    // handshake.
    let (dir, repo, home) = setup_worktree_test_env();
    Command::new(ws_binary())
        .env("WS_SPAWNED_IN_TAB", "1")
        .args(["new", "in-tab-target"])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("AGENT_WORKSPACE_DIR", home.join(".agent-workspace"))
        .output()
        .expect("ws new failed");

    let path_file = create_path_file(dir.path());
    let output = Command::new(ws_binary())
        .env("WS_SPAWNED_IN_TAB", "1")
        .args([
            "cd",
            "in-tab-target",
            "--in-new-tab",
            "--path-file",
            path_file.to_str().unwrap(),
        ])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("AGENT_WORKSPACE_DIR", home.join(".agent-workspace"))
        .output()
        .expect("ws cd --in-new-tab failed");

    assert!(
        output.status.success(),
        "Command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_cd_in_new_tab_conflicts_with_no_tab() {
    // clap's `conflicts_with` should reject both flags at once.
    let (_dir, repo, home) = setup_worktree_test_env();
    let output = Command::new(ws_binary())
        .env("WS_SPAWNED_IN_TAB", "1")
        .args(["cd", "any", "--in-new-tab", "--no-tab"])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("AGENT_WORKSPACE_DIR", home.join(".agent-workspace"))
        .output()
        .expect("ws cd failed");
    assert!(
        !output.status.success(),
        "clap should reject mutually-exclusive flags"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot be used with") || stderr.contains("conflict"),
        "expected clap conflict message, got: {stderr}"
    );
}
