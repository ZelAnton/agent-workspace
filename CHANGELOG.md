# Changelog

All notable changes to **agent-workspace** (`ws`) are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Add entries to `[Unreleased]` as you work — manual bullets always win over the
release workflow's git-cliff auto-fill. On release the workflow promotes
`[Unreleased]` to a dated version section and posts it as the GitHub Release
notes.

## [Unreleased]

### Added
- `LICENSE` file (MIT) — the package already declared MIT but shipped no license text.
- `--format json` now covers every command that prints a result — `sync`, `clean`, `rm`, `mv`, `cd`, `config get`/`set`/`unset`/`list`, and `exclude` (list + add/remove/clear) join the existing `ls`/`status`/`repo-info`/`new`/`merge`. The mutating `config`/`exclude` subcommands previously printed confirmations to stdout even under `--format json`, corrupting the output for a parser; they now emit a single JSON object. Each JSON object carries a top-level `schema_version` (a stable, versioned contract for agents); see `docs/json-output.md`.
- Shell completion is now backend-aware: branch/worktree completion unions git refs with jj bookmarks, so the right names appear in pure-jj and colocated repos (previously git-only). Strategy flags (`--strategy`) and `ws config` keys also complete now.
- New-tab integration now supports **WezTerm** (`wezterm cli spawn`) and **tmux** (`tmux new-window`), in addition to Windows Terminal / iTerm2 / GNOME Terminal. tmux is chosen only when no tabbed GUI terminal is detected.
- `ws checkout` / `ws switch` are now aliases for `ws cd`.

### Changed
- User-facing messages now print merge/sync strategies as `squash`/`merge`/`rebase` (a `Display` impl) instead of the debug-formatted `Squash`/`Rebase`.
- Internal: deduplicated the git/jj `processkit::Error` mapping into shared `vcs::error` helpers; extracted a generic `BackendState<C>` so `GitBackend`/`JjBackend` no longer repeat constructor/cwd boilerplate; and pulled the global+project config merge into a pure, unit-tested `merge_global_project` (with exhaustive per-field coverage so a forgotten merge fails CI). No user-visible behavior change.
- The terminal-tab recursion guard (`WS_SPAWNED_IN_TAB`) now takes precedence over an explicit `--in-new-tab`, making it an unconditional spawn-disable (no behavior change in normal use; defense against runaway tab spawning).
- Internal: both VCS backends now run on the published `processkit` (async, job-backed process runner) + `vcs-git`/`vcs-jj` typed clients, instead of the in-house `vcs-runner`/`procpilot` pair; `ws` runs on a tokio runtime. The `vcs-github` (`gh`) client is wired in and ready (`Repo::github()`) but not yet used by any command. No user-visible behavior change.

