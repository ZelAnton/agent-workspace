# AGENTS.md

This file provides guidance to AI coding agents when working with code in this repository.

## Project

`agent-workspace` (CLI binary: `ws`) that manages git worktrees for AI coding agents. Distributed as a single static binary, packaged for npm with per-platform subpackages under `npm/`.

`ARCHITECTURE.md` (Chinese) is the canonical design doc. `FILE_TREE.local.md` (gitignored) is the single source of truth for per-file responsibilities — read it if it exists before doing structural work.

## Build, test, run

```bash
cargo build                  # debug binary at target/debug/ws
cargo build --release        # release binary
cargo test                   # all unit + integration tests
cargo test --test cmd_merge  # single integration test file (one per command in tests/)
cargo test <name>            # run tests matching a substring
cargo clippy --all-targets   # lint
cargo fmt                    # format

./scripts/build-npm.sh current        # build + stage binary into npm/agent-workspace-<platform>/bin
./scripts/build-npm.sh all            # cross-compile all four target triples (needs `cross`)
```

Integration tests in `tests/cmd_*.rs` shell out to `target/debug/ws` — they assume `cargo build` has run. `tests/common/mod.rs` provides `setup_git_repo`, `setup_worktree_test_env`, and `create_path_file` helpers; use them rather than rolling your own git fixtures.

## Architectural invariants

These aren't obvious from reading individual files — they constrain how features must be built.

### Shell integration is load-bearing for any `cwd`-changing command

`ws cd`, `ws new`, `ws rm`, `ws mv`, `ws merge -d`, `ws clean` change the user's shell directory. The binary can't do that directly; instead it writes the target path to a temp file passed via the hidden global `--path-file <FILE>` flag, and the shell wrapper (installed by `ws setup`, sources defined in `src/shell/mod.rs`) reads that file and `cd`s. Helpers: `cli::write_path_file` (single line) and `cli::write_path_file_lines` (multi-line, used by snap). Any new command that wants to move the user must accept `path_file: Option<&Path>` and write to it — `ws cd` deliberately errors out when `--path-file` is missing rather than silently no-op'ing.

The `# === agent-workspace BEGIN/END ===` markers around the wrapper are a wire contract with `ws uninstall` (and the standalone `uninstall.ps1` / `uninstall.sh` scripts). `src/shell/mod.rs::remove_wrapper` refuses to touch a file with unpaired markers — silently truncating after an orphan would wipe unrelated config (PATH exports, aliases). Any future wrapper change must keep the markers literal and balanced, or both `ws setup` (re-install path) and `ws uninstall` will refuse the file. `uninstall()` is the inverse of `install()` and lives next to it; both call into the same `remove_wrapper` helper so the safety net is single-sourced.

### Snap mode exit codes are a wire protocol with the shell wrapper

`src/cli/commands/snap/resume.rs` exports `EXIT_DONE = 0`, `EXIT_REOPEN = 2`, `EXIT_PRESERVE = 3`. The bash/zsh/fish/PowerShell wrapper scripts in `src/shell/mod.rs` switch on these to either loop the agent, drop the user into the worktree, or `cd` back to the main repo. **Changing these constants requires updating every wrapper template in lockstep.** The same exit codes are interpreted by the spawned-tab script generated in `src/terminal/script.rs` when `ws new --snap` opens a new terminal tab — keep that script's `case`/`if` block in sync too. Nested snap (`ws new -s` from inside a worktree) is refused for the same reason — two snap loops in one parent shell break the cwd tracking.

The path-file carries two lines for snap (`<worktree-path>\n<snap-command>`); the wrappers read line 1 as the path and the **single** command line as the agent command. The snap command must be a single line — a command containing a literal newline is parsed inconsistently across shells (bash/zsh/fish take the last line, PowerShell the first) and gets truncated. Use a single shell string (`ws new -s "a && b"`), not an embedded newline.

### Terminal-tab integration changes the spawn point for `ws new` and `ws cd`

