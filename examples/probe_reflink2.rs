// Deeper probe: 5000-file batches, sequential vs parallel (rayon),
// reflink_or_copy vs fs::copy, with error detail.
//
// Goal: figure out which combination gives the best throughput on this
// Windows + ReFS combo, and surface any silent failures.
//
// Usage: cargo run --release --example probe_reflink2 -- <src_dir> <dst_dir>

use rayon::prelude::*;
use std::io;
use std::path::{Path, PathBuf};
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

fn collect_files(root: &Path, limit: usize) -> Vec<(PathBuf, u64)> {
    let mut out = Vec::new();
    let walker = ignore::WalkBuilder::new(root)
        .standard_filters(false)
        .hidden(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .follow_links(false)
        .filter_entry(|e| !(e.depth() == 1 && e.file_name() == ".git"))
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

#[derive(Debug, Default)]
struct Stats {
    reflinked: AtomicU64,
    copied: AtomicU64,
    errors: AtomicU64,
}

fn copy_one(
    strategy: &str,
    src: &Path,
    dst: &Path,
    stats: &Stats,
    first_error: &std::sync::Mutex<Option<(PathBuf, String)>>,
) {
    if let Some(parent) = dst.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let result: io::Result<()> = match strategy {
        "reflink_or_copy" => match reflink_copy::reflink_or_copy(src, dst) {
            Ok(None) => {
                stats.reflinked.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Ok(Some(_)) => {
                stats.copied.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(e) => Err(e),
        },
        "fs::copy" => match std::fs::copy(src, dst) {
            Ok(_) => {
                stats.copied.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(e) => Err(e),
        },
        _ => unreachable!(),
    };
    if let Err(e) = result {
        stats.errors.fetch_add(1, Ordering::Relaxed);
        let mut g = first_error.lock().unwrap();
        if g.is_none() {
            *g = Some((src.to_path_buf(), e.to_string()));
        }
    }
}

fn run_test(
    label: &str,
    strategy: &str,
    parallel: bool,
    files: &[(PathBuf, u64)],
    dst_root: &Path,
) {
    let _ = std::fs::remove_dir_all(dst_root);
    std::fs::create_dir_all(dst_root).unwrap();

    let stats = Stats::default();
    let first_error: std::sync::Mutex<Option<(PathBuf, String)>> = std::sync::Mutex::new(None);
    let total_bytes: u64 = files.iter().map(|(_, s)| *s).sum();
    let t0 = Instant::now();

    let do_one = |(src, _size): &(PathBuf, u64)| {
        let stem = src.file_name().unwrap();
        // Hash path to avoid collisions when many files have the same basename.
        let h = {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut hasher = DefaultHasher::new();
            src.hash(&mut hasher);
            hasher.finish()
        };
        let dst = dst_root.join(format!("{h:016x}-{}", stem.to_string_lossy()));
        copy_one(strategy, src, &dst, &stats, &first_error);
    };

    if parallel {
        files.par_iter().for_each(do_one);
    } else {
        files.iter().for_each(do_one);
    }

    let dt = t0.elapsed();
    let reflinked = stats.reflinked.load(Ordering::Relaxed);
    let copied = stats.copied.load(Ordering::Relaxed);
    let errors = stats.errors.load(Ordering::Relaxed);

    println!(
        "  {label:<32}: {:>8.2?}  ({} reflinked, {} copied, {} errors)  {:.0} files/s  {:.1} MB/s",
        dt,
        reflinked,
        copied,
        errors,
        files.len() as f64 / dt.as_secs_f64(),
        (total_bytes as f64 / (1024.0 * 1024.0)) / dt.as_secs_f64()
    );

    if errors > 0
        && let Some((path, err)) = first_error.lock().unwrap().as_ref()
    {
        println!("      first error: {}: {}", path.display(), err);
    }

    let _ = std::fs::remove_dir_all(dst_root);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: probe_reflink2 <src_dir> <dst_dir>");
        std::process::exit(2);
    }
    let src_dir = PathBuf::from(&args[1]);
    let dst_dir = PathBuf::from(&args[2]);
    std::fs::create_dir_all(&dst_dir).expect("create dst_dir");

    println!("src = {}", src_dir.display());
    println!("dst = {}", dst_dir.display());
    println!("rayon threads = {}", rayon::current_num_threads());
    println!();

    println!("Collecting up to 5000 candidate files...");
    let t0 = Instant::now();
    let files = collect_files(&src_dir, 5000);
    println!("Found {} files in {:?}", files.len(), t0.elapsed());

    let total_bytes: u64 = files.iter().map(|(_, s)| *s).sum();
    println!("Total bytes: {}", human_bytes(total_bytes));

    println!();
    println!("{:=^100}", " timing matrix ");
    let strategies = ["reflink_or_copy", "fs::copy"];
    let parallel = [false, true];
    for s in &strategies {
        for p in &parallel {
            let label = format!("{s} ({})", if *p { "parallel" } else { "sequential" });
            run_test(
                &label,
                s,
                *p,
                &files,
                &dst_dir.join(format!("test_{s}_{}", if *p { "p" } else { "s" }).replace("::", "_")),
            );
        }
    }
}
