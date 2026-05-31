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
use crate::vcs::common::{path_str, CreateOutcome, WorktreeInfo};
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
) -> Result<CreateOutcome> {
    // Does a bookmark with this name already exist? When it does we RESUME
    // it (create the workspace at that bookmark) rather than minting a new
    // one — parity with the git backend. We only refuse when the bookmark
    // is already checked out in another workspace (jj, like git, forbids
    // the same branch being live in two working copies).
    let branch_already_exists = super::repo::branch_exists(runner, branch)?;
    if branch_already_exists {
        let worktrees = list_worktrees(runner)?;
        if worktrees.iter().any(|wt| wt.branch.as_deref() == Some(branch)) {
            return Err(Error::WorktreeExists(branch.to_string()));
        }
    }

    // For a resumed bookmark, the workspace starts AT that bookmark's
    // commit; the caller's `base` is only the recorded merge target and is
    // irrelevant to creation. For a new branch, `base` is the start point.
    let effective_base = if branch_already_exists { branch } else { base };

    // CoW probe — requires both the repo root and `path`'s parent to be
    // on the same reflink-capable volume. Mirrors the git dispatcher's
    // shape exactly so behaviour is consistent across backends.
    let parent = path.parent().unwrap_or(path);
    if std::env::var(crate::cow::DISABLE_COW_ENV).is_err()
        && let Ok(repo_root) = super::repo::repo_root(runner)
        && parent.exists()
        && crate::cow::can_clone(&repo_root, parent)
    {
        return create_worktree_cow(
            runner,
            &repo_root,
            path,
            branch,
            effective_base,
            branch_already_exists,
        );
    }

    create_worktree_plain(runner, path, branch, effective_base, branch_already_exists)
}

/// Create a workspace from a bookmark that exists only on `origin`: fetch
/// just that bookmark, then create the workspace from it.
///
/// After `jj git fetch -b <branch>`, the base depends on jj's
/// `git.auto-local-bookmark` setting: when ON, a local `<branch>` is
/// created and we resume it; when OFF (the modern default) only
/// `<branch>@origin` exists, so we base on that revset and the inner
/// `create_worktree` mints a fresh local `<branch>`. Either way the
/// resulting workspace carries a usable local bookmark.
pub(super) fn create_worktree_from_remote(
    runner: &dyn Runner,
    path: &Path,
    branch: &str,
) -> Result<CreateOutcome> {
    eprintln!("  Fetching '{branch}' from origin...");
    super::ops::fetch_remote_branch(runner, branch)?;
    let base = if super::repo::branch_exists(runner, branch)? {
        branch.to_string()
    } else {
        format!("{branch}@origin")
    };
    create_worktree(runner, path, branch, &base)
}

/// Standard `jj workspace add` — jj materialises the working copy itself.
///
/// When `branch_already_exists`, `base` is the bookmark name itself, so the
/// new workspace's `@` lands as an empty change above that bookmark's commit
/// (the bookmark stays put on `@-`, where `current_branch` still finds it) —
/// we skip minting a new bookmark.
fn create_worktree_plain(
    runner: &dyn Runner,
    path: &Path,
    branch: &str,
    base: &str,
    branch_already_exists: bool,
) -> Result<CreateOutcome> {
    let ws_name = workspace_name_for(branch);
    let path_arg = path_str(path)?;

    // Step 1: add the workspace.
    eprintln!("  Running jj workspace add...");
    super::exec(
        runner,
        &["workspace", "add", "--name", &ws_name, "-r", base, path_arg],
    )?;

    // Step 2: put the bookmark on the new workspace's @. The
    // `<workspace-name>@` revset always resolves to that workspace's
    // working-copy commit regardless of where we're running from.
    //
    // `ws` requires a bookmark on `@` (that's what `current_branch` reads).
    // For a NEW branch we create it; for a RESUMED branch the bookmark
    // already exists one commit below (on `@-`, since `workspace add -r
    // <branch>` puts an empty change on top), so we MOVE it forward onto
    // `@` with `bookmark set` — otherwise `@` would carry no bookmark and
    // `current_branch`/`ws merge`/`ws status` would fail in the workspace.
    let revset = format!("{ws_name}@");
    if branch_already_exists {
        eprintln!("  Moving bookmark to workspace...");
        super::exec(runner, &["bookmark", "set", branch, "-r", &revset])?;
    } else {
        eprintln!("  Creating bookmark...");
        super::exec(runner, &["bookmark", "create", branch, "-r", &revset])?;
    }

    Ok(CreateOutcome::Plain)
}

