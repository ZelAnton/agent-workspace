# AGENTS.md

This file provides guidance to AI coding agents when working with code in this repository.

## Project

`agent-workspace` — a Rust CLI (`wt`) that manages git worktrees for AI coding agents. Distributed as a single static binary, packaged for npm with per-platform subpackages under `npm/`.

`ARCHITECTURE.md` (Chinese) is the canonical design doc. `FILE_TREE.local.md` (gitignored) is the single source of truth for per-file responsibilities — read it if it exists before doing structural work.

## Build, test, run

```bash
cargo build                  # debug binary at target/debug/wt
cargo build --release        # release binary
cargo test                   # all unit + integration tests
cargo test --test cmd_merge  # single integration test file (one per command in tests/)
cargo test <name>            # run tests matching a substring
cargo clippy --all-targets   # lint
cargo fmt                    # format

./scripts/build-npm.sh current        # build + stage binary into npm/agent-workspace-<platform>/bin
./scripts/build-npm.sh all            # cross-compile all four target triples (needs `cross`)
```

Integration tests in `tests/cmd_*.rs` shell out to `target/debug/wt` — they assume `cargo build` has run. `tests/common/mod.rs` provides `setup_git_repo`, `setup_worktree_test_env`, and `create_path_file` helpers; use them rather than rolling your own git fixtures.

## Architectural invariants

These aren't obvious from reading individual files — they constrain how features must be built.

### Shell integration is load-bearing for any `cwd`-changing command

`wt cd`, `wt new`, `wt rm`, `wt mv`, `wt merge -d`, `wt clean` change the user's shell directory. The binary can't do that directly; instead it writes the target path to a temp file passed via the hidden global `--path-file <FILE>` flag, and the shell wrapper (installed by `wt setup`, sources defined in `src/shell/mod.rs`) reads that file and `cd`s. Helpers: `cli::write_path_file` (single line) and `cli::write_path_file_lines` (multi-line, used by snap). Any new command that wants to move the user must accept `path_file: Option<&Path>` and write to it — `wt cd` deliberately errors out when `--path-file` is missing rather than silently no-op'ing.

### Snap mode exit codes are a wire protocol with the shell wrapper

`src/cli/commands/snap/resume.rs` exports `EXIT_DONE = 0`, `EXIT_REOPEN = 2`, `EXIT_PRESERVE = 3`. The bash/zsh/fish/PowerShell wrapper scripts in `src/shell/mod.rs` switch on these to either loop the agent, drop the user into the worktree, or `cd` back to the main repo. **Changing these constants requires updating every wrapper template in lockstep.** Nested snap (`wt new -s` from inside a worktree) is refused for the same reason — two snap loops in one parent shell break the cwd tracking.

### Merge is atomic — no continue/abort

`wt merge` records the main repo's current branch, dry-runs the merge with `--squash --no-commit` or `--no-ff --no-commit` (matching the real strategy), and only proceeds if the dry-run is conflict-free. On any failure: `reset_merge` + checkout original branch. There is intentionally no `wt merge --continue/--abort` — the recovery path for conflicts is `wt sync` inside the worktree, then re-run `wt merge`. Don't add intermediate-state handling; preserve the atomic property.

`wt sync`, by contrast, is git-native rebase/merge — its conflicts *do* leave recoverable state, which is why `wt status` detects in-progress rebase/merge and prints `wt sync --continue/--abort` hints.

### Target branch resolution: CLI override > base_branch (if still exists) > trunk

`meta::resolve_target_branch` (pure, in `src/meta/mod.rs`) is the single resolver used by merge/sync/clean/status. `resolve_effective_target` is the I/O wrapper that loads the `{branch}.toml` meta. If the worktree's `base_branch` was deleted, the fallback to trunk for `wt clean` is fine, but `wt merge`/snap-continue refuse rather than silently retargeting (landing commits on the wrong branch is worse than an error). Anything that picks a target must go through this resolver.

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

- `src/cli/` — clap definitions + dispatch. Commands grouped: `commands/lifecycle/` (new/rm/clean), `commands/nav/` (cd), `commands/snap/` (resume), `commands/sys/` (setup/init/update), plus top-level `ls`, `merge`, `move`, `status`, `sync`.
- `src/git/` — git CLI wrappers split into `repo` / `worktree` / `branch` / `ops`. `mod.rs` is just re-exports + the shared `extract_error` (checks stderr, falls back to stdout because merge/commit put error text on stdout).
- `src/meta/` — `{branch}.toml` (de)serialization + target resolver (pure functions).
- `src/config/` — global + project loading and merging.
- `src/shell/` — wrapper script templates and rc-file install/uninstall (strict BEGIN/END marker pairing — refuses to touch a file with orphaned markers).
- `src/process/` `src/prompt/` `src/update/` `src/util/` — hook execution, dialoguer prompts, daily update check, random branch-name generator (~100 adjectives × ~100 nouns, numeric suffix on collision).

