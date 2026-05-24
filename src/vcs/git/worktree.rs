// ===========================================================================
// vcs/git/worktree - Worktree CRUD + porcelain parser
// ===========================================================================

use std::path::{Path, PathBuf};

use vcs_runner::{Cmd, RunError, Runner};

use super::errmap::map_run_err;
use crate::vcs::common::{path_str, CreateOutcome, WorktreeInfo};
use crate::vcs::error::{Error, Result};

/// Create a new git worktree.
///
/// Dispatches between two strategies:
///
///   - **Plain** (default fallback): `git worktree add` with normal
///     checkout. Single-step, slow on large repos (full file I/O).
///   - **CoW** (when filesystem supports block cloning and same-volume):
///     stash → checkout base → `worktree add --no-checkout` → reflink-copy
///     repo contents → restore source repo. The reflink step is near-
///     instant on ReFS / Btrfs / XFS / APFS.
///
/// CoW eligibility is decided by [`crate::cow::can_clone`] (which does a
/// real sentinel reflink at the destination's parent). When CoW isn't
/// possible we silently fall back to plain — no warnings, no errors.
pub(super) fn create_worktree(
    runner: &dyn Runner,
    path: &Path,
    branch: &str,
    base: &str,
) -> Result<CreateOutcome> {
    // Pre-flight `WorktreeExists` check applies to BOTH paths.
    let branch_already_exists = super::repo::branch_exists(runner, branch)?;
    if branch_already_exists {
        let worktrees = list_worktrees(runner)?;
        if worktrees.iter().any(|wt| wt.branch.as_deref() == Some(branch)) {
            return Err(Error::WorktreeExists(branch.to_string()));
        }
    }

    // CoW probe — requires both the repo root and `path`'s parent to be on
    // the same reflink-capable volume. The parent dir must already exist
    // (caller in `new.rs` calls `create_dir_all` on `workspace_dir` before
    // dispatching here).
    let parent = path.parent().unwrap_or(path);
    if std::env::var(crate::cow::DISABLE_COW_ENV).is_err()
        && let Ok(repo_root) = super::repo::repo_root(runner)
        && parent.exists()
        && crate::cow::can_clone(&repo_root, parent)
    {
        return create_worktree_cow(runner, &repo_root, path, branch, base, branch_already_exists);
    }

    create_worktree_plain(runner, path, branch, base, branch_already_exists)
}

/// Standard `git worktree add` — git materialises the working copy.
fn create_worktree_plain(
    runner: &dyn Runner,
    path: &Path,
    branch: &str,
    base: &str,
    branch_already_exists: bool,
) -> Result<CreateOutcome> {
    let path_arg = path_str(path)?;
    if branch_already_exists {
        super::exec(runner, &["worktree", "add", path_arg, branch])?;
    } else {
        super::exec(runner, &["worktree", "add", "-b", branch, path_arg, base])?;
    }
    Ok(CreateOutcome::Plain)
}

