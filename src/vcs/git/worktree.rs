// ===========================================================================
// vcs/git/worktree - Worktree CRUD + porcelain parser
// ===========================================================================

use std::path::{Path, PathBuf};

use vcs_git::GitApi;

use super::GitClient;
use crate::vcs::common::{CreateOutcome, WorktreeInfo, path_str};
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
/// CoW eligibility is decided by [`crate::cow::can_clone`]. When CoW isn't
/// possible we silently fall back to plain — no warnings, no errors.
pub(super) async fn create_worktree(
    git: &GitClient,
    cwd: &Path,
    path: &Path,
    branch: &str,
    base: &str,
) -> Result<CreateOutcome> {
    // Pre-flight `WorktreeExists` check applies to BOTH paths.
    let branch_already_exists = super::repo::branch_exists(git, cwd, branch).await?;
    if branch_already_exists {
        let worktrees = list_worktrees(git, cwd).await?;
        if worktrees.iter().any(|wt| wt.branch.as_deref() == Some(branch)) {
            return Err(Error::WorktreeExists(branch.to_string()));
        }
    }

    // CoW probe — requires both the repo root and `path`'s parent to be on
    // the same reflink-capable volume. The parent dir must already exist.
    let parent = path.parent().unwrap_or(path);
    if std::env::var(crate::cow::DISABLE_COW_ENV).is_err()
        && let Ok(repo_root) = super::repo::repo_root(git, cwd).await
        && parent.exists()
        && crate::cow::can_clone(&repo_root, parent)
    {
        return create_worktree_cow(git, cwd, &repo_root, path, branch, base, branch_already_exists)
            .await;
    }

    create_worktree_plain(cwd, path, branch, base, branch_already_exists).await
}

/// Create a worktree from a branch that exists only on `origin`: fetch
/// just that branch, then create the worktree from the remote-tracking
/// ref. Reuses [`create_worktree`] with `base = "origin/<branch>"`.
pub(super) async fn create_worktree_from_remote(
    git: &GitClient,
    cwd: &Path,
    path: &Path,
    branch: &str,
) -> Result<CreateOutcome> {
    eprintln!("  Fetching '{branch}' from origin...");
    super::ops::fetch_remote_branch(cwd, branch).await?;
    create_worktree(git, cwd, path, branch, &format!("origin/{branch}")).await
}

/// Standard `git worktree add` — git materialises the working copy.
async fn create_worktree_plain(
    cwd: &Path,
    path: &Path,
    branch: &str,
    base: &str,
    branch_already_exists: bool,
) -> Result<CreateOutcome> {
    let path_arg = path_str(path)?;
    eprintln!("  Running git worktree add...");
    if branch_already_exists {
        super::exec(cwd, ["worktree", "add", path_arg, branch]).await?;
    } else {
        super::exec(cwd, ["worktree", "add", "-b", branch, path_arg, base]).await?;
    }
    Ok(CreateOutcome::Plain)
}

/// Run `jj git import` in `repo_root`, best-effort: warn (don't fail) on a
/// non-zero exit or a spawn failure. Used to bracket the raw git operations
/// in the CoW flow so a colocated jj repo's view doesn't drift.
async fn jj_import_best_effort(repo_root: &Path, phase: &str) {
    // Drive jj through the typed `vcs-jj` client (job-backed runner).
    use vcs_jj::JjApi;
    let jj = vcs_jj::Jj::new();
    match jj.git_import(repo_root).await {
        Ok(()) => {}
        Err(processkit::Error::Spawn { .. }) => {
            eprintln!(
                "Warning: {phase} `jj git import` failed to spawn (is jj installed?); \
                 colocated jj state may drift from git"
            );
        }
        Err(e) => {
            eprintln!(
                "Warning: {phase} `jj git import` failed: {e} — jj-side refs may be \
                 stale after this operation; run `jj git import` manually if needed"
            );
        }
    }
}

