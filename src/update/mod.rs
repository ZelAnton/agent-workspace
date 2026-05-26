// ===========================================================================
// update - Version Update Check + Channel Detection + Self-update
// ===========================================================================

use std::path::Path;
use std::time::{Duration, SystemTime};

pub type Result<T> = std::result::Result<T, Error>;

const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60); // 24 hours
const MARKER_FILE: &str = "last_update_check";
const CHANNEL_FILE: &str = "install_channel";

/// GitHub repo for releases. Canonical source of truth for both update checks
/// and shell-installer downloads.
pub const GITHUB_REPO: &str = "ZelAnton/agent-workspace";

/// GitHub API endpoint returning the latest release JSON. Requires a User-Agent
/// header (otherwise GitHub returns 403).
pub const GITHUB_RELEASES_LATEST: &str =
    "https://api.github.com/repos/ZelAnton/agent-workspace/releases/latest";

/// User-Agent string sent to GitHub. Must be non-empty.
const USER_AGENT: &str = concat!("agent-workspace/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("network error: {0}")]
    Network(String),

    #[error("parse error: {0}")]
    Parse(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("unsupported platform for self-update: {0}-{1}")]
    UnsupportedPlatform(&'static str, &'static str),

    #[error("self-update failed: {0}")]
    SelfReplace(String),
}

// ---------------------------------------------------------------------------
// 24-hour check throttle
// ---------------------------------------------------------------------------

/// Check if we should perform update check (once per day)
pub fn should_check(base_dir: &Path) -> bool {
    let marker = base_dir.join(MARKER_FILE);
    if !marker.exists() {
        return true;
    }

    marker
        .metadata()
        .and_then(|m| m.modified())
        .map(|mtime| SystemTime::now().duration_since(mtime).unwrap_or_default() > CHECK_INTERVAL)
        .unwrap_or(true)
}

/// Mark that we've checked for updates
pub fn mark_checked(base_dir: &Path) -> Result<()> {
    let marker = base_dir.join(MARKER_FILE);
    std::fs::write(&marker, "")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Install channel detection
// ---------------------------------------------------------------------------

/// How this `ws` binary was installed. Determines how `ws update` performs
/// the actual update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// Installed via `npm install -g @zelanton/agent-workspace`. Update by re-running npm.
    Npm,
    /// Installed via the shell installer script. Update by downloading from
    /// GitHub Releases and self-replacing.
    Shell,
}

/// Read the channel marker file at `<base_dir>/install_channel`.
///
/// Missing or unrecognized → `Channel::Npm` (default for backwards compatibility
/// with installs that predate the marker file).
pub fn detect_channel(base_dir: &Path) -> Channel {
    let marker = base_dir.join(CHANNEL_FILE);
    match std::fs::read_to_string(&marker)
        .ok()
        .as_deref()
        .map(str::trim)
    {
        Some("shell") => Channel::Shell,
        _ => Channel::Npm,
    }
}

