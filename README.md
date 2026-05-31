# agent-workspace

[![npm version](https://img.shields.io/npm/v/@zelanton/agent-workspace)](https://www.npmjs.com/package/@zelanton/agent-workspace)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](#license)
[![Backends: git + jj](https://img.shields.io/badge/backends-git%20%2B%20jj-orange.svg)](#vcs-backend-selection)

A fast Git/Jujutsu **worktree workflow tool for AI coding agents** (CLI: `ws`).
Spin up isolated worktrees in milliseconds, run an agent in each, then merge and
clean up — all from one short command. Distributed as a single static binary.

![Cover](cover.png)

## Why

AI coding agents work best with isolated environments — one agent per worktree,
no stepping on each other's files, no half-finished branches polluting your main
checkout:

- **Parallel execution** — run multiple agents simultaneously without interference; each gets its own working directory and branch.
- **Clean separation** — a feature's work-in-progress never touches your main checkout. Throw the worktree away and your repo is pristine.
- **Snap mode** — a "use and discard" loop: create a worktree, run an agent in it, and on exit `ws` walks you through merge-or-keep and tidies up.
- **Near-instant creation** — Copy-on-Write (reflink) cloning makes `ws new` near-instant even on multi-gigabyte monorepos, using almost no extra disk.
- **Native git *and* [Jujutsu (`jj`)](https://jj-vcs.github.io/jj/)** — both backends are first-class; colocated repos default to jj.
- **Terminal-tab aware** — on Windows Terminal / iTerm2 / GNOME Terminal, `ws new` and `ws cd` open the worktree in a fresh tab and leave your current shell put.

### At a glance

```bash
ws new fix-auth -s claude   # new worktree + branch, run Claude in it, merge on exit
ws ls                       # see every worktree and its base branch
ws cd fix-auth              # jump into one (new tab if your terminal supports it)
ws merge -d                 # squash-merge back to the base branch and delete the worktree
ws clean                    # garbage-collect worktrees already merged
```

### Requirements

- **git** ≥ 2.x (always required, including for colocated repos). Optionally **[`jj`](https://jj-vcs.github.io/jj/)** for the Jujutsu backend.
- A POSIX shell (bash / zsh / fish) or PowerShell — needed for the `cd`-changing commands (`ws setup` installs the integration).
- Copy-on-Write is opportunistic: a reflink-capable filesystem (Windows ReFS / DevDrive, Linux Btrfs / XFS, macOS APFS) enables it; everything else falls back to a plain copy automatically.

### Fork notes

This is a fork of [`nekocode/agent-worktree`](https://github.com/nekocode/agent-worktree). Highlights of the fork:

- **Native [Jujutsu (`jj`)](https://jj-vcs.github.io/jj/) backend** alongside git. Both backends are feature-complete for `ws`'s happy-path workflows (new / ls / cd / rm / clean / merge / sync / status / mv). Lives behind `src/vcs/` with `GitBackend` and `JjBackend` as separate impls of a common trait. Colocated repos (both `.git/` and `.jj/` present) default to jj — override with `--vcs=git` or `[general] vcs = "git"` in `.workspace.toml`. A few git-shaped operations have no clean jj analogue and surface `Error::Unsupported` with a hint: `ws mv` (use `ws rm` + `ws new` instead) and `ws sync --abort`/`--continue` (jj records conflicts in commits — resolve files and re-run). See [`AGENTS.md`](AGENTS.md) → "VCS backend compatibility" for the full semantic-delta table.
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
- **npm** — re-runs `npm install -g @zelanton/agent-workspace@latest`.

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
| `ws new --no-cow` | Skip Copy-on-Write cloning for this creation |
| `ws new --no-tab` / `--in-new-tab` | Override terminal-tab behavior for this creation |
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
| `ws repo-info` | Show the cached per-repo metadata (file count, size, origin URL, GitHub slug) |
| `ws repo-info --refresh` | Force-refresh that cache (otherwise auto-refreshed every 30 days) |
| `ws update` | Update to the latest version |

### Per-repo settings

| Command | Description |
|---------|-------------|
| `ws config list` | List supported per-repo keys with descriptions |
| `ws config get <key>` | Read a setting (e.g. `workspace.alias`, `workspace.use_path_hash`) |
| `ws config set <key> <value>` | Write a setting to the repo-level config |
| `ws config unset <key>` | Remove a setting |
| `ws exclude` | Open the TUI tree picker for `[copy] exclude` patterns |
| `ws exclude <pattern>...` | Add CoW exclude patterns (e.g. `target node_modules '**/*.iso'`) |
| `ws exclude --list` | Show current exclude patterns |
| `ws exclude --remove <pattern>` | Drop a pattern |
| `ws exclude --clear` | Remove all patterns |

> Both `ws config` and `ws exclude` write the local repo-level `.workspace.toml`
> and keep it out of git automatically (via `.git/info/exclude`).

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
> with no sandboxing or timeout. Treat `.workspace.toml` like any
> shell script: only run repos whose hooks you would `bash` directly.
>
> **Hook CWD** — `pre_merge` and `post_merge` always run with the worktree
> root as the working directory. `post_create` runs in the new worktree.

### Project Config `.workspace.toml`

Repo/worktree config lives in `.workspace.toml` — a **local, per-machine** file that `ws` keeps out of git by adding it to the repo's local exclude file (`.git/info/exclude`), so there's nothing to commit and your working tree stays clean (existing repos with a committed `.agent-workspace.toml` keep working: it's read as a fallback when no `.workspace.toml` is present). It layers in three tiers, later overriding earlier:

1. **global** — `~/.agent-workspace/config.toml`
2. **repo-level** — `.workspace.toml` at the main repo root
3. **worktree-level** — `.workspace.toml` at an individual worktree's root (overrides the repo-level file for commands run inside that worktree)

`trunk` is project-only; other fields are merged (`copy_files` / `copy.exclude` append, `hooks` replace per-phase when set, the rest are overrides). `ws config` and `ws exclude` write the repo-level file; worktree-level files are hand-authored.

```toml
[general]
trunk = "main"  # Trunk branch (auto-detected if omitted)
vcs = "git"     # Force a backend for this repo: "git" | "jj" (default: auto-detect)
merge_strategy = "merge"  # Override global merge strategy
sync_strategy = "merge"   # Override global sync strategy
copy_files = ["*.secret.*"]  # Appended to global copy_files

[hooks]
post_create = ["pnpm install"]  # Replaces global hooks if set

[copy]
# Gitignore-style patterns the Copy-on-Write path SKIPS when cloning the source
# repo into a new worktree. Anchor with a leading `/` for top-level only.
exclude = ["/target", "/node_modules", "**/*.iso"]

[create]
use_cow = true          # Copy-on-Write worktree creation (default: true when supported)

[ui]
open_in_new_tab = true  # Open ws new / ws cd in a new terminal tab (default: true)

[workspace]
alias = "myrepo"        # Override the workspace directory name (default: repo basename)
use_path_hash = false   # Append a `-<hash>` suffix to disambiguate same-named repos
```

## VCS backend selection

`ws` speaks both git and Jujutsu. The backend is resolved once per invocation,
in this order:

1. `--vcs <auto|git|jj>` global flag
2. `[general] vcs` in the repo-level `.workspace.toml`
3. `[general] vcs` in the global config
4. auto-detection from `.git/` / `.jj/` presence

**Colocated repos (both `.git/` and `.jj/`) default to jj** — you installed jj
for a reason, so `ws` respects it. Force git with `--vcs=git` or `vcs = "git"`.
A handful of git-shaped operations have no clean jj analogue and return a clear
`Error::Unsupported` with a hint rather than guessing:

| Operation | jj guidance |
|-----------|-------------|
| `ws mv` | use `ws rm` + `ws new` |
| `ws sync --abort` / `--continue` | jj records conflicts in commits — resolve the files and re-run |

See [`AGENTS.md`](AGENTS.md) → "VCS backend compatibility" for the full
semantic-delta table (no staging area, bookmarks vs branches, atomic operations).

## How shell integration works

A child process can't change its parent shell's directory, so the commands that
move you (`ws cd`, `ws new`, `ws rm`, `ws mv`, `ws merge -d`, `ws clean`) can't
`cd` you directly. Instead the binary writes the destination to a temp file and
a tiny shell wrapper (installed by `ws setup`) reads it and performs the `cd`.

Consequences worth knowing:

- Run `ws setup` once per shell. Without it, the `cd`-changing commands still do
  their work but can't move your shell — and `ws` refuses to delete or move the
  **current** worktree without integration (it would strand you in a deleted
  directory). `cd` to the main repo first, or install the wrapper.
- The wrapper is bracketed by `# === agent-workspace BEGIN/END ===` markers in
  your rc file; `ws uninstall` removes exactly that block and nothing else.
- On terminals with tab support, `ws new` / `ws cd` open a new tab instead, and
  the originating shell stays exactly where it was.

## Troubleshooting

- **`ws cd` doesn't change my directory** — shell integration isn't active in
  this shell. Run `ws setup`, then open a new shell (or `source` your rc file).
- **"Refusing to remove/move the current worktree without shell integration"** —
  same cause. Run `ws setup`, or `ws cd` back to the main repo and retry.
- **`ws new` opened a new tab I didn't want** — pass `--no-tab`, or set
  `[ui] open_in_new_tab = false` in your config.
- **Creation isn't using Copy-on-Write** — the source repo and
  `$AGENT_WORKSPACE_DIR` must be on the *same* reflink-capable volume. `ws` probes
  with a real sentinel reflink and falls back to a plain copy silently otherwise.
  Force a plain copy with `--no-cow`.
- **Merge says "Base branch '…' no longer exists"** — the worktree's base branch
  was deleted. Pick an explicit target: `ws merge --into <branch>`.

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

[MIT](LICENSE) © Anton Zhelezniakou. Fork of [`nekocode/agent-worktree`](https://github.com/nekocode/agent-worktree).
