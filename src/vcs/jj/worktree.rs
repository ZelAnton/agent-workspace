// ===========================================================================
// vcs/jj/worktree - Workspace CRUD (jj's analogue of git worktrees)
// ===========================================================================
//
// **jj concept mapping**:
//   - `git worktree` → `jj workspace` (each one has its own working copy)
//   - `git worktree add <path> <branch>` → `jj workspace add --name <derived>
//     -r <base> <path>` + `jj bookmark create <branch> -r <derived>@`
//   - `git worktree list` → `jj workspace list` + `jj workspace root --name`
//     (jj 0.38's `WorkspaceRef` template type exposes `name` and `target`
//     but NOT path — we N+1 query for the path)
//   - `git worktree remove` → `std::fs::remove_dir_all` + `jj workspace forget`
//   - `git worktree move` → not supported (per locked semantic decision)

use std::path::{Path, PathBuf};

use vcs_runner::{Cmd, RunError, Runner};

use super::errmap::map_run_err;
use crate::vcs::common::{path_str, WorktreeInfo};
use crate::vcs::error::{Error, Result};

/// jj workspace-list template: one line per workspace, tab-separated.
///
/// Schema: `name\tcommit_id\tbookmark1,bookmark2,...\n`
///
/// **Why tabs**: jj's `escape_json` works inside JSON strings, but the
/// `WorkspaceRef` type (jj 0.38) doesn't expose enough fields to build a
/// well-formed JSON object — no `working_copy_path` keyword. Tab-separated
/// dodges the issue and is trivial to split.
///
/// The path isn't in the template (jj 0.38 has no `path`/`working_copy_path`
/// accessor on `WorkspaceRef`). [`list_worktrees`] resolves paths by calling
/// `jj workspace root --name <name>` for each row.
const WORKSPACE_TEMPLATE: &str = concat!(
    r#"name"#,
    r#" ++ "\t" ++ target.commit_id().short()"#,
    r#" ++ "\t" ++ target.local_bookmarks().map(|b| b.name()).join(",")"#,
    r#" ++ "\n""#,
);

/// Derive a jj workspace name from a branch name.
///
/// jj's workspace names must be valid identifiers — no `/`, `.`, `\`, or
/// whitespace. Branch names commonly contain `/` (`feat/x`), so we
/// substitute to `_`. The derivation is deterministic so `remove_worktree`
/// can reconstruct the workspace name from the branch name.
pub(super) fn workspace_name_for(branch: &str) -> String {
    branch
        .chars()
        .map(|c| match c {
            '/' | '\\' | '.' | ':' | ' ' | '\t' => '_',
            other => other,
        })
        .collect()
}

/// Create a new workspace + bookmark for the requested branch.
///
/// **Three-step recipe** (the bookmark create is what makes
/// `current_branch()` work inside the new workspace — locked decision #1
/// requires every jj-managed worktree to have a bookmark on `@`):
///
///   1. Pre-check: `branch_exists(branch)` → `Error::WorktreeExists` (matches
///      the git backend's behaviour even though git/jj have different
///      reasons to reject — for the user, "branch already in use" is one
///      mental model).
///   2. `jj workspace add --name <derived> -r <base> <path>` — creates the
///      workspace with `@` = empty change above `<base>`.
///   3. `jj bookmark create <branch> -r <derived>@` (run in main repo's
///      cwd) — attaches `<branch>` to the new workspace's `@` so the
///      `current_branch()` query inside the workspace returns it.
pub(super) fn create_worktree(
    runner: &dyn Runner,
    path: &Path,
    branch: &str,
    base: &str,
) -> Result<()> {
    if super::repo::branch_exists(runner, branch)? {
        return Err(Error::WorktreeExists(branch.to_string()));
    }

    let ws_name = workspace_name_for(branch);
    let path_arg = path_str(path)?;

    // Step 1: add the workspace.
    super::exec(
        runner,
        &["workspace", "add", "--name", &ws_name, "-r", base, path_arg],
    )?;

    // Step 2: attach the bookmark to the new workspace's @. Use the
    // `<workspace-name>@` revset which always resolves to that workspace's
    // working-copy commit regardless of where we're running from.
    let revset = format!("{ws_name}@");
    super::exec(runner, &["bookmark", "create", branch, "-r", &revset])
}

