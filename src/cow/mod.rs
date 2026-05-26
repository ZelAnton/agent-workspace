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
/// **Progress UI** — two-pass for accurate ETA on large monorepos:
///   1. Scan pass walks every entry, counting files and summing sizes.
///      Shown as a spinner with live "N files, M GB" message. On a 20+ GB
///      tree this takes a few seconds (metadata stats only, no file I/O).
///   2. Copy pass does the actual reflink + per-file fallback, updating a
///      byte-based progress bar with ETA, current path, and file count.
///
/// `indicatif` auto-detects TTY on stderr — when piped, both pass UIs are
/// no-op draw targets and only the regular `eprintln!` warnings surface.
pub fn try_clone_dir_except(src: &Path, dst: &Path, excludes: &[&str]) -> Result<()> {
    use indicatif::{HumanBytes, ProgressBar, ProgressStyle};
    use std::time::Duration;

    let excludes_owned: Vec<String> = excludes.iter().map(|s| s.to_string()).collect();

    // ------------------------------------------------------------------
    // Phase 1: scan to learn total files + total bytes.
    // ------------------------------------------------------------------
    // The spinner ticks on a steady timer so even very slow filesystems
    // (network drives, antivirus-scanned NTFS) give the user constant
    // visual feedback. We push `set_message` only every 256 files so the
    // formatter doesn't dominate the scan-loop cost on fast SSDs.
    let scan_pb = ProgressBar::new_spinner();
    scan_pb.set_style(
        ProgressStyle::with_template("  {spinner:.cyan} Scanning files... {msg}")
            .unwrap()
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ "),
    );
    scan_pb.enable_steady_tick(Duration::from_millis(80));

    let mut total_files: u64 = 0;
    let mut total_bytes: u64 = 0;
    for entry_result in build_clone_walker(src, &excludes_owned) {
        let entry = match entry_result {
            Ok(e) => e,
            Err(_) => continue,
        };
        let ft = match entry.file_type() {
            Some(ft) => ft,
            None => continue,
        };
        if ft.is_file() {
            total_files += 1;
            if let Ok(meta) = entry.metadata() {
                total_bytes += meta.len();
            }
            if total_files.is_multiple_of(256) {
                scan_pb.set_message(format!(
                    "{} files, {}",
                    total_files,
                    HumanBytes(total_bytes)
                ));
            }
        }
    }
    scan_pb.finish_and_clear();

    // ------------------------------------------------------------------
    // Phase 2: actual copy, with byte-based progress + per-file count.
    // ------------------------------------------------------------------
    // Byte-based (not file-based) progress because file sizes in a real
    // monorepo are wildly skewed — a 4 GB `node_modules.tar` next to
    // 50,000 1-KB sources. File-count percent would jump from 1% to 99%
    // in three big files; byte-count tracks actual disk throughput.
    let pb = ProgressBar::new(total_bytes);
    pb.set_style(
        ProgressStyle::with_template(
            "  {spinner:.cyan} [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({percent}%) | {msg} | ETA {eta}",
        )
        .unwrap()
        .progress_chars("█▉▊▋▌▍▎▏ ")
        .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ "),
    );
    pb.enable_steady_tick(Duration::from_millis(80));
    pb.set_message(format!("0/{} files", total_files));

    let mut copied_files: u64 = 0;
    for entry_result in build_clone_walker(src, &excludes_owned) {
        // Walker errors are per-entry (permission denied on one file,
        // broken symlink during traversal, etc.). Log and continue
        // instead of aborting the whole clone — a partial worktree with
        // explicit warnings is more useful than a hard failure that
        // forces the user to fall back to `--no-cow` and retry.
        //
        // `pb.suspend` temporarily hides the bar so the warning line
        // doesn't get overdrawn on the next tick.
        let entry = match entry_result {
            Ok(e) => e,
            Err(e) => {
                pb.suspend(|| eprintln!("Warning: cow walk skipped entry: {e}"));
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

        // Cache the file size before the copy: we need it for `pb.inc()`
        // after a successful reflink (the reflink call itself doesn't
        // return a byte count when it actually reflinks vs falls back).
        let file_size = if file_type.is_file() {
            entry.metadata().map(|m| m.len()).unwrap_or(0)
        } else {
            0
        };

        let copy_result = if file_type.is_dir() {
            std::fs::create_dir_all(&dest).map_err(Error::from)
        } else if file_type.is_file() {
            // Parent-dir creation MUST be part of `copy_result` (not a bare
            // `?`) — otherwise a permission failure on `mkdir parent` aborts
            // the whole walk, contradicting the per-entry-logged contract
            // that walker errors and reflink errors follow.
            let mkdir_result = match dest.parent() {
                Some(parent) => std::fs::create_dir_all(parent).map_err(Error::from),
                None => Ok(()),
            };
            mkdir_result.and_then(|()| {
                // Reflink with per-file fallback to plain copy. The crate
                // handles "this kernel/FS doesn't support reflink" by
                // copying — we don't need to distinguish here.
                reflink_copy::reflink_or_copy(entry.path(), &dest)
                    .map(|_| ())
                    .map_err(Error::from)
            })
        } else if file_type.is_symlink() {
            copy_symlink(entry.path(), &dest)
        } else {
            // Other types (sockets, devices, fifos) are skipped silently —
            // they don't belong in a worktree.
            Ok(())
        };

        match copy_result {
            Ok(()) => {
                if file_type.is_file() {
                    copied_files += 1;
                    pb.inc(file_size);
                    // Throttle the message update — set_message redraws,
                    // and on a fast reflink path we'd otherwise spend more
                    // CPU formatting strings than copying. Every 16 files
                    // is visually smooth and cheap.
                    if copied_files.is_multiple_of(16) || copied_files == total_files {
                        pb.set_message(format!("{copied_files}/{total_files} files"));
                    }
                }
            }
            Err(e) => {
                pb.suspend(|| eprintln!("Warning: cow copy {}: {e}", rel.display()));
            }
        }
    }

    pb.finish_and_clear();
    eprintln!(
        "  Cloned {} files ({}) via reflink.",
        copied_files,
        HumanBytes(total_bytes)
    );

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
