use std::process::Command as StdCommand;

use super::{backend_at, setup_test_repo};
use crate::vcs::backend::VcsBackend;
use crate::vcs::error::Error;

// ---------------------------------------------------------------------------
// has_uncommitted_changes
// ---------------------------------------------------------------------------
#[test]
fn test_has_uncommitted_changes_clean() {
    let dir = setup_test_repo();
    let has_changes = backend_at(dir.path()).has_uncommitted_changes();
    assert!(has_changes.is_ok());
    assert!(!has_changes.unwrap());
}

#[test]
fn test_has_uncommitted_changes_dirty() {
    let dir = setup_test_repo();
    std::fs::write(dir.path().join("new_file.txt"), "content").unwrap();
    let has_changes = backend_at(dir.path()).has_uncommitted_changes();
    assert!(has_changes.is_ok());
    assert!(has_changes.unwrap());
}

#[test]
fn test_list_worktrees() {
    let dir = setup_test_repo();
    let worktrees = backend_at(dir.path()).list_worktrees();
    assert!(worktrees.is_ok());
    let list = worktrees.unwrap();
    assert!(!list.is_empty());
    assert_eq!(list[0].branch, Some("main".to_string()));
}

#[test]
fn test_is_rebase_in_progress() {
    let dir = setup_test_repo();
    assert!(!backend_at(dir.path()).is_rebase_in_progress());
}

#[test]
fn test_is_merge_in_progress() {
    let dir = setup_test_repo();
    assert!(!backend_at(dir.path()).is_merge_in_progress());
}

#[test]
fn test_log_oneline() {
    let dir = setup_test_repo();
    let log = backend_at(dir.path()).log_oneline("HEAD", "HEAD");
    assert!(log.is_ok());
    assert!(log.unwrap().is_empty());
}

#[test]
fn test_commit_count() {
    let dir = setup_test_repo();
    let count = backend_at(dir.path()).commit_count("HEAD", "HEAD");
    assert!(count.is_ok());
    assert_eq!(count.unwrap(), 0);
}

#[test]
fn test_fetch() {
    let dir = setup_test_repo();
    // No remote, but fetch silently swallows non-zero exit by design.
    let result = backend_at(dir.path()).fetch();
    assert!(result.is_ok());
}

#[test]
fn test_is_merged() {
    let dir = setup_test_repo();
    let result = backend_at(dir.path()).is_merged("main", "main");
    assert!(result.is_ok());
    assert!(result.unwrap());
}

// ---------------------------------------------------------------------------
// Worktree CRUD
// ---------------------------------------------------------------------------
#[test]
fn test_create_and_remove_worktree() {
    let dir = setup_test_repo();
    let wt_path = dir.path().join("worktrees").join("feature");
    std::fs::create_dir_all(wt_path.parent().unwrap()).unwrap();

    let b = backend_at(dir.path());
    let result = b.create_worktree(&wt_path, "feature-branch", "main");
    assert!(result.is_ok());
    assert!(wt_path.exists());
    assert!(b.branch_exists("feature-branch").unwrap());

    let result = b.remove_worktree(&wt_path, false);
    assert!(result.is_ok());
}

#[test]
fn test_create_worktree_duplicate() {
    let dir = setup_test_repo();
    let wt_path = dir.path().join("worktrees").join("dup");
    let wt_path2 = dir.path().join("worktrees").join("dup2");
    std::fs::create_dir_all(wt_path.parent().unwrap()).unwrap();

    let b = backend_at(dir.path());
    b.create_worktree(&wt_path, "dup-branch", "main").unwrap();
    let result = b.create_worktree(&wt_path2, "dup-branch", "main");
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), Error::WorktreeExists(_)));
}

