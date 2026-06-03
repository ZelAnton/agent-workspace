// ===========================================================================
// vcs/jj/worktree - Workspace CRUD (jj's analogue of git worktrees)
// ===========================================================================
//
// **jj concept mapping**:
//   - `git worktree` → `jj workspace` (each has its own working copy)
//   - `git worktree add <path> <branch>` → `jj workspace add --name <derived>
//     -r <base> <path>` + bookmark create/set on the new `@`
//   - `git worktree list` → `jj workspace list` + `jj workspace root --name`
//   - `git worktree remove` → `fs::remove_dir_all` + `jj workspace forget`
//   - `git worktree move` → not supported (locked semantic decision)

use std::path::{Path, PathBuf};

use vcs_jj::{JjApi, WorkspaceAdd};

use super::JjClient;
use super::errmap::map_pk_err;
use crate::vcs::common::{CreateOutcome, WorktreeInfo, path_str};
use crate::vcs::error::{Error, Result};

/// Derive a jj workspace name from a branch name. jj workspace names must be
/// valid identifiers — no `/`, `.`, `\`, `:`, or whitespace — so substitute to
/// `_`. Deterministic so `remove_worktree` can reconstruct it.
pub(crate) fn workspace_name_for(branch: &str) -> String {
    branch
        .chars()
        .map(|c| match c {
            '/' | '\\' | '.' | ':' | ' ' | '\t' | '\n' | '\r' => '_',
            other => other,
        })
        .collect()
}

/// Create a new workspace + bookmark for the requested branch.
pub(crate) async fn create_worktree(
    jj: &JjClient,
    cwd: &Path,
    path: &Path,
    branch: &str,
    base: &str,
) -> Result<CreateOutcome> {
    // Resume an existing bookmark (create the workspace at it) rather than
    // minting a new one — parity with the git backend. Refuse only when the
    // bookmark is already checked out in another workspace.
    let branch_already_exists = super::repo::branch_exists(jj, cwd, branch).await?;
    if branch_already_exists {
        let worktrees = list_worktrees(jj, cwd).await?;
        if worktrees.iter().any(|wt| wt.branch.as_deref() == Some(branch)) {
            return Err(Error::WorktreeExists(branch.to_string()));
        }
    }

    // For a resumed bookmark the workspace starts AT that bookmark; the caller's
    // `base` is only the recorded merge target. For a new branch, `base` is the
    // start point.
    let effective_base = if branch_already_exists { branch } else { base };

    // CoW probe — mirrors the git dispatcher's shape exactly.
    let parent = path.parent().unwrap_or(path);
    if std::env::var(crate::cow::DISABLE_COW_ENV).is_err()
        && let Ok(repo_root) = super::repo::repo_root(jj, cwd).await
        && parent.exists()
        && crate::cow::can_clone(&repo_root, parent)
    {
        // The jj CoW flow `jj edit`s the source workspace's `@`, which is unsafe
        // to run concurrently with another `ws new`. Hold a per-repo lock for
        // it; on contention, fall through to the concurrency-safe plain path.
        // `_cow_lock` stays bound until `create_worktree_cow` returns.
        if let Some(_cow_lock) = crate::cow::CowLock::try_acquire(&repo_root) {
            return create_worktree_cow(
                jj,
                cwd,
                &repo_root,
                path,
                branch,
                effective_base,
                branch_already_exists,
            )
            .await;
        }
        eprintln!(
            "  Another `ws new` is using copy-on-write on this repo; \
             using a plain workspace add (safe, slower)."
        );
    }

    create_worktree_plain(jj, cwd, path, branch, effective_base, branch_already_exists).await
}

/// Create a workspace from a bookmark that exists only on `origin`: fetch just
/// that bookmark, then create the workspace from it (resuming a local bookmark
/// if `git.auto-local-bookmark` minted one, else basing on `<branch>@origin`).
pub(crate) async fn create_worktree_from_remote(
    jj: &JjClient,
    cwd: &Path,
    path: &Path,
    branch: &str,
) -> Result<CreateOutcome> {
    eprintln!("  Fetching '{branch}' from origin...");
    super::ops::fetch_remote_branch(jj, cwd, branch).await?;
    let base = if super::repo::branch_exists(jj, cwd, branch).await? {
        branch.to_string()
    } else {
        format!("{branch}@origin")
    };
    create_worktree(jj, cwd, path, branch, &base).await
}

