# M3-6 task 用户面（LUM-1429）：agent-builder 4 条 + task / lifecycle / usage / retry 11 条

本文件是 M3 子波二 W3b 第三片（M3-6）的落地记录。范围**只有**这 15 条路由；daemon 面
（36 条）、`…/gc-check` 5 条、`recover-orphans`、8 条 runtime 异步往返、计费与配额结算
都不在本片（`docs/15-M3-PLAN.md` §1.4 / §1.6、§5 M3-6 与 `docs/36-M3-W3B-PREFLIGHT.md` §5 是范围依据）。

| 项 | 值 |
| --- | --- |
| issue | **LUM-1429**（parent epic LUM-1334） |
| 分支 | `feat/multica-rs-m3b-task-queue` → PR 目标 `feat/multica-rs-initial` |
| 基线 | `feat/multica-rs-initial` @ `617036e`（含 W0-B2 schema 切换 #27、daemon ws #28、M3-5 agent 面 #29） |
| 上游对照 | `louloulin/multica` @ `f41fae6b`：`server/internal/handler/{agent_builder,client_usage,issue_trigger,task_lifecycle,daemon,chat,agent,handler}.go`、`server/internal/service/{task,issue_trigger}.go`、`server/internal/middleware/{client,workspace}.go`、`server/cmd/server/router.go` L1635/1970/1992-1998/2016-2017/2224-2229/2314/2324 |
| 契约来源 | `migrations/upstream/494_issue_status_category_read_contract.up.sql`（`issue_effective_status`）+ `contracts/upstream-schema.sql` |
| 门禁 | `bash scripts/gates.sh --with-db` **10/10 全绿** |

## 1. 结论

1. **15 条路由全部从 gap 变为真实实现**，其中 **6 条是原地替换**（`preview-trigger` /
   `active-task` / `rerun` / `task-runs` / `usage` / `{id}/tasks/{taskId}/cancel`
   此前是 `routes/issues/mod.rs` 里的 `not_implemented` 501 占位）：
   同一 path + method **不能二次注册**（`Router::route` 会 panic），因此只能把占位删掉、
   在新 router 里注册同一对 path/method。**handler 名必须跟着换**，否则 route_parity 的
   占位计数会继续把它们算作「已实现」（这正是 `docs/15` §1.6 点的坑）。
2. **本片是 W3b 里唯一改既有文件的切片**（另两片只新增与删占位）。跨切片面共 **4 处**，
   逐条列出（PR 描述同列，便于集成周期审）：
   | 文件 | 改动 | 为何必须 |
   | --- | --- | --- |
   | `routes/issues/mod.rs` | 删掉 6 条 `not_implemented` 占位注册（-17 行） | 同一 path+method 不能二次注册 |
   | `routes/agents.rs`（M3-5 的文件） | +`AgentScope::can_invoke`（invoke 门）、+`agent_opt`（不报错的 `loadAgentForUser`） | M3-5 只落了 view 门（`can_access_private`），invoke 门与它**不同**（见 §5 第 16 条，本片逐行核实上游 `agent_access.go:49` 才敢写） |
   | `mc-repos/src/agent.rs`（M3-5 的文件） | +`AgentRepo::find_in_workspace`（`None` 即不存在） | 批量判定要它（一个 agent 不可见不该让整个 preview 请求失败） |
   | `crates/mc-http/tests/issues/auth.rs`（LUM-1423 的文件） | 501 断言从 `preview-trigger` 改落到 `GET /api/issues/:id/labels` | 前者被本片实现成真实路由，断言必然失效（门 ⑥ 会红）；`labels` 是本仓**连表都没有**的缺口，短期不会再失效 |
   另外 `crates/mc-repos/Cargo.toml` + `Cargo.lock` 多一条 `mc-repos → mc-task` 依赖边
   （写路径要用 `Cancellation` / `TaskError`）。`routes/mount.rs`、`routes/mod.rs`
   一行未动。该 `issues` 文件在 M2-A 之后已被 **LUM-1423**（R7 补课）拆成 `routes/issues/`，
   所以改动落在 `routes/issues/mod.rs`（`docs/36` §6 记的 `routes/issues.rs` 是拆分前的路径）。
3. **Repo 层兑现 M3-3 的 `TaskStore` port 之外的写面**：`crates/mc-repos/src/task.rs`
   （18 行 stub）被拆成 `task/{mod,row,queries,store,builder}.rs` + `task/tests/`。
   SQL **一律运行时 builder + 参数绑定**（无编译期宏，构建不需要库），列名逐条对照
   `contracts/upstream-schema.sql`，`mc_core::Id` 没有 sqlx impl ⇒ `FromRow` 手写。
