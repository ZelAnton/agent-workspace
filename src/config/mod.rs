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
    /// same-volume worktrees, this dramatically speeds up `ws new` for
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
    /// Open `ws new` in a new terminal tab when running inside a
    /// supported terminal (Windows Terminal, iTerm2, GNOME Terminal).
    /// Default: `true`. Disable via `[ui] open_in_new_tab = false` or
    /// per-call via `ws new --no-tab`.
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
// Project Config (.workspace.toml, legacy fallback .agent-workspace.toml)
// ---------------------------------------------------------------------------

/// Primary repo/worktree config filename. Local, per-machine — kept out of
/// git via the local exclude file (see [`ensure_workspace_config_ignored`]).
/// Repo-level lives at the main repo root; worktree-level lives at each git
/// worktree root (see [`Config::load`]).
pub const WORKSPACE_CONFIG_FILENAME: &str = ".workspace.toml";

/// Legacy committed project-config filename, read as a fallback when no
/// [`WORKSPACE_CONFIG_FILENAME`] is present so existing repos keep working.
pub const LEGACY_PROJECT_CONFIG_FILENAME: &str = ".agent-workspace.toml";

/// Resolve which file holds project config at `root`: prefer
/// `.workspace.toml`, fall back to `.agent-workspace.toml` when only the
/// legacy file exists, else return the `.workspace.toml` path (the write
/// target for new files). Mirrors [`crate::meta::meta_path_with_fallback`].
pub fn project_config_path_with_fallback(root: &Path) -> PathBuf {
    let primary = root.join(WORKSPACE_CONFIG_FILENAME);
    if primary.exists() {
        return primary;
    }
    let legacy = root.join(LEGACY_PROJECT_CONFIG_FILENAME);
    if legacy.exists() {
        return legacy;
    }
    primary
}

/// Best-effort: keep a freshly-written `.workspace.toml` out of git WITHOUT
/// requiring a commit, by adding it to the repo's **local exclude file**
/// (`<git-common-dir>/info/exclude`) rather than the committed `.gitignore`.
///
/// No-op unless `config_path` points at a `.workspace.toml` — editing a legacy
/// committed `.agent-workspace.toml` in place introduces no new local file.
/// The exclude file lives in the common git dir, which every worktree shares,
/// so one entry covers the main repo and all worktrees. Errors are swallowed
/// (callers must not fail on this).
pub fn ensure_workspace_config_ignored(config_path: &Path) {
    let is_workspace_file = config_path.file_name().and_then(|n| n.to_str())
        == Some(WORKSPACE_CONFIG_FILENAME);
    if !is_workspace_file {
        return;
    }
    let Some(repo_dir) = config_path.parent() else {
        return;
    };
    if let Some(exclude_file) = local_exclude_file(repo_dir) {
        let _ = crate::git_exclude::ensure_pattern(&exclude_file, WORKSPACE_CONFIG_FILENAME);
    }
}

/// Resolve the repo's local exclude file (`<git-common-dir>/info/exclude`) by
/// asking git from `cwd`. `--git-common-dir` resolves the *common* git dir even
/// from inside a linked worktree, so the path is shared across worktrees.
/// `None` when git isn't available (e.g. a jj repo with no git backing) — the
/// caller then silently skips, leaving the file un-excluded.
fn local_exclude_file(cwd: &Path) -> Option<PathBuf> {
    let common = run_git_utf8(cwd, &["rev-parse", "--git-common-dir"]).ok()?;
    let common = common.trim();
    if common.is_empty() {
        return None;
    }
    let p = PathBuf::from(common);
    let p = if p.is_absolute() { p } else { cwd.join(p) };
    Some(p.join("info").join("exclude"))
}

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

    /// Project-level workspace-directory naming overrides. Controls how
    /// the per-repo project directory under `$AGENT_WORKSPACE_DIR` is
    /// named for THIS repo. See [`ProjectWorkspaceConfig`].
    #[serde(default)]
    pub workspace: ProjectWorkspaceConfig,

    /// Project-level CoW-copy exclusions — gitignore-style patterns the
    /// CoW path skips when copying the source repo into a new worktree.
    /// See [`ProjectCopyConfig`].
    #[serde(default)]
    pub copy: ProjectCopyConfig,
}

