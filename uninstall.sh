#!/bin/sh
# ============================================================================
# uninstall.sh — agent-workspace shell uninstaller (macOS / Linux)
# ============================================================================
#
# Quick uninstall:
#   curl -fsSL https://github.com/ZelAnton/agent-workspace/releases/latest/download/uninstall.sh | sh
#
# This script removes shell integration installed by `wt setup` or
# install.sh. It is the inverse of install.sh and works even if the
# `wt` binary is broken or missing.
#
# What it does:
#   1. Strip the `# === agent-workspace BEGIN/END ===` block from the
#      detected shell rc (matches the markers written by `wt setup`).
#   2. Strip the `# === agent-workspace installer BEGIN/END ===` PATH
#      block from the same rc (matches install.sh markers).
#   3. Print instructions for the manual cleanup it deliberately skips:
#      - the install directory `~/.agent-workspace/` (may contain worktree
#        state, channel marker).
#      - the npm package `@zelanton/agent-workspace`.
#
# Environment variables (all optional):
#   AGENT_WORKSPACE_DIR           Base dir (default: ~/.agent-workspace)
# ============================================================================

set -eu

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

info() { printf '[agent-workspace] %s\n' "$*"; }
err()  { printf '[agent-workspace] ERROR: %s\n' "$*" >&2; exit 1; }

# Markers — must match src/shell/mod.rs (wrapper) and install.sh (installer).
wrapper_begin='# === agent-workspace BEGIN ==='
wrapper_end='# === agent-workspace END ==='
installer_begin='# === agent-workspace installer BEGIN ==='
installer_end='# === agent-workspace installer END ==='

# ---------------------------------------------------------------------------
# Detect rc file (same logic as install.sh + src/shell/mod.rs::config_file)
# ---------------------------------------------------------------------------

uname_s=$(uname -s 2>/dev/null || echo unknown)

case "${SHELL:-}" in
    */zsh)  rc="$HOME/.zshrc"  ;;
    */bash)
        if [ "$uname_s" = "Darwin" ] && [ -f "$HOME/.bash_profile" ]; then
            rc="$HOME/.bash_profile"
        else
            rc="$HOME/.bashrc"
        fi
        ;;
    */fish) rc="$HOME/.config/fish/config.fish" ;;
    *)
        rc=""
        info "Could not detect shell from \$SHELL=${SHELL:-unset}."
        info "Manually delete the '# === agent-workspace BEGIN/END ===' block from your shell rc."
        ;;
esac

# ---------------------------------------------------------------------------
# Strip block helper. Uses awk: refuses to touch the file on unpaired markers
# (matches src/shell/mod.rs::remove_wrapper discipline), so a half-edited rc
# can't be silently truncated.
# ---------------------------------------------------------------------------

strip_block() {
    file="$1"
    begin="$2"
    end="$3"
    label="$4"

    if [ ! -f "$file" ]; then
        return 0
    fi

    # Count markers up front. Unpaired = abort.
    n_begin=$(grep -cF "$begin" "$file" 2>/dev/null || echo 0)
    n_end=$(grep -cF "$end"   "$file" 2>/dev/null || echo 0)
    if [ "$n_begin" != "$n_end" ]; then
        err "orphaned $label markers in $file (BEGIN=$n_begin, END=$n_end) — fix manually before retrying."
    fi
    if [ "$n_begin" = "0" ]; then
        return 0
    fi

    tmp=$(mktemp 2>/dev/null || mktemp -t agent-workspace-rc)
    awk -v b="$begin" -v e="$end" '
        BEGIN { in_block = 0 }
        index($0, b) > 0 {
            if (in_block) { print "nested begin" > "/dev/stderr"; exit 2 }
            in_block = 1; next
        }
        index($0, e) > 0 {
            if (!in_block) { print "end without begin" > "/dev/stderr"; exit 2 }
            in_block = 0; next
        }
        !in_block { print }
    ' "$file" > "$tmp" || err "awk failed processing $file"

    # Trim trailing blank lines so repeated uninstall+install cycles don't grow the file.
    awk 'BEGIN{n=0} {lines[n++]=$0} END{
        while (n > 0 && lines[n-1] ~ /^[[:space:]]*$/) n--;
        for (i = 0; i < n; i++) print lines[i]
    }' "$tmp" > "$tmp.2"

    mv "$tmp.2" "$file"
    rm -f "$tmp"
    info "Removed $label block from $file"
}

# ---------------------------------------------------------------------------
# 1+2. Strip both blocks from the detected rc
# ---------------------------------------------------------------------------

if [ -n "$rc" ]; then
    if [ -f "$rc" ]; then
        strip_block "$rc" "$wrapper_begin"   "$wrapper_end"   "wrapper"
        strip_block "$rc" "$installer_begin" "$installer_end" "installer PATH"
    else
        info "$rc not found — nothing to strip."
    fi
fi

# Fish has a separate completions file — clean it up too.
fish_completions="$HOME/.config/fish/completions/wt.fish"
if [ -f "$fish_completions" ]; then
    rm -f "$fish_completions"
    info "Removed fish completions: $fish_completions"
fi

# ---------------------------------------------------------------------------
# 3. Hints for manual cleanup we deliberately skip
# ---------------------------------------------------------------------------

base_dir="${AGENT_WORKSPACE_DIR:-$HOME/.agent-workspace}"

printf '\n'
if [ -d "$base_dir" ]; then
    info "Install dir kept: $base_dir"
    info "  (may contain channel marker + cached worktree state — delete manually if desired)"
    info "  rm -rf '$base_dir'"
fi
info "If installed via npm:  npm uninstall -g '@zelanton/agent-workspace'"
printf '\n'
info "Done. Open a new shell (or 'source ${rc:-your shell rc}') to drop the wrapper."
