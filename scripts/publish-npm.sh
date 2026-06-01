#!/bin/bash
# ============================================================
# Publish npm packages
# ============================================================

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
NPM_DIR="$PROJECT_ROOT/npm"

# ============================================================
# Helpers
# ============================================================

log() {
    echo "[publish-npm] $1"
}

error() {
    echo "[publish-npm] ERROR: $1" >&2
    exit 1
}

# ============================================================
# Version Sync
# ============================================================

sync_versions() {
    local version=$1

    log "Syncing version to $version..."

    for pkg_dir in "$NPM_DIR"/agent-workspace*/; do
        local pkg_json="$pkg_dir/package.json"
        if [[ -f "$pkg_json" ]]; then
            # macOS sed needs different syntax
            if [[ "$(uname)" == "Darwin" ]]; then
                sed -i '' "s/\"version\": \".*\"/\"version\": \"$version\"/" "$pkg_json"
            else
                sed -i "s/\"version\": \".*\"/\"version\": \"$version\"/" "$pkg_json"
            fi
            log "Updated $(basename "$pkg_dir")"
        fi
    done

    # Also update optionalDependencies versions in main package
    local main_pkg="$NPM_DIR/agent-workspace/package.json"
    for platform in darwin-arm64 linux-x64 win32-x64; do
        if [[ "$(uname)" == "Darwin" ]]; then
            sed -i '' "s/\"@zelanton\/agent-workspace-$platform\": \".*\"/\"@zelanton\/agent-workspace-$platform\": \"$version\"/" "$main_pkg"
        else
            sed -i "s/\"@zelanton\/agent-workspace-$platform\": \".*\"/\"@zelanton\/agent-workspace-$platform\": \"$version\"/" "$main_pkg"
        fi
    done
}

# ============================================================
# Registry Check
# ============================================================

is_published() {
    local pkg_name=$1
    local version=$2
    npm view "${pkg_name}@${version}" version &>/dev/null
}

# ============================================================
# Publish
# ============================================================

publish_package() {
    local pkg_dir=$1
    local pkg_name=$2
    local version=$3
    local dry_run=$4

    # Idempotency: a prior (partial) run may have already published this exact
    # version — skip so a re-run sails past it instead of erroring. This matters
    # because a release publishes FOUR packages: if the previous run died after
    # package 2 of 4, the re-run must skip 1-2 and finish 3-4.
    if is_published "$pkg_name" "$version"; then
        log "Skipping $pkg_name@$version (already published)"
        return 0
    fi

    cd "$pkg_dir"

    if [[ "$dry_run" == "true" ]]; then
        # Packaging/metadata validation ONLY — no upload, no provenance (which
        # needs the real registry round-trip). Run before the irreversible real
        # publish so a malformed package fails cheaply instead of consuming the
        # version. `set -e` aborts the script if this fails.
        log "Validating $pkg_name@$version (dry run)..."
        npm publish --dry-run --access public
        return 0
    fi

    # `--provenance` records GitHub-OIDC-signed attestations on npm so consumers
    # can verify the package was built from this repo's release.yml workflow.
    # It's a no-op outside CI (gracefully degrades) but a hard requirement once
    # Trusted Publishing is configured — npm rejects unprovenance'd CI publishes
    # then.
    #
    # Retry transient registry/network failures. Treat "this exact version is
    # already published" as success — covers a prior run that uploaded THIS
    # package but died before finishing the rest. The phrasing match is narrow
    # (npm's actual wording) so an unrelated error can never masquerade as a
    # successful publish and let a re-run tag an unpublished version.
    log "Publishing $pkg_name@$version..."
    local attempt=0
    local max=3
    local out
    while :; do
        attempt=$((attempt + 1))
        if out="$(npm publish --provenance --access public 2>&1)"; then
            printf '%s\n' "$out"
            return 0
        fi
        printf '%s\n' "$out"
        if printf '%s' "$out" | grep -qiE 'cannot publish over the previously published version|previously published versions|EPUBLISHCONFLICT'; then
            log "$pkg_name@$version is already on the registry — treating as published."
            return 0
        fi
        if [[ "$attempt" -ge "$max" ]]; then
            error "npm publish failed for $pkg_name@$version after ${max} attempts."
        fi
        log "publish attempt ${attempt} for $pkg_name failed; retrying in 20s..."
        sleep 20
    done
}

# ============================================================
# Main
# ============================================================

VERSION=${1:-}
DRY_RUN=${2:-false}

if [[ -z "$VERSION" ]]; then
    # Read version from Cargo.toml
    VERSION=$(grep '^version' "$PROJECT_ROOT/Cargo.toml" | head -1 | sed 's/.*"\(.*\)".*/\1/')
    log "Using version from Cargo.toml: $VERSION"
fi

sync_versions "$VERSION"

# Publish platform packages first (main package depends on them)
for platform in darwin-arm64 linux-x64 win32-x64; do
    pkg_dir="$NPM_DIR/agent-workspace-$platform"
    pkg_name="@zelanton/agent-workspace-$platform"
    # Windows uses .exe extension
    if [[ "$platform" == "win32-x64" ]]; then
        binary="$pkg_dir/bin/ws.exe"
    else
        binary="$pkg_dir/bin/ws"
    fi
    if [[ -f "$binary" && -s "$binary" ]]; then
        publish_package "$pkg_dir" "$pkg_name" "$VERSION" "$DRY_RUN"
    else
        log "Skipping $pkg_name (no binary found)"
    fi
done

# Publish main package last
publish_package "$NPM_DIR/agent-workspace" "@zelanton/agent-workspace" "$VERSION" "$DRY_RUN"

log "Publish complete!"
