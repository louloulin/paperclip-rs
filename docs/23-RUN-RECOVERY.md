# 运行中断与工作区抢救（daemon restart playbook）

> 实测时间：**2026-09-22 19:35–19:41 CST**（autopilot 19:30 cycle，LUM-1377）。
> 实测方式：只读 —— `multica issue runs` / `multica daemon status` / `multica issue run-messages`，
> 以及对旧、新两代工作区做 `git status --short`、`git diff --stat`、`git diff | git hash-object --stdin`。
> 本轮**没有**写任何其他 run 的工作区（原因见 §4）。
>
> 适用面：所有并发切片（M1/M2/M3），不限于 M2；集成切片 LUM-1354 开跑前先读一遍 §2–§4。

---

## 1. 事件事实（2026-09-22 UTC）

daemon 在 **11:35:34Z** 重启（`multica daemon status`：`pid 39`、`uptime` 仅数分钟、
`active_task_count 5`），三个在飞 run 被同时判死，同一秒起自动重试（`attempt 2`）：

| 切片 | issue | 被中断的 run（attempt 1） | 起跑 | 重试 run（attempt 2） | 新工作区 |
| --- | --- | --- | --- | --- | --- |
| M2-A issue 核心 | LUM-1348 | `01a0c876-…-415787f356d2` | 09:33:32Z（**2h02m**） | `01a0c8e6-…-36a319e516ba` | `lum-1348-36a319e516ba` |
| M1-F PAT 持久化 | LUM-1375 | `01a0c8d2-…-0cef3da58917` | 11:14:17Z（21m） | `01a0c8e6-…-89dc20c1d9ce` | `lum-1375-89dc20c1d9ce` |
| T1 路由对账工具 | LUM-1376 | `01a0c8d2-…-5237cd988519` | 11:14:17Z（22m） | `01a0c8e6-…-18d4dc3219e2` | `lum-1376-18d4dc3219e2` |

三个 run 的终止字段完全一致，可作为「是否发生过重启」的判据：

```
status         = failed
error          = "daemon restarted while task was in flight"
failure_reason = runtime_recovery
max_attempts   = 2          # 同一秒（11:35:34Z）自动派发 attempt 2
```

被中断时落在旧工作区里、**尚未提交**的工作量（实测）：

| 切片 | 旧工作区（`lum-<n>-<旧 task-id>/workdir/paperclip-rs`） | 分支 @ head | 未提交改动 | `git diff` 哈希 | 恢复模式（§2.1） |
| --- | --- | --- | --- | --- | --- |
| M2-A | `lum-1348-415787f356d2` | `feat/multica-rs-m2a-issue` @ `fd6fdf6` | 3 文件 **+4134 −13** + 2 个未跟踪新文件（`mc-repos/src/issue_status.rs`、`mc-http/tests/issues.rs`） | `5bc95fef` | **B**（原地继续） |
| M1-F | `lum-1375-0cef3da58917` | `feat/multica-rs-m1f-pat-persistence` @ `9851ebf` | 5 文件 **+705 −194** | `a6ed4b44` | A（补丁重放，✓ 哈希一致） |
| T1 | `lum-1376-5237cd988519` | `feat/multica-rs-tooling-route-parity` @ `9c57592` | 7 文件（含 5 个新文件），补丁 98,821 B | `a4880bec`（staged） | A（补丁重放，✓ 哈希一致） |

---

## 2. 恢复机制的真相：平台不救，得自己救

1. **重试 run 拿到的是全新空工作区**：`lum-<issue#>-<新 task-id>/workdir/`，里面只有 `AGENTS.md`，
   既没有仓库、也没有上一次 run 的任何上下文（prompt / 会话 / 摘要都不带旧路径）。
2. **旧工作区仍在磁盘上**，未提交改动原封不动（本次三个都在）。
3. **平台既不注入旧路径、也不提醒去抢救**。本次是 T1 的重试自己 `ls ~/multica_workspaces/<ws>/`
   发现旧目录的（`multica issue run-messages <run>` seq 39 → 50 → 106 → 113），随后生成补丁重放。
   也就是说：**没人主动做 §3，那 2 小时就是静默丢失**（旧工作区不会被 GC，但会一直没人看）。
4. 因此规程是硬的：`multica issue runs <issue>` 出现 `runtime_recovery` 的切片，
   **重试 run 的第一件事是 §3 抢救，然后才是写新代码**。

### 2.1 两种恢复模式（本次三条切片各占其一，都实测到了）

| 模式 | 做法 | 本次实例 | 判据 |
| --- | --- | --- | --- |
| **A. 补丁重放** | 在旧 workdir 生成补丁 → 在新 workdir `apply --index`，之后在**新**目录干活 | M1-F（LUM-1375）、T1（LUM-1376） | 新 workdir 有 checkout，且两侧 `git diff --cached` 哈希相同（本次 `a6ed4b44` / `a4880bec`） |
| **B. 原地继续** | 直接 `cd` 到旧 workdir 接着干活（不新建 checkout） | M2-A（LUM-1348） | 重试自己的 `lum-1348-36a319e516ba/workdir/` 里**始终只有 `AGENTS.md`**，而旧 workdir 的文件 mtime 在持续变新（`multica issue run-messages <重试 run>` seq 140/142 可见它 `ls ~/multica_workspaces/` 后切过去） |

模式 B 的两个额外后果（对集成和协调 cycle 都重要）：

- 旧 workdir 从「静态恢复源」变成**活的工作区**，里面每一份内容都在动——任何外部快照都只能拍到某个瞬间。
  所以不要去建「保险用的 wip 快照分支」：本 cycle 试过（读到 11:41:56 的内容），几分钟后文件就有新改动、
  与旧工作区不再一致（`crates/mc-http/tests/issues.rs` 1047 → 1062 行），只能丢弃。
  真正可靠的动作是**只读核对 + 把结论写进文档**，抢救留给重试 run 自己。
