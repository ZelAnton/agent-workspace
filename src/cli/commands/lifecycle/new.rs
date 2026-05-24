// ===========================================================================
// wt new - Create a new worktree
// ===========================================================================

use std::path::Path;

use clap::Args;
use clap_complete::engine::ArgValueCompleter;

use crate::cli::{write_path_file, write_path_file_lines, Error, Result};
use crate::complete;
use crate::config::Config;
use crate::vcs;
use crate::meta::{self, WorktreeMeta};
use crate::process;
use crate::util;

#[derive(Args)]
pub struct NewArgs {
    /// Branch name (random name like 'swift-fox' if not provided)
    branch: Option<String>,

    /// Base branch to create from and merge back to (default: current branch)
    #[arg(long, value_name = "BRANCH", add = ArgValueCompleter::new(complete::complete_branches))]
    base: Option<String>,

    /// Run command in snap mode: create -> run -> merge -> cleanup
    #[arg(short, long, value_name = "CMD")]
    snap: Option<String>,

    /// Force opening creation in a new terminal tab (Windows Terminal /
    /// iTerm2 / GNOME Terminal). Useful to bypass `[ui] open_in_new_tab
    /// = false` per-call. No effect if no supported terminal is detected.
    #[arg(long, conflicts_with = "no_tab")]
    in_new_tab: bool,

    /// Force creation in the current shell, even when running inside a
    /// terminal that supports tabs and `[ui] open_in_new_tab` is true.
    #[arg(long, conflicts_with = "in_new_tab")]
    no_tab: bool,

    /// Disable Copy-on-Write worktree creation even on filesystems that
    /// support it (ReFS / DevDrive / Btrfs / XFS / APFS). Falls back to
    /// plain `git worktree add` with full checkout. Useful for debugging
    /// or when something unusual about the source repo makes the
    /// stash-checkout-restore dance undesirable.
    #[arg(long)]
    no_cow: bool,
}

