// ===========================================================================
// vcs/git/tests - GitBackend unit tests
// ===========================================================================
//
// Two layers, matching the project's "validate against the real tool" stance:
//   - **Real-git e2e** (`#[tokio::test]`): a `GitBackend` anchored at a fresh
//     temp repo via `backend_at`, driving the actual `git` binary through the
//     processkit + vcs-git stack. No mocks — these validate the async backend
//     end-to-end at the unit level.
//   - **Pure parsers / predicates** (`#[test]`): `parse_worktree_list`,
//     `parse_shortstat`, `clean_git_error`, `is_transient_fetch_err` — no
//     subprocess, no runtime.
//
// Command-building (the exact argv each method emits) is owned and tested by the
// `vcs-git` crate; we don't re-assert it here.

use std::path::Path;
use std::process::Command as StdCommand;

use tempfile::tempdir;

use super::GitBackend;
use crate::vcs::backend::VcsBackend;

/// Set up a minimal real git repo with one commit on `main`.
fn setup_test_repo() -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    let path = dir.path();

    let run = |args: &[&str]| {
        StdCommand::new("git").args(args).current_dir(path).output().expect("git failed");
    };
    run(&["init", "-q"]);
    run(&["config", "user.email", "test@test.com"]);
    run(&["config", "user.name", "Test"]);
    std::fs::write(path.join("README.md"), "# Test\n").unwrap();
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "Initial commit"]);
    run(&["branch", "-M", "main"]);

    dir
}

/// A `GitBackend` anchored at `path` (real runner, explicit cwd — no process
/// chdir, so tests run in parallel).
fn backend_at(path: &Path) -> GitBackend {
    GitBackend::at(path.to_path_buf())
}

// ---------------------------------------------------------------------------
// Real-git e2e
// ---------------------------------------------------------------------------

#[tokio::test]
async fn clean_repo_has_no_uncommitted_changes() {
    let dir = setup_test_repo();
    assert!(!backend_at(dir.path()).has_uncommitted_changes().await.unwrap());
}

#[tokio::test]
async fn dirty_repo_has_uncommitted_changes() {
    let dir = setup_test_repo();
    std::fs::write(dir.path().join("new_file.txt"), "content").unwrap();
    assert!(backend_at(dir.path()).has_uncommitted_changes().await.unwrap());
}

#[tokio::test]
async fn lists_the_main_worktree() {
    let dir = setup_test_repo();
    let list = backend_at(dir.path()).list_worktrees().await.unwrap();
    assert!(!list.is_empty());
    assert_eq!(list[0].branch, Some("main".to_string()));
}

#[tokio::test]
async fn current_branch_is_main() {
    let dir = setup_test_repo();
    assert_eq!(backend_at(dir.path()).current_branch().await.unwrap(), "main");
}

#[tokio::test]
async fn branch_exists_reports_presence() {
    let dir = setup_test_repo();
    let b = backend_at(dir.path());
    assert!(b.branch_exists("main").await.unwrap());
    assert!(!b.branch_exists("nope").await.unwrap());
}

#[tokio::test]
async fn detect_trunk_finds_main() {
    let dir = setup_test_repo();
    assert_eq!(backend_at(dir.path()).detect_trunk().await.unwrap(), "main");
}

#[tokio::test]
async fn no_rebase_or_merge_in_progress_on_clean_repo() {
    let dir = setup_test_repo();
    let b = backend_at(dir.path());
    assert!(!b.is_rebase_in_progress().await);
    assert!(!b.is_merge_in_progress().await);
}

#[tokio::test]
async fn commit_count_same_ref_is_zero() {
    let dir = setup_test_repo();
    assert_eq!(backend_at(dir.path()).commit_count("HEAD", "HEAD").await.unwrap(), 0);
}

#[tokio::test]
async fn log_oneline_same_ref_is_empty() {
    let dir = setup_test_repo();
    assert!(backend_at(dir.path()).log_oneline("HEAD", "HEAD").await.unwrap().is_empty());
}

#[tokio::test]
async fn create_plain_worktree_then_list_and_remove() {
    let dir = setup_test_repo();
    let b = backend_at(dir.path());

    // Disable CoW so this is a fast, deterministic plain `git worktree add`.
    unsafe {
        std::env::set_var(crate::cow::DISABLE_COW_ENV, "1");
    }
    let wt_path = dir.path().join("wt-feature");
    let outcome = b.create_worktree(&wt_path, "feature", "main").await.unwrap();
    assert_eq!(outcome, crate::vcs::CreateOutcome::Plain);

    let list = b.list_worktrees().await.unwrap();
    assert!(list.iter().any(|wt| wt.branch.as_deref() == Some("feature")));

    b.remove_worktree(&wt_path, true).await.unwrap();
    let list = b.list_worktrees().await.unwrap();
    assert!(!list.iter().any(|wt| wt.branch.as_deref() == Some("feature")));
}

#[tokio::test]
async fn outside_a_repo_repo_root_is_not_in_repo() {
    let dir = tempdir().unwrap(); // plain dir, no `git init`
    assert!(matches!(
        backend_at(dir.path()).repo_root().await,
        Err(crate::vcs::Error::NotInRepo)
    ));
}

// ---------------------------------------------------------------------------
// Pure parsers / predicates
// ---------------------------------------------------------------------------

#[test]
fn parse_worktree_list_extracts_branch_and_commit() {
    let porcelain = "worktree /repo\nHEAD abc123\nbranch refs/heads/main\n\n\
                     worktree /repo/wt\nHEAD def456\nbranch refs/heads/feature\n";
    let wts = super::parse_worktree_list(porcelain);
    assert_eq!(wts.len(), 2);
    assert_eq!(wts[0].branch.as_deref(), Some("main"));
    assert_eq!(wts[0].commit.as_deref(), Some("abc123"));
    assert_eq!(wts[1].branch.as_deref(), Some("feature"));
}

#[test]
fn parse_shortstat_reads_insertions_and_deletions() {
    let stat = super::parse_shortstat(" 3 files changed, 120 insertions(+), 30 deletions(-)");
    assert_eq!((stat.insertions, stat.deletions), (120, 30));
    assert_eq!(super::parse_shortstat(""), crate::vcs::DiffStat::default());
}

#[test]
fn clean_git_error_strips_prefixes_and_rewrites_uncommitted() {
    assert_eq!(super::clean_git_error("fatal: bad thing"), "bad thing");
    assert_eq!(super::clean_git_error("error: nope"), "nope");
    let msg = super::clean_git_error(
        "'/path/to/feature' contains modified or untracked files, use --force to delete it",
    );
    assert_eq!(msg, "worktree 'feature' has uncommitted changes, use --force");
}

#[test]
fn is_transient_fetch_err_matches_network_markers() {
    assert!(super::ops::is_transient_fetch_err("fatal: Could not resolve host: github.com"));
    assert!(super::ops::is_transient_fetch_err("Connection reset by peer"));
    assert!(super::ops::is_transient_fetch_err("fatal: the remote end hung up unexpectedly"));
    assert!(!super::ops::is_transient_fetch_err("fatal: couldn't find remote ref refs/heads/x"));
}
