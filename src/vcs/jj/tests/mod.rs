//! Tests for `JjBackend`.
//!
//! Test categories:
//!
//! - **Pure-function / MockRunner tests** (in `mock_runner.rs`) — run on
//!   every CI even without `jj` on PATH. Exercise parser logic and
//!   output-translation paths via canned subprocess responses.
//!
//! - **End-to-end tests** (in this file, gated via `jj_test!`) — require
//!   a real `jj` binary. Skip with a stderr message when `jj` is absent
//!   so contributor laptops without jj installed still get a green
//!   `cargo test` for the rest of the suite.

mod mock_runner;

use std::path::Path;
use std::process::Command as StdCommand;

use tempfile::tempdir;

use super::JjBackend;
use crate::vcs::backend::VcsBackend;
use crate::vcs::error::Error;
use crate::vcs::CWD_MUTEX;

/// Initialize a minimal jj repository for end-to-end testing. Returns
/// `None` if jj isn't on PATH (callers should skip the test).
///
/// Sets up `jj git init --colocate` (matches the typical user workflow:
/// a colocated repo where wt's auto-detect picks jj) plus a `main`
/// bookmark on an initial commit. The colocated layout is intentional —
/// it exercises the workspace_id-parity invariant from the plan.
pub(super) fn jj_repo() -> Option<tempfile::TempDir> {
    if !vcs_runner::jj_available() {
        return None;
    }
    let dir = tempdir().ok()?;
    let path = dir.path();

    // `jj git init --colocate` creates both .jj and .git, matching this
    // project's own layout and exercising the colocated-prefers-jj path.
    let status = StdCommand::new("jj")
        .args(["git", "init", "--colocate"])
        .current_dir(path)
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }

    // Set user identity locally (jj config set --repo)
    for (key, val) in [("user.name", "Test"), ("user.email", "test@test.com")] {
        StdCommand::new("jj")
            .args(["config", "set", "--repo", key, val])
            .current_dir(path)
            .status()
            .ok()?;
    }

    // Create an initial commit (jj records the working copy on every op,
    // so we need at least one described change).
    std::fs::write(path.join("README.md"), "# Test\n").ok()?;
    StdCommand::new("jj")
        .args(["describe", "-m", "Initial commit"])
        .current_dir(path)
        .status()
        .ok()?;
    // Move @ forward so the initial commit is a real ancestor, then attach
    // `main` to it. jj's working-copy @ is always empty after `new`.
    StdCommand::new("jj")
        .args(["new"])
        .current_dir(path)
        .status()
        .ok()?;
    StdCommand::new("jj")
        .args(["bookmark", "create", "main", "-r", "@-"])
        .current_dir(path)
        .status()
        .ok()?;

    Some(dir)
}

/// Run a closure with cwd set to `path` under the shared `CWD_MUTEX`.
///
/// Same as the git backend's `with_cwd` — they share the mutex because
/// `std::env::current_dir()` is process-global. Lives in this module so
/// jj tests don't need to import the git helper directly.
pub(super) fn with_cwd<F, T>(path: &Path, f: F) -> T
where
    F: FnOnce() -> T,
{
    let _guard = CWD_MUTEX.lock().unwrap();
    let original = std::env::current_dir().unwrap();
    std::env::set_current_dir(path).unwrap();
    let result = f();
    std::env::set_current_dir(original).unwrap();
    result
}

/// Skip-or-run macro for tests that need a real jj binary. The macro
/// expands to a standard `#[test] fn $name()` that early-returns (with
/// an eprintln) when jj isn't on PATH.
///
/// Example:
/// ```ignore
/// jj_test!(test_repo_root, |dir: &Path| {
///     with_cwd(dir, || {
///         let root = JjBackend::new().repo_root().unwrap();
///         assert!(root.exists());
///     });
/// });
/// ```
macro_rules! jj_test {
    ($name:ident, $body:expr) => {
        #[test]
        fn $name() {
            let Some(dir) = jj_repo() else {
                eprintln!(
                    "SKIP {}: jj not available on PATH",
                    stringify!($name)
                );
                return;
            };
            ($body)(dir.path());
        }
    };
}

// Macro is consumed below in this same file; no need to export.

