// ===========================================================================
// repo_meta - Cached per-repository metadata
// ===========================================================================
//
// On a 300k-file repo the cow phase-1 scan costs ~15 s of pure metadata
// I/O (one stat per file) just to learn `total_files` and `total_bytes`
// for the progress-bar denominator. Across an active day with several
// `ws new` invocations this adds up to multiple minutes of pure setup
// work — for a quantity that barely changes between calls.
//
// This module persists those slow-to-compute values (plus a few
// cheap-to-compute identifiers we want to surface elsewhere — git
// origin URL, derived GitHub `Owner/Repo` slug, repo basename) to
// `<workspaces_dir>/<project>-<hash>/.repo-meta.toml`, refreshing the
// file only every 30 days. Within the freshness window `ws new` skips
// the scan entirely and uses the cached totals straight from the TOML.
//
// **Trade-off**: between cache refreshes the cached numbers can drift
// from reality if files are added/removed/resized in the source repo.
// The numbers feed the progress bar only — actual robocopy / reflink
// operations always walk the live filesystem — so the worst case is a
// progress bar that slightly under- or over-shoots by the time the
// copy completes. Acceptable given the time savings on every `ws new`
// against a large repo.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Filename of the cache file, kept inside `<workspaces_dir>/<id>/`.
/// Leading dot to keep it out of normal listings; the `.toml` extension
/// signals plain-text inspect-with-a-text-editor data.
pub const CACHE_FILENAME: &str = ".repo-meta.toml";

/// Refresh interval. 30 days × 24 h × 60 min × 60 s.
///
/// Picked deliberately wider than a typical sprint cycle: the cache is
/// here to avoid recomputing values that change slowly. A monthly
/// refresh catches genuinely added/removed packages, new monorepo
/// modules, etc., without thrashing on every checkout.
pub const REFRESH_INTERVAL_SECS: u64 = 30 * 24 * 60 * 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoMeta {
    /// Unix timestamp (seconds since epoch) of the last refresh. Used
    /// to decide whether the cache is still within
    /// [`REFRESH_INTERVAL_SECS`].
    pub last_refresh: u64,

    /// `git remote get-url origin` output captured verbatim. `None` if
    /// the repo has no `origin` remote (or the git call failed). We
    /// keep this for downstream UI surfaces (`ws status` linking to
    /// the GitHub PR page, etc.) — not used by the CoW path itself.
    pub origin: Option<String>,

    /// `Owner/Repo` slug parsed out of `origin` when it looks like a
    /// GitHub URL. `None` for non-GitHub remotes (GitLab, custom Git
    /// hosts, etc.). Supports the three URL flavours we see in
    /// practice:
    ///   - `https://github.com/Owner/Repo[.git]`
    ///   - `git@github.com:Owner/Repo[.git]`
    ///   - `ssh://git@github.com/Owner/Repo[.git]`
    pub github_repo: Option<String>,

    /// Last path component of the repo root directory — the same name
    /// the user typically refers to the repo by ("CargoWise",
    /// "agent-workspace", etc.). Stored so future UI bits don't need
    /// to re-derive it from a possibly-canonicalised path.
    pub repo_name: String,

    /// Total tracked-and-untracked file count, excluding `.git/` and
    /// (for colocated repos) `.jj/`. Drives the progress-bar
    /// denominator in the CoW path.
    pub total_files: u64,

    /// Total bytes of those same files. Drives the human-readable
    /// "Copying repository: N files, X GiB" heading.
    pub total_bytes: u64,
}

impl RepoMeta {
    /// True if the cache was refreshed within the last
    /// [`REFRESH_INTERVAL_SECS`] seconds.
    ///
    /// Clock-rollback safety: `saturating_sub` means a system whose
    /// clock has moved backwards across the cache write treats the
    /// cache as fresh (rather than infinitely stale). Negligible-
    /// chance edge case, but the saturating arithmetic is free.
    pub fn is_fresh(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        now.saturating_sub(self.last_refresh) < REFRESH_INTERVAL_SECS
    }
}

/// Errors that don't propagate out of [`load_or_refresh`] — we'd
/// rather fall back to a fresh scan than abort `ws new` on a
/// metadata-cache read/write hiccup. Kept around in case the caller
/// wants to log diagnostics.
#[derive(Debug)]
pub enum MetaError {
    Io(std::io::Error),
    Parse(toml::de::Error),
    Scan(String),
}

impl std::fmt::Display for MetaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MetaError::Io(e) => write!(f, "io: {e}"),
            MetaError::Parse(e) => write!(f, "parse: {e}"),
            MetaError::Scan(s) => write!(f, "scan: {s}"),
        }
    }
}

