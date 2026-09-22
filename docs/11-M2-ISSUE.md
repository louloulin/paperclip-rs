# M2-A — issue 核心 Repo + `/api/issues` + `/api/issue-statuses`

对应 issue：`LUM-1348`（M2 三切片之一，计划见 `docs/10-M2-PLAN.md` §2 M2-A）
上游基准：`louloulin/multica` `server/cmd/server/router.go` @ `f41fae6`，
handler 参考 `server/internal/handler/issue.go`（CreateIssue L2931 / UpdateIssue L3466 /
ListIssues L1164 / GetIssue L2345 / SearchIssues L1005 / QueryIssues L1150 / Grouped L1873）
与 `issue_status.go`。

## 1. 交付物

| 文件 | 内容 |
| --- | --- |
| `crates/mc-repos/src/issue.rs` | `IssueRepo`（DB-backed，`query_as`/`query` 运行期 builder + 全参数绑定） |
| `crates/mc-repos/src/issue_status.rs` | `IssueStatusRepo`（状态目录 CRUD / reorder / 默认目录） |
| `crates/mc-http/src/routes/issues/` | `/api/issues*` + `/api/issue-statuses*` handlers + `router()`（R7 拆分：`mod.rs` 入口 + 8 个分片，单文件均 ≤ 800 行） |
| `crates/mc-repos/src/lib.rs` | 仅追加一行 `pub mod issue_status;` |
| `crates/mc-http/tests/issues/main.rs` | 7 个 e2e（`test-util` feature 门控，需真库；R7 拆分后的入口，分片在 `tests/issues/`） |

`mount_slice_issue()` / `mount.rs::router()` / `routes/mod.rs` **未改动** —— scaffold
（`feat/multica-rs-initial` @ `fd6dfd6` 的 `4aa275a`）已把切片合并进主 router。

## 2. 路由清单（1:1 对齐上游路径）

### `/api/issues`（上游 L1955–L2016）

| Method | Path | Handler | 状态 |
| --- | --- | --- | --- |
| GET | `/api/issues` | `list_issues` | ✅ |
| GET | `/api/issues/` | `list_issues` | ✅（尾斜杠别名） |
| POST | `/api/issues` | `create_issue` | ✅ |
| POST | `/api/issues/query` | `query_issues` | ✅（超大 filter 集的 POST 孪生） |
| GET | `/api/issues/search` | `search_issues` | ✅ |
| GET | `/api/issues/grouped` | `list_grouped` | ✅ |
| GET | `/api/issues/children` | `list_children_by_parents` | ✅（`parent_ids` 上限 200） |
| GET | `/api/issues/child-progress` | `child_progress` | ✅ |
| POST | `/api/issues/batch-update` | `batch_update` | ✅ |
| POST | `/api/issues/batch-delete` | `batch_delete` | ✅ |
| POST | `/api/issues/quick-create` | `quick_create_issue` | ⚠️ 降级实现（见 §4） |
| GET | `/api/issues/limit-usage` | — | ❌ 501（依赖 agent/plan 配额，M3） |
| POST | `/api/issues/preview-trigger` | — | ❌ 501（依赖 daemon，M3） |
| POST | `/api/issues/table/{groups,rows,facets}` | — | ❌ 501（归 **M2-D / LUM-1355**） |

### `/api/issues/{id}`（上游 L1974–L2015）

