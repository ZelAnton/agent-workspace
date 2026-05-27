// ===========================================================================
// vcs/jj/repo - Repository identity + bookmark CRUD (jj)
// ===========================================================================
//
// Mirrors the public surface of `src/vcs/git/repo.rs` + `branch.rs`'s
// bookmark-shaped methods. Implementations go through `vcs_runner::Cmd`
// with the `jj` binary; all reuse the `Runner` injection so tests can mock.

use std::path::PathBuf;

use vcs_runner::{
    parse_bookmark_output, parse_log_output, Cmd, RemoteStatus, RunError, Runner, BOOKMARK_TEMPLATE,
    LOG_TEMPLATE,
};

use super::errmap::map_run_err;
use crate::vcs::error::{Error, Result};

/// Get the working-copy root for the active jj workspace.
///
/// jj's `root` command prints the workspace root directly — no walk-up
/// needed (unlike git's `--git-common-dir` dance). For colocated repos this
/// is the same canonical path git's `repo_root()` returns, so
/// [`workspace_id`] produces an identical hash across both backends — the
/// load-bearing invariant for git→jj migration in colocated repos.
pub(super) fn repo_root(runner: &dyn Runner) -> Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    let out = runner
        .run(Cmd::new("jj").in_dir(&cwd).args(["root"]))
        .map_err(|e| match e {
            RunError::NonZeroExit { .. } => Error::NotInRepo,
            other => map_run_err(other),
        })?;
    let root = PathBuf::from(out.stdout_lossy().trim());
    root.canonicalize().map_err(|_| Error::NotInRepo)
}

/// Repo name = the working-copy root's directory name. Same algorithm as
/// git's `repo_name()` so the two backends agree for colocated repos.
pub(super) fn repo_name(runner: &dyn Runner) -> Result<String> {
    let root = repo_root(runner)?;
    root.file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .ok_or_else(|| Error::Command("cannot determine repo name".into()))
}

/// Workspace ID — identical hashing to `GitBackend::workspace_id()` so
/// colocated repos keep the same `$AGENT_WORKSPACE_DIR/<id>/` directory
/// whether `ws` resolves to git or jj. **Don't change the algorithm in
/// isolation** — both backends must move in lockstep.
pub(super) fn workspace_id(runner: &dyn Runner) -> Result<String> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let root = repo_root(runner)?;
    let name = repo_name(runner)?;

    let mut hasher = DefaultHasher::new();
    root.hash(&mut hasher);
    let hash = hasher.finish();

    Ok(format!("{}-{:06x}", name, hash & 0xFFFFFF))
}

/// Current bookmark on `@`. If `@` has 0 bookmarks → error (per locked
/// decision: jj-managed worktrees must have a bookmark). If multiple,
/// pick the lexicographically smallest for deterministic output.
///
/// Uses vcs-runner's `LOG_TEMPLATE` + `parse_log_output` so the bookmark
/// list comes back already parsed; pick `entries[0].local_bookmarks.min()`.
pub(super) fn current_branch(runner: &dyn Runner) -> Result<String> {
    let cwd = std::env::current_dir()?;
    let out = runner
        .run(
            Cmd::new("jj")
                .in_dir(&cwd)
                .args(["log", "-r", "@", "-T", LOG_TEMPLATE, "--no-graph", "--limit", "1"]),
        )
        .map_err(|e| match e {
            RunError::NonZeroExit { .. } => Error::NotInRepo,
            other => map_run_err(other),
        })?;

    let parsed = parse_log_output(&out.stdout_lossy());
    let entry = parsed.entries.first().ok_or(Error::NotInRepo)?;

    // Pick deterministically (lexicographically smallest) so a commit with
    // multiple bookmarks doesn't yield different answers across calls.
    entry.local_bookmarks.iter().min().cloned().ok_or_else(|| {
        // No domain variant for this in `vcs::Error` — use `Command` as the
        // user-facing catch-all. Message stays prescriptive so the user knows
        // exactly what to fix.
        Error::Command(
            "no bookmark on @; jj-managed worktrees must have a named bookmark — \
             run `ws new` to create one, or `jj bookmark create <name>` manually"
                .into(),
        )
    })
}

/// Current working-copy change-id (a stable jj identifier that survives
/// content rewrites, unlike commit_id which changes on every snapshot).
/// Used to restore the source workspace's `@` after CoW orchestration:
/// `jj edit <commit_id>` jumps to a frozen point in history; `jj edit
/// <change_id>` jumps to whichever revision currently bears that
/// change-id, which is what we want after intervening snapshots.
pub(super) fn current_change_id(runner: &dyn Runner) -> Result<String> {
    let cwd = std::env::current_dir()?;
    runner
        .run(
            Cmd::new("jj")
                .in_dir(&cwd)
                .args(["log", "-r", "@", "-T", "change_id", "--no-graph", "--limit", "1"]),
        )
        .map(|out| out.stdout_lossy().trim().to_string())
        .map_err(|e| match e {
            RunError::NonZeroExit { .. } => Error::NotInRepo,
            other => map_run_err(other),
        })
}

