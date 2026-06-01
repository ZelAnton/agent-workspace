// ===========================================================================
// vcs/detect - which VCS backend a directory belongs to
// ===========================================================================
//
// Replaces the former `vcs_runner::detect_vcs`: a cheap filesystem walk up the
// ancestor chain looking for `.jj` / `.git`. No subprocess — purely structural.
//
// **Policy** (matches the historical behaviour): a directory carrying `.jj`
// resolves to `Jj` even when `.git` sits alongside it (colocated repos prefer
// jj — the user installed jj for a reason, and the jj backend drives the
// colocated git layer where needed). A directory with only `.git` resolves to
// `Git`. The first ancestor that carries either wins.

use std::path::Path;

use super::Backend;

/// Detect the VCS backend governing `start` (or any ancestor). Returns `None`
/// when neither `.jj` nor `.git` is found anywhere up the chain — callers then
/// fall through to their own default.
pub(super) fn detect_backend(start: &Path) -> Option<Backend> {
    let mut current = Some(start);
    while let Some(dir) = current {
        // `.jj` is always a directory. `.git` is a directory in a normal repo
        // but a *file* (gitlink) inside a linked worktree — accept either.
        if dir.join(".jj").is_dir() {
            return Some(Backend::Jj);
        }
        if dir.join(".git").exists() {
            return Some(Backend::Git);
        }
        current = dir.parent();
    }
    None
}