When `ws new <branch>` or `ws cd <branch>` runs inside a supported terminal (Windows Terminal, iTerm2, GNOME Terminal — detection via `WT_SESSION`, `TERM_PROGRAM=iTerm.app`, `GNOME_TERMINAL_SERVICE`/`GNOME_TERMINAL_SCREEN`) and `[ui] open_in_new_tab` is enabled (default), the flow does NOT run in the originating shell. Instead `src/terminal/` opens a fresh tab titled with the branch name. The originating shell prints `Opened in new tab: <branch>` and exits cleanly without writing the `--path-file` (so its shell wrapper stays put).

**Two spawn modes** (see `src/terminal/mod.rs::TabMode`):

- **`WtNew`** (used by `ws new`): the spawned tab re-invokes `ws new <args>` with `WS_SPAWNED_IN_TAB=1`, runs creation inside the new tab, optionally enters the snap-resume loop. Script body is substantial — includes the `--path-file` dance and snap exit-code handling.
- **`OpenAtCwd`** (used by `ws cd`): the spawned tab opens at the target worktree's directory via the terminal's native cwd flag (`wt.exe new-tab -d`, `gnome-terminal --working-directory`, iTerm2's AppleScript). The script body is minimal — only sets the recursion-guard env and emits OSC 0 for the title.

**Recursion guard**: `WS_SPAWNED_IN_TAB=1` is critical for both modes. Without it, every re-entry would open another tab. The shared precedence resolver `terminal::should_open_in_new_tab` checks this guard before consulting flags/config.

**`ws cd` specifics**:
- **Validate before spawn**: non-existent worktree errors in originating shell, no tab opens. (Inverted from `ws new`, which validates AFTER the spawn since creation happens in the tab.)
- **Same-target short-circuit**: `ws cd <branch>` when already in that worktree skips the spawn (no duplicate tabs). `--in-new-tab` overrides.
- **No-arg `ws cd`** (return to main repo): always spawns when integration enabled — consistent UX. The tab title falls back to the repo name.

On Windows the spawned tab MUST run PowerShell (per design); the binary spawn uses `wt.exe new-tab pwsh -NoExit -Command ...` and locates Microsoft Windows Terminal's `wt.exe` via PATH lookup. Pre-v0.13.0 our own binary was *also* called `wt.exe`, requiring an elaborate skip-our-own-binary PATH walk in `src/terminal/windows_terminal.rs::locate_wt_binary`; the v0.13.0 rename to `ws.exe` resolved that collision and the function simplified to a straight PATH walk.

User overrides: `--in-new-tab` / `--no-tab` flags on `ws new` and `ws cd`; `[ui] open_in_new_tab = false` (project or global) to disable by default.

### Worktree creation uses Copy-on-Write when the filesystem supports it

On filesystems with block cloning (Windows ReFS / DevDrive, Linux Btrfs / XFS, macOS APFS), `ws new` creates worktrees via reflink instead of git's standard checkout. The result is near-instant creation and minimal disk usage even for large monorepos — only the diff occupies physical space until either side mutates.

**Git workflow** (`src/vcs/git/worktree.rs::create_worktree_cow`):
1. Capture `current_branch()` and `has_uncommitted_changes()`.
2. If dirty, `git stash push -u -m "ws-cow-create-<pid>"`.
3. If `current_branch != base`, `git checkout <base>`.
4. `git worktree add --no-checkout <path> <branch>` — creates only the `.git` gitlink file.
5. `cow::try_clone_dir_except(repo_root, path, &[".git"])` — reflink-copies every file/dir except `.git/`. Uses `reflink-copy` crate (single API for ReFS / Btrfs / XFS / APFS; per-file fallback to plain copy when reflink is rejected).
6. Restore source repo: `git checkout <orig_branch>` then `git stash pop`. Both wrapped in error-tolerant warnings — stash pop conflict surfaces a clear message directing the user to `git stash list`.

**Rollback**: if step 4 or 5 fails, the partial worktree directory is removed and `git worktree prune` clears git's registry. Step 6 still runs to restore source state. The original error then propagates.

**CoW eligibility**: `cow::can_clone(src_dir, dst_parent)` does a real sentinel reflink probe at the destination's parent dir (not just a volume-serial check — NTFS shares serials but doesn't support reflinks). Cached implicitly by the per-call probe.

