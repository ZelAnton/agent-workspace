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

### Merge is atomic — no continue/abort

`ws merge` records the main repo's current branch, dry-runs the merge with `--squash --no-commit` or `--no-ff --no-commit` (matching the real strategy), and only proceeds if the dry-run is conflict-free. On any failure: `reset_merge` + checkout original branch. There is intentionally no `ws merge --continue/--abort` — the recovery path for conflicts is `ws sync` inside the worktree, then re-run `ws merge`. Don't add intermediate-state handling; preserve the atomic property.

`ws sync`, by contrast, is git-native rebase/merge — its conflicts *do* leave recoverable state, which is why `ws status` detects in-progress rebase/merge and prints `ws sync --continue/--abort` hints.

### Target branch resolution: CLI override > base_branch (if still exists) > trunk

`meta::resolve_target_branch` (pure, in `src/meta/mod.rs`) is the single resolver used by merge/sync/clean/status. `resolve_effective_target` is the I/O wrapper that loads the `{branch}.toml` meta. If the worktree's `base_branch` was deleted, the fallback to trunk for `ws clean` is fine, but `ws merge`/snap-continue refuse rather than silently retargeting (landing commits on the wrong branch is worse than an error). Anything that picks a target must go through this resolver.

### Worktree metadata: format migrated, legacy still readable

Current format: `{wt_dir}/{branch}.toml` with `created_at` + `base_branch`. Legacy format: `{branch}.status.toml` with `trunk` (now an alias for `base_branch`) plus dropped fields (`base_commit`, `snap_command`). `meta::meta_path_with_fallback` and the `RawMeta` deserialization shim handle both — `base_branch` wins when both keys are present. Use `meta::remove_meta` (deletes both names) rather than `fs::remove_file` directly.

### Project config is read from the main-repo root, not cwd

`src/config/mod.rs` resolves `.agent-workspace.toml` by walking up from `git rev-parse --git-common-dir` — so the same config applies whether the user is in the main repo, a worktree, or a subdirectory.

**Config merge rules** (project over global): `copy_files` appends, `hooks` *replaces* if project is non-empty (not appends), `merge_strategy`/`sync_strategy` are `Option`-overrides, `trunk` is project-only.

### Hooks run unsandboxed — treat `.agent-workspace.toml` as a committed shell script

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
- **To cut a release:** run the **Release** workflow, choose `patch`/`minor`/`major` (optionally untick *Publish to npm*). The workflow's `prepare` job computes the version + guards against an existing tag + promotes `[Unreleased]` → `[<version>] - <date>` + extracts curated notes; `build` compiles the 3 platform binaries with the version stamped in; `release` commits `Cargo.toml` + `Cargo.lock` + `CHANGELOG.md` back to `main`, pushes the tag atomically, and publishes the GitHub Release with the curated notes (not raw commit logs); `publish-npm` ships the npm packages. There is **no** `push:`/`tag:` auto-trigger — accidental tag pushes never start a release.

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

This fork's primary goal is native `jj` backend support alongside `git`. All VCS-touching code lives under `src/vcs/`:

- `src/vcs/backend.rs` — the `VcsBackend` trait. **Every public VCS operation is a trait method, no exceptions.**
- `src/vcs/git/` — `GitBackend`. Each method goes through `vcs_runner::Cmd::new("git").in_dir(cwd).args(...)` via an `Arc<dyn procpilot::Runner>` so tests can swap in `MockRunner`.
- `src/vcs/jj/` — `JjBackend`. Implemented for `ws`'s happy-path workflows (identity, bookmarks, workspaces, state probes, diff, merge/rebase/checkout/commit/fetch). The locked-decision methods that have no jj analogue surface `Error::Unsupported`: `move_worktree` (use `jj workspace forget` + manual move) and `*_abort`/`*_continue` (jj records conflicts in commits — resolve and re-run).
- `src/vcs/mod.rs` — facade `vcs::foo()` free functions backed by a thread-local backend. Production code in `src/cli/commands/` calls `crate::vcs::repo_root()` etc. — no `Box<dyn VcsBackend>` ever leaks out of this module. Backend resolution lives in `resolve_backend(cli, project, global)`, called once from `Cli::run`. The `vcs::backend_name()` accessor is the only intentional leak of the active-backend tag (for UI hint branching in `ws sync`/`ws status` — never use it for behavioural switches).

