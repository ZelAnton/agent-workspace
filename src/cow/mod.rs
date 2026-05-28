// ===========================================================================
// cow - Copy-on-Write directory cloning for fast worktree creation
// ===========================================================================
//
// On filesystems that support block cloning (Windows ReFS / DevDrive, Linux
// Btrfs/XFS, macOS APFS), `reflink-copy` exposes a single `reflink_or_copy`
// API that performs a Copy-on-Write clone when source and destination share
// a volume. The clone is near-instant regardless of file size, and the new
// file initially shares all extents with the source — disk usage grows only
// as either side diverges.
//
// We use this for `ws new` to replace the slow `git worktree add` checkout
// of large monorepos with a fast `git worktree add --no-checkout` plus a
// reflink-based bulk copy of every file (sans `.git/`).
//
// **Same-volume probe** is mandatory: cross-volume reflinks return errors
// that vary per platform and aren't reliable signals. We pre-check that
// source and destination resolve to the same volume via platform-specific
// device IDs (Win32 `dwVolumeSerialNumber` / Unix `st_dev`), cache the
// result per (src_vol, dst_vol) pair, and only then attempt CoW.

use std::path::Path;

mod detect;
#[cfg(test)]
mod tests;

pub use detect::same_volume;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("walk error: {0}")]
    Walk(#[from] ignore::Error),
}

/// Env var that disables the CoW worktree-creation path even when the
/// filesystem supports it. Set by `ws new --no-cow` or by config
/// (`[create] use_cow = false`) before any dispatcher fires.
///
/// Exposed as a const so all readers (dispatchers in `vcs::git` and
/// `vcs::jj`) and the writer (`cli::commands::lifecycle::new`) share
/// one source of truth — silent string-literal drift between modules
/// would otherwise be a silent-disable risk.
pub const DISABLE_COW_ENV: &str = "WT_DISABLE_COW";

/// True if `src_dir` and `dst_parent` are on the same volume **and** a
/// sentinel reflink attempt succeeds at `dst_parent`.
///
/// We probe with a real reflink (zero-byte temp file → reflink → delete)
/// because:
///   - Volume serial equality alone doesn't guarantee the FS supports
///     block cloning (NTFS doesn't, even when src/dst share the same
///     drive letter).
///   - `reflink-copy`'s `ReflinkSupport` query isn't available on all
///     versions of the crate; the probe is portable.
///
/// `dst_parent` must exist (we don't create it). Caller typically passes
/// the workspace dir under `$AGENT_WORKSPACE_DIR/<id>/` (pre-v0.13.6
/// installs used `$AGENT_WORKSPACE_DIR/workspaces/<id>/`).
pub fn can_clone(src_dir: &Path, dst_parent: &Path) -> bool {
    if !same_volume(src_dir, dst_parent).unwrap_or(false) {
        return false;
    }

    // Sentinel probe: create a tiny source file under dst_parent (same
    // volume, definitely reflinkable if FS supports it), reflink it to
    // another path, then clean up. We use dst_parent for both to avoid
    // poking at src_dir's actual contents (which may be read-only).
    //
    // Both temp files use `tempfile::Builder` (random suffix) — not
    // process-id-based names — so concurrent invocations from the same
    // PID don't collide on the destination path. The previous PID-only
    // scheme would clobber a sibling probe in rapid-succession runs.
    let probe_src = match tempfile::Builder::new()
        .prefix(".ws-cow-probe-src-")
        .tempfile_in(dst_parent)
    {
        Ok(f) => f,
        Err(_) => return false,
    };
    let probe_dst = match tempfile::Builder::new()
        .prefix(".ws-cow-probe-dst-")
        .tempfile_in(dst_parent)
    {
        Ok(f) => f,
        Err(_) => return false,
    };
    let probe_dst_path = probe_dst.path().to_path_buf();
    // tempfile creates an EMPTY file at `probe_dst_path`. Reflink needs
    // the dest to not exist (or be replaceable). Drop the handle (which
    // would delete on Drop) AFTER reflink — but we need an absent dest
    // first. Convert NamedTempFile to TempPath then `close()` to delete
    // without keeping the path reservation.
    let probe_dst_path = match probe_dst.into_temp_path().keep() {
        Ok(p) => {
            // Keep returns the path but doesn't auto-delete. Remove the
            // empty file so reflink has room to write.
            let _ = std::fs::remove_file(&p);
            p
        }
        Err(_) => probe_dst_path,
    };
    let _cleanup = Cleanup(&probe_dst_path);

    reflink_copy::reflink(probe_src.path(), &probe_dst_path).is_ok()
}

struct Cleanup<'a>(&'a Path);
impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.0);
    }
}

