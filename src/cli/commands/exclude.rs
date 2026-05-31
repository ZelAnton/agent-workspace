// ===========================================================================
// ws exclude - Manage [copy] exclude patterns for the current repo
// ===========================================================================
//
// `ws new` copies the entire source repo into each new worktree (minus
// `.git` and, for colocated repos, `.jj`). On real-world monorepos
// that drags multi-GB build artefacts into every throwaway worktree.
// `ws exclude` lets the user opt specific dirs/files out of that copy
// step.
//
// Patterns are stored as gitignore-style strings in `[copy] exclude` in
// the current repo's `.workspace.toml` (read fallback: the legacy
// committed `.agent-workspace.toml`). Read by `Config::load()` and
// threaded into the CoW walker via `Config::copy_excludes`. Writing a
// fresh `.workspace.toml` auto-adds it to the repo's local git exclude
// file (`.git/info/exclude`) so it stays out of git without a commit.
//
// Three operating modes:
//   - **TUI** (no args): interactive tree picker — see `tui.rs`. Not
//     yet implemented in this version; positional CLI mode is the
//     full v0.13.18 surface.
//   - **CLI add** (`ws exclude <path>...`): append paths to the list.
//   - **--remove / --list / --clear**: the rest of git-config-style
//     management.
//
// Persistence goes through `toml_edit` (already a dep) so user-
// written comments in the config file survive set/unset.

use std::path::{Path, PathBuf};

use clap::Args;
use toml_edit::{value, Array, DocumentMut, Item};

use crate::cli::{Error, Result};

#[derive(Args)]
pub struct ExcludeArgs {
    /// Paths or gitignore-style patterns. Without `--remove`, these
    /// are ADDED to the exclude list; with `--remove`, they're
    /// DROPPED. Mutually exclusive only with `--list` / `--clear`.
    /// Examples: `target`, `/Bin`, `node_modules/`, `**/*.iso`.
    #[arg(conflicts_with_all = ["list", "clear"])]
    paths: Vec<String>,

    /// Remove paths/patterns from the exclude list instead of adding.
    #[arg(long, conflicts_with_all = ["list", "clear"])]
    remove: bool,

    /// Print the current `[copy] exclude` list and exit.
    #[arg(long, conflicts_with_all = ["remove", "clear"])]
    list: bool,

    /// Wipe the entire `[copy] exclude` list (the `[copy]` section
    /// itself is dropped if no other keys remain inside it).
    #[arg(long, conflicts_with_all = ["remove", "list"])]
    clear: bool,
}

pub fn run(args: ExcludeArgs, repo: &crate::vcs::Repo) -> Result<()> {
    let repo_root = repo.repo_root().map_err(|e| Error::Other(e.to_string()))?;
    // Prefer `.workspace.toml`; edit a legacy `.agent-workspace.toml` in
    // place when only it exists. New repos get `.workspace.toml`.
    let config_path = crate::config::project_config_path_with_fallback(&repo_root);

    if args.list {
        return list(&config_path);
    }
    if args.clear {
        return clear(&config_path);
    }
    if args.remove {
        if args.paths.is_empty() {
            return Err(Error::Other(
                "`--remove` requires one or more paths".into(),
            ));
        }
        return remove_paths(&config_path, &args.paths);
    }
    if args.paths.is_empty() {
        // TUI mode. Load current patterns, launch picker, write back
        // on Save / no-op on Cancel.
        return run_tui(&repo_root, &config_path);
    }

    add_paths(&config_path, &args.paths)
}

