# agent-workspace 架构设计文档

## 概述

agent-workspace 是一个 Git Worktree 工作流工具，为 AI coding agent 提供隔离的并行开发环境。

**核心价值**：
- 并行开发：同时运行多个 agent，互不干扰
- 环境隔离：每个功能独立工作目录
- 流程自动化：`-s/--snap` 模式实现"即用即删"的完整开发闭环

---

## 目录结构

基础目录默认 `~/.agent-workspace`，可通过 `AGENT_WORKSPACE_DIR` 环境变量覆盖（空串视同未设）。

```
$AGENT_WORKSPACE_DIR/  (默认 ~/.agent-workspace/)
├── config.toml                    # 全局配置
└── workspaces/                    # 所有 worktree 存储位置
    └── {repo}-{hash}/             # 按项目组织（hash 基于仓库绝对路径，防止同名冲突）
        ├── swift-fox.toml         # worktree 元数据（旧版 .status.toml 仍兼容）
        ├── swift-fox/             # 随机生成的分支名
        ├── fix-auth-bug.toml
        ├── fix-auth-bug/          # 用户指定的分支名
        ├── quiet-moon.toml
        └── quiet-moon/
            └── ...                # 项目文件

项目根目录/
└── .workspace.toml                 # 项目级配置（本地，自动加入 .git/info/exclude；legacy 回退 .agent-workspace.toml）
```

### 元数据格式

```toml
created_at = 2024-01-15T10:30:00Z
base_branch = "main"             # 创建时的源分支（merge/sync 默认目标）
```

> 旧版字段（`base_commit`/`trunk`/`snap_command`）已弃用。读取时若缺 `base_branch` 则回退到旧 `trunk` 字段；其他旧字段静默忽略。

---

## 命令设计

### 1. Worktree 管理

```bash
ws new [branch]              # 创建 worktree 并进入（base = current_branch；detached HEAD 时回退 trunk）
ws new [branch] --base <br>  # 显式指定 base 分支（必须存在，覆盖默认；同时记录到 meta）
ws new [branch] -s <cmd>     # 创建 + snap 模式
ws cd [branch]               # 切换到指定 worktree（省略则回到主仓库）
ws ls                        # 列出 worktree（按创建时间降序）
ws status                    # 查看当前 worktree 详细信息
ws mv <old> <new>            # 重命名 worktree 分支（old 可用 . 表示当前）
ws rm <branch> [-f]          # 删除 worktree（branch 可用 . 表示当前）
ws clean [--dry-run]         # 清理所有与 target 无差异的 worktree（target = base_branch > trunk）
```

### 2. 工作流

```bash
ws merge [options]           # 合并当前 worktree（默认 merge 回 base branch，fallback trunk）
    -s, --strategy <squash|merge>  # 合并策略，默认 squash
    --into <branch>          # 合并到指定分支（覆盖 base branch / trunk，校验存在性）
    -d, --delete             # 合并后删除 worktree（默认保留）
    -H, --skip-hooks         # 跳过 pre-merge hook

ws sync [options]            # 从 base branch 同步更新到当前 worktree（fallback trunk）
    -s, --strategy <rebase|merge>  # 同步策略，默认 rebase（可被 sync_strategy 配置覆盖）
    --from <branch>          # 指定同步源分支（覆盖 base branch / trunk，校验存在性）
    --continue               # 解决冲突后继续
    --abort                  # 放弃同步，恢复到冲突前状态
```

### 3. 维护

```bash
ws update                    # 更新到最新版本
```

### 4. 配置

```bash
ws setup                     # 安装 shell 集成（自动检测 shell）
ws setup --shell zsh         # 指定 shell
ws init [options]            # 在当前项目初始化配置
    --trunk <branch>         # 主干分支
    --merge-strategy <squash|merge>  # 默认合并策略
    --sync-strategy <rebase|merge>   # 默认同步策略
    --copy-files <pattern>   # 复制文件模式（可重复）
```

---

## Shell 集成

`ws cd`、`ws new`、`ws rm`、`ws mv`、`ws merge`、`ws clean` 等命令需要改变 shell 工作目录，因此需要 shell wrapper。

运行 `ws setup` 自动安装（npm 安装时会自动执行），会在 shell 配置文件中添加 wrapper 函数。

**支持的 shell**：bash、zsh、fish、powershell

**配置文件位置**：
- bash: `~/.bashrc`
- zsh: `~/.zshrc`
- fish: `~/.config/fish/config.fish`
- powershell: `~/Documents/PowerShell/Microsoft.PowerShell_profile.ps1`

### 集成约束