### Fixed
- Windows Terminal new-tab integration now soft-probes for `wt.exe` at detection time, so a stale `WT_SESSION` (inherited by a shell no longer running under WT) falls back to in-place creation instead of failing `ws new` mid-flight.
- The "not in a … repository" error is now backend-neutral ("version-controlled" instead of "git"), matching jj support.
- CoW `ws new` on Windows no longer silently ignores deep/glob `[copy] exclude` patterns (e.g. `**/*.iso`): robocopy can only express literal top-level excludes, so when a pattern can match deeper the copy now falls through to the in-process copier (which honors it at every depth) instead of dropping it. Top-level excludes still take the fast robocopy path.
- `ws config --help` (and related comments) now say `.workspace.toml`, matching where `ws config` / `ws exclude` actually write.
- `ws update` on the npm channel installed the wrong (unscoped) package; it now uses the correct `@zelanton/agent-workspace`.
- `ws merge` no longer silently retargets to trunk when the worktree's base branch was deleted — it refuses and points you at `ws merge --into <branch>`, matching snap-mode behavior.
- jj `ws merge` now advances the explicit destination branch instead of guessing the lexicographically-smallest bookmark on the commit, which could move (and with `--allow-backwards`, lose) the wrong bookmark when several share a commit (e.g. `main` + `master`).
- Snap-mode "no changes" cleanup now steps out of the worktree before removing it, so it works on Windows (the OS locks the process's current directory).
- `ws merge -d`, `ws clean`, and `ws mv` no longer strand the shell in a deleted/moved directory when run without shell integration — they refuse (or skip) the current worktree, matching `ws rm`.
- `ws init` now writes its config at the main repo root instead of the current directory, so running it from a subdirectory no longer creates a config that nothing reads.
- Copy-on-Write worktree creation now fails (and rolls back) instead of silently reporting success when files can't be copied, preventing a corrupt worktree with missing files. The jj backend likewise rolls back if its post-copy snapshot fails.
- git Copy-on-Write no longer applies a stashed change onto the wrong branch when restoring the source repo after a failed creation.
- Repo-level `.workspace.toml` is no longer silently ignored on unusual git layouts (bare repos / custom git-dir names) where the repo root couldn't be walked back from the git common dir.
- Windows hooks are now passed to `cmd /C` verbatim, so hook commands containing quotes, `%`, or `^` are no longer mangled by argument re-quoting.
- `WorktreeMeta` is now written atomically (temp file + rename), so an interrupted write can't leave a truncated meta file that makes merge/sync silently fall back to trunk.
- `ws cd <branch>` rejects path-traversal arguments (e.g. `../../foo`) that could move the shell outside the workspace.
- Hardened shell-wrapper removal to match the `# === agent-workspace BEGIN/END ===` markers as whole lines, so a stray mention of the marker text elsewhere in an rc file can't trigger deletion of unrelated config.

## [0.15.0] - 2026-05-30

### Added
- Per-worktree config: `.workspace.toml` at an individual worktree's root now overrides the repo-level config for commands run inside that worktree (3-tier hierarchy: global → repo → worktree).

### Changed
- Project config file renamed to `.workspace.toml` (local, per-machine — auto-added to the repo's local git exclude file `.git/info/exclude`, so it needs no commit and never dirties the working tree; one entry in the shared common git dir covers the main repo and every worktree). The legacy committed `.agent-workspace.toml` is still read as a fallback, so existing repos keep working. `ws config` / `ws exclude` now write `.workspace.toml`.

### Fixed
-

## [0.14.1] - 2026-05-29

### Added
- `ws new <name>` now resumes an existing branch: if a branch/bookmark named `<name>` already exists, the worktree is created from it instead of failing.
- `ws new <name>` now also checks the remote (cheap `git ls-remote`, no fetch): if `<name>` isn't local but exists on `origin`, that one branch is fetched and the worktree created from it automatically, without prompting.
- `ws new <name>` for a new branch now offers an interactive menu (when run in a terminal): create the branch from the current branch (default), or pick a different base branch.

### Changed
-

### Fixed
-

## [0.14.0] - 2026-05-29

### Added

- feat(release): add CHANGELOG.md + cliff.toml, baseline Cargo.toml to 0.13.27


### Changed

- ux(git): rename 'worktree skeleton' status line to 'workspace skeleton'
- ci(release): bump-choice versioning + curated CHANGELOG release notes

## [0.13.27] - 2026-05-29

### Changed

- Baseline entry; changelog tracking and automatic versioning introduced from this release onward.

[Unreleased]: https://github.com/ZelAnton/agent-workspace/compare/v0.15.0...HEAD
[0.15.0]: https://github.com/ZelAnton/agent-workspace/compare/v0.14.1...v0.15.0
[0.14.1]: https://github.com/ZelAnton/agent-workspace/compare/v0.14.0...v0.14.1
[0.14.0]: https://github.com/ZelAnton/agent-workspace/compare/v0.13.27...v0.14.0
[0.13.27]: https://github.com/ZelAnton/agent-workspace/releases/tag/v0.13.27