pub fn run(args: NewArgs, config: &Config, path_file: Option<&Path>) -> Result<()> {
    // Terminal-tab dispatch happens FIRST, before any VCS work, so the
    // user sees the new tab open immediately instead of after the
    // (sometimes slow) workspace-id hash + repo discovery. The spawned
    // tab then re-enters `wt new` with `WT_SPAWNED_IN_TAB=1` set and
    // skips this branch — actual creation runs there.
    if should_open_new_tab(&args, config) && let Some(terminal) = crate::terminal::detect() {
        return spawn_in_new_tab(&args, terminal.as_ref());
    }

    // CoW toggle resolution. The dispatcher in `vcs::create_worktree`
    // reads `WT_DISABLE_COW` from the env (set here when CoW is
    // explicitly disabled by flag or config). The dispatcher always
    // probes filesystem support and gracefully falls back to plain
    // `git worktree add` when CoW isn't actually possible.
    if !should_use_cow(&args, config) {
        // SAFETY: env vars are process-global. We set this before
        // any subprocess spawns and never unset it; child processes
        // (post_create hooks etc) inherit it but it has no effect on
        // them (only `vcs::create_worktree` reads it).
        unsafe {
            std::env::set_var(crate::cow::DISABLE_COW_ENV, "1");
        }
    }

    // Ensure we're in a git repo
    let repo_root = vcs::repo_root()?;
    let workspace_id = vcs::workspace_id()?;
    let workspace_dir = config.workspaces_dir.join(&workspace_id);

    // Nested snap stacks two loops in the parent shell and breaks cwd tracking
    // when the inner one finishes.
    if args.snap.is_some() && vcs::is_cwd_inside(&workspace_dir) {
        return Err(Error::Other(
            "Refusing to start snap mode inside an existing worktree.\n\
             Run 'wt cd' to return to the main repo, then retry."
                .into(),
        ));
    }

    // Determine trunk branch
    let trunk = config.resolve_trunk();

    // Resolve base branch: --base flag > current branch > trunk.
    // Determines both the checkout starting point and the default merge/sync target.
    let base_branch = if let Some(ref b) = args.base {
        if !vcs::branch_exists(b)? {
            return Err(Error::Other(format!("Branch '{b}' does not exist")));
        }
        b.clone()
    } else {
        // Detached HEAD falls back to trunk.
        vcs::current_branch()
            .ok()
            .filter(|b| b != "HEAD")
            .unwrap_or_else(|| trunk.clone())
    };

    // Generate or use provided branch name
    let branch = args.branch.unwrap_or_else(|| {
        util::generate_unique_branch_name(|n| vcs::branch_exists(n).unwrap_or(false))
    });

    // If we're running inside a spawned terminal tab, update the tab
    // title to match the *real* branch name. Critical when the user ran
    // `wt new` without a branch arg — the spawn fires before the random
    // branch is generated, so the tab opens with the "wt-new" placeholder
    // title in [`spawn_in_new_tab`]. This OSC 0 escape fixes that.
    //
    // The escape is also harmless on terminals that don't interpret it
    // (the bytes are printed and consumed by the terminal driver if
    // supported; ignored otherwise). We gate on the recursion-guard env
    // var so the originating shell's tab title isn't mutated.
    if crate::terminal::is_spawned_in_tab() {
        // Defense-in-depth: strip control chars from the branch name
        // before embedding in the OSC sequence. The OSC 0 control
        // sequence is terminated by BEL (\x07) or ST (\x1b\\). A branch
        // name containing either could close the sequence early and
        // inject subsequent bytes as a different terminal command.
        // Git/jj ref validation already rejects most control chars, but
        // we don't trust the upstream layer — sanitize at the use site.
        let safe_title: String = branch
            .chars()
            .filter(|c| !c.is_control())
            .collect();
        // OSC 0 ; <title> BEL — sets both window title and tab title.
        // \x1b is ESC; \x07 is BEL. printed to stderr so it doesn't
        // mingle with structured stdout output (path-file etc).
        use std::io::Write as _;
        let _ = write!(std::io::stderr(), "\x1b]0;{safe_title}\x07");
    }

    // Worktree path
    let wt_dir = &workspace_dir;
    let wt_path = wt_dir.join(&branch);

    // Create workspace directory if needed
    std::fs::create_dir_all(wt_dir).map_err(|e| Error::Other(e.to_string()))?;

    let create_outcome = vcs::create_worktree(&wt_path, &branch, &base_branch)?;

    let meta = WorktreeMeta::new(base_branch);
    let meta_path = meta::meta_path(wt_dir, &branch);
    meta.save(&meta_path)
        .map_err(|e| Error::Other(e.to_string()))?;

    // Copy `[general] copy_files` patterns from main repo into the
    // new worktree — `.env`, `.env.local`, etc. that aren't tracked by
    // git but the user wants present in every worktree.
    //
    // **Skipped when CoW was used**: the reflink copy already populated
    // every file from the source repo, so these specific patterns are
    // already in `wt_path`. Running copy_files again would be redundant
    // work (and would overwrite identical files).
    if matches!(create_outcome, vcs::CreateOutcome::Plain) {
        copy_files(&repo_root, &wt_path, config)?;
    }

    // Run post_create hooks. On failure, leave the worktree in place — the
    // user usually wants to fix the hook (e.g. install missing tool) and
    // resume manually rather than have us silently rm a half-created tree.
    if !config.hooks.post_create.is_empty() {
        eprintln!("Running post-create hooks...");
        if let Err(e) = process::run_hooks(&config.hooks.post_create, &wt_path) {
            eprintln!();
            eprintln!("post_create hook failed: {e}");
            eprintln!("Worktree '{branch}' was created at: {}", wt_path.display());
            eprintln!("Fix the hook and `cd` in manually, or run 'wt rm {branch}' to discard.");
            return Err(Error::Other(format!("post_create hook failed: {e}")));
        }
    }

    // Handle snap mode - write path + command for shell wrapper to execute
    if let Some(cmd) = args.snap {
        if path_file.is_some() {
            write_path_file_lines(path_file, &[&wt_path.display().to_string(), &cmd])?;
        } else {
            return Err(Error::Other(
                "Snap mode requires shell integration. Run 'wt setup' first.".into(),
            ));
        }
        return Ok(());
    }

    // Write path for shell integration
    if path_file.is_some() {
        write_path_file(path_file, &wt_path)?;
    } else {
        eprintln!("Created worktree: {branch} (from {})", meta.base_branch);
        eprintln!("Path: {}", wt_path.display());
    }

    Ok(())
}

