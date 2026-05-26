# v0.13.0 — Binary rename `wt` → `ws` (breaking)

This release renames the CLI binary from `wt` to **`ws`** ("workspace") to
permanently escape the collision with Microsoft Windows Terminal's `wt.exe`.
The old name was an App Execution Alias placed early in user PATH and kept
intercepting our commands no matter how aggressively the PowerShell wrapper
filtered for our binary. The new name `ws` is free across Windows, macOS,
and Linux — verified empty in `Get-Command`/`which`/`type -P`.

The npm package keeps its name (`@zelanton/agent-workspace`); only the
binary that lands on your PATH changes. All subcommands keep their names —
`wt new feat-x` becomes `ws new feat-x`, etc.

## Migrating from v0.12.x

```bash
# 1. Remove the old shell integration (handles either functional or broken wt):
ws uninstall                # if ws is already on PATH (npm picks it up automatically)
# or, if the old wt is still around but broken:
curl -fsSL https://github.com/ZelAnton/agent-workspace/releases/latest/download/uninstall.sh | sh   # macOS / Linux
iwr https://github.com/ZelAnton/agent-workspace/releases/latest/download/uninstall.ps1 -UseBasicParsing | iex   # Windows

# 2. Delete the orphan binary (the new installer writes ws but doesn't remove old wt):
rm ~/.agent-workspace/bin/wt          # macOS / Linux
Remove-Item ~/.agent-workspace/bin/wt.exe   # Windows

# 3. Reinstall — picks up ws:
npm install -g @zelanton/agent-workspace
# or
curl -fsSL https://github.com/ZelAnton/agent-workspace/releases/latest/download/install.sh | sh
iwr https://github.com/ZelAnton/agent-workspace/releases/latest/download/install.ps1 -UseBasicParsing | iex

# 4. Re-run setup with the new binary:
ws setup
```

After step 4, restart your shell (or `. $PROFILE` / `source ~/.bashrc`) and
`ws --version` should print `ws 0.13.0`. The PowerShell `Get-AgentWorkspaceBinary`
helper that used to filter out Microsoft's `wt.exe` is gone (the rename made
it structurally impossible to collide).

## What changed under the hood

- **Cargo `[[bin]]` name** `wt` → `ws`; clap `#[command(name = ...)]` matches.
- **All 4 shell wrappers** (bash/zsh/fish/PowerShell) redefine the function
  as `ws`. The PowerShell wrapper drops the WindowsApps PATH filter — no
  longer needed.
- **Fish completions file** moved from `~/.config/fish/completions/wt.fish`
  to `~/.config/fish/completions/ws.fish`. Old file is cleaned up by `ws uninstall`.
- **Recursion-guard env var** `WT_SPAWNED_IN_TAB` → `WS_SPAWNED_IN_TAB`
  (the previous prefix was misleadingly similar to Microsoft's `WT_SESSION`).
- **Self-update** now extracts `ws`/`ws.exe` from release archives. Users on
  v0.12.x running `wt update` against this release will get a clear error
  about the missing `wt` binary in the archive — follow the migration steps
  above to reinstall.
- **CI workflow** matrix updated to package `ws`/`ws.exe`. Release assets
  remain `agent-workspace-<version>-<platform>.tar.gz` (package name
  unchanged), with `ws`/`ws.exe` at the top level of each tarball.
- **No backward-compat shim**: there is no `wt` alias installed. The whole
  point of the rename is to escape the Microsoft collision; aliasing it back
  would defeat that. Train your fingers to type `ws` instead.

## Microsoft's `wt.exe` is unaffected

We still invoke `wt.exe new-tab` to spawn a new Windows Terminal tab when
the user runs `ws new` or `ws cd` with terminal-tab integration enabled.
That's Microsoft's CLI — not ours. The rename only changes our own binary.
