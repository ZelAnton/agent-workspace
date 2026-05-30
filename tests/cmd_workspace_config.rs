// ===========================================================================
// Integration Tests - .workspace.toml config + local (no-commit) exclude
// ===========================================================================
//
// Covers the end-to-end behaviour of the `.workspace.toml` repo/worktree
// config: that `ws config` / `ws exclude` write it and auto-exclude it via the
// repo's LOCAL git exclude file (`.git/info/exclude`) — never `.gitignore`, so
// nothing needs committing and the working tree stays clean; that the legacy
// `.agent-workspace.toml` is still read and edited in place; that `Config::load`
// actually reads `.workspace.toml`; and that worktree-level config overrides
// repo-level. The 3-tier merge precedence is unit-tested in `config::tests`.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::*;
use tempfile::tempdir;

/// Count exact-match occurrences of `pattern` as a whole line in the repo's
/// local exclude file (`.git/info/exclude`).
fn exclude_lines(repo: &Path, pattern: &str) -> usize {
    std::fs::read_to_string(repo.join(".git").join("info").join("exclude"))
        .map(|s| s.lines().filter(|l| l.trim() == pattern).count())
        .unwrap_or(0)
}

/// `git status --porcelain` output for `repo` (empty = clean working tree).
fn git_status_porcelain(repo: &Path) -> String {
    let out = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(repo)
        .output()
        .expect("git status failed");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn config_set_writes_workspace_toml_and_excludes_locally() {
    let dir = tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    setup_git_repo(&repo);
    let home = dir.path().join("home");

    let out = ws_command(&home)
        .args(["config", "set", "workspace.alias", "myalias"])
        .current_dir(&repo)
        .output()
        .expect("ws config set failed");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // `.workspace.toml` written; legacy file NOT created.
    assert!(repo.join(".workspace.toml").exists());
    assert!(!repo.join(".agent-workspace.toml").exists());
    let ws = std::fs::read_to_string(repo.join(".workspace.toml")).unwrap();
    assert!(ws.contains("alias = \"myalias\""), "got:\n{ws}");

    // The rule went into the LOCAL exclude file, exactly once — NOT `.gitignore`.
    assert_eq!(exclude_lines(&repo, ".workspace.toml"), 1);
    assert!(
        !repo.join(".gitignore").exists(),
        "must not create a committed .gitignore"
    );

    // The whole point: nothing to commit. `.workspace.toml` is excluded and the
    // exclude file lives inside `.git`, so the working tree is clean.
    assert_eq!(
        git_status_porcelain(&repo),
        "",
        "working tree must stay clean (nothing to commit)"
    );

    // Idempotent: a second set does not duplicate the exclude line.
    let out2 = ws_command(&home)
        .args(["config", "set", "workspace.alias", "other"])
        .current_dir(&repo)
        .output()
        .expect("second ws config set failed");
    assert!(out2.status.success());
    assert_eq!(exclude_lines(&repo, ".workspace.toml"), 1);
}

#[test]
fn legacy_agent_workspace_toml_read_and_edited_in_place() {
    let dir = tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    setup_git_repo(&repo);
    let home = dir.path().join("home");

    // Pre-seed the legacy committed file only.
    std::fs::write(
        repo.join(".agent-workspace.toml"),
        "[workspace]\nalias = \"legacy\"\n",
    )
    .unwrap();

    // `get` reads it via the fallback path.
    let got = ws_command(&home)
        .args(["config", "get", "workspace.alias"])
        .current_dir(&repo)
        .output()
        .expect("ws config get failed");
    assert!(got.status.success());
    assert!(
        String::from_utf8_lossy(&got.stdout).contains("legacy"),
        "stdout: {}",
        String::from_utf8_lossy(&got.stdout)
    );

    // `set` edits the legacy file in place — no new `.workspace.toml`, and
    // no exclude entry (editing a committed file introduces no local file).
    let set = ws_command(&home)
        .args(["config", "set", "workspace.alias", "updated"])
        .current_dir(&repo)
        .output()
        .expect("ws config set failed");
    assert!(set.status.success());
    assert!(!repo.join(".workspace.toml").exists());
    let legacy = std::fs::read_to_string(repo.join(".agent-workspace.toml")).unwrap();
    assert!(legacy.contains("updated"), "got:\n{legacy}");
    assert_eq!(exclude_lines(&repo, ".workspace.toml"), 0);
}

#[test]
fn exclude_add_writes_workspace_toml_and_excludes_locally() {
    let dir = tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    setup_git_repo(&repo);
    let home = dir.path().join("home");

    let out = ws_command(&home)
        .args(["exclude", "target"])
        .current_dir(&repo)
        .output()
        .expect("ws exclude failed");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let ws = std::fs::read_to_string(repo.join(".workspace.toml")).unwrap();
    assert!(ws.contains("target"), "got:\n{ws}");
    assert_eq!(exclude_lines(&repo, ".workspace.toml"), 1);
    assert!(!repo.join(".gitignore").exists());
}

#[test]
fn workspace_toml_is_loaded_by_config_load() {
    // Proves the loader reads `.workspace.toml` end-to-end: the alias it
    // declares determines the workspace directory name `ws new` picks.
    let dir = tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    setup_git_repo(&repo);
    let home = dir.path().join("home");

    std::fs::write(
        repo.join(".workspace.toml"),
        "[workspace]\nalias = \"myalias\"\n",
    )
    .unwrap();

    let pf = create_path_file(dir.path());
    let out = ws_command(&home)
        .args([
            "new",
            "feat",
            "--no-cow",
            "--path-file",
            pf.to_str().unwrap(),
        ])
        .current_dir(&repo)
        .output()
        .expect("ws new failed");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let wt_path = read_path_file(&pf);
    assert!(
        wt_path.contains("myalias"),
        "worktree path should use alias from .workspace.toml: {wt_path}"
    );
}

#[test]
fn new_does_not_modify_ignore_state() {
    // `ws new` is hands-off w.r.t. ignore state: it neither creates a
    // `.gitignore` nor adds a `.workspace.toml` local-exclude entry. The
    // exclude rule is added only when `ws config` / `ws exclude` write the
    // file, and (living in the shared common git dir) it then covers every
    // worktree automatically.
    let dir = tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    setup_git_repo(&repo);
    let home = dir.path().join("home");

    let pf = create_path_file(dir.path());
    let out = ws_command(&home)
        .args([
            "new",
            "feat",
            "--no-cow",
            "--path-file",
            pf.to_str().unwrap(),
        ])
        .current_dir(&repo)
        .output()
        .expect("ws new failed");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // `ws new` created no `.gitignore` and added no exclude entry.
    assert!(
        !repo.join(".gitignore").exists(),
        "ws new must not create .gitignore in the main repo"
    );
    assert_eq!(
        exclude_lines(&repo, ".workspace.toml"),
        0,
        "ws new must not add a local-exclude entry"
    );
    let wt = PathBuf::from(read_path_file(&pf).trim());
    assert!(
        !wt.join(".gitignore").exists(),
        "ws new must not create .gitignore in the new worktree"
    );
}

#[test]
fn worktree_level_config_overrides_repo_level() {
    // End-to-end proof that the worktree-level `.workspace.toml` overrides the
    // repo-level one for commands run from inside that worktree. Observable via
    // `[workspace] alias`, which decides the workspace-dir name `ws new` picks:
    // running `ws new` from inside a worktree whose `.workspace.toml` sets a
    // distinct alias must place the next worktree under that alias.
    let dir = tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    setup_git_repo(&repo);
    let home = dir.path().join("home");

    // First worktree (repo has no config → default alias = repo basename).
    let pf1 = create_path_file(dir.path());
    let out1 = ws_command(&home)
        .args(["new", "wt1", "--no-cow", "--path-file", pf1.to_str().unwrap()])
        .current_dir(&repo)
        .output()
        .expect("ws new wt1 failed");
    assert!(
        out1.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out1.stderr)
    );
    let wt1 = PathBuf::from(read_path_file(&pf1).trim());

    // Drop a worktree-level config overriding the alias.
    std::fs::write(
        wt1.join(".workspace.toml"),
        "[workspace]\nalias = \"wtoverride\"\n",
    )
    .unwrap();

    // From inside wt1, create wt2. The merged config (repo default overlaid by
    // wt1's `.workspace.toml`) must use alias="wtoverride" for the workspace dir.
    let pf2 = create_path_file(dir.path());
    let out2 = ws_command(&home)
        .args(["new", "wt2", "--no-cow", "--path-file", pf2.to_str().unwrap()])
        .current_dir(&wt1)
        .output()
        .expect("ws new wt2 failed");
    assert!(
        out2.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out2.stderr)
    );

    let wt2 = read_path_file(&pf2);
    assert!(
        wt2.contains("wtoverride"),
        "wt2 path should reflect the worktree-level alias override: {wt2}"
    );
}