/// Restore the source repo after the CoW flow: checkout BEFORE stash pop so the
/// pop applies to the right branch. `git stash pop` runs ONLY if the restoring
/// checkout succeeded — otherwise the user's stashed work would land on the
/// wrong branch with no signal. Returns whether the checkout restored cleanly.
async fn restore_source_repo(
    cwd: &Path,
    needs_restore: bool,
    restore_target: &str,
    needs_stash: bool,
    report: bool,
) -> bool {
    let t = std::time::Instant::now();
    let restored = if needs_restore {
        match super::exec(cwd, ["checkout", restore_target]).await {
            Ok(_) => true,
            Err(e) => {
                eprintln!("Warning: failed to restore '{restore_target}': {e}");
                false
            }
        }
    } else {
        true
    };
    if needs_stash {
        if restored {
            if let Err(e) = super::exec(cwd, ["stash", "pop"]).await {
                eprintln!(
                    "Warning: 'git stash pop' failed: {e}\n\
                     Your changes are saved in 'git stash list'; resolve manually."
                );
            }
        } else {
            eprintln!(
                "Warning: could not restore '{restore_target}', so your stashed \
                 changes were left untouched in 'git stash list'.\n\
                 Check out the right branch and run 'git stash pop' manually."
            );
        }
    }
    if report {
        eprintln!("  Restored source branch ({}).", crate::util::format_step(t.elapsed()));
    }
    restored
}