/// TUI dispatcher. Reads current patterns from disk, asks
/// `exclude_tui::run` for the new set, writes back if the user
/// confirmed with `s`.
fn run_tui(repo_root: &Path, config_path: &Path) -> Result<()> {
    let doc = load_doc(config_path)?;
    let current = read_excludes(&doc);

    // `.git` and (for colocated repos) `.jj` are hardcoded-excluded
    // and shouldn't appear in the picker at all.
    let mut hidden: Vec<&str> = vec![".git"];
    if repo_root.join(".jj").is_dir() {
        hidden.push(".jj");
    }

    match super::exclude_tui::run(repo_root, &current, &hidden)? {
        Some(new_patterns) => {
            let mut doc = load_doc(config_path)?;
            write_excludes(&mut doc, &new_patterns)?;
            save_doc(config_path, &doc)?;
            println!(
                "Saved {} exclude pattern(s) to {}.",
                new_patterns.len(),
                config_path.display()
            );
            Ok(())
        }
        None => {
            println!("Cancelled — no changes written.");
            Ok(())
        }
    }
}

fn list(config_path: &Path) -> Result<()> {
    let doc = load_doc(config_path)?;
    let patterns = read_excludes(&doc);
    if patterns.is_empty() {
        println!("(no exclude patterns set for this repo)");
        println!("Add some with: ws exclude <path>...");
    } else {
        println!("Current [copy] exclude patterns ({} entries):", patterns.len());
        for p in &patterns {
            println!("  {p}");
        }
    }
    Ok(())
}

fn add_paths(config_path: &Path, paths: &[String]) -> Result<()> {
    let mut doc = load_doc(config_path)?;
    let mut existing = read_excludes(&doc);
    let before = existing.len();

    // Dedupe: don't append the same pattern twice (case-sensitive
    // match on the raw string — gitignore semantics are themselves
    // case-sensitive on most platforms).
    for p in paths {
        if !existing.iter().any(|e| e == p) {
            existing.push(p.clone());
        }
    }

    write_excludes(&mut doc, &existing)?;
    save_doc(config_path, &doc)?;

    let added = existing.len() - before;
    println!("Added {added} pattern(s); list now has {} entries.", existing.len());
    println!("(written to {})", config_path.display());
    Ok(())
}

fn remove_paths(config_path: &Path, paths: &[String]) -> Result<()> {
    let mut doc = load_doc(config_path)?;
    let existing = read_excludes(&doc);
    let before = existing.len();

    let kept: Vec<String> = existing
        .into_iter()
        .filter(|p| !paths.iter().any(|x| x == p))
        .collect();

    let removed = before - kept.len();
    if removed == 0 {
        println!("Nothing matched the supplied paths; list unchanged.");
        return Ok(());
    }

    write_excludes(&mut doc, &kept)?;
    save_doc(config_path, &doc)?;

    println!("Removed {removed} pattern(s); list now has {} entries.", kept.len());
    println!("(written to {})", config_path.display());
    Ok(())
}

fn clear(config_path: &Path) -> Result<()> {
    let mut doc = load_doc(config_path)?;
    if doc.get("copy").is_none() {
        println!("`[copy]` section already absent; nothing to clear.");
        return Ok(());
    }
    // Remove just `exclude`; if the section ends up empty, drop the
    // section header too so we don't litter the file with `[copy]`
    // followed by nothing.
    if let Some(table) = doc.get_mut("copy").and_then(|i| i.as_table_mut()) {
        table.remove("exclude");
        if table.is_empty() {
            doc.remove("copy");
        }
    }
    save_doc(config_path, &doc)?;
    println!("Cleared [copy] exclude (written to {}).", config_path.display());
    Ok(())
}

