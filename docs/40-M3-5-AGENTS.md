# M3-5 agent 面（LUM-1428）：`/api/agents*` 13 条 + workspace 级统计 3 条

本文件是 M3 agent 面切片的落地记录。范围**只有**这 16 条路由；skills（M6）、mcp-servers（M8）、
dingtalk（M7）、mika（M9）不在本片，`docs/15-M3-PLAN.md` §1.3 与 `docs/fixtures/route-owners.tsv`
第 17/18/45/46/49 行是范围依据。

| 项 | 值 |
| --- | --- |
| issue | **LUM-1428**（parent epic LUM-1334） |
| 分支 | `feat/multica-rs-m3b-agents` → PR 目标 `feat/multica-rs-initial` |
| 基线 | `feat/multica-rs-initial` @ `2a759ae`（含 W0-B2 schema 切换 #27 / daemon ws #28） |
| 上游对照 | `louloulin/multica` @ `f41fae6b`：`server/internal/handler/{agent,agent_env,label,agent_permission,agent_access,agent_validation}.go`、`server/cmd/server/router.go` L2177-2200 / L2318-2332、`server/pkg/db/queries/agent.sql`、`agentconfig/concurrency.go` |
| 契约来源 | `contracts/upstream-schema.sql`（`agent` / `agent_invocation_target` / `agent_to_label` / `issue_label` / `agent_task_queue` / `agent_runtime`） |
| 门禁 | `bash scripts/gates.sh --with-db` **10/10 全绿** |

## 1. 结论

1. **16 条路由全部从 gap 变为真实实现**，写在 M3-0 的空 router 锚点上；`mount.rs` /
   `routes/mod.rs` / `routes/issues.rs` / `mc-repos/src/task.rs` **一行未动**（本片 0 改动面）。
2. **SQL 只针对上游 schema**：W0-B2 之后运行时迁移已是上游 560 条，本仓 `0001_init` 里那张同名
   `agent` 表是**另一套形状**，因此所有查询逐列对照 `contracts/upstream-schema.sql`，
   `db_create_applies_upstream_column_defaults` 里还有一条**直接读 `information_schema`**
   的断言（`max_concurrent_tasks=6`、`visibility='private'::text`、`kind='user'::text` …），
   防止夹具/迁移漂移。
3. **门 ⑦ / ⑨ 零回归**（实测）：`implemented 128 real + 10 placeholder = 138 / 456`、
   `known_gap 318`、`unclaimed 0`、`regression 0`、`local_only 11`；`report matches`。
   本片**不**重生成两个快照（按 `docs/15-M3-PLAN.md` §7.3，路由基线由 M3 集成周期统一刷）。
4. **测试 29 条**：`mc-repos` 17（9 纯函数 + 8 真库 `#[ignore]`）+ `mc-http` e2e 12（真库）
   + `mc-http` lib 内 21 条 `routes::agents::*` 纯函数（DTO 掩码 / 权限解析 / workspace 解析等）。
5. **两处上游不变量**是本片最容易踩的坑，已写进 fixture 注释：
   `agent_task_queue_active_requires_runtime`（在飞行必须有真 runtime）与
   `set_agent_task_comment_thread`（`context->>'wakeup_id'` 会被转 uuid）。

### 交付文件（行数实测，全部 ≤ 800）

| 文件 | 行数 | 说明 |
| --- | --- | --- |
| `crates/mc-repos/src/agent.rs` | 611 | 模块根：常量、校验、行结构、`AgentRepo` 主 CRUD |
| `crates/mc-repos/src/agent/labels.rs` | 114 | `agent_to_label` × `issue_label` |
| `crates/mc-repos/src/agent/tasks.rs` | 222 | `agent_task_queue` 读聚合 + 取消 |
| `crates/mc-repos/src/agent/env.rs` | 102 | `custom_env` 写路径 + `activity_log` 审计 |
| `crates/mc-repos/src/agent/tests.rs` | 185 | 纯函数单测 |
| `crates/mc-repos/src/agent/tests/db_tests.rs` | 652 | PG 集成测试（除任务外） |
| `crates/mc-repos/src/agent/tests/db_tests/tasks.rs` | 259 | PG 集成测试（任务读/取消/聚合） |
| `crates/mc-http/src/routes/agents.rs` | 402 | router（16 条）+ `AgentScope` + 错误 helper |
| `crates/mc-http/src/routes/agents/dto.rs` | 694 | 出参 DTO + 掩码/规范化 helper |
| `crates/mc-http/src/routes/agents/dto/input.rs` | 241 | 入参请求 + `double_option` + `parse_permission_input` |
| `crates/mc-http/src/routes/agents/crud.rs` | 737 | 8 条 CRUD/生命周期路由 |
| `crates/mc-http/src/routes/agents/env.rs` | 261 | 2 条 env 路由 |
| `crates/mc-http/src/routes/agents/labels.rs` | 127 | 3 条 label 路由 |
| `crates/mc-http/src/routes/agents/stats.rs` | 111 | 3 条 workspace 统计路由 |
| `crates/mc-http/tests/agents/{main,support,crud,auth,labels,env,stats}.rs` | 31 + 283 + 655 + 281 + 138 + 167 + 120 | e2e（真库，`main.rs` 是 target 根） |

