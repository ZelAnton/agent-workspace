mod mock_runner;
mod ops;

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use tempfile::tempdir;

use super::*;
use crate::vcs::backend::VcsBackend;
use crate::vcs::error::Error;

// ---------------------------------------------------------------------------
// Helper: Setup a minimal git repo for testing
// ---------------------------------------------------------------------------
pub(super) fn setup_test_repo() -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    let path = dir.path();

    StdCommand::new("git")
        .args(["init"])
        .current_dir(path)
        .output()
        .expect("git init failed");

    StdCommand::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(path)
        .output()
        .unwrap();

    StdCommand::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(path)
        .output()
        .unwrap();

    std::fs::write(path.join("README.md"), "# Test\n").unwrap();

    StdCommand::new("git")
        .args(["add", "."])
        .current_dir(path)
        .output()
        .unwrap();

    StdCommand::new("git")
        .args(["commit", "-m", "Initial commit"])
        .current_dir(path)
        .output()
        .unwrap();

    StdCommand::new("git")
        .args(["branch", "-M", "main"])
        .current_dir(path)
        .output()
        .ok();

    dir
}

/// Construct a `GitBackend` anchored at `path` for tests. Uses `DefaultRunner`
/// (via `GitBackend::at`) so the test exercises real git against an explicit
/// directory — no process-cwd mutation, so the suite is parallel-safe.
fn backend_at(path: &Path) -> GitBackend {
    GitBackend::at(path.to_path_buf())
}

// ---------------------------------------------------------------------------
// Pure parser tests — no cwd dependency
// ---------------------------------------------------------------------------
#[test]
fn test_parse_worktree_list_empty() {
    let result = parse_worktree_list("");
    assert!(result.is_empty());
}

#[test]
fn test_parse_worktree_list_single() {
    let content = r#"worktree /path/to/repo
HEAD abc1234567890
branch refs/heads/main
"#;
    let result = parse_worktree_list(content);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].path, PathBuf::from("/path/to/repo"));
    assert_eq!(result[0].branch, Some("main".to_string()));
    assert_eq!(result[0].commit, Some("abc1234567890".to_string()));
    assert!(!result[0].is_bare);
}

#[test]
fn test_parse_worktree_list_multiple() {
    let content = r#"worktree /path/to/main
HEAD abc123
branch refs/heads/main

worktree /path/to/feature
HEAD def456
branch refs/heads/feature-branch

worktree /path/to/detached
HEAD 789abc
detached
"#;
    let result = parse_worktree_list(content);
    assert_eq!(result.len(), 3);

    assert_eq!(result[0].branch, Some("main".to_string()));
    assert_eq!(result[1].branch, Some("feature-branch".to_string()));
    assert_eq!(result[2].branch, None); // detached HEAD
}

#[test]
fn test_parse_worktree_list_bare() {
    let content = r#"worktree /path/to/bare.git
bare
"#;
    let result = parse_worktree_list(content);
    assert_eq!(result.len(), 1);
    assert!(result[0].is_bare);
    assert!(result[0].branch.is_none());
}

// ---------------------------------------------------------------------------
// Error display tests
// ---------------------------------------------------------------------------
#[test]
fn test_error_display() {
    let err = Error::NotInRepo;
    // Message changed from "not in a git repository" — the new wording
    // covers future jj-only repositories without lying about the backend.
    assert_eq!(err.to_string(), "not in a version-controlled repository");

    let err = Error::WorktreeNotFound("feature".to_string());
    assert_eq!(err.to_string(), "worktree 'feature' not found");

    let err = Error::WorktreeExists("feature".to_string());
    assert_eq!(err.to_string(), "worktree 'feature' already exists");

    let err = Error::BranchNotFound("missing".to_string());
    assert_eq!(err.to_string(), "branch 'missing' not found");

    let err = Error::Command("something failed".to_string());
    assert_eq!(err.to_string(), "something failed");

    let err = Error::Unsupported("jj: merge".to_string());
    assert_eq!(
        err.to_string(),
        "operation not yet supported by this backend: jj: merge"
    );
}

// ---------------------------------------------------------------------------
// clean_git_error tests
// ---------------------------------------------------------------------------
#[test]
fn test_clean_git_error_fatal_prefix() {
    let msg = clean_git_error("fatal: invalid reference: xxx");
    assert_eq!(msg, "invalid reference: xxx");
}

#[test]
fn test_clean_git_error_error_prefix() {
    let msg = clean_git_error("error: some git error");
    assert_eq!(msg, "some git error");
}

#[test]
fn test_clean_git_error_worktree_uncommitted() {
    let msg = clean_git_error(
        "fatal: '/Users/foo/.agent-workspace/workspaces/proj/branch' contains modified or untracked files, use --force to delete it",
    );
    assert_eq!(msg, "worktree 'branch' has uncommitted changes, use --force");
}

#[test]
fn test_clean_git_error_no_prefix() {
    let msg = clean_git_error("some plain message");
    assert_eq!(msg, "some plain message");
}

