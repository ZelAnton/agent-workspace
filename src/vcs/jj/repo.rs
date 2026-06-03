// ===========================================================================
// vcs/jj/repo - Repository identity + bookmark CRUD (jj)
// ===========================================================================
//
// Mirrors the public surface of `src/vcs/git/repo.rs` + `branch.rs`'s
// bookmark-shaped methods, on the typed `vcs-jj` client.

use std::path::{Path, PathBuf};
use std::time::Duration;

use processkit::Error as PkError;
use vcs_jj::JjApi;

use super::JjClient;
use super::errmap::map_pk_err;
use crate::vcs::error::{Error, Result};

/// Working-copy root for the active jj workspace. For colocated repos this is
/// the same canonical path git's `repo_root()` returns, so [`workspace_id`]
/// produces an identical hash across both backends — load-bearing for git→jj
/// migration in colocated repos.
pub(crate) async fn repo_root(jj: &JjClient, cwd: &Path) -> Result<PathBuf> {
    let root = jj.root(cwd).await.map_err(|e| match e {
        PkError::Exit { .. } => Error::NotInRepo,
        other => map_pk_err(other),
    })?;
    root.canonicalize().map_err(|_| Error::NotInRepo)
}

/// Repo name = the working-copy root's directory name. Same algorithm as git's
/// `repo_name()` so the two backends agree for colocated repos.
pub(crate) async fn repo_name(jj: &JjClient, cwd: &Path) -> Result<String> {
    let root = repo_root(jj, cwd).await?;
    root.file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .ok_or_else(|| Error::Command("cannot determine repo name".into()))
}

/// Workspace ID — identical hashing to the git backend's `workspace_id` so colocated
/// repos keep the same `$AGENT_WORKSPACE_DIR/<id>/` directory whether `ws`
/// resolves to git or jj. **Don't change the algorithm in isolation** — both
/// backends must move in lockstep.
pub(crate) async fn workspace_id(jj: &JjClient, cwd: &Path) -> Result<String> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let root = repo_root(jj, cwd).await?;
    let name = repo_name(jj, cwd).await?;

    let mut hasher = DefaultHasher::new();
    root.hash(&mut hasher);
    let hash = hasher.finish();

    Ok(format!("{}-{:06x}", name, hash & 0xFFFFFF))
}

/// Current bookmark on `@`. Per locked decision, jj-managed worktrees must have
/// a bookmark — a bookmark-less `@` is a usage error with a prescriptive hint.
pub(crate) async fn current_branch(jj: &JjClient, cwd: &Path) -> Result<String> {
    let bookmark = jj.current_bookmark(cwd).await.map_err(|e| match e {
        PkError::Exit { .. } => Error::NotInRepo,
        other => map_pk_err(other),
    })?;
    bookmark.ok_or_else(|| {
        Error::Command(
            "no bookmark on @; jj-managed worktrees must have a named bookmark — \
             run `ws new` to create one, or `jj bookmark create <name>` manually"
                .into(),
        )
    })
}

/// Current working-copy change-id (stable across content rewrites, unlike
/// commit-id). Used to restore the source workspace's `@` after CoW.
pub(crate) async fn current_change_id(jj: &JjClient, cwd: &Path) -> Result<String> {
    let out = jj.template_query(cwd, "@", "change_id", Some(1)).await.map_err(|e| match e {
        PkError::Exit { .. } => Error::NotInRepo,
        other => map_pk_err(other),
    })?;
    Ok(out.trim().to_string())
}

/// HEAD commit id (short or full — callers treat it as an opaque pointer).
pub(crate) async fn current_commit(jj: &JjClient, cwd: &Path) -> Result<String> {
    let out = jj.template_query(cwd, "@", "commit_id", Some(1)).await.map_err(|e| match e {
        PkError::Exit { .. } => Error::NotInRepo,
        other => map_pk_err(other),
    })?;
    Ok(out.trim().to_string())
}

