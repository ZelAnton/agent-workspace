# JSON output contract

`ws` is driven by AI agents as well as humans. Pass the global `--format json`
flag to any read/result command and it emits a **single JSON object on stdout**;
all progress, notices, hints, and warnings go to **stderr**, so stdout stays a
clean parse target.

```bash
ws ls --format json | jq '.worktrees[].branch'
```

## The `schema_version` envelope

Every JSON object carries a top-level `schema_version` (currently `1`), injected
centrally in `src/cli/output.rs` (`emit_json`). Agents should gate on it.

**Versioning policy:**

- **Additive** changes (a new key alongside existing ones) do **not** bump the
  version. Consumers must ignore unknown keys.
- **Breaking** changes (a key removed/renamed, or its type/meaning changed) bump
  `SCHEMA_VERSION`.

## Per-command shapes

`<envelope>` below is shorthand for the always-present `"schema_version": 1`.

| Command | Object shape (besides the envelope) |
|---|---|
| `ws ls` | `{ worktrees: [ { branch, base_branch, is_current, uncommitted, commits, insertions, deletions, path, created_at } ] }` |
| `ws status` | per-worktree detail object (branch, base, target, counts, path, state) |
| `ws repo-info` | repo metadata cache (file count, size, origin URL, GitHub slug) |
| `ws new` | creation result `{ branch, path, base_branch, created, snap }`. When the flow opens a new terminal tab instead (the worktree is then created in that tab), this shell emits `{ opened_in_tab: true, branch, snap }` instead. |
| `ws merge` | `{ merged, branch, target, commits, deleted }` |
| `ws sync` | `{ action: "rebased"\|"merged"\|"aborted"\|"continued", branch, target, strategy }` |
| `ws clean` | `{ dry_run, cleaned: [branch], skipped_dirty: [branch], returned_to }` |
| `ws rm` | `{ action: "removed", branch, returned_to }` |
| `ws mv` | `{ action: "renamed", old_branch, new_branch, returned_to }` |
| `ws cd` | `{ action: "cd", target, title, opened_in_tab }` |
| `ws config get` | `{ key, value }` (`value` is `null` when unset) |
| `ws config set` / `unset` | `{ action: "set"\|"unset"\|"noop", key, value }` (`value` null for unset) |
| `ws config list` | `{ path, keys: [ { key, kind, value, description } ] }` |
| `ws exclude <path>...` / `--remove` / `--clear` | `{ action: "added"\|"removed"\|"cleared"\|"noop"\|"saved"\|"cancelled", patterns: [string] }` (`patterns` = full list after the mutation) |
| `ws exclude --list` | `{ patterns: [string] }` |

Fields named `returned_to` carry the main-repo path the shell was redirected to
when the command removed/renamed the worktree the user was standing in (else
`null`).

## stdout / stderr discipline

In `json` mode, stdout contains **only** the one JSON object. Everything else —
the "Merging…" progress line, the update nag, hook output — is on stderr. A
command that takes a slow path (e.g. `ws new` copying a large repo) suppresses
its progress spinner entirely under `--format json`.