// ---------------------------------------------------------------------------
// extract_message tests (signature changed: now takes split stderr/stdout
// to match vcs_runner::RunError shape)
// ---------------------------------------------------------------------------
#[test]
fn test_extract_message_prefers_stderr() {
    let msg = super::errmap::extract_message("fatal: something broke", b"stdout info");
    assert_eq!(msg, "something broke");
}

#[test]
fn test_extract_message_falls_back_to_stdout() {
    // Load-bearing case — `git merge` puts CONFLICT messages on stdout.
    let msg = super::errmap::extract_message("", b"CONFLICT (content): Merge conflict in file.txt\n");
    assert!(msg.contains("CONFLICT"));
}

#[test]
fn test_extract_message_whitespace_only_stderr() {
    let msg = super::errmap::extract_message("  \n  ", b"nothing to commit, working tree clean");
    assert!(msg.contains("nothing to commit"));
}

// ---------------------------------------------------------------------------
// is_cwd_inside tests (pure filesystem helper)
// ---------------------------------------------------------------------------
#[test]
fn test_is_cwd_inside_current_dir() {
    let cwd = std::env::current_dir().unwrap();
    assert!(is_cwd_inside(&cwd));
}

#[test]
fn test_is_cwd_inside_nonexistent() {
    assert!(!is_cwd_inside(Path::new("/nonexistent/path/12345")));
}

// ---------------------------------------------------------------------------
// GitBackend method tests (require changing cwd, use mutex)
// ---------------------------------------------------------------------------

#[test]
fn test_repo_root() {
    let dir = setup_test_repo();
    let root = backend_at(dir.path()).repo_root();
    assert!(root.is_ok());
    let root_path = root.unwrap();
    assert!(root_path.exists());
    assert!(root_path.join(".git").exists());
}

#[test]
fn test_repo_root_not_in_repo() {
    let dir = tempdir().unwrap();
    let root = backend_at(dir.path()).repo_root();
    assert!(root.is_err());
    assert!(matches!(root.unwrap_err(), Error::NotInRepo));
}

#[test]
fn test_repo_name() {
    let dir = setup_test_repo();
    let name = backend_at(dir.path()).repo_name();
    assert!(name.is_ok());
    assert!(!name.unwrap().is_empty());
}

#[test]
fn test_workspace_id_format() {
    let dir = setup_test_repo();
    let id = backend_at(dir.path()).workspace_id().unwrap();
    let parts: Vec<&str> = id.rsplitn(2, '-').collect();
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0].len(), 6);
    assert!(parts[0].chars().all(|c: char| c.is_ascii_hexdigit()));
}

#[test]
fn test_workspace_id_deterministic() {
    let dir = setup_test_repo();
    let b = backend_at(dir.path());
    let id1 = b.workspace_id().unwrap();
    let id2 = b.workspace_id().unwrap();
    assert_eq!(id1, id2);
}

#[test]
fn test_workspace_id_unique_for_different_paths() {
    let dir1 = setup_test_repo();
    let dir2 = setup_test_repo();

    let id1 = backend_at(dir1.path()).workspace_id().unwrap();
    let id2 = backend_at(dir2.path()).workspace_id().unwrap();

    assert_ne!(id1, id2);
}

#[test]
fn test_current_branch() {
    let dir = setup_test_repo();
    let branch = backend_at(dir.path()).current_branch();
    assert!(branch.is_ok());
    assert_eq!(branch.unwrap(), "main");
}

#[test]
fn test_detect_trunk() {
    let dir = setup_test_repo();
    let trunk = backend_at(dir.path()).detect_trunk();
    assert!(trunk.is_ok());
    assert_eq!(trunk.unwrap(), "main");
}

#[test]
fn test_branch_exists_true() {
    let dir = setup_test_repo();
    let exists = backend_at(dir.path()).branch_exists("main");
    assert!(exists.is_ok());
    assert!(exists.unwrap());
}

#[test]
fn test_branch_exists_false() {
    let dir = setup_test_repo();
    let exists = backend_at(dir.path()).branch_exists("nonexistent-branch-12345");
    assert!(exists.is_ok());
    assert!(!exists.unwrap());
}

// ---------------------------------------------------------------------------
// Remote-branch awareness (remote_branch_exists + create_worktree_from_remote)
// ---------------------------------------------------------------------------

/// A work repo wired to a bare `origin` that carries a `remote-only`
/// branch absent from the work repo (local branch + remote-tracking ref
/// both removed). Returns (work_dir, remote_dir) — keep both alive for the
/// test's duration so the tempdirs aren't dropped.
fn setup_repo_with_remote_only_branch() -> (tempfile::TempDir, tempfile::TempDir) {
    let work = setup_test_repo();
    let remote = tempdir().unwrap();
    StdCommand::new("git")
        .args(["init", "--bare"])
        .current_dir(remote.path())
        .output()
        .unwrap();
    let url = remote.path().to_str().unwrap();
    for args in [
        vec!["remote", "add", "origin", url],
        vec!["push", "origin", "main"],
        vec!["branch", "remote-only", "main"],
        vec!["push", "origin", "remote-only"],
        // Make it truly remote-only: drop the local branch AND any
        // remote-tracking ref `push` may have created.
        vec!["branch", "-D", "remote-only"],
        vec!["update-ref", "-d", "refs/remotes/origin/remote-only"],
    ] {
        StdCommand::new("git")
            .args(&args)
            .current_dir(work.path())
            .output()
            .unwrap();
    }
    (work, remote)
}

