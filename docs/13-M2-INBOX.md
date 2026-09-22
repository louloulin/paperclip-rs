# M2-C：inbox 仓储 + `/api/inbox` 全套路由 + issue subscribers（LUM-1349）

本文件记录 M2-C 切片的**实测产出**与**与上游的有意偏离**。上游 = `louloulin/multica`
`main`（`server/cmd/server/router.go`、`server/internal/handler/{inbox,inbox_archive,subscriber}.go`、
`server/pkg/db/queries/{inbox,inbox_archive,subscriber}.sql`，2026-09-22 抓取）。

## 1. 产出文件

| 文件 | 内容 |
|---|---|
| `crates/mc-repos/src/inbox.rs`（1159 行） | `InboxRepo`：list / 归档视图 / 计数器 / 批量与单条状态迁移 |
| `crates/mc-repos/src/subscriber.rs`（459 行） | `IssueSubscriberRepo`：subscribe / unsubscribe / 子树退订 / 查询 |
| `crates/mc-http/src/routes/inbox.rs`（967 行） | 14 条 `/api/inbox*` 路由 + DTO + 过滤/游标解析 |
| `crates/mc-http/src/routes/subscribers.rs`（378 行） | 4 条 `/api/issues/{id}/…` 订阅路由 |
| `crates/mc-http/tests/inbox.rs`（945 行） | 1 条无库路由守卫 + 4 条 DB e2e |

`mount.rs` / `routes/mod.rs` / `state.rs` / `main.rs` / 各 `Cargo.toml` **零改动**：
M2 anchor scaffold（LUM-1347）预留的 `mount_slice_inbox()` / `mount_slice_subscriber()`
与本文件的两个 `router()` 直接对接，避开了与 M2-A / M2-B 的锚点文件三方冲突。

## 2. 路由映射（14 + 4，逐条对齐上游）

| method | path | 上游 handler | 本仓 |
|---|---|---|---|
| GET | `/api/inbox`（及 `/api/inbox/`） | `ListInbox` | `list_inbox` |
| GET | `/api/inbox/archived` | `ListArchivedInbox` | `list_archived` |
| GET | `/api/inbox/archived/page` | `ListArchivedInboxPage` | `list_archived_page` |
| GET | `/api/inbox/archived/facets` | `GetArchivedInboxFacets` | `get_archived_facets` |
| GET | `/api/inbox/unread-count` | `CountUnreadInbox` | `count_unread` |
| GET | `/api/inbox/unread-summary` | `UnreadInboxSummary` | `unread_summary` |
| POST | `/api/inbox/mark-all-read` | `MarkAllInboxRead` | `mark_all_read` |
| POST | `/api/inbox/archive-all` | `ArchiveAllInbox` | `archive_all` |
| POST | `/api/inbox/archive-all-read` | `ArchiveAllReadInbox` | `archive_all_read` |
| POST | `/api/inbox/archive-completed` | `ArchiveCompletedInbox` | `archive_completed` |
| POST | `/api/inbox/{id}/read` | `MarkInboxRead` | `mark_read` |
| POST | `/api/inbox/{id}/unread` | `MarkInboxUnread` | `mark_unread` |
| POST | `/api/inbox/{id}/archive` | `ArchiveInboxItem` | `archive_item` |
| POST | `/api/inbox/{id}/unarchive` | `UnarchiveInboxItem` | `unarchive_item` |
| GET | `/api/issues/{id}/subscribers` | `ListIssueSubscribers` | `list_subscribers` |
| POST | `/api/issues/{id}/subscribe` | `SubscribeToIssue` | `subscribe` |
| POST | `/api/issues/{id}/unsubscribe` | `UnsubscribeFromIssue` | `unsubscribe` |
| POST | `/api/issues/{id}/unsubscribe/subtree` | `UnsubscribeFromIssueSubtree` | `unsubscribe_subtree` |

`/api/inbox` 同时注册了带尾斜杠的变体：上游是 chi `Route("/api/inbox") + Get("/")`，
两种写法命中同一 handler。axum 0.7（matchit 0.7）把两者视为不同节点，不会 panic（守卫：单元测试
`routes::inbox::tests::mounted_router_builds_without_route_conflict` + e2e
`tests/inbox.rs::route_paths_are_mounted`）。

> 计数口径：`docs/10-M2-PLAN.md` §5.3 的“M2-C 覆盖面 = 18 条”指 **18 条上游路径**（14 + 4）；
> 下面的 e2e 守卫断言的是 **19 条注册**，多出来的一条就是 `/api/inbox/` 别名。