/// Standard `jj workspace add` — jj materialises the working copy itself.
async fn create_worktree_plain(
    jj: &JjClient,
    cwd: &Path,
    path: &Path,
    branch: &str,
    base: &str,
    branch_already_exists: bool,
) -> Result<CreateOutcome> {
    let ws_name = workspace_name_for(branch);

    eprintln!("  Running jj workspace add...");
    jj.workspace_add(cwd, WorkspaceAdd::new(ws_name.clone(), base.to_string(), path))
        .await
        .map_err(map_pk_err)?;

    // Put the bookmark on the new workspace's @. `<ws_name>@` resolves to that
    // workspace's working-copy commit regardless of where we run from. For a NEW
    // branch we create it; for a RESUMED branch the bookmark sits on `@-` (since
    // `workspace add -r <branch>` puts an empty change on top), so MOVE it
    // forward onto `@` with `bookmark set` — `ws` requires a bookmark on `@`.
    let revset = format!("{ws_name}@");
    if branch_already_exists {
        eprintln!("  Moving bookmark to workspace...");
        jj.bookmark_set(cwd, branch, &revset).await.map_err(map_pk_err)?;
    } else {
        eprintln!("  Creating bookmark...");
        jj.bookmark_create(cwd, branch, &revset).await.map_err(map_pk_err)?;
    }

    Ok(CreateOutcome::Plain)
}

