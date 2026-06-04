// ===========================================================================
// vcs/git/repo - Repository identity (root, name, branch, commit, trunk)
// ===========================================================================

use std::path::{Path, PathBuf};

use processkit::Error as PkError;
use vcs_git::{GitApi, GitAt};

use super::GitClient;
use super::errmap::map_pk_err;
use crate::vcs::error::{Error, Result};

/// Get the root directory of the main git repository (not worktree).
///
/// Uses `--git-common-dir` (via the typed client's `common_dir`) to handle
/// worktrees correctly — it returns the main repo's `.git` regardless of which
/// worktree the caller is sitting in.
pub(crate) async fn repo_root(git: &GitClient, cwd: &Path) -> Result<PathBuf> {
    // Outside-a-repo is the most common failure here; collapse a non-zero exit
    // to the friendly NotInRepo variant rather than the raw git stderr.
    let git_dir = git.common_dir(cwd).await.map_err(|e| match e {
        PkError::Exit { .. } => Error::NotInRepo,
        other => map_pk_err(other),
    })?;

    // Resolve a relative `--git-common-dir` against the same directory the
    // command ran in.
    let git_dir = if git_dir.is_absolute() { git_dir } else { cwd.join(&git_dir) };

    let git_dir = git_dir.canonicalize().map_err(|_| Error::NotInRepo)?;

    // Walk up to find `.git`. For worktrees, --git-common-dir returns the
    // main repo's `.git/` directly; for worktree-internal queries it may
    // return `.git/worktrees/<branch>` — both paths converge at `.git`.
    let git_dir = if git_dir.ends_with(".git") {
        git_dir
    } else {
        let mut current = git_dir.as_path();
        loop {
            if current.ends_with(".git") {
                break;
            }
            current = current.parent().ok_or(Error::NotInRepo)?;
        }
        current.to_path_buf()
    };

    git_dir.parent().map(|p| p.to_path_buf()).ok_or(Error::NotInRepo)
}

/// Get the directory name of the current repository.
pub(crate) async fn repo_name(git: &GitClient, cwd: &Path) -> Result<String> {
    let root = repo_root(git, cwd).await?;
    root.file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .ok_or_else(|| Error::Command("cannot determine repo name".into()))
}

/// Get the unique workspace ID for the current repository.
///
/// Format: `{repo_name}-{hash[0:6]}` where hash is derived from the
/// absolute repo path. Ensures repos with the same directory name living
/// in different absolute locations get distinct workspace directories.
pub(crate) async fn workspace_id(git: &GitClient, cwd: &Path) -> Result<String> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let root = repo_root(git, cwd).await?;
    let name = repo_name(git, cwd).await?;

    let mut hasher = DefaultHasher::new();
    root.hash(&mut hasher);
    let hash = hasher.finish();

    Ok(format!("{}-{:06x}", name, hash & 0xFFFFFF))
}

/// Get the current branch name (`HEAD` symbolic ref).
pub(crate) async fn current_branch(g: GitAt<'_>) -> Result<String> {
    g.current_branch().await.map_err(|e| match e {
        PkError::Exit { .. } => Error::NotInRepo,
        other => map_pk_err(other),
    })
}

/// Get the current HEAD commit hash.
pub(crate) async fn current_commit(g: GitAt<'_>) -> Result<String> {
    g.rev_parse("HEAD").await.map_err(|e| match e {
        PkError::Exit { .. } => Error::NotInRepo,
        other => map_pk_err(other),
    })
}

/// List all local branch names. One subprocess instead of N `branch_exists` calls.
pub(crate) async fn local_branches(g: GitAt<'_>) -> Result<Vec<String>> {
    match g.branches().await {
        Ok(branches) => Ok(branches.into_iter().map(|b| b.name).collect()),
        // Outside a repo: return empty rather than error, matching the
        // original code which returned `Ok(Vec::new())` on non-success.
        Err(PkError::Exit { .. }) => Ok(Vec::new()),
        Err(e) => Err(map_pk_err(e)),
    }
}

/// Check whether a local branch exists. `show-ref --verify --quiet` exits
/// non-zero when the branch is absent — mapped to `Ok(false)` rather than
/// surfacing an error.
pub(crate) async fn branch_exists(g: GitAt<'_>, name: &str) -> Result<bool> {
    match g.branch_exists(name).await {
        Ok(exists) => Ok(exists),
        Err(PkError::Exit { .. }) => Ok(false),
        Err(e) => Err(map_pk_err(e)),
    }
}

/// Resolve a ref / revision to its full commit hash (`git rev-parse
/// <rev>^{commit}`). `^{commit}` peels annotated tags to the commit they
/// point at; for a plain branch it's a no-op. Used by the CoW resume path
/// to check out a branch's commit *detached* (so the branch ref stays free
/// for `git worktree add <path> <branch>`).
pub(crate) async fn resolve_commit(g: GitAt<'_>, rev: &str) -> Result<String> {
    g.resolve_commit(rev).await.map_err(map_pk_err)
}

/// Whether a branch named `name` exists on `origin`, queried WITHOUT a fetch.
///
/// Delegates to the typed client, which queries the fully-qualified
/// `refs/heads/<name>` ref (an EXACT match — `ls-remote origin <name>` would
/// tail-match `x/<name>`) with `GIT_TERMINAL_PROMPT=0` + a 10s timeout.
///
/// **Best-effort**: any failure (no remote, auth prompt, network down, timeout,
/// missing `git`) maps to `Ok(false)` so `ws new` never blocks on the probe.
pub(crate) async fn remote_branch_exists(g: GitAt<'_>, name: &str) -> Result<bool> {
    Ok(g.remote_branch_exists(name).await.unwrap_or(false))
}

/// Whether `cwd` is inside `path` (after canonicalizing both). Pure
/// filesystem helper; no subprocess.
pub fn is_cwd_inside(path: &std::path::Path) -> bool {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| cwd.canonicalize().ok())
        .and_then(|cwd| path.canonicalize().ok().map(|p| cwd.starts_with(p)))
        .unwrap_or(false)
}