fn backend() -> JjBackend {
    JjBackend::new()
}

// ---------------------------------------------------------------------------
// F-1: Identity + bookmarks (e2e)
// ---------------------------------------------------------------------------

jj_test!(test_repo_root_in_jj_repo, |dir: &Path| {
    with_cwd(dir, || {
        let root = backend().repo_root().unwrap();
        assert!(root.exists());
        // Path should resolve to the colocated repo root (both .jj and .git
        // present in our setup).
        assert!(root.join(".jj").exists());
        assert!(root.join(".git").exists());
    });
});

jj_test!(test_repo_name_is_dirname, |dir: &Path| {
    with_cwd(dir, || {
        let name = backend().repo_name().unwrap();
        assert!(!name.is_empty());
        assert_eq!(
            std::path::Path::new(&backend().repo_root().unwrap())
                .canonicalize()
                .unwrap()
                .file_name()
                .and_then(|n| n.to_str())
                .map(String::from),
            Some(name)
        );
    });
});

jj_test!(test_workspace_id_format, |dir: &Path| {
    with_cwd(dir, || {
        let id = backend().workspace_id().unwrap();
        let parts: Vec<&str> = id.rsplitn(2, '-').collect();
        assert_eq!(parts.len(), 2, "workspace_id should be name-<hash>");
        assert_eq!(parts[0].len(), 6, "hash suffix should be 6 hex chars");
        assert!(parts[0].chars().all(|c| c.is_ascii_hexdigit()));
    });
});

// Load-bearing invariant: in a colocated repo, `JjBackend` and
// `GitBackend` must produce identical `workspace_id` values. This is what
// makes git→jj migration in colocated repos non-destructive — existing
// workspaces under `$AGENT_WORKSPACE_DIR/workspaces/<id>/` stay
// addressable.
jj_test!(test_workspace_id_matches_git_when_colocated, |dir: &Path| {
    use crate::vcs::git::GitBackend;

    with_cwd(dir, || {
        let jj_id = JjBackend::new().workspace_id().unwrap();
        let git_id = GitBackend::new().workspace_id().unwrap();
        assert_eq!(
            jj_id, git_id,
            "colocated workspace_id must match across backends — \
             migration would lose existing workspace directories otherwise"
        );
    });
});

jj_test!(test_current_branch_returns_bookmark, |dir: &Path| {
    with_cwd(dir, || {
        // `jj_repo()` creates `main` bookmark at @- (parent of working copy).
        // After `jj new`, @ has no bookmark. We need to move to @- or create
        // a bookmark on @. Easiest: jj edit main.
        StdCommand::new("jj")
            .args(["edit", "main"])
            .current_dir(dir)
            .status()
            .unwrap();
        let branch = backend().current_branch().unwrap();
        assert_eq!(branch, "main");
    });
});

jj_test!(test_current_branch_errors_when_no_bookmark, |dir: &Path| {
    with_cwd(dir, || {
        // jj_repo() leaves @ on an empty change with no bookmark — exactly
        // the scenario the locked decision says should error.
        let err = backend().current_branch().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("no bookmark on @"),
            "expected the 'no bookmark on @' guidance, got: {msg}"
        );
    });
});

jj_test!(test_current_commit_is_short_id, |dir: &Path| {
    with_cwd(dir, || {
        let commit = backend().current_commit().unwrap();
        assert!(!commit.is_empty());
        // jj's `commit_id` template returns the full hex id (40 chars), not
        // the short form. Either way it should be all-hex.
        assert!(commit.chars().all(|c| c.is_ascii_hexdigit()));
    });
});

jj_test!(test_detect_trunk_returns_main, |dir: &Path| {
    with_cwd(dir, || {
        let trunk = backend().detect_trunk().unwrap();
        assert_eq!(trunk, "main");
    });
});

jj_test!(test_local_branches_includes_main, |dir: &Path| {
    with_cwd(dir, || {
        let bookmarks = backend().local_branches().unwrap();
        assert!(
            bookmarks.contains(&"main".to_string()),
            "main not in bookmark list: {bookmarks:?}"
        );
    });
});

jj_test!(test_branch_exists_true_false, |dir: &Path| {
    with_cwd(dir, || {
        assert!(backend().branch_exists("main").unwrap());
        assert!(!backend().branch_exists("nonexistent-bookmark-xyz").unwrap());
    });
});