4. **门 ⑦ / ⑨ 零回归**（实测）：`local 167 registered / baseline 156`、
   `implemented 137 real + 10 placeholder = 147 / 456`、`known_gap 309`、`unclaimed 0`、
   `regression 0`、`local_only 11`；`report matches`。本片**不**重生成两个快照
   （按 `docs/15-M3-PLAN.md` §7.3，路由基线与 ⑨ 报告由 M3 集成周期统一刷；本片只把
   +9 条 `real`、+11 个注册键的读数登记在这里）。
5. **测试 44 条**：`mc-repos` 15（真库 `#[ignore]`，入队/claim/完成、部分唯一索引、
   rerun 链、`task_message` 分页、usage 聚合、working-agents 聚合）+ `mc-http` lib 11
   （纯函数：探针校验 / provider 名 / 版本 / OS 归一化 / `<uuid>` 解析 / `TaskError` 映射）
   + `mc-http` e2e 18（真库，8 个模块）。
6. **两处上游不变量**是本片最容易踩的坑，已在 fixture 注释与 DB 测试里真实触发：
   `idx_one_pending_task_per_issue_agent_thread`（在飞 slot 的部分唯一索引，`queued`/`dispatched`
   才占位）与 `agent_task_queue_active_requires_runtime`（在飞行必须有 runtime）。

### 交付文件（行数实测，全部 ≤ 800）

| 文件 | 行数 | 说明 |
| --- | --- | --- |
| `crates/mc-repos/src/task/mod.rs` | 210 | 模块根：常量、`TaskRow`/`IssueBrief`/`TaskRepo` 装配 |
| `crates/mc-repos/src/task/row.rs` | 474 | 行结构 + 手写 `FromRow` |
| `crates/mc-repos/src/task/queries.rs` | 686 | 只读聚合（active-task / task-runs / messages / usage / working-agents / builder list） |
| `crates/mc-repos/src/task/store.rs` | 782 | 写路径（cancel / rerun / retry / draft / runtime 切换 / client-usage upsert） |
| `crates/mc-repos/src/task/builder.rs` | 342 | agent-builder 会话三段写 |
| `crates/mc-repos/src/task/tests/{mod,builder,lifecycle,queries}.rs` | 155 + 262 + 356 + 461 | 真库集成测试（15 条，全部 `#[ignore]`） |
| `crates/mc-http/src/routes/tasks.rs` | 354 | router（17 个注册键）+ `TaskScope` + 错误 helper |
| `crates/mc-http/src/routes/tasks/dto.rs` | 567 | 请求 / 响应 DTO（收窄投影） |
| `crates/mc-http/src/routes/tasks/lifecycle.rs` | 399 | preview-trigger / active-task / task-runs |
| `crates/mc-http/src/routes/tasks/rerun.rs` | 277 | rerun / retry-source-context |
| `crates/mc-http/src/routes/tasks/usage.rs` | 493 | client-usage / issue-usage / task messages（含 11 条纯函数单测） |
| `crates/mc-http/src/routes/tasks/builder.rs` | 277 | agent-builder 四条 |
| `crates/mc-http/src/routes/tasks/cancel.rs` | 159 | 两条取消路由 |
| `crates/mc-http/src/routes/tasks/working.rs` | 111 | working-agents |
| `crates/mc-http/tests/tasks/{main,support,usage,issue_runs,rerun,cancel,builder,working}.rs` | 13 + 534 + 498 + 497 + 367 + 289 + 482 + 241 | e2e（真库，`main.rs` 是 target 根） |

拆分动因都是门 ⑩（R7 单文件 800 行硬上限）：`store.rs` 782 与 `queries.rs` 686 是上限内的
最大块；`usage.rs` 493（实现 + 单测同文件）与 `builder.rs` 482 也按域切开了。

## 2. 路由表与鉴权门

列 `门` 的含义：**M** = workspace 成员（`TaskScope::resolve`，非成员 → 404 `workspace`）；
**I** = 再叠 invoke 门（`canInvokeAgent` 的 member 分支，拒 → 403 原始 `dispatch_blocked` 体）；
**A** = 再叠私有 agent 可见性（`canAccessPrivateAgent`）；**W** = **不要求** workspace。

