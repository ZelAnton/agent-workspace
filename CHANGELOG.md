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

[Unreleased]: https://github.com/ZelAnton/agent-workspace/compare/v0.14.1...HEAD
[0.14.1]: https://github.com/ZelAnton/agent-workspace/compare/v0.14.0...v0.14.1
[0.14.0]: https://github.com/ZelAnton/agent-workspace/compare/v0.13.27...v0.14.0
[0.13.27]: https://github.com/ZelAnton/agent-workspace/releases/tag/v0.13.27