**注意**：axum 0.7 的路径参数是 `:id`。写成上游文档风格的 `{id}` 会把整段当字面量、恒 404
——`tests/inbox.rs` 的第一条测试正是为了防这个回归：19 条路径在"缺用户头 / 缺 workspace / 坏
workspace uuid"下必须分别给出 401 / 400 / 400，而不是兜底 404。

## 3. 仓储层语义要点（与上游 SQL 逐条对齐）

- **组（group）= `COALESCE(issue_id, id)`**：issue 的通知按 issue 聚合，无 issue 的通知自成一
  组。归档 / 归档视图 / facets / `archive-all-read` 全部以组为粒度（`NEWEST_ARCHIVED_CTE`，
  `inbox.rs:61`）。因此：
  - 单条 `archive` / `unarchive` 会连带同 issue 的兄弟行（issue 级）；
  - 归档视图每组只回"最新一条"，且**有活跃行的组整体不出现**；
  - `archived/page` 的 `group_id` 过滤，对有 issue 的组要用 **issue id**（`= group_id OR
    (issue_id IS NULL AND id = group_id)`）。
- **`archive-all-read` 不是"归档读过的行"**：先取每组最新行，最新行已读 → 整组归档；未读组
  一行不动（否则归档最新行后旧兄弟会重新冒出来）。SQL 结构与上游 `inbox.sql:182` 一致，
  注释里保留了上游的理由说明。
- **幂等**：`read` / `unread` / `archive` / `unarchive` 用 `COALESCE(…, now())` 或 `SET NULL`
  加 `guard`（`read_at IS NULL` / `archived_at IS NULL` 等）实现，重复调用不改变时间戳、
  不重复计数。
- **终结状态**（`archive-completed`）：本 workspace `issue_status.category = 'closed'` 的 key
  ∪ 内置 `done` / `cancelled`（`inbox.rs:654`）。上游由 `issuestatus.ExpandCategories(done,
  closed)` 提供，本仓 0001 的 `issue_status.category` CHECK 只有 `open` / `closed`，故显式并
  上内置 key。
- **未读计数两种粒度**：`unread-count` 数**行**；`unread-summary` / `archive-all-read` 数**组**，
  且 `unread-summary` 只统计调用者仍是成员的 workspace（`JOIN member`，同上游）。
- **列表 body 预览**：只有"有 issue 的 `new_comment`"截断，按**字符**计数，200 上限**含**省略号
  （199 字符 + `…`），`list_body_preview`（`inbox.rs:689`）与上游 `inboxListBody` 逐字对应；
  单条变更接口返回**完整** body。
- **游标**：`{"time","id","scope"}` JSON，外层 hex 编码；`scope` = sha256(workspace|user|
  statuses|priorities|actors|unread_only|group_id)，换过滤条件续页 → 400 `invalid archive cursor`。
  上限 2048 字符、`limit` ∈ [1,100]（默认 50）、过滤器取值 ≤100 个 / ≤8192 字节、`unread_only`
  只接受 `true`/`false` —— 全部与上游逐字一致，错误文案也一致。

## 4. 与上游的有意偏离（请 master 逐条确认）

| # | 项 | 上游 | 本仓 | 影响 / 后续 |
|---|---|---|---|---|
| D1 | 列表分页 | 一次返回全部活跃行 | `?limit`（默认 200，上限 500）`&offset` | 大收件箱不再无限返回；超 200 条需要客户端翻页。**这是本切片唯一的契约增项**，超出 200 条时响应会少于上游 |
| D2 | 游标编码 | base64(RawURL) JSON | hex JSON | 对客户端仍是不透明串；但**上游发的游标不能拿到本仓续页**。hex 是为避免给 `mc-http` 加 `base64` 依赖 |
| D3 | workspace 解析 | `X-Workspace-ID` / `?workspace_id` / `X-Workspace-Slug` / `?workspace_slug` / task token | 只支持 `X-Workspace-ID` 与 `?workspace_id` | slug 解析与 task-token 分支**未实现**，属 M3；上游用 slug 的客户端需改传 id |
| D4 | 主体解析 | `X-Actor-Source: task_token` → agent 身份 | 恒为 `user`（`AuthUser` from `X-Multica-User-Id`） | agent 以自己的身份订阅需等 M3；body 里显式 `user_type: "agent"` **已经能用** |
| D5 | 退订 | 保留 tombstone（`unsubscribed_at`），阻止自动重订阅 | **硬删除** | 退订不留痕、重新订阅总成功；子树退订不会阻止未来新增子 issue 的自动订阅。需 M3 给 `issue_subscriber` 加 tombstone 列（0004 未预留） |
| D6 | `severity` | 真实分级 | 恒 `"info"` | 本仓 `inbox_item` 无该列 |
| D7 | `details` | 归档视图会合并 `details.comment_id` 等 | 恒 `{}` | 本仓无 comment 锚点列；M2-B 若能给出 comment 关联再补 |
| D8 | `recipient_type` / `recipient_id` / `type` | 列名语义 | 由 `user_id` / `category` 映射，`recipient_type` 恒 `"user"` | 本仓词汇表是 `user`（不是 `member`），与 0001/0004 CHECK 一致 |
| D9 | 错误体 | `{"error":"msg"}` | `{"error":{"code","message"}}`，且 `NotFound` 文案是 `"not found: {resource}"` | M1 既有约定，全仓库统一；**状态码逐条对齐**（400/401/403/404） |
| D10 | realtime | 每次变更 `publish(EventInbox*, …)` | 不发事件 | 与 M1 各路由一致（`RealtimeHandle` 尚未接入任何路由）；M3 补 |
| D11 | 列表顺序/字段 | 上游 SQL 排序 | 同 `created_at DESC, id DESC` | 无偏离，仅记录 |
| D12 | `unread-summary` 的 workspace 上下文 | 路由在 `RequireWorkspaceMember` 组内 | 同样要求可解析 workspace + 成员身份 | 查询本身是账户级，但缺 workspace 上下文仍 400（与上游一致） |