| method | path | 上游 handler | 门 | 备注 |
| --- | --- | --- | --- | --- |
| GET | `/api/agent-builder/sessions/` | `ListAgentBuilderSessions` `agent_builder.go:192` | M | creator-scoped，admin 也看不到别人的 |
| POST | `/api/agent-builder/sessions/` | `CreateAgentBuilderSession` `agent_builder.go:57` | M | → 201 |
| PATCH | `/api/agent-builder/sessions/:session_id/runtime` | `SwitchAgentBuilderRuntime` `agent_builder.go:401` | M | → 200 |
| PUT | `/api/agent-builder/sessions/:session_id/draft` | `SaveAgentBuilderDraft` `agent_builder.go:256` | M | → 204 |
| POST | `/api/client-usage` | `UpsertClientUsage` `client_usage.go:49` | **W** | → 204；**唯一**不要求 workspace |
| POST | `/api/issues/preview-trigger` | `PreviewIssueTrigger` `issue_trigger.go:146` | M | 只读，原 501 |
| GET | `/api/issues/:id/active-task` | `GetActiveTaskForIssue` `daemon.go:5394` | M | `{"tasks":[…]}`，原 501 |
| POST | `/api/issues/:id/tasks/:task_id/cancel` | `CancelTask` `daemon.go:5421` | M | 原 501 |
| POST | `/api/issues/:id/rerun` | `RerunIssue` `task_lifecycle.go:165` | M+I | → 202，原 501 |
| GET | `/api/issues/:id/task-runs` | `ListTasksByIssue` `daemon.go:5519` | M | `?scope=family` 有 20 条上限，原 501 |
| GET | `/api/issues/:id/usage` | `GetIssueUsage` `daemon.go:5792` | M | 原 501 |
| GET | `/api/tasks/:task_id/messages` | `ListTaskMessagesByUser` `daemon.go:5724` | M | `?since=` 只收整数 |
| POST | `/api/tasks/:task_id/retry-source-context` | `RetrySourceContextQuickCreate` `task_lifecycle.go:237` | M+I | → 202 |
| POST | `/api/tasks/:task_id/cancel` | `CancelTaskByUser` `chat.go:1780` | M+A | chat 会话另有 creator 门 |
| GET | `/api/working-agents` | `ListWorkspaceWorkingAgents` `agent.go:2766` | M | 裸数组；按私有 agent 过滤 |

**四条落地约定**（`routes/tasks.rs` 顶部另有同款说明）：

- **尾斜杠别名**：上游 `r.Route("/api/agent-builder/sessions", …)` + `r.Get("/")` / `r.Post("/")`
  两种形态都能命中；本片注册 `/api/agent-builder/sessions/` 与 `/api/agent-builder/sessions`
  **两个键**（共 17 个注册键）。axum 0.7 / matchit 0.7 只注册其一时另一种返回 **404 而非 307**
  （M3-4 / M3-5 同款，见 `docs/15` §7.3）。
- **路径参数写 `:id` / `:task_id`**（不是 `{id}`）：写成 `{id}` 能编译但恒 404（M1-D 的坑）。
- **非成员 → 404 `workspace`**（上游 `requireWorkspaceRole` 的 workspace 分支），不是 403；
  缺 `X-Multica-User-Id` → 401。
- **鉴权复用 M3-5 的 `AgentScope`**，本片不新增判定：`AgentScope::can_invoke`（invoke 门）、
  `can_access_private`（可见性）、`filter_accessible`（列表逐行过滤）、`agent_opt`（不报错的
  `loadAgentForUser`）都是 M3-5 已合入的实现，本片只多包一层 `TaskScope`（agent 面 + `TaskRepo`）。

## 3. 上游语义对齐（逐条核过的点）

### 3.1 作用域与 issue/task 解析

| 上游 | 本片落点 |
| --- | --- |
| `loadIssueForUser`（`handler.go:1029`） | `TaskScope::issue`：不存在 / 别的 workspace → 404 `issue`；`:id` 同时接受 UUID 与 identifier（`LUM-42`）——用 `issue_for_workspace_ref`（`identifier` 命中即取，否则按 uuid 解析） |
| `visibleTaskHistory`（`handler.go:783`） | 未启动的委派回退行（`escalation_for_task_id` 非空、`started_at` 为空、状态 ∈ {`deferred`,`cancelled`}）不进 `task-runs` / `active-task` |
| `parseUUIDOrBadRequest` | `tasks::parse_uuid` → 400 `invalid <field>`（`task_id` / `runtime_id` / `chat session id` / `chat_session_id`） |
| 上游 `parseUUID`（不报错变体）+ `GetAgentTask` | `cancel_issue_task` 对畸形 `task_id` 回 **404 `task`**（不是 400）——上游走的是「查不到」 |

### 3.2 `preview-trigger`（只读判定）

上游 `service.WillEnqueueRun`（`service/issue_trigger.go:97`）的分支顺序被逐条搬进
`lifecycle.rs`（`PreviewCandidate` → `will_enqueue`）：