拆分动因都是门 ⑩（R7 单文件 800 行硬上限）：`crud.rs` 737 行、`dto.rs` 694 行是**上限内**
的最大块（`dto.rs` 初版 912 行，把请求与权限解析拆到 `dto/input.rs` 才过门）；`mc-repos`
的集成测试拆了两层（`tests/` 再进 `tests/db_tests/tasks.rs`）。

## 2. 路由表与鉴权门

`|` 标记：**M** = workspace 成员即可；**V** = 再加 `memberAllowedToViewAgent` 可见性；
**C** = 再加 `canManageAgent`；**E** = 再加 env 专用（admin 或 agent 人类 owner）。

| method | path | 上游 handler | 门 |
| --- | --- | --- | --- |
| GET | `/api/agents/` | `ListAgents` `agent.go:1110` | M+V（逐行过滤，不是 403） |
| POST | `/api/agents/` | `CreateAgent` `agent.go:1379` | M（→ 201） |
| GET | `/api/agents/:id/` | `GetAgent` `agent.go:1223` | M+V |
| PUT | `/api/agents/:id/` | `UpdateAgent` `agent.go:1878` | M+C |
| POST | `/api/agents/:id/archive` | `ArchiveAgent` `agent.go:2541` | M+C |
| POST | `/api/agents/:id/restore` | `RestoreAgent` `agent.go:2599` | M+C |
| POST | `/api/agents/:id/cancel-tasks` | `CancelAgentTasks` `agent.go:2651` | M+C |
| GET | `/api/agents/:id/tasks` | `ListAgentTasks` `agent.go:2673` | M+V |
| GET | `/api/agents/:id/labels` | `ListLabelsForAgent` `label.go:576` | M（**无**可见性门） |
| POST | `/api/agents/:id/labels` | `AttachLabelToAgent` `label.go:591` | M+C |
| DELETE | `/api/agents/:id/labels/:label_id` | `DetachLabelFromAgent` `label.go:620` | M+C |
| GET | `/api/agents/:id/env` | `GetAgentEnv` `agent_env.go:141` | M+E |
| PUT | `/api/agents/:id/env` | `UpdateAgentEnv` `agent_env.go:190` | M+E |
| GET | `/api/agent-task-snapshot` | `ListWorkspaceAgentTaskSnapshot` `agent.go:2976` | M+V（按 agent 过滤） |
| GET | `/api/agent-activity-30d` | `GetWorkspaceAgentActivity30d` `agent.go:2925` | M+V |
| GET | `/api/agent-run-counts` | `GetWorkspaceAgentRunCounts` `agent.go:2884` | M+V |

**三条落地约定**（`routes/agents.rs` 顶部另有同款说明）：

- **尾斜杠是上游行为，不是笔误**：chi 的 `r.Route("/api/agents", …)` + `r.Get("/", …)` ⇒ 实际路径
  `/api/agents/`。axum 的 matchit **不会**做尾斜杠重定向，因此 `/api/agents/x` 是真 404，
  客户端必须带尾斜杠（`docs/fixtures/upstream-routes.tsv:24-25,27-28` 逐字对齐）。
- **路径参数写 `:id` / `:label_id`**：写成 `{id}` 能编译但恒 404（M1-D 的坑，已在本片复核）。
- **非成员 → 404 `workspace`**（上游 `requireWorkspaceRole` 的 workspace 分支），不是 403；
  缺 `X-Multica-User-Id` → 401。

## 3. 上游语义对齐（逐条核过的点）

### 3.1 访问控制