/// CoW creation: stash → checkout base → `worktree add --no-checkout`
/// → reflink-copy → restore. See module docstring for the rationale.
///
/// On any failure between stash and pop the source repo is restored to its
/// original state before the error propagates. The half-created worktree
/// (if any) is deleted and `git worktree prune` clears git's registry.
#[allow(clippy::too_many_arguments)]
async fn create_worktree_cow(
    git: &GitClient,
    cwd: &Path,
    repo_root: &Path,
    path: &Path,
    branch: &str,
    base: &str,
    branch_already_exists: bool,
) -> Result<CreateOutcome> {
    let path_arg = path_str(path)?;

    // 0. Colocated detection. When the repo has `.jj/` alongside `.git/`, the
    //    raw git operations below mutate git's HEAD/index without going through
    //    jj — desyncing jj's view. Bracket the whole CoW flow with `jj git
    //    import` so jj's bookmarks/refs catch up before and after. Best-effort:
    //    jj may not be installed (a colocated repo can travel between machines).
    let is_colocated = repo_root.join(".jj").is_dir();
    if is_colocated {
        jj_import_best_effort(repo_root, "pre-CoW").await;
    }

    // 1. Capture source state — branch name AND commit hash. When the branch
    //    name is "HEAD" the repo is detached, and `git checkout HEAD` is a
    //    no-op; restore via the captured commit hash in that case.
    let orig_branch = super::repo::current_branch(git, cwd).await?;
    let orig_commit = super::repo::current_commit(git, cwd).await?;
    let is_detached = orig_branch == "HEAD";
    let needs_stash = super::branch::has_uncommitted_changes(git, cwd).await?;

    #[cfg(windows)]
    eprintln!("  Using ReFS block clone...");
    #[cfg(not(windows))]
    eprintln!("  Using CoW (reflink) clone...");

    // 2. Stash if dirty. `-u` includes untracked. The message embeds PID +
    //    nanosecond timestamp so multiple failed runs leave distinguishable
    //    `git stash list` entries.
    let stash_message = format!(
        "ws-cow-create-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    if needs_stash {
        let t = std::time::Instant::now();
        super::exec(cwd, ["stash", "push", "-u", "-m", &stash_message]).await?;
        eprintln!("  Stashed uncommitted changes ({}).", crate::util::format_step(t.elapsed()));
    }

    // Steps 3-5 with explicit rollback on any error. The source repo must
    // present, for the reflink, the SAME tree the new worktree's HEAD points at.
    let mut moved_source = false;
    let inner: Result<()> = async {
        // 3. Put the source working tree on the right commit.
        if branch_already_exists {
            // Resolve the BRANCH ref explicitly (`refs/heads/<branch>`), not the
            // bare name: a tag sharing the name would otherwise win git's
            // ref-precedence and resolve to the wrong commit.
            let branch_commit =
                super::repo::resolve_commit(git, cwd, &format!("refs/heads/{branch}")).await?;
            if orig_commit != branch_commit {
                let t = std::time::Instant::now();
                super::exec(cwd, ["checkout", "--detach", &branch_commit]).await?;
                moved_source = true;
                eprintln!("  Switched to '{branch}' ({}).", crate::util::format_step(t.elapsed()));
            }
        } else if orig_branch != base {
            let t = std::time::Instant::now();
            super::exec(cwd, ["checkout", base]).await?;
            moved_source = true;
            eprintln!("  Switched to '{base}' ({}).", crate::util::format_step(t.elapsed()));
        }

        // 4. Create worktree with --no-checkout. git creates the `.git` gitlink
        //    file at `path` and registers the worktree, but writes no
        //    working-tree files.
        let t = std::time::Instant::now();
        if branch_already_exists {
            super::exec(cwd, ["worktree", "add", "--no-checkout", path_arg, branch]).await?;
        } else {
            super::exec(cwd, ["worktree", "add", "--no-checkout", "-b", branch, path_arg, base])
                .await?;
        }
        eprintln!("  Created workspace skeleton ({}).", crate::util::format_step(t.elapsed()));

        // 5. Reflink-copy every file/dir from repo root to `path`, except
        //    `.git/`. The copy is synchronous (rayon + reflink IOCTLs / robocopy)
        //    and CPU/IO heavy, so it runs on a blocking thread to keep the async
        //    executor free.
        let repo_root_owned = repo_root.to_path_buf();
        let path_owned = path.to_path_buf();
        let copied_bytes = match tokio::task::spawn_blocking(move || {
            crate::cow::try_clone_dir_except(&repo_root_owned, &path_owned, &[".git"])
        })
        .await
        .expect("reflink clone task panicked")
        {
            Ok(bytes) => bytes,
            Err(e) => {
                // CoW failed mid-walk. Remove the half-populated worktree dir and
                // prune git's registry. Preserve the structured cause via Error::Cow.
                let _ = std::fs::remove_dir_all(path);
                let _ = super::exec(cwd, ["worktree", "prune"]).await;
                return Err(Error::Cow(e));
            }
        };

        // 6. Reconcile the new worktree's index with the files on disk. After
        //    `--no-checkout` the index is empty and files were materialised
        //    outside git's view, so `git status` would report every file as
        //    both staged-deletion and untracked. `read-tree HEAD` +
        //    `update-index --refresh` fixes that. Synchronous (drives progress
        //    bars / batches), so it runs on a blocking thread.
        let path_for_refresh = path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            refresh_index_with_progress(&path_for_refresh, copied_bytes)
        })
        .await
        .expect("index refresh task panicked")?;

        Ok(())
    }
    .await;

    // Restore source repo. Detached HEAD restores via the captured commit hash.
    let restore_target = if is_detached { orig_commit.as_str() } else { orig_branch.as_str() };
    let needs_restore = moved_source;

    let mut source_restored = true;
    if inner.is_ok() && (needs_restore || needs_stash) {
        source_restored =
            restore_source_repo(cwd, needs_restore, restore_target, needs_stash, true).await;
    } else if inner.is_err() {
        // Failure path: still try to restore so the repo isn't left half-broken.
        source_restored =
            restore_source_repo(cwd, needs_restore, restore_target, needs_stash, false).await;
    }

    // 7. Colocated post-sync. Skip when the source restore failed — importing a
    //    known-broken intermediate git state would record that into jj's view.
    if is_colocated && source_restored {
        jj_import_best_effort(repo_root, "post-CoW").await;
    } else if is_colocated && !source_restored {
        eprintln!(
            "Warning: skipped post-CoW `jj git import` because the source repo \
             could not be restored; fix git state, then run `jj git import` manually."
        );
    }

    inner?;
    Ok(CreateOutcome::CowCloned)
}