/// User-facing copy-exclusion list for the CoW worktree-creation path.
///
/// `ws new` normally copies every file from the source repo into the
/// new worktree (excluding `.git` and, on colocated repos, `.jj` —
/// those are hard-coded and not user-configurable). On a real monorepo
/// users often want to skip generated artefacts they neither need nor
/// want copied into a throwaway worktree: `target/`, `node_modules/`,
/// `Bin/`, big assets like ISOs, etc.
///
/// Patterns are gitignore-style, matched via
/// [`ignore::gitignore::Gitignore`] rooted at the repo. The matcher
/// understands:
///
///   - `target`            — match any directory named `target` at any depth
///   - `/target`           — match only top-level `target` (anchored)
///   - `node_modules/`     — directory only (not files named the same)
///   - `**/*.iso`          — any-depth glob for `.iso` files
///   - `!keep-this-anyway` — negation, re-include something excluded
///     by an earlier broader pattern
///
/// Set via the dedicated `ws exclude` command (positional args, or its
/// TUI tree picker) or by editing `.workspace.toml` directly:
///
/// ```toml
/// [copy]
/// exclude = [
///     "/target",
///     "/node_modules",
///     "/Bin",
///     "**/*.iso",
/// ]
/// ```
///
/// **Note**: this is NOT the same as `[general] copy_files` — that one
/// is the *include* list for files copied INTO each new worktree (like
/// `.env`, `.env.local`) after creation. Different feature, same
/// gitignore-pattern shape. They live in separate sections to keep the
/// semantics straight.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ProjectCopyConfig {
    /// Gitignore-style patterns the CoW path skips when copying the
    /// source repo. Empty = copy everything (except the always-excluded
    /// `.git` / `.jj`).
    #[serde(default)]
    pub exclude: Vec<String>,
}

/// Per-repo settings controlling the workspace-directory NAME chosen
/// for `ws new` / `ws ls` / etc. — i.e. the directory under
/// `$AGENT_WORKSPACE_DIR` that holds all of this repo's worktrees.
///
/// Defaults (no `[workspace]` table in the project config) give:
///   - `alias`         = None → use the repo's basename (e.g.
///     `CargoWise`).
///   - `use_path_hash` = None → resolved to `false`, i.e. NO `-<hash>`
///     suffix. Earlier versions of `ws` always appended a 6-hex
///     disambiguation suffix (`CargoWise-56f172`) computed from the repo
///     root path; that's now opt-in via `use_path_hash = true` for users
///     with multiple same-named repos at different paths.
///
/// Set via the new `ws config` subcommand or by editing
/// `.workspace.toml` directly:
///
/// ```toml
/// [workspace]
/// alias = "my-cargowise"
/// use_path_hash = false
/// ```
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ProjectWorkspaceConfig {
    /// Override the repo basename in the workspace-dir path. Useful
    /// when the on-disk directory name is generic (e.g. `repo`) but
    /// you want the workspace dir to be more descriptive
    /// (`my-cargowise`).
    pub alias: Option<String>,

    /// When `Some(true)`, append `-<6-hex-hash>` to the workspace dir
    /// name (where the hash is derived from the repo root path). The
    /// pre-v0.13.16 behaviour. `None` or `Some(false)` = no suffix.
    pub use_path_hash: Option<bool>,
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

    /// Lowercase wire/display name — used in user-facing messages and JSON
    /// (replaces leaking `{:?}` Debug, which renders `Squash`/`Merge`).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Squash => "squash",
            Self::Merge => "merge",
        }
    }
}

impl std::fmt::Display for MergeStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum SyncStrategy {
    #[default]
    Rebase,
    Merge,
}

impl SyncStrategy {
    /// Lowercase wire/display name — used in user-facing messages and JSON.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Rebase => "rebase",
            Self::Merge => "merge",
        }
    }
}