| 上游 | 本片落点 |
| --- | --- |
| `canManageAgent` = 角色 ∈ {owner,admin,member} 且（角色 ∈ {owner,admin} 或 `agent.owner_id == user`） | `AgentScope::require_can_manage`，否则 403 `only the agent owner can manage this agent` |
| `memberAllowedToViewAgent` = admin/owner 或 owner==user 或（`permission_mode='public_to'` 且命中 `agent_invocation_target`） | `member_allowed_to_view` + `filter_accessible`（列表走 `list(ws, true)` 后逐行过滤） |
| `loadAgentForUser` = resolve user → resolve ws（空 → 400）→ `GetAgentInWorkspace`（失败 → 404）→ `kind != 'user'` → 404 | `AgentScope::load_agent`（`not_found("agent")`） |
| `authorizeAgentEnv` = 先拒 agent actor，再要 admin 或人类 owner | `require_can_manage_env`，403 `only the agent owner or a workspace owner/admin can manage this agent's env` |
| `archive` 幂等保护：已归档 → 409 `agent is already archived`；系统 agent → 400 `built into Multica` | 同文案同状态码（`is_archived()` / `is_system()`） |

**UpdateAgent 的顺序**（`agent.go:1878`，本片逐行核过）：`loadAgentForUser` → `canManageAgent`
（失败 403）→ 解析 body（失败 400 `invalid request body`）→ 拒 `custom_env`（400，
指向 `PUT /api/agents/{id}/env`）→ 权限字段判定。因此：

- **非 owner 的普通成员连 PUT 都进不来**（`canManageAgent` 在前）；
- `permissionInputChangesAgent` 的「无改动重放放行」**只对非 owner 的 admin 生效**——
  admin 原样重放同一份 `permission_mode + invocation_targets`（或 legacy `visibility`）→ 200，
  真改权限 → 403 `only the agent owner can change access (permission_mode / invocation_targets)`；
- legacy-only 形态（只发 `visibility`）按**派生 visibility**比较：member-only 的 `public_to`
  派生为 `private`，所以 admin 重放 `visibility:"private"` 是 no-op 而非降级。

### 3.2 建/改的输入校验

- `name` 必填（400 `name is required`）、`description ≤ 255`、`runtime_id` 必填；
  `conversation_starters ≤ 3` 且逐条 trim / 长度上限（上游 `normaliseAgentConversationStarters`）。
- `max_concurrent_tasks`：字段缺失或 `null` → `6`；否则必须在 1..=50
  （400 `max_concurrent_tasks must be between 1 and 50`）。
- `visibility` 空串 → `private`；`visibility` 只接受 `private`/`workspace`，
  `permission_mode` 只接受 `private`/`public_to`。
- `runtime_id` 必须指向**本 workspace** 的 runtime，否则 400 `invalid runtime_id`；
  runtime 私有且调用者不是它的 owner → 403 `this runtime is private; only its owner can create agents on it`。
- 同 workspace 同名（活跃、非归档）→ 409（上游 `agent_workspace_name_active` 唯一索引）。
- `permission_mode` 解析（上游 `parsePermissionInput`，本片逐分支对齐）：`private` **忽略**
  提交的目标列表；`public_to` + 目标列表为空 → 归一到单个 workspace 目标（MUL-3963 裁定）；
  `member`/`team` 目标缺 `target_id` 或非 uuid → 400。
- 写权限时 `agent_invocation_target` **整表替换**（先删后插），与 `agent` 行**同一事务**
  （上游 `replaceInvocationTargetsWithQueries`）。

### 3.3 env（**与 issue 正文的括注不同**）

issue 正文写的「GET 返回脱敏后的键值与标记」与上游源码不符，本片按源码实现：

- **GET 返回明文** `{"agent_id", "custom_env"}`，并且**先落审计行再出明文**
  （`agent_env_revealed`，`issue_id` 为 NULL）；审计写失败 → 500
  `audit log write failed; refusing to serve env without a recorded reveal`（fail-closed）。
- **PUT 才用哨兵**：值 `****` → 该键**保留库里原值**；键 + `****` 但库里没有 → 整键丢弃
  （永不把字面 `****` 写进库）。PUT 与 `agent_env_updated` 审计行同事务。
- body 严格解码：形状不对 → 400 `invalid request body`（**不**静默当空 map，否则等于清空凭据）。

### 3.4 label

- 三个端点响应都是 `{"labels":[...]}`（上游也是 `map[string]any{"labels": …}`，**没有**额外的 `total`），`usage_count` 恒 `0`
  （agent 侧不 join 计数）。