/// Strategy threshold: above this many bytes copied, the index refresh switches
/// to batched-stdin mode (real progress bar) instead of the single-process
/// spinner. Below it the single process finishes fast enough that the
/// spinner-only path is the better UX.
const INDEX_REFRESH_BATCHED_THRESHOLD: u64 = 2 * 1024 * 1024 * 1024; // 2 GiB

/// Run `git read-tree HEAD` to populate the index, then refresh stat info for
/// every entry. Synchronous (uses `std::process` directly) so it can run inside
/// `spawn_blocking` with its progress bars / batches intact. Two paths:
///
///   - **Small repos (< 2 GiB copied)**: a single `git update-index --refresh
///     -q`, fronted by an elapsed-time spinner.
///   - **Large repos (≥ 2 GiB copied)**: read the entry list once
///     (`git ls-files -z`), then run `git update-index --stdin -z --refresh -q`
///     in batches, incrementing a real progress bar per batch.
fn refresh_index_with_progress(path: &Path, copied_bytes: u64) -> Result<()> {
    use std::process::Command;
    use std::time::Instant;

    // Step 1: populate index from HEAD. Fast (one file write, no per-entry I/O).
    let status = Command::new("git")
        .current_dir(path)
        .args(["read-tree", "HEAD"])
        .status()
        .map_err(|e| Error::Command(format!("spawn git read-tree: {e}")))?;
    if !status.success() {
        return Err(Error::Command(format!(
            "git read-tree exited with {}",
            status.code().unwrap_or(-1)
        )));
    }

    let started = Instant::now();
    if copied_bytes < INDEX_REFRESH_BATCHED_THRESHOLD {
        refresh_index_spinner(path, started)
    } else {
        refresh_index_batched(path, started)
    }
}

/// Small-repo path: one `git update-index --refresh -q` with a spinner ticking
/// elapsed seconds while it runs.
fn refresh_index_spinner(path: &Path, started: std::time::Instant) -> Result<()> {
    use indicatif::{ProgressBar, ProgressStyle};
    use std::process::Command;
    use std::time::Duration;

    // Cheap entry count for the user-facing heading.
    let ls_out = Command::new("git")
        .current_dir(path)
        .args(["ls-files"])
        .output()
        .map_err(|e| Error::Command(format!("spawn git ls-files: {e}")))?;
    let total_entries = String::from_utf8_lossy(&ls_out.stdout).lines().count() as u64;

    eprintln!("  Refreshing git index ({total_entries} entries)...");

    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::with_template("  {spinner:.cyan} {msg}")
            .unwrap()
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ "),
    );
    pb.set_message("0s elapsed");
    pb.enable_steady_tick(Duration::from_millis(80));

    let pb_for_thread = pb.clone();
    let stop_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_flag_for_thread = std::sync::Arc::clone(&stop_flag);
    let ticker = std::thread::spawn(move || {
        while !stop_flag_for_thread.load(std::sync::atomic::Ordering::Relaxed) {
            let elapsed = started.elapsed().as_secs();
            pb_for_thread.set_message(format!("{elapsed}s elapsed"));
            std::thread::sleep(Duration::from_millis(500));
        }
    });

    // Non-zero exit is expected (every entry needs updating after read-tree
    // zero-stat'd them), so ignore the status.
    let _ = Command::new("git")
        .current_dir(path)
        .args(["update-index", "--refresh", "-q"])
        .status();

    stop_flag.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = ticker.join();
    pb.finish_and_clear();
    let elapsed = started.elapsed().as_secs();
    eprintln!("  Refreshed {total_entries} entries in {elapsed}s.");
    Ok(())
}