impl std::fmt::Display for SyncStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
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

    /// Resolved `[workspace] alias` from the project config. `None` =
    /// use the repo's basename in workspace-dir paths. See
    /// [`ProjectWorkspaceConfig::alias`].
    pub workspace_alias: Option<String>,

    /// Resolved `[workspace] use_path_hash` from the project config.
    /// `false` (default) = workspace dir is `<name>/`; `true` =
    /// `<name>-<6-hex-hash>/` (pre-v0.13.16 behaviour). See
    /// [`ProjectWorkspaceConfig::use_path_hash`].
    pub use_path_hash: bool,

    /// Resolved `[copy] exclude` from the project config: gitignore-
    /// style patterns the CoW path skips. Empty = copy everything
    /// (except always-excluded `.git` / `.jj`). Project-only, no
    /// global counterpart — exclusion is fundamentally repo-specific.
    pub copy_excludes: Vec<String>,
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

        // v0.13.6+: project worktree directories live DIRECTLY under
        // `base_dir` (e.g. `~/.agent-workspace/<project>-<hash>/<branch>/`)
        // — the historical intermediate `workspaces/` subdirectory is gone.
        // Rationale: one fewer directory level in every shown path, and
        // it matches the user mental model when `AGENT_WORKSPACE_DIR` is
        // explicitly set to a dedicated path like `D:/ws/` ("worktrees
        // live in this drive root, not in this drive root's `workspaces/`
        // subfolder").
        //
        // Reserved top-level names that are NOT project directories:
        //   - `bin/`               — the binary (installer + ws update)
        //   - `config.toml`        — global config
        //   - `install_channel`    — npm / shell channel marker
        //   - `last_update_check`  — update-check throttle marker
        //   - `workspaces/`        — legacy pre-0.13.6 worktrees (kept
        //                            intact for backwards inspection; see
        //                            the migration warning below)
        // Project IDs are `<repo-name>-<6-hex-hash>` so collisions with
        // the reserved names are effectively impossible.
        let workspaces_dir = base_dir.clone();

        // (Earlier versions printed a legacy-workspaces migration nudge
        // on every `Config::load()`. Removed in v0.13.11 because the
        // banner had outlived its usefulness — most users on this
        // version did the cleanup once and never needed to see it
        // again, and on the rare upgrade case the nudge in the
        // README's storage-layout section is sufficient guidance.)

        let global = Self::load_global(&base_dir)?;
        // Project config is itself a 2-layer fold: repo-level (main repo root)
        // overlaid by worktree-level (current worktree root), the latter only
        // when running inside a linked worktree. The folded result then feeds
        // the global-vs-project merge below unchanged.
        let (repo, worktree) = Self::load_project_layers()?;
        let project = match worktree {
            Some(wt) => merge_project_layers(repo, wt),
            None => repo,
        };

        Ok(merge_global_project(global, project, base_dir, workspaces_dir))
    }

    /// Resolve the per-repo workspace directory under `workspaces_dir`.
    ///
    /// The directory name is one of (in priority order):
    ///   1. `<alias>-<hash>` if `use_path_hash = true` AND alias set
    ///   2. `<alias>`        if alias set, no path hash
    ///   3. `<repo>-<hash>`  if `use_path_hash = true`, no alias
    ///   4. `<repo>`         (default) — just the repo basename
    ///
    /// **Backward compatibility**: pre-v0.13.16 always used form (3).
    /// If the user upgrades and their project config has neither alias
    /// nor `use_path_hash = true`, the new default would be (4) and
    /// any existing worktrees in the (3) directory would be lost from
    /// `ws ls`. To preserve them transparently we check on disk: if
    /// the (4) directory doesn't exist BUT the (3) directory does, we
    /// keep using (3). The user can flip `use_path_hash` explicitly
    /// to lock in either form.
    ///
    /// `workspace_id` is the VCS-provided `<repo>-<hash>` string we
    /// already compute everywhere (see `vcs::workspace_id`). Parsed
    /// via `rsplit_once('-')` to extract the hash; this handles repos
    /// whose names contain dashes (`agent-workspace-abc123` →
    /// name=`agent-workspace`, hash=`abc123`).
    pub fn project_dir_for(&self, workspace_id: &str) -> PathBuf {
        let (repo_name, hash) = workspace_id
            .rsplit_once('-')
            .unwrap_or((workspace_id, ""));

        let display_name = self.workspace_alias.as_deref().unwrap_or(repo_name);

        if self.use_path_hash {
            let name = if hash.is_empty() {
                display_name.to_string()
            } else {
                format!("{display_name}-{hash}")
            };
            return self.workspaces_dir.join(name);
        }

        // No-hash form is the new default. Before returning, check
        // for the legacy hash-suffixed dir on disk and prefer it if
        // it exists — keeps existing worktrees accessible after the
        // user upgrades without forcing a manual rename.
        let no_hash = self.workspaces_dir.join(display_name);
        if no_hash.exists() {
            return no_hash;
        }
        if !hash.is_empty() {
            let with_hash = self
                .workspaces_dir
                .join(format!("{display_name}-{hash}"));
            if with_hash.exists() {
                return with_hash;
            }
        }
        no_hash // fresh repo; will be created at the no-hash path
    }

    /// 解析 trunk 分支：配置 > 自动检测 > 默认 "main"
    pub async fn resolve_trunk(&self, repo: &crate::vcs::Repo) -> String {
        match &self.trunk {
            Some(t) => t.clone(),
            None => repo.detect_trunk().await.unwrap_or_else(|_| "main".into()),
        }
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

    /// Load the two project-config layers: repo-level (main repo root, with
    /// legacy `.agent-workspace.toml` fallback) and the optional worktree-level
    /// overlay (current worktree root, `.workspace.toml` only).
    ///
    /// **Why resolve roots here instead of `Repo::repo_root()`?** This runs
    /// BEFORE `Cli::run` builds the `Repo`, so no backend is available yet.
    /// In a pure-jj repo (no `.git`), the git backend's
    /// `git rev-parse --git-common-dir` would fail → `Error::NotInRepo` →
    /// project config silently lost. We resolve roots with a local filesystem
    /// probe + plain `git` subprocess instead — see [`resolve_main_repo_root`] /
    /// [`resolve_current_worktree_root`].
    ///
    /// The worktree layer is `Some` only when the current worktree root
    /// differs from the main repo root — running in the main checkout would
    /// otherwise apply the same file twice.
    fn load_project_layers() -> Result<(ProjectConfig, Option<ProjectConfig>)> {
        let cwd = match std::env::current_dir() {
            Ok(c) => c,
            Err(_) => return Ok((ProjectConfig::default(), None)),
        };
        let main_root = match resolve_main_repo_root(&cwd) {
            Some(r) => r,
            None => return Ok((ProjectConfig::default(), None)),
        };

        // Repo-level: prefer `.workspace.toml`, fall back to the legacy file.
        let repo = load_project_file(&project_config_path_with_fallback(&main_root))?;

        // Worktree-level: only when we're in a linked worktree whose root
        // differs from the main repo root. Reads `.workspace.toml` only — the
        // legacy name is a repo-level concept. Compare canonicalized forms:
        // `resolve_main_repo_root`'s pure-jj path returns a non-canonical
        // root, while `resolve_current_worktree_root` always canonicalizes —
        // a string `!=` could otherwise double-apply the same file.
        let main_root_canon = main_root.canonicalize().unwrap_or_else(|_| main_root.clone());
        let main_root_canon = strip_verbatim_prefix(main_root_canon);
        let worktree = match resolve_current_worktree_root(&cwd) {
            Some(wt_root) if wt_root != main_root_canon => {
                let path = wt_root.join(WORKSPACE_CONFIG_FILENAME);
                if path.exists() {
                    Some(load_project_file(&path)?)
                } else {
                    None
                }
            }
            _ => None,
        };

        Ok((repo, worktree))
    }
}