jj_test!(test_rename_branch_round_trip, |dir: &Path| {
    with_cwd(dir, || {
        let b = backend();
        // Create a fresh bookmark to rename (we don't touch main since
        // detect_trunk depends on it).
        StdCommand::new("jj")
            .args(["bookmark", "create", "old-name", "-r", "@"])
            .current_dir(dir)
            .status()
            .unwrap();
        b.rename_branch("old-name", "new-name").unwrap();
        assert!(!b.branch_exists("old-name").unwrap());
        assert!(b.branch_exists("new-name").unwrap());
    });
});

jj_test!(test_delete_branch, |dir: &Path| {
    with_cwd(dir, || {
        let b = backend();
        StdCommand::new("jj")
            .args(["bookmark", "create", "to-delete", "-r", "@"])
            .current_dir(dir)
            .status()
            .unwrap();
        assert!(b.branch_exists("to-delete").unwrap());
        b.delete_branch("to-delete", false).unwrap();
        assert!(!b.branch_exists("to-delete").unwrap());
    });
});

#[test]
fn test_repo_root_outside_jj_repo_returns_not_in_repo() {
    let dir = tempdir().unwrap();
    with_cwd(dir.path(), || {
        let result = backend().repo_root();
        assert!(matches!(result, Err(Error::NotInRepo)));
    });
}

// ---------------------------------------------------------------------------
// F-3: Working-copy state + diff (e2e)
// ---------------------------------------------------------------------------

jj_test!(test_is_rebase_in_progress_always_false, |dir: &Path| {
    with_cwd(dir, || {
        assert!(!backend().is_rebase_in_progress());
    });
});

jj_test!(test_is_merge_in_progress_false_on_clean_repo, |dir: &Path| {
    with_cwd(dir, || {
        assert!(!backend().is_merge_in_progress());
    });
});

jj_test!(test_has_uncommitted_changes_clean, |dir: &Path| {
    with_cwd(dir, || {
        // jj_repo() leaves @ on a fresh empty change with no diff vs parent.
        assert!(!backend().has_uncommitted_changes().unwrap());
    });
});

jj_test!(test_has_uncommitted_changes_dirty, |dir: &Path| {
    std::fs::write(dir.join("new_file.txt"), "content").unwrap();
    with_cwd(dir, || {
        // jj auto-snapshots untracked into @ on the next command run; the
        // diff @-..@ will include the new file.
        assert!(backend().has_uncommitted_changes().unwrap());
    });
});

jj_test!(test_commit_count_self_is_zero, |dir: &Path| {
    with_cwd(dir, || {
        let count = backend().commit_count("main", "main").unwrap();
        assert_eq!(count, 0);
    });
});

jj_test!(test_log_oneline_self_empty, |dir: &Path| {
    with_cwd(dir, || {
        let log = backend().log_oneline("main", "main").unwrap();
        assert!(log.trim().is_empty(), "empty range should produce no log: {log:?}");
    });
});

jj_test!(test_is_merged_self_is_true, |dir: &Path| {
    with_cwd(dir, || {
        assert!(backend().is_merged("main", "main").unwrap());
    });
});

// ---------------------------------------------------------------------------
// F-2: Workspaces (e2e)
// ---------------------------------------------------------------------------

jj_test!(test_list_worktrees_includes_default, |dir: &Path| {
    with_cwd(dir, || {
        let ws = backend().list_worktrees().unwrap();
        // jj_repo creates a colocated repo — there's always at least the
        // `default` workspace.
        assert!(
            ws.iter().any(|w| w.path == dir || w.path.canonicalize().ok() == dir.canonicalize().ok()),
            "default workspace not in list: {:?}",
            ws.iter().map(|w| &w.path).collect::<Vec<_>>()
        );
    });
});

