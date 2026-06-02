// ===========================================================================
// Integration Tests - JSON output contract (--format json)
//
// Every read/result command must emit a single JSON object on stdout carrying
// a top-level `schema_version` (the stable contract injected centrally in
// `cli::output::emit_json`). These tests pin that contract end-to-end by
// running the real binary and parsing its stdout.
// ===========================================================================

mod common;

use common::*;

/// Parse a command's stdout as JSON and assert the versioned envelope.
fn assert_versioned(stdout: &[u8]) -> serde_json::Value {
    let v: serde_json::Value =
        serde_json::from_slice(stdout).expect("stdout must be a single JSON object");
    assert_eq!(
        v["schema_version"], 1,
        "every JSON payload carries schema_version=1; got: {v}"
    );
    v
}

#[test]
fn ls_json_has_schema_version_and_worktrees() {
    let dir = tempfile::tempdir().unwrap();
    setup_git_repo(dir.path());

    let out = ws_command(dir.path())
        .args(["ls", "--format", "json"])
        .current_dir(dir.path())
        .output()
        .expect("run ws ls --format json");
    assert!(out.status.success(), "ws ls should succeed");

    let v = assert_versioned(&out.stdout);
    assert!(v["worktrees"].is_array(), "ls payload has a worktrees array");
}

#[test]
fn clean_dry_run_json_has_result_arrays() {
    let dir = tempfile::tempdir().unwrap();
    setup_git_repo(dir.path());

    let out = ws_command(dir.path())
        .args(["clean", "--dry-run", "--format", "json"])
        .current_dir(dir.path())
        .output()
        .expect("run ws clean --dry-run --format json");
    assert!(out.status.success(), "ws clean --dry-run should succeed");

    let v = assert_versioned(&out.stdout);
    assert_eq!(v["dry_run"], true);
    assert!(v["cleaned"].is_array());
    assert!(v["skipped_dirty"].is_array());
}

#[test]
fn config_list_json_has_keys() {
    let dir = tempfile::tempdir().unwrap();
    setup_git_repo(dir.path());

    let out = ws_command(dir.path())
        .args(["config", "list", "--format", "json"])
        .current_dir(dir.path())
        .output()
        .expect("run ws config list --format json");
    assert!(out.status.success(), "ws config list should succeed");

    let v = assert_versioned(&out.stdout);
    let keys = v["keys"].as_array().expect("config payload has a keys array");
    assert!(
        keys.iter().any(|k| k["key"] == "workspace.alias"),
        "known keys include workspace.alias"
    );
}

#[test]
fn exclude_list_json_has_patterns() {
    let dir = tempfile::tempdir().unwrap();
    setup_git_repo(dir.path());

    let out = ws_command(dir.path())
        .args(["exclude", "--list", "--format", "json"])
        .current_dir(dir.path())
        .output()
        .expect("run ws exclude --list --format json");
    assert!(out.status.success(), "ws exclude --list should succeed");

    let v = assert_versioned(&out.stdout);
    assert!(v["patterns"].is_array(), "exclude payload has a patterns array");
}

// --- Mutating commands must ALSO keep stdout pure JSON (regression: they used
//     to `println!` confirmations straight to stdout, corrupting the parse). ---

#[test]
fn config_set_json_is_pure_object() {
    let dir = tempfile::tempdir().unwrap();
    setup_git_repo(dir.path());

    let out = ws_command(dir.path())
        .args(["config", "set", "workspace.alias", "my-alias", "--format", "json"])
        .current_dir(dir.path())
        .output()
        .expect("run ws config set --format json");
    assert!(out.status.success(), "ws config set should succeed");

    // The whole point: stdout parses cleanly (no "Set ... = ..." preamble).
    let v = assert_versioned(&out.stdout);
    assert_eq!(v["action"], "set");
    assert_eq!(v["key"], "workspace.alias");
    assert_eq!(v["value"], "my-alias");
}

#[test]
fn exclude_add_json_is_pure_object() {
    let dir = tempfile::tempdir().unwrap();
    setup_git_repo(dir.path());

    let out = ws_command(dir.path())
        .args(["exclude", "target", "--format", "json"])
        .current_dir(dir.path())
        .output()
        .expect("run ws exclude target --format json");
    assert!(out.status.success(), "ws exclude (add) should succeed");

    let v = assert_versioned(&out.stdout);
    assert_eq!(v["action"], "added");
    let patterns = v["patterns"].as_array().expect("patterns array");
    assert!(patterns.iter().any(|p| p == "target"));
}