/// Large-repo path: batched `update-index --stdin --refresh` invocations with a
/// real progress bar incrementing after each batch. Uses raw stdin piping and
/// raw byte output from `ls-files` (paths can be non-UTF-8 on Unix).
fn refresh_index_batched(path: &Path, started: std::time::Instant) -> Result<()> {
    use indicatif::{ProgressBar, ProgressStyle};
    use std::io::Write;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    // 5000 entries per batch ≈ 60 progress updates on a 300k-entry repo.
    const BATCH_SIZE: usize = 5000;

    let ls_output = Command::new("git")
        .current_dir(path)
        .args(["ls-files", "-z"])
        .output()
        .map_err(|e| Error::Command(format!("spawn git ls-files: {e}")))?;
    if !ls_output.status.success() {
        return Err(Error::Command(format!(
            "git ls-files exited with {}",
            ls_output.status.code().unwrap_or(-1)
        )));
    }
    let all_paths: Vec<&[u8]> = ls_output
        .stdout
        .split(|&b| b == 0)
        .filter(|p| !p.is_empty())
        .collect();
    let total_entries = all_paths.len() as u64;

    eprintln!("  Refreshing git index ({total_entries} entries, batched)...");

    let pb = ProgressBar::new(total_entries);
    pb.set_style(
        ProgressStyle::with_template(
            "  {spinner:.cyan} [{wide_bar:.cyan/blue}] {pos}/{len} entries ({percent}%) | ETA {eta}",
        )
        .unwrap()
        .progress_chars("█▉▊▋▌▍▎▏ ")
        .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ "),
    );
    pb.enable_steady_tick(Duration::from_millis(80));

    for chunk in all_paths.chunks(BATCH_SIZE) {
        let mut child = Command::new("git")
            .current_dir(path)
            .args(["update-index", "--stdin", "-z", "--refresh", "-q"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| Error::Command(format!("spawn git update-index: {e}")))?;

        {
            let mut stdin = child.stdin.take().expect("stdin piped");
            for &path_bytes in chunk {
                stdin.write_all(path_bytes).ok();
                stdin.write_all(&[0]).ok();
            }
            // stdin dropped here, closing the pipe → EOF signal to git.
        }

        // update-index --refresh returns non-zero whenever any entry needed
        // updating (always, after read-tree zero-stat'd everything), so ignore it.
        let _ = child.wait();
        pb.inc(chunk.len() as u64);
    }

    pb.finish_and_clear();
    let elapsed = started.elapsed().as_secs();
    eprintln!("  Refreshed {total_entries} entries in {elapsed}s.");
    Ok(())
}

/// Remove a worktree.
pub(super) async fn remove_worktree(cwd: &Path, path: &Path, force: bool) -> Result<()> {
    let path_arg = path_str(path)?;
    if force {
        super::exec(cwd, ["worktree", "remove", "--force", path_arg]).await
    } else {
        super::exec(cwd, ["worktree", "remove", path_arg]).await
    }
}

/// Move a worktree to a new path.
pub(super) async fn move_worktree(cwd: &Path, old_path: &Path, new_path: &Path) -> Result<()> {
    super::exec(cwd, ["worktree", "move", path_str(old_path)?, path_str(new_path)?]).await
}

/// List all worktrees attached to the repo.
pub(super) async fn list_worktrees(git: &GitClient, cwd: &Path) -> Result<Vec<WorktreeInfo>> {
    let worktrees = git.worktree_list(cwd).await.map_err(|e| match e {
        processkit::Error::Exit { .. } => Error::NotInRepo,
        other => super::errmap::map_pk_err(other),
    })?;
    Ok(worktrees
        .into_iter()
        .map(|wt| WorktreeInfo {
            path: wt.path,
            branch: wt.branch,
            commit: wt.head,
            is_bare: wt.bare,
        })
        .collect())
}

/// Parse `git worktree list --porcelain` output.
///
/// Kept as a pure helper (re-exported for tests) even though the typed client
/// now parses worktree listings internally.
///
/// **This parser is git-specific** — jj's `jj workspace list` uses an entirely
/// different schema.
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