| Method | Path | Handler | 状态 |
| --- | --- | --- | --- |
| GET / PUT / DELETE | `/api/issues/:id` | `get_issue` / `update_issue` / `delete_issue` | ✅（`:id` 接受 UUID 或 `LUM-1348`） |
| POST | `/api/issues/:id/move` | `move_issue` | ✅（`before_id` / `after_id` 锚点重排） |
| GET | `/api/issues/:id/children` | `list_issue_children` | ✅ |
| GET / POST / DELETE | `/api/issues/:id/reactions` | `list_reactions` / `add_reaction` / `remove_reaction` | ✅（幂等） |
| GET | `/api/issues/:id/metadata` | `get_metadata` | ✅ |
| PUT / DELETE | `/api/issues/:id/metadata/:key` | `set_metadata_key` / `delete_metadata_key` | ✅（key ≤64 且限 `[A-Za-z0-9_\-.]`、≤50 条、值必须是标量） |
| PUT / DELETE | `/api/issues/:id/properties/:propertyId` | `set_property` / `delete_property` | ⚠️ 值面落 `issue.properties` JSONB；定义目录无表（见 §5） |
| GET | `/api/issues/:id/labels` | — | ❌ 501（**无表**，见 §5） |
| DELETE | `/api/issues/:id/labels/:labelId` | — | ❌ 501（**无表**） |
| GET | `/api/issues/:id/timeline` | — | ❌ 501（依赖 M9 activity log） |
| GET | `/api/issues/:id/active-task`、`/task-runs`、`/usage` | — | ❌ 501（依赖 M3 任务队列） |
| POST | `/api/issues/:id/rerun`、`/tasks/:taskId/cancel` | — | ❌ 501（依赖 M3） |
| GET/POST/PUT/PATCH | `/api/issues/:id/wakeups*`、`/api/issue-wakeups` | — | ❌ 501（依赖 M5 wakeup） |
| GET | `/api/issues/:id/attachments` | — | ❌ 501（依赖 M5 storage 面） |
| GET | `/api/issues/:id/pull-requests` | — | ❌ 501（依赖 M9 PR 同步） |
| GET | `/api/issues/:id/quick-actions`、`POST /:id/comments/trigger-preview` | — | ❌ 501（M2-B / M3） |

`comments`（POST/GET）与 `subscribers` / `subscribe` / `unsubscribe[/subtree]` 四条
挂在 `/api/issues/:id` 下但**归 M2-B / M2-C**，不在本切片注册（见 §6）。

### `/api/issue-statuses`（上游 L2054–L2063）

| Method | Path | Handler | 状态 |
| --- | --- | --- | --- |
| GET / POST | `/api/issue-statuses` | `list_statuses` / `create_status` | ✅（尾斜杠别名同 `GET`） |
| PATCH | `/api/issue-statuses/reorder` | `reorder_statuses` | ✅ |
| PATCH / DELETE | `/api/issue-statuses/:id` | `update_status` / `delete_status` | ✅ |

读对任意成员开放（`GET` 内含 `ensure_defaults` self-heal）；**写路径限 owner/admin**
（`require_workspace_admin`，与上游 router.go L2051 注释一致；非 admin → 403）。
删除时该 key 仍被 issue 使用 → `409`（上游同义）。

### 501 统一形态

```json
{ "error": { "code": "not_implemented", "message": "...", "path": "/api/issues/…" } }
```

占位端点**注册在真实 path 上**（不是 404），便于前端/M3 增量替换时按 path 对账。

## 3. Repo 对外接口（**M2-B / M2 集成对账用**）

`IssueRepo`（`mc_repos::issue`，`new(db: Db)`，错误类型 `mc_repos::RepoError`：

