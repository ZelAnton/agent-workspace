// Full-repo probe: mirror ws new's CoW architecture (scan + mkdir +
// parallel copy) on a real source tree and time each phase separately
// so we know where the wall-clock budget goes.
//
// Usage: cargo run --release --example probe_full -- <src_dir> <dst_dir>

use rayon::prelude::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

fn human_bytes(b: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{v:.2} {}", UNITS[i])
}

enum Op {
    Dir(PathBuf),
    // `size` is recorded for parity with ws new's planned-op shape even though
    // this probe sums bytes separately; allow it to stay unread.
    File {
        src: PathBuf,
        dst: PathBuf,
        #[allow(dead_code)]
        size: u64,
    },
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: probe_full <src_dir> <dst_dir>");
        std::process::exit(2);
    }
    let src_dir = PathBuf::from(&args[1]);
    let dst_dir = PathBuf::from(&args[2]);
    let _ = std::fs::remove_dir_all(&dst_dir);
    std::fs::create_dir_all(&dst_dir).unwrap();

    println!("src = {}", src_dir.display());
    println!("dst = {}", dst_dir.display());
    println!("rayon threads = {}", rayon::current_num_threads());

    // ---- Phase 1: walk + collect ----
    let t0 = Instant::now();
    let walker = ignore::WalkBuilder::new(&src_dir)
        .standard_filters(false)
        .hidden(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .follow_links(false)
        .filter_entry(|e| !(e.depth() == 1 && e.file_name() == ".git"))
        .build();

    let mut planned: Vec<Op> = Vec::new();
    let mut total_files: u64 = 0;
    let mut total_bytes: u64 = 0;
    let mut total_dirs: u64 = 0;
    for entry in walker.flatten() {
        let rel = match entry.path().strip_prefix(&src_dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if rel.as_os_str().is_empty() {
            continue;
        }
        let dest = dst_dir.join(rel);
        let ft = match entry.file_type() {
            Some(t) => t,
            None => continue,
        };
        if ft.is_dir() {
            total_dirs += 1;
            planned.push(Op::Dir(dest));
        } else if ft.is_file() {
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            total_files += 1;
            total_bytes += size;
            planned.push(Op::File {
                src: entry.path().to_path_buf(),
                dst: dest,
                size,
            });
        }
    }
    let scan_dt = t0.elapsed();
    println!();
    println!(
        "Phase 1 — scan:     {:>8.2?}  ({} files, {} dirs, {} total)",
        scan_dt,
        total_files,
        total_dirs,
        human_bytes(total_bytes)
    );

    // ---- Phase 2: mkdir (serial) ----
    let t0 = Instant::now();
    for op in &planned {
        if let Op::Dir(d) = op {
            let _ = std::fs::create_dir_all(d);
        }
    }
    let mkdir_dt = t0.elapsed();
    println!("Phase 2 — mkdir:    {:>8.2?}  ({} dirs)", mkdir_dt, total_dirs);

    // ---- Phase 3a: parallel reflink_or_copy WITH the per-file
    // create_dir_all(parent) defensive call (mirror current ws new code).
    let t0 = Instant::now();
    let stats3a = (AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0));
    planned.par_iter().for_each(|op| {
        if let Op::File { src, dst, .. } = op {
            // Defensive parent create — what our cow/mod.rs currently does.
            if let Some(parent) = dst.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match reflink_copy::reflink_or_copy(src, dst) {
                Ok(None) => {
                    stats3a.0.fetch_add(1, Ordering::Relaxed);
                }
                Ok(Some(_)) => {
                    stats3a.1.fetch_add(1, Ordering::Relaxed);
                }
                Err(_) => {
                    stats3a.2.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    });
    let copy3a_dt = t0.elapsed();
    println!(
        "Phase 3a — par-copy (with mkdir):   {:>8.2?}  ({} reflinked, {} copied, {} errors)  {:.0} files/s  {:.1} MB/s",
        copy3a_dt,
        stats3a.0.load(Ordering::Relaxed),
        stats3a.1.load(Ordering::Relaxed),
        stats3a.2.load(Ordering::Relaxed),
        total_files as f64 / copy3a_dt.as_secs_f64(),
        (total_bytes as f64 / (1024.0 * 1024.0)) / copy3a_dt.as_secs_f64()
    );

    // Reset dst tree
    let _ = std::fs::remove_dir_all(&dst_dir);
    std::fs::create_dir_all(&dst_dir).unwrap();
    for op in &planned {
        if let Op::Dir(d) = op {
            let _ = std::fs::create_dir_all(d);
        }
    }

    // ---- Phase 3b: parallel reflink_or_copy WITHOUT the per-file
    // create_dir_all (since phase 2 already did it).
    let t0 = Instant::now();
    let stats3b = (AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0));
    planned.par_iter().for_each(|op| {
        if let Op::File { src, dst, .. } = op {
            match reflink_copy::reflink_or_copy(src, dst) {
                Ok(None) => {
                    stats3b.0.fetch_add(1, Ordering::Relaxed);
                }
                Ok(Some(_)) => {
                    stats3b.1.fetch_add(1, Ordering::Relaxed);
                }
                Err(_) => {
                    stats3b.2.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    });
    let copy3b_dt = t0.elapsed();
    println!(
        "Phase 3b — par-copy (NO mkdir):     {:>8.2?}  ({} reflinked, {} copied, {} errors)  {:.0} files/s  {:.1} MB/s",
        copy3b_dt,
        stats3b.0.load(Ordering::Relaxed),
        stats3b.1.load(Ordering::Relaxed),
        stats3b.2.load(Ordering::Relaxed),
        total_files as f64 / copy3b_dt.as_secs_f64(),
        (total_bytes as f64 / (1024.0 * 1024.0)) / copy3b_dt.as_secs_f64()
    );

    // Reset for comparison
    let _ = std::fs::remove_dir_all(&dst_dir);
    std::fs::create_dir_all(&dst_dir).unwrap();
    for op in &planned {
        if let Op::Dir(d) = op {
            let _ = std::fs::create_dir_all(d);
        }
    }

    // ---- Phase 3c: parallel std::fs::copy (no reflink path at all).
    let t0 = Instant::now();
    let stats3c = AtomicU64::new(0);
    let errs3c = AtomicU64::new(0);
    planned.par_iter().for_each(|op| {
        if let Op::File { src, dst, .. } = op {
            match std::fs::copy(src, dst) {
                Ok(_) => {
                    stats3c.fetch_add(1, Ordering::Relaxed);
                }
                Err(_) => {
                    errs3c.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    });
    let copy3c_dt = t0.elapsed();
    println!(
        "Phase 3c — par-copy fs::copy:       {:>8.2?}  ({} copied, {} errors)  {:.0} files/s  {:.1} MB/s",
        copy3c_dt,
        stats3c.load(Ordering::Relaxed),
        errs3c.load(Ordering::Relaxed),
        total_files as f64 / copy3c_dt.as_secs_f64(),
        (total_bytes as f64 / (1024.0 * 1024.0)) / copy3c_dt.as_secs_f64()
    );

    let total = scan_dt + mkdir_dt + copy3b_dt;
    println!();
    println!(
        "Total (scan + mkdir + 3b): {:.2?}",
        total
    );
}