/// Return the cached metadata for the repo at `repo_root`, refreshing
/// from a fresh filesystem walk if no cache exists or the existing one
/// is older than [`REFRESH_INTERVAL_SECS`].
///
/// Cache file lives at `project_dir.join(CACHE_FILENAME)`, where
/// `project_dir` is the per-repo directory under `workspaces_dir` —
/// for the CoW path this is `dst.parent()` (i.e. the worktree's
/// containing `<project>-<hash>/`). Caller is responsible for
/// `create_dir_all`ing `project_dir` before calling us; the standard
/// `ws new` flow does this earlier when materialising the worktree.
///
/// A failed read/parse silently triggers a refresh; a failed write
/// silently returns the just-computed values without persisting (next
/// call will re-scan). Best-effort by design — the cache is purely an
/// optimisation, never a correctness load-bearing thing.
pub fn load_or_refresh(
    project_dir: &Path,
    repo_root: &Path,
    excludes: &[&str],
    user_patterns: &[String],
) -> std::result::Result<LoadResult, MetaError> {
    let cache_path = project_dir.join(CACHE_FILENAME);

    // Attempt to read + parse + freshness-check. Any failure here
    // falls through to a fresh refresh — we don't want a corrupted
    // cache to make `ws new` fail.
    if let Ok(content) = std::fs::read_to_string(&cache_path)
        && let Ok(meta) = toml::from_str::<RepoMeta>(&content)
        && meta.is_fresh()
    {
        return Ok(LoadResult { meta, from_cache: true });
    }

    let meta = compute(repo_root, excludes, user_patterns)?;

    // Best-effort write. If we can't persist (read-only volume,
    // permission denied, etc.) we still return the in-memory value;
    // the user-visible outcome is just "scan runs again next time".
    if let Ok(toml_str) = toml::to_string_pretty(&meta) {
        let _ = std::fs::create_dir_all(project_dir);
        let _ = std::fs::write(&cache_path, toml_str);
    }

    Ok(LoadResult { meta, from_cache: false })
}

/// Bundles the [`RepoMeta`] with whether it came from disk (`true`)
/// or was recomputed (`false`). UIs use the flag to surface a small
/// "(using cached metadata)" hint when applicable.
#[derive(Debug, Clone)]
pub struct LoadResult {
    pub meta: RepoMeta,
    pub from_cache: bool,
}

