# M2 计划：Issue / Comment / Inbox 三切片

> 本文件是 LUM-1346（13:30 autopilot cycle）的交付物之一。
> 前置条件：M1 集成完成 + M2 anchor scaffold 已落在 `feat/multica-rs-initial`
> （见 `docs/09-M1-INTEGRATION.md` §6）。三个 M2 分支都基于 scaffold 后的
> `feat/multica-rs-initial` 创建，完成后 merge 回该分支。
> 子任务文档编号：`docs/11-M2-ISSUE.md` / `docs/12-M2-COMMENT.md` / `docs/13-M2-INBOX.md`。

## 0. 上游路由清单（`server/cmd/server/router.go`，行号为 2026-09-22 快照）

### /api/issues（L1955–L2016）

| Method | Path | Handler |
| --- | --- | --- |
| GET | `/api/issues/limit-usage` | GetIssueLimitUsage |
| POST | `/api/issues/table/{groups,rows,facets}` | ListIssueTable{Groups,Rows,Facets} |
| GET | `/api/issues/search` | SearchIssues |
| GET | `/api/issues/child-progress` | ChildIssueProgress |
| GET | `/api/issues/children` | ListChildrenByParents |
| GET | `/api/issues/grouped` | ListGroupedIssues |
| GET | `/api/issues/` | ListIssues |
| POST | `/api/issues/query` | QueryIssues（GET / 的 POST 孪生，超大 filter 集） |
| POST | `/api/issues/` | CreateIssue |
| POST | `/api/issues/quick-create` | QuickCreateIssue |
| POST | `/api/issues/preview-trigger` | PreviewIssueTrigger |
| POST | `/api/issues/batch-update` \| `batch-delete` | BatchUpdate/DeleteIssues |

`/api/issues/{id}`（L1974–L2015）：`GET /`、`PUT /`、`POST /move`、`DELETE /`、
`comments`（POST/GET + `comments/trigger-preview`）、`timeline`、`subscribers` +
`subscribe`/`unsubscribe`/`unsubscribe/subtree`、`wakeups`（GET/POST/PUT + disable/enable/instruction）、
`active-task`、`tasks/{taskId}/cancel`、`rerun`、`quick-actions/{id}/{run,render}`、
`task-runs`、`usage`、`reactions`（POST/DELETE）、`attachments`、`children`、
`labels`（GET/POST/DELETE `{labelId}`）、`metadata`（GET/PUT/DELETE `{key}`）、
`properties`（PUT/DELETE `{propertyId}`）、`pull-requests`。

### /api/comments/{commentId}（L2159–L2174）

`sub-issue-preview`(GET, human)、`sub-issues`(POST, human)、`PUT /`、`DELETE /`、
`DELETE /keep-replies`、`POST/DELETE /resolve`、`POST/DELETE /reactions`。

（评论的创建/列表挂在 `/api/issues/{id}/comments`，见上表。）

### /api/issue-statuses（L2054–L2063）

`GET /`、`POST /`、`PATCH /reorder`、`PATCH /{id}`、`DELETE /{id}`。
读对任意 member 开放，写在 handler 内限定 owner/admin（上游注释 L2051）。

### /api/inbox（L2377–L2394）

`GET /`、`GET /archived`、`GET /archived/page`、`GET /archived/facets`、
`GET /unread-count`、`GET /unread-summary`（跨 workspace 账户级）、
`POST /mark-all-read`、`/archive-all`、`/archive-all-read`、`/archive-completed`、
`POST /{id}/{read,unread,archive,unarchive}`。

上游 handler 参考文件：`issue.go`（CreateIssue L2931 / UpdateIssue L3466 / ListIssues L1164 /
GetIssue L2345 / SearchIssues L1005 / QueryIssues L1150 / Grouped L1873）、
`comment.go`（ListComments L404 / CreateComment L1680）、
`issue_status.go`、`inbox.go` + `inbox_archive.go`。

## 1. schema 现状

`migrations/0001_init.up.sql` 已有：`issue`、`issue_status`、`comment`、`inbox_item`、`wakeup`。
**缺失**（上游有，需 M2 scaffold 补）：`comment_reaction`（上游 026）、`issue_reaction`（027）、
`issue_subscriber`（015）——已由 scaffold `0004_reactions_and_subscribers.up.sql` 补齐；
`attachment`（029）、`issue_properties`（191）留到 M2 尾部或 M3。

