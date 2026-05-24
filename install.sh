#!/bin/sh
# ============================================================================
# install.sh — agent-workspace shell installer (macOS / Linux)
# ============================================================================
#
# Quick install:
#   curl -fsSL https://github.com/ZelAnton/agent-workspace/releases/latest/download/install.sh | sh
#
# Environment variables (all optional):
#   AGENT_WORKSPACE_VERSION       Pin to a specific version (default: latest GitHub release)
#   AGENT_WORKSPACE_INSTALL_DIR   Install dir for the `wt` binary
#                                 (default: $AGENT_WORKSPACE_DIR/bin = ~/.agent-workspace/bin)
#   AGENT_WORKSPACE_DIR           Base dir for channel marker and worktrees
#                                 (default: ~/.agent-workspace)
#
# This script:
#   1. Detects platform (darwin-arm64 / linux-x64).
#   2. Downloads the matching .tar.gz from GitHub Releases.
#   3. Installs `wt` to ~/.agent-workspace/bin.
#   4. Appends a PATH-export block to the user's shell rc with strict
#      BEGIN/END markers (matching src/shell/mod.rs discipline).
#   5. Writes ~/.agent-workspace/install_channel = "shell" so `wt update`
#      self-updates via GitHub Releases instead of npm.
#   6. Runs `wt setup` to install the shell wrappers required for
#      `wt cd`, `wt new`, etc. to change the user's cwd.
# ============================================================================

set -eu

GITHUB_REPO="ZelAnton/agent-workspace"

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

info() {
    printf '[agent-workspace] %s\n' "$*"
}

err() {
    printf '[agent-workspace] ERROR: %s\n' "$*" >&2
    exit 1
}

need_cmd() {
    if ! command -v "$1" >/dev/null 2>&1; then
        err "required command not found: $1"
    fi
}

need_cmd uname
need_cmd curl
need_cmd tar
need_cmd mktemp

# ---------------------------------------------------------------------------
# Platform detection — must match keys in npm/agent-workspace/bin/wt.js and
# the CI release archive naming (agent-workspace-<version>-<platform>.tar.gz).
# ---------------------------------------------------------------------------

uname_s=$(uname -s)
uname_m=$(uname -m)

case "$uname_s" in
    Darwin) os="darwin" ;;
    Linux)  os="linux"  ;;
    *) err "unsupported OS: $uname_s" ;;
esac

case "$uname_m" in
    x86_64|amd64)  arch="x64"   ;;
    aarch64|arm64) arch="arm64" ;;
    *) err "unsupported arch: $uname_m" ;;
esac

platform="${os}-${arch}"

case "$platform" in
    darwin-arm64|linux-x64) : ;;
    *) err "no prebuilt binary for $platform (supported: darwin-arm64, linux-x64). Intel Mac (darwin-x64) is not packaged — build from source via 'cargo install --path .'." ;;
esac

# ---------------------------------------------------------------------------
# Version resolution
# ---------------------------------------------------------------------------

if [ -n "${AGENT_WORKSPACE_VERSION:-}" ]; then
    version="$AGENT_WORKSPACE_VERSION"
    info "Using pinned version $version"
else
    info "Resolving latest release..."
    api_url="https://api.github.com/repos/${GITHUB_REPO}/releases/latest"
    tag=$(curl -fsSL -H "User-Agent: agent-workspace-installer" "$api_url" \
        | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
        | head -n1)
    if [ -z "$tag" ]; then
        err "could not resolve latest release from $api_url"
    fi
    version="${tag#v}"
    info "Latest release: $version"
fi

# ---------------------------------------------------------------------------
# Paths
# ---------------------------------------------------------------------------

base_dir="${AGENT_WORKSPACE_DIR:-$HOME/.agent-workspace}"
install_dir="${AGENT_WORKSPACE_INSTALL_DIR:-$base_dir/bin}"

mkdir -p "$install_dir" "$base_dir"

