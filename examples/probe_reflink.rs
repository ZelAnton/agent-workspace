// Diagnostic: probe reflink + fs::copy + CopyFileExW timings on a real file
// pulled from a real source directory, on a real ReFS volume.
//
// Usage:
//   cargo run --release --example probe_reflink -- <src_dir> <dst_dir>
//
// Picks 5 representative source files (small / medium / large / very large /
// random), then for each:
//   - calls reflink_copy::reflink (the raw reflink IOCTL path)
//   - calls reflink_copy::reflink_or_copy (reflink + automatic fallback)
//   - calls std::fs::copy (CopyFileExW on Windows; may auto-block-clone
//     on Windows 11 24H2+ / Server 2025 ReFS)
// Prints per-call duration and the underlying error if any.
//
// Goal: figure out *which* layer is slow on this Windows + ReFS combo —
// the kernel IOCTL itself, the reflink-copy crate's wrapping, or the
// CopyFileExW fallback.

use std::path::{Path, PathBuf};
use std::time::Instant;

fn human_bytes(b: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{:.2} {}", v, UNITS[i])
}

fn collect_files(root: &Path, limit: usize) -> Vec<(PathBuf, u64)> {
    let mut out = Vec::new();
    let walker = ignore::WalkBuilder::new(root)
        .standard_filters(false)
        .hidden(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .follow_links(false)
        .filter_entry(|e| {
            // Skip .git so we don't pick gitlink files.
            !(e.depth() == 1 && e.file_name() == ".git")
        })
        .build();
    for entry in walker.flatten() {
        if out.len() >= limit {
            break;
        }
        if let Some(ft) = entry.file_type()
            && ft.is_file()
            && let Ok(meta) = entry.metadata()
        {
            out.push((entry.path().to_path_buf(), meta.len()));
        }
    }
    out
}

fn time_op<F, E>(label: &str, dst: &Path, f: F) -> Option<std::time::Duration>
where
    F: FnOnce() -> Result<(), E>,
    E: std::fmt::Display,
{
    // Make sure dst doesn't exist before each attempt — both reflink and
    // fs::copy fail with AlreadyExists otherwise.
    let _ = std::fs::remove_file(dst);
    let t0 = Instant::now();
    match f() {
        Ok(()) => {
            let dt = t0.elapsed();
            println!("    {label:>22}: OK in {:?}", dt);
            Some(dt)
        }
        Err(e) => {
            let dt = t0.elapsed();
            println!("    {label:>22}: FAIL in {:?} — {e}", dt);
            None
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: probe_reflink <src_dir> <dst_dir>");
        std::process::exit(2);
    }
    let src_dir = PathBuf::from(&args[1]);
    let dst_dir = PathBuf::from(&args[2]);
    std::fs::create_dir_all(&dst_dir).expect("create dst_dir");

    println!("src = {}", src_dir.display());
    println!("dst = {}", dst_dir.display());

    // Volume check (same as cow::can_clone uses internally).
    println!("same_volume = {:?}", agent_workspace::cow::same_volume(&src_dir, &dst_dir));

    println!();
    println!("Collecting candidate files...");
    let mut files = collect_files(&src_dir, 5000);
    println!("Found {} files", files.len());
    if files.is_empty() {
        eprintln!("no files found, aborting");
        std::process::exit(1);
    }

    // Sort by size and pick representative samples.
    files.sort_by_key(|(_, s)| *s);
    let n = files.len();
    let samples: Vec<&(PathBuf, u64)> = vec![
        &files[0],                 // smallest (often 0 bytes)
        &files[n / 4],
        &files[n / 2],
        &files[(3 * n) / 4],
        &files[n - 1],             // largest
    ];

    println!();
    println!("{:=^80}", " per-file probe ");
    for (path, size) in &samples {
        println!();
        println!("FILE: {} ({})", path.display(), human_bytes(*size));

        let stem = path.file_name().unwrap().to_string_lossy().into_owned();
        let dst_reflink = dst_dir.join(format!("{stem}.reflink"));
        let dst_roc = dst_dir.join(format!("{stem}.roc"));
        let dst_copy = dst_dir.join(format!("{stem}.fscopy"));

        time_op("reflink::reflink", &dst_reflink, || {
            reflink_copy::reflink(path, &dst_reflink)
        });

        time_op("reflink_or_copy", &dst_roc, || {
            match reflink_copy::reflink_or_copy(path, &dst_roc) {
                Ok(None) => {
                    println!("                          (reflinked, no fallback)");
                    Ok(())
                }
                Ok(Some(bytes)) => {
                    println!("                          (FELL BACK to fs::copy, {} bytes)", bytes);
                    Ok(())
                }
                Err(e) => Err(e),
            }
        });

        time_op("std::fs::copy", &dst_copy, || -> std::io::Result<()> {
            std::fs::copy(path, &dst_copy).map(|_| ())
        });

        // Clean up
        let _ = std::fs::remove_file(&dst_reflink);
        let _ = std::fs::remove_file(&dst_roc);
        let _ = std::fs::remove_file(&dst_copy);
    }

    println!();
    println!("{:=^80}", " batch timing: 100 files ");

    // Batch test: time copying the first 100 small/medium files via each
    // strategy. Better signal for sustained per-file overhead than the
    // single-file probe above.
    let batch: Vec<&(PathBuf, u64)> = files.iter().take(100).collect();
    let total_bytes: u64 = batch.iter().map(|(_, s)| *s).sum();
    println!("Batch: {} files, total {}", batch.len(), human_bytes(total_bytes));

    for strategy in ["reflink_or_copy", "fs::copy"] {
        let batch_dst = dst_dir.join(format!("batch_{}", strategy.replace("::", "_")));
        let _ = std::fs::remove_dir_all(&batch_dst);
        std::fs::create_dir_all(&batch_dst).unwrap();

        let t0 = Instant::now();
        let mut reflinked = 0u64;
        let mut copied = 0u64;
        let mut errors = 0u64;
        for (path, _) in &batch {
            let stem = path.file_name().unwrap();
            let dst = batch_dst.join(stem);
            let result = match strategy {
                "reflink_or_copy" => match reflink_copy::reflink_or_copy(path, &dst) {
                    Ok(None) => {
                        reflinked += 1;
                        Ok(())
                    }
                    Ok(Some(_)) => {
                        copied += 1;
                        Ok(())
                    }
                    Err(e) => Err(e),
                },
                "fs::copy" => match std::fs::copy(path, &dst) {
                    Ok(_) => {
                        copied += 1;
                        Ok(())
                    }
                    Err(e) => Err(e),
                },
                _ => unreachable!(),
            };
            if result.is_err() {
                errors += 1;
            }
        }
        let dt = t0.elapsed();
        println!(
            "  {:>16}: {:?} ({} reflinked, {} copied, {} errors) → {:.1} files/s, {:.1} MB/s",
            strategy,
            dt,
            reflinked,
            copied,
            errors,
            batch.len() as f64 / dt.as_secs_f64(),
            (total_bytes as f64 / (1024.0 * 1024.0)) / dt.as_secs_f64()
        );

        let _ = std::fs::remove_dir_all(&batch_dst);
    }
}