| 方法 | 签名（简化） | 语义 |
| --- | --- | --- |
| `create` | `(NewIssue) -> Result<IssueRow>` | 分配 `number`/`identifier`（`UNIQUE(workspace_id, number)`，冲突重试 4 次） |
| `get` | `(workspace_id: Id, id: Id) -> Result<IssueRow>` | 按 UUID；不存在 → `NotFound` |
| `get_by_identifier` | `(workspace_id: Id, identifier: &str) -> Result<IssueRow>` | 按 `LUM-1348` |
| `update` | `(workspace_id: Id, id: Id, &IssueUpdate) -> Result<IssueRow>` | 全字段 CASE 补丁；`expected_revision` 不匹配 → `Conflict` |
| `delete` | `(workspace_id: Id, id: Id) -> Result<()>` | 硬删（子 issue 由 FK 处理） |
| `list` / `list_with_total` | `(&IssueFilter) -> Result<Vec<IssueRow>> / Result<(Vec<IssueRow>, i64)>` | status/priority/assignee/creator/parent/project/stage/`q`/`include_closed`/分页/排序 |
| `children_of` | `(workspace_id: Id, parent_id: Id) -> Result<Vec<IssueRow>>` | 单父 |
| `children_of_parents` | `(workspace_id: Id, &[Uuid]) -> Result<Vec<IssueRow>>` | 多父，空入参 → 空表 |
| `child_progress` | `(workspace_id: Id, terminal: &[String]) -> Result<Vec<ChildProgressRow>>` | `{parent_issue_id, total, done}` |
| `grouped_counts` | `(&IssueFilter, IssueGroupField) -> Result<Vec<GroupedCountRow>>` | `{key, total, done}` |
| `batch_update` | `(workspace_id: Id, &[Id], &IssueUpdate) -> Result<u64>` | 逐行复用 `update`，返回成功行数；单行失败即中断 |
| `batch_delete` | `(workspace_id: Id, &[Id]) -> Result<u64>` | |
| `move_issue` / `move_issue_with_update` | `(workspace_id, id, before_id: Option<Id>, after_id: Option<Id>, [&IssueUpdate]) -> Result<IssueRow>` | 锚点推导 `position`；循环父子 → `Conflict` |
| `has_ancestor` | `(workspace_id: Id, issue_id: Id, candidate: Id) -> Result<bool>` | 递归 CTE |
| `count_in_workspace` | `(workspace_id: Id) -> Result<i64>` | |
| `workspace_prefix` / `next_number` / `terminal_status_keys` | `(workspace_id: Id) -> Result<…>` | identifier 前缀 / 下一个 number / 终态 key 列表 |
| `get_metadata` / `set_metadata_key` / `delete_metadata_key` | `(workspace_id, id, key, …) -> Result<JsonValue>` | 返回更新后的整个 JSONB |
| `set_property` / `delete_property` | `(workspace_id, id, property_id: &str, …) -> Result<JsonValue>` | 同上 |
| `list_reactions` | `(issue_id: Id) -> Result<Vec<IssueReactionRow>>` | |
| `add_reaction` / `remove_reaction` | `(issue_id, actor_type: &str, actor_id: &str, emoji: &str) -> Result<IssueReactionRow>` | 加为 `ON CONFLICT DO UPDATE` 幂等；删不存在 → `NotFound` |

错误映射（复用 `crate::workspace::map_sqlx_err`）：`RowNotFound → RepoError::NotFound`、
PG `23505 → Conflict`、其余 → `RepoError::Db`。**HTTP 层**再翻译：
`NotFound → 404`、`Conflict → 409`、`Db → 500`。

`IssueStatusRepo`（`mc_repos::issue_status`）：`ensure_defaults(workspace_id) -> u64`、
`list` / `get` / `find_by_key`、`resolve_category` / `is_closed_key`、
`create(&NewIssueStatus)` / `update` / `reorder(&[(Id, f64)])` /
`count_issues_using_key` / `delete`。自由函数 `validate_key` / `derive_key_from_name` /
`parse_category` / `category_str` / `DEFAULT_STATUSES`（7 条内置目录）。

## 4. `quick-create` 的降级实现

上游 `QuickCreateIssue` 把「快速创建」交给常驻 daemon 异步落库并派发 agent 任务。
本仓尚无 M3 的任务队列 / daemon，因此按 `docs/10-M2-PLAN.md` §2「无 daemon 时按上游降级」
走同步分支：建 issue（`origin = quick_create`），返回与 `POST /api/issues` 相同的
`201` + `IssueDto`。

- 请求体沿用 `CreateIssueRequest`（`title` 必填，可选 `description` / `project_id` /
  `priority` / `stage` / `parent_issue_id` …）；未显式给 `origin` 时自动补
  `origin_type = "quick_create"` + 随机 `origin_id`（上游同字段用于幂等，本仓建 issue
  后不再消费该值）。
- **待接线**：M3 落地 daemon/task queue 后，本 handler 应改为「建 issue + 入队任务 +
  返回任务句柄」，并把响应体换成上游形态。

## 5. 与上游的有意偏离