/// CoW creation: edit source to base → `jj workspace add --sparse-patterns
/// empty` → reflink-copy → restore patterns + snapshot → bookmark → restore
/// source `@`. `--sparse-patterns empty` is the analogue of git's
/// `--no-checkout`. Rollback via `jj op restore <pre_op>` + `remove_dir_all` on
/// internal failure.
#[allow(clippy::too_many_arguments)]
async fn create_worktree_cow(
    jj: &JjClient,
    cwd: &Path,
    repo_root: &Path,
    path: &Path,
    branch: &str,
    base: &str,
    branch_already_exists: bool,
) -> Result<CreateOutcome> {
    let ws_name = workspace_name_for(branch);
    let path_arg = path_str(path)?;

    // 1. Capture pre-op for precise op-log rollback.
    let pre_op = super::ops::capture_op_id(jj, cwd).await?;
    // 2. Source @ change-id (survives snapshot rewrites; a later `jj edit
    //    <change_id>` lands on whatever that logical change has become).
    let orig_change_id = super::repo::current_change_id(jj, cwd).await?;
    // 3. Resolve `base` to a concrete commit-id BEFORE moving.
    let base_commit = super::repo::resolve_commit(jj, cwd, base).await?;
    // 4. Move source workspace to base (skip if already there).
    let orig_commit = super::repo::current_commit(jj, cwd).await?;
    let needs_move = orig_commit != base_commit;
    if needs_move {
        eprintln!("  Switching to base revision...");
        jj.edit(cwd, &base_commit).await.map_err(map_pk_err)?;
    }

    #[cfg(windows)]
    eprintln!("  Using ReFS block clone...");
    #[cfg(not(windows))]
    eprintln!("  Using CoW (reflink) clone...");

    let inner: Result<()> = async {
        // 5. Create the empty (sparse) workspace.
        eprintln!("  Creating workspace skeleton...");
        super::exec(
            cwd,
            [
                "workspace",
                "add",
                "--name",
                ws_name.as_str(),
                "-r",
                base_commit.as_str(),
                "--sparse-patterns",
                "empty",
                path_arg,
            ],
        )
        .await?;

        // 6. Reflink-copy source files (matching base's tree) into the new
        //    workspace, skipping BOTH `.jj/` and `.git/` (colocated). Sync +
        //    heavy, so it runs on a blocking thread.
        let repo_root_owned = repo_root.to_path_buf();
        let path_owned = path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            crate::cow::try_clone_dir_except(&repo_root_owned, &path_owned, &[".jj", ".git"])
        })
        .await
        .expect("reflink clone task panicked")
        .map(|_bytes| ())
        .map_err(Error::Cow)?;

        // 7. Restore the sparse-pattern set to "all files" and snapshot. After
        //    this, jj's `@` working copy matches base's tree.
        eprintln!("  Configuring sparse patterns and snapshotting...");
        super::exec(path, ["sparse", "set", "--pattern", "."]).await?;
        // `jj status` forces a working-copy snapshot. NOT best-effort: step 8
        // pins the bookmark to this `@`, so a failed snapshot would record the
        // empty tree while reflinked files sit on disk — a corrupt worktree
        // reported as success. Propagate so rollback fires.
        jj.status(path).await.map_err(map_pk_err)?;

        // 8. Put the bookmark on the new workspace's @.
        let revset = format!("{ws_name}@");
        if branch_already_exists {
            eprintln!("  Moving bookmark to workspace...");
            jj.bookmark_set(cwd, branch, &revset).await.map_err(map_pk_err)?;
        } else {
            eprintln!("  Creating bookmark...");
            jj.bookmark_create(cwd, branch, &revset).await.map_err(map_pk_err)?;
        }

        Ok(())
    }
    .await;

    // 9. Cleanup. On SUCCESS: restore source @ via the change-id. On FAILURE:
    //    `jj op restore <pre_op>` rolls back step 4's edit AND every mutation
    //    since in one shot, plus delete any half-materialised workspace dir.
    match &inner {
        Ok(_) => {
            if needs_move {
                eprintln!("  Restoring source workspace...");
                if let Err(e) = jj.edit(cwd, &orig_change_id).await {
                    eprintln!("Warning: failed to restore source workspace @ '{orig_change_id}': {e}");
                }
            }
        }
        Err(_) => {
            let _ = jj.op_restore(cwd, &pre_op).await;
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

/// Remove a workspace + delete the on-disk directory. **Order matters**: delete
/// the filesystem dir first, then `jj workspace forget` — an orphan dir `ws` has
/// lost track of is worse than a still-attached workspace.
pub(crate) async fn remove_worktree(jj: &JjClient, cwd: &Path, path: &Path, _force: bool) -> Result<()> {
    let ws_name = workspace_name_for_path(jj, cwd, path).await?;
    if path.exists() {
        std::fs::remove_dir_all(path).map_err(|e| Error::Command(e.to_string()))?;
    }
    // Best-effort forget — jj happily forgets an already-deleted ws dir.
    let _ = jj.workspace_forget(cwd, &ws_name).await;
    Ok(())
}

/// Normalize a path for comparison against jj's `workspace root` output:
/// canonicalize (symlinks / macOS case) + strip the Windows verbatim prefix
/// (`\\?\…`, which `canonicalize` adds but jj never emits).
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

/// Look up the workspace name whose root matches `path`. Uses jj's recorded
/// workspace `name` (not a re-derived `path.file_name()` guess) so branches
/// containing `/` resolve correctly.
async fn workspace_name_for_path(jj: &JjClient, cwd: &Path, path: &Path) -> Result<String> {
    let target = normalize_for_compare(path);
    let workspaces = jj.workspace_list(cwd).await.map_err(map_pk_err)?;
    for ws in workspaces {
        let Ok(root) = jj.workspace_root(cwd, Some(ws.name.clone())).await else {
            continue;
        };
        if normalize_for_compare(&root) == target || root == target || root == path {
            return Ok(ws.name);
        }
    }
    Err(Error::WorktreeNotFound(path.display().to_string()))
}

/// Synchronous path→workspace-name lookup for [`WorktreeGuard`]'s `Drop` (which
/// can't await). Mirrors [`workspace_name_for_path`] using blocking `jj`
/// subprocesses. Returns `None` when no workspace matches `path` (jj missing /
/// not in a repo / no match) — the caller then SKIPS the forget rather than
/// guessing a name, so it can never forget an unrelated workspace.
pub(crate) fn workspace_name_for_path_blocking(cwd: &Path, path: &Path) -> Option<String> {
    let target = normalize_for_compare(path);
    // `jj workspace list -T 'name ++ "\n"'` → one workspace name per line.
    let out = std::process::Command::new("jj")
        .current_dir(cwd)
        .args(["workspace", "list", "-T", "name ++ \"\\n\""])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    for name in String::from_utf8_lossy(&out.stdout).lines() {
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let root = std::process::Command::new("jj")
            .current_dir(cwd)
            .args(["workspace", "root", "--name", name])
            .output();
        if let Ok(r) = root
            && r.status.success()
        {
            let p = PathBuf::from(String::from_utf8_lossy(&r.stdout).trim().to_string());
            if normalize_for_compare(&p) == target || p == target || p == path {
                return Some(name.to_string());
            }
        }
    }
    None
}

/// List all attached workspaces, projected into the backend-agnostic
/// `WorktreeInfo`. jj's `Workspace` carries no path, so we resolve each via
/// `jj workspace root --name`.
pub(crate) async fn list_worktrees(jj: &JjClient, cwd: &Path) -> Result<Vec<WorktreeInfo>> {
    let workspaces = jj.workspace_list(cwd).await.map_err(|e| match e {
        processkit::Error::Exit { .. } => Error::NotInRepo,
        other => map_pk_err(other),
    })?;
    let mut out = Vec::new();
    for ws in workspaces {
        // Skip rows whose path can't be resolved — there's no useful entry
        // without a path.
        let Ok(root) = jj.workspace_root(cwd, Some(ws.name.clone())).await else {
            continue;
        };
        out.push(WorktreeInfo {
            path: root,
            // Match git: pick the first attached bookmark.
            branch: ws.bookmarks.into_iter().next(),
            commit: if ws.commit.is_empty() { None } else { Some(ws.commit) },
            is_bare: false,
        });
    }
    Ok(out)
}