1. 解析 assignee（`assignee_type`/`assignee_id`）→ 2.`candidate.triage` 非空 → `None`
→ 3. 无 assignee → `None` → 4. `effective_status`（迁移 494 的 SQL 函数，见 §5）
→ 5. `is_create || assignee_changed` 时：`current == "backlog"` → `None`，否则 `"assign"`
→ 6. 否则 `status_changed && prev == "backlog" && current ∉ {backlog, done, cancelled}` → `"status"`
→ 7. `assignee_type != "agent"` → `None` → 8. agent 就绪（`runtime_id` 有值且未归档）
→ 9. `can_invoke` 白名单；`"status"` 分支还要求 `has_pending_task_for_issue_agent` 为假。

- 请求上限 `maxPreviewTriggerIssues = 500`（400 `too many issue_ids`），`is_create` 项必须有
  `status` / `assignee`（否则 400）。
- 响应 `{"triggers":[{"issue_id","agent_id","source"}],"total_count":n}`，`source` ∈
  `assign` / `status`。
- **只读**：不落库、不改 issue 的 assignee（上游 `PreviewIssueTrigger` 也是纯判定）。
- **不做 squad 分支**（本仓无 squad 仓储，见 §5），agent-actor 自环判定同样缺席（本仓一律按 member）。

### 3.3 `active-task` / `task-runs`

- `GetActiveTaskForIssue`：上游**吞掉**聚合错误回 `{"tasks":[]}`（`daemon.go:5401`）——
  本仓逐字保留（查库失败也回空列表，不 500）。
- `ListTasksByIssue`：默认只回本 issue 的历史；`?scope=family` 切到「同一 family」，
  并套 `familyActiveRunCap = 20` 的截断 + 响应头 `x-active-runs-truncated: true`
  （上游 `HeaderActiveRunsTruncated`）。两条都返回 `AgentTaskResponse[]` 的**收窄投影**
  `TaskDto`（见 §5）。

### 3.4 usage 三条

- **`POST /api/client-usage`（204）**：`x-client-platform` ∈ {`web`,`desktop`}（否则 400
  `client platform must be web or desktop`），`x-client-version` 匹配 `^[\x20-\x7e]{1,64}$`（否则 400
  `invalid client version`），`os` 白名单 {macos,windows,linux,ios,android,chromeos} 之外归
  `unknown`；body 上限 **16KB**（`clientUsageBodyLimit`）；`install_id` 必须是 UUID；
  `runtime` 探针仅 `desktop` 可带（否则 400 `runtime data is only accepted from desktop`）；
  `probe_result` 只能 `success` / `error`，且**探针失败时不许带计数**（上游
  `validateClientUsageRuntime`）。落 `client_usage_daily` 的 upsert 键 =
  `(user_id, client_type, install_id, activity_date)`，`activity_date` 用 SQL 的
  `(CURRENT_TIMESTAMP AT TIME ZONE 'UTC')::date`；「本次带探针」与「本次探针结果」共用
  一个布尔位（`runtime_probed_at IS NOT NULL`），**探针列按 `CASE WHEN` 保留旧值** ——
  e2e 实测第 4 步「再报一次但不带探针」后五个探针列原样保留。
- **`GET /api/issues/:id/usage`**：`task_usage` × `agent_task_queue` 聚合，metered
  （`total_*`）与 unmetered（`uncosted_*`）分列，并给 `task_count` / `terminal_task_count`
  / `metered_task_count` / `unreported_task_count`。**只读**，不做计费结算（`docs/15` §0）。
- **`GET /api/tasks/:task_id/messages`**：`?since=` 只接受整数（`strconv.Atoi` 语义，
  非数字 → 400 `invalid since parameter`，**不**静默忽略），按 `seq` 过滤。

### 3.5 `rerun` / `retry-source-context`

- **`POST /api/issues/:id/rerun`（202）**：body 可选（`{}` / 空体 = 派生；`{"task_id": …}`
  = 具名）。顺序：加载 issue → 具名来源校验（必须属于该 issue、不能是 triage 任务）→
  解析目标 agent → **invoke 门（先过门再动数据，被拒时旧任务原样保留）** → 清同
  `(issue, agent, thread)` 的 **pending** slot（`running` / `waiting_local_directory`
  故意不动）→ 入队。清 slot 与入队是两个事务，撞并发时**只重试一次**（上游同款），
  第二次失败即上报。
- **triage 门是 403 而不是 400**：`dispatch_blocked("issue_in_triage")`，且**只对派生
  rerun** 生效（具名 rerun 是「重放一次讨论」，triage 里的具名 rerun 合法）。