**Skip when CoW used**: the `copy_files` (`[general] copy_files`) step in `src/cli/commands/lifecycle/new.rs::run` is **redundant** after a successful CoW clone — every file from source is already in the new worktree. The caller switches on `CreateOutcome::CowCloned` vs `CreateOutcome::Plain` returned from `vcs::create_worktree` to decide.

**Jj backend** (`src/vcs/jj/worktree.rs::create_worktree_cow`): mirrors the git workflow via jj's `--sparse-patterns empty` analogue of `--no-checkout`:
1. Capture op id (precise rollback) and current `@` change-id (source restore).
2. `jj edit <base>` to move source workspace's working-copy to base's tree.
3. `jj workspace add --name <derived> -r <base> --sparse-patterns empty <path>` — creates the workspace + empty change above base; no files materialised.
4. `cow::try_clone_dir_except(repo_root, path, &[".jj", ".git"])` — reflink-copies source's files (which now match base) into the new workspace. Both `.jj/` and `.git/` are excluded (the latter for colocated repos).
5. `jj sparse set --pattern .` + `jj status` inside the new workspace — restores sparse-pattern set to "all files" and triggers a snapshot so jj's view of `@` matches the on-disk tree.
6. `jj bookmark create <branch> -r <derived>@`.
7. `jj edit <orig_change_id>` to restore source workspace's `@`.

Rollback on internal failure: `jj op restore <pre_op>` + `fs::remove_dir_all(path)`.

**Colocated git-force bracketing** (`src/vcs/git/worktree.rs::create_worktree_cow`): when the source repo has `.jj/` alongside `.git/` and the user forced git backend via `--vcs=git`, raw git ops (stash, checkout, worktree add) would desync jj's view. The CoW flow brackets the entire git-side work with `jj git import` calls (before and after) so jj's bookmarks/refs catch up. Calls are best-effort — silently skipped if jj isn't installed.

**User overrides**: `--no-cow` flag on `ws new`; `[create] use_cow = false` (project or global) to disable by default. Sets the `WT_DISABLE_COW` env var which both backends' CoW dispatchers check.

**Concurrency**: the CoW flow mutates the *shared* source repo (git stash + checkout base; jj `jj edit @`), so two `ws new` against the same repo can't run it simultaneously. Both dispatchers hold a per-repo `cow::CowLock` (a temp-dir lock file keyed by the repo-root hash, auto-reclaimed after 30 min if a holder crashes) for the source-mutating window; on contention the loser falls back to the plain path (`git worktree add` / `jj workspace add`), which never touches the source working copy and is concurrency-safe. The lock's failure mode is purely speed — a lock bug degrades to plain, never corrupts.

**Known limitation — git submodules**: `ws` has no submodule support, and CoW makes it visible: `git worktree add --no-checkout` writes the gitlink, but the reflink copies the submodule's *working files*, so the new worktree reports a permanent `M <submodule>` — which makes `ws merge`'s "worktree has uncommitted changes" guard refuse forever. Don't use `ws` worktrees on submodule-bearing repos (or `--no-cow` + manual `git submodule update`).

### Merge is atomic — no continue/abort

`ws merge` records the main repo's current branch, dry-runs the merge with `--squash --no-commit` or `--no-ff --no-commit` (matching the real strategy), and only proceeds if the dry-run is conflict-free. On any failure: `reset_merge` + checkout original branch. There is intentionally no `ws merge --continue/--abort` — the recovery path for conflicts is `ws sync` inside the worktree, then re-run `ws merge`. Don't add intermediate-state handling; preserve the atomic property.

`ws sync`, by contrast, is git-native rebase/merge — its conflicts *do* leave recoverable state, which is why `ws status` detects in-progress rebase/merge and prints `ws sync --continue/--abort` hints.