**Semantic deltas (jj vs git)** worth knowing when reading the trait impls:

- **No staging area.** jj snapshots the working copy into `@` on every command, so "uncommitted" means "`@` differs from `@-`". `has_staged_changes` was removed from the trait in F-7 (git's only consumer, `merge.rs::execute_merge`, was refactored to drop the staging gate).
- **No in-progress merge/rebase state.** jj operations are atomic; conflicts record into the resulting commit. `is_rebase_in_progress` is always `false`. `is_merge_in_progress` checks `jj st` for the `"unresolved conflicts"` marker (jj ≥ 0.16 wording — fall back to a regex if a future jj rev changes the string).
- **Bookmarks ≠ branches.** jj bookmarks don't auto-follow `@`. The `merge()` impl explicitly advances the target bookmark via `jj bookmark move <name> --to <revset> --allow-backwards` after the squash/merge. **`ws new` always creates a bookmark on the new workspace's `@`** — `current_branch()` errors otherwise with the message "no bookmark on @; run ws new or jj bookmark create".
- **Dry-run merge** in jj uses `jj op log` to capture the operation id, then materialises the merge via `jj new`, checks the resulting commit's `conflict` flag, and `jj op restore <pre-op-id>` to roll back. There's a ~10ms window where a concurrent reader sees the merge commit on `@`. Acceptable per the locked decision; documented inline in `src/vcs/jj/ops.rs::dry_run_merge`.

**When adding a new VCS operation, in order**:
1. Add the trait method to `VcsBackend` (signature + doc comment).
2. Implement it on `GitBackend` (helper function in the appropriate `src/vcs/git/*.rs` submodule + delegation in `src/vcs/git/mod.rs`). Prefer the `runner.run(Cmd::new("git").in_dir(&cwd).args(...))` pattern; pull a `vcs-runner` parser only if the output shape genuinely matches (don't force-fit `parse_diff_summary` onto `--shortstat`, etc.).
3. Implement it on `JjBackend` (helper function in the appropriate `src/vcs/jj/*.rs` submodule). If the op has no clean jj analogue, return `Error::Unsupported("jj: <opname> — <hint>")` inline rather than a panic — users hitting the path get a clear message.
4. Add the facade free function in `src/vcs/mod.rs`.
5. Test against `MockRunner` if the logic is parser-heavy (see `src/vcs/{git,jj}/tests/mock_runner.rs`); against a real repo (`setup_test_repo` / `jj_repo` + `with_cwd`) otherwise. The shared `CWD_MUTEX` (in `src/vcs/mod.rs`) serializes both test suites — they run in the same lib test binary.

**Backend selection**:

- CLI `--vcs <auto|git|jj>` (global flag) > project `[general] vcs` > global `[general] vcs` > `vcs_runner::detect_vcs(cwd)` > `git` fallback.
- **Colocated repos default to jj** (`.git/` + `.jj/` both present → `JjBackend`). The user installed jj for a reason; respect that. Override with `--vcs=git` or `vcs = "git"` in `.agent-workspace.toml`.
- `--vcs=jj` in a git-only checkout, or `--vcs=git` in a jj-only checkout, are both honored — they install the requested backend, which will surface real errors when methods are called. We don't pre-validate.

**Network ops retry; local ops don't**. Git's `fetch()` uses a custom transient predicate (`is_transient_fetch_err` in `src/vcs/git/ops.rs`) matching DNS / connection / EOF stderr patterns. Jj's `fetch()` uses `vcs_runner::is_transient_error` directly (jj's stderr shapes match procpilot's default). `RetryPolicy::default()` matches `"stale"`/`".lock"` only — wrong shape for network failures.

**Edition / MSRV**: this crate is on `edition = "2024"`, `rust-version = "1.91"` (matches `vcs-runner` MSRV).