/// Build a fresh [`RepoMeta`] by walking `repo_root` (cheap-to-compute
/// fields like origin URL come from `git remote get-url`, expensive
/// ones from a metadata-only walk).
///
/// Filter semantics mirror cow-side `build_clone_walker` so the cached
/// totals reflect EXACTLY what `ws new` will copy:
///   - `excludes`: hardcoded top-level names (`.git`, `.jj`), anchored
///     to root via `/X` patterns.
///   - `user_patterns`: gitignore-style patterns from `[copy] exclude`
///     in the project config.
///
/// Both feed a single `ignore::gitignore::Gitignore` matcher.
fn compute(
    repo_root: &Path,
    excludes: &[&str],
    user_patterns: &[String],
) -> std::result::Result<RepoMeta, MetaError> {
    use ignore::gitignore::GitignoreBuilder;

    let mut total_files: u64 = 0;
    let mut total_bytes: u64 = 0;

    let mut gi = GitignoreBuilder::new(repo_root);
    for &name in excludes {
        let _ = gi.add_line(None, &format!("/{name}"));
    }
    for pat in user_patterns {
        let _ = gi.add_line(None, pat);
    }
    let matcher = gi
        .build()
        .unwrap_or_else(|_| GitignoreBuilder::new(repo_root).build().expect("empty gitignore builds"));

    let walker = ignore::WalkBuilder::new(repo_root)
        .standard_filters(false)
        .hidden(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .follow_links(false)
        .filter_entry(move |entry| {
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            !matcher.matched(entry.path(), is_dir).is_ignore()
        })
        .build();

    for entry in walker.flatten() {
        if let Some(ft) = entry.file_type()
            && ft.is_file()
        {
            total_files += 1;
            if let Ok(meta) = entry.metadata() {
                total_bytes += meta.len();
            }
        }
    }

    let origin = read_origin(repo_root);
    let github_repo = origin.as_deref().and_then(parse_github_repo);
    let repo_name = repo_root
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".to_string());

    let last_refresh = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    Ok(RepoMeta {
        last_refresh,
        origin,
        github_repo,
        repo_name,
        total_files,
        total_bytes,
    })
}

/// Shell out to `git remote get-url origin` and trim the result.
/// Returns `None` if the command fails, the repo has no `origin` (jj
/// repos, fresh local repos, etc.), or the output is empty.
fn read_origin(repo_root: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .current_dir(repo_root)
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

/// Pull `Owner/Repo` out of a `github.com` URL in any of the common
/// shapes. Returns `None` for non-github hosts or unparseable inputs.
///
/// Strips a trailing `.git` and any trailing `/`. Tolerates user-info
/// in the URL (`https://<user>@github.com/...`,
/// `https://<user>:<token>@github.com/...`), which is what `git remote
/// get-url` reports after `gh auth login` or any other helper that
/// embeds the username in the stored remote.
///
/// Steps:
///   1. Strip the URL scheme (`https://`, `http://`, `ssh://`, `git://`).
///   2. Drop everything up to the LAST `@` in the remaining authority
///      part — that's the user-info section (e.g. `oauth2:token@`).
///      Using the last `@` is correct because tokens may legitimately
///      contain `@` and the host part by spec doesn't.
///   3. Match `github.com/` or `github.com:` (the SCP-like form used
///      by `git@github.com:Owner/Repo`).
///   4. Split the remainder on `/` and accept only Owner/Repo with
///      both parts non-empty — anything with extra path segments
///      (`tree/main`, blob URLs, etc.) isn't a plain repo remote.
pub fn parse_github_repo(origin: &str) -> Option<String> {
    let candidate = origin.trim().trim_end_matches('/');
    let candidate = candidate.strip_suffix(".git").unwrap_or(candidate);

    // 1. Strip the scheme.
    let no_scheme = candidate
        .strip_prefix("https://")
        .or_else(|| candidate.strip_prefix("http://"))
        .or_else(|| candidate.strip_prefix("ssh://"))
        .or_else(|| candidate.strip_prefix("git://"))
        .unwrap_or(candidate);

    // 2. Drop user-info if present. Cut at the LAST `@` in the
    //    portion before the first `/` or `:`, so a URL like
    //    `oauth2:abc@def@github.com/...` (yes, this happens with
    //    some token providers) still resolves to the right host.
    let host_end = no_scheme.find('/').unwrap_or(no_scheme.len());
    let host_end = no_scheme[..host_end]
        .rfind(':')
        .map(|colon| colon.max(0))
        .unwrap_or(host_end)
        .min(host_end);
    let authority = &no_scheme[..host_end.max(
        no_scheme.find('/').unwrap_or(no_scheme.len()),
    )];
    let no_userinfo = match authority.rfind('@') {
        Some(at_pos) => &no_scheme[at_pos + 1..],
        None => no_scheme,
    };

    // 3. Match a github.com host.
    let after_host = if let Some(rest) = no_userinfo.strip_prefix("github.com/") {
        rest
    } else if let Some(rest) = no_userinfo.strip_prefix("github.com:") {
        rest
    } else {
        return None;
    };

    // 4. Split into Owner / Repo. Reject anything with more path
    //    segments — those aren't a plain repo URL.
    let parts: Vec<&str> = after_host.split('/').collect();
    if parts.len() == 2 && !parts[0].is_empty() && !parts[1].is_empty() {
        Some(format!("{}/{}", parts[0], parts[1]))
    } else {
        None
    }
}

/// Convenience accessor for `<workspaces_dir>/<workspace_id>/`. The CoW
/// path derives this from the worktree path's parent; this helper is
/// here for symmetry with the cache filename constant.
pub fn project_dir_for(workspaces_dir: &Path, workspace_id: &str) -> PathBuf {
    workspaces_dir.join(workspace_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_github_repo_https_with_dot_git() {
        assert_eq!(
            parse_github_repo("https://github.com/ZelAnton/agent-workspace.git"),
            Some("ZelAnton/agent-workspace".to_string())
        );
    }

    #[test]
    fn parse_github_repo_https_without_dot_git() {
        assert_eq!(
            parse_github_repo("https://github.com/ZelAnton/agent-workspace"),
            Some("ZelAnton/agent-workspace".to_string())
        );
    }

    #[test]
    fn parse_github_repo_https_with_trailing_slash() {
        assert_eq!(
            parse_github_repo("https://github.com/ZelAnton/agent-workspace/"),
            Some("ZelAnton/agent-workspace".to_string())
        );
    }

    #[test]
    fn parse_github_repo_ssh_form() {
        assert_eq!(
            parse_github_repo("git@github.com:ZelAnton/agent-workspace.git"),
            Some("ZelAnton/agent-workspace".to_string())
        );
    }

    #[test]
    fn parse_github_repo_ssh_protocol_form() {
        assert_eq!(
            parse_github_repo("ssh://git@github.com/ZelAnton/agent-workspace.git"),
            Some("ZelAnton/agent-workspace".to_string())
        );
    }

    #[test]
    fn parse_github_repo_https_with_username() {
        // `gh auth login` and similar helpers persist remotes with
        // the username embedded in the URL. Must still resolve.
        assert_eq!(
            parse_github_repo("https://ZelAnton@github.com/ZelAnton/agent-workspace"),
            Some("ZelAnton/agent-workspace".to_string())
        );
    }

    #[test]
    fn parse_github_repo_https_with_user_and_token() {
        // PATs and OAuth tokens embed as `user:token@host`.
        assert_eq!(
            parse_github_repo("https://oauth2:ghp_abc123@github.com/Org/Repo.git"),
            Some("Org/Repo".to_string())
        );
    }

    #[test]
    fn parse_github_repo_non_github_returns_none() {
        assert_eq!(
            parse_github_repo("https://gitlab.com/owner/repo.git"),
            None
        );
        assert_eq!(
            parse_github_repo("https://bitbucket.org/owner/repo.git"),
            None
        );
    }

    #[test]
    fn parse_github_repo_extra_path_segments_rejected() {
        // Not a top-level repo URL — extra path beyond Owner/Repo.
        assert_eq!(
            parse_github_repo("https://github.com/Org/Repo/tree/main"),
            None
        );
    }

    #[test]
    fn parse_github_repo_empty_owner_or_repo_rejected() {
        assert_eq!(parse_github_repo("https://github.com//Repo"), None);
        assert_eq!(parse_github_repo("https://github.com/Owner/"), None);
    }

    #[test]
    fn is_fresh_returns_true_for_now() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let meta = RepoMeta {
            last_refresh: now,
            origin: None,
            github_repo: None,
            repo_name: "x".into(),
            total_files: 0,
            total_bytes: 0,
        };
        assert!(meta.is_fresh());
    }

    #[test]
    fn is_fresh_returns_false_for_old_timestamp() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // 31 days ago — past the refresh window.
        let meta = RepoMeta {
            last_refresh: now - (31 * 24 * 60 * 60),
            origin: None,
            github_repo: None,
            repo_name: "x".into(),
            total_files: 0,
            total_bytes: 0,
        };
        assert!(!meta.is_fresh());
    }

    #[test]
    fn is_fresh_clock_rollback_treats_cache_as_fresh() {
        // last_refresh is "in the future" — saturating_sub gives 0.
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let meta = RepoMeta {
            last_refresh: now + 100_000,
            origin: None,
            github_repo: None,
            repo_name: "x".into(),
            total_files: 0,
            total_bytes: 0,
        };
        assert!(meta.is_fresh());
    }

    #[test]
    fn load_or_refresh_writes_cache_then_returns_cached() {
        let tmp = tempfile::tempdir().unwrap();
        let project_dir = tmp.path().join("proj");
        let repo_root = tmp.path().join("repo");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::create_dir_all(&repo_root).unwrap();
        std::fs::write(repo_root.join("a.txt"), b"hello").unwrap();
        std::fs::write(repo_root.join("b.txt"), b"world!").unwrap();

        // First call: scans + writes.
        let first = load_or_refresh(&project_dir, &repo_root, &[], &[]).unwrap();
        assert!(!first.from_cache, "first call should refresh");
        assert_eq!(first.meta.total_files, 2);
        assert_eq!(first.meta.total_bytes, 5 + 6);

        // Cache file now exists.
        let cache_path = project_dir.join(CACHE_FILENAME);
        assert!(cache_path.exists());

        // Mutate the repo — second call should still return the
        // cached values (since the cache is fresh).
        std::fs::write(repo_root.join("c.txt"), b"ignored-by-cache").unwrap();
        let second = load_or_refresh(&project_dir, &repo_root, &[], &[]).unwrap();
        assert!(second.from_cache, "second call should hit cache");
        assert_eq!(second.meta.total_files, 2, "cache should be used");
        assert_eq!(second.meta.total_bytes, 5 + 6);
    }

    #[test]
    fn load_or_refresh_excludes_skip_dotgit() {
        let tmp = tempfile::tempdir().unwrap();
        let project_dir = tmp.path().join("proj");
        let repo_root = tmp.path().join("repo");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::create_dir_all(repo_root.join(".git")).unwrap();
        std::fs::write(repo_root.join(".git/HEAD"), b"x").unwrap();
        std::fs::write(repo_root.join("kept.txt"), b"y").unwrap();

        let res = load_or_refresh(&project_dir, &repo_root, &[".git"], &[]).unwrap();
        assert_eq!(res.meta.total_files, 1, ".git contents should be skipped");
    }
}
