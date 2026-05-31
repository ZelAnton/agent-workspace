// ===========================================================================
// ws ls - List worktrees with git status info
// ===========================================================================

use std::collections::HashSet;

use clap::Args;
use serde::Serialize;

use chrono::{DateTime, Utc};

use crate::cli::output::{self, OutputFormat, Render};
use crate::cli::Result;
use crate::config::Config;
use crate::vcs;
use crate::meta;

#[derive(Args)]
pub struct LsArgs {
    /// Show full path for each worktree
    #[arg(short, long)]
    pub long: bool,
}

pub fn run(args: LsArgs, config: &Config, format: OutputFormat, repo: &vcs::Repo) -> Result<()> {
    let workspace_id = repo.workspace_id()?;
    let wt_dir = config.project_dir_for(&workspace_id);

    if !wt_dir.exists() {
        return emit_no_worktrees(format);
    }

    let worktrees = repo.list_worktrees()?;

    let managed: Vec<_> = worktrees
        .iter()
        .filter(|wt| wt.path.starts_with(&wt_dir))
        .collect();

    if managed.is_empty() {
        return emit_no_worktrees(format);
    }

    let trunk = config.resolve_trunk(repo);
    // Fetch all local branches once instead of N subprocess calls.
    let known_branches: HashSet<String> = repo.local_branches()
        .unwrap_or_default()
        .into_iter()
        .collect();

    let current = repo.current_branch().ok();
    let home = dirs::home_dir();

    let mut rows: Vec<LsItem> = Vec::new();
    for wt in &managed {
        let branch = wt.branch.as_deref().unwrap_or("(detached)");
        let is_current = current.as_deref() == Some(branch);

        let meta_path = meta::meta_path_with_fallback(&wt_dir, branch);
        let loaded_meta = meta::WorktreeMeta::load(&meta_path).ok();

        let base_branch = loaded_meta.as_ref().map(|m| m.base_branch.clone());
        let created_at = loaded_meta.as_ref().map(|m| m.created_at);

        let effective_target = meta::resolve_target_branch(
            None,
            base_branch.as_deref(),
            |b| known_branches.contains(b),
            &trunk,
        );

        let uncommitted = repo.uncommitted_count_in(&wt.path).unwrap_or(0);
        let commits = repo.commit_count(&effective_target, branch).unwrap_or(0);

        let c = repo.diff_shortstat(&effective_target, branch).unwrap_or(vcs::DiffStat {
            insertions: 0,
            deletions: 0,
        });
        let u = repo.diff_shortstat_in(&wt.path).unwrap_or(vcs::DiffStat {
            insertions: 0,
            deletions: 0,
        });

        let path = if args.long {
            Some(shorten_path(&wt.path, &home))
        } else {
            None
        };

        rows.push(LsItem {
            branch: branch.to_string(),
            base_branch,
            is_current,
            uncommitted,
            commits,
            insertions: c.insertions + u.insertions,
            deletions: c.deletions + u.deletions,
            path,
            created_at,
        });
    }

    // Sort newest-first; rows without meta sink to the bottom (None < Some).
    rows.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    output::emit(&LsOutput { worktrees: rows }, format);
    Ok(())
}

/// In `human` mode a friendly notice on stderr; in `json` mode a valid empty
/// object on stdout (an agent piping to a parser must never get a bare notice).
fn emit_no_worktrees(format: OutputFormat) -> Result<()> {
    match format {
        OutputFormat::Human => eprintln!("No worktrees for this project."),
        OutputFormat::Json => output::emit(&LsOutput { worktrees: Vec::new() }, format),
    }
    Ok(())
}

/// Machine-facing `ws ls` payload. An object (not a bare array) so it stays
/// extensible — future fields like `project`/`trunk` can be added alongside.
#[derive(Serialize)]
struct LsOutput {
    worktrees: Vec<LsItem>,
}

impl Render for LsOutput {
    fn render_human(&self) {
        print_table(&self.worktrees);
    }
}

#[derive(Serialize)]
struct LsItem {
    branch: String,
    base_branch: Option<String>,
    is_current: bool,
    uncommitted: usize,
    commits: usize,
    insertions: usize,
    deletions: usize,
    path: Option<String>,
    created_at: Option<DateTime<Utc>>,
}

fn print_table(rows: &[LsItem]) {
    let bw = rows
        .iter()
        .map(|r| r.branch.len())
        .max()
        .unwrap_or(6)
        .max(6);
    let show_path = rows.iter().any(|r| r.path.is_some());
    let show_base = rows.iter().any(|r| r.base_branch.is_some());

    let sw = if show_base {
        rows.iter()
            .filter_map(|r| r.base_branch.as_ref().map(|s| s.len()))
            .max()
            .unwrap_or(6)
            .max(6)
    } else {
        0
    };

    let mut header = format!("  {:<bw$}", "BRANCH", bw = bw);
    if show_base {
        header.push_str(&format!("   {:<sw$}", "BASE", sw = sw));
    }
    header.push_str(&format!(
        "   {:>8}   {:>7}   {:>10}",
        "UNCOMMIT", "COMMITS", "DIFF"
    ));
    if show_path {
        header.push_str("   PATH");
    }
    println!("{header}");

    let sep_len = 2
        + bw
        + 3
        + 8
        + 3
        + 7
        + 3
        + 10
        + if show_base { 3 + sw } else { 0 }
        + if show_path { 40 } else { 0 };
    println!("{}", "-".repeat(sep_len));

    for row in rows {
        let marker = if row.is_current { "* " } else { "  " };

        let diff = if row.insertions == 0 && row.deletions == 0 {
            "-".to_string()
        } else {
            format!("+{} -{}", row.insertions, row.deletions)
        };

        let mut line = format!("{}{:<bw$}", marker, row.branch, bw = bw);
        if show_base {
            let src = row.base_branch.as_deref().unwrap_or("-");
            line.push_str(&format!("   {:<sw$}", src, sw = sw));
        }
        line.push_str(&format!(
            "   {:>8}   {:>7}   {:>10}",
            row.uncommitted, row.commits, diff
        ));

        if let Some(ref path) = row.path {
            println!("{line}   {path}");
        } else {
            println!("{line}");
        }
    }
}

fn shorten_path(path: &std::path::Path, home: &Option<std::path::PathBuf>) -> String {
    match home {
        Some(h) if path.starts_with(h) => {
            format!("~/{}", path.strip_prefix(h).unwrap().display())
        }
        _ => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ls_output_json_shape() {
        let out = LsOutput {
            worktrees: vec![LsItem {
                branch: "feat".into(),
                base_branch: Some("main".into()),
                is_current: true,
                uncommitted: 2,
                commits: 3,
                insertions: 10,
                deletions: 1,
                path: None,
                created_at: None,
            }],
        };
        let v = serde_json::to_value(&out).unwrap();
        let item = &v["worktrees"][0];
        assert_eq!(item["branch"], "feat");
        assert_eq!(item["base_branch"], "main");
        assert_eq!(item["is_current"], true);
        assert_eq!(item["commits"], 3);
        assert!(item["path"].is_null());
    }

    #[test]
    fn ls_empty_is_object_with_array() {
        let out = LsOutput {
            worktrees: vec![],
        };
        let v = serde_json::to_value(&out).unwrap();
        assert!(v["worktrees"].is_array());
        assert_eq!(v["worktrees"].as_array().unwrap().len(), 0);
    }
}