/// Parse a single project-config file. Missing file → default (caller usually
/// resolves the path with [`project_config_path_with_fallback`] first).
fn load_project_file(path: &Path) -> Result<ProjectConfig> {
    if !path.exists() {
        return Ok(ProjectConfig::default());
    }
    let content = std::fs::read_to_string(path)?;
    Ok(toml::from_str(&content)?)
}

/// Fold the global layer under the (already layer-folded) project layer into
/// the runtime [`Config`]. Pure (no I/O) so the merge semantics are unit-
/// testable — `base_dir`/`workspaces_dir` are the only I/O-derived inputs and
/// are passed in. Per-field rules:
///   - `merge_strategy`/`sync_strategy`/`open_in_new_tab`/`use_cow`: Option
///     override — project wins when set, else global's value.
///   - `copy_files`: append (global first, then project).
///   - `hooks`: replace-per-phase when the project phase is non-empty (see
///     [`merge_hooks`]) — NOT append.
///   - `trunk`/`vcs`/`workspace_alias`/`use_path_hash`/`copy_excludes`:
///     project-only (no global counterpart); `vcs_global` is kept separate so
///     `resolve_backend` can see both layers.
///
/// **Adding a config field?** Wire it here and extend the `merge_*` coverage
/// test below so a forgotten merge fails CI instead of silently dropping the
/// project (or global) value.
fn merge_global_project(
    global: GlobalConfig,
    project: ProjectConfig,
    base_dir: PathBuf,
    workspaces_dir: PathBuf,
) -> Config {
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
    let workspace_alias = project.workspace.alias.clone();
    let use_path_hash = project.workspace.use_path_hash.unwrap_or(false);
    let copy_excludes = project.copy.exclude.clone();

    Config {
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
        workspace_alias,
        use_path_hash,
        copy_excludes,
    }
}

