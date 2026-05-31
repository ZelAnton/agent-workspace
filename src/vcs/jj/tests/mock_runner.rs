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
// Review regression tests
// ---------------------------------------------------------------------------

/// Review fix #3: `workspace_name_for_path` must use the workspace's
/// registered `name` field from `jj workspace list`, not a re-derivation
/// from the path basename. Regression for the `feat/x` → ws name
/// `feat_x` mismatch.
#[test]
fn workspace_name_for_path_uses_registered_name_not_basename() {
    use crate::vcs::jj::worktree::workspace_name_for_path;
    let list_out = "feat_x\tdeadbeef\tfeat/x\n";
    let root_path = if cfg!(windows) { "C:\\repo\\wt\\feat\\x" } else { "/repo/wt/feat/x" };
    let mock = MockRunner::new()
        .expect_when(
            |cmd| cmd.to_string().starts_with("jj workspace list -T"),
            ok_str(list_out),
        )
        .expect_when(
            |cmd| cmd.to_string().starts_with("jj workspace root --name feat_x"),
            ok_str(format!("{root_path}\n")),
        );

    // The path argument is the same dir jj reported — exercises the equality
    // branch without going through canonicalize (the dir doesn't exist).
    let result = workspace_name_for_path(&mock, std::path::Path::new(root_path)).unwrap();
    assert_eq!(
        result, "feat_x",
        "must use the workspace's registered name, not basename ('x')"
    );
}

/// Review fix #4: jj merge() must return Ok(()) without spawning any
/// `jj new` when `branch` has no commits the working copy lacks. Prevents
/// degenerate merge commits during `ws sync --strategy=merge` on an
/// up-to-date worktree.
#[test]
fn jj_merge_noop_when_branch_already_in_ancestors() {
    use procpilot::testing::ok_str;
    // The pre-flight `commit_count_via_revset("(branch) ~ ancestors(@)")`
    // emits an empty `jj log` — count is 0, merge() returns Ok(()) early.
    let mock = MockRunner::new().expect_when(
        |cmd| {
            let s = cmd.to_string();
            s.starts_with("jj log -r")
                && s.contains("(branch) ~ ancestors(@)")
        },
        ok_str(""),
    );
    let backend = JjBackend::with_runner(Arc::new(mock));
    backend.merge("branch", "main", false, false, None).unwrap();
    // If the no-op short-circuit failed, MockRunner's strict-by-default
    // mode would panic on the unexpected `jj new` invocation.
}

/// Review round-2 fix #3: `detect_trunk` must return a deterministic
/// answer when `trunk()` resolves to a commit with multiple local
/// bookmarks. Regression for non-deterministic ordering across jj
/// versions / hash-map iteration shuffles.
#[test]
fn detect_trunk_prefers_main_over_master_when_both_on_trunk_commit() {
    // jj emits the bookmarks in some order; we pin selection to "main"
    // first regardless. Putting master first in the mock output stresses
    // the priority logic.
    let mock = MockRunner::new().expect_when(
        |cmd| {
            let s = cmd.to_string();
            // Cmd::display single-quotes `trunk()` because of the parens.
            s.starts_with("jj log -r 'trunk()'")
        },
        ok_str("master\nmain\n"),
    );
    let backend = JjBackend::with_runner(Arc::new(mock));
    assert_eq!(backend.detect_trunk().unwrap(), "main");
}

/// Same regression with reverse ordering — selection must NOT depend on
/// the order jj emits names.
#[test]
fn detect_trunk_prefers_main_regardless_of_emission_order() {
    let mock = MockRunner::new().expect_when(
        |cmd| cmd.to_string().starts_with("jj log -r 'trunk()'"),
        ok_str("main\nmaster\n"),
    );
    let backend = JjBackend::with_runner(Arc::new(mock));
    assert_eq!(backend.detect_trunk().unwrap(), "main");
}

/// R3-Fix #3: workspace_name_for must escape newlines so embedded \\n in
/// a branch name doesn't break the WORKSPACE_TEMPLATE parsing (one row
/// per line, tab-separated).
#[test]
fn workspace_name_for_replaces_newline_with_underscore() {
    use crate::vcs::jj::worktree::workspace_name_for;
    assert_eq!(workspace_name_for("feat\nx"), "feat_x");
    assert_eq!(workspace_name_for("a\rb\nc"), "a_b_c");
    assert_eq!(workspace_name_for("normal-name"), "normal-name");
}

/// When neither main nor master is attached but other bookmarks are, fall
/// back to lex-smallest (alphabetical) — still deterministic.
#[test]
fn detect_trunk_lex_smallest_when_no_well_known_name() {
    let mock = MockRunner::new().expect_when(
        |cmd| cmd.to_string().starts_with("jj log -r 'trunk()'"),
        ok_str("zeta\nalpha\nbeta\n"),
    );
    let backend = JjBackend::with_runner(Arc::new(mock));
    assert_eq!(backend.detect_trunk().unwrap(), "alpha");
}

/// R4 review: stress detect_trunk's lex-smallest fallback with a larger,
/// non-trivially-shuffled set. Three names could pass with a no-op sort
/// by chance — exercise more to pin the actual sort.
#[test]
fn detect_trunk_lex_smallest_stable_with_many_shuffled_names() {
    // 10 names in reverse-alphabetical order. Any working sort returns "a-feat".
    // A no-op (or fixed-order picker) would return "z-feat" or similar.
    let mock = MockRunner::new().expect_when(
        |cmd| cmd.to_string().starts_with("jj log -r 'trunk()'"),
        ok_str("z-feat\ny-feat\nx-feat\nw-feat\nv-feat\nu-feat\nt-feat\ns-feat\nr-feat\na-feat\n"),
    );
    let backend = JjBackend::with_runner(Arc::new(mock));
    assert_eq!(backend.detect_trunk().unwrap(), "a-feat");
}

/// R4 review: full slash-branch round-trip — the motivating scenario for
/// R1-Fix #3. Branch `feat/x` → ws name `feat_x` (slash → underscore via
/// `workspace_name_for`) → workspace path under `<wt_dir>/feat/x`. The
/// path→name lookup must return `feat_x`, not `x` (basename).
#[test]
fn workspace_name_for_path_slash_branch_full_round_trip() {
    use crate::vcs::jj::worktree::{workspace_name_for, workspace_name_for_path};

    // Step 1: derive ws name from branch. Confirms the contract.
    assert_eq!(workspace_name_for("feat/x"), "feat_x");

    // Step 2: simulate `jj workspace list` showing this ws at the slash
    // path. Path comparison should match by the registered name even
    // though the path's basename ("x") would yield the wrong derivation.
    let list_out = "feat_x\tdeadbeef\tfeat/x\n";
    let path_str = if cfg!(windows) {
        "C:\\repo\\wt\\feat\\x"
    } else {
        "/repo/wt/feat/x"
    };
    let mock = MockRunner::new()
        .expect_when(
            |cmd| cmd.to_string().starts_with("jj workspace list -T"),
            ok_str(list_out),
        )
        .expect_when(
            |cmd| cmd.to_string().starts_with("jj workspace root --name feat_x"),
            ok_str(format!("{path_str}\n")),
        );

    let result = workspace_name_for_path(&mock, std::path::Path::new(path_str)).unwrap();
    assert_eq!(
        result, "feat_x",
        "slash-branch round-trip must return 'feat_x' (registered name), not 'x' (path basename)"
    );
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
