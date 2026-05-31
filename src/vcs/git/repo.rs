// ===========================================================================
// vcs/git/repo - Repository identity (root, name, branch, commit, trunk)
// ===========================================================================

use std::path::{Path, PathBuf};
use std::time::Duration;

use vcs_runner::{Cmd, RunError, Runner};

use super::errmap::map_run_err;
use crate::vcs::error::{Error, Result};

/// Get the root directory of the main git repository (not worktree).
///
/// Uses `--git-common-dir` to handle worktrees correctly — it returns the
/// main repo's `.git` regardless of which worktree the caller is sitting
/// in. Note: this is **not** equivalent to `vcs_runner::detect_vcs`, which
/// finds the nearest `.git`/`.jj` ancestor and would point at the worktree's
/// own gitdir instead.
pub(super) fn repo_root(runner: &dyn Runner, cwd: &Path) -> Result<PathBuf> {
    let out = runner
        .run(Cmd::new("git").in_dir(cwd).args(["rev-parse", "--git-common-dir"]))
        .map_err(|e| match e {
            // Outside-a-repo is the most common failure here; collapse it to
            // the friendly NotInRepo variant rather than the raw git stderr.
            RunError::NonZeroExit { .. } => Error::NotInRepo,
            other => map_run_err(other),
        })?;

    let git_dir = PathBuf::from(out.stdout_lossy().trim());

    // Resolve a relative `--git-common-dir` against the same directory the
    // command ran in (was `current_dir()` — identical when `cwd` is `None`).
    let git_dir = if git_dir.is_absolute() {
        git_dir
    } else {
        cwd.join(&git_dir)
    };

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

    git_dir
        .parent()
        .map(|p| p.to_path_buf())
        .ok_or(Error::NotInRepo)
}

/// Get the directory name of the current repository.
pub(super) fn repo_name(runner: &dyn Runner, cwd: &Path) -> Result<String> {
    let root = repo_root(runner, cwd)?;
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
pub(super) fn workspace_id(runner: &dyn Runner, cwd: &Path) -> Result<String> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let root = repo_root(runner, cwd)?;
    let name = repo_name(runner, cwd)?;

    let mut hasher = DefaultHasher::new();
    root.hash(&mut hasher);
    let hash = hasher.finish();

    Ok(format!("{}-{:06x}", name, hash & 0xFFFFFF))
}

/// Get the current branch name (`HEAD` symbolic ref).
pub(super) fn current_branch(runner: &dyn Runner, cwd: &Path) -> Result<String> {
    runner
        .run(Cmd::new("git").in_dir(cwd).args(["rev-parse", "--abbrev-ref", "HEAD"]))
        .map(|out| out.stdout_lossy().trim().to_string())
        .map_err(|e| match e {
            RunError::NonZeroExit { .. } => Error::NotInRepo,
            other => map_run_err(other),
        })
}

/// Get the current HEAD commit hash.
pub(super) fn current_commit(runner: &dyn Runner, cwd: &Path) -> Result<String> {
    runner
        .run(Cmd::new("git").in_dir(cwd).args(["rev-parse", "HEAD"]))
        .map(|out| out.stdout_lossy().trim().to_string())
        .map_err(|e| match e {
            RunError::NonZeroExit { .. } => Error::NotInRepo,
            other => map_run_err(other),
        })
}

/// Detect the trunk branch.
///
/// Priority: `origin/HEAD` (remote-authoritative) > `main` > `master` > `"main"`.
///
/// `origin/HEAD` wins because it reflects the upstream's actual default
/// branch — avoids silently picking `main` when the real trunk is `master`
/// (or vice versa) just because both happen to exist locally.
pub(super) fn detect_trunk(runner: &dyn Runner, cwd: &Path) -> Result<String> {
    if let Ok(out) = runner.run(
        Cmd::new("git")
            .in_dir(cwd)
            .args(["symbolic-ref", "refs/remotes/origin/HEAD"]),
    ) {
        let full = out.stdout_lossy().trim().to_string();
        if let Some(branch) = full.strip_prefix("refs/remotes/origin/") {
            return Ok(branch.to_string());
        }
    }

    for branch in ["main", "master"] {
        if branch_exists(runner, cwd, branch)? {
            return Ok(branch.to_string());
        }
    }

    Ok("main".to_string())
}

/// List all local branch names. One subprocess instead of N `branch_exists` calls.
pub(super) fn local_branches(runner: &dyn Runner, cwd: &Path) -> Result<Vec<String>> {
    match runner.run(Cmd::new("git").in_dir(cwd).args([
        "for-each-ref",
        "--format=%(refname:short)",
        "refs/heads/",
    ])) {
        Ok(out) => Ok(out
            .stdout_lossy()
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| l.to_string())
            .collect()),
        // Outside a repo: return empty rather than error, matching the
        // original code which returned `Ok(Vec::new())` on non-success.
        Err(RunError::NonZeroExit { .. }) => Ok(Vec::new()),
        Err(e) => Err(map_run_err(e)),
    }
}

/// Check whether a local branch exists. Uses `show-ref --verify --quiet`
/// — non-zero exit means "no", and we map that to `Ok(false)` rather than
/// surfacing an error.
pub(super) fn branch_exists(runner: &dyn Runner, cwd: &Path, name: &str) -> Result<bool> {
    let refname = format!("refs/heads/{name}");
    match runner.run(
        Cmd::new("git")
            .in_dir(cwd)
            .args(["show-ref", "--verify", "--quiet", &refname]),
    ) {
        Ok(_) => Ok(true),
        Err(RunError::NonZeroExit { .. }) => Ok(false),
        Err(e) => Err(map_run_err(e)),
    }
}

/// Resolve a ref / revision to its full commit hash (`git rev-parse
/// <rev>^{commit}`). `^{commit}` peels annotated tags to the commit they
/// point at; for a plain branch it's a no-op. Used by the CoW resume path
/// to check out a branch's commit *detached* (so the branch ref stays free
/// for `git worktree add <path> <branch>`).
pub(super) fn resolve_commit(runner: &dyn Runner, cwd: &Path, rev: &str) -> Result<String> {
    let out = runner
        .run(
            Cmd::new("git")
                .in_dir(cwd)
                .args(["rev-parse", "--verify", &format!("{rev}^{{commit}}")]),
        )
        .map_err(map_run_err)?;
    Ok(out.stdout_lossy().trim().to_string())
}

/// Whether a branch named `name` exists on `origin`, queried WITHOUT a
/// fetch via `git ls-remote --heads origin <name>`.
///
/// **Best-effort**: any failure (no remote, auth/credential prompt,
/// network down, timeout) maps to `Ok(false)` so `ws new` never blocks on
/// the probe. `GIT_TERMINAL_PROMPT=0` makes a private remote without
/// cached credentials fail fast instead of hanging on an interactive
/// prompt; a 10 s timeout caps a wedged connection.
pub(super) fn remote_branch_exists(runner: &dyn Runner, cwd: &Path, name: &str) -> Result<bool> {
    match runner.run(
        Cmd::new("git")
            .in_dir(cwd)
            .args(["ls-remote", "--heads", "origin", name])
            .env("GIT_TERMINAL_PROMPT", "0")
            .timeout(Duration::from_secs(10)),
    ) {
        Ok(out) => Ok(crate::vcs::common::ls_remote_has_branch(&out.stdout_lossy(), name)),
        // No remote / auth failure / timeout / spawn error — treat as
        // "not on remote" rather than surfacing an error. RunError is
        // #[non_exhaustive]; a catch-all keeps this robust across variants.
        Err(_) => Ok(false),
    }
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