### Target branch resolution: CLI override > base_branch (if still exists) > trunk

`meta::resolve_target_branch` (pure, in `src/meta/mod.rs`) is the single resolver used by merge/sync/clean/status. `resolve_effective_target` is the I/O wrapper that loads the `{branch}.toml` meta. If the worktree's `base_branch` was deleted, the fallback to trunk for `ws clean` is fine, but `ws merge`/snap-continue refuse rather than silently retargeting (landing commits on the wrong branch is worse than an error). Anything that picks a target must go through this resolver.

### Worktree metadata: format migrated, legacy still readable

Current format: `{wt_dir}/{branch}.toml` with `created_at` + `base_branch`. Legacy format: `{branch}.status.toml` with `trunk` (now an alias for `base_branch`) plus dropped fields (`base_commit`, `snap_command`). `meta::meta_path_with_fallback` and the `RawMeta` deserialization shim handle both — `base_branch` wins when both keys are present. Use `meta::remove_meta` (deletes both names) rather than `fs::remove_file` directly.

### Project config is a 3-tier hierarchy, read from repo + worktree roots

Repo/worktree config lives in `.workspace.toml` — a **local, per-machine** file kept out of git via the repo's local exclude file, not a committed `.gitignore` (legacy committed `.agent-workspace.toml` is still read as a fallback when `.workspace.toml` is absent, via `config::project_config_path_with_fallback`). Three layers merge, later overriding earlier:

