// ===========================================================================
// cow/tests - Unit tests for the CoW module
// ===========================================================================
//
// All tests use tempdirs on the dev box's actual filesystem. On NTFS /
// ext4 / etc, `can_clone` returns false and the integration path falls
// back to plain copy — these tests verify the fallback works. On ReFS /
// Btrfs / APFS the tests still pass; they additionally exercise the
// real reflink code path.

use super::*;
use std::fs;

#[test]
fn same_volume_returns_true_for_self() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(same_volume(tmp.path(), tmp.path()), Some(true));
}

#[test]
fn same_volume_returns_true_for_sibling_dirs_under_tempdir() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    fs::create_dir(&a).unwrap();
    fs::create_dir(&b).unwrap();
    assert_eq!(same_volume(&a, &b), Some(true));
}

#[test]
fn same_volume_returns_none_for_missing_path() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("does-not-exist");
    assert_eq!(same_volume(tmp.path(), &missing), None);
}

#[test]
fn try_clone_dir_except_skips_top_level_dotgit() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");
    fs::create_dir(&src).unwrap();
    fs::create_dir(&dst).unwrap();

    // src/.git/HEAD          → must NOT appear in dst
    // src/README.md          → must appear in dst
    // src/sub/.git           → MUST appear (depth > 1, only top-level filtered)
    // src/sub/file.txt       → must appear
    fs::create_dir_all(src.join(".git")).unwrap();
    fs::write(src.join(".git/HEAD"), b"ref: refs/heads/main\n").unwrap();
    fs::write(src.join("README.md"), b"# repo\n").unwrap();
    fs::create_dir_all(src.join("sub")).unwrap();
    fs::write(src.join("sub/.git"), b"gitdir: ../.git/worktrees/sub\n").unwrap();
    fs::write(src.join("sub/file.txt"), b"hello\n").unwrap();

    try_clone_dir_except(&src, &dst, &[".git"]).unwrap();

    assert!(!dst.join(".git").exists(), "top-level .git must be excluded");
    assert!(dst.join("README.md").exists());
    assert!(dst.join("sub/.git").exists(), "nested .git file is in scope");
    assert!(dst.join("sub/file.txt").exists());
}

#[test]
fn try_clone_dir_except_preserves_nested_structure() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");
    fs::create_dir(&src).unwrap();
    fs::create_dir(&dst).unwrap();

    fs::create_dir_all(src.join("a/b/c")).unwrap();
    fs::write(src.join("a/b/c/leaf.txt"), b"leaf\n").unwrap();
    fs::write(src.join("root.txt"), b"root\n").unwrap();
    // Hidden file at root — included (we set `.hidden(false)`).
    fs::write(src.join(".env"), b"SECRET=1\n").unwrap();

    try_clone_dir_except(&src, &dst, &[]).unwrap();

    assert_eq!(fs::read_to_string(dst.join("a/b/c/leaf.txt")).unwrap(), "leaf\n");
    assert_eq!(fs::read_to_string(dst.join("root.txt")).unwrap(), "root\n");
    assert_eq!(fs::read_to_string(dst.join(".env")).unwrap(), "SECRET=1\n");
}

#[test]
fn try_clone_dir_except_handles_empty_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");
    fs::create_dir(&src).unwrap();
    fs::create_dir(&dst).unwrap();
    fs::create_dir(src.join("empty")).unwrap();

    try_clone_dir_except(&src, &dst, &[]).unwrap();
    assert!(dst.join("empty").is_dir());
}

#[test]
fn try_clone_dir_except_multiple_excludes() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");
    fs::create_dir(&src).unwrap();
    fs::create_dir(&dst).unwrap();
    fs::create_dir(src.join(".git")).unwrap();
    fs::create_dir(src.join(".jj")).unwrap();
    fs::write(src.join(".git/HEAD"), b"x").unwrap();
    fs::write(src.join(".jj/op_log"), b"y").unwrap();
    fs::write(src.join("kept.txt"), b"z").unwrap();

    try_clone_dir_except(&src, &dst, &[".git", ".jj"]).unwrap();

    assert!(!dst.join(".git").exists());
    assert!(!dst.join(".jj").exists());
    assert!(dst.join("kept.txt").exists());
}

#[test]
fn can_clone_runs_probe_without_panicking() {
    // On NTFS / ext4 (typical CI), this returns false (no block cloning).
    // On ReFS / Btrfs / APFS, returns true. Either is fine — the test
    // pins that the probe doesn't crash and cleans up its sentinels.
    let tmp = tempfile::tempdir().unwrap();
    let _ = can_clone(tmp.path(), tmp.path());
    // Sentinel files should not leak in tempdir after probe completes.
    let leftover: Vec<_> = fs::read_dir(tmp.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(".wt-cow-probe-")
        })
        .collect();
    assert!(leftover.is_empty(), "probe should clean up its sentinels");
}