/// Write the channel marker. Used by the npm postinstall and shell installer
/// to stamp the install channel.
pub fn write_channel(base_dir: &Path, channel: Channel) -> Result<()> {
    let marker = base_dir.join(CHANNEL_FILE);
    let value = match channel {
        Channel::Npm => "npm",
        Channel::Shell => "shell",
    };
    std::fs::write(&marker, value)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Platform key — canonical mapping shared with npm/agent-workspace/bin/ws.js
// and the CI release archive naming.
// ---------------------------------------------------------------------------

/// Returns the canonical platform key (e.g. "darwin-arm64") for the current
/// host, matching the keys used in npm's `ws.js`, release archive names, and
/// installer scripts. `None` if the host isn't supported.
pub fn platform_key() -> Option<&'static str> {
    use std::env::consts::{ARCH, OS};
    match (OS, ARCH) {
        ("macos", "aarch64") => Some("darwin-arm64"),
        // Intel Mac (darwin-x64) is intentionally NOT packaged — the
        // macos-13 GitHub runner is too flaky to keep in the release
        // matrix. Intel Mac users build from source via cargo.
        ("linux", "x86_64") => Some("linux-x64"),
        ("windows", "x86_64") => Some("win32-x64"),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Version comparison
// ---------------------------------------------------------------------------

/// Compare versions: returns true if latest > current.
///
/// Pre-release tags (e.g. "0.11.0-rc1") have any non-numeric suffix stripped
/// before comparison — so "0.11.0-rc1" parses as "0.11.0" for ordering. This
/// matches the loose semver tolerance expected for `ws update`.
pub fn compare_versions(current: &str, latest: &str) -> bool {
    let parse = |v: &str| -> Vec<u32> {
        v.split(['.', '-', '+'])
            .filter_map(|s| s.parse().ok())
            .collect()
    };

    let current_parts = parse(current);
    let latest_parts = parse(latest);

    for i in 0..current_parts.len().max(latest_parts.len()) {
        let c = current_parts.get(i).copied().unwrap_or(0);
        let l = latest_parts.get(i).copied().unwrap_or(0);
        if l > c {
            return true;
        }
        if l < c {
            return false;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// GitHub Releases — latest version + asset URLs
// ---------------------------------------------------------------------------

/// Strip a leading 'v' from a tag like "v0.11.0" → "0.11.0".
fn strip_v_prefix(tag: &str) -> &str {
    tag.strip_prefix('v').unwrap_or(tag)
}

/// Build the asset download URL for a release.
///
/// e.g. version="0.11.0", platform="linux-x64" →
/// "https://github.com/ZelAnton/agent-workspace/releases/download/v0.11.0/agent-workspace-0.11.0-linux-x64.tar.gz"
pub fn asset_url(version: &str, platform: &str) -> String {
    format!(
        "https://github.com/{}/releases/download/v{}/agent-workspace-{}-{}.tar.gz",
        GITHUB_REPO, version, version, platform
    )
}

fn http_agent() -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::config::Config::builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .user_agent(USER_AGENT)
            .build(),
    )
}

/// Check for updates from GitHub Releases.
/// Returns Some(latest_version) if update available, None otherwise.
pub fn check_update(current_version: &str) -> Result<Option<String>> {
    let body: String = http_agent()
        .get(GITHUB_RELEASES_LATEST)
        .call()
        .map_err(|e| Error::Network(e.to_string()))?
        .body_mut()
        .read_to_string()
        .map_err(|e| Error::Parse(e.to_string()))?;

    #[derive(serde::Deserialize)]
    struct Release {
        tag_name: String,
    }
    let release: Release = serde_json::from_str(&body).map_err(|e| Error::Parse(e.to_string()))?;

    let latest = strip_v_prefix(&release.tag_name);

    if compare_versions(current_version, latest) {
        Ok(Some(latest.to_string()))
    } else {
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// Self-update (shell channel)
// ---------------------------------------------------------------------------

/// Download the release archive for `version`, extract `ws`/`ws.exe`, and
/// atomically replace the currently running binary via `self_replace`.
///
/// Caller is expected to re-invoke `ws setup` afterwards if shell wrappers
/// might have changed.
pub fn self_update(version: &str) -> Result<()> {
    let platform = platform_key().ok_or(Error::UnsupportedPlatform(
        std::env::consts::OS,
        std::env::consts::ARCH,
    ))?;
    let url = asset_url(version, platform);

    // Streaming download to a tempfile.
    let temp_dir = tempfile::Builder::new()
        .prefix("agent-workspace-update-")
        .tempdir()?;
    let archive_path = temp_dir.path().join("ws.tar.gz");

    let mut response = http_agent()
        .get(&url)
        .call()
        .map_err(|e| Error::Network(format!("download failed: {e}")))?;
    let mut reader = response.body_mut().as_reader();
    let mut file = std::fs::File::create(&archive_path)?;
    std::io::copy(&mut reader, &mut file)?;
    drop(file);

    // Extract via flate2 + tar (avoids shelling out to `tar`).
    let tar_gz = std::fs::File::open(&archive_path)?;
    let tar = flate2::read::GzDecoder::new(tar_gz);
    let mut archive = tar::Archive::new(tar);
    archive.unpack(temp_dir.path())?;

    // The archive places `ws` (or `ws.exe`) at the top level.
    let bin_name = if cfg!(windows) { "ws.exe" } else { "ws" };
    let new_binary = temp_dir.path().join(bin_name);
    if !new_binary.exists() {
        return Err(Error::Parse(format!(
            "expected {} inside archive, not found",
            bin_name
        )));
    }

    // Atomic replace of the running binary. On Windows this uses the
    // rename-trick (move running exe aside, drop new one in place).
    self_replace::self_replace(&new_binary).map_err(|e| Error::SelfReplace(e.to_string()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    #[test]
    fn test_should_check_no_marker_file() {
        // No marker file = should check
        let temp = TempDir::new().unwrap();
        assert!(should_check(temp.path()));
    }

    #[test]
    fn test_should_check_fresh_marker() {
        // Fresh marker (just created) = should NOT check
        let temp = TempDir::new().unwrap();
        let marker = temp.path().join("last_update_check");
        std::fs::write(&marker, "").unwrap();
        assert!(!should_check(temp.path()));
    }

    #[test]
    fn test_should_check_stale_marker() {
        // Marker older than 24 hours = should check
        let temp = TempDir::new().unwrap();
        let marker = temp.path().join("last_update_check");
        std::fs::write(&marker, "").unwrap();

        // Set mtime to 25 hours ago
        let old_time = std::time::SystemTime::now() - std::time::Duration::from_secs(25 * 60 * 60);
        filetime::set_file_mtime(&marker, filetime::FileTime::from_system_time(old_time)).unwrap();

        assert!(should_check(temp.path()));
    }

    #[test]
    fn test_mark_checked_creates_marker() {
        let temp = TempDir::new().unwrap();
        let marker = temp.path().join("last_update_check");
        assert!(!marker.exists());

        mark_checked(temp.path()).unwrap();

        assert!(marker.exists());
    }

    #[test]
    fn test_check_update_same_version_returns_none() {
        // Mock this by testing version comparison logic
        let result = compare_versions("0.4.5", "0.4.5");
        assert!(!result);
    }

    #[test]
    fn test_check_update_older_version_returns_none() {
        let result = compare_versions("0.4.5", "0.4.4");
        assert!(!result);
    }

    #[test]
    fn test_check_update_newer_version_returns_true() {
        let result = compare_versions("0.4.5", "0.4.6");
        assert!(result);

        let result = compare_versions("0.4.5", "0.5.0");
        assert!(result);

        let result = compare_versions("0.4.5", "1.0.0");
        assert!(result);
    }

    #[test]
    fn test_compare_versions_edge_cases() {
        // Different segment counts
        assert!(compare_versions("0.4", "0.4.1"));
        assert!(!compare_versions("0.4.1", "0.4"));

        // Large numbers
        assert!(compare_versions("0.9.9", "0.10.0"));
    }

    #[test]
    fn test_compare_versions_strips_prerelease_suffix() {
        // "0.11.0-rc1" → parsed as 0.11.0 → equal to 0.11.0
        assert!(!compare_versions("0.11.0-rc1", "0.11.0"));
        // 0.11.0 < 0.12.0-rc1 (parsed as 0.12.0)
        assert!(compare_versions("0.11.0", "0.12.0-rc1"));
    }

    #[test]
    fn test_strip_v_prefix() {
        assert_eq!(strip_v_prefix("v0.11.0"), "0.11.0");
        assert_eq!(strip_v_prefix("0.11.0"), "0.11.0");
        assert_eq!(strip_v_prefix("v1.0.0-rc1"), "1.0.0-rc1");
    }

    #[test]
    fn test_asset_url() {
        let url = asset_url("0.11.0", "linux-x64");
        assert_eq!(
            url,
            "https://github.com/ZelAnton/agent-workspace/releases/download/v0.11.0/agent-workspace-0.11.0-linux-x64.tar.gz"
        );
    }

    #[test]
    fn test_detect_channel_missing_defaults_to_npm() {
        let temp = TempDir::new().unwrap();
        assert_eq!(detect_channel(temp.path()), Channel::Npm);
    }

    #[test]
    fn test_detect_channel_shell() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("install_channel"), "shell").unwrap();
        assert_eq!(detect_channel(temp.path()), Channel::Shell);
    }

    #[test]
    fn test_detect_channel_npm_explicit() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("install_channel"), "npm").unwrap();
        assert_eq!(detect_channel(temp.path()), Channel::Npm);
    }

    #[test]
    fn test_detect_channel_trims_whitespace() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("install_channel"), "  shell\n").unwrap();
        assert_eq!(detect_channel(temp.path()), Channel::Shell);
    }

    #[test]
    fn test_detect_channel_unknown_value_defaults_to_npm() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("install_channel"), "homebrew").unwrap();
        assert_eq!(detect_channel(temp.path()), Channel::Npm);
    }

    #[test]
    fn test_write_channel_roundtrip() {
        let temp = TempDir::new().unwrap();
        write_channel(temp.path(), Channel::Shell).unwrap();
        assert_eq!(detect_channel(temp.path()), Channel::Shell);

        write_channel(temp.path(), Channel::Npm).unwrap();
        assert_eq!(detect_channel(temp.path()), Channel::Npm);
    }

    #[test]
    fn test_platform_key_current_host() {
        // We can't assert a specific value (depends on test host), but the
        // function shouldn't panic and should return Some on any supported CI
        // target. Just exercise it.
        let _ = platform_key();
    }
}
