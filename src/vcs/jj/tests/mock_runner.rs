// ===========================================================================
// vcs/jj/tests/mock_runner — parser-heavy unit tests via MockRunner
// ===========================================================================
//
// These tests run without `jj` on PATH because they substitute the runner
// entirely. Use them for: parser correctness, error-mapping shape, command
// argument construction.
//
// **Don't** move e2e tests here — the real-binary tests catch jj-version
// drift and template-language mismatches that MockRunner can't simulate.

use std::sync::Arc;

use procpilot::testing::{nonzero, ok_str, MockRunner};

use super::super::JjBackend;
use crate::vcs::backend::VcsBackend;
use crate::vcs::error::Error;

// ---------------------------------------------------------------------------
// F-1 method shape (argument construction + parsing of canned output)
// ---------------------------------------------------------------------------

#[test]
fn current_commit_emits_no_graph_template() {
    let mock = MockRunner::new().expect(
        "jj log -r @ -T commit_id --no-graph --limit 1",
        ok_str("deadbeef1234\n"),
    );
    let backend = JjBackend::with_runner(Arc::new(mock));
    assert_eq!(backend.current_commit().unwrap(), "deadbeef1234");
}

#[test]
fn current_branch_parses_log_template_output() {
    // Sample of vcs-runner's LOG_TEMPLATE output, one JSON object per line.
    let log_line = r#"{"commitId":"abc123","changeId":"xyz789","authorName":"T","authorEmail":"<t@t>","description":"msg","parents":["par111"],"localBookmarks":["main"],"remoteBookmarks":[],"isWorkingCopy":"true","conflict":"false","empty":"false"}
"#;
    // Use a predicate matcher so we don't have to spell out the long template
    // string in the expected display.
    let mock = MockRunner::new().expect_when(
        move |cmd| {
            let s = cmd.to_string();
            s.starts_with("jj log -r @") && s.contains("--no-graph")
        },
        ok_str(log_line),
    );
    let backend = JjBackend::with_runner(Arc::new(mock));
    assert_eq!(backend.current_branch().unwrap(), "main");
}

#[test]
fn current_branch_errors_when_local_bookmarks_empty() {
    let log_line = r#"{"commitId":"abc","changeId":"xyz","authorName":"T","authorEmail":"<t@t>","description":"","parents":[],"localBookmarks":[],"remoteBookmarks":[],"isWorkingCopy":"true","conflict":"false","empty":"true"}
"#;
    let mock = MockRunner::new().expect_when(
        |cmd| cmd.to_string().starts_with("jj log -r @"),
        ok_str(log_line),
    );
    let backend = JjBackend::with_runner(Arc::new(mock));
    let err = backend.current_branch().unwrap_err();
    assert!(
        err.to_string().contains("no bookmark on @"),
        "expected guidance message, got: {err}"
    );
}

#[test]
fn current_branch_picks_lexicographically_smallest_when_multiple() {
    // Multiple bookmarks on @ — must return the smallest deterministically.
    let log_line = r#"{"commitId":"abc","changeId":"xyz","authorName":"T","authorEmail":"<t@t>","description":"","parents":[],"localBookmarks":["zebra","alpha","main"],"remoteBookmarks":[],"isWorkingCopy":"true","conflict":"false","empty":"false"}
"#;
    let mock = MockRunner::new().expect_when(
        |cmd| cmd.to_string().starts_with("jj log -r @"),
        ok_str(log_line),
    );
    let backend = JjBackend::with_runner(Arc::new(mock));
    assert_eq!(backend.current_branch().unwrap(), "alpha");
}

#[test]
fn local_branches_filters_via_bookmark_template_parse() {
    // Two bookmarks, parsable by vcs-runner's `parse_bookmark_output`.
    let bookmark_json = concat!(
        r#"{"name":"main","commitId":"abc","changeId":"x","localBookmarks":["main"],"remoteBookmarks":["main@origin"]}"#,
        "\n",
        r#"{"name":"feature","commitId":"def","changeId":"y","localBookmarks":["feature"],"remoteBookmarks":[]}"#,
        "\n",
    );
    let mock = MockRunner::new().expect_when(
        |cmd| cmd.to_string().starts_with("jj bookmark list -T"),
        ok_str(bookmark_json),
    );
    let backend = JjBackend::with_runner(Arc::new(mock));
    let mut bookmarks = backend.local_branches().unwrap();
    bookmarks.sort();
    assert_eq!(bookmarks, vec!["feature".to_string(), "main".to_string()]);
}

#[test]
fn repo_root_maps_nonzero_to_not_in_repo() {
    let mock = MockRunner::new().expect(
        "jj root",
        nonzero(1, "There is no jj repo in \".\""),
    );
    let backend = JjBackend::with_runner(Arc::new(mock));
    assert!(matches!(backend.repo_root(), Err(Error::NotInRepo)));
}

