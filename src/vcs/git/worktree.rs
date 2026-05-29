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

/// Create a worktree from a branch that exists only on `origin`: fetch
/// just that branch, then create the worktree from the remote-tracking
/// ref. Reuses [`create_worktree`] with `base = "origin/<branch>"`; since
/// `<branch>` doesn't exist locally, that takes the new-branch path
/// (`git worktree add -b <branch> <path> origin/<branch>`), which mints a
/// local `<branch>` tracking the remote.
pub(super) fn create_worktree_from_remote(
    runner: &dyn Runner,
    path: &Path,
    branch: &str,
) -> Result<CreateOutcome> {
    eprintln!("  Fetching '{branch}' from origin...");
    super::ops::fetch_remote_branch(runner, branch)?;
    create_worktree(runner, path, branch, &format!("origin/{branch}"))
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
    eprintln!("  Running git worktree add...");
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

    // Windows uses CopyFileExW via robocopy which transparently
    // block-clones on ReFS. Linux/macOS use the explicit reflink IOCTLs
    // (ioctl_ficlone / clonefile). Same outcome, different surfacing.
    #[cfg(windows)]
    eprintln!("  Using ReFS block clone...");
    #[cfg(not(windows))]
    eprintln!("  Using CoW (reflink) clone...");

    // 2. Stash if dirty. `-u` includes untracked.
    //
    // The stash message includes both PID and a nanosecond timestamp so
    // multiple failed runs in the same shell session leave distinguishable
    // entries in `git stash list` — without the timestamp, a user who
    // hits two consecutive CoW failures would see two stashes with
    // identical names and have to drop them by index alone.
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
        super::exec(runner, &["stash", "push", "-u", "-m", &stash_message])?;
        eprintln!(
            "  Stashed uncommitted changes ({}).",
            crate::util::format_step(t.elapsed())
        );
    }

    // The inner closure handles steps 3-5 with explicit rollback on any
    // error. We always restore the source repo (step 6) afterwards, even
    // on success — current_branch may have differed from base.
    let inner: Result<()> = (|| {
        // 3. Checkout base if not already there.
        if orig_branch != base {
            let t = std::time::Instant::now();
            super::exec(runner, &["checkout", base])?;
            eprintln!(
                "  Switched to '{base}' ({}).",
                crate::util::format_step(t.elapsed())
            );
        }

        // 4. Create worktree with --no-checkout. git creates the `.git`
        // gitlink file at `path` and registers the worktree, but doesn't
        // write any working-tree files.
        let t = std::time::Instant::now();
        let add_result = if branch_already_exists {
            super::exec(runner, &["worktree", "add", "--no-checkout", path_arg, branch])
        } else {
            super::exec(
                runner,
                &["worktree", "add", "--no-checkout", "-b", branch, path_arg, base],
            )
        };
        add_result?;
        eprintln!(
            "  Created workspace skeleton ({}).",
            crate::util::format_step(t.elapsed())
        );

        // 5. Reflink-copy every file/dir from repo root to `path`, except
        // `.git/` (which `--no-checkout` already created as a gitlink).
        // `try_clone_dir_except` prints its own scan-spinner + progress bar
        // and a "Cloned N files (X GB) via reflink." summary on completion.
        let copied_bytes = match crate::cow::try_clone_dir_except(repo_root, path, &[".git"]) {
            Ok(bytes) => bytes,
            Err(e) => {
                // CoW failed mid-walk. Remove the half-populated worktree dir
                // and run `git worktree prune` so git's registry stays clean.
                // Preserve the structured `cow::Error` via `Error::Cow` so
                // callers/tests can match the underlying cause.
                let _ = std::fs::remove_dir_all(path);
                let _ = super::exec(runner, &["worktree", "prune"]);
                return Err(Error::Cow(e));
            }
        };

        // 6. Reconcile the new worktree's index with the actual files on
        //    disk. `git worktree add --no-checkout` leaves the worktree's
        //    INDEX empty — it skips the index-population step that the
        //    default checkout does. Then we materialise files outside of
        //    git's view (via reflink / robocopy / fs::copy). Result:
        //    HEAD has all files, index is empty, working tree has all
        //    files — and `git status` reports every file as both
        //    "staged deletion" (HEAD → index) and "untracked"
        //    (working-tree → index).
        //
        //    `git read-tree HEAD` populates the index with HEAD's tree
        //    (no I/O on file contents, just metadata). Then
        //    `git update-index --refresh -q` walks the index, stats
        //    every file, and records the actual mtime/size/inode so
        //    `git status` knows the working tree is in sync. `-q`
        //    suppresses the "needs update" lines that --refresh
        //    normally prints for every entry it touches. The exit code
        //    is non-zero whenever any entry needed updating, which is
        //    expected here (every entry will), so we ignore it.
        //
        //    Cost: O(num_files) stat() calls, dominated by the same FS
        //    metadata bandwidth that the copy itself uses. For small
        //    repos this is a quick spinner; for >2 GiB repos the helper
        //    switches to batched `--stdin` mode that drives a real
        //    progress bar (see helper).
        refresh_index_with_progress(runner, path, copied_bytes)?;

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
    let needs_restore = is_detached || orig_branch != base;
    if inner.is_ok() && (needs_restore || needs_stash) {
        let t = std::time::Instant::now();
        if needs_restore
            && let Err(e) = super::exec(runner, &["checkout", restore_target])
        {
            eprintln!("Warning: failed to restore '{restore_target}': {e}");
        }
        if needs_stash
            && let Err(e) = super::exec(runner, &["stash", "pop"])
        {
            // Stash pop conflict is the worst-case: user's changes are
            // safe in `git stash list` but require manual resolution.
            eprintln!(
                "Warning: 'git stash pop' failed: {e}\n\
                 Your changes are saved in 'git stash list'; resolve manually."
            );
        }
        eprintln!(
            "  Restored source branch ({}).",
            crate::util::format_step(t.elapsed())
        );
    } else if !inner.is_ok() {
        // Failure path: still try to restore so the user's repo isn't
        // left in a half-broken state, but don't time/report — they
        // have bigger problems.
        if needs_restore {
            let _ = super::exec(runner, &["checkout", restore_target]);
        }
        if needs_stash {
            let _ = super::exec(runner, &["stash", "pop"]);
        }
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

/// Strategy threshold: above this many bytes copied, the index refresh
/// switches to the batched-stdin mode (real progress bar, real cost in
/// per-batch process spawns) instead of the single-process spinner. On
/// repos smaller than this the single process finishes fast enough that
/// the spinner-only path is the better UX — no point eating ~3 s of
/// spawn overhead to show progress for a 5-second refresh.
const INDEX_REFRESH_BATCHED_THRESHOLD: u64 = 2 * 1024 * 1024 * 1024; // 2 GiB

/// Run `git read-tree HEAD` to populate the index, then refresh stat
/// info for every entry. Two implementations:
///
///   - **Small repos (< 2 GiB copied)**: single `git update-index
///     --refresh -q` invocation, fronted by an elapsed-time spinner.
///     Cheap, simple, finishes in 1-5 s.
///
///   - **Large repos (≥ 2 GiB copied)**: read the entry list once
///     (`git ls-files -z`), then run `git update-index --stdin -z
///     --refresh -q` in batches of `BATCH_SIZE` entries — feeding each
///     batch's paths via stdin and incrementing a real progress bar
///     after each batch returns. Spawn overhead is ~50 ms × 60 batches
///     ≈ 3 s extra wall time on a 300k-entry CargoWise repo, which is
///     a fair price for visible progress across what would otherwise be
///     a silent 2-minute step.
///
/// **Why not parse `update-index --verbose` stdout for progress in the
/// single-process path**: tried it in v0.13.11 and confirmed it doesn't
/// work on Windows. When git's stdout is piped (rather than connected
/// to a TTY) MSVCRT puts the stream into full-block buffering, so
/// per-entry verbose lines pile up in the buffer and only flush when
/// git exits — bar stays at 0% for the entire refresh, jumps to 100%
/// at the end. Real per-entry progress on a single process would need
/// a PTY allocation. The batched-stdin approach gets us progress at the
/// granularity of batches (5 k entries each) without any extra deps —
/// good enough.
fn refresh_index_with_progress(runner: &dyn Runner, path: &Path, copied_bytes: u64) -> Result<()> {
    use std::time::Instant;

    // Step 1: populate index from HEAD. Fast (one file write, no
    // per-entry I/O), no progress needed.
    runner
        .run(Cmd::new("git").in_dir(path).args(["read-tree", "HEAD"]))
        .map(|_| ())
        .map_err(map_run_err)?;

    let started = Instant::now();
    if copied_bytes < INDEX_REFRESH_BATCHED_THRESHOLD {
        refresh_index_spinner(runner, path, started)
    } else {
        refresh_index_batched(path, started)
    }
}

/// Small-repo path: one `git update-index --refresh -q` call with a
/// spinner ticking elapsed seconds while it runs.
fn refresh_index_spinner(runner: &dyn Runner, path: &Path, started: std::time::Instant) -> Result<()> {
    use indicatif::{ProgressBar, ProgressStyle};
    use std::time::Duration;

    // Cheap entry count for the user-facing heading; we don't need
    // it to drive the spinner itself.
    let ls_out = runner
        .run(Cmd::new("git").in_dir(path).args(["ls-files"]))
        .map_err(map_run_err)?;
    let total_entries = ls_out.stdout_lossy().lines().count() as u64;

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

    let _ = runner.run(
        Cmd::new("git")
            .in_dir(path)
            .args(["update-index", "--refresh", "-q"]),
    );

    stop_flag.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = ticker.join();
    pb.finish_and_clear();
    let elapsed = started.elapsed().as_secs();
    eprintln!("  Refreshed {total_entries} entries in {elapsed}s.");
    Ok(())
}

/// Large-repo path: batched `update-index --stdin --refresh` invocations
/// with a real progress bar incrementing after each batch.
///
/// Bypasses `runner` because we need direct stdin access (the runner
/// API doesn't expose it) and we want raw byte output from `ls-files`
/// (paths can contain bytes that aren't valid UTF-8 on Unix).
fn refresh_index_batched(path: &Path, started: std::time::Instant) -> Result<()> {
    use indicatif::{ProgressBar, ProgressStyle};
    use std::io::Write;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    // Tuning: 5000 entries per batch gives ~60 progress updates on the
    // CargoWise 300k-entry repo (one every ~2 s during the ~2-minute
    // run). Smaller batches = smoother progress + more spawn overhead;
    // larger batches = chunkier progress + less overhead. 5 k is the
    // empirical sweet spot.
    const BATCH_SIZE: usize = 5000;

    // Read entry list with raw bytes (paths can be non-UTF-8 on Unix).
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
        // Spawn one `git update-index --stdin -z --refresh -q` per
        // batch. Each invocation reads paths from stdin (NUL-separated
        // via `-z`), refreshes their stat info, writes the index, and
        // exits. We swallow stderr because update-index emits warnings
        // for entries that "needed update" — every entry needs update
        // after read-tree zero-stat'd them, so the warnings would be
        // 5000 lines of noise per batch.
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
                // `.ok()` because if the child died early (e.g. SIGPIPE)
                // we'd rather move on than abort the whole refresh.
                stdin.write_all(path_bytes).ok();
                stdin.write_all(&[0]).ok();
            }
            // stdin dropped here, closing the pipe → EOF signal to git.
        }

        // update-index --refresh returns non-zero whenever any entry
        // needed updating (i.e. always after read-tree zero-stat'd
        // everything), so the status is uninformative for our purposes.
        let _ = child.wait();
        pb.inc(chunk.len() as u64);
    }

    pb.finish_and_clear();
    let elapsed = started.elapsed().as_secs();
    eprintln!("  Refreshed {total_entries} entries in {elapsed}s.");
    Ok(())
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