1. **global** — `~/.agent-workspace/config.toml`
2. **repo-level** — `<main-repo-root>/.workspace.toml` (fallback `.agent-workspace.toml`), located by walking up from `git rev-parse --git-common-dir` so it applies from the main repo, any worktree, or a subdirectory
3. **worktree-level** — `<current-worktree-root>/.workspace.toml`, located via `git rev-parse --show-toplevel`, applied only when the current worktree root differs from the main repo root (otherwise the main checkout's file would be applied twice)

`Config::load` runs **before** any VCS backend is installed, so both roots are resolved with raw `vcs_runner` calls (`resolve_main_repo_root` / `resolve_current_worktree_root`), not the backend facade — pure-jj repos and the worktree gitlink case are handled there.

`ws config` / `ws exclude` write the **repo-level** file (worktree-level files are load-only / hand-authored in v1). When they create/update a `.workspace.toml`, `config::ensure_workspace_config_ignored` (it gates on the filename, resolves `<git-common-dir>/info/exclude` via `config::local_exclude_file`, then calls the generic `git_exclude::ensure_pattern`) idempotently adds the pattern to the repo's **local exclude file**. This needs no commit and never dirties the working tree (the exclude file lives inside `.git`); because it sits in the *common* git dir, a single entry covers the main repo and every linked worktree, and both git and jj (colocated) honour it. **`ws new` deliberately does NOT touch ignore state** — the entry written by `ws config`/`ws exclude` already covers all worktrees, and there's nothing for `ws new` to add. The CoW copy (`cow::try_clone_dir_except`) additionally **excludes `.workspace.toml`** (added to its `user_patterns`, anchored `/.workspace.toml` — not the `/XD`-only hardcoded list) so a CoW-created worktree never inherits a stale copy of the repo-level file; this matches the plain `git worktree add` path (the file isn't committed) and keeps worktree-level config opt-in. The legacy committed `.agent-workspace.toml` is intentionally still copied.

**Config merge rules:** the repo+worktree layers fold first via `merge_project_layers` (worktree over repo), then that result merges over global. Per field: `copy_files` appends, `copy.exclude` appends+dedups, `hooks` *replace* per-phase when the higher layer is non-empty (not append), `merge_strategy`/`sync_strategy`/`open_in_new_tab`/`use_cow`/`alias`/`use_path_hash` are `Option`-overrides, `trunk` is project-only.

### Hooks run unsandboxed — treat the config file as a committed shell script

Hooks execute via `sh -c` (Unix) / `cmd /C` (Windows) with no sandbox and no timeout. `pre_merge`/`post_merge` always run with the worktree root as cwd; `post_create` runs in the new worktree. `copy_files` patterns are gitignore-style but reject leading `/` and `..` segments and don't follow symlinks (enforced in config parsing).

### Workspace storage layout uses a hash to disambiguate same-named repos

Worktrees live under `$AGENT_WORKSPACE_DIR/workspaces/{repo}-{hash}/` where the hash is derived from the absolute repo path. `git::workspace_id()` is the single source — never construct this path manually. `AGENT_WORKSPACE_DIR` defaults to `~/.agent-workspace`; empty string is treated as unset.

## Module layout

- `src/cli/` — clap definitions + dispatch. Commands grouped: `commands/lifecycle/` (new/rm/clean), `commands/nav/` (cd), `commands/snap/` (resume), `commands/sys/` (setup/uninstall/init/update), plus top-level `ls`, `merge`, `move` (`mv`), `status`, `sync`, `config`, `exclude` (+`exclude_tui` interactive editor), `repo_info`.
- `src/vcs/` — the backend abstraction (`VcsBackend` trait + `GitBackend`/`JjBackend`). **All git/jj CLI interaction lives here** — see the *VCS backend compatibility* section for the full contract. The old top-level `src/git/` is gone; production code calls `crate::vcs::*` free functions.
- `src/cow/` — Copy-on-Write reflink helpers (`can_clone` sentinel probe, `try_clone_dir_except`) used by both backends' `create_worktree_cow`. See the *Worktree creation uses Copy-on-Write* invariant.
- `src/terminal/` — new-tab spawning for Windows Terminal / iTerm2 / GNOME Terminal (`mod.rs` precedence + `TabMode`, per-terminal impls, `script.rs` spawned-tab body). See the *Terminal-tab integration* invariant.
- `src/meta/` — `{branch}.toml` (de)serialization + target resolver (pure functions); `src/repo_meta.rs` — per-repo metadata.
- `src/config/` — global + project loading and merging.
- `src/shell/` — wrapper script templates and rc-file install/uninstall (strict BEGIN/END marker pairing — refuses to touch a file with orphaned markers).
- `src/complete/` — shell-completion script generation.
- `src/process/` `src/prompt/` `src/update/` `src/util/` — hook execution, dialoguer prompts, daily update check, random branch-name generator (~100 adjectives × ~100 nouns, numeric suffix on collision).

## Install channels and `ws update`

Two install channels, distinguished by a marker file at `~/.agent-workspace/install_channel` (content: `npm` or `shell`):

- **npm** — `npm install -g agent-workspace`. Postinstall (`npm/agent-workspace/install.js`) writes `npm` to the marker.
- **shell** — `install.sh` / `install.ps1` at repo root. Downloads a prebuilt archive from GitHub Releases, places `ws` at `~/.agent-workspace/bin/ws`, writes `shell` to the marker.

`ws update` (`src/cli/commands/sys/update.rs`) reads the marker via `update::detect_channel()` and branches:
- `Channel::Npm` → `npm install -g agent-workspace@latest` (legacy path).
- `Channel::Shell` → `update::self_update()` downloads `agent-workspace-<version>-<platform>.tar.gz` from the GitHub release and uses the `self-replace` crate for atomic binary replacement, then re-invokes `ws setup`.

Missing marker defaults to `Channel::Npm` — keeps existing installs predating the marker working.

The version check (`update::check_update` in `src/update/mod.rs`) hits the **GitHub Releases API** for both channels — GitHub is the canonical truth, npm publishes happen after a GitHub release. Requires a non-empty `User-Agent` header (GitHub returns 403 otherwise — `USER_AGENT` const handles this).

Platform key strings (`darwin-arm64`, `linux-x64`, `win32-x64`) must stay consistent across `update::platform_key()`, `npm/agent-workspace/bin/ws.js`, `install.sh`, `install.ps1`, and the CI release archive naming (`.github/workflows/release.yml`). Changing them requires touching all five. **Intel Mac (`darwin-x64`) is intentionally dropped** — the `macos-13` GitHub runner is too flaky for the release matrix. Intel Mac users build from source via `cargo install --path .`.

## Releasing and the changelog

Releases are **manual-trigger only** (`workflow_dispatch` in `.github/workflows/release.yml`) and the version number is **never typed by hand** — you pick a bump and the workflow computes the rest. The model mirrors the sibling `ProcessGroup` repo.

- **`Cargo.toml` `version` is the single source of truth.** The release workflow reads it, bumps it (patch/minor/major from the form), stamps it into the build, and **commits the bump back to `main`** alongside the changelog — so `Cargo.toml`, the git tag (`v<version>`), the GitHub Release, and the npm packages never drift. (This drift is what caused the old "update loop": tags advanced while `Cargo.toml` stood still, so released binaries reported the stale version forever.)
- **`CHANGELOG.md` ([Keep a Changelog](https://keepachangelog.com/)) tracks release notes.** Curate the `[Unreleased]` section as you work — add bullets under `Added` / `Changed` / `Fixed`. **Manual bullets always win.** If `[Unreleased]` has no real bullets at release time, the workflow auto-fills it from git history via `git-cliff` (config: `cliff.toml`), bucketing commit subjects by prefix (`feat`→Added, `fix`→Fixed, `remove`→Removed, `perf`/`ux`/`refactor`/`ci`/…→Changed, `docs`/`chore`/`test`→skipped). Clean conventional-commit subjects are what make that fallback useful — keep writing them.
- **To cut a release:** run the **Release** workflow, choose `patch`/`minor`/`major` (optionally untick *Publish to npm*). The workflow's `prepare` job computes the version + guards against an existing tag + preflights the required release files + promotes `[Unreleased]` → `[<version>] - <date>` + extracts curated notes; `build` compiles the 3 platform binaries with the version stamped in; `publish-npm` dry-run-validates then ships the npm packages; `release` (which **runs only after `publish-npm` succeeds or is skipped**) commits `Cargo.toml` + `Cargo.lock` + `CHANGELOG.md` back to `main`, pushes the tag atomically, and publishes the GitHub Release with the curated notes (not raw commit logs). There is **no** `push:`/`tag:` auto-trigger — accidental tag pushes never start a release.
- **Failure-safety ordering (npm is the pivot).** npm publish is the only **irreversible** step (a version can never be re-published), so it runs **before** anything is pushed to git. A failed npm publish strands nothing — no tag, no bumped `main`, no Release — so you just **re-run** (publish is idempotent: already-published packages are skipped via `npm view`, transient errors retried; partial failure across the 4 packages is safe). The reversible steps after it are hardened so they can never drop a finished release: the commit+tag push is **`--atomic`** (branch and tag land together or not at all) **and idempotent** (skip-if-already-committed/tagged/pushed), and the **GitHub Release is idempotent + retried** (edit + re-upload if it already exists, else create). So if the GitHub Release step ultimately fails, **GitHub's "Re-run failed jobs" is a safe recovery** — it reuses *this* version and every step is idempotent — or finish it by hand with `gh release create`. Only a full **re-dispatch** of the workflow is dangerous: it re-reads the already-bumped `Cargo.toml` from `main`, ships the *next* version, and orphans this tag's release. (There is no long-lived publish token to preflight: npm uses OIDC Trusted Publishing and the Release uses the auto `GITHUB_TOKEN`.)

## Local-only files

`.gitignore` carves out `*.local.md`, `task_plan.md`, `findings.md`, `progress.md` — use those names freely for scratch notes; they won't be committed. `FILE_TREE.local.md` is the convention for the per-file responsibility doc.

## Windows specifics

For the **shell channel**, `ws update` uses the `self-replace` crate which handles the running-`.exe` lock via the rename-trick (move running exe aside as `.old`, drop new one in place, OS cleans up on next reboot). No user action needed.

For the **npm channel**, `ws update` shells out to `npm install -g`. npm tries to overwrite the running `wt.exe` and fails — users on the npm channel must close all shells running `wt` before updating. Reflect this in any user-facing messaging you add to the npm update path.

The repo's working tree may carry CRLF line endings on Windows despite `.gitattributes` mandating LF — that's stat-cache state from a pre-attributes checkout, not actual file divergence. The committed blobs are LF; pushed commits are clean. Colocated `jj st` may show phantom modifications for files that haven't been re-extracted since `.gitattributes` was added.

## Version control workflow

The repo uses [jujutsu (`jj`)](https://jj-vcs.github.io/jj/) (colocated with git). Use `jj` commands; the canonical workflow:

- **Per-prompt evaluation (mandatory).** Before any edits, run `jj st` and classify the incoming prompt against the current change description:

	| Signal in prompt | Category | Action |
	|---|---|---|
	| Same topic, refinement, follow-up of in-progress work | **Continuation** | Just work. jj auto-folds edits into the current change. |
	| Same change but goal has been refined or expanded | **Scope shift** | `jj describe -m "<refined summary>"`. **Don't** start a new change. |
	| Orthogonal topic, different area, "теперь сделай X" | **New work** | If current change is finished → `jj new -m "<summary>"` (descendant). If still in progress → `jj new @- -m "..."` (parallel sibling). |

	Reliable signals: word changes like "теперь" / "now" / "next" / "также сделай" / "and also" usually mean **new work** or **scope shift**. Imperative follow-ups inside the same scope ("исправь это", "почини", "продолжи") mean **continuation**. When in doubt, ask the user.

	A `UserPromptSubmit` hook (`.claude/hooks/jj-prompt-reminder.sh`) injects this same checklist into context each turn — the hook is the reminder, this table is the rulebook.

- **Describe early.** When starting a new piece of work, immediately set the change description:
	```
	jj describe -m "Concise summary"
	```
	The description should reflect intent *before* the work — not be backfilled at commit time. Keep extending the same `jj` change for follow-ups; don't spawn one per edit.
- **Sync on the user's trigger.** When the user says `pull` (or `push`/`sync`), run the full handshake:
	1. `jj git fetch` first — picks up any remote movement (CI release commits, etc.).
	2. Rebase if `main@origin` advanced: `jj rebase -r @- -d main@origin`.
	3. `jj bookmark set main -r <rev>` then `jj git push --bookmark main`.

	Never push without an explicit signal from the user.
- **Undoing dropped work.** When the user decides to abandon something already done, reach for `jj`'s safety net rather than hand-cleanup:
	- `jj undo` (alias of `jj op undo`) reverses the last operation — describe, edit, squash, rebase, abandon, push, all of it. Repeatable.
	- `jj abandon <rev>` drops a specific change entirely; descendants auto-rebase.
	- `jj restore` discards working-copy edits back to the parent's tree.
	- `jj op log` is the full reflog if you need to go further back via `jj op restore <op-id>`.
- **No new bookmarks** unless the user explicitly asks. Work lives on `main`; that is the publish target.

## VCS backend compatibility

This fork's primary goal is native `jj` backend support alongside `git`. The VCS layer is built on the published **`vcs-core`** facade (`vcs-toolkit-rs`); all VCS-touching code lives under `src/vcs/`:

- `src/vcs/repo.rs` — `crate::vcs::Repo`, the **single handle every command reaches a backend through**. It wraps a `vcs_core::Repo` (which owns the detected `vcs_git::Git` / `vcs_jj::Jj` client + the bound cwd) and dispatches each operation, by `inner.kind()`, to `ws`'s git/jj helper functions via the `inner.git()` / `inner.jj()` escape hatches. The wrapper is a thin, behaviour-preserving dispatch layer — **no semantic mapping happens here**; the helpers carry all of `ws`'s policy.
- `src/vcs/git/`, `src/vcs/jj/` — the per-backend helper functions (`repo`/`branch`/`ops`/`worktree` submodules), each taking a typed client + an explicit cwd and returning `ws`'s `Result`/DTOs. The locked-decision jj methods with no analogue surface `Error::Unsupported` from the wrapper's jj arm (via `jj::unsupported`): `move_worktree` and `*_abort`/`*_continue`.
- `src/vcs/mod.rs` — `resolve_backend(cwd, cli, project, global) -> Repo`, called once from `Cli::run`. There is **no** `VcsBackend` trait, `BackendState`, or `Box<dyn>` any more — `vcs_core::Repo` is the handle. `Repo::backend_name()` is the only intentional leak of the active-backend tag (for UI hint branching in `ws sync`/`ws status` — never for behavioural switches).
- `src/vcs/common.rs` — `ws`'s DTOs (`WorktreeInfo`, `DiffStat{insertions,deletions}`, `CreateOutcome`). These are kept (not swapped for the `#[non_exhaustive]` `vcs_core` DTOs, which `ws` can't construct in its parsers); the wrapper's helpers convert from the facade's DTOs at the boundary.

**Semantic deltas (jj vs git)** worth knowing when reading the helpers:

- **No staging area.** jj snapshots the working copy into `@` on every command, so "uncommitted" means "`@` differs from `@-`".
- **No in-progress merge/rebase state.** jj operations are atomic; conflicts record into the resulting commit. `is_rebase_in_progress` is always `false` (wrapper jj arm); `is_merge_in_progress` checks `@` for a conflict via `jj.has_workingcopy_conflict`.
- **Bookmarks ≠ branches.** jj bookmarks don't auto-follow `@`. The jj `merge()` helper advances the target bookmark explicitly via `jj bookmark move <name> --to <revset> --allow-backwards`. **`ws new` always creates a bookmark on the new workspace's `@`** — `current_branch()` errors otherwise ("no bookmark on @; …").
- **Dry-run merge** in jj captures the op id (`jj.op_head`), materialises the merge, checks the resulting commit's conflict flag, and `jj.op_restore`s to roll back. Documented inline in `src/vcs/jj/ops.rs::dry_run_merge`.

**When adding a new VCS operation, in order**:
1. Write the git helper in the appropriate `src/vcs/git/*.rs` submodule (`pub(crate) async fn foo(git: &GitClient, cwd: &Path, …) -> Result<…>`), driving the typed `vcs_git::Git` client (or a raw `processkit::Command` via `git::exec`/`capture` when the client doesn't model it).
2. Write the jj helper in `src/vcs/jj/*.rs`. If the op has no clean jj analogue, return `jj::unsupported("<opname>", "<hint>")` from the wrapper's jj arm rather than a panic.
3. Add a `Repo` method in `src/vcs/repo.rs` that dispatches `if self.is_git() { git::… } else { jj::… }`.
4. Test git helpers against a real repo via `Repo::git_at(path)` (see `src/vcs/git/tests.rs`); facade-layer logic can be tested hermetically with `vcs_core::Repo::from_git(ScriptedRunner)` if needed.

**Backend selection** (`resolve_backend`):

- CLI `--vcs <auto|git|jj>` (global flag) > project `[general] vcs` > global `[general] vcs` > `vcs_core::detect(cwd)` (the `.jj` > `.git` ancestor walk) > `git` fallback.
- **Colocated repos default to jj** (`.jj` present → `Jj`). Override with `--vcs=git` or `vcs = "git"` in `.workspace.toml`.
- A forced (non-`Auto`) choice bypasses detection and builds the requested backend via `vcs_core::Repo::from_git`/`from_jj`, regardless of what's on disk — it surfaces real errors when methods are called. We don't pre-validate.

**Network ops retry; local ops don't**. The targeted-fetch helpers (`src/vcs/{git,jj}/ops.rs::fetch_remote_branch`) retry on transient failures classified by `vcs_git::is_transient_fetch_error` / `vcs_jj::is_transient_fetch_error` (DNS / connection / EOF / processkit timeout) — `ws` no longer hand-rolls a marker list.

**Versions / edition**: built on `vcs-core 0.1` + `vcs-git`/`vcs-jj`/`vcs-github` `0.3` + `processkit 0.5`, all from crates.io. `edition = "2024"`, `rust-version = "1.91"`.
