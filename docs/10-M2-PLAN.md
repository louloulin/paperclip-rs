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

`migrations/0001_init.up.sql` 已有：`issue`、`issue_status`、`comment`、`inbox_item`、
`issue_label`、`issue_to_label`、`wakeup`。**缺失**（上游有，需 M2 scaffold 补）：
`comment_reaction`（上游 026）、`issue_reaction`（027）、`issue_subscriber`（015）；
`attachment`（029）、`issue_properties`（191）留到 M2 尾部或 M3。

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
- 覆盖：`/api/inbox` 全部 15 条（含 `archived/*` 4 条、`unread-summary` 账户级）、
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