/// Fold two project-config layers; `over` (worktree) wins over `base` (repo).
/// Mirrors the global-vs-project rules: Option fields override, `copy_files`
/// appends (base then over), hooks replace-per-phase when `over` is non-empty,
/// and `copy.exclude` appends+dedups (exclusions are cumulative, not a single
/// policy like hooks).
fn merge_project_layers(base: ProjectConfig, over: ProjectConfig) -> ProjectConfig {
    let mut copy_files = base.general.copy_files;
    copy_files.extend(over.general.copy_files);

    let mut exclude = base.copy.exclude;
    for pat in over.copy.exclude {
        if !exclude.contains(&pat) {
            exclude.push(pat);
        }
    }

    ProjectConfig {
        general: ProjectGeneralConfig {
            trunk: over.general.trunk.or(base.general.trunk),
            merge_strategy: over.general.merge_strategy.or(base.general.merge_strategy),
            sync_strategy: over.general.sync_strategy.or(base.general.sync_strategy),
            copy_files,
            vcs: over.general.vcs.or(base.general.vcs),
        },
        hooks: HooksConfig {
            post_create: merge_hooks(&base.hooks.post_create, &over.hooks.post_create),
            pre_merge: merge_hooks(&base.hooks.pre_merge, &over.hooks.pre_merge),
            post_merge: merge_hooks(&base.hooks.post_merge, &over.hooks.post_merge),
        },
        ui: ProjectUiConfig {
            open_in_new_tab: over.ui.open_in_new_tab.or(base.ui.open_in_new_tab),
        },
        create: ProjectCreateConfig {
            use_cow: over.create.use_cow.or(base.create.use_cow),
        },
        workspace: ProjectWorkspaceConfig {
            alias: over.workspace.alias.or(base.workspace.alias),
            use_path_hash: over.workspace.use_path_hash.or(base.workspace.use_path_hash),
        },
        copy: ProjectCopyConfig { exclude },
    }
}

/// What [`detect_vcs_at`] found: the marker directory plus which backends it
/// carries. Backend-independent, computed by a cheap filesystem walk (config
/// loads before the async runtime / VCS clients are up, so it can't use them).
struct DetectedVcs {
    /// Nearest ancestor of `cwd` carrying `.jj` or `.git` — the repo/worktree root.
    root: PathBuf,
    /// Whether that directory has a `.git` entry (dir in a normal repo, gitlink
    /// file inside a linked worktree).
    has_git: bool,
}