/// Resolve a revset to a concrete commit-id (used by the CoW path to pin `base`
/// before editing). `--limit 1` picks the first match for multi-commit revsets.
pub(crate) async fn resolve_commit(jj: &JjClient, cwd: &Path, revset: &str) -> Result<String> {
    let out = jj.template_query(cwd, revset, "commit_id", Some(1)).await.map_err(map_pk_err)?;
    let commit = out.trim().to_string();
    if commit.is_empty() {
        return Err(Error::Command(format!("jj: revision '{revset}' resolved to empty commit-id")));
    }
    Ok(commit)
}

/// Detect the trunk bookmark name.
///
/// Priority: jj's `trunk()` revset (already prefers default-remote trunk →
/// `main` → `master` → `trunk`) → local `main`/`master` bookmark → literal
/// `"main"`.
pub(crate) async fn detect_trunk(jj: &JjClient, cwd: &Path) -> Result<String> {
    if let Ok(Some(name)) = jj.trunk(cwd).await {
        return Ok(name);
    }
    for candidate in ["main", "master"] {
        if branch_exists(jj, cwd, candidate).await? {
            return Ok(candidate.to_string());
        }
    }
    Ok("main".to_string())
}

/// List all local bookmark names.
pub(crate) async fn local_branches(jj: &JjClient, cwd: &Path) -> Result<Vec<String>> {
    match jj.bookmarks(cwd).await {
        Ok(bookmarks) => Ok(bookmarks.into_iter().map(|b| b.name).collect()),
        // Outside a repo: empty list, matching git.
        Err(PkError::Exit { .. }) => Ok(Vec::new()),
        Err(e) => Err(map_pk_err(e)),
    }
}

/// Check whether a bookmark exists in this workspace.
pub(crate) async fn branch_exists(jj: &JjClient, cwd: &Path, name: &str) -> Result<bool> {
    Ok(local_branches(jj, cwd).await?.iter().any(|b| b == name))
}

/// Whether a bookmark named `name` exists on `origin`, queried WITHOUT a fetch.
///
/// jj has no cheap native remote-ref probe, so on a COLOCATED repo (the common
/// case) we run the same `git ls-remote` the git backend uses — read-only, so no
/// `jj git import` reconciliation is needed afterward.
///
/// **Best-effort**: a pure-jj repo (no `.git`), an unreachable remote, an auth
/// prompt, or a timeout all map to `Ok(false)`.
pub(crate) async fn remote_branch_exists(jj: &JjClient, cwd: &Path, name: &str) -> Result<bool> {
    let Ok(root) = repo_root(jj, cwd).await else {
        return Ok(false);
    };
    if !root.join(".git").exists() {
        return Ok(false);
    }
    let res = processkit::Command::new("git")
        .current_dir(cwd)
        .args(["ls-remote", "--heads", "origin", name])
        .env("GIT_TERMINAL_PROMPT", "0")
        .timeout(Duration::from_secs(10))
        .output_string()
        .await;
    // Exact `refs/heads/<name>` match — `ls-remote` matches by trailing path
    // component, so a non-empty test would falsely match `x/<name>`.
    Ok(matches!(res, Ok(out)
        if out.is_success() && crate::vcs::common::ls_remote_has_branch(out.stdout(), name)))
}

/// Rename a bookmark.
pub(crate) async fn rename_branch(jj: &JjClient, cwd: &Path, old: &str, new: &str) -> Result<()> {
    jj.bookmark_rename(cwd, old, new).await.map_err(map_pk_err)
}

/// Delete a bookmark. jj has no "force" flag — `_force` is accepted for trait
/// parity (jj's `bookmark delete` is already safe: commits remain reachable via
/// change-id / op log).
pub(crate) async fn delete_branch(jj: &JjClient, cwd: &Path, name: &str, _force: bool) -> Result<()> {
    jj.bookmark_delete(cwd, name).await.map_err(map_pk_err)
}
