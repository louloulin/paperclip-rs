# 上游 multica 代码分析配方（含一个已验证的死锁失败模式）

> 本文件是 LUM-1371（18:30 autopilot cycle）的交付物。
> 起因：2026-09-22 09:42 起 M2-A（LUM-1348）与 M2-B（LUM-1350）两个切片**同时卡在一条
> `git grep` 上 50–65 分钟**，吃掉 3 个并发位中的 2 个。根因不是网络，是「blobby 仓库 + `git grep`」
> 这个组合。下面每条数字都是本次在本机实测的。

## 1. 结论

分析上游 `louloulin/multica` 时：

- **要 grep / diff / log -p / blame → 必须是「全 blob 浅克隆」**：
  `git clone --depth 1 --single-branch --branch main https://github.com/louloulin/multica <dir>`
  （实测 **5.4s / 96MB**，之后整仓 `git grep` **0.023s**）。
- **只要文件名清单 / 路由表 → blobby 克隆够用**，`git ls-tree -r --name-only FETCH_HEAD` 实测 **5ms**。
- **只要单个文件 → `git show FETCH_HEAD:server/cmd/server/router.go`**。
- **绝对不要**在 `git init` + `git fetch --filter=blob:none` 建出来的「裸 blobby」仓库上跑 `git grep`。

## 2. 症状（实测证据）

| 观测项 | 实测值 |
| --- | --- |
| 卡住的进程 | `git grep -n AddIssueReaction FETCH_HEAD -- server` PID 9931，elapsed **49:41**（采样时）→ 65 分钟附近才返回 |
| 另一条 | `git grep -n CreateCommentSubIssue FETCH_HEAD -- server` PID 3650，elapsed **55:24** → 约 65 分钟返回 |
| 进程状态 | `STAT=Sl`，`WCHAN=futex_wait_queue_me`（在等自己的懒取子进程，不是在等锁） |
| 累计 I/O | `rchar` **1.73 GB / 1.94 GB**，`syscr` **199 万 / 222 万**（反复读同一个 pack） |
| 父链 | `bash -c ← pi ← /usr/local/bin/multica daemon start`（即 agent 的 tool call，不是孤儿进程） |
| 网络健康度 | `git ls-remote https://github.com/louloulin/multica HEAD` = **1.253s** → 网络完全正常 |
| 仓库形态 | `/tmp/ups/multica` 仅 7.2M，`remote.origin.partialclonefilter=blob:none`，`remote.origin.promisor=true` |
| 取对象速度 | `in-pack` 3027 → 3041（20s）≈ **0.7 obj/s** |
| 待取规模 | `server/` 下 **3104** 个文件（全仓 6091）→ ETA ≈ **74 分钟/次 grep** |

## 3. 根因

`git init` + `git fetch --depth 1 --filter=blob:none` 只拿到 commit + tree，**一个 blob 都没有**。
`git grep` 必须读文件内容，于是对每个缺失 blob 触发一次 promisor **懒取**：每次一个小 HTTP 往返
（实测 ~1.4s/对象），还伴随 `Auto packing the repository in background`。于是：

- 同一台机器、同一个远端：全 blob 克隆后 grep **0.023s**；裸 blobby 仓库 grep **90s 都跑不完**（被 `timeout` 杀掉）。
- 两条 grep 各占一个并发位 → M2 三切片里有 2 个在 65 分钟内零文件产出（`workdir` 里除 checkout 外无任何新文件）。

对照实验（本次实测，同一网络同一时刻）：

| 做法 | 建仓耗时 | 仓库占用 | `git grep -- server` |
| --- | --- | --- | --- |
| `git clone --depth 1 --single-branch`（全 blob） | **5.382s** | 96M（pack 21.11 MiB / 6535 objects） | **0.023s** |
| `git init` + `fetch --depth 1 --filter=blob:none`（裸 blobby） | 2.149s | 392K | **>90s 未完成**（timeout 124） |
| 上者 + 本文 §4 救援 | +**5.435s** | 22M | **0.025s** |
| `git clone --filter=blob:none`（带 checkout） | 14.566s | 97M | 可用（checkout 已经把 blob 拉全了） |

> 注意最后一行：`git clone --filter=blob:none` 带工作区 checkout 时，checkout 会把所有 blob 懒取回来
> （97M，和全量一样），所以它**不是**危险组合；危险的是 `git init` + `fetch --filter=blob:none`
> 这种「只有 tree、没有工作区」的仓库。别把这一条误读成「filter 安全」。

## 4. 已建好的 blobby 仓库怎么救活（本次实测，5.4s，且不杀任何进程）

