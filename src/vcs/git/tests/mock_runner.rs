// ===========================================================================
// vcs/git/tests/mock_runner - Demonstrate Runner-trait test injection
// ===========================================================================
//
// Existing GitBackend tests run against the real `git` binary via
// `setup_test_repo` + `with_cwd` — that's the project's primary testing
// philosophy ("validate against the real tool"). The MockRunner pattern
// here is opt-in for cases where:
//   - The logic under test is mostly argument-construction + output-parsing
//     and doesn't need real-git side effects to validate.
//   - You want to assert the exact `git` invocation a method emits.
//   - You want to test specific stderr / exit-code shapes without faking
//     them via real git commands.
//
// Don't move existing real-git tests onto MockRunner unprompted — keep them
// end-to-end. Add MockRunner tests for new parser-heavy code only.

use std::sync::Arc;

use procpilot::testing::{nonzero, ok_str, MockRunner};
use procpilot::{Cmd, RunError};

use super::super::GitBackend;
use crate::vcs::backend::VcsBackend;
use crate::vcs::error::Error;

#[test]
fn current_branch_parses_mock_stdout() {
    // The CWD_MUTEX in this thread is NOT needed for MockRunner-based tests
    // — they don't shell out to a real git, so std::env::current_dir() is
    // only read to populate Cmd::in_dir (the value is otherwise unused).
    let mock = MockRunner::new().expect("git rev-parse --abbrev-ref HEAD", ok_str("main\n"));
    let backend = GitBackend::with_runner(Arc::new(mock));
    assert_eq!(backend.current_branch().unwrap(), "main");
}

#[test]
fn current_branch_maps_nonzero_to_not_in_repo() {
    let mock = MockRunner::new().expect(
        "git rev-parse --abbrev-ref HEAD",
        nonzero(128, "fatal: not a git repository (or any of the parent directories): .git"),
    );
    let backend = GitBackend::with_runner(Arc::new(mock));
    assert!(matches!(backend.current_branch(), Err(Error::NotInRepo)));
}

#[test]
fn merge_emits_squash_args_when_squash_true() {
    // Asserts the exact arg vector merge() builds — proves the trait method
    // doesn't quietly drop or reorder flags. The mock matches on the
    // display string (program + args joined as shell-style).
    // Cmd's display uses single quotes for whitespace-containing args.
    let mock = MockRunner::new().expect(
        "git merge --squash --no-ff -m 'my msg' feature",
        ok_str(""),
    );
    let backend = GitBackend::with_runner(Arc::new(mock));
    backend.merge("feature", true, true, Some("my msg")).unwrap();
}

#[test]
fn fetch_silently_swallows_nonzero_exit() {
    // fetch() intentionally ignores non-zero exit so a flaky remote
    // doesn't break downstream commands. Verify that property holds.
    let mock = MockRunner::new().expect("git fetch --quiet", nonzero(128, "fatal: unable to access"));
    let backend = GitBackend::with_runner(Arc::new(mock));
    assert!(backend.fetch().is_ok());
}

#[test]
fn has_staged_changes_treats_exit_1_as_true() {
    // `git diff --cached --quiet` uses exit 1 as the "has diff" signal,
    // not an error. Confirm GitBackend translates that correctly.
    let mock = MockRunner::new().expect("git diff --cached --quiet", nonzero(1, ""));
    let backend = GitBackend::with_runner(Arc::new(mock));
    assert!(backend.has_staged_changes().unwrap());
}

// ---------------------------------------------------------------------------
// is_transient_fetch_err predicate — pure-function tests, no MockRunner
// ---------------------------------------------------------------------------

fn nonzero_with_stderr(stderr: &str) -> RunError {
    // Build a NonZeroExit shape matching what vcs-runner emits, with the
    // status code we use throughout: platform-specific construction.
    #[cfg(unix)]
    let status = {
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(256) // exit code 1
    };
    #[cfg(windows)]
    let status = {
        use std::os::windows::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(1)
    };
    RunError::NonZeroExit {
        command: Cmd::new("git").display(),
        status,
        stdout: Vec::new(),
        stderr: stderr.to_string(),
        attempts: 1,
    }
}

#[test]
fn is_transient_fetch_err_matches_dns_failure() {
    let err = nonzero_with_stderr("fatal: unable to access 'https://github.com/x/y.git/': Could not resolve host: github.com");
    assert!(super::super::ops::is_transient_fetch_err(&err));
}

#[test]
fn is_transient_fetch_err_matches_connection_refused() {
    let err = nonzero_with_stderr("fatal: unable to access: Failed to connect: Connection refused");
    assert!(super::super::ops::is_transient_fetch_err(&err));
}

#[test]
fn is_transient_fetch_err_matches_early_eof() {
    let err = nonzero_with_stderr("error: RPC failed; curl 18 transfer closed with outstanding read data remaining\nfatal: early EOF");
    assert!(super::super::ops::is_transient_fetch_err(&err));
}

#[test]
fn is_transient_fetch_err_does_not_match_pathspec_failure() {
    // Pathspec / ref errors are caller bugs, not transient — must NOT retry.
    let err = nonzero_with_stderr("fatal: pathspec 'nonexistent' did not match any files");
    assert!(!super::super::ops::is_transient_fetch_err(&err));
}

#[test]
fn is_transient_fetch_err_does_not_match_spawn_failure() {
    // Spawn errors aren't NonZeroExit, so the predicate must return false.
    let err = RunError::Spawn {
        command: Cmd::new("git").display(),
        source: std::io::Error::other("binary not found"),
    };
    assert!(!super::super::ops::is_transient_fetch_err(&err));
}
