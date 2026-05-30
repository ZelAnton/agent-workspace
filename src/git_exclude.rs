// ===========================================================================
// git_exclude - Idempotent local git-exclude management
// ===========================================================================
//
// Keeps local-only config files (notably `.workspace.toml`) out of git WITHOUT
// requiring a commit, by appending a pattern to the repo's local exclude file
// (`<git-common-dir>/info/exclude`) rather than the committed `.gitignore`.
//
// Why the local exclude file:
//   - It lives inside `.git`, so writing it never dirties the working tree
//     (a committed-`.gitignore` edit would, and would break a later
//     `ws merge` / `ws sync`).
//   - It needs no commit and creates no diff noise — the rule is per-clone.
//   - It lives in the *common* git dir, which every linked worktree shares,
//     so a single entry covers the main repo and all worktrees at once.
//   - Both git and jj (colocated) honour it.
//
// This module is generic over the target file; the `.git/info/exclude`
// resolution lives in `config::ensure_workspace_config_ignored`. All callers
// treat errors as best-effort.

use std::io;
use std::path::Path;

/// Compute the new exclude-file body needed to guarantee `pattern` is present.
///
/// Pure core — no I/O — so the line-matching/append logic is unit-testable.
/// Returns `None` when `pattern` already has an exact (trimmed) line and no
/// write is needed. Otherwise returns the full new body:
/// - missing file (`None`) → `"{pattern}\n"`
/// - existing body → prior bytes verbatim + a separating newline (only if the
///   body doesn't already end in one) + `"{pattern}\n"`.
fn compute_exclude_update(existing: Option<&str>, pattern: &str) -> Option<String> {
    match existing {
        None => Some(format!("{pattern}\n")),
        Some(content) => {
            if content.lines().any(|line| line.trim() == pattern) {
                return None;
            }
            let sep = if content.is_empty() || content.ends_with('\n') {
                ""
            } else {
                "\n"
            };
            Some(format!("{content}{sep}{pattern}\n"))
        }
    }
}

/// Ensure the exclude-format `file` contains an exact-match line for `pattern`.
///
/// Idempotent: a no-op when the line already exists. Creates the file (and any
/// missing parent dirs) when absent, preserves prior content and trailing-
/// newline shape otherwise.
pub fn ensure_pattern(file: &Path, pattern: &str) -> io::Result<()> {
    let existing = match std::fs::read_to_string(file) {
        Ok(s) => Some(s),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    if let Some(new_body) = compute_exclude_update(existing.as_deref(), pattern) {
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(file, new_body)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_body_when_file_missing() {
        assert_eq!(
            compute_exclude_update(None, ".workspace.toml"),
            Some(".workspace.toml\n".to_string())
        );
    }

    #[test]
    fn none_when_pattern_already_present() {
        assert_eq!(
            compute_exclude_update(Some("target/\n.workspace.toml\n"), ".workspace.toml"),
            None
        );
    }

    #[test]
    fn none_when_present_without_trailing_newline() {
        assert_eq!(
            compute_exclude_update(Some(".workspace.toml"), ".workspace.toml"),
            None
        );
    }

    #[test]
    fn none_when_present_among_default_exclude_comments() {
        // `git init` seeds info/exclude with commented sample lines.
        let seeded = "# git ls-files --others --exclude-from=.git/info/exclude\n# Lines that start with '#' are comments.\n.workspace.toml\n";
        assert_eq!(compute_exclude_update(Some(seeded), ".workspace.toml"), None);
    }

    #[test]
    fn appends_when_absent_preserving_content() {
        assert_eq!(
            compute_exclude_update(Some("target/\n"), ".workspace.toml"),
            Some("target/\n.workspace.toml\n".to_string())
        );
    }

    #[test]
    fn appends_separating_newline_when_missing() {
        assert_eq!(
            compute_exclude_update(Some("target/"), ".workspace.toml"),
            Some("target/\n.workspace.toml\n".to_string())
        );
    }

    #[test]
    fn appends_to_empty_file_without_blank_line() {
        assert_eq!(
            compute_exclude_update(Some(""), ".workspace.toml"),
            Some(".workspace.toml\n".to_string())
        );
    }

    #[test]
    fn ensure_pattern_creates_file_and_parents_then_noops() {
        let dir = tempfile::tempdir().unwrap();
        // Mimic the real target: <common-dir>/info/exclude, with `info/` absent.
        let file = dir.path().join("info").join("exclude");
        ensure_pattern(&file, ".workspace.toml").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), ".workspace.toml\n");

        // Second call is a no-op: no duplicate line.
        ensure_pattern(&file, ".workspace.toml").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), ".workspace.toml\n");
    }

    #[test]
    fn ensure_pattern_appends_to_existing() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("exclude");
        std::fs::write(&file, "target/\n").unwrap();
        ensure_pattern(&file, ".workspace.toml").unwrap();
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "target/\n.workspace.toml\n"
        );
    }
}