# ---------------------------------------------------------------------------
# Download + extract
# ---------------------------------------------------------------------------

archive="agent-workspace-${version}-${platform}.tar.gz"
url="https://github.com/${GITHUB_REPO}/releases/download/v${version}/${archive}"

tmp_dir=$(mktemp -d 2>/dev/null || mktemp -d -t agent-workspace)
trap 'rm -rf "$tmp_dir"' EXIT INT TERM

info "Downloading $url"
if ! curl -fsSL -o "$tmp_dir/$archive" "$url"; then
    err "download failed — does release v$version exist for $platform?"
fi

info "Extracting $archive"
if ! tar -xzf "$tmp_dir/$archive" -C "$tmp_dir"; then
    err "extraction failed"
fi

if [ ! -f "$tmp_dir/wt" ]; then
    err "binary 'wt' not found in archive"
fi

mv "$tmp_dir/wt" "$install_dir/wt"
chmod +x "$install_dir/wt"
info "Installed $install_dir/wt"

# ---------------------------------------------------------------------------
# Channel marker — tells `wt update` to self-update from GitHub Releases
# rather than re-invoking npm. The Rust side reads this at
# update::detect_channel() in src/update/mod.rs.
# ---------------------------------------------------------------------------

printf 'shell' > "$base_dir/install_channel"

# ---------------------------------------------------------------------------
# PATH injection into shell rc file
#
# Strict marker pairing — refuses to touch a file with orphaned markers,
# mirrors src/shell/mod.rs discipline so an interrupted install + retry
# never corrupts the rc file.
# ---------------------------------------------------------------------------

marker_begin="# === agent-workspace installer BEGIN ==="
marker_end="# === agent-workspace installer END ==="

case "${SHELL:-}" in
    */zsh)  rc="$HOME/.zshrc"  ;;
    */bash)
        if [ "$os" = "darwin" ] && [ -f "$HOME/.bash_profile" ]; then
            rc="$HOME/.bash_profile"
        else
            rc="$HOME/.bashrc"
        fi
        ;;
    */fish) rc="$HOME/.config/fish/config.fish" ;;
    *)
        rc=""
        info "Could not detect shell (\$SHELL=${SHELL:-unset}); skipping PATH injection."
        info "Add to your shell rc manually:"
        info "    export PATH=\"$install_dir:\$PATH\""
        ;;
esac

if [ -n "$rc" ]; then
    mkdir -p "$(dirname "$rc")"
    touch "$rc"

    n_begin=$(grep -cF "$marker_begin" "$rc" 2>/dev/null || echo 0)
    n_end=$(grep -cF "$marker_end" "$rc" 2>/dev/null || echo 0)
    if [ "$n_begin" != "$n_end" ]; then
        err "orphaned installer markers in $rc — fix manually before retrying"
    fi

    if [ "$n_begin" = "0" ]; then
        case "$rc" in
            */config.fish)
                {
                    printf '\n%s\n' "$marker_begin"
                    printf 'set -gx PATH %s $PATH\n' "$install_dir"
                    printf '%s\n' "$marker_end"
                } >> "$rc"
                ;;
            *)
                {
                    printf '\n%s\n' "$marker_begin"
                    printf 'export PATH="%s:$PATH"\n' "$install_dir"
                    printf '%s\n' "$marker_end"
                } >> "$rc"
                ;;
        esac
        info "Added $install_dir to PATH in $rc"
    else
        info "PATH block already present in $rc"
    fi
fi

# ---------------------------------------------------------------------------
# Hand off to `wt setup` for shell wrapper installation
# ---------------------------------------------------------------------------

if "$install_dir/wt" setup; then
    info "Shell integration installed."
else
    info "wt setup failed — run '$install_dir/wt setup' manually."
fi

# ---------------------------------------------------------------------------
# Done
# ---------------------------------------------------------------------------

cat <<EOF

[agent-workspace] Done. Open a new shell (or 'source ${rc:-your shell rc}'),
then verify:

    wt --version

EOF