- 活工作区分支还停在**旧 base**（M2-A：`feat/multica-rs-m2a-issue` @ `fd6fdf6`，早于当前 base 的三个 docs 提交），
  集成时要照 `docs/21` §7 先 `fetch` 再 rebase/合并，不能假设它已在当前 base 上。


---

## 3. 抢救步骤（本次实测跑通的原样命令）

```bash
WS=~/multica_workspaces/lumos-659117e3ca3d
PRIOR=$WS/lum-1376-5237cd988519/workdir/paperclip-rs   # 旧（被中断的）workdir
MINE=$WS/lum-1376-18d4dc3219e2/workdir                 # 本次重试的 workdir

# 1) 在旧 workdir 把所有改动落进 index（含未跟踪文件），只生成补丁、不 commit
cd "$PRIOR" && git add -A && git diff --cached --binary > "$MINE/recover.patch"

# 2) 把旧 workdir 从原分支摘下（detach），文件与 index 都保留：
#    后续任何分支操作都不会再把这批改动当"本分支已提交"而清掉
git -C "$PRIOR" switch --detach

# 3) 在新 workdir 切到计划分支（base = 最新 feat/multica-rs-initial）后 --index 应用
cd "$MINE/paperclip-rs" && git fetch origin --quiet
git checkout -B feat/multica-rs-tooling-route-parity 9c57592
git apply --index "$MINE/recover.patch"
```

**必须校验（三项都做）**：

1. **哈希对账**——新旧改动必须逐字节一致，这是本次用的证明方式：
   ```bash
   git -C "$PRIOR" diff --cached HEAD | git hash-object --stdin   # 旧
   git -C "$MINE/paperclip-rs" diff --cached HEAD | git hash-object --stdin  # 新
   ```
   本次实测：M1-F 两侧 `a6ed4b44`（5 文件 +705 −194）、T1 两侧 `a4880bec`（7 文件）**相同** → 恢复完整。
2. **`grep -c 'new file mode'`**——补丁里必须出现新增文件。只跑 `git diff`（不先 `git add`）
   会**漏掉全部未跟踪新文件**：本次三个切片各有 2 / 0 / 5 个新文件，漏一个就是「恢复了一半」，
   而 `git status` 看起来还挺正常。
3. **`git status --short` 第一列是 `M` / `A`**（已暂存、第二列为空）→ index 已建好，可以直接继续改。
   若是旧 workdir 的 `git status` 显示 `??`，说明第 1 步没 `add -A`。

---

## 4. 硬规则：不要写别人的 workdir

恢复源就是旧 workdir 的**工作区 + index**，任何写操作都可能让它变空：

- `git checkout .` / `git restore .` / `git clean -fd` / `git stash` / `git commit`（把改动挪进提交、
  工作区清空）→ 后来者的补丁直接变成 0 字节，而且是**静默**的（没人会再看一眼旧目录）。
- 本次 19:30 cycle 里本 cycle 一度计划去那三个旧 workdir「提交 WIP 分支以抢救」，
  **那正好会毁掉三个重试的恢复源**（发现重试已经在做恢复后放弃，改为只读核对 + 写本文件）。
- 检查他人工作区一律只读：`git status` / `git diff --stat` / `ls` / `git rev-parse`。
  真要落盘，把补丁拷到自己 cycle 的 checkout 里重放，别在原处动 git 写命令。
- 唯一例外：**旧工作区的主人（重试 run 自己）** 按 §3 做 `add -A` + `switch --detach` 是预期动作；
  它做完后旧目录会停在 `detached HEAD @ <base>` + 全量 staged 的状态——别再把它当"干净目录"去清理。

---

## 5. 恢复之后仍然要走门禁

恢复是「搬迁」，不是「验证」：diff 哈希相同只说明改动一份不少，不说明它能编译、能过测试。

- 三个切片恢复后照样按 `docs/21-M2-INTEGRATION-RECIPE.md` §4 的五道门禁（fmt / clippy / build /
  test / 路由扫查）自测，PR 描述里注明「本切片经历过 runtime_recovery 并已按 docs/23 恢复 + 重测」。
- 集成方（LUM-1354）额外做一次算术对账：分支的 `git diff --stat <base> <branch>` 应与
  §1 表格里旧工作区的未提交 diffstat **同量级**；差得远就说明恢复只做了一半。

---

## 6. 运维建议

1. **切片在跑时别重启 daemon**。本次代价 ≈ **2 小时 45 分**并行算力（M2-A 2h02m + M1-F 21m + T1 22m），
   其中 M2-A 正是关键路径（M2-A → LUM-1354 集成）。
2. 重启后立即按 §1 的两条判据逐 issue 扫一遍 `multica issue runs`（`failed` + `runtime_recovery` +
   同秒 `attempt 2`），确认旧工作区还在同一台机器上（跨机器或清盘之后**无解**）。
3. 抢救动作要落在**重试 run 自己**身上最稳（它有 issue 上下文、知道计划分支名与 base）；
   协调 cycle 只做只读核对 + 把结论写进文档，不建快照分支、不在旧 workdir 里动 git 写命令（§2.1 有实测教训）。
4. 协调 cycle 能做且值得做的两件事：(a) 用 `multica issue run-messages <重试 run>` 确认它**真的在恢复**
   （模式 A/B 的判据见 §2.1），而不是从零重做；(b) 对模式 B 提醒集成方「活工作区停在旧 base」。