- 挂载要求 `issue_label.resource_type = 'agent'`，否则 404（上游 `agent label not found`）；
  重复挂载靠 `ON CONFLICT DO NOTHING` 幂等（`rows_affected = 0`，仍 200）。
- **GET labels 没有可见性门**（`label.go:576` 实测只有 `loadAgentForUser`）——私有 agent
  对非 owner 成员也返回 200，这是**对齐**而不是漏门。

### 3.5 任务读聚合 / 取消

所有权划分：状态机与 lease 属 M3-3，`mc-repos/src/task.rs` 属 M3-6；本片只做**读投影 + 状态置位**。

- `ACTIVE_TASK_STATUSES` = `queued` / `dispatched` / `running` / `waiting_local_directory`
  （上游 4 个），`deferred` **不在其中**；但 `cancel_tasks` 的 `WHERE status IN (…)`
  与上游一样**包含 `deferred`**。
- `list_tasks` 套 `visibleTaskHistory`：`escalation_for_task_id` 非空且 `started_at` 为空且
  状态 ∈ {`deferred`,`cancelled`} 的「未启动委派回退行」不展示。
- snapshot = 在飞半边（含 `deferred AND context->>'wakeup_id' IS NOT NULL`）∪ 每 agent 的
  LATERAL Top-1 结果（仅 `completed`/`failed`，`cancelled` 不算结果）。
- run counts：近 30 天**全部**任务（含在飞与取消）都算一次 run；
  activity：锚点 `completed_at`，`FILTER` 分列 failed/completed/cancelled，无完成的日期不产行。
- 取消不广播（M3-7）、不做委派失败结算 / `ReconcileAgentStatus`（M3-3/M3-6）。
- 归档前会尝试取消在飞任务，失败**仅 warn**（上游同语义）。

### 3.6 列默认值（逐条对齐 `contracts/upstream-schema.sql`）

`max_concurrent_tasks=6`、`visibility='private'`、`permission_mode='private'`、`status='offline'`、
`kind='user'`、`runtime_config='{}'`、`custom_env='{}'`、`custom_args='[]'`、
`conversation_starters='[]'`、`disabled_runtime_skills='[]'`。集成测试同时断言映射层与
`information_schema.columns.column_default`，任何一侧漂移都会红。

## 4. 模块地图

| 模块 | 职责 | 上游对应 |
| --- | --- | --- |
| `mc-repos::agent` | 常量/白名单/`AgentRow`/`NewAgent`/`AgentUpdatePatch`/`NullableAgentField`、CRUD、归档恢复、`replace_invocation_targets`、`runtime_binding` | `agent.go` + `agent.sql` |
| `mc-repos::agent::labels` | `list_labels` / `get_label` / `attach_label` / `detach_label` | `label.go` + `issue_label.sql` |
| `mc-repos::agent::tasks` | `list_tasks` / `cancel_tasks` / `task_snapshot` / `run_counts_30d` / `activity_30d` | `agent.go` + `agent.sql:2601` |
| `mc-repos::agent::env` | `update_custom_env_audited`（同事务）、`record_env_activity`（读路径前置） | `agent_env.go:159-265` |
| `mc-http::routes::agents` | router、`AgentScope`（workspace 解析 / 成员校验 / `load_agent` / 两个门） | `handler.go` `loadAgentForUser:1169`、`agent_access.go` |
| `…::agents::dto` | 出参 DTO、掩码（`mcp_config`、`composio_toolkit_allowlist`）、`derive_legacy_visibility` | `AgentResponse:359` |
| `…::agents::dto::input` | 入参请求、`double_option`、`parse_permission_input` | `UpdateAgentRequest:1652`、`agent_permission.go:99` |
| `…::agents::crud` | 8 条 CRUD/生命周期路由 | `agent.go:1110-2700` |
| `…::agents::{env,labels,stats}` | 其余 8 条 | `agent_env.go`、`label.go`、`agent.go:2884-3010` |

## 5. 有意偏离（全部可核对）

1. **错误 body 的 message 带内部 kind 前缀**：`mc-errors::Error` 全仓统一派生
   `#[error("not found: {resource}")]` 等，所以本片 404 的 message 是 `not found: agent`，
   上游是裸的 `agent not found`；`error_code`（`"not_found"`）与状态码一致。
   这是**继承 M2 的既有约定**，不在本片改（改它会动全仓所有切片的响应）。