/// Remove a workspace + delete the on-disk directory.
///
/// **Order matters**: remove the filesystem directory first, then call
/// `jj workspace forget`. If the swap is reversed and `forget` succeeds
/// but `remove_dir_all` fails, the user is left with an orphan directory
/// that `wt` has lost track of — worse than a still-attached workspace
/// (which they can retry). With our ordering, an `fs` failure leaves the
/// workspace attached but intact, and a `forget` failure (rare once the
/// dir is gone) is benign — jj's `forget` doesn't require the dir to
/// exist anyway.
pub(super) fn remove_worktree(runner: &dyn Runner, path: &Path, _force: bool) -> Result<()> {
    // Resolve workspace name BEFORE deleting the dir (we need the live
    // workspace list to find which name corresponds to this path).
    let ws_name = workspace_name_for_path(runner, path)?;

    // Step 1: filesystem removal first.
    if path.exists() {
        std::fs::remove_dir_all(path)?;
    }

    // Step 2: forget the workspace from jj's registry. Best-effort —
    // jj 0.38's `forget` happily processes already-deleted workspace dirs,
    // but if it fails after the fs removal, we log and move on (the
    // user-visible state is "no worktree at this path", which is what
    // remove_worktree promises).
    let _ = super::exec(runner, &["workspace", "forget", &ws_name]);

    Ok(())
}

/// Look up the workspace name whose root matches `path`. Returns
/// `Error::WorktreeNotFound` if no workspace has that root.
fn workspace_name_for_path(runner: &dyn Runner, path: &Path) -> Result<String> {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    for ws in list_worktrees(runner)? {
        if ws.path == canonical || ws.path == path {
            // `path` field comes from `jj workspace root --name <name>`
            // — that's a canonical path on Windows but a plain absolute
            // path on Unix. Compare both forms to be safe.
            return Ok(workspace_name_for(
                ws.branch.as_deref().unwrap_or_else(|| {
                    // Fallback: if no bookmark is attached, the workspace
                    // name itself is the only handle we have. We can't
                    // recover that from `WorktreeInfo` directly (the type
                    // doesn't carry it) — best-effort use of the dir
                    // basename as the derivation source.
                    path.file_name().and_then(|n| n.to_str()).unwrap_or("default")
                }),
            ));
        }
    }
    Err(Error::WorktreeNotFound(
        path.display().to_string(),
    ))
}

/// List all attached workspaces.
///
/// Implementation note: jj 0.38's `WorkspaceRef` template type exposes
/// `name` and `target` but not the workspace path. We list names via the
/// template, then call `jj workspace root --name <name>` per workspace to
/// fetch the path. N+1 in the worst case, but worktree counts in `wt`
/// usage are small (typically <10) and the per-row call is fast.
pub(super) fn list_worktrees(runner: &dyn Runner) -> Result<Vec<WorktreeInfo>> {
    let cwd = std::env::current_dir()?;
    let out = runner
        .run(
            Cmd::new("jj")
                .in_dir(&cwd)
                .args(["workspace", "list", "-T", WORKSPACE_TEMPLATE]),
        )
        .map_err(|e| match e {
            RunError::NonZeroExit { .. } => Error::NotInRepo,
            other => map_run_err(other),
        })?;

    let mut workspaces = Vec::new();
    for line in out.stdout_lossy().lines() {
        if line.trim().is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.splitn(3, '\t').collect();
        if parts.len() != 3 {
            continue;
        }
        let name = parts[0];
        let commit = parts[1];
        let bookmarks: Vec<&str> = parts[2]
            .split(',')
            .filter(|s| !s.trim().is_empty())
            .collect();

        // Resolve path via a per-workspace call. jj's `workspace root --name`
        // is a one-shot lookup, no template needed.
        let path = match runner.run(Cmd::new("jj").in_dir(&cwd).args([
            "workspace", "root", "--name", name,
        ])) {
            Ok(p_out) => PathBuf::from(p_out.stdout_lossy().trim()),
            Err(_) => continue, // skip workspaces whose root we can't resolve
        };

        workspaces.push(WorktreeInfo {
            path,
            // Match git's behaviour: pick the first attached bookmark.
            // Multiple bookmarks on @ are possible but rare; deterministic
            // ordering is preserved by template (the join order is whatever
            // jj's internal ordering produces — stable per repo).
            branch: bookmarks.first().map(|s| s.to_string()),
            commit: Some(commit.to_string()),
            is_bare: false,
        });
    }
    Ok(workspaces)
}
