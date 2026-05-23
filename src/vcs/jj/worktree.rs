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
///
/// **Newline handling**: `\n`/`\r` would corrupt the tab/newline-delimited
/// `WORKSPACE_TEMPLATE` output parsed by `list_workspace_rows`. Branch
/// names with embedded newlines are unusual but possible (some git
/// imports allow them); collapsing them to `_` keeps the round-trip
/// consistent and prevents silent row drops in workspace lookups.
pub(super) fn workspace_name_for(branch: &str) -> String {
    branch
        .chars()
        .map(|c| match c {
            '/' | '\\' | '.' | ':' | ' ' | '\t' | '\n' | '\r' => '_',
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

/// One row of `jj workspace list -T WORKSPACE_TEMPLATE` plus its resolved
/// filesystem path. Private to this module — the public `WorktreeInfo` type
/// doesn't carry the workspace name, but we need it for `forget`/`remove`
/// lookups, so we keep it alongside in this internal struct.
pub(super) struct WorkspaceRow {
    pub(super) name: String,
    pub(super) path: PathBuf,
    pub(super) commit: Option<String>,
    pub(super) bookmarks: Vec<String>,
}

/// List all workspaces with their internal `name` field (which the public
/// `WorktreeInfo` drops). Used internally for path→name lookups when
/// removing workspaces by path.
pub(super) fn list_workspace_rows(runner: &dyn Runner) -> Result<Vec<WorkspaceRow>> {
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

    let mut rows = Vec::new();
    for line in out.stdout_lossy().lines() {
        if line.trim().is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.splitn(3, '\t').collect();
        if parts.len() != 3 {
            continue;
        }
        let name = parts[0].to_string();
        let commit = parts[1].to_string();
        let bookmarks: Vec<String> = parts[2]
            .split(',')
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.to_string())
            .collect();

        // Resolve path via per-workspace call. Failures skip the row —
        // there's no useful WorkspaceRow without a path.
        let path = match runner.run(Cmd::new("jj").in_dir(&cwd).args([
            "workspace", "root", "--name", &name,
        ])) {
            Ok(p_out) => PathBuf::from(p_out.stdout_lossy().trim()),
            Err(_) => continue,
        };

        rows.push(WorkspaceRow {
            name,
            path,
            commit: if commit.is_empty() { None } else { Some(commit) },
            bookmarks,
        });
    }
    Ok(rows)
}

/// Normalize a path for comparison against jj's `workspace root` output.
///
/// Two normalizations:
///   - `canonicalize` to resolve symlinks and case differences on macOS.
///   - Strip the Windows verbatim prefix (`\\?\C:\…`). `canonicalize` on
///     Windows always returns verbatim paths; jj returns plain paths. Bare
///     equality would always fail without this strip.
fn normalize_for_compare(p: &Path) -> PathBuf {
    let canonical = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    #[cfg(windows)]
    {
        let s = canonical.to_string_lossy();
        if let Some(rest) = s.strip_prefix(r"\\?\")
            && !rest.starts_with("UNC\\")
        {
            return PathBuf::from(rest.to_string());
        }
    }
    canonical
}

/// Look up the workspace name whose root matches `path`. Returns
/// `Error::WorktreeNotFound` if no workspace has that root.
///
/// **Uses the workspace `name` field directly** from `jj workspace list`
/// — not a re-derived guess from `path.file_name()`. Critical when:
///   - branch contains `/` (e.g. `feat/x` → ws name `feat_x`, path
///     basename `x`); the re-derived guess would forget the wrong ws.
///   - the @ bookmark was deleted (the `branch` field becomes None, but
///     the registered ws name is still recoverable from list output).
pub(super) fn workspace_name_for_path(runner: &dyn Runner, path: &Path) -> Result<String> {
    let target = normalize_for_compare(path);
    for row in list_workspace_rows(runner)? {
        let row_normalized = normalize_for_compare(&row.path);
        if row_normalized == target || row.path == target || row.path == path {
            return Ok(row.name);
        }
    }
    Err(Error::WorktreeNotFound(path.display().to_string()))
}

/// List all attached workspaces.
///
/// Wraps the internal [`list_workspace_rows`] (which also tracks the
/// workspace name for path→name lookups) and projects each row into the
/// backend-agnostic `WorktreeInfo` shape. The workspace name itself is
/// dropped here — only `remove_worktree` needs it, and it goes through
/// the internal helper directly.
pub(super) fn list_worktrees(runner: &dyn Runner) -> Result<Vec<WorktreeInfo>> {
    Ok(list_workspace_rows(runner)?
        .into_iter()
        .map(|row| WorktreeInfo {
            path: row.path,
            // Match git's behaviour: pick the first attached bookmark.
            // Multiple bookmarks on @ are possible but rare; ordering is
            // whatever jj's template emits — stable per repo per op.
            branch: row.bookmarks.into_iter().next(),
            commit: row.commit,
            is_bare: false,
        })
        .collect())
}