/// HEAD commit id (short form, matching git's behaviour of returning
/// whatever `rev-parse HEAD` prints — a full sha for git, a short id for jj.
/// Callers use this as an opaque pointer; full vs short doesn't matter).
pub(super) fn current_commit(runner: &dyn Runner) -> Result<String> {
    let cwd = std::env::current_dir()?;
    runner
        .run(
            Cmd::new("jj")
                .in_dir(&cwd)
                .args(["log", "-r", "@", "-T", "commit_id", "--no-graph", "--limit", "1"]),
        )
        .map(|out| out.stdout_lossy().trim().to_string())
        .map_err(|e| match e {
            RunError::NonZeroExit { .. } => Error::NotInRepo,
            other => map_run_err(other),
        })
}

/// Detect the trunk bookmark name.
///
/// Priority: jj's `trunk()` revset (which already prefers default-remote
/// trunk → `main` → `master` → `trunk`) → check for local `main`/`master`
/// bookmark explicitly → literal `"main"`.
///
/// **Deterministic selection** when `trunk()` resolves to a commit with
/// multiple bookmarks attached (e.g. both `main` and `master`): prefer
/// `main` → `master` → lex-smallest. jj's internal bookmark iteration
/// order is implementation-defined and could shift across versions; we
/// pin a stable choice so `ws status`/`ws cd` output doesn't flicker.
pub(super) fn detect_trunk(runner: &dyn Runner) -> Result<String> {
    let cwd = std::env::current_dir()?;
    if let Ok(out) = runner.run(Cmd::new("jj").in_dir(&cwd).args([
        "log",
        "-r",
        "trunk()",
        "-T",
        r#"self.local_bookmarks().map(|b| b.name()).join("\n") ++ "\n""#,
        "--no-graph",
        "--limit",
        "1",
    ])) {
        let mut names: Vec<String> = out
            .stdout_lossy()
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        // Stable priority: well-known trunk names first, then alphabetical
        // for the lex-smallest fallback. The sort only matters for the
        // fallback — the well-known lookup doesn't care about order — so
        // do it lazily inside that branch.
        if !names.is_empty() {
            for preferred in ["main", "master", "trunk"] {
                if let Some(name) = names.iter().find(|n| n.as_str() == preferred) {
                    return Ok(name.clone());
                }
            }
            names.sort();
            return Ok(names.remove(0));
        }
    }

    for candidate in ["main", "master"] {
        if branch_exists(runner, candidate)? {
            return Ok(candidate.to_string());
        }
    }
    Ok("main".to_string())
}

/// List all local bookmarks (i.e. those without a non-git remote tracking).
///
/// Reuses vcs-runner's `parse_bookmark_output`. Filters to `RemoteStatus::Local`
/// to match the git impl's `for-each-ref refs/heads/` semantics.
pub(super) fn local_branches(runner: &dyn Runner) -> Result<Vec<String>> {
    let cwd = std::env::current_dir()?;
    match runner.run(
        Cmd::new("jj")
            .in_dir(&cwd)
            .args(["bookmark", "list", "-T", BOOKMARK_TEMPLATE]),
    ) {
        Ok(out) => {
            let parsed = parse_bookmark_output(&out.stdout_lossy());
            // Local-only OR synced/unsynced — match what `git
            // for-each-ref refs/heads/` returns (everything with a local
            // pointer, regardless of remote tracking). Filter only fully
            // remote-only bookmarks (which jj's bookmark list shouldn't
            // emit anyway since this template only walks normal_target).
            Ok(parsed
                .bookmarks
                .into_iter()
                .filter(|b| matches!(b.remote, RemoteStatus::Local | RemoteStatus::Synced | RemoteStatus::Unsynced))
                .map(|b| b.name)
                .collect())
        }
        // Outside a repo: empty list, matching git.
        Err(RunError::NonZeroExit { .. }) => Ok(Vec::new()),
        Err(e) => Err(map_run_err(e)),
    }
}

/// Check whether a bookmark exists in this workspace.
///
/// Uses the bookmark list (cached implicitly by the OS file cache) rather
/// than `jj log -r <name>` because the latter resolves through the full
/// revset language — including potentially-ambiguous prefix matching —
/// which would falsely succeed on partial change-id matches.
pub(super) fn branch_exists(runner: &dyn Runner, name: &str) -> Result<bool> {
    let bookmarks = local_branches(runner)?;
    Ok(bookmarks.iter().any(|b| b == name))
}

/// Rename a bookmark. `jj bookmark rename` is a direct equivalent.
pub(super) fn rename_branch(runner: &dyn Runner, old: &str, new: &str) -> Result<()> {
    super::exec(runner, &["bookmark", "rename", old, new])
}

/// Delete a bookmark. jj has no "force" flag — the `_force` parameter is
/// accepted for trait parity but is a no-op here. (jj's `bookmark delete`
/// is already safe: it just removes the local pointer; the commits remain
/// reachable via `change_id` and `op log`.)
pub(super) fn delete_branch(runner: &dyn Runner, name: &str, _force: bool) -> Result<()> {
    super::exec(runner, &["bookmark", "delete", name])
}