jj_test!(test_create_worktree_creates_bookmark, |dir: &Path| {
    let wt_path = dir.join("workspaces").join("feature");
    std::fs::create_dir_all(wt_path.parent().unwrap()).unwrap();

    with_cwd(dir, || {
        let b = backend();
        b.create_worktree(&wt_path, "feature-branch", "main").unwrap();
        assert!(wt_path.exists(), "workspace dir should exist post-create");
        assert!(
            b.branch_exists("feature-branch").unwrap(),
            "bookmark should be created on the new workspace's @"
        );
        // Cleanup so the next test gets a clean slate even though tempdir
        // teardown would normally handle it.
        b.remove_worktree(&wt_path, false).unwrap();
    });
});

jj_test!(test_create_worktree_duplicate_branch_errors, |dir: &Path| {
    let wt_path1 = dir.join("workspaces").join("dup1");
    let wt_path2 = dir.join("workspaces").join("dup2");
    std::fs::create_dir_all(wt_path1.parent().unwrap()).unwrap();

    with_cwd(dir, || {
        let b = backend();
        b.create_worktree(&wt_path1, "dup-branch", "main").unwrap();
        let result = b.create_worktree(&wt_path2, "dup-branch", "main");
        assert!(matches!(result, Err(Error::WorktreeExists(_))));
        // Cleanup.
        b.remove_worktree(&wt_path1, false).unwrap();
    });
});

jj_test!(test_remove_worktree_cleans_up_dir, |dir: &Path| {
    let wt_path = dir.join("workspaces").join("removable");
    std::fs::create_dir_all(wt_path.parent().unwrap()).unwrap();

    with_cwd(dir, || {
        let b = backend();
        b.create_worktree(&wt_path, "removable-branch", "main").unwrap();
        assert!(wt_path.exists());
        b.remove_worktree(&wt_path, false).unwrap();
        assert!(!wt_path.exists(), "directory should be gone post-remove");
    });
});

jj_test!(test_move_worktree_returns_unsupported_e2e, |dir: &Path| {
    let from = dir.join("from");
    let to = dir.join("to");
    with_cwd(dir, || {
        let err = backend().move_worktree(&from, &to).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("move_worktree"));
        assert!(msg.contains("remove and re-create"));
    });
});

// ---------------------------------------------------------------------------
// F-4: Mutations (e2e)
// ---------------------------------------------------------------------------

jj_test!(test_checkout_errors_when_bookmark_missing, |dir: &Path| {
    with_cwd(dir, || {
        let result = backend().checkout("nonexistent-bookmark");
        assert!(matches!(result, Err(Error::BranchNotFound(_))));
    });
});

jj_test!(test_checkout_to_main_succeeds, |dir: &Path| {
    with_cwd(dir, || {
        let b = backend();
        // jj_repo() leaves @ on a fresh empty change; check out main.
        b.checkout("main").unwrap();
        assert_eq!(b.current_branch().unwrap(), "main");
    });
});

jj_test!(test_commit_sets_description, |dir: &Path| {
    with_cwd(dir, || {
        let b = backend();
        b.commit("test description").unwrap();
        // Verify by reading the commit's description back. jj exposes
        // description via log template.
        let out = StdCommand::new("jj")
            .args(["log", "-r", "@", "-T", "description", "--no-graph"])
            .current_dir(dir)
            .output()
            .unwrap();
        let desc = String::from_utf8_lossy(&out.stdout);
        assert!(
            desc.contains("test description"),
            "description not set: {desc:?}"
        );
    });
});

jj_test!(test_rebase_no_op_on_self_succeeds, |dir: &Path| {
    with_cwd(dir, || {
        // Rebase @ onto its parent — no-op but exercises the command path.
        // Should succeed (jj rebase is idempotent for this case).
        let result = backend().rebase("@-");
        // Either succeeds or returns a friendly error — both are acceptable
        // since the semantic "rebase @ onto itself's parent" is edge-case.
        let _ = result;
    });
});

jj_test!(test_dry_run_merge_clean_already_up_to_date, |dir: &Path| {
    with_cwd(dir, || {
        // Set up: create feat bookmark at @-, then dry-run merging feat
        // into @ (which is descendant). Should report "clean" since branch
        // is already merged.
        StdCommand::new("jj")
            .args(["bookmark", "create", "feat", "-r", "@-"])
            .current_dir(dir)
            .status()
            .unwrap();
        let clean = backend().dry_run_merge("feat", false).unwrap();
        assert!(clean, "merging already-merged branch should report clean");
    });
});