/// Reject patterns that could escape the repo root.
///
/// Without this guard, a malicious `.agent-workspace.toml` could exfiltrate
/// host files into the worktree via `/abs/path` or `..` traversal — the
/// downstream `strip_prefix` would silently skip mismatches.
fn validate_copy_pattern(pattern: &str) -> Result<()> {
    if pattern.starts_with('/') {
        return Err(Error::Other(format!(
            "copy_files pattern '{pattern}' cannot start with '/' (absolute path)"
        )));
    }
    if pattern.split(['/', '\\']).any(|seg| seg == "..") {
        return Err(Error::Other(format!(
            "copy_files pattern '{pattern}' cannot contain '..'"
        )));
    }
    Ok(())
}

fn copy_files(from: &Path, to: &Path, config: &Config) -> Result<()> {
    use ignore::overrides::OverrideBuilder;
    use ignore::WalkBuilder;

    if config.copy_files.is_empty() {
        return Ok(());
    }

    for pattern in &config.copy_files {
        validate_copy_pattern(pattern)?;
    }

    // Build gitignore-style matcher
    // Patterns work like .gitignore: "*.md" matches all .md files, "/*.md" matches only root
    let mut builder = OverrideBuilder::new(from);
    for pattern in &config.copy_files {
        builder
            .add(pattern)
            .map_err(|e| Error::Other(format!("invalid pattern '{}': {}", pattern, e)))?;
    }
    let overrides = builder.build().map_err(|e| Error::Other(e.to_string()))?;

    // follow_links=false: a symlink in the repo could otherwise pull files
    // from outside the repo into the worktree.
    let walker = WalkBuilder::new(from)
        .overrides(overrides)
        .standard_filters(false)
        .follow_links(false)
        .build();

    for entry in walker.flatten() {
        let path = entry.path();
        if path.is_file() {
            let rel = match path.strip_prefix(from) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!(
                        "Warning: failed to strip prefix for {}: {e}",
                        path.display()
                    );
                    continue;
                }
            };
            let dest = to.join(rel);

            if let Some(parent) = dest.parent()
                && let Err(e) = std::fs::create_dir_all(parent) {
                    eprintln!(
                        "Warning: failed to create directory {}: {e}",
                        parent.display()
                    );
                    continue;
                }

            if let Err(e) = std::fs::copy(path, &dest) {
                eprintln!("Warning: failed to copy {}: {e}", rel.display());
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Terminal-tab dispatch helpers
// ---------------------------------------------------------------------------

/// Decide whether `wt new` should open a new terminal tab instead of
/// running the creation inline. Precedence:
///   1. `--no-tab` flag → always false (user explicitly disabled)
///   2. `--in-new-tab` flag → true (user explicitly enabled)
///   3. Already running inside a spawned tab → false (recursion guard)
///   4. `[ui] open_in_new_tab` config (project over global) → that value
///
/// Terminal-detection still happens at the call site — even if this
/// returns true, [`crate::terminal::detect()`] may return `None` and we
/// fall through to in-place creation.
/// Decide whether `wt new` should use the Copy-on-Write creation path.
/// The actual filesystem-capability probe happens later in the VCS
/// dispatcher (`vcs::create_worktree`). This function ONLY decides
/// whether to even attempt CoW.
///
/// Precedence:
///   1. `--no-cow` flag → false (user explicitly disabled)
///   2. `[create] use_cow` config (project over global) → that value
fn should_use_cow(args: &NewArgs, config: &Config) -> bool {
    if args.no_cow {
        return false;
    }
    config.use_cow
}

fn should_open_new_tab(args: &NewArgs, config: &Config) -> bool {
    if args.no_tab {
        return false;
    }
    if args.in_new_tab {
        return true;
    }
    if crate::terminal::is_spawned_in_tab() {
        return false;
    }
    config.open_in_new_tab
}

/// Spawn the new tab and return immediately. The caller (originating
/// shell) sees a clean exit — does NOT write any `--path-file`, so the
/// outer shell wrapper doesn't `cd` and stays put. The new tab runs the
/// re-entrant `wt new <args>` with `WT_SPAWNED_IN_TAB=1` set.
fn spawn_in_new_tab(
    args: &NewArgs,
    terminal: &dyn crate::terminal::TerminalIntegration,
) -> Result<()> {
    use std::path::PathBuf;

    // Title: prefer the explicit branch; fall back to a placeholder.
    // The actual branch may be auto-generated downstream; the placeholder
    // is just for the brief window before creation finishes.
    let title = args.branch.clone().unwrap_or_else(|| "wt-new".into());

    let cwd = std::env::current_dir()
        .map_err(|e| Error::Other(format!("cannot read current directory: {e}")))?;

    // Use the absolute path to OUR binary. On Windows, `wt` on PATH may
    // resolve to Microsoft Store's Windows Terminal binary (also named
    // `wt.exe`); the spawned tab MUST run our binary.
    let binary = std::env::current_exe()
        .map_err(|e| Error::Other(format!("cannot resolve own binary path: {e}")))?;
    let binary: PathBuf = binary.canonicalize().unwrap_or(binary);
    let binary = crate::config::strip_verbatim_prefix(binary);

    // Reconstruct argv for the new tab's `wt new ...` invocation. Keep
    // tab-control flags (`--in-new-tab` / `--no-tab`) OUT of the re-entry
    // — they were resolved here; the spawned process would just confuse
    // itself with them (plus the recursion guard short-circuits anyway).
    let mut new_args = Vec::new();
    if let Some(b) = &args.branch {
        new_args.push(b.clone());
    }
    if let Some(base) = &args.base {
        new_args.push("--base".into());
        new_args.push(base.clone());
    }
    if let Some(snap_cmd) = &args.snap {
        new_args.push("--snap".into());
        new_args.push(snap_cmd.clone());
    }

    let spec = crate::terminal::TabSpec {
        title: title.clone(),
        cwd,
        binary,
        args: new_args,
        is_snap: args.snap.is_some(),
    };

    terminal
        .open_tab(&spec)
        .map_err(|e| Error::Other(format!("failed to open new tab: {e}")))?;

    eprintln!("Opened in new tab: {title} (terminal: {})", terminal.name());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_copy_pattern_accepts_relative_glob() {
        assert!(validate_copy_pattern(".env").is_ok());
        assert!(validate_copy_pattern(".env.*").is_ok());
        assert!(validate_copy_pattern("config/*.toml").is_ok());
        assert!(validate_copy_pattern("**/.secret").is_ok());
    }

    #[test]
    fn validate_copy_pattern_rejects_absolute_path() {
        let err = validate_copy_pattern("/etc/passwd").unwrap_err();
        assert!(err.to_string().contains("absolute"));
    }

    #[test]
    fn validate_copy_pattern_rejects_parent_traversal() {
        let err = validate_copy_pattern("../secrets").unwrap_err();
        assert!(err.to_string().contains(".."));

        let err = validate_copy_pattern("config/../../etc/passwd").unwrap_err();
        assert!(err.to_string().contains(".."));
    }

    #[test]
    fn validate_copy_pattern_rejects_backslash_traversal() {
        // Windows-style path separator should still be rejected.
        let err = validate_copy_pattern("..\\secrets").unwrap_err();
        assert!(err.to_string().contains(".."));
    }
}
