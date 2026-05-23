// ===========================================================================
// config - Configuration Loading & Merging
// ===========================================================================

use std::path::{Path, PathBuf};

use directories::BaseDirs;
use serde::{Deserialize, Serialize};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to read config: {0}")]
    Read(#[from] std::io::Error),

    #[error("failed to parse config: {0}")]
    Parse(#[from] toml::de::Error),

    #[error("failed to serialize config: {0}")]
    Serialize(#[from] toml::ser::Error),

    #[error("home directory not found")]
    NoHome,
}

// ---------------------------------------------------------------------------
// Global Config (~/.agent-workspace/config.toml)
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct GlobalConfig {
    #[serde(default)]
    pub general: GeneralConfig,

    #[serde(default)]
    pub hooks: HooksConfig,

    #[serde(default)]
    pub ui: UiConfig,

    #[serde(default)]
    pub create: CreateConfig,
}

/// Worktree-creation tunables. Today only the CoW toggle lives here;
/// future creation knobs (parallelism, ignore-pattern overrides) can join
/// without breaking config compatibility.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateConfig {
    /// Use Copy-on-Write (reflink) when the filesystem supports it. On
    /// ReFS/DevDrive (Windows), Btrfs/XFS (Linux), or APFS (macOS) and
    /// same-volume worktrees, this dramatically speeds up `wt new` for
    /// large monorepos. Falls back to plain `git worktree add` silently
    /// when not possible. Default: `true`.
    #[serde(default = "default_use_cow")]
    pub use_cow: bool,
}

impl Default for CreateConfig {
    fn default() -> Self {
        Self {
            use_cow: default_use_cow(),
        }
    }
}

fn default_use_cow() -> bool {
    true
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ProjectCreateConfig {
    /// Override the global `use_cow` for this project. `None` =
    /// inherit global.
    pub use_cow: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiConfig {
    /// Open `wt new` in a new terminal tab when running inside a
    /// supported terminal (Windows Terminal, iTerm2, GNOME Terminal).
    /// Default: `true`. Disable via `[ui] open_in_new_tab = false` or
    /// per-call via `wt new --no-tab`.
    #[serde(default = "default_open_in_new_tab")]
    pub open_in_new_tab: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            open_in_new_tab: default_open_in_new_tab(),
        }
    }
}

fn default_open_in_new_tab() -> bool {
    true
}

// ---------------------------------------------------------------------------
// Project Config (.agent-workspace.toml)
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ProjectConfig {
    #[serde(default)]
    pub general: ProjectGeneralConfig,

    #[serde(default)]
    pub hooks: HooksConfig,

    /// Project-level UI overrides. Project values take precedence over the
    /// global `[ui]` table.
    #[serde(default)]
    pub ui: ProjectUiConfig,

    /// Project-level worktree-creation overrides. Project values take
    /// precedence over the global `[create]` table.
    #[serde(default)]
    pub create: ProjectCreateConfig,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ProjectUiConfig {
    /// Override the global `open_in_new_tab` for this project. `None` =
    /// inherit global.
    pub open_in_new_tab: Option<bool>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct GeneralConfig {
    #[serde(default)]
    pub merge_strategy: MergeStrategy,

    #[serde(default)]
    pub sync_strategy: SyncStrategy,

    #[serde(default)]
    pub copy_files: Vec<String>,

    /// Optional VCS-backend override (`"auto"`, `"git"`, or `"jj"`). Absent
    /// or `"auto"` falls through to the detection step. See
    /// [`crate::vcs::resolve_backend`].
    pub vcs: Option<crate::vcs::VcsChoice>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ProjectGeneralConfig {
    pub trunk: Option<String>,

    pub merge_strategy: Option<MergeStrategy>,

    pub sync_strategy: Option<SyncStrategy>,

    #[serde(default)]
    pub copy_files: Vec<String>,

    /// Optional VCS-backend override scoped to this repo. Overrides the
    /// global `[general] vcs`. See [`crate::vcs::resolve_backend`].
    pub vcs: Option<crate::vcs::VcsChoice>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct HooksConfig {
    #[serde(default)]
    pub post_create: Vec<String>,

    #[serde(default)]
    pub pre_merge: Vec<String>,

    #[serde(default)]
    pub post_merge: Vec<String>,
}

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum MergeStrategy {
    #[default]
    Squash,
    Merge,
}

impl MergeStrategy {
    pub fn is_squash(&self) -> bool {
        matches!(self, Self::Squash)
    }
}

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum SyncStrategy {
    #[default]
    Rebase,
    Merge,
}

// ---------------------------------------------------------------------------
// Merged Config (runtime)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Config {
    pub base_dir: PathBuf,
    pub workspaces_dir: PathBuf,
    pub merge_strategy: MergeStrategy,
    pub sync_strategy: SyncStrategy,
    pub copy_files: Vec<String>,
    pub hooks: HooksConfig,
    pub trunk: Option<String>,
    /// VCS backend choice from project config (preferred) or global config.
    /// `None` means "no explicit choice" — auto-detect at the
    /// [`crate::vcs::resolve_backend`] level.
    pub vcs: Option<crate::vcs::VcsChoice>,
    /// VCS choice resolved separately from project vs global config — kept
    /// split because `resolve_backend` honours project-over-global
    /// precedence and needs to see both layers.
    pub vcs_global: Option<crate::vcs::VcsChoice>,
    /// Resolved `[ui] open_in_new_tab` (project over global). See
    /// [`UiConfig::open_in_new_tab`].
    pub open_in_new_tab: bool,
    /// Resolved `[create] use_cow` (project over global). See
    /// [`CreateConfig::use_cow`].
    pub use_cow: bool,
}

impl Config {
    /// Load and merge global + project config
    pub fn load() -> Result<Self> {
        let base_dir = Self::base_dir()?;
        // Canonicalize base_dir 解决 macOS /var -> /private/var symlink，
        // 确保与 git worktree list 返回的 canonicalized 路径一致。
        // Windows-specific: std::fs::canonicalize returns a verbatim path
        // (`\\?\C:\...`) which git refuses to accept. Strip the prefix —
        // canonicalize is mostly noise on Windows since there are no
        // /var-style symlinks in user-data paths.
        let base_dir = base_dir.canonicalize().unwrap_or(base_dir);
        let base_dir = strip_verbatim_prefix(base_dir);
        let workspaces_dir = base_dir.join("workspaces");

        let global = Self::load_global(&base_dir)?;
        let project = Self::load_project()?;

        // Merge: project overrides global
        let merge_strategy = project
            .general
            .merge_strategy
            .unwrap_or(global.general.merge_strategy);
        let sync_strategy = project
            .general
            .sync_strategy
            .unwrap_or(global.general.sync_strategy);
        let mut copy_files = global.general.copy_files;
        copy_files.extend(project.general.copy_files);

        let hooks = HooksConfig {
            post_create: merge_hooks(&global.hooks.post_create, &project.hooks.post_create),
            pre_merge: merge_hooks(&global.hooks.pre_merge, &project.hooks.pre_merge),
            post_merge: merge_hooks(&global.hooks.post_merge, &project.hooks.post_merge),
        };

        let open_in_new_tab = project.ui.open_in_new_tab.unwrap_or(global.ui.open_in_new_tab);
        let use_cow = project.create.use_cow.unwrap_or(global.create.use_cow);

        Ok(Self {
            base_dir,
            workspaces_dir,
            merge_strategy,
            sync_strategy,
            copy_files,
            hooks,
            trunk: project.general.trunk,
            vcs: project.general.vcs,
            vcs_global: global.general.vcs,
            open_in_new_tab,
            use_cow,
        })
    }

    /// 解析 trunk 分支：配置 > 自动检测 > 默认 "main"
    pub fn resolve_trunk(&self) -> String {
        self.trunk
            .clone()
            .unwrap_or_else(|| crate::vcs::detect_trunk().unwrap_or_else(|_| "main".into()))
    }

    pub fn base_dir() -> Result<PathBuf> {
        Self::resolve_base_dir(std::env::var("AGENT_WORKSPACE_DIR").ok().as_deref())
    }

    // Split out so tests can exercise both env and fallback branches
    // without mutating process-global env state (unsafe + racy under parallel tests).
    fn resolve_base_dir(env_override: Option<&str>) -> Result<PathBuf> {
        if let Some(dir) = env_override.filter(|s| !s.is_empty()) {
            return Ok(PathBuf::from(dir));
        }
        let base = BaseDirs::new().ok_or(Error::NoHome)?;
        Ok(base.home_dir().join(".agent-workspace"))
    }

    fn load_global(base_dir: &Path) -> Result<GlobalConfig> {
        let path = base_dir.join("config.toml");
        if !path.exists() {
            return Ok(GlobalConfig::default());
        }
        let content = std::fs::read_to_string(&path)?;
        Ok(toml::from_str(&content)?)
    }

    fn load_project() -> Result<ProjectConfig> {
        // Resolve the **main repo** root so the same `.agent-workspace.toml`
        // applies whether the user is in the main repo, a worktree, or a
        // subdirectory of either.
        //
        // **Why not just `crate::vcs::repo_root()`?** This runs BEFORE
        // `Cli::run` calls `set_backend(...)`, so the lazy-init GitBackend
        // is active. In a pure-jj repo (no `.git`), GitBackend's
        // `git rev-parse --git-common-dir` fails → `Error::NotInRepo` →
        // project config silently lost (R2-Fix #1 motivation).
        //
        // **Why not just `vcs_runner::detect_vcs`?** It checks
        // `dir.join(".git").exists()` which is true for both gitdirs AND
        // gitlink files. In a git worktree, `.git` is a FILE pointing at
        // the main repo's `.git/worktrees/<name>`. detect_vcs stops at
        // the worktree and returns its path instead of the main repo —
        // R3 regression review caught this. The pre-Phase-F path used
        // git's own `--git-common-dir` which resolves the gitlink.
        //
        // The hybrid: ask git first (handles worktrees correctly via
        // `--git-common-dir`); fall back to detect_vcs for pure-jj repos
        // or filesystems where git isn't available.
        let cwd = match std::env::current_dir() {
            Ok(c) => c,
            Err(_) => return Ok(ProjectConfig::default()),
        };
        let root = match resolve_main_repo_root(&cwd) {
            Some(r) => r,
            None => return Ok(ProjectConfig::default()),
        };
        let path = root.join(".agent-workspace.toml");
        if !path.exists() {
            return Ok(ProjectConfig::default());
        }
        let content = std::fs::read_to_string(&path)?;
        Ok(toml::from_str(&content)?)
    }
}

/// Resolve the **main repo** root for project-config discovery, handling
/// git worktrees correctly.
///
/// This is the backend-independent counterpart to `GitBackend::repo_root`,
/// used during `Config::load` (which runs before any backend is installed).
/// See [`Config::load_project`] for the rationale.
fn resolve_main_repo_root(cwd: &Path) -> Option<PathBuf> {
    use std::path::Component;

    let (backend, detect_root) = vcs_runner::detect_vcs(cwd).ok()?;

    // For git or colocated repos, `--git-common-dir` correctly resolves
    // gitlink files (used by git worktrees) to the main repo's `.git`.
    if backend.has_git()
        && let Ok(common_dir) = vcs_runner::run_git_utf8(cwd, &["rev-parse", "--git-common-dir"])
    {
        let common_dir = common_dir.trim();
        if !common_dir.is_empty() {
            let p = PathBuf::from(common_dir);
            let p = if p.is_absolute() { p } else { cwd.join(&p) };
            if let Ok(canonical) = p.canonicalize() {
                let canonical = strip_verbatim_prefix(canonical);
                // canonical typically ends in `.git`; the main repo root
                // is its parent. For worktree-internal queries it can be
                // `.git/worktrees/<name>` — walk back to `.git`.
                let mut current = canonical.as_path();
                while !current
                    .components()
                    .next_back()
                    .is_some_and(|c| matches!(c, Component::Normal(s) if s == ".git"))
                {
                    current = current.parent()?;
                }
                if let Some(parent) = current.parent() {
                    return Some(parent.to_path_buf());
                }
            }
        }
    }

    // Pure-jj or git-unavailable fallback: use detect_vcs's answer
    // directly. For pure-jj there are no worktree gitlinks to resolve;
    // detect_root IS the repo root.
    Some(detect_root)
}

fn merge_hooks(global: &[String], project: &[String]) -> Vec<String> {
    if project.is_empty() {
        global.to_vec()
    } else {
        project.to_vec()
    }
}

/// Strip Windows verbatim path prefix (`\\?\`) from a canonicalized path.
///
/// `std::fs::canonicalize` on Windows returns paths prefixed with `\\?\`,
/// which git (and many other Win32 tools) refuse to accept. Verbatim paths
/// also break naive `Path::join` comparisons. We strip the prefix unless the
/// path is a UNC verbatim (`\\?\UNC\...`), where stripping would lose
/// information.
///
/// On non-Windows targets this is a no-op identity function.
pub(crate) fn strip_verbatim_prefix(p: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        let s = p.to_string_lossy();
        if let Some(rest) = s.strip_prefix(r"\\?\")
            && !rest.starts_with("UNC\\") {
                return PathBuf::from(rest.to_string());
            }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_global_config_defaults() {
        let config = GlobalConfig::default();
        assert_eq!(config.general.merge_strategy, MergeStrategy::Squash);
        assert_eq!(config.general.sync_strategy, SyncStrategy::Rebase);
        assert!(config.general.copy_files.is_empty());
        assert!(config.hooks.post_create.is_empty());
    }

    #[test]
    fn test_project_config_defaults() {
        let config = ProjectConfig::default();
        assert!(config.general.trunk.is_none());
        assert!(config.general.merge_strategy.is_none());
        assert!(config.general.sync_strategy.is_none());
        assert!(config.general.copy_files.is_empty());
    }

    #[test]
    fn test_global_config_parse() {
        let toml = r#"
[general]
merge_strategy = "merge"
copy_files = ["*.secret"]

[hooks]
post_create = ["npm install"]
pre_merge = ["npm test"]
"#;
        let config: GlobalConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.general.merge_strategy, MergeStrategy::Merge);
        assert_eq!(config.general.copy_files, vec!["*.secret"]);
        assert_eq!(config.hooks.post_create, vec!["npm install"]);
        assert_eq!(config.hooks.pre_merge, vec!["npm test"]);
    }

    #[test]
    fn test_project_config_parse() {
        let toml = r#"
[general]
trunk = "develop"
copy_files = [".env", ".env.local"]

[hooks]
post_create = ["pnpm install"]
"#;
        let config: ProjectConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.general.trunk, Some("develop".to_string()));
        assert_eq!(config.general.copy_files, vec![".env", ".env.local"]);
        assert_eq!(config.hooks.post_create, vec!["pnpm install"]);
    }

    #[test]
    fn test_merge_hooks_empty_project() {
        let global = vec!["global-hook".to_string()];
        let project: Vec<String> = vec![];
        let merged = merge_hooks(&global, &project);
        assert_eq!(merged, vec!["global-hook"]);
    }

    #[test]
    fn test_merge_hooks_project_overrides() {
        let global = vec!["global-hook".to_string()];
        let project = vec!["project-hook".to_string()];
        let merged = merge_hooks(&global, &project);
        assert_eq!(merged, vec!["project-hook"]);
    }

    #[test]
    fn test_merge_strategy_roundtrip() {
        // Test via GlobalConfig since toml can't serialize bare enums
        let toml_squash = r#"[general]
merge_strategy = "squash"
"#;
        let config: GlobalConfig = toml::from_str(toml_squash).unwrap();
        assert_eq!(config.general.merge_strategy, MergeStrategy::Squash);
    }

    // =========================================================================
    // Additional tests for better coverage
    // =========================================================================

    #[test]
    fn test_merge_strategy_merge() {
        let toml = r#"[general]
merge_strategy = "merge"
"#;
        let config: GlobalConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.general.merge_strategy, MergeStrategy::Merge);
    }

    #[test]
    fn test_sync_strategy_defaults() {
        assert_eq!(SyncStrategy::default(), SyncStrategy::Rebase);
    }

    #[test]
    fn test_sync_strategy_parse() {
        #[derive(Deserialize)]
        struct TestConfig {
            sync: SyncStrategy,
        }
        let toml = r#"sync = "merge""#;
        let config: TestConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.sync, SyncStrategy::Merge);

        let toml = r#"sync = "rebase""#;
        let config: TestConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.sync, SyncStrategy::Rebase);
    }

    #[test]
    fn test_hooks_config_defaults() {
        let hooks = HooksConfig::default();
        assert!(hooks.post_create.is_empty());
        assert!(hooks.pre_merge.is_empty());
        assert!(hooks.post_merge.is_empty());
    }

    #[test]
    fn test_hooks_config_with_post_merge() {
        let toml = r#"
[hooks]
post_merge = ["git push", "notify-team"]
"#;
        let config: GlobalConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.hooks.post_merge, vec!["git push", "notify-team"]);
    }

    #[test]
    fn test_project_general_config_defaults() {
        let general = ProjectGeneralConfig::default();
        assert!(general.trunk.is_none());
        assert!(general.merge_strategy.is_none());
        assert!(general.sync_strategy.is_none());
        assert!(general.copy_files.is_empty());
    }

    #[test]
    fn test_config_base_dir() {
        let result = Config::base_dir();
        assert!(result.is_ok());
        let path = result.unwrap();
        assert!(path.to_string_lossy().contains(".agent-workspace"));
    }

    #[test]
    fn test_resolve_base_dir_with_env() {
        let path = Config::resolve_base_dir(Some("/tmp/custom-wt")).unwrap();
        assert_eq!(path, PathBuf::from("/tmp/custom-wt"));
    }

    #[test]
    fn test_resolve_base_dir_empty_env_falls_back() {
        let path = Config::resolve_base_dir(Some("")).unwrap();
        assert!(path.to_string_lossy().contains(".agent-workspace"));
    }

    #[test]
    fn test_resolve_base_dir_none_falls_back() {
        let path = Config::resolve_base_dir(None).unwrap();
        assert!(path.to_string_lossy().contains(".agent-workspace"));
    }

    #[test]
    fn test_error_display() {
        let err = Error::NoHome;
        assert_eq!(err.to_string(), "home directory not found");
    }

    #[test]
    fn test_global_config_serialize() {
        let config = GlobalConfig {
            general: GeneralConfig {
                merge_strategy: MergeStrategy::Merge,
                sync_strategy: SyncStrategy::default(),
                copy_files: vec![".env".to_string()],
                vcs: None,
            },
            hooks: HooksConfig {
                post_create: vec!["npm install".to_string()],
                pre_merge: vec![],
                post_merge: vec![],
            },
            ui: UiConfig::default(),
            create: CreateConfig::default(),
        };
        let serialized = toml::to_string(&config).unwrap();
        assert!(serialized.contains("merge"));
        assert!(serialized.contains(".env"));
        assert!(serialized.contains("npm install"));
    }

    #[test]
    fn test_project_merge_strategy_override() {
        let toml = r#"
[general]
merge_strategy = "merge"
"#;
        let config: ProjectConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.general.merge_strategy, Some(MergeStrategy::Merge));
    }

    #[test]
    fn test_project_merge_strategy_absent() {
        let toml = r#"
[general]
trunk = "develop"
"#;
        let config: ProjectConfig = toml::from_str(toml).unwrap();
        assert!(config.general.merge_strategy.is_none());
    }

    #[test]
    fn test_global_config_parse_sync_strategy() {
        let toml = r#"
[general]
sync_strategy = "merge"
"#;
        let config: GlobalConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.general.sync_strategy, SyncStrategy::Merge);
    }

    #[test]
    fn test_project_sync_strategy_override() {
        let toml = r#"
[general]
sync_strategy = "merge"
"#;
        let config: ProjectConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.general.sync_strategy, Some(SyncStrategy::Merge));
    }

    #[test]
    fn test_project_sync_strategy_absent() {
        let toml = r#"
[general]
trunk = "develop"
"#;
        let config: ProjectConfig = toml::from_str(toml).unwrap();
        assert!(config.general.sync_strategy.is_none());
    }

    #[test]
    fn test_project_config_serialize() {
        let config = ProjectConfig {
            general: ProjectGeneralConfig {
                trunk: Some("develop".to_string()),
                merge_strategy: None,
                sync_strategy: None,
                copy_files: vec![".env.local".to_string()],
                vcs: None,
            },
            hooks: HooksConfig::default(),
            ui: ProjectUiConfig::default(),
            create: ProjectCreateConfig::default(),
        };
        let serialized = toml::to_string(&config).unwrap();
        assert!(serialized.contains("develop"));
        assert!(serialized.contains(".env.local"));
    }

    #[test]
    fn test_merge_hooks_both_empty() {
        let global: Vec<String> = vec![];
        let project: Vec<String> = vec![];
        let merged = merge_hooks(&global, &project);
        assert!(merged.is_empty());
    }

    #[test]
    fn test_global_config_clone() {
        let config = GlobalConfig::default();
        let cloned = config.clone();
        assert_eq!(cloned.general.merge_strategy, config.general.merge_strategy);
    }

    #[test]
    fn test_merge_strategy_equality() {
        assert_eq!(MergeStrategy::Squash, MergeStrategy::Squash);
        assert_ne!(MergeStrategy::Squash, MergeStrategy::Merge);
    }

    #[test]
    fn test_sync_strategy_equality() {
        assert_eq!(SyncStrategy::Rebase, SyncStrategy::Rebase);
        assert_ne!(SyncStrategy::Rebase, SyncStrategy::Merge);
    }

    #[test]
    fn test_merge_strategy_copy() {
        let strategy = MergeStrategy::Squash;
        let copied = strategy;
        assert_eq!(strategy, copied);
    }

    #[test]
    fn test_config_debug() {
        let config = GlobalConfig::default();
        let debug = format!("{:?}", config);
        assert!(debug.contains("GlobalConfig"));
    }
}
