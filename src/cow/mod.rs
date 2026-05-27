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
/// the workspace dir under `$AGENT_WORKSPACE_DIR/workspaces/<id>/`.
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

/// Outcome of a single `reflink_copy::reflink_or_copy` invocation.
/// Lets the caller report `N reflinked, M copied` instead of an opaque
/// "cloned X files" — important for verifying that the ReFS block-clone
/// fast path is actually firing (the crate falls back silently to
/// `fs::copy` on any per-file error, and historically we threw away the
/// `Ok(None)` vs `Ok(Some(_))` signal).
#[derive(Default)]
struct CopyStats {
    reflinked: std::sync::atomic::AtomicU64,
    copied: std::sync::atomic::AtomicU64,
    errors: std::sync::atomic::AtomicU64,
    done_files: std::sync::atomic::AtomicU64,
    done_bytes: std::sync::atomic::AtomicU64,
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
    // Phase 2: create directories serially.
    // ------------------------------------------------------------------
    // Doing this up-front means the parallel copy phase doesn't race on
    // `create_dir_all`. The defensive `create_dir_all(parent)` before
    // each file copy below stays — it costs only a single stat per file
    // on the happy path and protects against unusual nesting orders.
    for op in &planned {
        if let PlannedOp::Dir(d) = op
            && let Err(e) = std::fs::create_dir_all(d)
        {
            eprintln!("Warning: cow mkdir {}: {e}", d.display());
        }
    }

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
        let pb = Arc::clone(&pb);
        let stats = Arc::clone(&stats);
        match op {
            PlannedOp::Dir(_) => {
                // Already created in phase 2.
            }
            PlannedOp::File { src, dst, size } => {
                // Defensive parent-dir create — `create_dir_all` is a
                // no-op stat when the directory already exists.
                if let Some(parent) = dst.parent()
                    && let Err(e) = std::fs::create_dir_all(parent)
                {
                    stats.errors.fetch_add(1, Ordering::Relaxed);
                    pb.suspend(|| {
                        eprintln!("Warning: cow mkdir {}: {e}", parent.display())
                    });
                    return;
                }
                match reflink_copy::reflink_or_copy(src, dst) {
                    Ok(None) => {
                        stats.reflinked.fetch_add(1, Ordering::Relaxed);
                    }
                    Ok(Some(_bytes)) => {
                        stats.copied.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(e) => {
                        stats.errors.fetch_add(1, Ordering::Relaxed);
                        pb.suspend(|| {
                            eprintln!("Warning: cow copy {}: {e}", src.display())
                        });
                    }
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
                if let Some(parent) = dst.parent()
                    && let Err(e) = std::fs::create_dir_all(parent)
                {
                    stats.errors.fetch_add(1, Ordering::Relaxed);
                    pb.suspend(|| {
                        eprintln!("Warning: cow mkdir {}: {e}", parent.display())
                    });
                    return;
                }
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

    // Three reported categories so the user can see whether ReFS
    // block-clone actually fired:
    //   - "reflinked": kernel succeeded with FSCTL_DUPLICATE_EXTENTS_TO_FILE
    //     (or the platform equivalent) — near-instant, share extents.
    //   - "copied":   reflink_or_copy fell back to fs::copy — real bytes
    //     moved. Expected for files on volumes that don't support
    //     block-clone (NTFS, ext4 w/o reflink, etc.) and for the small
    //     edge cases the reflink-copy crate refuses (cluster-size
    //     mismatch, etc.).
    //   - "errors":   neither reflink nor copy worked. Per-file warning
    //     was already printed; reported here as a final summary count.
    if errors > 0 {
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