> **⚠️ 更正（2026-09-22 18:00，LUM-1368 cycle 实测）**：本节原本写 `issue_label` /
> `issue_to_label` 也存在，**这是错的**——本仓 `migrations/` 里没有这两张表
> （`grep -rni label migrations/` 为空）。详见 §5.1，M2-A 的 labels 路由因此不可实现。

## 2. 三切片划分

### M2-A：issue 核心 —— `feat/multica-rs-m2a-issue`

- Repo：`crates/mc-repos/src/issue.rs`（Row/NewIssue/UpdateIssue/Filter + Memory/Pg 双实现，
  与 M1 各 Repo 同约定）。
- 路由：`crates/mc-http/src/routes/issues.rs`，挂入 `mount_slice_issue()`。
- 覆盖：CRUD（GET/PUT/DELETE `/api/issues/{id}`）、`GET/POST /api/issues`、`query`、
  `search`、`grouped`、`children`、`child-progress`、`move`、`batch-update/delete`、
  `quick-create`（无 daemon 时按上游降级）、`/api/issue-statuses` 全部 5 条、
  `metadata` / `labels` / `properties`（表已存在，读写优先级低于 CRUD）、
  issue 级 `reactions`（表由 scaffold 建好）。
- 不做（占位保留，另立后续）：`table/{groups,rows,facets}`（issue table 查询面大，
  单独立 M2-D）、`preview-trigger` / `active-task` / `task-runs` / `rerun` /
  `tasks/{taskId}/cancel` / `usage` / `wakeups` / `pull-requests`（依赖 M3 任务队列与 M5 wakeup）、
  `timeline`（依赖 M9 activity log）、`attachments`。
- 测试：repo 单测 ≥5（create/get round-trip、number+identifier 唯一、`UNIQUE(workspace_id, number)`
  冲突、parent/children、status 迁移）；路由 e2e ≥4（POST→GET→PUT→DELETE、
  search 过滤、status catalog CRUD、并发 revision 冲突 409）。
- 文档：`docs/11-M2-ISSUE.md`。

### M2-B：comment + reactions —— `feat/multica-rs-m2b-comment`

- Repo：`crates/mc-repos/src/comment.rs`。
- 路由：`crates/mc-http/src/routes/comments.rs`，挂入 `mount_slice_comment()`。
- 覆盖：`POST/GET /api/issues/{id}/comments`（含 `parent_id` 线程回复）、
  `PUT/DELETE /api/comments/{commentId}`（软删 `deleted_at`）、`DELETE /keep-replies`、
  `POST/DELETE /api/comments/{commentId}/resolve`、评论级 `reactions`、
  `POST /api/comments/{commentId}/sub-issues`（把 `mention://issue` 展开建子 issue——
  通过 `IssueRepo` 协作，依赖 M2-A 的 trait 接口而非具体实现）。
- 不做：`trigger-preview`、mention 触发 agent 派单（`triggerTasksForComment` 一整套，
  依赖 M3 task queue）、`sub-issue-preview`（human-only，M3+）。
- 边界：**不修改** `routes/mod.rs` 的 `pub mod`（scaffold 已声明）、不碰 `mount_slice_*` 之外的切片。
- 测试：repo ≥5（create/list by issue、parent 链、resolve 幂等、软删可见性、reaction 增删幂等）；
  路由 e2e ≥3（发评论→列表→线程回复；resolve；速率/权限 403 用例）。
- 文档：`docs/12-M2-COMMENT.md`。

### M2-C：inbox + subscribers —— `feat/multica-rs-m2c-inbox`

- Repo：`crates/mc-repos/src/inbox.rs` + `subscriber.rs`（`issue_subscriber` 读写）。
- 路由：`crates/mc-http/src/routes/inbox.rs`，挂入 `mount_slice_inbox()`。
- 覆盖：`/api/inbox` 全部 **14** 条（含 `archived/*` 4 条、`unread-summary` 账户级；§5.3 有实测更正）、
  `/api/issues/{id}` 下 `subscribers` / `subscribe` / `unsubscribe` / `unsubscribe/subtree`
  （这 4 条挂在 `mount_slice_issue` 里会踩 M2-A 的文件——约定：这 4 条注册在
  `routes/subscribers.rs` 的独立 router 里，通过 `mount_slice_inbox()` 一并 merge，
  避免两个分支同文件；如 axum path 不允许，改由 master 集成时搬移并在 PR 注明）。
- 不做：notification-preferences（`/api/notification-preferences`，M9）、
  inbox item 的**产生**逻辑（由 comment/task 事件写入，依赖 M3——本切片只做 CRUD 面，
  item 写入用 repo 直接构造 + 测试夹具驱动）。
