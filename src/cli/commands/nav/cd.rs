// ===========================================================================
// ws cd - Change to worktree directory
// ===========================================================================

use std::path::{Path, PathBuf};

use clap::Args;
use clap_complete::engine::ArgValueCompleter;

use crate::cli::{write_path_file, Error, Result};
use crate::complete;
use crate::config::Config;
use crate::vcs;

#[derive(Args)]
pub struct CdArgs {
    /// Branch name to switch to (omit to return to main repo)
    #[arg(add = ArgValueCompleter::new(complete::complete_worktrees))]
    branch: Option<String>,

    /// Force opening in a new terminal tab (Windows Terminal /
    /// iTerm2 / GNOME Terminal). Useful to bypass `[ui] open_in_new_tab
    /// = false` per-call OR to override the same-target short-circuit
    /// (which normally skips spawning when target == current worktree).
    /// No effect if no supported terminal is detected.
    #[arg(long, conflicts_with = "no_tab")]
    in_new_tab: bool,

    /// Force `cd` in the current shell, even when running inside a
    /// terminal that supports tabs and `[ui] open_in_new_tab` is true.
    #[arg(long, conflicts_with = "in_new_tab")]
    no_tab: bool,
}

pub fn run(args: CdArgs, config: &Config, path_file: Option<&Path>, repo: &vcs::Repo) -> Result<()> {
    // `ws cd` only makes sense behind the shell wrapper — a child process
    // can't change its parent shell's CWD. Without a path_file the wrapper
    // isn't installed (or the binary was invoked directly), so refuse loudly
    // instead of pretending to switch.
    //
    // **Exception**: when the tab-integration path takes over (spawns a
    // new tab whose own shell starts at the target dir), the originating
    // shell doesn't need the path-file dance — but `path_file` is always
    // passed by the wrapper, so the precondition still holds. The check
    // stays where it is to guard direct-binary invocations.
    if path_file.is_none() {
        return Err(Error::Other(
            "Shell integration not installed. Run 'ws setup' first.".into(),
        ));
    }

    // 1. Resolve target path. No-arg case → main repo root; explicit
    //    branch → worktree dir.
    let (target_path, title) = resolve_target_and_title(args.branch.as_deref(), config, repo)?;

    // 2. Validate target exists AND is a directory. Per plan: validate
    //    BEFORE the tab-spawn decision so non-existent branches don't
    //    open spurious tabs. `is_dir()` (not `exists()`) catches the
    //    edge case where a regular file lives at the worktree path —
    //    `ws cd` into it would fail downstream with a confusing
    //    terminal-spawn error; better to surface a clear one here.
    if !target_path.is_dir() {
        return Err(Error::Git(vcs::Error::WorktreeNotFound(
            args.branch.clone().unwrap_or_else(|| "<main>".into()),
        )));
    }

    // 3. Same-target short-circuit. When `ws cd <branch>` would land in
    //    the same directory the user is already in, spawning a new tab
    //    is pure noise — skip and let the originating shell `cd` to the
    //    same place (no-op for cd, but writes the path-file so the
    //    wrapper completes its handshake).
    //
    // Skipped for the no-arg case per locked decision (spawn always
    // when returning to main repo). The `--in-new-tab` flag also
    // overrides this skip (user explicitly forces a tab).
    let cwd_is_target = std::env::current_dir()
        .ok()
        .and_then(|c| c.canonicalize().ok())
        .and_then(|c| target_path.canonicalize().ok().map(|t| c == t))
        .unwrap_or(false);
    let skip_for_same = args.branch.is_some() && cwd_is_target && !args.in_new_tab;

    // 4. Tab-integration dispatch. Mirrors `ws new`'s precedence logic
    //    via the shared `should_open_in_new_tab` helper. Note the order
    //    difference vs `ws new`: validation happens BEFORE this check
    //    (see step 2's comment).
    if !skip_for_same
        && crate::terminal::should_open_in_new_tab(
            args.no_tab,
            args.in_new_tab,
            config.open_in_new_tab,
        )
        && let Some(terminal) = crate::terminal::detect()
    {
        let spec = crate::terminal::TabSpec {
            title: title.clone(),
            cwd: target_path.clone(),
            mode: crate::terminal::TabMode::OpenAtCwd,
        };
        terminal
            .open_tab(&spec)
            .map_err(|e| Error::Other(format!("failed to open new tab: {e}")))?;
        eprintln!("Opened in new tab: {title} (terminal: {})", terminal.name());
        // Do NOT write the path-file: the originating shell stays put.
        return Ok(());
    }

    // 5. Fall-through: existing behaviour — write path-file, wrapper cd's.
    write_path_file(path_file, &target_path)?;
    Ok(())
}

/// Resolve the target directory and the tab title for `ws cd`.
///
/// No-arg case: target = main repo root, title = repo name (or `"main"`
/// fallback). Explicit branch: target = worktree dir, title = branch
/// name. Branch existence is NOT checked here (caller does it after).
fn resolve_target_and_title(
    branch: Option<&str>,
    config: &Config,
    repo: &vcs::Repo,
) -> Result<(PathBuf, String)> {
    match branch {
        None => {
            let repo_root = repo.repo_root()?;
            let title = repo.repo_name().unwrap_or_else(|_| "main".into());
            Ok((repo_root, title))
        }
        Some(b) => {
            // Reject path-traversal in the branch arg: `ws cd ../../foo` would
            // otherwise resolve outside the workspace dir and the wrapper would
            // `cd` the parent shell to an arbitrary location, bypassing the
            // "cd to a worktree" contract. (A real worktree name never contains
            // `..` or a leading separator.)
            if b.split(['/', '\\']).any(|seg| seg == "..") || b.starts_with(['/', '\\']) {
                return Err(Error::Git(vcs::Error::WorktreeNotFound(b.to_string())));
            }
            let workspace_id = repo.workspace_id()?;
            let wt_dir = config.project_dir_for(&workspace_id);
            let wt_path = wt_dir.join(b);
            Ok((wt_path, b.to_string()))
        }
    }
}