- 入队行：`status='queued'`、`force_fresh_session=TRUE`、`originator_source='direct_human'`、
  `rerun_of_task_id`（具名时）、`trigger_comment_id`（具名时沿用来源任务的），
  `enqueue_rerun_task` **不写 `context`** ⇒ 该任务的 `task_kind` 收敛为 `"direct"`。
- **`POST /api/tasks/:task_id/retry-source-context`（202）**：先证明调用者是**原请求者**
  （`originator_user_id == 调用者`，否则 403 `not your task`），再走
  `create_quick_create_retry`（来源必须 `status='failed'` 且 `issue_id` / `chat_session_id`
  / `autopilot_run_id` **全为空**），否则 409 **原始体**
  `{"code":"source_context_retry_unavailable","error":"This context can no longer be retried. Start again from the branch point."}`
  （`ErrSourceContextRetryUnavailable`）。本片只做入队，**不触发实际重跑**（重跑在 M3-7 的
  daemon 回路，`docs/15` §5 M3-6「范围限制」）。

### 3.6 两条取消

| | `/api/issues/:id/tasks/:task_id/cancel` | `/api/tasks/:task_id/cancel` |
| --- | --- | --- |
| 范围证明 | task 必须属于 URL 里的 issue（跨 issue / 跨 workspace 的 UUID 不能借这条路由取消） | task 的拥有 agent 提供租户判定（对 issue / chat / autopilot / quick-create 四种都成立） |
| 额外门 | 无 | chat task → 会话 creator 才可取消（否则 403 `not your task`）；其余 → 私有 agent 可见性 |
| CAS | 无 | `?expected_status=queued` + `chat_session_id` + `queue_action`（`edit`/`remove`）三元组，只取消仍为 `queued` 的行，否则 409 `task is no longer queued` |
| 错误语义 | service 层错误 → 400（上游同款） | 领域错误经 `TaskError` 映射（`Conflict` → 409） |

两条都写同一组 `cancelled_by_type/id/name` 列（`Cancellation::by_user`，名字取
`user.name`，查不到就 `None`——名字缺失不该让取消失败），并在取消后**重新取行**再出参
（保证响应里的 `status` / `cancelled_at` 是终态而不是取消前的快照）。

### 3.7 agent-builder 四条

- `resolveBuilderRuntime` 的三道门**顺序**是契约：解析 + 本 workspace（400
  `invalid runtime_id`）→ 私有 runtime 只有 owner 能用（403
  `this runtime is private; only its owner can use it`，**无 owner 的 runtime 谁都不能用**）
  → 必须 `online`（409 `runtime must be online to {start|switch} an agent builder session`，
  动词随路径）。
- 会话一律 **creator-scoped**：`list` 只列 `creator_id = 调用者` 且 `status='active'`
  的会话（admin 也看不到别人的草稿）；写路径用 `(workspace_id, creator_id)` 过滤。
- `create`：先解析 body（`runtime_id` 必填，空 → 400 `runtime_id is required`），再建
  **carrier agent**（`kind='system'`、`system_key LIKE 'agent_builder:%'`、`owner_id = 调用者`）
  与 `chat_session`（同一事务），→ 201 `{session_id, builder_agent_id, runtime_id}`。
- `draft`（PUT → 204）：草稿对服务端**不透明**（迁移 252），只校验「是对象 / 有 `draft` 键
  / ≤256KB / 是合法 JSON」；`null` 是合法草稿（原样入库），数组 body → 400
  `invalid request body`，超限 → **413**（`mc_errors` 没有 413 变体，手工构造同形信封）。
  会话状态分流：不存在 → 404 `chat session`；是普通会话（非 builder carrier）→ 404
  `agent builder session`；已归档 → 400 `chat session is archived`。
- `switch`：先过 runtime 门，再有飞行任务就 409 `stop the current reply before switching
  runtime`；成功后**只改 carrier agent 的 `runtime_id` + 清 `model`**，`chat_session.runtime_id`
  保持旧值（上游同款，`list` 以 carrier 为准）→ 200 `{runtime_id}`。
- `list` 的响应是 **`{"sessions":[…]}`**（上游 `ListAgentBuilderSessionsResponse`
  带 `json:"sessions"`），不是裸数组。

### 3.8 `working-agents`

- 参数校验的**顺序**是契约（上游逐个 `switch`，先命中先回）：
  `invalid type: must be issue, autopilot, or chat` → `relation requires scope=mine`
  → `scope=mine requires type=issue` → `invalid scope: must be mine` →
  `parent requires type=issue` → `parent cannot be combined with scope`。
