// ===========================================================================
// complete - Dynamic Completion Candidates
//
// Completers are fail-silent: errors return empty list.
// They run at tab time via CompleteEnv, so the CLI calls reflect real repo
// state. Branch/worktree completers **union git refs with jj bookmarks** so
// the right names appear regardless of backend: a pure-git repo yields git
// branches, a pure-jj repo yields jj bookmarks, and a colocated repo yields
// both (extra candidates are harmless suggestions — never a wrong action).
// ===========================================================================

use std::collections::BTreeSet;
use std::ffi::OsStr;

use clap_complete::engine::CompletionCandidate;

/// Run `<bin> <args>` synchronously in the process cwd, returning UTF-8 stdout
/// on success.
///
/// Completers must be **synchronous** (clap invokes them at tab time, and the
/// async VCS layer can't be `block_on`'d from inside the tokio runtime that
/// `main` runs on), so a plain `std::process` call is the right tool here.
/// Fail-silent: a missing binary or non-zero exit yields `None`.
fn completion_output(bin: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(bin).args(args).output().ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        None
    }
}

/// Local jj bookmark names (empty when not a jj repo / jj absent). The template
/// prints one bookmark name per line; `--all-remotes` is intentionally omitted
/// so remote-tracking bookmarks don't clutter local-branch completion.
fn jj_bookmark_names() -> Vec<String> {
    let Some(out) = completion_output("jj", &["bookmark", "list", "-T", "name ++ \"\\n\""]) else {
        return vec![];
    };
    out.lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// De-dup, prefix-filter, and wrap names into completion candidates.
fn candidates<I: IntoIterator<Item = String>>(names: I, prefix: &str) -> Vec<CompletionCandidate> {
    names
        .into_iter()
        .filter(|b| b.starts_with(prefix))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(CompletionCandidate::new)
        .collect()
}

/// Complete worktree branch names (for cd/rm/mv).
///
/// Union of git's linked worktrees (the main worktree is skipped — it's not a
/// valid cd/rm/mv target) and jj bookmarks (which name `ws`-created workspaces
/// in a pure-jj repo, where there are no git worktrees to list).
pub fn complete_worktrees(current: &OsStr) -> Vec<CompletionCandidate> {
    let Some(prefix) = current.to_str() else {
        return vec![];
    };

    let mut names: Vec<String> = Vec::new();
    if let Some(out) = completion_output("git", &["worktree", "list", "--porcelain"]) {
        names.extend(
            crate::vcs::parse_worktree_list(&out)
                .iter()
                .skip(1)
                .filter_map(|wt| wt.branch.as_deref())
                .map(str::to_string),
        );
    }
    // The git path skips index 0 (the main worktree) — not a valid cd/rm/mv
    // target. jj bookmarks have no such ordering, so the trunk bookmark would
    // otherwise leak in as a candidate (the main repo isn't under the workspace
    // dir, so `ws cd main` would just error). Drop the well-known trunk names
    // to preserve the git-only behaviour. (We can't resolve a custom trunk name
    // here — completers are synchronous and can't call the async resolver — but
    // these cover the overwhelming majority; a stray candidate is harmless.)
    names.extend(
        jj_bookmark_names()
            .into_iter()
            .filter(|n| !matches!(n.as_str(), "main" | "master" | "trunk")),
    );

    candidates(names, prefix)
}

/// Complete local branch names (for --base/--into/--from/--trunk): union of
/// git local branches and jj bookmarks.
pub fn complete_branches(current: &OsStr) -> Vec<CompletionCandidate> {
    let Some(prefix) = current.to_str() else {
        return vec![];
    };

    let mut names: Vec<String> = Vec::new();
    if let Some(out) =
        completion_output("git", &["for-each-ref", "--format=%(refname:short)", "refs/heads/"])
    {
        names.extend(out.lines().filter(|b| !b.is_empty()).map(str::to_string));
    }
    names.extend(jj_bookmark_names());

    candidates(names, prefix)
}

/// Complete merge strategy values (`squash` / `merge`).
pub fn complete_merge_strategies(current: &OsStr) -> Vec<CompletionCandidate> {
    static_values(current, &["squash", "merge"])
}

/// Complete sync strategy values (`rebase` / `merge`).
pub fn complete_sync_strategies(current: &OsStr) -> Vec<CompletionCandidate> {
    static_values(current, &["rebase", "merge"])
}

/// Complete the allow-listed `ws config` dotted keys.
pub fn complete_config_keys(current: &OsStr) -> Vec<CompletionCandidate> {
    static_values(current, &["workspace.alias", "workspace.use_path_hash"])
}

/// Prefix-filter a fixed value set into candidates.
fn static_values(current: &OsStr, values: &[&str]) -> Vec<CompletionCandidate> {
    let Some(prefix) = current.to_str() else {
        return vec![];
    };
    values
        .iter()
        .filter(|v| v.starts_with(prefix))
        .map(|v| CompletionCandidate::new(*v))
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

    #[test]
    fn merge_strategies_complete_and_prefix_filter() {
        let all = complete_merge_strategies(OsStr::new(""));
        assert_eq!(all.len(), 2, "squash + merge");
        // Prefix narrows to a single match.
        let sq = complete_merge_strategies(OsStr::new("sq"));
        assert_eq!(sq.len(), 1);
    }

    #[test]
    fn sync_strategies_complete() {
        assert_eq!(complete_sync_strategies(OsStr::new("")).len(), 2);
        assert_eq!(complete_sync_strategies(OsStr::new("re")).len(), 1);
    }

    #[test]
    fn config_keys_complete_and_prefix_filter() {
        assert_eq!(complete_config_keys(OsStr::new("")).len(), 2);
        // Both known keys share the `workspace.` prefix.
        assert_eq!(complete_config_keys(OsStr::new("workspace.")).len(), 2);
        assert_eq!(complete_config_keys(OsStr::new("workspace.a")).len(), 1);
        assert_eq!(complete_config_keys(OsStr::new("nope")).len(), 0);
    }
}