/// Find the nearest ancestor of `cwd` (inclusive) carrying `.jj` or `.git`.
/// Replaces the former `vcs_runner::detect_vcs` for config-time discovery.
fn detect_vcs_at(cwd: &Path) -> Option<DetectedVcs> {
    let mut current = Some(cwd);
    while let Some(dir) = current {
        let has_jj = dir.join(".jj").is_dir();
        let has_git = dir.join(".git").exists();
        if has_jj || has_git {
            return Some(DetectedVcs { root: dir.to_path_buf(), has_git });
        }
        current = dir.parent();
    }
    None
}

/// Run `git <args>` in `cwd`, returning UTF-8 stdout on success. Sync and
/// `std::process`-based on purpose: config resolution happens before the tokio
/// runtime / async VCS clients exist, so it can't go through them.
fn run_git_utf8(cwd: &Path, args: &[&str]) -> std::io::Result<String> {
    let out = std::process::Command::new("git").current_dir(cwd).args(args).output()?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(std::io::Error::other("git exited non-zero"))
    }
}

/// Resolve the **main repo** root for project-config discovery, handling
/// git worktrees correctly.
///
/// This is the backend-independent counterpart to the git backend's
/// `repo::repo_root` helper, used during `Config::load` (which runs before any
/// backend is installed).
/// See [`Config::load_project`] for the rationale.
fn resolve_main_repo_root(cwd: &Path) -> Option<PathBuf> {
    use std::path::Component;

    let detected = detect_vcs_at(cwd)?;

    // For git or colocated repos, `--git-common-dir` correctly resolves
    // gitlink files (used by git worktrees) to the main repo's `.git`.
    if detected.has_git
        && let Ok(common_dir) = run_git_utf8(cwd, &["rev-parse", "--git-common-dir"])
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
                //
                // If no `.git` component is found (bare repo whose git dir
                // isn't named `.git`, custom GIT_DIR, etc.), DON'T abort the
                // whole function — fall through to the `detect_root` fallback
                // below. Using `?` here would return `None` and silently drop
                // ALL repo-level config for those layouts.
                let mut current = Some(canonical.as_path());
                while let Some(c) = current {
                    if c.components()
                        .next_back()
                        .is_some_and(|comp| matches!(comp, Component::Normal(s) if s == ".git"))
                    {
                        if let Some(parent) = c.parent() {
                            return Some(parent.to_path_buf());
                        }
                        break;
                    }
                    current = c.parent();
                }
            }
        }
    }

    // Pure-jj or git-unavailable fallback: use the detected marker dir
    // directly. For pure-jj there are no worktree gitlinks to resolve;
    // it IS the repo root.
    Some(detected.root)
}