- `relation` ∈ {assigned, created, involved, any}：`assigned` = 调用者是 issue 的 member
  assignee；`created` = member creator；`involved` = agent 是 assignee（本仓无 squad 分支）；
  `parent` 收窄到直接子 issue；`type` 三态（`issue` / `chat` / `autopilot`，空 = 不过滤）。
- 聚合在 SQL 里做（`status='running'`、`kind='user'`、未归档），**可见性过滤在 Rust 侧后置**
  ——与上游 `accessibleAgentIDs` 后置过滤同序：私有 agent 不得凭名字 / 头像 / 计数暴露存在性。
- `avatar_url` 是**可空列**，为 NULL 时整个键省略（本片不做 `resolveAvatarURLPtr` 的 URL 拼接）。

## 4. 模块地图

| 模块 | 职责 | 上游对应 |
| --- | --- | --- |
| `mc-repos::task` | 常量 / `TaskRow` / `IssueBrief` / `TaskRepo` 装配 / 手写 `FromRow` | `queries/*.sql` + `handler.go` 的投影 |
| `mc-repos::task::queries` | 只读：`issue_for_workspace(_ref)` / `effective_status` / `task_in_workspace` / `task_for_issue` / `list_tasks_by_issue` / `list_task_messages` / `issue_usage` / `list_working_agents` / `list_builder_sessions` / `has_pending_task_for_issue_agent` | `daemon.go:5519/5724/5792`、`agent.go:2766` |
| `mc-repos::task::store` | 写：`cancel_task` / `enqueue_rerun_task` / `cancel_pending_tasks_in_thread` / `create_quick_create_retry` / `upsert_client_usage` / `switch_builder_runtime` / `save_builder_draft` | `service/task.go:5849`、`chat.go:1780`、`client_usage.go` |
| `mc-repos::task::builder` | `create_builder_session`（carrier agent + chat_session 同事务） | `agent_builder.go:57` |
| `mc-http::routes::tasks`（本文件） | router（17 键）、`TaskScope`、`parse_uuid`、`task_error`、`user_display_name`、`non_empty_query` | `handler.go` 的 workspace / 角色解析 |
| `…::tasks::dto` | 收窄 DTO：`TaskDto` / `IssueTriggerPreview*` / `IssueUsageDto` / `TaskMessageDto` / `BuilderSession*` / `WorkingAgentDto` | `AgentTaskResponse`、`agent_builder.go:180` |
| `…::tasks::{lifecycle,cancel,rerun,usage,builder,working}` | 15 条 handler | 见 §2 表 |

## 5. 有意偏离（全部可核对）

1. **错误 body 的 message 带内部 kind 前缀**：`mc-errors::Error` 全仓统一派生
   `not found: {resource}` 等，因此本片 404 的 message 是 `not found: issue` /
   `not found: task`，上游是裸的 `issue not found`。`error_code` 与状态码一致。
   这是**继承 M2 的既有约定**（`docs/40` §5 第 1 条），不在本片改。
2. **两条 403 走「原始体」而不套信封**：`rerun` / `retry-source-context` 的 invoke 门拒绝
   回上游 `dispatchBlockedResponse` 的
   `{"error": <msg>, "reason_code": <invocation_not_allowed|issue_in_triage>}` 平铺 JSON
   （`admission.go:105`）——前端按 `reason_code` 分支，套信封会让它拿不到码。
   `retry-source-context` 的 409 同理（`{"code","error"}` 原始体）。
   `resolveBuilderRuntime` 的 403 与 `cancel` 的 403 则是**本仓信封**（上游 `writeError`），
   差异在 PR 描述里逐条列出。
3. **agent-builder 的 403 被折叠成 404**：上游 `loadAgentBuilderSession` 对「不是你的会话」
   回 403、对「不存在」回 404；本仓仓储层用 `(workspace_id, creator_id)` 过滤，两者同形 ⇒
   一律 404（`SessionNotFound`）。**不会**因此泄露他人会话的存在性。
4. **`preview-trigger` 是收窄的 `WillEnqueueRun`**：没有 squad 分支（本仓无 squad 仓储）、
   没有 agent-actor 自环判定（本仓一律按 member 处理，与 `docs/40` §5 第 5 条同款放宽）。
5. **`rerun` 的 `priority` 绑 `0`**：`IssueBrief` 没有 `priority` 列（上游读 `issue.Priority`）；
   `agent_task_queue.priority` 的默认值本来就是 0，因此行为等价，但上游会把 issue 优先级
   带进队列 —— 接 issue 优先级属后续切片。
