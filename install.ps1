# ============================================================================
# install.ps1 — agent-workspace shell installer (Windows)
# ============================================================================
#
# Quick install:
#   iwr https://github.com/ZelAnton/agent-workspace/releases/latest/download/install.ps1 -UseBasicParsing | iex
#
# Environment variables (all optional):
#   AGENT_WORKSPACE_VERSION       Pin to a specific version (default: latest GitHub release)
#   AGENT_WORKSPACE_INSTALL_DIR   Install dir for ws.exe
#                                 (default: $AGENT_WORKSPACE_DIR\bin = ~\.agent-workspace\bin)
#   AGENT_WORKSPACE_DIR           Base dir for channel marker and worktrees
#                                 (default: ~\.agent-workspace)
#
# This script mirrors install.sh for Windows: detects platform, downloads the
# matching .tar.gz from GitHub Releases (uses Windows 10 1803+ built-in tar.exe),
# installs ws.exe, adds the install dir to User PATH, stamps the channel
# marker, and runs `ws setup`.
# ============================================================================

$ErrorActionPreference = 'Stop'

$GitHubRepo = 'ZelAnton/agent-workspace'

function Info($msg) { Write-Host "[agent-workspace] $msg" }
function Fail($msg) { Write-Host "[agent-workspace] ERROR: $msg" -ForegroundColor Red; exit 1 }

# ---------------------------------------------------------------------------
# Pre-flight — require tar.exe (Windows 10 1803+ / Windows 11)
# ---------------------------------------------------------------------------

if (-not (Get-Command tar -ErrorAction SilentlyContinue)) {
    Fail "tar.exe not found — install requires Windows 10 1803 or newer."
}

# ---------------------------------------------------------------------------
# Platform
# ---------------------------------------------------------------------------

# We only ship x64 builds for Windows currently; reject arm64 etc. up front.
$arch = $env:PROCESSOR_ARCHITECTURE
if ($arch -ne 'AMD64' -and $arch -ne 'x86_64') {
    Fail "unsupported arch: $arch (only x64 builds are published for Windows)"
}
$Platform = 'win32-x64'

# ---------------------------------------------------------------------------
# Version resolution
# ---------------------------------------------------------------------------

if ($env:AGENT_WORKSPACE_VERSION) {
    $Version = $env:AGENT_WORKSPACE_VERSION
    Info "Using pinned version $Version"
} else {
    Info "Resolving latest release..."
    $apiUrl = "https://api.github.com/repos/$GitHubRepo/releases/latest"
    $headers = @{ 'User-Agent' = 'agent-workspace-installer' }
    try {
        $release = Invoke-RestMethod -Uri $apiUrl -Headers $headers -UseBasicParsing
    } catch {
        Fail "could not resolve latest release: $_"
    }
    $Version = $release.tag_name -replace '^v', ''
    Info "Latest release: $Version"
}

# ---------------------------------------------------------------------------
# Paths
# ---------------------------------------------------------------------------

$BaseDir = if ($env:AGENT_WORKSPACE_DIR) { $env:AGENT_WORKSPACE_DIR } else { Join-Path $HOME '.agent-workspace' }
$InstallDir = if ($env:AGENT_WORKSPACE_INSTALL_DIR) { $env:AGENT_WORKSPACE_INSTALL_DIR } else { Join-Path $BaseDir 'bin' }

New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
New-Item -ItemType Directory -Path $BaseDir -Force | Out-Null

# ---------------------------------------------------------------------------
# Download + extract
# ---------------------------------------------------------------------------

$Archive = "agent-workspace-$Version-$Platform.tar.gz"
$Url = "https://github.com/$GitHubRepo/releases/download/v$Version/$Archive"

$TmpDir = Join-Path ([System.IO.Path]::GetTempPath()) "agent-workspace-install-$([System.Guid]::NewGuid().ToString())"
New-Item -ItemType Directory -Path $TmpDir -Force | Out-Null

try {
    $archivePath = Join-Path $TmpDir $Archive
    Info "Downloading $Url"
    try {
        Invoke-WebRequest -Uri $Url -OutFile $archivePath -UseBasicParsing -UserAgent 'agent-workspace-installer'
    } catch {
        Fail "download failed — does release v$Version exist for $Platform? $_"
    }

    Info "Extracting $Archive"
    & tar -xzf $archivePath -C $TmpDir
    if ($LASTEXITCODE -ne 0) { Fail "extraction failed (tar exit $LASTEXITCODE)" }

    $newBin = Join-Path $TmpDir 'ws.exe'
    if (-not (Test-Path $newBin)) { Fail "binary ws.exe not found in archive" }

    $targetExe = Join-Path $InstallDir 'ws.exe'

    # If a ws.exe is already in place AND it's the one currently running
    # (somehow), Windows would refuse the overwrite with a lock error.
    # `Move-Item -Force` handles the common case; for the running-binary case
    # users should use `ws update` instead of re-running install.ps1.
    Move-Item -Path $newBin -Destination $targetExe -Force
    Info "Installed $targetExe"
} finally {
    Remove-Item -Path $TmpDir -Recurse -Force -ErrorAction SilentlyContinue
}

# ---------------------------------------------------------------------------
# Channel marker
# ---------------------------------------------------------------------------

Set-Content -Path (Join-Path $BaseDir 'install_channel') -Value 'shell' -NoNewline -Encoding ASCII

# ---------------------------------------------------------------------------
# PATH injection (User scope) — persists across new shells.
# ---------------------------------------------------------------------------

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$pathParts = @()
if ($userPath) { $pathParts = $userPath -split ';' | Where-Object { $_ -ne '' } }

if ($pathParts -notcontains $InstallDir) {
    $newPath = if ($userPath) { "$userPath;$InstallDir" } else { $InstallDir }
    [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
    Info "Added $InstallDir to User PATH"
    # Also update the current session so the user can run `ws` right away
    # without opening a new shell.
    $env:Path = "$env:Path;$InstallDir"
} else {
    Info "$InstallDir already in User PATH"
}

# ---------------------------------------------------------------------------
# Hand off to `ws setup` for shell wrapper installation (PowerShell profile)
# ---------------------------------------------------------------------------

try {
    & (Join-Path $InstallDir 'ws.exe') setup
    Info "Shell integration installed."
} catch {
    Info "ws setup failed — run '$InstallDir\ws.exe setup' manually."
}

# ---------------------------------------------------------------------------
# Done
# ---------------------------------------------------------------------------

Write-Host ""
Write-Host "[agent-workspace] Done. Open a new PowerShell window (or restart"
Write-Host "                  current shell to pick up PATH + wrapper), then verify:"
Write-Host ""
Write-Host "    ws --version"
Write-Host ""