/// Pull the current list of patterns out of the document, or empty.
fn read_excludes(doc: &DocumentMut) -> Vec<String> {
    doc.get("copy")
        .and_then(|i| i.as_table())
        .and_then(|t| t.get("exclude"))
        .and_then(|i| i.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Replace the `[copy] exclude` array with the given patterns. Creates
/// the `[copy]` section if absent.
fn write_excludes(doc: &mut DocumentMut, patterns: &[String]) -> Result<()> {
    if doc.get("copy").is_none() {
        doc["copy"] = Item::Table(toml_edit::Table::new());
    }
    let table = doc["copy"].as_table_mut().ok_or_else(|| {
        Error::Other("`copy` exists in config but is not a table".into())
    })?;

    if patterns.is_empty() {
        table.remove("exclude");
        // Drop the empty section while we're at it.
        if table.is_empty() {
            doc.remove("copy");
        }
        return Ok(());
    }

    let mut arr = Array::new();
    for p in patterns {
        arr.push(p.as_str());
    }
    table["exclude"] = value(arr);
    Ok(())
}

fn load_doc(path: &Path) -> Result<DocumentMut> {
    match std::fs::read_to_string(path) {
        Ok(s) => s
            .parse::<DocumentMut>()
            .map_err(|e| Error::Other(format!("failed to parse {}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(DocumentMut::new()),
        Err(e) => Err(Error::Other(format!("failed to read {}: {e}", path.display()))),
    }
}

fn save_doc(path: &Path, doc: &DocumentMut) -> Result<()> {
    let content = doc.to_string();
    let _ = std::fs::create_dir_all(path.parent().unwrap_or(&PathBuf::from(".")));
    std::fs::write(path, content)
        .map_err(|e| Error::Other(format!("failed to write {}: {e}", path.display())))?;
    crate::config::ensure_workspace_config_ignored(path);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        args: ExcludeArgs,
    }

    #[test]
    fn parses_no_args() {
        let cli = TestCli::try_parse_from(["test"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn parses_positional_paths() {
        let cli = TestCli::try_parse_from(["test", "target", "node_modules"]);
        let parsed = cli.unwrap();
        assert_eq!(parsed.args.paths, vec!["target", "node_modules"]);
    }

    #[test]
    fn parses_list_flag() {
        let cli = TestCli::try_parse_from(["test", "--list"]);
        assert!(cli.is_ok());
        assert!(cli.unwrap().args.list);
    }

    #[test]
    fn parses_remove_with_paths() {
        let cli = TestCli::try_parse_from(["test", "--remove", "target"]);
        let parsed = cli.unwrap();
        assert!(parsed.args.remove);
        assert_eq!(parsed.args.paths, vec!["target"]);
    }

    #[test]
    fn rejects_remove_plus_list() {
        let cli = TestCli::try_parse_from(["test", "--remove", "x", "--list"]);
        assert!(cli.is_err());
    }

    #[test]
    fn rejects_clear_plus_paths() {
        let cli = TestCli::try_parse_from(["test", "--clear", "target"]);
        assert!(cli.is_err());
    }

    #[test]
    fn add_writes_patterns() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".agent-workspace.toml");
        add_paths(&path, &["target".to_string(), "node_modules".to_string()]).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("[copy]"));
        assert!(content.contains("target"));
        assert!(content.contains("node_modules"));
    }

    #[test]
    fn add_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".agent-workspace.toml");
        add_paths(&path, &["target".to_string()]).unwrap();
        add_paths(&path, &["target".to_string()]).unwrap();
        let doc = load_doc(&path).unwrap();
        let patterns = read_excludes(&doc);
        assert_eq!(patterns, vec!["target"]);
    }

    #[test]
    fn remove_drops_matching_pattern() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".agent-workspace.toml");
        add_paths(&path, &["target".to_string(), "node_modules".to_string()]).unwrap();
        remove_paths(&path, &["target".to_string()]).unwrap();
        let doc = load_doc(&path).unwrap();
        let patterns = read_excludes(&doc);
        assert_eq!(patterns, vec!["node_modules"]);
    }

    #[test]
    fn clear_removes_empty_section() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".agent-workspace.toml");
        add_paths(&path, &["target".to_string()]).unwrap();
        clear(&path).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            !content.contains("[copy]"),
            "empty [copy] section should be removed; got:\n{content}"
        );
    }

    #[test]
    fn add_preserves_other_sections() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".agent-workspace.toml");
        std::fs::write(&path, "[general]\ntrunk = \"main\"\n").unwrap();
        add_paths(&path, &["target".to_string()]).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("trunk = \"main\""), "general section must survive");
        assert!(content.contains("target"));
    }
}