6. **`TaskDto` 是有意收窄的投影**（上游 `AgentTaskResponse` 的子集）：不含 `workspace_id`、
   daemon-only 字段（`remote_mcp_*`、`plugin_hook_tools`、`workspace_context`、`issue_statuses`）、
   usage 水合与 attribution 平铺；`cancelled_by` 只在 `status == "cancelled"` 时出现。
   上游的 `parent_task_id` 在本仓叫 `escalation_for_task_id`。
7. **`TaskMessageDto` 的省略语义**：NULL 与空串都省略（上游 `pgtype.*.String` 取零值后
   `omitempty`）；`input` 只在库里存的是 **JSON 对象**时出现（上游解成 `map[string]any`，
   数组 / 标量 / `null` 都解组失败 ⇒ 省略）；`output` 不做 JSON 解析，原样输出字符串。
8. **`client-usage` 的 workspace 是可选的**：上游 `resolveWorkspaceID` 取不到值时落
   `workspace_id = NULL`，只有**显式给了** workspace 才校验成员身份 —— 非成员 → **403
   `workspace not found`**（上游 `client_usage.go:107`），非法 workspace id → 400。
   这条 403 是**本片唯一**「非成员不报 404」的地方，与其它 14 条路由的 404 口径不同。
9. **`create` 响应里的 `runtime_id` 回库内规范形式**（上游回请求原串）：只在输入非规范
   （大小写 / 花括号形态）时不同；`switch` 响应本来就是库内的值（上游同）。
10. **`save_draft` 的大小判定用重新序列化后的字节数**（`serde_json::to_vec(draft).len()`），
    与上游对 `json.RawMessage` 原串计长在空白布局不同的输入上可能差几十字节 —— 判定阈值
    与语义（> 256KB 拒绝）一致。
11. **不做计费 / 配额结算、不做 ws 广播**：`/api/issues/:id/usage` 只读；取消与 rerun 都不
    发 `agent:status` / `task:*` 事件（M3-7 的 hub 接管，见 §8）。
12. **`client-usage` 的 `os` 归一化写在 HTTP 层而不是仓储层**：上游在 `handler` 里归一化后
    再入库，本片同序（仓储层只存 `String`）。
13. **两条上游 `RequireHumanActor` 中间件（`/api/client-usage`、
    `/api/tasks/{taskId}/retry-source-context`）在本仓天然满足**：`AuthUser` 只接受成员身份，
    本仓不解析 agent actor 的 header ⇒ 不存在「agent 冒充人类」的通道（M3-7 引入 task token
    时补可信来源判定）。
14. **`working-agents` 不做头像 URL 拼接**（`resolveAvatarURLPtr`）：`avatar_url` 原样透传
    或省略；CDN / 代理前缀属 M3-5 的 agent 面未接线项。
15. **`effective_status` 用 SQL 函数而不是 Rust 重算**：迁移 494 已建
    `issue_effective_status(...)`，`preview-trigger` 直接调它——避免两处实现漂移
    （上游的 `ComputeEffectiveStatus` 与本仓函数同源）。
16. **invoke 门不复用 view 门**（本片新增 `AgentScope::can_invoke`）：上游 `canInvokeAgent`
    （`agent_access.go:49`）的 member 分支是「owner 直接放行；否则必须
    `permission_mode='public_to'` 且白名单命中」，**没有** admin 越权（而 view 门
    `canAccessPrivate` 对 admin 放行）。同一个 admin 读得到私有 agent，却不能因此就把它
    跑起来 —— `rerun` / `retry-source-context` 走 invoke 门，`cancel` 走 view 门。
17. **`tests/issues/auth.rs` 的 501 断言落点换到 `labels`**：本片把 `preview-trigger`
    实现后，原断言（占位 501 + `not_implemented`）必然失效；改用
    `GET /api/issues/:id/labels`——本仓 schema 里连 `issue_label` 表都不存在
    （`docs/10-M2-PLAN.md` §5），是当前最稳的缺口。

## 6. 测试

```bash
# 纯函数（不需要库）
cargo test -p mc-http --lib routes::tasks

# 真库（PG 必须已 `mc-migrate run --dir migrations`）
MULTICA_TEST_DATABASE_URL='postgres://…' \
  cargo test -p mc-repos --lib task:: -- --ignored
MULTICA_TEST_DATABASE_URL='postgres://…' \
  cargo test -p mc-http --test tasks --features mc-http/test-util -- --ignored
```

- `mc-repos` 15（真库）：入队 → claim → 完成的状态迁移、`idx_one_pending_task_per_issue_agent_thread`
  的部分唯一索引冲突、rerun 链（`rerun_of_task_id` / `force_fresh_session` / 不写 `context`）、
  `task_message` 按 `seq` 分页、`issue_usage` 的 metered / unmetered 分列、
  `working-agents` 的 4 种 relation、`client_usage_daily` 的 upsert 与探针保留、
  builder 三段写（carrier agent 的 `kind` / `system_key` / `owner_id`）、
  `create_quick_create_retry` 的前置条件。