- **Wrapper 必装才能 cd**：`ws cd` 检测无 `--path-file` 直接报错，提示 `ws setup`——不再静默 noop
- **`ws rm .` 防误操**：cwd 在被删 worktree 内且无 wrapper → 拒绝（避免 dangling cwd）
- **rc 文件 marker 严格配对**：`ws setup` 找到孤立 BEGIN/END 直接报错，不动 rc，避免截断
- **path_file 唯一**：bash/zsh wrapper 用 `mktemp` 而非 `$$`（subshell 中 `$$` 是父 PID，并发会撞）
- **agent 退出统一**：crash/SIGINT/非零状态都进 snap-continue
- **Windows update**：`ws update` 调用 npm，运行中的 `wt.exe` 被 OS 锁定 → 先关闭所有 wt 进程

---

## 分支名生成

1. **用户指定**：`ws new fix-auth-bug` → 使用 `fix-auth-bug`
2. **自动生成**：`ws new` → 生成 `形容词-名词` 格式，如 `swift-fox`

词库内置约 100 个形容词 + 100 个名词。冲突时追加数字后缀（`swift-fox-2`）。

---

## Snap 模式

"即用即删"的完整流程：

```
创建 worktree → 进入目录 → 启动 agent → [开发] → agent 退出 → 检查更改 → 合并 → 清理
```

```bash
ws new -s claude  # 简单命令，随机分支名
ws new -s "aider --model sonnet"  # 带参数的命令需要引号
ws new fix-bug -s cursor  # 指定分支名
```

### Agent 退出处理

**正常退出**，检查 git 状态：

| 状态 | 行为 |
|------|------|
| 无改动（uncommitted=❌, commits=❌） | 直接清理 worktree |
| 只有 commits（uncommitted=❌, commits=✅） | prompt: [m] merge / [q] exit |
| 有未提交改动（uncommitted=✅） | prompt: [r] reopen / [q] exit |

**有 commits 时** prompt：
```
[m] Merge into trunk
[q] Exit snap mode
```

**有未提交改动时** prompt：
```
[r] Reopen agent (let agent commit)
[q] Exit snap mode
```

选择 `[q]` 退出时：
- 保留在当前 worktree（不 cd 到 main）
- worktree 完整保留，后续可手动处理：
```bash
git add . && git commit -m 'message'
ws merge          # merge 并清理
```

**异常退出**（crash / Ctrl+C），worktree 保留为普通 worktree

---

## Merge 冲突处理

### 原子 merge（预检测模式）

merge 为原子操作——要么成功，要么 HEAD 回到原分支。不残留中间状态。

```
ws merge
  → 记录主 repo 当前分支为 original
  → checkout target（main repo）
  → dry-run（按真实策略：squash 用 --squash --no-commit，否则 --no-ff）
  → 有冲突？
      YES → 清理 + checkout original → 报错 "先 ws sync 解决冲突"
      NO  → 清理 + 执行真实 merge
              失败 → reset_merge + checkout original → 抛错
              成功 → 跑 post_merge hook → 可选删 worktree
```

### 冲突处理流程

用户需在 worktree 中先 sync 对齐目标分支，再执行 merge：

```bash
ws sync          # 在 worktree 中解决冲突
ws merge         # 无冲突，原子完成
```

### 安全检查与约束

- 主 repo 的未完成 merge / rebase / uncommitted changes → 拒绝
- worktree dirty → 拒绝（消息明示是 worktree 端脏）
- 主 repo dirty → 拒绝（消息明示是 main repo 端脏）
- `--into <branch>` 已被另一 worktree checkout → 拒绝（避免 git 报底层错）
- `MergeStrategy::Merge` already-up-to-date → 返回 "Nothing to merge" 不删 worktree
- 失败一律 rollback HEAD 到原分支 + reset_merge 清 squash 半成品

### merge 入口

- `merge::execute_merge()` 处理 squash/merge 策略，`snap_continue` 和 `ws merge` 共用
- `git::dry_run_merge(branch, squash)` 用于预检测冲突，按策略走 `--squash --no-commit` 或 `--no-ff --no-commit`

> 不提供 `ws merge --continue/--abort`：原子语义保证失败 = HEAD 复位，无残留 git 状态需要续/弃。冲突恢复路径只有一条：在 worktree 中 `ws sync`，然后重新 `ws merge`。

---

## Status 输出

`ws status` 显示当前 worktree 的：

- Branch / Base branch（meta）/ Trunk / Merge target（CLI > base_branch（仍存在）> trunk）
- Created at（meta）
- Commits ahead of merge target / Uncommitted count / 累计 diff +ins -del
- Worktree 路径

并检测 git-native 同步状态——`is_rebase_in_progress()` 或 `is_merge_in_progress()` 命中时，追加：

```
State:        REBASE/MERGE IN PROGRESS (sync)
  Resolve conflicts, then: ws sync --continue
  Or abort: ws sync --abort
```

> 仅识别 git-native 状态。`ws merge` 是原子的，不残留可识别状态。

---

## Clean 行为

`ws clean` 遍历当前项目所有 worktree（按 `workspaces_dir/{workspace_id}` 前缀过滤），按以下顺序判定：