/// Build the `ignore::Walk` used by both the scan and copy passes of
/// [`try_clone_dir_except`]. Single source of truth for filter rules —
/// the scan pass MUST visit the same set the copy pass will, otherwise
/// the total-bytes denominator would diverge from the actual work and the
/// progress bar would over- or under-shoot.
/// Build the cow walker.
///
/// Two pattern sources, both fed into a single `Gitignore` matcher:
///
///   - `hardcoded_excludes`: anchored top-level names the caller
///     always wants skipped — `.git` (always), `.jj` (colocated jj).
///     We promote each entry `X` to the anchored pattern `/X` so it
///     only matches the top-level dir, mirroring the pre-v0.13.18
///     behaviour exactly.
///   - `user_patterns`: gitignore-style patterns from
///     `[copy] exclude` in the project config — `target`,
///     `/node_modules`, `**/*.iso`, `!keep-this`, etc.
///
/// Both feed `ignore::gitignore::GitignoreBuilder`. A single
/// `Match::Ignore` check in `filter_entry` decides skip/keep,
/// supporting:
///   - Anchored matches (`/target`) — top-level only.
///   - Unanchored matches (`target`) — any depth.
///   - Globs (`**/*.iso`, `*.tmp`).
///   - Negations (`!keep-this`) — re-include after a broader rule.
fn build_clone_walker(
    src: &Path,
    hardcoded_excludes: &[&str],
    user_patterns: &[String],
) -> ignore::Walk {
    use ignore::WalkBuilder;
    use ignore::gitignore::GitignoreBuilder;

    let mut gi = GitignoreBuilder::new(src);
    // Anchor each hardcoded exclude to repo root so e.g. `.git` only
    // matches the top-level `.git` (existing semantics) — not a
    // nested `.git` inside a submodule or an asset directory.
    for &name in hardcoded_excludes {
        let _ = gi.add_line(None, &format!("/{name}"));
    }
    // User patterns added verbatim; their leading `/` (if any) is
    // honoured by gitignore semantics, anchoring to repo root.
    for pat in user_patterns {
        let _ = gi.add_line(None, pat);
    }
    // If pattern compilation fails we fall back to an empty matcher
    // (= nothing excluded). Better than aborting the entire copy on
    // a syntax slip in user config.
    let matcher = gi
        .build()
        .unwrap_or_else(|_| GitignoreBuilder::new(src).build().expect("empty gitignore builds"));

    WalkBuilder::new(src)
        // Walk everything — including hidden files. We want a complete
        // mirror, not a gitignore-filtered subset (our own matcher
        // above is the only source of skips).
        .standard_filters(false)
        .hidden(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        // Don't follow symlinks; copy them as-is below.
        .follow_links(false)
        .filter_entry(move |entry| {
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            !matcher.matched(entry.path(), is_dir).is_ignore()
        })
        .build()
}

/// One planned filesystem operation captured during the scan pass.
///
/// Collecting these up-front (rather than walking twice) means:
///   - The scan pass already knows every byte we need to copy → accurate
///     progress bar denominator.
///   - The copy pass can be driven by `rayon::par_iter` over a `Vec`,
///     which is trivially parallelisable without re-entering the walker.
enum PlannedOp {
    Dir(std::path::PathBuf),
    File {
        src: std::path::PathBuf,
        dst: std::path::PathBuf,
        size: u64,
    },
    Symlink {
        src: std::path::PathBuf,
        dst: std::path::PathBuf,
    },
}

/// Outcome counters for a single copy phase.
///
/// On non-Windows we use `reflink_copy::reflink_or_copy` and distinguish
/// `reflinked` (Ok(None)) vs `copied` (Ok(Some(_))) — important for
/// verifying that the FS block-clone fast path is actually firing.
///
/// On Windows we use `std::fs::copy` (which calls `CopyFileExW`). Modern
/// Windows 11 24H2 / Server 2025 transparently uses block-clone in
/// CopyFileEx when source and destination are on the same ReFS volume,
/// and it does so 2-5× faster than the manual FSCTL_DUPLICATE_EXTENTS
/// IOCTL approach the reflink-copy crate uses. We can't externally
/// distinguish "block-cloned" from "byte-copied" — the kernel makes that
/// choice transparently — so the `reflinked` counter stays at zero on
/// Windows and the `copied` count is the total.
#[derive(Default)]
struct CopyStats {
    reflinked: std::sync::atomic::AtomicU64,
    copied: std::sync::atomic::AtomicU64,
    errors: std::sync::atomic::AtomicU64,
    done_files: std::sync::atomic::AtomicU64,
    done_bytes: std::sync::atomic::AtomicU64,
}

/// Per-file copy primitive — platform-specific dispatch.
///
/// **Windows**: `std::fs::copy` → `CopyFileExW`. On modern Windows + ReFS
/// this transparently uses block-clone. On a 300k-file / 13 GB CargoWise
/// repo this is ~5× faster than `reflink_copy::reflink_or_copy` because:
///   1. CopyFileExW is a single high-level kernel call vs reflink-copy's
///      7-IOCTL dance (open + stat + create + set_sparse +
///      get_integrity_information + set_integrity_information + set_len
///      + FSCTL_DUPLICATE_EXTENTS_TO_FILE).
///   2. On Windows 11 24H2 / Server 2025, CopyFileEx's auto-block-clone
///      path is the same kernel code as the manual IOCTL, just without
///      per-file syscall overhead.
///
/// **Linux/macOS**: `reflink_copy::reflink_or_copy` → `ioctl_ficlone` /
/// `clonefile`. `std::fs::copy` on these platforms does a plain byte
/// copy with NO auto-clone, so the explicit reflink API is required to
/// get block-clone behaviour on btrfs/xfs/APFS.
#[cfg(windows)]
fn copy_file(src: &Path, dst: &Path, stats: &CopyStats) -> bool {
    use std::sync::atomic::Ordering;
    match std::fs::copy(src, dst) {
        Ok(_) => {
            stats.copied.fetch_add(1, Ordering::Relaxed);
            true
        }
        Err(_) => {
            stats.errors.fetch_add(1, Ordering::Relaxed);
            false
        }
    }
}

#[cfg(not(windows))]
fn copy_file(src: &Path, dst: &Path, stats: &CopyStats) -> bool {
    use std::sync::atomic::Ordering;
    match reflink_copy::reflink_or_copy(src, dst) {
        Ok(None) => {
            stats.reflinked.fetch_add(1, Ordering::Relaxed);
            true
        }
        Ok(Some(_)) => {
            stats.copied.fetch_add(1, Ordering::Relaxed);
            true
        }
        Err(_) => {
            stats.errors.fetch_add(1, Ordering::Relaxed);
            false
        }
    }
}

/// Recursively clone every file/dir from `src` into `dst`, skipping any
/// top-level directory whose name matches an entry in `excludes`.
///
/// Each regular file is cloned via `reflink_or_copy` (per-file fallback
/// to plain copy if the kernel rejects the reflink — graceful). Symlinks
/// are NOT followed (security: mirrors the `copy_files` policy in
/// `src/cli/commands/lifecycle/new.rs::copy_files`).
///
/// `dst` must exist before the call (use `fs::create_dir_all` upstream).
/// Excludes are matched against the immediate child name only — nested
/// directories named `.git` deeper in the tree are NOT skipped (mirrors
/// git's worktree semantics: only the root `.git` is special).
///
/// **Architecture** — three phases for accurate ETA and good throughput
/// on large monorepos:
///
///   1. **Scan** (single-threaded walk): visit every entry, record paths,
///      sizes, and types into a `Vec<PlannedOp>`. Spinner shows live
///      "N files, M GB" so the user sees progress even during this
///      metadata-only pass. On a 20+ GB tree this takes a few seconds.
///
///   2. **Create dirs** (serial): walk the planned ops and `create_dir_all`
///      every `PlannedOp::Dir` so parallel file copies in phase 3 don't
///      race on parent-dir creation. `create_dir_all` is idempotent — we
///      also call it defensively before each file copy — but doing it
///      once up-front cuts down on duplicate syscalls.
///
///   3. **Parallel copy** (rayon): `par_iter` over the planned files +
///      symlinks. Each task calls `reflink_copy::reflink_or_copy` and
///      classifies the result into `reflinked` / `copied` / `error`
///      counters. The progress bar is shared via `Arc` and updated under
///      its internal lock — `indicatif::ProgressBar` is `Sync`.
///
/// Parallelisation matters: even when the underlying ReFS block-clone
/// FSCTL is near-instant, the per-file path (open src, stat, create dst,
/// `set_sparse`, `get_integrity_information`, `set_len`, FSCTL) is 5–7
/// syscalls. Sequential at ~0.5 ms per file × 100k files ≈ 50 seconds
/// before any actual data movement — the very bottleneck robocopy works
/// around with `/MT`.
///
/// `indicatif` auto-detects TTY on stderr — when piped, both pass UIs are
/// no-op draw targets and only the regular `eprintln!` warnings + final
/// summary line surface.
/// Returns the total bytes scanned + copied. Callers use this to size
/// follow-up work (e.g. the git index refresh that runs over every
/// entry — for >2 GiB trees we batch it for progress; for small trees
/// a plain spinner is enough).
///
/// `excludes` are always-skipped top-level directory NAMES (`.git`,
/// `.jj`) — hard-coded by the VCS-specific callers, never user-
/// touchable.
///
/// **User-configurable excludes** (`[copy] exclude` in the project
/// config) are loaded internally — calling `Config::load()` once at
/// the top of this function lets the VCS backend callers stay
/// unaware of the new config field. Failure to load (no config, parse
/// error) silently falls through to an empty pattern set — best-
/// effort, never blocks the copy.
pub fn try_clone_dir_except(
    src: &Path,
    dst: &Path,
    excludes: &[&str],
) -> Result<u64> {
    let user_patterns: Vec<String> = crate::config::Config::load()
        .map(|c| c.copy_excludes)
        .unwrap_or_default();
    let user_patterns = user_patterns.as_slice();

    // Windows fast path: hand the entire copy off to robocopy. It's the
    // empirically fastest copier on this platform — beats both our
    // parallel `std::fs::copy` and `reflink_copy::reflink_or_copy` on
    // multi-100k-file repos. Robocopy ships with every Windows since
    // Vista, so the spawn essentially never fails for a missing binary;
    // if it does, we fall through to the in-process implementation
    // below.
    #[cfg(windows)]
    match try_clone_via_robocopy(src, dst, excludes, user_patterns) {
        Ok(bytes) => return Ok(bytes),
        Err(RobocopyError::SpawnFailed(e)) => {
            eprintln!(
                "Note: failed to spawn robocopy ({e}); falling back to in-process copy."
            );
            // fall through to in-process path
        }
        Err(RobocopyError::ExitCode(code)) => {
            return Err(Error::Io(std::io::Error::other(format!(
                "robocopy exited with code {code} (>= 8 indicates real errors; \
                 try `ws new --no-cow` to bypass robocopy)"
            ))));
        }
    }

    try_clone_dir_except_inproc(src, dst, excludes, user_patterns)
}

#[cfg(windows)]
enum RobocopyError {
    /// `Command::new("robocopy").status()` itself failed — typically
    /// `NotFound` if robocopy somehow isn't on PATH. We fall back to the
    /// in-process implementation in this case.
    SpawnFailed(std::io::Error),
    /// robocopy ran to completion but returned an exit code >= 8
    /// (real error). The dst may have partial data; don't retry.
    ExitCode(i32),
}

/// Copy `src` to `dst` via robocopy. Wall time on a 300k-file / 13 GB
/// CargoWise repo on a 20-core ReFS DevDrive: ~280s.
///
/// **UI**: three vertically-stacked elements (via `MultiProgress`):
///   1. A scanning spinner (Phase 1 — we walk source to learn total
///      file count, ~15s on a 300k-file tree).
///   2. The main byte/file progress bar (Phase 2 — incremented on every
///      per-file line robocopy emits to its stdout).
///   3. A rolling 10-line tail frame showing the most recent robocopy
///      output (currently-being-copied filenames, etc.). Gives the user
///      something to watch and surfaces transient errors without
///      requiring a full log dump on success.
///
/// **Why /MT:10 instead of /MT:16**: empirically, 16 threads pegs all
/// 20 logical cores hard enough to make the rest of the system
/// noticeably laggy during the clone (user-reported). /MT:10 leaves
/// enough idle CPU for editor/browser/etc. to stay responsive; the copy
/// throughput delta is small (~10%) since the bottleneck on a 300k-file
/// repo is filesystem metadata, not CPU.
///
/// **Flags** (concise reference):
///   - `/E`     — copy all subdirectories, including empty ones
///   - `/MT:10` — 10-thread parallel copier
///   - `/R:1 /W:1` — retry once with a 1s wait (default /R:1M would
///                  hang ~1M minutes on permission errors)
///   - `/NDL /NJH /NJS /NP /NC` — suppress dir lines, job header/footer,
///                                percentages, class column. The file
///                                list itself is KEPT so we can parse
///                                it in real time for progress.
///   - `/XD <src>\<excl>` — exclude TOP-LEVEL only (full path makes the
///                          match exact; a bare name would also exclude
///                          any deeply-nested directory of the same
///                          name, which differs from our in-process
///                          walker's semantics).
///
/// **Exit code semantics**: robocopy uses a bitmap, NOT POSIX 0/non-0.
///   - 0     — nothing to do (source == dest, idempotent)
///   - 1     — files copied successfully (the happy path)
///   - 2     — extra files in dst (won't happen for us — dst is fresh)
///   - 3     — 1+2
///   - 4-7   — file mismatches (won't happen for us)
///   - 8+    — real failure (one or more files could not be copied)
///   - 16    — fatal error (robocopy never started anything)
///
/// We treat any code < 8 as success.
#[cfg(windows)]
fn try_clone_via_robocopy(
    src: &Path,
    dst: &Path,
    excludes: &[&str],
    user_patterns: &[String],
) -> std::result::Result<u64, RobocopyError> {
    use indicatif::{HumanBytes, ProgressBar, ProgressStyle};
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    // Robocopy refuses verbatim Windows paths (`\\?\C:\…`) — it errors
    // out with "ERROR 53 The network path was not found" because it
    // tries to treat them as UNC. Rust's `canonicalize()` returns
    // verbatim paths by default on Windows; some callers pass already-
    // canonicalized paths into us. Strip the prefix here so robocopy
    // sees plain drive-letter paths regardless of how the caller got
    // here. See also `config::strip_verbatim_prefix`.
    let src_stripped = crate::config::strip_verbatim_prefix(src.to_path_buf());
    let dst_stripped = crate::config::strip_verbatim_prefix(dst.to_path_buf());

    // ------------------------------------------------------------------
    // Phase 1: get total file count + bytes via the per-repo cache,
    // refreshing it (with a live spinner) only when stale.
    // ------------------------------------------------------------------
    // The cache lives at `<workspaces_dir>/<project>-<hash>/.repo-meta.toml`
    // and is refreshed at most every 30 days. Within that window the
    // call is a cheap TOML read; outside it we fall back to the same
    // metadata walk we used to do unconditionally (~15 s on a 300k-
    // file tree).
    //
    // `dst.parent()` is the per-project dir (`workspaces_dir.join(
    // workspace_id)`). The walk that fills the cache must use the same
    // top-level exclusions as the copy itself so the cached totals are
    // a faithful denominator.
    let project_dir = dst_stripped
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| dst_stripped.clone());

    let scan_pb = ProgressBar::new_spinner();
    scan_pb.set_style(
        ProgressStyle::with_template("  {spinner:.cyan} Scanning files (refreshing cache)... {msg}")
            .unwrap()
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ "),
    );
    scan_pb.enable_steady_tick(Duration::from_millis(80));

    let (total_files, total_bytes, from_cache) = match crate::repo_meta::load_or_refresh(
        &project_dir,
        &src_stripped,
        excludes,
        user_patterns,
    ) {
        Ok(res) => (res.meta.total_files, res.meta.total_bytes, res.from_cache),
        Err(e) => {
            scan_pb.suspend(|| {
                eprintln!("Warning: repo-meta cache load/refresh failed: {e}");
            });
            (0, 0, false)
        }
    };
    scan_pb.finish_and_clear();

    if from_cache {
        eprintln!("  (using cached repo metadata; auto-refresh after 30 days)");
    }

    // Heads-up line so the user knows the multi-minute work that's
    // about to happen — and what scale of work it is. Without this the
    // bar appears mysteriously after the scan spinner clears, with no
    // human-readable preface explaining what's being copied.
    eprintln!(
        "  Copying repository: {} files, {}",
        total_files,
        HumanBytes(total_bytes)
    );

    // ------------------------------------------------------------------
    // Phase 2: simple elapsed-time spinner.
    // ------------------------------------------------------------------
    // The v0.13.7 implementation drove a real progress bar + a 10-line
    // log tail by piping robocopy's per-file stdout (no /NFL) into a
    // background reader thread. On the user's CargoWise repo
    // (303 k files / 13 GB on ReFS) that took 7 minutes — vs 2 minutes
    // for the user's user-validated `robocopy ... /MT:128 /R:0 /W:0
    // /COPY:DAT /DCOPY:DAT /NFL /NDL /NP` hand-run.
    //
    // Two compounding causes of our slowdown:
    //   1. `/MT:10` was too conservative for this kind of monorepo —
    //      the user's measured fastest setting is `/MT:128` on the
    //      same hardware (ReFS metadata throughput scales with
    //      contention up to surprisingly high thread counts).
    //   2. Piping stdout to a Rust reader thread that updates 10
    //      `ProgressBar` messages + an Arc<Mutex<Vec>> per line is
    //      300 k of extra mutex acquisitions + indicatif draws. The
    //      pipe back-pressure then makes robocopy wait on our reads,
    //      and once we had /MT:10 plus the pipe-stall they
    //      compounded.
    //
    // The fix: copy the user's hand-validated flag set verbatim, plus
    // route stdout to `Stdio::null()` so the per-file lines never
    // even reach us. We lose the live progress bar + log-tail frame;
    // we gain ~3-5 min of wall time on the bulk-copy phase. The user
    // explicitly prioritised speed over UI polish.
    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        ProgressStyle::with_template("  {spinner:.cyan} Copying via robocopy ... {msg}")
            .unwrap()
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ "),
    );
    spinner.set_message("0s elapsed");
    spinner.enable_steady_tick(Duration::from_millis(120));

    // Heads-up so the user can see what's about to be copied; same
    // information they used to glean from the per-file tail frame,
    // now compressed into one line since the frame is gone.
    eprintln!("  ({} files / {})", total_files, HumanBytes(total_bytes));

    // ------------------------------------------------------------------
    // Phase 3: spawn robocopy with the user-validated flag set.
    // ------------------------------------------------------------------
    // Flag rationale (every flag below was either user-measured or
    // exists to keep stdout silent so the pipe never back-pressures
    // robocopy):
    //
    //   /E          — copy all subdirectories, including empty
    //   /MT:128     — 128-thread internal copier. Empirically the
    //                 fastest setting on the user's 20-core
    //                 ReFS DevDrive monorepo (~12 GB / 300 k files
    //                 in 2 min). Robocopy's max is /MT:128.
    //   /R:0 /W:0   — zero retries, zero wait. The default `/R:1M
    //                 /W:30` would hang for tens of minutes on a
    //                 single permission flake; we'd rather see an
    //                 error and bail than wait.
    //   /COPY:DAT   — copy Data + Attributes + Timestamps only.
    //                 Default robocopy includes Security ACLs (/S)
    //                 and Owner info (/O); both are noise for our
    //                 worktree-copy use case and add per-file work.
    //   /DCOPY:DAT  — same trio for directories.
    //   /NFL /NDL   — no file list, no directory list. The biggest
    //                 stdout silencers; per-file lines were the bulk
    //                 of v0.13.7's pipe-stall cost.
    //   /NJH /NJS   — drop the job header + summary blocks.
    //   /NP         — no per-file percentage.
    //   /NC         — no action-class column.
    let mut cmd = Command::new("robocopy");
    cmd.arg(&src_stripped);
    cmd.arg(&dst_stripped);
    cmd.arg("/E");
    cmd.arg("/MT:128");
    cmd.arg("/R:0");
    cmd.arg("/W:0");
    cmd.arg("/COPY:DAT");
    cmd.arg("/DCOPY:DAT");
    cmd.arg("/NFL");
    cmd.arg("/NDL");
    cmd.arg("/NJH");
    cmd.arg("/NJS");
    cmd.arg("/NP");
    cmd.arg("/NC");
    for exc in excludes {
        cmd.arg("/XD");
        cmd.arg(src_stripped.join(exc));
    }
    // Honor `user_patterns` at the robocopy boundary by pre-scanning
    // the source's top-level entries and feeding any matching paths to
    // robocopy as `/XD <fullpath>` (dirs) / `/XF <fullpath>` (files).
    //
    // **Limitation**: robocopy only understands literal paths, not
    // gitignore globs. A pattern like `**/*.iso` that matches a file
    // 5 levels deep cannot be expressed to robocopy without
    // enumerating every match — which would defeat the purpose. So
    // only TOP-LEVEL matches are honored on the robocopy path.
    // Deep glob excludes silently no-op when robocopy is used. Users
    // who need them can `--no-cow` to bypass robocopy and fall through
    // to the in-process implementation (which respects the matcher
    // at every depth).
    {
        use ignore::gitignore::GitignoreBuilder;
        let mut gi = GitignoreBuilder::new(&src_stripped);
        for pat in user_patterns {
            let _ = gi.add_line(None, pat);
        }
        if let Ok(matcher) = gi.build()
            && let Ok(entries) = std::fs::read_dir(&src_stripped)
        {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let full = src_stripped.join(&name);
                let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                if matcher.matched(&full, is_dir).is_ignore() {
                    cmd.arg(if is_dir { "/XD" } else { "/XF" });
                    cmd.arg(&full);
                }
            }
        }
    }
    // Direct robocopy → null. Even with the suppression flags above,
    // any pipe at all reintroduces back-pressure on robocopy if our
    // reader is even fractionally slower than its writer; nulling the
    // streams sidesteps that entirely. Stderr is kept piped only so
    // we can surface anything that comes out of it on a failure exit
    // code (robocopy's exit-code-only error mode is unhelpful).
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::piped());

    let copy_start = std::time::Instant::now();
    let mut child = cmd.spawn().map_err(RobocopyError::SpawnFailed)?;
    let stderr = child.stderr.take().expect("stderr was piped");

    // Spinner ticker — updates the `{msg}` once a second with the
    // wall-clock elapsed seconds. Same pattern as
    // `refresh_index_spinner` in vcs/git/worktree.rs.
    let spinner_for_thread = spinner.clone();
    let stop_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_flag_for_thread = std::sync::Arc::clone(&stop_flag);
    let ticker = std::thread::spawn(move || {
        let started = std::time::Instant::now();
        while !stop_flag_for_thread.load(std::sync::atomic::Ordering::Relaxed) {
            let elapsed = started.elapsed().as_secs();
            spinner_for_thread.set_message(format!("{elapsed}s elapsed"));
            std::thread::sleep(Duration::from_millis(500));
        }
    });

    // Drain stderr into a buffer for error-case diagnostics. Doesn't
    // block the spinner — robocopy emits very little to stderr in
    // practice.
    let stderr_captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::with_capacity(256)));
    let stderr_captured_clone = Arc::clone(&stderr_captured);
    let stderr_thread = std::thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines().map_while(|r| r.ok()) {
            stderr_captured_clone.lock().unwrap().push(line);
        }
    });

    // Wait for completion, tear down spinner + ticker.
    let status = child.wait().map_err(RobocopyError::SpawnFailed)?;
    stop_flag.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = ticker.join();
    let _ = stderr_thread.join();
    spinner.finish_and_clear();

    let code = status.code().unwrap_or(-1);
    if code >= 8 {
        let stderr_lines = stderr_captured.lock().unwrap();
        if !stderr_lines.is_empty() {
            eprintln!("--- robocopy stderr ({} lines) ---", stderr_lines.len());
            for line in stderr_lines.iter() {
                eprintln!("{line}");
            }
            eprintln!("---------------------------------");
        }
        return Err(RobocopyError::ExitCode(code));
    }

    eprintln!(
        "  Cloned {} files ({}) via robocopy /MT:128 ({}, exit {code}).",
        total_files,
        HumanBytes(total_bytes),
        crate::util::format_step(copy_start.elapsed())
    );
    Ok(total_bytes)
}