- `mc-http` lib 11（纯函数）：探针校验（`success` 必须有匹配的 provider 总数 / `error` 不许
  带计数）、provider 名正则、client 版本正则、OS 归一化白名单、「合法 UUID 或 400」、
  `TaskError` → 状态码映射、空白 query 语义。
- `mc-http` e2e 18（真库，8 模块）：`usage` 5（upsert + 探针保留 + 平台/版本/体积/未知字段 +
  workspace 可选但成员校验 + task messages 省略与 `since` + issue usage 汇总）、  `issue_runs` 4（active-task 与 task-runs 的 issue/family 作用域 + 20 条截断头 +
  preview-trigger 的 create/reassign/status 三源 + 500 条上限 + 只读性）、
  `rerun` 3（派生与具名、invoke 门与 triage 403、retry 只对原请求者）、
  `cancel` 2（issue 作用域必须自证、chat 私有性与 CAS 409）、
  `builder` 2（会话生命周期 + runtime / 会话归属 / 归档 / 体积 / 离线各门）、
  `working` 2（参数校验顺序与鉴权、type/relation/parent 的过滤矩阵与私有 agent 隐藏）。
- **DB 测试的跳过策略**：未设 `MULTICA_TEST_DATABASE_URL` → 打印跳过并 `return`；
  **设了但连不上 / 没建表 → panic**（与 `tests/agents` 同款，防「绿但空跑」）。
- **夹具自清理**：`support::cleanup()` 先删 `agent_builder_draft` / `issue_source_context` /
  `client_usage_daily`，再删 workspace 与全部成员用户；跨 workspace 用例在外层显式
  `cleanup(&fx.pool, foreign_ws, &[foreign_user])`（删 workspace 会级联掉 task / issue /
  chat_session，但 `"user"` 与 `client_usage_daily` 不会）。
- **跨切片的回归面**：本片替换的 6 个占位会打破 `tests/issues/auth.rs` 里「占位返回 501」
  的断言（门 ⑥ 的红就是它）——把落点换到仍缺的 `GET /api/issues/:id/labels`，见 §5 第 17 条。
  其余 42 条既有 e2e（`agents` 12 / `comments` 6 / `contract_gaps` 8 等）不碰这 15 条路由的
  路径，一行未改。

## 7. 门禁

`bash scripts/gates.sh --with-db` **10/10 全绿**（真库 `multica_lum1429`；⑤ 由脚本 `env -u`
剥掉库变量，⑦/⑨ 同样剥掉）。关键读数：

- ③④ clippy `-D warnings` 干净（`--all-targets`，含 `test-util` 下的 e2e 目标）；
- ⑤ 全 workspace 绿；⑥ 真库 e2e 44 条全过（`mc-repos` 15 + `mc-http` 18 + lib）；
- ⑦ 见 §1.4；⑧ schema-drift 无差异；⑨ `report matches`；⑩ 全部文件 ≤ 800 行。

## 8. 交接

- **M3-7（LUM-1438）**：本片不碰 daemon 面。`enqueue_rerun_task` / `create_quick_create_retry`
  只把行写成 `queued`，**实际执行**（claim → prepare-lease → 执行 → 完成）在 daemon 回路；
  `retry-source-context` 也不触发重跑。
- **ws 广播缺口（跨切片）**：上游在取消 / rerun / teardown 后会推 `agent:status` /
  `autopilot:updated` / `BroadcastCancelledTasks`，本仓 `mc-ws` 尚未接线（M3-5 的 agent 面
  同样缺）⇒ 已在 `docs/39` §4.8 登记，由 M3-7 的 hub 统一补。
- **M3-3**：`mc-repos/src/task` 的写路径兑现了 `TaskStore` port；状态机与 lease 的**判定**
  仍在 `mc-task`，本片只做「行落库 + 状态置位」。
- **集成周期**：⑦ 的基线（156）与 ⑨ 的 `report.json` 由 M3 集成周期统一刷新
  （`docs/15` §7.3）；本片只登记读数，不写快照，避免与同波 M3-4 / M3-5 的 PR 争同一个文件。
- **后续可做**：`IssueBrief` 补 `priority`（让 rerun 带上 issue 优先级）；squad 仓储落地后
  补 `preview-trigger` / `rerun` / `working-agents` 的 squad 分支；`TaskDto` 的水合字段
  （usage / attribution）随 M9 的 analytics 面补齐。