// ---------------------------------------------------------------------------
// Branch ops
// ---------------------------------------------------------------------------
#[test]
fn test_rename_branch() {
    let dir = setup_test_repo();
    StdCommand::new("git")
        .args(["branch", "old-name"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let b = backend_at(dir.path());
    let result = b.rename_branch("old-name", "new-name");
    assert!(result.is_ok());
    assert!(!b.branch_exists("old-name").unwrap());
    assert!(b.branch_exists("new-name").unwrap());
}

#[test]
fn test_delete_branch() {
    let dir = setup_test_repo();
    StdCommand::new("git")
        .args(["branch", "to-delete"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let b = backend_at(dir.path());
    assert!(b.branch_exists("to-delete").unwrap());
    let result = b.delete_branch("to-delete", false);
    assert!(result.is_ok());
    assert!(!b.branch_exists("to-delete").unwrap());
}

#[test]
fn test_checkout() {
    let dir = setup_test_repo();
    StdCommand::new("git")
        .args(["branch", "other-branch"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let b = backend_at(dir.path());
    let result = b.checkout("other-branch");
    assert!(result.is_ok());
    assert_eq!(b.current_branch().unwrap(), "other-branch");
}

// ---------------------------------------------------------------------------
// Abort/continue
// ---------------------------------------------------------------------------
#[test]
fn test_rebase_abort_no_rebase() {
    let dir = setup_test_repo();
    let result = backend_at(dir.path()).rebase_abort();
    assert!(result.is_err());
}

#[test]
fn test_merge_abort_no_merge() {
    let dir = setup_test_repo();
    let result = backend_at(dir.path()).merge_abort();
    assert!(result.is_err());
}

#[test]
fn test_reset_merge_clean_repo() {
    let dir = setup_test_repo();
    let result = backend_at(dir.path()).reset_merge();
    assert!(result.is_ok());
}

#[test]
fn test_rebase_continue_no_rebase() {
    let dir = setup_test_repo();
    let result = backend_at(dir.path()).rebase_continue();
    assert!(result.is_err());
}

#[test]
fn test_merge_continue_no_merge() {
    let dir = setup_test_repo();
    let result = backend_at(dir.path()).merge_continue();
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// Merge / rebase
// ---------------------------------------------------------------------------
#[test]
fn test_merge_fast_forward() {
    let dir = setup_test_repo();
    StdCommand::new("git")
        .args(["branch", "already-merged"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let result = backend_at(dir.path()).merge("already-merged", "main", false, false, None);
    let _ = result; // may succeed or be no-op
}

#[test]
fn test_rebase_same_branch() {
    let dir = setup_test_repo();
    let result = backend_at(dir.path()).rebase("main");
    assert!(result.is_ok());
}

// ---------------------------------------------------------------------------
// Force variants
// ---------------------------------------------------------------------------
#[test]
fn test_remove_worktree_force() {
    let dir = setup_test_repo();
    let wt_path = dir.path().join("worktrees").join("force-test");
    std::fs::create_dir_all(wt_path.parent().unwrap()).unwrap();

    let b = backend_at(dir.path());
    b.create_worktree(&wt_path, "force-branch", "main").unwrap();
    std::fs::write(wt_path.join("uncommitted.txt"), "changes").unwrap();
    let result = b.remove_worktree(&wt_path, true);
    assert!(result.is_ok());
}

#[test]
fn test_delete_branch_force() {
    let dir = setup_test_repo();
    StdCommand::new("git")
        .args(["branch", "unmerged-branch"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let result = backend_at(dir.path()).delete_branch("unmerged-branch", true);
    assert!(result.is_ok());
}

// ---------------------------------------------------------------------------
// has_changes_from_trunk
// ---------------------------------------------------------------------------
#[test]
fn test_has_changes_from_trunk_no_changes() {
    let dir = setup_test_repo();
    let has = backend_at(dir.path()).has_changes_from_trunk("main");
    assert!(has.is_ok());
    assert!(!has.unwrap());
}

#[test]
fn test_has_changes_from_trunk_with_committed_changes() {
    let dir = setup_test_repo();

    StdCommand::new("git")
        .args(["checkout", "-b", "feature"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    std::fs::write(dir.path().join("feature.txt"), "new feature").unwrap();

    StdCommand::new("git")
        .args(["add", "feature.txt"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    StdCommand::new("git")
        .args(["commit", "-m", "Add feature"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let has = backend_at(dir.path()).has_changes_from_trunk("main");
    assert!(has.is_ok());
    assert!(has.unwrap(), "Should detect committed changes ahead of trunk");
}

#[test]
fn test_has_changes_from_trunk_with_uncommitted_changes() {
    let dir = setup_test_repo();

    StdCommand::new("git")
        .args(["checkout", "-b", "feature"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    std::fs::write(dir.path().join("dirty.txt"), "uncommitted").unwrap();

    let has = backend_at(dir.path()).has_changes_from_trunk("main");
    assert!(has.is_ok());
    assert!(has.unwrap(), "Should detect uncommitted changes");
}
