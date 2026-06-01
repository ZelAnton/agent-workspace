// ===========================================================================
// complete - Dynamic Completion Candidates
//
// Completers are fail-silent: errors return empty list.
// They run at tab time via CompleteEnv, so git calls reflect real repo state.
// ===========================================================================

use std::ffi::OsStr;

use clap_complete::engine::CompletionCandidate;

/// Run `git <args>` synchronously in the process cwd, returning UTF-8 stdout on
/// success.
///
/// Completers must be **synchronous** (clap invokes them at tab time, and the
/// async VCS layer can't be `block_on`'d from inside the tokio runtime that
/// `main` runs on). They're also git-only and best-effort by design — matching
/// the historical behaviour where the completion path always used the git
/// backend — so a plain `std::process` git call is the right tool here.
fn git_completion_output(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git").args(args).output().ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        None
    }
}

/// Complete worktree branch names (for cd/rm/mv)
pub fn complete_worktrees(current: &OsStr) -> Vec<CompletionCandidate> {
    let Some(prefix) = current.to_str() else {
        return vec![];
    };

    let Some(out) = git_completion_output(&["worktree", "list", "--porcelain"]) else {
        return vec![];
    };
    let worktrees = crate::vcs::parse_worktree_list(&out);

    // Main worktree is not a valid cd/rm/mv target
    worktrees
        .iter()
        .skip(1)
        .filter_map(|wt| wt.branch.as_deref())
        .filter(|b| b.starts_with(prefix))
        .map(CompletionCandidate::new)
        .collect()
}

/// Complete local git branch names (for --base/--into/--from/--trunk)
pub fn complete_branches(current: &OsStr) -> Vec<CompletionCandidate> {
    let Some(prefix) = current.to_str() else {
        return vec![];
    };

    let Some(out) =
        git_completion_output(&["for-each-ref", "--format=%(refname:short)", "refs/heads/"])
    else {
        return vec![];
    };

    out.lines()
        .filter(|b| !b.is_empty())
        .filter(|b| b.starts_with(prefix))
        .map(CompletionCandidate::new)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_worktrees_does_not_panic() {
        // Should not panic regardless of CWD (inside or outside repo)
        let _ = complete_worktrees(OsStr::new(""));
    }

    #[test]
    fn complete_branches_does_not_panic() {
        let _ = complete_branches(OsStr::new(""));
    }

    #[test]
    fn complete_worktrees_handles_invalid_utf8() {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let invalid = OsStr::from_bytes(&[0xff, 0xfe]);
            let result = complete_worktrees(invalid);
            assert!(result.is_empty());
        }
    }

    #[test]
    fn complete_branches_handles_invalid_utf8() {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let invalid = OsStr::from_bytes(&[0xff, 0xfe]);
            let result = complete_branches(invalid);
            assert!(result.is_empty());
        }
    }
}