#[test]
fn move_worktree_returns_unsupported_with_remediation() {
    let mock = MockRunner::new(); // no expectations — should never spawn
    let backend = JjBackend::with_runner(Arc::new(mock));
    let err = backend
        .move_worktree(std::path::Path::new("/a"), std::path::Path::new("/b"))
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("move_worktree"));
    assert!(
        msg.contains("remove and re-create"),
        "Unsupported error should hint the remediation, got: {msg}"
    );
}

// has_staged_changes_always_false_no_subprocess was removed in F-7 along
// with the method itself — there's no caller and jj's "no staging area"
// invariant is documented in AGENTS.md.

#[test]
fn is_rebase_in_progress_always_false_no_subprocess() {
    let mock = MockRunner::new();
    let backend = JjBackend::with_runner(Arc::new(mock));
    assert!(!backend.is_rebase_in_progress());
}

#[test]
fn name_is_jj() {
    let backend = JjBackend::new();
    assert_eq!(backend.name(), "jj");
}

// ---------------------------------------------------------------------------
// Sync hint methods — Unsupported with jj-specific guidance
// ---------------------------------------------------------------------------

#[test]
fn rebase_abort_explains_no_in_progress_state() {
    let mock = MockRunner::new();
    let backend = JjBackend::with_runner(Arc::new(mock));
    let msg = backend.rebase_abort().unwrap_err().to_string();
    assert!(msg.contains("rebase_abort"));
    assert!(msg.contains("conflicts in commits"));
}

#[test]
fn merge_continue_explains_no_in_progress_state() {
    let mock = MockRunner::new();
    let backend = JjBackend::with_runner(Arc::new(mock));
    let msg = backend.merge_continue().unwrap_err().to_string();
    assert!(msg.contains("merge_continue"));
}

// ---------------------------------------------------------------------------
// F-3: parse_jj_stat_footer — pure parser tests
// ---------------------------------------------------------------------------

use crate::vcs::jj::parse_jj_stat_footer;

#[test]
fn parse_jj_stat_footer_full() {
    let input = " src/foo.rs | 10 ++++------\n src/bar.rs | 5 +++--\n 2 files changed, 7 insertions(+), 8 deletions(-)";
    let stat = parse_jj_stat_footer(input);
    assert_eq!(stat.insertions, 7);
    assert_eq!(stat.deletions, 8);
}

#[test]
fn parse_jj_stat_footer_insertions_only() {
    let input = " src/foo.rs | 5 +++++\n 1 file changed, 5 insertions(+)";
    let stat = parse_jj_stat_footer(input);
    assert_eq!(stat.insertions, 5);
    assert_eq!(stat.deletions, 0);
}

#[test]
fn parse_jj_stat_footer_deletions_only() {
    let input = " src/foo.rs | 10 ----------\n 1 file changed, 10 deletions(-)";
    let stat = parse_jj_stat_footer(input);
    assert_eq!(stat.insertions, 0);
    assert_eq!(stat.deletions, 10);
}

#[test]
fn parse_jj_stat_footer_empty_output() {
    let stat = parse_jj_stat_footer("");
    assert_eq!(stat.insertions, 0);
    assert_eq!(stat.deletions, 0);
}

#[test]
fn parse_jj_stat_footer_no_footer_returns_zero() {
    // If --stat produces only per-file lines without the summary footer,
    // we shouldn't error — just return zeros.
    let stat = parse_jj_stat_footer(" src/foo.rs | 10 ++++++++++\n");
    assert_eq!(stat.insertions, 0);
    assert_eq!(stat.deletions, 0);
}

#[test]
fn is_merge_in_progress_true_when_jj_st_reports_conflicts() {
    // Faux `jj st` output with the unresolved-conflicts marker.
    let st = "Working copy changes:\nC src/foo.rs\nThere are unresolved conflicts at these paths:\nsrc/foo.rs   2-sided conflict\n";
    let mock = MockRunner::new()
        .expect_when(|cmd| cmd.to_string() == "jj st", ok_str(st));
    let backend = JjBackend::with_runner(Arc::new(mock));
    assert!(backend.is_merge_in_progress());
}

#[test]
fn is_merge_in_progress_false_when_jj_st_clean() {
    let st = "Working copy : zzzz Initial commit\nParent commit: yyyy (empty)\n";
    let mock = MockRunner::new()
        .expect_when(|cmd| cmd.to_string() == "jj st", ok_str(st));
    let backend = JjBackend::with_runner(Arc::new(mock));
    assert!(!backend.is_merge_in_progress());
}
