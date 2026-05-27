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
fn build_clone_walker(src: &Path, excludes: &[String]) -> ignore::Walk {
    use ignore::WalkBuilder;

    let excludes_owned = excludes.to_vec();
    WalkBuilder::new(src)
        // Walk everything — including hidden files. We want a complete
        // mirror, not a gitignore-filtered subset.
        .standard_filters(false)
        .hidden(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        // Don't follow symlinks; copy them as-is below.
        .follow_links(false)
        // Filter the immediate-child excludes (`.git/`, `.jj/`).
        .filter_entry(move |entry| {
            if entry.depth() == 1
                && let Some(name) = entry.file_name().to_str()
                && excludes_owned.iter().any(|ex| ex == name)
            {
                return false;
            }
            true
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
pub fn try_clone_dir_except(src: &Path, dst: &Path, excludes: &[&str]) -> Result<()> {
    use indicatif::{HumanBytes, ProgressBar, ProgressStyle};
    use rayon::prelude::*;
    use std::sync::atomic::Ordering;
    use std::sync::Arc;
    use std::time::Duration;

    let excludes_owned: Vec<String> = excludes.iter().map(|s| s.to_string()).collect();

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

    for entry_result in build_clone_walker(src, &excludes_owned) {
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
    if cfg!(windows) {
        if errors > 0 {
            eprintln!(
                "  Cloned {total_done} files ({}) via CopyFileExW ({errors} errors).",
                HumanBytes(total_bytes)
            );
        } else {
            eprintln!(
                "  Cloned {total_done} files ({}) via CopyFileExW.",
                HumanBytes(total_bytes)
            );
        }
    } else if errors > 0 {
        eprintln!(
            "  Cloned {total_done} files ({}): {reflinked} reflinked, {copied} copied, {errors} errors.",
            HumanBytes(total_bytes)
        );
    } else {
        eprintln!(
            "  Cloned {total_done} files ({}): {reflinked} reflinked, {copied} copied.",
            HumanBytes(total_bytes)
        );
    }

    Ok(())
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
