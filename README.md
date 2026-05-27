# agent-workspace

[![npm version](https://img.shields.io/npm/v/agent-workspace)](https://www.npmjs.com/package/agent-workspace)

A Git worktree workflow tool for AI coding agents. Enables parallel development with isolated environments.

![Cover](cover.png)

## Why

AI coding agents work best with isolated environments:

- **Parallel execution**: Run multiple agents simultaneously without interference
- **Clean separation**: Each feature gets its own working directory
- **Snap mode**: "Use and discard" workflow — create worktree, run agent, merge, cleanup

### Fork notes

This is a fork of [`nekocode/agent-worktree`](https://github.com/nekocode/agent-worktree). Highlights of the fork:

- **Native [Jujutsu (`jj`)](https://jj-vcs.github.io/jj/) backend** alongside git. Both backends are feature-complete for `ws`'s happy-path workflows (new / ls / cd / rm / clean / merge / sync / status / mv). Lives behind `src/vcs/` with `GitBackend` and `JjBackend` as separate impls of a common trait. Colocated repos (both `.git/` and `.jj/` present) default to jj — override with `--vcs=git` or `[general] vcs = "git"` in `.agent-workspace.toml`. A few git-shaped operations have no clean jj analogue and surface `Error::Unsupported` with a hint: `ws mv` (use `ws rm` + `ws new` instead) and `ws sync --abort`/`--continue` (jj records conflicts in commits — resolve files and re-run). See [`AGENTS.md`](AGENTS.md) → "VCS backend compatibility" for the full semantic-delta table.
- **Terminal-tab integration** for `ws new` and `ws cd` on Windows Terminal, iTerm2, and GNOME Terminal — the creation / navigation flow opens a new tab titled with the branch and runs there; the originating shell stays put. `ws cd <branch>` to your current worktree is a no-op (no duplicate tabs unless `--in-new-tab` forces it). Auto-enabled when supported; disable with `--no-tab` or `[ui] open_in_new_tab = false`.
- **Copy-on-Write worktree creation** on filesystems that support block cloning (Windows ReFS / DevDrive, Linux Btrfs / XFS, macOS APFS). `ws new` becomes near-instant on large monorepos and the new worktree initially occupies only its diff on disk. Supported on both backends: git uses `worktree add --no-checkout` + reflink; jj uses `workspace add --sparse-patterns empty` + reflink + sparse-set restore. Colocated repos with `--vcs=git` bracket the git ops with `jj git import` to keep jj in sync. Auto-enabled when the source repo and `$AGENT_WORKSPACE_DIR` are on the same reflink-capable volume; falls back silently otherwise. Disable with `--no-cow` or `[create] use_cow = false`.

## Install

### Quick install (no Node.js required)

**macOS / Linux:**

```bash
curl -fsSL https://github.com/ZelAnton/agent-workspace/releases/latest/download/install.sh | sh
```

**Windows (PowerShell):**

```powershell
iwr https://github.com/ZelAnton/agent-workspace/releases/latest/download/install.ps1 -UseBasicParsing | iex
```

Installs `ws` to `~/.agent-workspace/bin`, adds it to your PATH, and runs `ws setup` for shell integration.

### Via npm

```bash
npm install -g @zelanton/agent-workspace
```

### Update

```bash
ws update
```

`ws update` detects how `ws` was installed (`~/.agent-workspace/install_channel`):
- **Shell installer** — downloads the latest release from GitHub and atomically replaces itself (uses [`self_replace`](https://crates.io/crates/self-replace) for the Windows .exe rename-trick).
- **npm** — re-runs `npm install -g /agent-workspace@latest`.

Shell integration is installed automatically by both channels. To reinstall manually:

```bash
ws setup
```

Supported shells: bash, zsh, fish, PowerShell

### Uninstall

Remove the shell wrapper installed by `ws setup`:

```bash
ws uninstall
```

If the `ws` binary is missing or broken, run the standalone script instead — it works without a functioning binary:

```bash
# macOS / Linux
curl -fsSL https://github.com/ZelAnton/agent-workspace/releases/latest/download/uninstall.sh | sh

# Windows (PowerShell)
iwr https://github.com/ZelAnton/agent-workspace/releases/latest/download/uninstall.ps1 -UseBasicParsing | iex
```

The uninstall step does **not** delete `~/.agent-workspace/` (binary, channel marker, cached config) or any worktrees you created — both scripts print follow-up commands for those. If installed via npm, also run `npm uninstall -g @zelanton/agent-workspace`.

## Quick Start

```bash
# Create a worktree and enter it
ws new feature-x

# ... develop, commit ...

# Merge back (merges to the branch you were on when creating)
ws merge            # keeps worktree
ws merge -d         # deletes worktree after merge
```

Other useful commands:

```bash
ws ls              # List all worktrees (with BASE branch info)
ws cd feature-y    # Switch to another worktree
ws cd              # Return to main repository
```

## Snap Mode

One-liner for AI agent workflows:

```bash
ws new -s claude           # Random branch name
ws new fix-bug -s codex    # Specified branch name
ws new -s "claude --dangerously-skip-permissions"  # Command with arguments
```

> **Argument quoting** — `-s` takes a single token. Use quotes whenever the
> command has flags or arguments (`-s "agent --flag"`), otherwise the shell
> hands the trailing args to `ws new` instead.
>
> **Nested snap is refused** — running `ws new -s` from inside an existing
> worktree exits with an error. Run `ws cd` to return to the main repo first.

Flow: Create worktree → Enter → Run agent → [Develop] → Agent exits → Check changes → Merge → Cleanup

After the agent exits — whether normally or with a crash / Ctrl+C — `ws`
checks the worktree state:

- **No changes**: Worktree cleaned up automatically
- **Only commits** (nothing uncommitted):
  ```
  [m] Merge into base branch
  [q] Exit snap mode
  ```
- **Uncommitted changes**:
  ```
  [r] Reopen agent (let agent commit)
  [q] Exit snap mode (commit manually)
  ```

> **base_branch must still exist** — if the worktree's base branch was
> deleted while the agent ran, `[m]` errors out. Use `ws merge --into <branch>`
> to pick an explicit target instead.

## Commands

### Worktree Management

| Command | Description |
|---------|-------------|
| `ws new [branch]` | Create worktree from current branch (random name if omitted) |
| `ws new --base <branch>` | Create from specific base branch (default: current branch) |
| `ws new -s <cmd>` | Create + snap mode |
| `ws cd [branch]` | Switch to worktree (omit branch to return to main repo) |
| `ws ls` | List worktrees |
| `ws ls -l` | Show full path for each worktree |
| `ws mv <old> <new>` | Rename worktree (use `.` for current) |
| `ws rm <branch>` | Remove worktree (use `.` for current) |
| `ws rm -f <branch>` | Force remove with uncommitted changes |
| `ws clean` | Remove worktrees with no diff from their base branch (falls back to trunk); dirty worktrees are skipped |
| `ws clean --dry-run` | Preview which worktrees would be cleaned |

### Workflow

| Command | Description |
|---------|-------------|
| `ws merge` | Merge to base branch (falls back to trunk, default: squash) |
| `ws merge -s <strategy>` | Merge with strategy (squash/merge) |
| `ws merge --into <branch>` | Merge to specific branch (overrides base) |
| `ws merge -d` | Delete worktree after merge (default: keep) |
| `ws merge -H` | Skip pre-merge hooks |
| `ws sync` | Sync from base branch (falls back to trunk, default: rebase) |
| `ws sync -s <strategy>` | Sync with strategy (rebase/merge) |
| `ws sync --from <branch>` | Sync from specific branch (overrides base) |
| `ws sync --continue` | Continue after resolving conflicts |
| `ws sync --abort` | Abort sync |

### Info

| Command | Description |
|---------|-------------|
| `ws status` | Show current worktree info (also reports in-progress `ws sync` rebase/merge with recovery hints) |
| `ws update` | Update to the latest version |

### Configuration

| Command | Description |
|---------|-------------|
| `ws setup` | Install shell integration (auto-detect) |
| `ws setup --shell zsh` | Install for specific shell |
| `ws uninstall` | Remove shell integration (inverse of `setup`) |
| `ws init` | Initialize project config |
| `ws init --trunk <branch>` | Initialize with specific trunk branch |
| `ws init --merge-strategy <strategy>` | Set default merge strategy (squash/merge) |
| `ws init --sync-strategy <strategy>` | Set default sync strategy (rebase/merge) |
| `ws init --copy-files <pattern>` | Files to copy to new worktrees (repeatable) |

## Configuration

### Base Directory

Defaults to `~/.agent-workspace`. Override via `AGENT_WORKSPACE_DIR`:

```bash
export AGENT_WORKSPACE_DIR=/data/agent-workspace
```

### Global Config `$AGENT_WORKSPACE_DIR/config.toml` (default `~/.agent-workspace/config.toml`)

```toml
[general]
merge_strategy = "squash"  # squash | merge
sync_strategy = "rebase"   # rebase | merge
copy_files = [".env", ".env.*"]  # Gitignore-style patterns for files to copy

[hooks]
post_create = ["pnpm install"]
pre_merge = ["pnpm test", "pnpm lint"]
post_merge = []
```

> **`copy_files` constraints** — patterns are gitignore-style and must stay
> inside the repo: leading `/` (absolute paths) and `..` traversal are
> rejected. Symlinks are not followed.
>
> **Hook trust boundary** — hooks run via `sh -c` (or `cmd /C` on Windows)
> with no sandboxing or timeout. Treat `.agent-workspace.toml` like any
> committed shell script: only run repos whose hooks you would `bash` directly.
>
> **Hook CWD** — `pre_merge` and `post_merge` always run with the worktree
> root as the working directory. `post_create` runs in the new worktree.

### Project Config `.agent-workspace.toml`

Project config overrides global. `trunk` is project-only; other fields are merged.

```toml
[general]
trunk = "main"  # Trunk branch (auto-detected if omitted)
merge_strategy = "merge"  # Override global merge strategy
sync_strategy = "merge"   # Override global sync strategy
copy_files = ["*.secret.*"]  # Appended to global copy_files

[hooks]
post_create = ["pnpm install"]  # Replaces global hooks if set
```

## Storage Layout

```
~/.agent-workspace/
├── bin/                           # ws binary (shell installer)
├── config.toml                    # Global config
├── install_channel                # 'npm' or 'shell'
└── {project}-{hash}/              # one dir per source repo
    ├── {branch_name}.toml         # Worktree metadata
    ├── {branch_name}/             # Worktree directory
    └── ...
```

> **v0.13.6 layout change** — earlier versions added an extra `workspaces/`
> directory between the base and the per-project dirs
> (`~/.agent-workspace/workspaces/{project}-{hash}/{branch}/`). That
> intermediate level is gone now. Existing `workspaces/` directories from
> pre-0.13.6 installs are NOT migrated automatically — the worktrees
> inside have absolute paths baked into git's gitlink / jj's workspace
> registration that would break on relocation. `ws` prints a one-line
> migration nudge on startup whenever it spots a non-empty legacy
> `workspaces/` dir. Run `ws rm <branch>` from inside each old worktree
> to clean up, or `rm -rf ~/.agent-workspace/workspaces` if you don't
> need them.

## License

MIT