| 主题 | 上游 | 本仓 | 原因 |
| --- | --- | --- | --- |
| identifier 前缀 | workspace 上独立配置的 issue 前缀 | `issue_prefix_from_slug(workspace.slug)`（大写、截断） | `0001` 的 `workspace` 表没有前缀列 |
| `properties` | 独立 `issue_properties` 表 + `/api/properties` 定义目录 | `issue.properties` JSONB 列（列在 `0001:153`） | 表不存在，**不新增迁移**（编号由集成 master 统一分配） |
| `labels` | `issue_label` / `issue_to_label` 两表 | **501** | `grep -rni label migrations/` 为空，两表不存在 |
| reaction `actor_type` | `'member'` | `'user'` | 跟随 scaffold `0004` 的 CHECK（`user`/`agent`/`system`）+ `actor_id TEXT` |
| `assignee_type` | `member` | 入参接受 `member`，落库/回显统一 `user` | `mc-core` 领域类型只有 `user`/`agent` |
| assignee 存在性 | `validateAssigneePair`：member/agent/squad 必须在本 workspace 内存在；agent/squad 再过 `canInvokeAgent` 权限门（403） | **同样做存在性 + 归档位**（`validate_assignee_target`，含 `autopilot`），但**没有** 403 权限门 | 本仓 M2 无 agent 可见性/私有点判定面（LUM-1410） |
| `assignee_type` 取值面 | 只接受 `member`/`agent`/`squad`，其余（含 `autopilot`）→ 400 | 另收 `user`（`member` 同义）与 `autopilot`（须真实存在） | 本仓四值枚举 + `0001` CHECK 允许 `autopilot`（LUM-1410 登记） |
| `attachment_ids` | 逐元素 `util.ParseUUID` → 非法 400；合法则把附件挂到新 issue（含归属校验） | **同样逐元素校验 UUID**（400 `invalid attachment_ids`，写库之前），但**不绑定** | 无 `attachment` 表，storage 面归 M5（LUM-1410 登记） |
| `triage_state` | 可写字段 | 接受但忽略（不落库） | `0001` 列存在但 M2 无 triage 交互面 |

## 6. 未覆盖项 TODO（本切片明确不做）

| 端点 / 能力 | 归属 | 备注 |
| --- | --- | --- |
| `table/{groups,rows,facets}` | **M2-D / LUM-1355** | 依赖 `IssueRepo::grouped_counts` + facet 聚合，本切片已备好 `grouped_counts` |
| `timeline` | M9 | 需 activity log 表 |
| `wakeups` + `/api/issue-wakeups` | M5 | `wakeup` 表存在，wake 语义留给 M5 |
| `active-task` / `task-runs` / `rerun` / `tasks/:taskId/cancel` / `usage` / `limit-usage` / `preview-trigger` | M3 | 需 agent task queue |
| `attachments` | M5 | 需 storage + `attachment` 表（上游 029） |
| `pull-requests` | M9 | 需 PR 同步 |
| `labels`（GET/POST/DELETE） | 待立项（M2-E 或 M3） | **表不存在**；需先由 master 分配迁移编号 |
| `properties` 定义目录（`/api/properties`） | 待立项（M2-E） | 值面已可用（JSONB） |
| `/api/issues/:id/comments*` | M2-B / LUM-1350 | 含 `trigger-preview`（M3） |
| `/api/issues/:id/subscribers` + `subscribe` / `unsubscribe[/subtree]` | M2-C / LUM-1349 | 注册在 `routes/subscribers.rs` |

## 7. 测试

`crates/mc-repos/src/issue.rs` / `issue_status.rs` 内联单测（无需 DB）：

```
cargo test -p mc-repos --lib
# issue: 8 个（filter 缺省、group 白名单、LIST_WHERE 占位符唯一、锚点 position 推导与
#         拒绝、identifier 前缀、comma 参数、revision 空补丁…）
# issue_status: 5 个（category round-trip、默认目录、key 派生/校验去重…）
```

DB 集成测试（标 `#[ignore]`，需真库）：