/// CoW creation: edit source to base → `jj workspace add --sparse-patterns
/// empty` → reflink-copy → restore patterns → bookmark → restore source @.
///
/// jj's `--sparse-patterns empty` is the analogue of git's `--no-checkout`:
/// the new workspace is registered and `@` is set to a fresh empty change
/// above `<base>`, but no files are materialised. We then CoW-copy the
/// source workspace's working-copy contents (which we just moved to base)
/// into the new workspace path, restore the sparse-pattern set to "all
/// files", and snapshot to make jj's `@` tree equal to base's tree
/// (effectively an empty change above base — same observable state as
/// `jj workspace add -r <base>` would produce, just via reflink).
///
/// **Rollback** via `jj op restore <pre_op>` on internal failure plus
/// `fs::remove_dir_all(path)` to clean any half-materialised workspace
/// directory.
fn create_worktree_cow(
    runner: &dyn Runner,
    repo_root: &Path,
    path: &Path,
    branch: &str,
    base: &str,
    branch_already_exists: bool,
) -> Result<CreateOutcome> {
    let ws_name = workspace_name_for(branch);
    let path_arg = path_str(path)?;

    // 1. Capture pre-op for precise op-log rollback.
    let pre_op = super::ops::capture_op_id(runner)?;

    // 2. Capture source @ change-id (NOT commit-id — change-id survives
    //    snapshot rewrites, so a later `jj edit <change_id>` lands on
    //    whatever the same logical change has become).
    let orig_change_id = super::repo::current_change_id(runner)?;

    // 3. Resolve `base` to a concrete commit-id BEFORE moving. `base`
    //    may be a local bookmark, remote ref (`origin/main`), revset, or
    //    commit hash. `jj edit` accepts commit-ids unambiguously; remote
    //    refs and complex revsets work as `-r` to `jj log` here even if
    //    they wouldn't pass to `jj edit` directly. Resolving up-front
    //    also lets us compare against `orig_change_id` correctly (a
    //    name-vs-commit-id compare in the previous version always
    //    differed, so we always moved even when at base).
    //
    // **Ambiguous revsets**: if `base` is a revset matching multiple
    // commits (e.g. `ancestors(main)`), `--limit 1` silently picks the
    // first by jj's evaluation order. Acceptable for the typical case
    // (user passes bookmark name or commit hash); document the edge
    // case rather than over-engineer rejection logic that misfires on
    // legitimate single-commit revsets.
    let base_commit = runner
        .run(
            Cmd::new("jj")
                .in_dir(&std::env::current_dir()?)
                .args(["log", "-r", base, "-T", "commit_id", "--no-graph", "--limit", "1"]),
        )
        .map(|out| out.stdout_lossy().trim().to_string())
        .map_err(map_run_err)?;
    if base_commit.is_empty() {
        return Err(Error::Command(format!(
            "jj: base revision '{base}' resolved to empty commit-id"
        )));
    }

    // 4. Move source workspace to base. Skip if @ already there
    //    (commit-id equality means @ is on base's commit).
    let orig_commit = super::repo::current_commit(runner)?;
    let needs_move = orig_commit != base_commit;
    if needs_move {
        eprintln!("  Switching to base revision...");
        super::exec(runner, &["edit", &base_commit])?;
    }

    // Windows uses CopyFileExW which transparently block-clones on ReFS.
    // Linux/macOS use the explicit reflink IOCTLs (ioctl_ficlone /
    // clonefile). Same outcome, different surfacing.
    #[cfg(windows)]
    eprintln!("  Using ReFS block clone...");
    #[cfg(not(windows))]
    eprintln!("  Using CoW (reflink) clone...");
    let inner: Result<()> = (|| {
        // 5. Create the empty workspace.
        eprintln!("  Creating workspace skeleton...");
        super::exec(
            runner,
            &[
                "workspace",
                "add",
                "--name",
                &ws_name,
                "-r",
                &base_commit,
                "--sparse-patterns",
                "empty",
                path_arg,
            ],
        )?;

        // 6. Reflink-copy source workspace's files (matching base's tree)
        //    into the new workspace. Skip BOTH `.jj/` (jj's metadata that
        //    new workspace already set up) AND `.git/` (colocated repos
        //    have it too; new workspace doesn't need a copy).
        // `try_clone_dir_except` prints its own scan-spinner + progress bar
        // and a "Cloned N files (X GB) via reflink." summary on completion.
        // We don't care about the returned byte count here — jj's
        // post-copy reconciliation (`jj sparse set` + `jj status`) does
        // its own snapshot/refresh that's both small and well-instrumented.
        crate::cow::try_clone_dir_except(repo_root, path, &[".jj", ".git"])
            .map(|_bytes| ())
            .map_err(Error::from)?;

        // 7. Restore sparse-pattern set to "all files" in the new
        //    workspace and trigger a snapshot. After this, jj's view of
        //    `@`'s working copy matches base's tree — an empty change
        //    above base, same as if `jj workspace add -r <base>` had
        //    materialised it directly.
        //
        // `.in_dir(path)` sets the child's cwd; the parent process's cwd
        // is untouched, so no save/restore needed here.
        eprintln!("  Configuring sparse patterns and snapshotting...");
        runner
            .run(
                Cmd::new("jj")
                    .in_dir(path)
                    .args(["sparse", "set", "--pattern", "."]),
            )
            .map(|_| ())
            .map_err(map_run_err)?;
        // `jj status` is the cheapest command that forces a working-copy
        // snapshot. This is NOT best-effort: step 8 immediately pins the
        // bookmark to this workspace's `@`, so if the snapshot failed, `@`'s
        // recorded tree would still be the empty `--sparse-patterns empty`
        // tree while the reflinked files sit on disk — a corrupt worktree
        // reported as success. Propagate the error so the rollback
        // (`op restore` + `remove_dir_all`) fires, mirroring the git path's
        // `refresh_index_with_progress?`.
        runner
            .run(Cmd::new("jj").in_dir(path).args(["status"]))
            .map(|_| ())
            .map_err(map_run_err)?;

        // 8. Put the bookmark on the new workspace's @. For a NEW branch
        //    we create it; when RESUMING an existing bookmark it sits one
        //    commit below (on `@-`), so we MOVE it forward onto `@` with
        //    `bookmark set` — `ws` requires a bookmark on `@`
        //    (`current_branch` reads it), and minting a duplicate would
        //    error "bookmark already exists".
        let revset = format!("{ws_name}@");
        if branch_already_exists {
            eprintln!("  Moving bookmark to workspace...");
            super::exec(runner, &["bookmark", "set", branch, "-r", &revset])?;
        } else {
            eprintln!("  Creating bookmark...");
            super::exec(runner, &["bookmark", "create", branch, "-r", &revset])?;
        }

        Ok(())
    })();

    // 9. Cleanup paths.
    //
    // On SUCCESS: restore source @ via `jj edit <orig_change_id>`. The
    // change-id survives snapshot rewrites that may have happened
    // during inner ops, so this lands on whatever the same logical
    // change is now.
    //
    // On FAILURE: skip the per-step source restore — `jj op restore`
    // below rolls back to pre_op which includes step 4's edit AND every
    // mutation since, restoring source state in one shot. Running an
    // extra `jj edit` here would just be undone by op restore and clutter
    // the op log.
    match &inner {
        Ok(_) => {
            if needs_move {
                eprintln!("  Restoring source workspace...");
                if let Err(e) = super::exec(runner, &["edit", &orig_change_id]) {
                    eprintln!(
                        "Warning: failed to restore source workspace @ '{orig_change_id}': {e}"
                    );
                }
            }
        }
        Err(_) => {
            let _ = super::exec(runner, &["op", "restore", &pre_op]);
            // Filesystem cleanup may fail on Windows when files are held
            // open by background indexers / antivirus. Logging the
            // failure beats silently leaving an orphan dir that the
            // next `ws new <same-branch>` would trip over with a
            // confusing "path exists" error.
            if path.exists()
                && let Err(rm_err) = std::fs::remove_dir_all(path)
            {
                eprintln!(
                    "Warning: failed to clean up partial workspace at {}: {rm_err}\n\
                     Remove manually if needed before retrying `ws new`.",
                    path.display()
                );
            }
        }
    }

    inner?;
    Ok(CreateOutcome::CowCloned)
}

/// Remove a workspace + delete the on-disk directory.
///
/// **Order matters**: remove the filesystem directory first, then call
/// `jj workspace forget`. If the swap is reversed and `forget` succeeds
/// but `remove_dir_all` fails, the user is left with an orphan directory
/// that `ws` has lost track of — worse than a still-attached workspace
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