/// In-process fallback used on non-Windows and when robocopy fails to
/// spawn. Identical to the v0.13.5 implementation — see the trait-level
/// doc for the 3-phase architecture.
fn try_clone_dir_except_inproc(
    src: &Path,
    dst: &Path,
    excludes: &[&str],
    user_patterns: &[String],
) -> Result<u64> {
    use indicatif::{HumanBytes, ProgressBar, ProgressStyle};
    use rayon::prelude::*;
    use std::sync::atomic::Ordering;
    use std::sync::Arc;
    use std::time::Duration;

    // build_clone_walker now takes the &[&str] hardcoded excludes and
    // the gitignore-style user patterns separately; both feed the
    // same `Gitignore` matcher inside the walker builder. No need to
    // pre-clone into an owned Vec.
    let _ = excludes; // silence unused-binding lint; consumed by walker below

    // ------------------------------------------------------------------
    // Phase 1: scan + plan.
    // ------------------------------------------------------------------
    let scan_pb = ProgressBar::new_spinner();
    scan_pb.set_style(
        ProgressStyle::with_template("  {spinner:.cyan} Scanning files... {msg}")
            .unwrap()
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ "),
    );
    scan_pb.enable_steady_tick(Duration::from_millis(80));

    let mut planned: Vec<PlannedOp> = Vec::new();
    let mut total_files: u64 = 0;
    let mut total_bytes: u64 = 0;

    for entry_result in build_clone_walker(src, excludes, user_patterns) {
        let entry = match entry_result {
            Ok(e) => e,
            Err(e) => {
                scan_pb.suspend(|| eprintln!("Warning: cow walk skipped entry: {e}"));
                continue;
            }
        };
        let rel = match entry.path().strip_prefix(src) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if rel.as_os_str().is_empty() {
            continue; // root
        }
        let dest = dst.join(rel);

        let file_type = match entry.file_type() {
            Some(ft) => ft,
            None => continue,
        };

        if file_type.is_dir() {
            planned.push(PlannedOp::Dir(dest));
        } else if file_type.is_file() {
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            total_files += 1;
            total_bytes += size;
            planned.push(PlannedOp::File {
                src: entry.path().to_path_buf(),
                dst: dest,
                size,
            });
            if total_files.is_multiple_of(256) {
                scan_pb.set_message(format!(
                    "{} files, {}",
                    total_files,
                    HumanBytes(total_bytes)
                ));
            }
        } else if file_type.is_symlink() {
            planned.push(PlannedOp::Symlink {
                src: entry.path().to_path_buf(),
                dst: dest,
            });
        }
        // Other file types (sockets, devices, fifos) are skipped silently.
    }
    scan_pb.finish_and_clear();

    // Heads-up line — see the matching block in `try_clone_via_robocopy`.
    eprintln!(
        "  Copying repository: {} files, {}",
        total_files,
        HumanBytes(total_bytes)
    );

    // Stopwatch for the per-file copy phase, surfaced in the final
    // "Cloned ... ({elapsed})" line below.
    let copy_start = std::time::Instant::now();

    // ------------------------------------------------------------------
    // Phase 2: create directories in parallel.
    // ------------------------------------------------------------------
    // The walker yields entries in depth-first order, so a child dir may
    // come before its grandparent's siblings — but `create_dir_all` is
    // idempotent and walks upwards as needed, so any iteration order
    // works. We parallelise because on a real monorepo (~60k dirs on
    // CargoWise) sequential mkdir takes ~30s; parallel ~5s.
    //
    // After this phase EVERY parent directory referenced by a file's
    // `dst` already exists, so the parallel copy phase below can call
    // `reflink_or_copy` / `fs::copy` directly without a defensive
    // `create_dir_all(parent)` per file. That defensive call was the
    // single largest source of slowness in the old impl — on 300k files
    // it added ~10 minutes of stat-the-parent overhead.
    planned.par_iter().for_each(|op| {
        if let PlannedOp::Dir(d) = op
            && let Err(e) = std::fs::create_dir_all(d)
        {
            eprintln!("Warning: cow mkdir {}: {e}", d.display());
        }
    });

    // ------------------------------------------------------------------
    // Phase 3: parallel file copy.
    // ------------------------------------------------------------------
    // Byte-based (not file-based) progress because file sizes in a real
    // monorepo are wildly skewed — a 4 GB `node_modules.tar` next to
    // 50,000 1-KB sources. File-count percent would jump from 1% to 99%
    // in three big files; byte-count tracks actual disk throughput.
    let pb = Arc::new(ProgressBar::new(total_bytes));
    pb.set_style(
        ProgressStyle::with_template(
            "  {spinner:.cyan} [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({percent}%) | {msg} | ETA {eta}",
        )
        .unwrap()
        .progress_chars("█▉▊▋▌▍▎▏ ")
        .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ "),
    );
    pb.enable_steady_tick(Duration::from_millis(80));
    pb.set_message(format!("0/{total_files} files"));

    let stats = Arc::new(CopyStats::default());

    planned.par_iter().for_each(|op| {
        match op {
            PlannedOp::Dir(_) => {
                // Already created in phase 2.
            }
            PlannedOp::File { src, dst, size } => {
                if !copy_file(src, dst, &stats) {
                    // copy_file already incremented stats.errors; print
                    // the actual error via a side channel since we lost
                    // it in the bool conversion. Re-attempting just to
                    // capture the error message would change semantics
                    // (e.g. partial-write states). Instead the per-file
                    // error path stays terse; users see the aggregate
                    // "N errors" count in the summary and can re-run
                    // with `--no-cow` for verbose diagnostics.
                    let pb = Arc::clone(&pb);
                    pb.suspend(|| {
                        eprintln!("Warning: cow copy failed: {}", src.display())
                    });
                }
                let done_files = stats.done_files.fetch_add(1, Ordering::Relaxed) + 1;
                stats.done_bytes.fetch_add(*size, Ordering::Relaxed);
                pb.inc(*size);
                // Throttle message updates — set_message redraws, and on
                // the reflink fast path we'd spend more CPU formatting
                // than cloning. Every 16 files is visually smooth.
                if done_files.is_multiple_of(16) || done_files == total_files {
                    pb.set_message(format!("{done_files}/{total_files} files"));
                }
            }
            PlannedOp::Symlink { src, dst } => {
                if let Err(e) = copy_symlink(src, dst) {
                    stats.errors.fetch_add(1, Ordering::Relaxed);
                    pb.suspend(|| {
                        eprintln!("Warning: cow symlink {}: {e}", src.display())
                    });
                }
            }
        }
    });

    pb.finish_and_clear();

    let reflinked = stats.reflinked.load(Ordering::Relaxed);
    let copied = stats.copied.load(Ordering::Relaxed);
    let errors = stats.errors.load(Ordering::Relaxed);
    let total_done = reflinked + copied;

    // Summary format depends on the platform we just used (see `copy_file`):
    //
    //   - Windows: `std::fs::copy` / `CopyFileExW` — the kernel chooses
    //     between byte-copy and block-clone transparently; we can't tell
    //     externally. `reflinked` is always 0; the total goes into `copied`.
    //
    //   - Linux/macOS: `reflink_copy::reflink_or_copy` distinguishes
    //     `Ok(None)` (block-cloned) from `Ok(Some(_))` (fell back to
    //     fs::copy because the FS doesn't support reflink for this file).
    //     Both counts are meaningful — `reflinked` ≫ `copied` confirms the
    //     FS-level fast path is firing.
    let elapsed = crate::util::format_step(copy_start.elapsed());
    if cfg!(windows) {
        if errors > 0 {
            eprintln!(
                "  Cloned {total_done} files ({}) via CopyFileExW ({elapsed}, {errors} errors).",
                HumanBytes(total_bytes)
            );
        } else {
            eprintln!(
                "  Cloned {total_done} files ({}) via CopyFileExW ({elapsed}).",
                HumanBytes(total_bytes)
            );
        }
    } else if errors > 0 {
        eprintln!(
            "  Cloned {total_done} files ({}): {reflinked} reflinked, {copied} copied ({elapsed}, {errors} errors).",
            HumanBytes(total_bytes)
        );
    } else {
        eprintln!(
            "  Cloned {total_done} files ({}): {reflinked} reflinked, {copied} copied ({elapsed}).",
            HumanBytes(total_bytes)
        );
    }

    Ok(total_bytes)
}