#[test]
fn test_remote_branch_exists_true_and_false() {
    let (work, _remote) = setup_repo_with_remote_only_branch();
    let b = backend_at(work.path());
    // Not local…
    assert!(!b.branch_exists("remote-only").unwrap());
    // …but present on origin (cheap ls-remote, no fetch).
    assert!(b.remote_branch_exists("remote-only").unwrap());
    // Exact match only — no prefix false positives, and bogus → false.
    assert!(!b.remote_branch_exists("remote").unwrap());
    assert!(!b.remote_branch_exists("bogus-xyz").unwrap());
}

#[test]
fn test_remote_branch_exists_false_without_remote() {
    // No `origin` configured → best-effort Ok(false), never an error.
    let dir = setup_test_repo();
    assert!(!backend_at(dir.path()).remote_branch_exists("anything").unwrap());
}

#[test]
fn test_create_worktree_resume_existing_branch_checks_out_branch_tree() {
    // Resume must materialise the RESUMED BRANCH's tree, not the base's.
    // (Regression guard for the CoW path checking out `base` in the source
    // and reflinking that tree under a HEAD pointing at `branch`.) Runs the
    // plain path on non-reflink CI volumes and the CoW path on reflink ones
    // (ReFS/APFS/Btrfs) — the assertions must hold either way.
    let dir = setup_test_repo();
    // Branch `feature` carries a file that `main` does not.
    for args in [
        vec!["checkout", "-b", "feature"],
        vec!["add", "."],
    ] {
        StdCommand::new("git").args(&args).current_dir(dir.path()).output().unwrap();
    }
    std::fs::write(dir.path().join("feature.txt"), "feat\n").unwrap();
    for args in [
        vec!["add", "."],
        vec!["commit", "-m", "add feature.txt"],
        vec!["checkout", "main"],
    ] {
        StdCommand::new("git").args(&args).current_dir(dir.path()).output().unwrap();
    }

    let wt_path = dir.path().join("workspaces").join("feature");
    std::fs::create_dir_all(wt_path.parent().unwrap()).unwrap();

    let b = backend_at(dir.path());
    assert!(b.branch_exists("feature").unwrap());
    // base = "main" (the merge target); the worktree must hold FEATURE's tree.
    b.create_worktree(&wt_path, "feature", "main").unwrap();
    assert!(
        wt_path.join("feature.txt").exists(),
        "resumed worktree must contain the branch's own file, not base's tree"
    );
    assert!(wt_path.join("README.md").exists());

    assert_eq!(backend_at(&wt_path).current_branch().unwrap(), "feature");
}

#[test]
fn test_create_worktree_from_remote_materializes_branch() {
    let (work, _remote) = setup_repo_with_remote_only_branch();
    let wt_path = work.path().join("workspaces").join("remote-only");
    std::fs::create_dir_all(wt_path.parent().unwrap()).unwrap();
    let b = backend_at(work.path());
    b.create_worktree_from_remote(&wt_path, "remote-only").unwrap();
    assert!(wt_path.exists(), "worktree dir should exist");
    assert!(
        wt_path.join("README.md").exists(),
        "committed file should be checked out"
    );
    // The targeted fetch + create minted a local branch tracking origin.
    assert!(b.branch_exists("remote-only").unwrap());
}

#[test]
fn test_current_commit() {
    let dir = setup_test_repo();
    let commit = backend_at(dir.path()).current_commit();
    assert!(commit.is_ok());
    let hash = commit.unwrap();
    assert_eq!(hash.len(), 40);
}

// ---------------------------------------------------------------------------
// parse_shortstat tests (pure function)
// ---------------------------------------------------------------------------
#[test]
fn test_parse_shortstat_full() {
    let stat = parse_shortstat(" 3 files changed, 120 insertions(+), 30 deletions(-)");
    assert_eq!(stat.insertions, 120);
    assert_eq!(stat.deletions, 30);
}

#[test]
fn test_parse_shortstat_insertions_only() {
    let stat = parse_shortstat(" 1 file changed, 5 insertions(+)");
    assert_eq!(stat.insertions, 5);
    assert_eq!(stat.deletions, 0);
}

#[test]
fn test_parse_shortstat_deletions_only() {
    let stat = parse_shortstat(" 2 files changed, 10 deletions(-)");
    assert_eq!(stat.insertions, 0);
    assert_eq!(stat.deletions, 10);
}

#[test]
fn test_parse_shortstat_empty() {
    let stat = parse_shortstat("");
    assert_eq!(stat.insertions, 0);
    assert_eq!(stat.deletions, 0);
}

#[test]
fn test_parse_shortstat_single_change() {
    let stat = parse_shortstat(" 1 file changed, 1 insertion(+), 1 deletion(-)");
    assert_eq!(stat.insertions, 1);
    assert_eq!(stat.deletions, 1);
}