另外：`/api/inbox` 系列在上游位于 `RequireWorkspaceMember` 组内，非成员拿
`errWorkspaceNotFound` → 404。本仓复用 `invitations::require_workspace_member`，同样 404
（文案为 D9 的 `not found: workspace`）。

## 5. 验证证据

环境：rustup stable 1.98.1；本地 PostgreSQL 16.15（`multica_m2c`，4 个迁移全部应用）。

```
cargo build --workspace                              → Finished（0 error）
cargo clippy -p mc-repos -p mc-http --all-targets \
  --features mc-http/test-util -- -D warnings        → 0 error / 0 warning
cargo test -p mc-repos --lib                         → 28 passed; 26 ignored
cargo test -p mc-repos --lib -- --ignored            → 26 passed（含 inbox 8 + subscriber 2 条 DB 测试）
cargo test -p mc-http --lib                          → 41 passed
cargo test -p mc-http --test inbox --features test-util
                                                     → 1 passed（无库路由守卫）; 4 ignored
MULTICA_TEST_DATABASE_URL=… \
  cargo test -p mc-http --test inbox --features test-util -- --ignored
                                                     → 4 passed; 0 failed
```

e2e 覆盖的场景（`crates/mc-http/tests/inbox.rs`）：

1. `route_paths_are_mounted`（**不需要数据库**）：19 条路径 × 3 种缺头组合 = 57 个断言，
   保证路径拼写 / `:id` 语法 / 无重复注册。
2. `list_read_flow_and_visibility`：列表与 200 字预览、行粒度未读、已读幂等、
   `archive-all-read` 的组语义、issue 级归档/还原、别人通知 404、非成员 404、
   非法 item id 400、`?workspace_id=` 解析。
3. `archived_facets_and_cursor_paging`：facets 三维计数（每组只算最新一条）、
   游标翻页（`has_more` / 末页 `next_cursor=null`）、换 scope 续页 400、`group_id`
   的组键语义、上游的 6 条参数校验文案。
4. `bulk_operations_and_unread_summary`：`mark-all-read` / `archive-all-read`（未读组不动）/
   `archive-all` / `archive-completed`（含自定义 closed 状态与内置 `done`）、
   跨 workspace 未读汇总（行→组语义、读掉即消失、缺 workspace 400）。
5. `subscriber_routes_round_trip`：无 body 订阅、幂等、指定成员、非成员目标 403、
   `user_type` 校验 400、退订固定响应、子树退订（返回被移除的 issue id 集合）、
   坏 issue id / 跨 workspace / 非成员的 404。

## 6. 交接与后续（M3+）

- D1（列表分页上限）、D3（slug）、D4（agent 主体）、D5（退订 tombstone）、D6/D7（缺列）、
  D10（realtime）需要 master 决定是否排期；其中 **D5 需要新的迁移**（0004 未预留 tombstone 列）。
- `InboxRepo::create` / `NewInboxItem` 已就绪但本切片没有生产调用点：M2-A（issue 通知）、
  M2-B（comment 通知）落库时应复用，避免各自拼 SQL。
- M2-A 的 `issues.rs` 会注册 `/api/issues/:id`；本文件的 `/api/issues/:id/subscribers` 等
  是**更深的路径**，matchit 允许共存（已在完整 router 装配测试中验证）。