/// CoW creation: stash → checkout base → `worktree add --no-checkout`
/// → reflink-copy → restore. See module docstring for the rationale.
///
/// On any failure between stash and pop the source repo is restored to its
/// original state before the error propagates. The half-created worktree
/// (if any) is deleted and `git worktree prune` clears git's registry.
fn create_worktree_cow(
    runner: &dyn Runner,
    repo_root: &Path,
    path: &Path,
    branch: &str,
    base: &str,
    branch_already_exists: bool,
) -> Result<CreateOutcome> {
    let path_arg = path_str(path)?;

    // 0. Colocated detection. When the repo has `.jj/` alongside `.git/`,
    //    the raw git operations below (stash, checkout, worktree add)
    //    mutate git's HEAD/index without going through jj — desyncing
    //    jj's view of the repo. Bracket the whole CoW flow with `jj git
    //    import` so jj's bookmarks/refs catch up before and after.
    //
    // The import calls are best-effort: jj may not be installed locally
    // (a colocated repo can travel between machines), in which case the
    // calls silently fail and the user's jj-side state may drift —
    // already broken if they ran any raw git command without jj sync,
    // so no regression.
    let is_colocated = repo_root.join(".jj").is_dir();
    if is_colocated {
        match std::process::Command::new("jj")
            .current_dir(repo_root)
            .args(["git", "import"])
            .status()
        {
            Ok(s) if !s.success() => {
                eprintln!(
                    "Warning: pre-CoW `jj git import` exited {} — jj-side refs may \
                     be stale after this operation; run `jj git import` manually if needed",
                    s.code().unwrap_or(-1)
                );
            }
            Err(e) => {
                eprintln!(
                    "Warning: pre-CoW `jj git import` failed to spawn: {e} \
                     (is jj installed?); colocated jj state may drift from git"
                );
            }
            Ok(_) => {} // success; no output
        }
    }

    // 1. Capture source state. Both the branch name AND the commit hash:
    // when the branch name is "HEAD" the repo is in detached state, and
    // `git checkout HEAD` is a no-op (does not restore the original
    // commit after we move HEAD to `base`). Restore via the captured
    // commit hash in that case.
    let orig_branch = super::repo::current_branch(runner)?;
    let orig_commit = super::repo::current_commit(runner)?;
    let is_detached = orig_branch == "HEAD";
    let needs_stash = super::branch::has_uncommitted_changes(runner)?;

    // 2. Stash if dirty. `-u` includes untracked.
    //
    // The stash message includes both PID and a nanosecond timestamp so
    // multiple failed runs in the same shell session leave distinguishable
    // entries in `git stash list` — without the timestamp, a user who
    // hits two consecutive CoW failures would see two stashes with
    // identical names and have to drop them by index alone.
    let stash_message = format!(
        "wt-cow-create-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    if needs_stash {
        super::exec(runner, &["stash", "push", "-u", "-m", &stash_message])?;
    }

    // The inner closure handles steps 3-5 with explicit rollback on any
    // error. We always restore the source repo (step 6) afterwards, even
    // on success — current_branch may have differed from base.
    let inner: Result<()> = (|| {
        // 3. Checkout base if not already there.
        if orig_branch != base {
            super::exec(runner, &["checkout", base])?;
        }

        // 4. Create worktree with --no-checkout. git creates the `.git`
        // gitlink file at `path` and registers the worktree, but doesn't
        // write any working-tree files.
        let add_result = if branch_already_exists {
            super::exec(runner, &["worktree", "add", "--no-checkout", path_arg, branch])
        } else {
            super::exec(
                runner,
                &["worktree", "add", "--no-checkout", "-b", branch, path_arg, base],
            )
        };
        add_result?;

        // 5. Reflink-copy every file/dir from repo root to `path`, except
        // `.git/` (which `--no-checkout` already created as a gitlink).
        if let Err(e) = crate::cow::try_clone_dir_except(repo_root, path, &[".git"]) {
            // CoW failed mid-walk. Remove the half-populated worktree dir
            // and run `git worktree prune` so git's registry stays clean.
            // Preserve the structured `cow::Error` via `Error::Cow` so
            // callers/tests can match the underlying cause.
            let _ = std::fs::remove_dir_all(path);
            let _ = super::exec(runner, &["worktree", "prune"]);
            return Err(Error::Cow(e));
        }

        Ok(())
    })();

    // 6. Restore source repo. Order matters: checkout BEFORE stash pop so
    // pop applies to the right branch.
    //
    // Detached HEAD: `git checkout HEAD` would be a no-op (leaving us on
    // `base`). Use the captured commit hash to restore the actual
    // detached state. Skip when we're already on the right commit
    // (orig_branch == base case is already filtered out below).
    let restore_target = if is_detached { &orig_commit } else { &orig_branch };
    if (is_detached || orig_branch != base)
        && let Err(e) = super::exec(runner, &["checkout", restore_target])
    {
        eprintln!("Warning: failed to restore '{restore_target}': {e}");
    }
    if needs_stash
        && let Err(e) = super::exec(runner, &["stash", "pop"]) {
            // Stash pop conflict is the worst-case: user's changes are
            // safe in `git stash list` but require manual resolution.
            eprintln!(
                "Warning: 'git stash pop' failed: {e}\n\
                 Your changes are saved in 'git stash list'; resolve manually."
            );
        }

    // 7. Colocated post-sync: tell jj about the new worktree's ref
    //    movement and the source repo's branch/stash state restoration.
    if is_colocated {
        match std::process::Command::new("jj")
            .current_dir(repo_root)
            .args(["git", "import"])
            .status()
        {
            Ok(s) if !s.success() => {
                eprintln!(
                    "Warning: post-CoW `jj git import` exited {} — jj's bookmarks \
                     may not reflect the new worktree's HEAD; run `jj git import` manually",
                    s.code().unwrap_or(-1)
                );
            }
            Err(e) => {
                eprintln!(
                    "Warning: post-CoW `jj git import` failed to spawn: {e} \
                     (is jj installed?); colocated jj state may drift from git"
                );
            }
            Ok(_) => {}
        }
    }

    inner?;
    Ok(CreateOutcome::CowCloned)
}

