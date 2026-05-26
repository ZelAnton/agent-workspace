# ============================================================================
# uninstall.ps1 — agent-workspace shell uninstaller (Windows)
# ============================================================================
#
# Quick uninstall:
#   iwr https://github.com/ZelAnton/agent-workspace/releases/latest/download/uninstall.ps1 -UseBasicParsing | iex
#
# This script removes shell integration installed by `wt setup` or
# install.ps1. It is the inverse of install.ps1 and works even if the
# `wt.exe` binary is broken or missing.
#
# What it does:
#   1. Strip the `# === agent-workspace BEGIN/END ===` block from the
#      PowerShell profile (matching the markers used by `wt setup`).
#   2. Remove the install dir from the User PATH (if AGENT_WORKSPACE_DIR
#      or the default ~\.agent-workspace\bin is present).
#   3. Print instructions for the manual cleanup it deliberately skips:
#      - the install directory `~\.agent-workspace\` (may contain worktree
#        state / channel marker / cached config).
#      - the npm package `@zelanton/agent-workspace` (if installed that
#        way) — `npm uninstall -g` is the canonical reverse there.
#
# Environment variables (all optional):
#   AGENT_WORKSPACE_INSTALL_DIR   Override path removed from PATH (default:
#                                 $AGENT_WORKSPACE_DIR\bin or ~\.agent-workspace\bin)
#   AGENT_WORKSPACE_DIR           Base dir (default: ~\.agent-workspace)
# ============================================================================

$ErrorActionPreference = 'Stop'

function Info($msg) { Write-Host "[agent-workspace] $msg" }
function Fail($msg) { Write-Host "[agent-workspace] ERROR: $msg" -ForegroundColor Red; exit 1 }

$MarkerBegin = '# === agent-workspace BEGIN ==='
$MarkerEnd   = '# === agent-workspace END ==='

# ---------------------------------------------------------------------------
# Paths
# ---------------------------------------------------------------------------

$BaseDir = if ($env:AGENT_WORKSPACE_DIR) { $env:AGENT_WORKSPACE_DIR } else { Join-Path $HOME '.agent-workspace' }
$InstallDir = if ($env:AGENT_WORKSPACE_INSTALL_DIR) { $env:AGENT_WORKSPACE_INSTALL_DIR } else { Join-Path $BaseDir 'bin' }

# PowerShell profile (current user, current host). Matches Shell::PowerShell.config_file().
$ProfilePath = Join-Path $HOME 'Documents\PowerShell\Microsoft.PowerShell_profile.ps1'

# ---------------------------------------------------------------------------
# 1. Strip wrapper block from profile
# ---------------------------------------------------------------------------

if (-not (Test-Path $ProfilePath)) {
    Info "Profile not found at $ProfilePath — nothing to strip."
} else {
    $lines = Get-Content $ProfilePath -ErrorAction Stop

    # Count markers — paired BEGIN/END is required, refuse to touch on orphans.
    $beginCount = ($lines | Where-Object { $_ -match [regex]::Escape($MarkerBegin) }).Count
    $endCount   = ($lines | Where-Object { $_ -match [regex]::Escape($MarkerEnd) }).Count

    if ($beginCount -ne $endCount) {
        Fail "orphaned markers in profile (BEGIN=$beginCount, END=$endCount) — fix manually before retrying."
    }

    if ($beginCount -eq 0) {
        Info "No agent-workspace block found in $ProfilePath — nothing to strip."
    } else {
        $newLines = New-Object 'System.Collections.Generic.List[string]'
        $inBlock = $false
        $stripped = 0
        foreach ($line in $lines) {
            if ($line -match [regex]::Escape($MarkerBegin)) {
                if ($inBlock) {
                    Fail "nested BEGIN marker in profile — manual fixup needed."
                }
                $inBlock = $true
                continue
            }
            if ($line -match [regex]::Escape($MarkerEnd)) {
                if (-not $inBlock) {
                    Fail "END marker without matching BEGIN — manual fixup needed."
                }
                $inBlock = $false
                $stripped++
                continue
            }
            if (-not $inBlock) {
                $newLines.Add($line)
            }
        }
        if ($inBlock) {
            Fail "BEGIN marker without matching END — manual fixup needed."
        }

        # Trim trailing blank lines (don't leave a growing tail every uninstall).
        while ($newLines.Count -gt 0 -and [string]::IsNullOrWhiteSpace($newLines[$newLines.Count - 1])) {
            $newLines.RemoveAt($newLines.Count - 1)
        }

        if ($newLines.Count -eq 0) {
            # Profile was wrapper-only — clear it but keep the file so the user's
            # `$PROFILE` reference still resolves.
            Set-Content -Path $ProfilePath -Value '' -NoNewline -Encoding UTF8
        } else {
            Set-Content -Path $ProfilePath -Value $newLines -Encoding UTF8
        }
        Info "Removed $stripped agent-workspace block(s) from $ProfilePath"
    }
}

# ---------------------------------------------------------------------------
# 2. Remove install dir from User PATH
# ---------------------------------------------------------------------------

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if ($userPath) {
    $parts = $userPath -split ';' | Where-Object { $_ -ne '' }
    # Case-insensitive comparison (Windows paths) using -ne.
    $filtered = $parts | Where-Object { $_ -ne $InstallDir }
    if ($filtered.Count -ne $parts.Count) {
        $newPath = ($filtered -join ';')
        [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
        Info "Removed $InstallDir from User PATH"
    } else {
        Info "$InstallDir was not in User PATH — nothing to remove."
    }
} else {
    Info "User PATH is empty — nothing to remove."
}

# ---------------------------------------------------------------------------
# 3. Hints for manual cleanup we deliberately skip
# ---------------------------------------------------------------------------

Write-Host ''
if (Test-Path $BaseDir) {
    Info "Install dir kept: $BaseDir"
    Info "  (may contain channel marker + cached worktree state — delete manually if desired)"
    Info "  Remove-Item -Recurse -Force '$BaseDir'"
}
Info "If installed via npm:  npm uninstall -g '@zelanton/agent-workspace'"
Write-Host ''
Info "Done. Open a new PowerShell window to drop the wrapper from the current shell."