- 测试：repo ≥5（list/mark-read/archive 幂等、unread-count、archived 分页、facets、
  unread-summary 跨 workspace）；路由 e2e ≥3。
- 文档：`docs/13-M2-INBOX.md`。

## 3. 并发与晋升规则

- 三个 M2 分支互不共享可写文件（`routes/mod.rs`、`mount.rs` 的 `mount_slice_*` 由 scaffold 预留）；
  共享锚点只剩 `mount.rs::router()` 的 `.merge` 行——scaffold 一次性加三行，之后无人再动。
- **晋升顺序**：M1 集成 + scaffold 完成后，三个 M2 子任务可同时晋升为 `todo`
  （受 autopilot「最多 3 个并行任务」约束：晋升前先 `multica issue runs <LUM-1334> --siblings --active`
  确认空位）。
- 集成由 master（下一个 autopilot cycle 或 LUM-1342 复跑）执行，规则见 `docs/09-M1-INTEGRATION.md`。

## 4. 开工前置：环境与已知陷阱（2026-09-22 15:30 实测，LUM-1358 cycle）

M2 切片开工前必须知道这两条，都是 M1 切片实测踩到的：

1. **工具链**：`PATH` 上的 `/usr/bin/cargo` 是 1.75.0，**无法构建本仓库**
   （workspace 声明 `rust-version = "1.80"`）。可用的是 rustup stable **1.98.1**，在 `~/.cargo/bin`：

   ```bash
   export PATH="$HOME/.cargo/bin:$PATH"
   cargo --version   # 应为 1.98.1
   ```

   并发跑三个切片时 crates.io 索引会抢 `/home/devbox/.cargo/.package-cache` 锁；编译卡住时
   用 `ps aux | grep cargo` 判断是哪个 slice 占着，不要盲目重启 build。
2. **axum 0.7 路径参数写法**：`axum = "0.7"`（matchit 0.7）下 `{id}` 会被当成**字面量段**——
   编译通过、注册成功，但请求恒返 404。必须写 `:id`。M0 遗留的占位路由（`routes/mount.rs`
   里的 `/api/workspaces/{id}`、`/api/issues/{id}`）就是反例，M2 切片新增/接替路由时
   一并改掉（集成时的扫查命令见 `docs/09` §7.4）。
3. **不要重新修 M0 基线缺陷**：`056d2ae` 自身不编译（缺 `anyhow`/`dirs`/`tokio` 依赖等），
   M1 的三个切片各自重复修了一遍。M1-D 集成后基线即可编译，M2 切片从集成后的
   `feat/multica-rs-initial` 开分支，不会再遇到。

---

## 5. 覆盖缺口与 schema 事实更正（2026-09-22 18:00 CST，LUM-1368 cycle 实测）

实测方式：`git ls-remote` 核验远端 head；静态抽取 `fd6dfd6` 合并树全部 `.route(...)`；
与上游 `louloulin/multica` `origin/main` @ `f41fae6`（`server/cmd/server/router.go`，2610 行）逐条对账。

### 5.1 schema 事实更正（§1 有误，以本节为准）

| 表 | 本仓现状（`fd6dfd6`） | 上游 | 结论 |
| --- | --- | --- | --- |
| `issue_label` / `issue_to_label` | **不存在**（0001 的 24 张表里没有；全 `migrations/` 无 `label` 字样） | `001_init.up.sql:75`、`:82` | §1 原文写"已有"是**错的** |
| `issue_properties` | **不存在**；只有列 `issue.properties JSONB`（`0001:153`） | `191_issue_properties` | 值可存 JSONB，定义目录缺失 |
| `issue.metadata JSONB` | **存在**（`0001:152`） | 同 | metadata 读写可实现 |
| `comment_reaction` / `issue_reaction` / `issue_subscriber` | 存在（scaffold `0004`） | 026 / 027 / 015 | ✓ |

对三个切片的实际影响：

- `GET/PUT/DELETE /api/issues/{id}/metadata/{key}` → 用 `issue.metadata JSONB`，**可实现**。
- `PUT/DELETE /api/issues/{id}/properties/{propertyId}` → 值可落 `issue.properties JSONB`，但
  `/api/properties` 定义目录既无表也无路由 → **端到端不可用**，建议降级 TODO 并在 `docs/11` 注明。
- `GET/POST /api/issues/{id}/labels`、`DELETE /api/issues/{id}/labels/{labelId}` → **无表，不可实现**
  → 留 `501` / TODO。**不要在 M2 切片里新开迁移文件**（`0005` 编号由集成 master 统一分配）。