1. 跳过 trunk worktree
2. 解析 effective target：`base_branch`（仍存在时）> trunk
3. 与 target 仍有差异 → 跳过
4. uncommitted > 0 → 报告并跳过（`Skipping {branch}: N uncommitted change(s)`）
5. `--dry-run` → 仅打印 "Would clean (no diff from {target})"
6. 真清：`remove_worktree(force=false)` + `delete_branch(force=false)` + 删 meta；如当前 cwd 在被清的 worktree 内，写 path_file 让 shell cd 回主仓库

最终汇总 cleaned/skipped_dirty 计数。

---

## Git 错误处理

`git/mod.rs` 中的 `extract_error()` 统一从命令输出提取错误信息：
- 优先使用 stderr（git 的常规错误输出）
- stderr 为空时 fallback 到 stdout（merge 冲突信息走 stdout）

适用于 `merge`、`commit`、`merge_continue` 等冲突相关命令。

---

## 配置文件

### 全局配置 `$AGENT_WORKSPACE_DIR/config.toml`（默认 `~/.agent-workspace/config.toml`）

```toml
[general]
merge_strategy = "squash"               # squash（默认） | merge
sync_strategy = "rebase"                # rebase（默认） | merge
# 从主仓库复制到新 worktree 的文件（通常是被 gitignore 但开发必需的），支持 glob
copy_files = ["*.secret.*"]

[hooks]
post_create = []
pre_merge = []
post_merge = []
```

### 配置合并规则

- `copy_files`：global + project **追加**合并
- `hooks`：project 非空时**完全替代** global（不追加）
- `merge_strategy` / `sync_strategy`：project 非空时**覆盖** global（`Option` 语义）
- `trunk`：仅 project 级别配置

### 项目配置 `.workspace.toml`

本地、按机器存储的文件，`ws` 会自动把它加入仓库的本地排除文件 `.git/info/exclude`（不是已提交的 `.gitignore`，因此无需提交、不弄脏工作区；该文件位于 common git dir，被主仓库与所有 worktree 共享，git 与 jj 都遵守）。无 `.workspace.toml` 时回退读取 legacy 的已提交 `.agent-workspace.toml`。三层合并，后者覆盖前者：全局 `config.toml` → 主 repo 根的 `.workspace.toml` → 当前 worktree 根的 `.workspace.toml`。`ws config` / `ws exclude` 写入 repo 级文件；worktree 级文件手工编辑。

```toml
[general]
trunk = "main"                    # 主干分支，默认自动检测
merge_strategy = "merge"          # 可选，覆盖全局策略
sync_strategy = "merge"           # 可选，覆盖全局同步策略
copy_files = [".env", ".env.*"]

[hooks]
post_create = ["pnpm install"]
pre_merge = ["pnpm test", "pnpm lint"]
```

### 配置约束与信任边界

- **路径解析**：repo 级配置从 `git rev-parse --git-common-dir` 上溯到主 repo 根读取——worktree/子目录任意位置行为一致；worktree 级配置用 `git rev-parse --show-toplevel` 定位当前 worktree 根，仅当其与主 repo 根不同才生效。两者均在 VCS backend 安装前用原始 `vcs_runner` 调用解析
- **`copy_files` 路径沙箱**：拒绝 `/` 开头（绝对路径）和 `..` 段；不跟随符号链接
- **hooks 安全**：hooks 通过 `sh -c`（Windows `cmd /C`）执行，无沙箱无超时——按"committed shell script"信任处理，禁运行不信任 repo
- **hook CWD**：`pre_merge`/`post_merge` 一律 worktree 根；`post_create` 在新 worktree 内
- **trunk 检测**：`origin/HEAD` > `main` > `master` > 默认 `"main"`

---

## 模块划分

技术选型：**Rust**——单二进制、无运行时依赖、跨平台、快速启动。

> 各文件职责见 `FILE_TREE.local.md`（单一真源）。本节仅列顶层组织：
>
> - `src/cli/` — Cli struct + Command 分发；`commands/` 按语义分 `nav/` `lifecycle/` `snap/` `sys/` 子模块 + 顶层独立命令
> - `src/git/` — repo / worktree / branch / ops 拆分，`mod.rs` 仅导出
> - `src/meta/` — `{branch}.toml` 元数据（兼容旧 `.status.toml`）+ target resolver
> - `src/config/` — Global/Project 合并；从 `git --git-common-dir` 读项目配置
> - `src/shell/` — wrapper 脚本生成与安装；snap 退出码契约（0/2/3）与 `snap/resume.rs` 同步
> - `src/process/` `src/prompt/` `src/update/` `src/util/` — 进程/交互/版本检查/分支名生成
> - `tests/` — 按命令分文件 + `common/mod.rs` 共享辅助
> - `npm/` — 主包 + 各平台二进制子包（postinstall 自动装 shell wrapper）
> - `scripts/` — 构建与发布脚本