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
-

### Changed
-

### Fixed
-

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