/// Remove a worktree.
pub(super) fn remove_worktree(runner: &dyn Runner, path: &Path, force: bool) -> Result<()> {
    let path_arg = path_str(path)?;
    if force {
        super::exec(runner, &["worktree", "remove", "--force", path_arg])
    } else {
        super::exec(runner, &["worktree", "remove", path_arg])
    }
}

/// Move a worktree to a new path.
pub(super) fn move_worktree(runner: &dyn Runner, old_path: &Path, new_path: &Path) -> Result<()> {
    super::exec(
        runner,
        &["worktree", "move", path_str(old_path)?, path_str(new_path)?],
    )
}

/// List all worktrees attached to the repo.
pub(super) fn list_worktrees(runner: &dyn Runner) -> Result<Vec<WorktreeInfo>> {
    let cwd = std::env::current_dir()?;
    let out = runner
        .run(Cmd::new("git").in_dir(&cwd).args(["worktree", "list", "--porcelain"]))
        .map_err(|e| match e {
            RunError::NonZeroExit { .. } => Error::NotInRepo,
            other => map_run_err(other),
        })?;
    Ok(parse_worktree_list(&out.stdout_lossy()))
}

/// Parse `git worktree list --porcelain` output.
///
/// Kept `pub(crate)` so tests can hit it directly with literal fixtures.
///
/// Git's porcelain schema:
/// ```text
/// worktree /path/to/main
/// HEAD <sha>
/// branch refs/heads/main
///
/// worktree /path/to/feature
/// HEAD <sha>
/// branch refs/heads/feature
///
/// worktree /path/to/detached
/// HEAD <sha>
/// detached
/// ```
///
/// **This parser is git-specific** — jj's `jj workspace list` uses an
/// entirely different schema. `JjBackend` will need its own normalizer
/// that emits `WorktreeInfo` from `jj workspace list -T <template>`.
pub fn parse_worktree_list(content: &str) -> Vec<WorktreeInfo> {
    let mut worktrees = Vec::new();
    let mut current: Option<WorktreeInfo> = None;

    for line in content.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            if let Some(wt) = current.take() {
                worktrees.push(wt);
            }
            current = Some(WorktreeInfo {
                path: PathBuf::from(path),
                branch: None,
                commit: None,
                is_bare: false,
            });
        } else if let Some(ref mut wt) = current {
            if let Some(branch) = line.strip_prefix("branch refs/heads/") {
                wt.branch = Some(branch.to_string());
            } else if let Some(commit) = line.strip_prefix("HEAD ") {
                wt.commit = Some(commit.to_string());
            } else if line == "bare" {
                wt.is_bare = true;
            }
        }
    }

    if let Some(wt) = current {
        worktrees.push(wt);
    }

    worktrees
}