```
MULTICA_TEST_DATABASE_URL=postgres://multica:multica@127.0.0.1:5432/multica_test \
  cargo test -p mc-repos --lib -- --ignored
# issue::db_tests 6 个：CRUD round-trip、number 每 workspace 单调 + UNIQUE 冲突、
#   list/search/total、children+progress+grouped、move/batch/JSONB、move 合并补丁与祖先环
# issue_status::db_tests 3 个：自定义状态派生 key + 后缀、ensure_defaults 幂等、
#   update/reorder/delete 守卫（内置不可删、被引用不可删）
```

HTTP e2e（`crates/mc-http/tests/issues/main.rs`，`test-util` feature）：

```
MULTICA_TEST_DATABASE_URL=postgres://multica:multica@127.0.0.1:5432/multica_test \
  cargo test -p mc-http --features test-util --test issues -- --ignored
# 7 个：auth/workspace/501 形态（含 quick-create 400→201）、CRUD 全链 + revision 409 +
#   终态迁移 409、filter/search/grouped、children/move/batch、
#   metadata/properties/reactions（幂等）、status catalog 生命周期（含 member 写 → 403）、
#   create/update 的 assignee 存在性 + attachment_ids 形态校验（LUM-1410，含“400 在写库之前”
#   的 issue 计数断言）
```

不设 `MULTICA_TEST_DATABASE_URL` 时 DB 测试**静默 skip** —— 只看 “全绿” 可能是空跑。

## 8. 已知风险 / 后续

- `batch_update` 是逐行 `update`（每条一次 revision 自增 + 一次事务），与上游单条
  `UPDATE … WHERE id = ANY()` 的 revision 语义略有差异；批量 >100 时建议改单条 SQL。
- `next_number` 为 `MAX(number)+1` + 唯一约束重试，高并发下依赖重试窗口，未做序列化。
- `search` 的 `q` 目前是 `ILIKE`（title/description/identifier），上游在超过阈值时切
  `tsvector` 全文索引；`0001` 没有 `tsvector` 列，待 M2-D 或性能 cycle 评估。

## 9. 门禁实测（LUM-1348 交付时）

基线：`feat/multica-rs-initial` @ `afccdd5`（本切片从 `fd6dfd6` rebase 上来，只多出
M2-C 与 docs 提交；本切片文件无冲突）。

| 门禁 | 命令 | 结果 |
| --- | --- | --- |
| 格式 | `cargo fmt --all --check` | ⚠️ 见下 |
| 构建 | `cargo build --workspace` | ✅ |
| lint | `cargo clippy --workspace --all-targets -- -D warnings` | ✅ 0 warning |
| lint（测试码） | `cargo clippy -p mc-http --all-targets --features test-util -- -D warnings` | ✅ |
| 单测 | `cargo test --workspace`（不设 `MULTICA_TEST_DATABASE_URL`） | ✅ 全绿 |
| DB 集成 | `cargo test -p mc-repos --lib -- --ignored` | ✅ 35 passed |
| DB e2e | `cargo test -p mc-http --features test-util -- --ignored` | ✅ 16 passed（其中 `issues.rs` 6） |

**`cargo fmt --all --check` 的红点全在本切片之外**：只有 M2-C 的 5 个文件
（`routes/inbox.rs`、`routes/subscribers.rs`、`tests/inbox.rs`、`repos/inbox.rs`、
`repos/subscriber.rs`）报 diff，本切片 4 个文件零 diff。这是 base（`f05b803` 合入
M2-C）的遗留欠账，修复提交是 M1-E 的 `9851ebf`（PR #5，尚未并入 base）。
实测：在本切片 HEAD 上 cherry-pick `9851ebf` 后 `cargo fmt --all --check` **exit 0**
→ 集成顺序必须是 **M1-E 先并，再并 M2-A**，否则全仓 fmt 门禁红。

> `MULTICA_TEST_DATABASE_URL` **仅**给 `--ignored` 的 DB 测试用；`cargo test --workspace`
> 必须**不带**它跑：`tests/smoke.rs::workspace_member_http_e2e` 会对拿到的库重跑迁移，
> 指向已迁移库时报 `relation "user" already exists`（M1 遗留的测试设计问题，非本切片引入）。