```bash
dir=<blobby 仓库路径>
git -C "$dir" config --unset remote.origin.partialclonefilter
git -C "$dir" fetch --refetch --depth 1 origin main     # 一次性把全部 blob 拉进本地 pack
# 校验：应当瞬间返回
git -C "$dir" grep -n "AddIssueReaction" FETCH_HEAD -- server
```

本次对两个卡死仓库（`/tmp/ups/multica`、`.../lum-1350-.../workdir/upstream/multica-src`）执行上面三条后：

- 仓库从 7.2M / 7.6M 涨到 **29M / 30M**（blob 齐了），`git grep` 由「卡死」变 **0.028–0.030s**；
- **正在跑的 `git grep` 自己返回了**（对象已在本地，懒取不再需要网络），两个 PID 随即消失，
  两个被阻塞的 tool call 解封 —— 不需要 kill 任何进程，也不需要重跑命令。

## 5. 排障速查（下次再见到「agent 卡住」先跑这三条）

```bash
# 1) 卡在哪：看 elapsed / wchan / cmd
ps -eo pid,ppid,etime,stat,wchan:20,cmd | grep -E "git grep|git fetch|git-remote-https" | grep -v grep
# 2) 是不是 promisor 仓库
git -C "$dir" config --get remote.origin.partialclonefilter   # 输出 blob:none 就是它
# 3) 有没有在进展：20 秒内 in-pack 增长多少
git -C "$dir" count-objects -v | grep in-pack
```

## 6. 明确不要做

- 不要在 blobby 仓库上跑 `git grep` / `git log -p` / `git diff` / `git blame` / `git archive`。
- **卡住时不要重跑同一条命令**——它会再吃一个并发位；先按 §5 判断，再按 §4 救活。
- 不要 kill 别的 run 的进程（本次修法完全不需要），也不要 `rm -rf` 别人的 workdir 缓存。
- 不要为了「省流量」而选 `--filter=blob:none`：本仓实测全量浅克隆只要 **5.4s / 21MB pack**，
  省下来的那点流量换来的是 74 分钟/次的死锁风险。

## 7. 顺带复核到的 M2 合同（行号基于 `f41fae6` 快照）

`docs/10-M2-PLAN.md` §0 已列出路由，这里只补「同一快照下的行号 + 中间件」：

| 上游路由 | 位置 | 备注 |
| --- | --- | --- |
| `POST /api/comments/{commentId}/sub-issues` → `h.CreateCommentSubIssue` | `server/cmd/server/router.go:2161` | 外层 `r.With(handler.RequireHumanActor)` |
| `GET /api/comments/{commentId}/sub-issue-preview` → `h.PreviewCommentSubIssue` | `server/cmd/server/router.go:2160` | 同样 `RequireHumanActor` |
| `RequireHumanActor` 定义 | `server/internal/handler/actor_guards.go:96` | 机器身份必须被 403 |
| `POST` / `DELETE /api/issues/{issueId}/reactions` → `AddIssueReaction` / `RemoveIssueReaction` | `server/cmd/server/router.go:1999–2000` | 在 `/api/issues/{issueId}` 组内 |
| `CreateCommentSubIssue` 实现 | `server/internal/handler/source_context.go:437` | 上游**没有** `*sub_issue*` 命名的文件，别按文件名找 |

## 8. 环境快照

- `louloulin/multica` main = `f41fae6b08fb734afcbd13205c0b3203dd0bc9c6`（2026-09-22 实测）。
- 本机：`git ls-remote` 1.25s；`/tmp/up1371/full` 为本次对照用的全量浅克隆。
- 本文所有耗时均为 `time` 实测，未跑 cargo（避免与 M2 三切片的 build 抢 `~/.cargo/.package-cache`）。

## 9. 路由快照的生成（LUM-1376 起）

上游 `(method, path)` 全量清单不再手抄，改为从快照仓库一条命令生成，并成为 `scripts/route_parity.py`
对账的"上游那一半"：

```bash
git clone --depth 1 --single-branch --branch main https://github.com/louloulin/multica /tmp/up-multica
python3 scripts/gen_upstream_routes.py \
  --router     /tmp/up-multica/server/cmd/server/router.go \
  --const-file /tmp/up-multica/server/pkg/publicapi/v1/routes.go \
  --owner-rules scripts/route-owners.tsv \
  --out docs/fixtures/upstream-routes.tsv \
  --commit f41fae6b08fb734afcbd13205c0b3203dd0bc9c6
```

命令记录在 fixture 头部；`--check` 可做漂移检查（只比对、不写文件）。它展开
`r.Route(...)`/`r.Group(...)` 前缀、内联 `registerPluginActionRoutes(...)`（`/v1` 与
`/api/plugin-bridge/v1` 两个前缀），并从 `--const-file` 解析 `publicapiv1.*` 常量。完整规程（owner 词表、
刷新流程、能力边界）见 **`docs/22-ROUTE-PARITY.md`**。