## Install channels and `wt update`

Two install channels, distinguished by a marker file at `~/.agent-workspace/install_channel` (content: `npm` or `shell`):

- **npm** — `npm install -g agent-workspace`. Postinstall (`npm/agent-workspace/install.js`) writes `npm` to the marker.
- **shell** — `install.sh` / `install.ps1` at repo root. Downloads a prebuilt archive from GitHub Releases, places `wt` at `~/.agent-workspace/bin/wt`, writes `shell` to the marker.

`wt update` (`src/cli/commands/sys/update.rs`) reads the marker via `update::detect_channel()` and branches:
- `Channel::Npm` → `npm install -g agent-workspace@latest` (legacy path).
- `Channel::Shell` → `update::self_update()` downloads `agent-workspace-<version>-<platform>.tar.gz` from the GitHub release and uses the `self-replace` crate for atomic binary replacement, then re-invokes `wt setup`.

Missing marker defaults to `Channel::Npm` — keeps existing installs predating the marker working.

The version check (`update::check_update` in `src/update/mod.rs`) hits the **GitHub Releases API** for both channels — GitHub is the canonical truth, npm publishes happen after a GitHub release. Requires a non-empty `User-Agent` header (GitHub returns 403 otherwise — `USER_AGENT` const handles this).

Platform key strings (`darwin-arm64`, `darwin-x64`, `linux-x64`, `win32-x64`) must stay consistent across `update::platform_key()`, `npm/agent-workspace/bin/wt.js`, `install.sh`, `install.ps1`, and the CI release archive naming (`.github/workflows/release.yml`). Changing them requires touching all five.

## Local-only files

`.gitignore` carves out `*.local.md`, `task_plan.md`, `findings.md`, `progress.md` — use those names freely for scratch notes; they won't be committed. `FILE_TREE.local.md` is the convention for the per-file responsibility doc.

## Windows specifics

For the **shell channel**, `wt update` uses the `self-replace` crate which handles the running-`.exe` lock via the rename-trick (move running exe aside as `.old`, drop new one in place, OS cleans up on next reboot). No user action needed.

For the **npm channel**, `wt update` shells out to `npm install -g`. npm tries to overwrite the running `wt.exe` and fails — users on the npm channel must close all shells running `wt` before updating. Reflect this in any user-facing messaging you add to the npm update path.

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
- `src/vcs/jj/` — `JjBackend`. Implemented for `wt`'s happy-path workflows (identity, bookmarks, workspaces, state probes, diff, merge/rebase/checkout/commit/fetch). The locked-decision methods that have no jj analogue surface `Error::Unsupported`: `move_worktree` (use `jj workspace forget` + manual move) and `*_abort`/`*_continue` (jj records conflicts in commits — resolve and re-run).
- `src/vcs/mod.rs` — facade `vcs::foo()` free functions backed by a thread-local backend. Production code in `src/cli/commands/` calls `crate::vcs::repo_root()` etc. — no `Box<dyn VcsBackend>` ever leaks out of this module. Backend resolution lives in `resolve_backend(cli, project, global)`, called once from `Cli::run`. The `vcs::backend_name()` accessor is the only intentional leak of the active-backend tag (for UI hint branching in `wt sync`/`wt status` — never use it for behavioural switches).

**Semantic deltas (jj vs git)** worth knowing when reading the trait impls:

- **No staging area.** jj snapshots the working copy into `@` on every command, so "uncommitted" means "`@` differs from `@-`". `has_staged_changes` was removed from the trait in F-7 (git's only consumer, `merge.rs::execute_merge`, was refactored to drop the staging gate).
- **No in-progress merge/rebase state.** jj operations are atomic; conflicts record into the resulting commit. `is_rebase_in_progress` is always `false`. `is_merge_in_progress` checks `jj st` for the `"unresolved conflicts"` marker (jj ≥ 0.16 wording — fall back to a regex if a future jj rev changes the string).
- **Bookmarks ≠ branches.** jj bookmarks don't auto-follow `@`. The `merge()` impl explicitly advances the target bookmark via `jj bookmark move <name> --to <revset> --allow-backwards` after the squash/merge. **`wt new` always creates a bookmark on the new workspace's `@`** — `current_branch()` errors otherwise with the message "no bookmark on @; run wt new or jj bookmark create".
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