2. **`skills` 恒 `[]`**：`agent_skill` 的读写属 M6。
3. **`runtime_availability` 不返回**、`runtime_bound = runtime_id.is_some()`：runtime 在线投影属 M3-4。
4. **不做 provider 相关的 `thinking_level` / `service_tier` 枚举校验**（只透传字符串）。
5. **不解析 agent actor**：上游 `resolveActor` 认 `X-Agent-ID` / `X-Actor-Source`；本仓还没有
   「服务端可信地重写这些 header」的中间件，直接信任会让任意成员伪造成 agent 身份
   （`canAccessPrivateAgent` 对 agent actor 恒真）⇒ 本片一律按 member 处理（fail-closed）。
   M3-7 引入 task token 时补可信来源判定。
6. **不广播 WS 事件**（`agent:status` 等）：属 M3-7。
7. **`AgentTaskDto` 是有意收窄的投影**：不含 `workspace_id`、daemon-only 字段
   （`remote_mcp_*`、`plugin_hook_tools`、`workspace_context`、`issue_statuses`、
   usage hydration、attribution）；上游的 `parent_task_id` 在本仓叫 `escalation_for_task_id`。
8. **`settings.always_redact_env` 未接线**；`composioMCPAppsEnabled` 开关未实现。
9. **时间戳**用 `DateTime<Utc>::to_rfc3339()`（带小数秒），Go 的 `time.RFC3339` 会丢小数秒。
10. **`cancel_tasks` 只置位**：不广播、不结算委派失败、不 `ReconcileAgentStatus`（M3-3/M3-6）。
11. **`agent_to_label` 无 `usage_count`**：本仓 `issue_label` 也没有该列，因此恒 `0`。

## 6. 测试

```bash
# 纯函数（不需要库）
cargo test -p mc-repos --lib agent::
cargo test -p mc-http --lib routes::agents

# 真库（PG 必须已 `mc-migrate run --dir migrations`）
MULTICA_TEST_DATABASE_URL='postgres://…' \
  cargo test -p mc-repos --lib agent:: -- --ignored
MULTICA_TEST_DATABASE_URL='postgres://…' \
  cargo test -p mc-http --test agents --features test-util -- --ignored
```

- `mc-repos` 17：纯函数 9（白名单/上限/`NullableAgentField` SQL 静态性/`AGENT_COLUMNS` 覆盖
  `AgentRow` 全部字段/status 分类/`visibleTaskHistory`/invocation target 白名单）+ 真库 8
  （列默认值 + catalog、kind/workspace 作用域与归档恢复、`COALESCE` 与显式清空、允许列表
  整表替换与批量读、label 的 resource_type 守卫、任务读/快照/聚合/取消与幂等、runtime 绑定跨 workspace）。
- `mc-http` lib 21：`routes::agents::*` 的纯函数（掩码/权限输入解析/规范化/空白 query 语义/
  router 构建），不需库。
- `mc-http` e2e 12：roundtrip+默认值、创建校验与 runtime 绑定错误、update 权限门、
  归档/恢复/取消/任务列表生命周期、系统 agent 与 404、body 形状错误、label 生命周期、
  env 明文与哨兵、三条统计、以及 401/私有隐藏/已归档可达三条鉴权用例。
- **DB 测试的跳过策略**：未设 `MULTICA_TEST_DATABASE_URL` → 打印跳过并 `return`；
  **设了但连不上/没建表 → panic**。静默跳过会造出「绿但空跑」，本片实测踩过一次
  （库是空的，`--ignored` 报 7 passed 全是假绿）后改严。

## 7. 门禁

`bash scripts/gates.sh --with-db` **10/10 全绿**（真库 `multica_lum1428`；⑤ 由脚本 `env -u`
剥掉库变量，⑧ 需要库）。关键读数：③④ clippy `-D warnings` 干净（含 `--all-targets` 下的
测试目标）、⑤ 全 workspace 绿、⑦ 见 §1.3、⑩ 全部文件 ≤ 800 行。

## 8. 交接

- **M3-6**：`agent_task_queue` 的状态机/lease/结算接管 `mc-repos/src/agent/tasks.rs` 里的
  读投影；`cancel_tasks` 的进程内副作用（广播 + 结算 + reconcile）归那边。`mc-repos/src/task.rs`
  目前仍是 18 行 stub，本片未动。
- **M3-7**：agent actor 的可信来源（task token / 服务端重写 header）与 WS 广播。
- **M3-4**：`runtime_availability` / runtime 在线投影，以及 `AgentDto` 里现在恒 `false` 的
  在线字段。
- **M6**：`skills` + `agent_skill`（`AgentDto.skills` 现在是空数组占位）。