### 5.2 无 milestone 认领的上游路由（覆盖缺口）

| 上游路由块 | router.go 行号 | 条数 | 归属 |
| --- | --- | --- | --- |
| `/api/labels`（GET/POST `/`、GET/PUT/DELETE `/{id}`） | L2041–L2051 | 5 | **M2-E（LUM-1370，backlog）** |
| `/api/properties`（GET/POST `/`、GET/PATCH `/{id}`） | L2031–L2039 | 4 | **M2-E（LUM-1370，backlog）** |
| `/api/quick-actions`（GET/POST `/`、PATCH/DELETE `/{id}`） | L2021–L2029 | 4 | autopilot 域（M3+，未立项） |
| `POST /api/issues/{id}/quick-actions/{quickActionId}/{run,render}` | L1995–L1996 | 2 | 依赖 task queue（M3+，未立项） |

注：M2-A 只覆盖 issue-**从属**的 `/api/issues/{id}/labels`、`/properties`，上游的**定义目录**
（`/api/labels`、`/api/properties`）在 §0／§2 里从未出现——这是本计划的覆盖盲区，已单独立 **LUM-1370（M2-E）**。

### 5.3 计数更正与实测基线

- §2 M2-C 写"`/api/inbox` 全部 **15** 条" → 实际 **14 条**（§0 表格是对的）：`GET /`、`archived`、
  `archived/page`、`archived/facets`、`unread-count`、`unread-summary`、`mark-all-read`、`archive-all`、
  `archive-all-read`、`archive-completed`、`{id}/read`、`{id}/unread`、`{id}/archive`、`{id}/unarchive`。
  加上 4 条 subscriber 路由，M2-C 覆盖面 = **18** 条（不是 19）。
- `/api/issue-statuses` 上游 5 条与 §0 一致：`GET /`、`POST /`、`PATCH /reorder`、`PATCH /{id}`、`DELETE /{id}`。
- `fd6dfd6` 静态路由表实测：**(method, path) 52 条，无同 path+method 重复，无 `{param}` 字面量段**。
  §4 第 2 条举的 M0 占位反例（`/api/workspaces/{id}`、`/api/issues/{id}`）已不存在——
  workspace 占位被 M1-A 真实路由替换，`/api/issues`、`/api/issues/{id}`、`/api/comments`、`/api/inbox`
  占位由 M1-D 删除（M2 切片**无需**再删占位行）。

---

## 6. 切片验收门禁增补 + 分支实况（2026-09-22 19:00 CST，LUM-1373 cycle 实测）

### 6.1 `cargo fmt --all --check` 是必须跑的一道门禁（M2-C 漏了）

独立 checkout 实测：base **`69e9f4b` 的 `cargo fmt --all --check` = exit 1，54 处差异**，
全部落在 M2-C 的 5 个文件（`tests/inbox.rs` 27、`routes/inbox.rs` 11、`mc-repos/inbox.rs` 8、
`mc-repos/subscriber.rs` 7、`routes/subscribers.rs` 1）——即 **M2-C 是以 fmt 不干净的形态并入 base 的**。
M1-E（LUM-1362）以 `9ec5b56 chore(fmt)` 补齐，该 revision `fmt --check` = exit 0，且 `git diff -w` 证明
其差异仅为 rustfmt 换行/尾逗号，无逻辑改动。

**规程**：§2 各切片与集成切片 LUM-1354 的验收必须包含 `cargo fmt --all --check`；
M2-A / M2-B 在提交前先跑一次，不要把这个欠账留给下一个切片（这次是 M1-E 替 M2-C 还的）。

### 6.2 分支名实况（计划名 ≠ 实际名）

实测：M2-B（LUM-1350）落在 `multica repo checkout` 自动生成的 `agent/devbox5/28f8edc92edd` 上，
**不是** §2 写的 `feat/multica-rs-m2b-comment`。M1-E / M2-A 用的分别是
`feat/multica-rs-m1e-contract-gaps` / `feat/multica-rs-m2a-issue`（符合约定）。
集成方（LUM-1354）不要去按计划名找分支，先 `git branch --show-current` 核工作区、
`git log --oneline origin/feat/multica-rs-initial..<分支>` 核提交。

### 6.3 集成手册

跨切片冲突矩阵、五道门禁、重复路由静态扫查、合并顺序与冲突预案、blobby `git grep` 环境警示，
全部整理在 **`docs/21-M2-INTEGRATION-RECIPE.md`**（19:00 cycle 实测快照），集成前先读它。