/// Resolve the **current worktree** root (the working-copy root for `cwd`),
/// as opposed to the main repo root from [`resolve_main_repo_root`].
///
/// Backend-independent (runs before any backend is installed — see
/// [`Config::load_project_layers`]). For git it uses
/// `git rev-parse --show-toplevel`, which returns the linked worktree's root
/// when inside one and the main repo root when in the main checkout. The
/// pure-jj / git-unavailable path falls through to `detect_vcs`'s nearest-`.jj`
/// root. Returns canonicalized + verbatim-stripped paths so the equality
/// check against the main repo root in the loader is reliable.
fn resolve_current_worktree_root(cwd: &Path) -> Option<PathBuf> {
    let detected = detect_vcs_at(cwd)?;

    if detected.has_git
        && let Ok(toplevel) = run_git_utf8(cwd, &["rev-parse", "--show-toplevel"])
    {
        let toplevel = toplevel.trim();
        if !toplevel.is_empty()
            && let Ok(canonical) = PathBuf::from(toplevel).canonicalize()
        {
            return Some(strip_verbatim_prefix(canonical));
        }
    }

    // Pure-jj / git-unavailable fallback: nearest `.jj` ancestor is the
    // workspace root for `cwd`. Canonicalize for a reliable equality check.
    let detect_root = detected.root.canonicalize().unwrap_or(detected.root);
    Some(strip_verbatim_prefix(detect_root))
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

    /// Exhaustive merge-semantics coverage for [`merge_global_project`]: every
    /// field is set DIFFERENTLY in the global vs project layers, and each
    /// assertion pins the documented rule (override / append / replace /
    /// project-only). Adding a config field without wiring its merge will leave
    /// one of these assertions failing (or the struct literal incomplete), so a
    /// forgotten merge can't ship silently.
    #[test]
    fn merge_global_project_covers_every_field() {
        let global = GlobalConfig {
            general: GeneralConfig {
                merge_strategy: MergeStrategy::Squash,
                sync_strategy: SyncStrategy::Rebase,
                copy_files: vec!["global.env".into()],
                vcs: Some(crate::vcs::VcsChoice::Git),
            },
            hooks: HooksConfig {
                post_create: vec!["global-pc".into()],
                pre_merge: vec!["global-pm".into()],
                post_merge: vec!["global-pom".into()],
            },
            ui: UiConfig { open_in_new_tab: true },
            create: CreateConfig { use_cow: true },
        };
        let project = ProjectConfig {
            general: ProjectGeneralConfig {
                trunk: Some("develop".into()),
                merge_strategy: Some(MergeStrategy::Merge),
                sync_strategy: Some(SyncStrategy::Merge),
                copy_files: vec!["project.env".into()],
                vcs: Some(crate::vcs::VcsChoice::Jj),
            },
            hooks: HooksConfig {
                // Non-empty project phases REPLACE the global ones (not append).
                post_create: vec!["project-pc".into()],
                pre_merge: vec![], // empty → keep global's pre_merge
                post_merge: vec!["project-pom".into()],
            },
            ui: ProjectUiConfig { open_in_new_tab: Some(false) },
            create: ProjectCreateConfig { use_cow: Some(false) },
            workspace: ProjectWorkspaceConfig {
                alias: Some("my-alias".into()),
                use_path_hash: Some(true),
            },
            copy: ProjectCopyConfig {
                exclude: vec!["/target".into()],
            },
        };

        let cfg = merge_global_project(
            global,
            project,
            PathBuf::from("/base"),
            PathBuf::from("/base"),
        );

        // Option-override: project wins when set.
        assert_eq!(cfg.merge_strategy, MergeStrategy::Merge);
        assert_eq!(cfg.sync_strategy, SyncStrategy::Merge);
        assert!(!cfg.open_in_new_tab);
        assert!(!cfg.use_cow);
        // Append: global first, then project.
        assert_eq!(cfg.copy_files, vec!["global.env", "project.env"]);
        // Hooks replace-per-phase only when the project phase is non-empty.
        assert_eq!(cfg.hooks.post_create, vec!["project-pc"]);
        assert_eq!(cfg.hooks.pre_merge, vec!["global-pm"]); // empty project → global kept
        assert_eq!(cfg.hooks.post_merge, vec!["project-pom"]);
        // Project-only fields.
        assert_eq!(cfg.trunk.as_deref(), Some("develop"));
        assert_eq!(cfg.vcs, Some(crate::vcs::VcsChoice::Jj));
        assert_eq!(cfg.workspace_alias.as_deref(), Some("my-alias"));
        assert!(cfg.use_path_hash);
        assert_eq!(cfg.copy_excludes, vec!["/target"]);
        // vcs_global is kept separate so resolve_backend sees both layers.
        assert_eq!(cfg.vcs_global, Some(crate::vcs::VcsChoice::Git));
        // I/O-derived paths pass through unchanged.
        assert_eq!(cfg.base_dir, PathBuf::from("/base"));
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
        // Exercise the fallback branch directly (env override = None) rather
        // than `base_dir()`, which reads the ambient `AGENT_WORKSPACE_DIR` —
        // a developer/CI machine that actually uses `ws` has that set to a
        // custom path, which would make a `base_dir()`-based assertion fail
        // spuriously. `resolve_base_dir` was split out for exactly this.
        let result = Config::resolve_base_dir(None);
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
            workspace: ProjectWorkspaceConfig::default(),
            copy: ProjectCopyConfig::default(),
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

    // =========================================================================
    // 3-tier layering: merge_project_layers + project_config_path_with_fallback
    // =========================================================================

    fn project_with_alias(alias: &str) -> ProjectConfig {
        ProjectConfig {
            workspace: ProjectWorkspaceConfig {
                alias: Some(alias.to_string()),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn test_layers_option_override_worktree_wins() {
        let repo = project_with_alias("repo");
        let worktree = project_with_alias("worktree");
        let merged = merge_project_layers(repo, worktree);
        assert_eq!(merged.workspace.alias, Some("worktree".to_string()));
    }

    #[test]
    fn test_layers_option_falls_back_to_repo_when_worktree_none() {
        let mut repo = ProjectConfig::default();
        repo.general.trunk = Some("develop".to_string());
        let worktree = ProjectConfig::default();
        let merged = merge_project_layers(repo, worktree);
        assert_eq!(merged.general.trunk, Some("develop".to_string()));
    }

    #[test]
    fn test_layers_copy_files_append_repo_then_worktree() {
        let mut repo = ProjectConfig::default();
        repo.general.copy_files = vec![".env".to_string()];
        let mut worktree = ProjectConfig::default();
        worktree.general.copy_files = vec![".env.local".to_string()];
        let merged = merge_project_layers(repo, worktree);
        assert_eq!(merged.general.copy_files, vec![".env", ".env.local"]);
    }

    #[test]
    fn test_layers_hooks_replace_when_worktree_nonempty() {
        let mut repo = ProjectConfig::default();
        repo.hooks.post_create = vec!["repo-hook".to_string()];
        let mut worktree = ProjectConfig::default();
        worktree.hooks.post_create = vec!["wt-hook".to_string()];
        let merged = merge_project_layers(repo, worktree);
        assert_eq!(merged.hooks.post_create, vec!["wt-hook"]);
    }

    #[test]
    fn test_layers_hooks_inherit_repo_when_worktree_empty() {
        let mut repo = ProjectConfig::default();
        repo.hooks.post_create = vec!["repo-hook".to_string()];
        let worktree = ProjectConfig::default();
        let merged = merge_project_layers(repo, worktree);
        assert_eq!(merged.hooks.post_create, vec!["repo-hook"]);
    }

    #[test]
    fn test_layers_copy_exclude_append_dedup() {
        let mut repo = ProjectConfig::default();
        repo.copy.exclude = vec!["/target".to_string(), "/Bin".to_string()];
        let mut worktree = ProjectConfig::default();
        worktree.copy.exclude = vec!["/Bin".to_string(), "**/*.iso".to_string()];
        let merged = merge_project_layers(repo, worktree);
        // /Bin appears once; order preserved (repo first, then new worktree).
        assert_eq!(merged.copy.exclude, vec!["/target", "/Bin", "**/*.iso"]);
    }

    #[test]
    fn test_path_fallback_prefers_workspace_toml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(WORKSPACE_CONFIG_FILENAME), "").unwrap();
        std::fs::write(dir.path().join(LEGACY_PROJECT_CONFIG_FILENAME), "").unwrap();
        assert_eq!(
            project_config_path_with_fallback(dir.path()),
            dir.path().join(WORKSPACE_CONFIG_FILENAME)
        );
    }

    #[test]
    fn test_path_fallback_uses_legacy_when_only_legacy() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(LEGACY_PROJECT_CONFIG_FILENAME), "").unwrap();
        assert_eq!(
            project_config_path_with_fallback(dir.path()),
            dir.path().join(LEGACY_PROJECT_CONFIG_FILENAME)
        );
    }

    #[test]
    fn test_path_fallback_write_target_when_neither() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            project_config_path_with_fallback(dir.path()),
            dir.path().join(WORKSPACE_CONFIG_FILENAME)
        );
    }

    #[test]
    fn test_local_exclude_file_none_without_git() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            local_exclude_file(dir.path()).is_none(),
            "no git dir → no local exclude file → caller skips"
        );
    }
}