#[cfg(unix)]
fn copy_symlink(src: &Path, dst: &Path) -> Result<()> {
    let target = std::fs::read_link(src)?;
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::os::unix::fs::symlink(&target, dst)?;
    Ok(())
}

#[cfg(windows)]
fn copy_symlink(src: &Path, dst: &Path) -> Result<()> {
    let target = std::fs::read_link(src)?;
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Windows symlinks distinguish file/dir targets; pick based on the
    // resolved target's type. Requires SeCreateSymbolicLinkPrivilege or
    // Developer Mode; fall back below on permission errors.
    let result = if target.is_dir() {
        std::os::windows::fs::symlink_dir(&target, dst)
    } else {
        std::os::windows::fs::symlink_file(&target, dst)
    };
    match result {
        Ok(()) => Ok(()),
        Err(_) => {
            // Best-effort fallback chain:
            //   1. If target exists and is a regular file → copy its
            //      contents. Worktree gets a real file at dst instead of
            //      a symlink. Not ideal but workable.
            //   2. If target is missing/broken → log and continue with
            //      no file at dst. Better than silently dropping (the
            //      previous behaviour) — at least the user sees a
            //      warning naming the path.
            if target.is_file() {
                std::fs::copy(&target, dst)?;
            } else {
                eprintln!(
                    "Warning: cow: cannot create symlink {} → {} \
                     (no privilege; target missing or non-file)",
                    dst.display(),
                    target.display()
                );
            }
            Ok(())
        }
    }
}
