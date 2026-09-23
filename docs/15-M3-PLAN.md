# M3 计划：Runtime / Agent / Task Queue（plan1 W3「执行内核」）三子波切片

> - **编制**：LUM-1357（docs-only —— 本文件不含实现代码，也不创建 M3 切片子 issue）
> - **基线**：`feat/multica-rs-initial` @ **`0af39ab`**（编写时的最新 commit：W1 Google 登录 + W0-D schema-drift 门禁合入后）
> - **上游权威**：`docs/fixtures/upstream-routes.tsv`（471 行路由表）+ `server/cmd/server/router.go` 快照，上游 commit `f41fae6b08fb`
> - **冲突裁决**：**以 `docs/plan1.md` 为准**（LUM-1357 明确要求）。本文件与 `docs/01-PLAN.md` §6 冲突处，`docs/01` 的旧口径作废。
> - **不改动**：`docs/09` / `docs/10` 的既有结论一律保留；本文件对既有文档的事实更正集中在 §9。
> - 结构沿用 `docs/10-M2-PLAN.md`（§0 路由清单 / §2 切片划分 / §3 并发晋升 / §4 环境前置），集成与仲裁沿用 `docs/09-M1-INTEGRATION.md`。

---

## 0. 范围裁定：M3 = plan1 W3，不是 `docs/01` §6 的旧 M3

plan1 §5 把 `docs/01` §6 的旧 M3 边界整块拆进了 **W3**，并把其中一部分推给后续波次。逐项裁定：

| `docs/01` §6 旧 M3 项 | plan1 归属 | 本计划处置 |
| --- | --- | --- |
| 26 个 runtime profile | W3 `mc-runtime`(profile/catalog/quota/liveness) | **M3 做**（§5 M3-4；口径按 §9.3 更正） |
| daemon pair | W3 `mc-daemon`(client+execenv)+daemon 协议(36) | **M3 做**（§4 M3-1 冻结协议 → §6 M3-7 实现） |
| agent CRUD | W3 agent(25) | **M3 做 13 条**；其余 12 条已由 fixture 划给 M6/M7/M8/M9（§9.5） |
| agent builder | W3 agent 段内（4 条） | **M3 做**（§5 M3-6，`agent_builder_draft` 表阻塞） |
| task queue + lifecycle + usage + retry | W3 | **M3 做**（§4 M3-3 领域层 → §5 M3-6 路由面） |
| —— | W3 新增：**26 个 adapter**（先 1 后批） | **M3 做，但分批**（§6 M3-8；plan1 R4 + 本 issue 硬约束） |
| cloud-runtime(11) | **W9 商业面** | **M3 不做**，判给 M9（§9.1 冲突裁决） |

**M3 真实路由 = 101 − 11(cloud-runtime) = 90 条。**

这 90 条的当前状态（@`0af39ab` 实测，见 §1.8）：

- **84 条需要全新实现**（其中 3 条本地已有同形占位，必须先删占位）；
- **6 条已注册为 `not_implemented` stub**（恒 501，`routes/issues.rs`），M3 必须**替换**——它们被 §9.7 的 parity 盲区算作「已实现」，**不能**当成已完成。

> 即：M3 的实际实现工作量 = 90 条全做，没有一条可以“跳过”。

工时口径：plan1 §5 的 W3 = **8 人周**，D4 拆「3 子波（4+4）」。本计划按 3 子波切（§3），子波一 ≈ 2.5 人周、子波二 ≈ 3 人周、子波三 ≈ 2.5 人周。

**M3 明确不做**（避免切片范围蔓延）：cloud-runtime 11 条（→M9）；`M3+` 17 条（§9.4，其中 quick-actions 5 条依赖 M3 完成后才可立项）；adapter 的第 2/3 批（在子波三内分批推进，不越子波）；任何计费与配额结算（`task_usage*` 的**读**面在 M3，写面与报表出 M9/analytics）；plugin/skill 的**内容**（只在 M3-7 保留 daemon 通道，§1.5 注）。

---

## 1. 上游 M3 路由全集（method + path + handler + `router.go` 行号）

行号为上游快照 `f41fae6b`（`router.go` 共 2610 行 / 448 个注册调用）。七个分族合计 **101** 条，与 `docs/fixtures/upstream-routes.tsv` 里 owner=`M3` 的行数逐条一致（可用 `grep -c` 复算，见 §10.1）。

### 1.1 runtime-profiles（6）

| method | path | handler | 行 |
| --- | --- | --- | --- |
| GET | `/api/workspaces/{id}/runtime-profiles` | `h.ListRuntimeProfiles` | L1680 |
| GET | `/api/workspaces/{id}/runtime-profiles/{profileId}` | `h.GetRuntimeProfile` | L1681 |
| POST | `/api/workspaces/{id}/runtime-profiles` | `h.CreateRuntimeProfile` | L1717 |
| PATCH | `/api/workspaces/{id}/runtime-profiles/{profileId}` | `h.UpdateRuntimeProfile` | L1718 |
| PUT | `/api/workspaces/{id}/runtime-profiles/{profileId}` | `h.UpdateRuntimeProfile` | L1719 |
| DELETE | `/api/workspaces/{id}/runtime-profiles/{profileId}` | `h.DeleteRuntimeProfile` | L1720 |

> GET 两条（1680/1681）与写四条（1717–1720）在 `router.go` 里相隔 36 行、**不在同一个 `r.Route` 块内** —— 实现时别按块拆文件，按方法族拆。

### 1.2 runtimes（17）

| method | path | handler | 行 |
| --- | --- | --- | --- |
| GET | `/api/runtimes/` | `h.ListAgentRuntimes` | L2266 |
| PATCH | `/api/runtimes/{runtimeId}/` | `h.UpdateAgentRuntime` | L2268 |
| GET | `/api/runtimes/{runtimeId}/usage` | `h.GetRuntimeUsage` | L2269 |
| GET | `/api/runtimes/{runtimeId}/usage/by-agent` | `h.GetRuntimeUsageByAgent` | L2270 |
| GET | `/api/runtimes/{runtimeId}/usage/by-hour` | `h.GetRuntimeUsageByHour` | L2271 |
| GET | `/api/runtimes/{runtimeId}/activity` | `h.GetRuntimeTaskActivity` | L2272 |
| POST | `/api/runtimes/{runtimeId}/update` | `h.InitiateUpdate` | L2273 |
| GET | `/api/runtimes/{runtimeId}/update/{updateId}` | `h.GetUpdate` | L2274 |
| POST | `/api/runtimes/{runtimeId}/models` | `h.InitiateListModels` | L2275 |
| GET | `/api/runtimes/{runtimeId}/models/{requestId}` | `h.GetModelListRequest` | L2276 |
| POST | `/api/runtimes/{runtimeId}/local-skills` | `h.InitiateListLocalSkills` | L2277 |
| GET | `/api/runtimes/{runtimeId}/local-skills/{requestId}` | `h.GetLocalSkillListRequest` | L2278 |
| POST | `/api/runtimes/{runtimeId}/local-skills/import` | `h.InitiateImportLocalSkill` | L2279 |
| GET | `/api/runtimes/{runtimeId}/local-skills/import/{requestId}` | `h.GetLocalSkillImportRequest` | L2280 |
| DELETE | `/api/runtimes/{runtimeId}/` | `h.DeleteAgentRuntime` | L2281 |
| POST | `/api/runtimes/{runtimeId}/unbind-agents-and-delete` | `h.UnbindAgentsAndDeleteRuntime` | L2289 |
| POST | `/api/runtimes/{runtimeId}/archive-agents-and-delete` | `h.UnbindAgentsAndDeleteRuntime` | L2293 |

> `Initiate*` / `Get*Request` 8 条是**异步请求-应答**模式：用户侧发起 → 等 daemon 回 `…/result`（§1.5）→ 用户侧轮询。因此它们**必须**与 daemon 面同波次（§6 M3-7），不能放 §5。
> `unbind/archive-agents-and-delete` 两条共用同一个 handler（L2289/L2293），差异只在 archive 语义。

### 1.3 agents 面（16 = 13 条 `/api/agents` + 3 条 workspace 级统计）

| method | path | handler | 行 |
| --- | --- | --- | --- |
| GET | `/api/agents/` | `h.ListAgents` | L2178 |
| POST | `/api/agents/` | `h.CreateAgent` | L2179 |
| GET | `/api/agents/{id}/` | `h.GetAgent` | L2186 |
| PUT | `/api/agents/{id}/` | `h.UpdateAgent` | L2187 |
| POST | `/api/agents/{id}/archive` | `h.ArchiveAgent` | L2188 |
| POST | `/api/agents/{id}/restore` | `h.RestoreAgent` | L2189 |
| POST | `/api/agents/{id}/cancel-tasks` | `h.CancelAgentTasks` | L2190 |
| GET | `/api/agents/{id}/tasks` | `h.ListAgentTasks` | L2191 |
| GET | `/api/agents/{id}/labels` | `h.ListLabelsForAgent` | L2196 |
| POST | `/api/agents/{id}/labels` | `h.AttachLabelToAgent` | L2197 |
| DELETE | `/api/agents/{id}/labels/{labelId}` | `h.DetachLabelFromAgent` | L2198 |
| GET | `/api/agents/{id}/env` | `h.GetAgentEnv` | L2215 |
| PUT | `/api/agents/{id}/env` | `h.UpdateAgentEnv` | L2216 |
| GET | `/api/agent-task-snapshot` | `h.ListWorkspaceAgentTaskSnapshot` | L2318 |
| GET | `/api/agent-activity-30d` | `h.GetWorkspaceAgentActivity30d` | L2329 |
| GET | `/api/agent-run-counts` | `h.GetWorkspaceAgentRunCounts` | L2332 |

### 1.4 agent-builder（4）

| method | path | handler | 行 |
| --- | --- | --- | --- |
| GET | `/api/agent-builder/sessions/` | `h.ListAgentBuilderSessions` | L2224 |
| POST | `/api/agent-builder/sessions/` | `h.CreateAgentBuilderSession` | L2225 |
| PATCH | `/api/agent-builder/sessions/{sessionId}/runtime` | `h.SwitchAgentBuilderRuntime` | L2226 |
| PUT | `/api/agent-builder/sessions/{sessionId}/draft` | `h.SaveAgentBuilderDraft` | L2229 |

### 1.5 daemon 面（36）

| method | path | handler | 行 |
| --- | --- | --- | --- |
| POST | `/api/daemon/register` | `h.DaemonRegister` | L1523 |
| POST | `/api/daemon/deregister` | `h.DaemonDeregister` | L1524 |
| POST | `/api/daemon/heartbeat` | `h.DaemonHeartbeat` | L1525 |
| GET | `/api/daemon/ws` | `h.DaemonWebSocket` | L1526 |
| GET | `/api/daemon/workspaces` | `h.ListDaemonWorkspaces` | L1527 |
| GET | `/api/daemon/workspaces/{workspaceId}/repos` | `h.GetDaemonWorkspaceRepos` | L1528 |
| GET | `/api/daemon/workspaces/{workspaceId}/runtime-profiles` | `h.DaemonListRuntimeProfiles` | L1529 |
| POST | `/api/daemon/tasks/{id}/plugin-hooks` | `h.InvokeAgentPluginHook` | L1534 |
| GET | `/api/daemon/tasks/{id}/plugin-mcp/{contributionId}/credential` | `h.ResolvePluginMCPCredential` | L1537 |
| POST | `/api/daemon/runtimes/{runtimeId}/tasks/claim` | `h.ClaimTaskByRuntime` | L1539 |
| POST | `/api/daemon/tasks/claim` | `h.ClaimTasksByRuntime` | L1543 |
| POST | `/api/daemon/claim` | `h.ClaimTasksByRuntime` | L1544 |
| POST | `/api/daemon/runtimes/{runtimeId}/tasks/{taskId}/prepare-lease` | `h.ExtendTaskPrepareLease` | L1545 |
| POST | `/api/daemon/runtimes/{runtimeId}/tasks/{taskId}/skill-bundles/resolve` | `h.ResolveTaskSkillBundles` | L1546 |
| GET | `/api/daemon/runtimes/{runtimeId}/tasks/pending` | `h.ListPendingTasksByRuntime` | L1547 |
| POST | `/api/daemon/runtimes/{runtimeId}/update/{updateId}/result` | `h.ReportUpdateResult` | L1548 |
| POST | `/api/daemon/runtimes/{runtimeId}/models/{requestId}/result` | `h.ReportModelListResult` | L1549 |
| POST | `/api/daemon/runtimes/{runtimeId}/local-skills/{requestId}/result` | `h.ReportLocalSkillListResult` | L1550 |
| POST | `/api/daemon/runtimes/{runtimeId}/local-skills/import/{requestId}/result` | `h.ReportLocalSkillImportResult` | L1551 |
| GET | `/api/daemon/tasks/{taskId}/status` | `h.GetTaskStatus` | L1553 |
| POST | `/api/daemon/tasks/{taskId}/start` | `h.StartTask` | L1554 |
| POST | `/api/daemon/tasks/{taskId}/wait-local-directory` | `h.MarkTaskWaitingLocalDirectory` | L1555 |
| POST | `/api/daemon/tasks/{taskId}/progress` | `h.ReportTaskProgress` | L1556 |
| POST | `/api/daemon/tasks/{taskId}/complete` | `h.CompleteTask` | L1557 |
| POST | `/api/daemon/tasks/{taskId}/fail` | `h.FailTask` | L1558 |
| POST | `/api/daemon/tasks/{taskId}/usage` | `h.ReportTaskUsage` | L1559 |
| POST | `/api/daemon/tasks/{taskId}/messages` | `h.ReportTaskMessages` | L1560 |
| GET | `/api/daemon/tasks/{taskId}/messages` | `h.ListTaskMessages` | L1561 |
| POST | `/api/daemon/tasks/{taskId}/cancel-ack` | `h.AckTaskCancelled` | L1562 |
| POST | `/api/daemon/workspaces/{workspaceId}/issues/gc-check` | `h.BatchIssueGCCheck` | L1564 |
| GET | `/api/daemon/issues/{issueId}/gc-check` | `h.GetIssueGCCheck` | L1565 |
| GET | `/api/daemon/chat-sessions/{sessionId}/gc-check` | `h.GetChatSessionGCCheck` | L1566 |
| GET | `/api/daemon/autopilot-runs/{runId}/gc-check` | `h.GetAutopilotRunGCCheck` | L1567 |
| GET | `/api/daemon/tasks/{taskId}/gc-check` | `h.GetTaskGCCheck` | L1568 |
| POST | `/api/daemon/runtimes/{runtimeId}/recover-orphans` | `h.RecoverOrphanedTasks` | L1570 |
| POST | `/api/daemon/tasks/{taskId}/session` | `h.PinTaskSession` | L1571 |

> 三条 `…/plugin-hooks`、`…/plugin-mcp/…/credential`、`…/skill-bundles/resolve` 的**内容**属 W6（plugin/skill），但**通道**是 daemon 面：M3 只负责路由存在 + 透传/降级（无 plugin host 时按上游错误码返回），不实现 plugin host（避免越波次）。
> `POST /api/daemon/tasks/{taskId}/session` 是 `PinTaskSession`，与会话固定有关，**依赖 M1-B 的 session store 语义**，实现前先读 `docs/06-M1-AUTH.md`。

### 1.6 task / lifecycle / usage / retry 面（11）

| method | path | handler | 行 |
| --- | --- | --- | --- |
| POST | `/api/client-usage` | `h.UpsertClientUsage` | L1635 |
| POST | `/api/issues/preview-trigger` | `h.PreviewIssueTrigger` | L1970 |
| GET | `/api/issues/{id}/active-task` | `h.GetActiveTaskForIssue` | L1992 |
| POST | `/api/issues/{id}/tasks/{taskId}/cancel` | `h.CancelTask` | L1993 |
| POST | `/api/issues/{id}/rerun` | `h.RerunIssue` | L1994 |
| GET | `/api/issues/{id}/task-runs` | `h.ListTasksByIssue` | L1997 |
| GET | `/api/issues/{id}/usage` | `h.GetIssueUsage` | L1998 |
| GET | `/api/tasks/{taskId}/messages` | `h.ListTaskMessagesByUser` | L2016 |
| POST | `/api/tasks/{taskId}/retry-source-context` | `h.RetrySourceContextQuickCreate` | L2017 |
| POST | `/api/tasks/{taskId}/cancel` | `h.CancelTaskByUser` | L2314 |
| GET | `/api/working-agents` | `h.ListWorkspaceWorkingAgents` | L2324 |

> 这 11 条里有 **6 条**正是 `docs/10 §2` M2-A「不做」清单移交过来的（preview-trigger / active-task / task-runs / rerun / `{id}/tasks/{taskId}/cancel` / usage）—— 移交判断正确，M3 接收，`docs/10` 原文不改（本文件即为接收凭证）。
>
> 但这 **6 条已经以 501 占位形式注册在 `crates/mc-http/src/routes/issues.rs`**（L91 / L123 / L124 / L125 / L126 / L133，handler `not_implemented`，其注释原文即“501；依赖 agent/squad/task/附件/table 等 M3 能力”）。所以 M3-6 的任务不是“加路由”而是**把 6 个 stub 换成真实现**——同时必须把 handler 名改掉，否则 route_parity 的占位计数（§9.7）会继续把它们当“已实现”。

### 1.7 cloud-runtime（11，**判给 W9/M9，M3 不做**）

| method | path | handler | 行 |
| --- | --- | --- | --- |
| GET | `/api/cloud-runtime/` | `h.GetCloudRuntimeService` | L2300 |
| GET | `/api/cloud-runtime/healthz` | `h.GetCloudRuntimeHealth` | L2301 |
| GET | `/api/cloud-runtime/readyz` | `h.GetCloudRuntimeReady` | L2302 |
| GET | `/api/cloud-runtime/nodes` | `h.ListCloudRuntimeNodes` | L2303 |
| POST | `/api/cloud-runtime/nodes` | `h.CreateCloudRuntimeNode` | L2304 |
| DELETE | `/api/cloud-runtime/nodes` | `h.DeleteCloudRuntimeNode` | L2305 |
| POST | `/api/cloud-runtime/nodes/start` | `h.StartCloudRuntimeNode` | L2306 |
| POST | `/api/cloud-runtime/nodes/stop` | `h.StopCloudRuntimeNode` | L2307 |
| POST | `/api/cloud-runtime/nodes/reboot` | `h.RebootCloudRuntimeNode` | L2308 |
| POST | `/api/cloud-runtime/nodes/status` | `h.GetCloudRuntimeNodeStatus` | L2309 |
| POST | `/api/cloud-runtime/nodes/exec` | `h.ExecCloudRuntimeNode` | L2310 |

上游 handler 体量：`cloud_runtime.go` 208 行 —— 属 W9 的 `mc-cloud`，M3 不碰（裁决依据见 §9.1）。

### 1.8 本地实况对照（@`0af39ab` 实测）

`python3 scripts/route_parity.py`（exit 0）：上游 456 / 本地注册 140 / baseline 139 / **implemented 112 real + 13 placeholder = 125** / known_gap 331（其中 **M3 92**）/ unclaimed 0 / regression 0 / local_only 12。

但 `112 real` 这个数字**虚高**：其中 **23 条**的 handler 是 `not_implemented`（恒 501），只因 parity 的占位正则只认字面量 `placeholder` 而未被识别（§9.7）。按 handler 实测的分解：

| 口径 | 条数 | 说明 |
| --- | ---: | --- |
| 真实现（真 handler） | **89** | 112 − 23 |
| 501 stub（`not_implemented`） | 23 | 全部在 `routes/issues.rs`；其中 **M3 占 6 条** |
| M0 占位（`health::placeholder`） | 13 | 其中 **M3 占 3 条** |

M3 归属的 101 条：**9 条被“满足”**（3 占位 + 6 stub，**无一是真实现**）+ **92 条为 gap**（§9.7 注）。

M3 相关的本地既有物：

- 占位路由（**M3 必须删**，规则见 §7.3）：`GET|POST /api/agents`（`mount.rs:44-47`，与上游 `/api/agents/` **不同注册键**，不 panic 但是幽灵 501）、`GET|POST /api/runtimes`（`mount.rs:50-53`；其中 `POST /api/runtimes` 上游压根没有）。
- stub 路由（**M3 必须替换**）：§1.6 的 6 条（`issues.rs:91/123/124/125/126/133`）。
- 本地自造（`local_only`，M3 不动）：`GET /api/issues/:id/reactions`、`GET /api/issues/:id/quick-actions`（后者上游归 `M3+`）。
- 骨架遗留（**M3-2 替换**）：`crates/mc-http/src/state.rs::AdapterRegistryStub`（注释原文 "full version in M3"），`ConfigSnapshot` 尚无 runtime 段，`mc_config::RuntimeConfig` 已存在但没接线（§7.6）。

---

## 2. schema 现状：M3 的硬阻塞（2026-09-22 @`0af39ab` 实测）

**P4 硬约束**：表一律来自 `migrations/upstream/*`，**禁止手写表**。因此本节结论直接决定「哪些切片现在能开」。

### 2.1 表存在性

M3 需要的表共 16 张（口径：`contracts/upstream-schema.sql` 的 116 张 head 表集）：

| M3 相关表 | 上游 head | 本地 `migrations/0001–0004` |
| --- | --- | --- |
| `agent` / `agent_runtime` / `agent_task_queue` / `runtime_profile` | ✅ | ✅（**但形状不同，见 §2.2**） |
| `daemon_connection` / `daemon_token` | ✅ | ❌ |
| `task_message` / `task_token` | ✅ | ❌ |
| `task_usage` / `task_usage_hourly` / `task_usage_hourly_dirty` / `task_usage_hourly_rollup_state` | ✅ | ❌ |
| `client_usage_daily` / `agent_invocation_target` | ✅ | ❌ |
| `agent_skill` / `agent_mcp_server` / `agent_to_label` / `agent_builder_draft` | ✅ | ❌ |

本地现有 28 张表（26 张与上游同名 + 自造 `plugin` / `wakeup`），覆盖率 26/116 ≈ 22%。

> ⚠️ **本节描述的是 W0-B2（LUM-1387）之前的状态**。切换之后运行时应用的就是上游 116 张表本身，
> `plugin` / `wakeup` 不再存在（上游对应物是 `issue_wakeup` / `issue_wakeup_receipt`），§2.2 的「同表不同形」也已经消失——
> 所以 M3 可以在真正的上游表上干活。见 `docs/26-W0-SCHEMA-SWITCHOVER.md`。
> 仍然成立的部分：**本地不再有 4 张「形状不同」的 agent/task 表**，M3 的 port/领域代码不用再绕。

### 2.2 ⚠️ 同表不同形：本仓那 4 张「有」的表，**都不是上游那张表**

这是 M0 脚手架 `0001_init.up.sql` 按**推断**写的表，与上游同名表的定义相差很远（`docs/25 §5.2/§5.3` 的 `missing/extra/differs` 实测）：

| 表 | 上游缺失列 | 本仓多余列 | 真实差异（摘） | 后果 |
| --- | ---: | ---: | --- | --- |
| `agent_task_queue` | **46** | 7 | 上游 6 条缺失约束；本仓多 `retry_count`/`source_task_id` 等自造列 | **拿本地表实现 task queue = 全部返工** |
| `agent` | 13 | 1 | `max_concurrent_tasks` 默认 `1`→上游 `6`；`visibility` 默认 `'workspace'`→`'private'`；`description` 上游 `NOT NULL` | Agent CRUD 的默认值与必填校验会全错 |
| `runtime_profile` | 11 | 4 | **本地主键是 `name TEXT`（26 profile 目录）**，上游是 `id UUID` + `workspace_id` + `display_name` + `protocol_family`(25 值 CHECK) + `command_name` + `fixed_args` + `created_by` + `enabled`（见 `migrations/upstream/120_runtime_profile.up.sql`） | **两张完全不同的表同名**，profile CRUD 无法在本地表上实现 |
| `agent_runtime` | 7 | 6 | `visibility` 默认值不同；缺 `profile_id`（上游 120 迁移新增）与 `agent_runtime_workspace_daemon_profile_key` 部分唯一索引 | runtime 删除/绑定语义无法对齐 |

具体到任务表和领域语义，上游的真值来自迁移而非本地表：

- 生命周期守卫：`022_task_lifecycle_guards` 建了「每 issue 至多一条 `queued|dispatched` 任务」的**部分唯一索引** `idx_one_pending_task_per_issue` —— 本仓既无该状态值也无该索引。
- 重试/租约列：`055_task_lease_and_retry` 给 `agent_task_queue` 加 `attempt` / `max_attempts` / `parent_task_id` / `failure_reason` / `last_heartbeat_at` —— 本仓一个都没有，却自造了 `retry_count` / `source_task_id`。
- 结算：`032_task_usage` → `073/077/084/101` → `103_drop_legacy_daily_rollups` 的最终形态是 `task_usage` + `task_usage_hourly{,_dirty,_rollup_state}`（`task_usage_daily` 已被 103 删掉）。

### 2.3 任务状态机的**唯一真值来源**

上游 `pkg/protocol/events.go` 用注释把状态迁移写死在事件常量上（L34–L42）：

```
task:queued                  ∅ → queued
task:dispatch                queued → dispatched            （daemon claim）
task:running                 dispatched → running
task:waiting_local_directory dispatched → waiting_local_directory
task:progress / task:message 运行期附加信息
task:completed               running → completed
task:failed                  running → failed
task:cancelled               * → cancelled
```

而本仓 `agent_task_queue.status` 的 CHECK 是 `(queued, running, terminal_completed, terminal_cancelled, terminal_failed, delegated_failure)` —— **缺 `dispatched` 与 `waiting_local_directory`**。M3-3（§4.3）必须以事件表 + `022`/`055` 为状态机真值，**不得**把本地 CHECK 当契约。

### 2.4 依赖结论

```
                    ┌─ M3-1 daemon 协议冻结 ──┐   （零 DB 依赖，现在可开）
W3a（子波一）───────┼─ M3-2 runtime 抽象/adapter ┤
       现在可并行 3 片 └─ M3-3 task 领域层 ──────┘
                                    │
W0-B2 (LUM-1387, backlog) ──────────┼──► W3b（子波二）M3-4 / M3-5 / M3-6   路由面
   apply-set 切到 upstream + 6 处     │
   text→uuid 列修正                  └──► W3c（子波三）M3-7 ──► M3-8         daemon 面 / adapters
                                              （另需 M3-1 的冻结协议）
```

**结论**：

1. **凡是要落库的 M3 路由，一律等 W0-B2**（LUM-1387）。在切换前实现 = §2.2 那种「对着自造表写一遍再返工」，正是 `docs/25` 警告的失效模式。
2. **子波一三片零 DB 依赖**，可以现在就立项并行 —— 这是本计划给出的「2–3 个可并行 slice」。
3. 若 W0-B2 迟迟不排期，可选的**替代路径**是：把 §5 的 M3-4/5/6 降级为「只写 Repo 层 `Row/NewX/UpdateX/Filter` + 以上游列名为准的 SQL，跑 `--ignored` DB 测试」——但**本计划不推荐**，因为 `mc-repos` 每张表的 `FromRow` 要逐列手写（`mc_core::Id` 无 sqlx impl），列名猜错不会编译报错只会运行时错。

---

## 3. 子波划分总览

| 子波 | 切片 | 分支 | 路由 | 前置 | 可并行 |
| --- | --- | --- | ---: | --- | --- |
| **W3a** 协议与抽象 | M3-1 daemon 协议冻结 | `feat/multica-rs-m3a-daemon-proto` | 0（定 body） | 无 | ✅ 3 片同时 |
| | M3-2 runtime 抽象 + pi-local adapter | `feat/multica-rs-m3a-runtime-adapter` | 0 | 无 | ✅ |
| | M3-3 task 领域层 | `feat/multica-rs-m3a-task-domain` | 0 | 无 | ✅ |
| **W3b** 台账与生命周期面 | M3-4 runtime-profile + runtimes 台账 | `feat/multica-rs-m3b-runtime-profiles` | 15 | W0-B2 + M3-1 | ✅ 3 片同时 |
| | M3-5 agent 面 | `feat/multica-rs-m3b-agents` | 16 | W0-B2 | ✅ |
| | M3-6 agent-builder + task 用户面 | `feat/multica-rs-m3b-task-queue` | 15 | W0-B2 + M3-3 | ✅ |
| **W3c** daemon 与适配器 | M3-7 daemon 面 + ws 服务端 | `feat/multica-rs-m3c-daemon` | 44（36+8） | W0-B2 + M3-1 + M3-3 | 与 M3-8 串行 |
| | M3-8 execenv + adapters 分批 | `feat/multica-rs-m3c-adapters` | 0 | M3-2 + M3-7 | 3 批 × 串行 |
| **集成** | M3 集成 | （复用集成分支流程） | — | W3a+W3b+W3c | 单独 cycle |

合计覆盖 90 条 M3 路由（15+16+15+44 = 90 ✅，与 §0 的 101−11 一致）。按 §1.8 的实测状态拆：**84 条全新实现 + 6 条 501 stub 替换**（那 6 条全在 M3-6）；另有 **3 条需先删 M0 占位**（`GET|POST /api/agents` → M3-5，`GET /api/runtimes` → M3-4）。

文档编号（`docs/15/16/…` 由本计划预占；若并行切片已占用，顺延取下一个空号并在 PR 说明）：`15` 本文件、`16` 协议冻结、`18` runtime/adapter、`19` task 领域、`26` runtime 台账、`27` agent 面、`31` task 用户面、`32` daemon 面、`33` adapters、`34` M3 集成手册。

**编号补充登记（LUM-1437 / W3c 预飞，不改上面任何裁决）**：`35`/`36` = W3a/W3b 预飞（已落）；`37` = **W3c 预飞**（本计划 §3 W3c 的写集实测，见 `docs/37-M3-W3C-PREFLIGHT.md`）；`38` = **ws 传输层预切片**文档（若该预切片派发，见 `docs/37` §4）。`27`/`31` 两个号在本计划写出后已被别的文档占用（`docs/27` = W0 golden fixtures），`32`/`33`/`34` 仍按上表由 M3-7/M3-8/M3 集成使用。

plan1 §3.3 给 W3 列的 crate 与本计划的对应关系（四个职责一个不落）：

| plan1 §3.3 的 W3 条目 | 落在哪个切片 |
| --- | --- |
| `mc-runtime` — profile | M3-4（路由 + repo）+ scaffold 建 crate |
| `mc-runtime` — catalog（25 项白名单/launch header） | M3-2 |
| `mc-runtime` — liveness（版本探测/在线状态） | M3-2（probe）+ M3-7（`agent_runtime.status` 维护） |
| `mc-runtime` — quota | M3-3（`max_concurrent_tasks_per_agent` 的判定纯函数）+ M3-7（claim 路径上执行） |
| `mc-agent` | M3-5（若需领域逻辑才建 crate，否则并入 repo） |
| `mc-task` | M3-3 |
| `mc-daemon` — client | M3-7 |
| `mc-daemon` — execenv | M3-8 |
| daemon 协议（`pkg/protocol`） | M3-1 |

---

## 4. 子波一 W3a —— 现在即可并行 3 片（零 schema 依赖）

三片都不落库、不注册 HTTP 路由，因此**不受 W0-B2 阻塞**，且互为前置（M3-2/M3-3 的产类型由 M3-1 定型、M3-7 消费三片产物）。

### M3-1 daemon 协议冻结 —— `feat/multica-rs-m3a-daemon-proto`

- **基线**：晋升时 `feat/multica-rs-initial` 最新 commit。
- **Repo**：新 crate `crates/mc-daemon-proto`（src/lib.rs 单文件起步，超 800 行按 `messages/`、`events/`、`rpc/` 拆）。
- **上游来源**：`server/pkg/protocol/messages.go`（42 个 payload 类型 + `const` 块）、`server/pkg/protocol/events.go`（事件常量）、`server/internal/daemonws/hub.go`（帧封装与 method 分发）。
- **路由**：**0 条**（本片只定型 body/帧，不注册路径）；但它决定 §1.5 的 36 条与 §1.2 的 8 条异步路由的请求/响应形状。
- **测试**：①每个 payload 类型的 serde 往返 ≥1 例；②**逐字**从上游 Go 结构体抄 3 份 golden JSON（字段名、可选性、`omitempty` 语义必须一致）做反序列化断言；③未知字段容忍（`#[serde(default)]`）+ 未知 event 忽略；④版本/能力协商 1 例；⑤RPC method 名称表与上游 `hub.go` 的字符串逐条比对。
- **文档**：`docs/16-M3-DAEMON-PROTOCOL.md` —— 必须包含**冻结契约表**（method → 请求/响应类型 → 幂等性 → 错误码）+ 版本策略 + 变更流程（冻结后改协议要走 §8.4 仲裁）。
- **范围限制**：不实现 ws 传输/连接管理（`mc-ws` 与 axum ws 属 M3-7）；不实现 `hub`/`notifier`/`metrics`；不落库；不引 `tokio-tungstenite`（属 M3-7，依赖由 scaffold 预声明，§7.5）。
- **验收标准**：`bash scripts/gates.sh` 全绿；`docs/16` 的契约表覆盖 §1.5 全部 36 条 + §1.2 的 8 条 `Initiate*`/`Get*Request` 的返回体；"冻结"声明与本片 commit sha 同时写入 `docs/16` 头部。
- **晋升条件**：无依赖，立即可晋升 `todo`。

### M3-2 runtime 抽象 + pi-local adapter 端到端 —— `feat/multica-rs-m3a-runtime-adapter`

- **Repo**：新 crate `crates/mc-runtime`（plan1 §3.3 已列此 crate）。
- **内容**：①`RuntimeAdapter` trait（launch / stream / cancel / probe-version / capabilities）；②`AdapterRegistry`——**替换** `crates/mc-http/src/state.rs::AdapterRegistryStub` 与其 7 个调用点；③adapter 元数据表（**以 `pkg/agent/agent.go::SupportedTypes` 的 25 项为准**，见 §9.3；launch header 从同文件的 `launchHeaders` 逐条抄）；④**一致性测试套件**（宏，`adapter_conformance!(PiLocal)`，plan1 R4 要求的统一套件）；⑤**只做 1 个 adapter：pi-local**。
- **路由**：0 条。
- **测试**：一致性套件对 pi-local 全绿；端到端 1 例（用 `sh -c` 假 CLI 走 spawn → stdout 流 → progress → 退出码 → complete 全生命周期，**不落库**）；取消 1 例；超时 1 例；非零退出码映射为 `failure_reason='agent_error'` 1 例；registry 并发注册 1 例。
- **文档**：`docs/18-M3-RUNTIME-ADAPTER.md`（trait 契约、一致性套件用法、加一个 adapter 的 6 步、25 项白名单表）。
- **范围限制**：**不**复制第 2 个 adapter（plan1 R4 + 本 issue「不要把 26 个当一条切片」）；不做配额/计费；不做 Windows 分支（R3：Linux-first，`#[cfg(windows)]` 独立模块延后）；不再自造 profile 目录（`RuntimeConfig` 已存在，§7.6）。
- **验收标准**：plan1 §5 的 W3 门禁之一「**pi-local adapter 端到端跑通一次真实任务**」在本片达成（无 DB 版本）；`bash scripts/gates.sh` 全绿；一致性套件是**宏/模板**而非逐 adapter 复制粘贴（R4 的验收点，PR 里给出第 2 个 adapter 的「预计 diff 行数」估算）。
- **晋升条件**：无依赖（与 M3-1 并行）。**唯一写 `state.rs` 的切片**（§7.1）。

### M3-3 task 领域层状态机 —— `feat/multica-rs-m3a-task-domain`

- **Repo**：新 crate `crates/mc-task`。
- **内容**：①状态机（真值 = §2.3 的事件表 + `022`/`055`）：`queued→dispatched→running→{waiting_local_directory}→terminal_*`，含 `delegated_failure`（`terminal_failed` 的子类）；②租约（`prepare-lease` 语义、`lease_expires_at`、`last_heartbeat_at`、过期扫描）；③重试（`attempt`/`max_attempts`/`parent_task_id`/`failure_reason` 的 5 分类 `agent_error|timeout|runtime_offline|runtime_recovery|manual` + 自动重试判定）；④取消与 `cancel-ack`；⑤usage **结算纯计算**（token/时长 → `task_usage` 行形状，按 `032`+`101` 的列）。
- **路由**：0 条；`TaskStore` 以 trait（port）形式给出，SQL 实现在 M3-6，内存实现**只允许出现在 `test_state()`**（plan1 §3.4）。
- **测试**：状态迁移矩阵（合法迁移逐条 + 非法迁移必须 `Err`）；租约到期/续租；重试上限与 `failure_reason` 判定表；取消幂等；结算归一化 ≥3 个形状。属性测试（`proptest`，已在 workspace 依赖里）覆盖「任意事件序列不产生非法状态」。
- **文档**：`docs/19-M3-TASK-DOMAIN.md`（状态机图 + 迁移表 + failure_reason 判定 + 结算公式）。
- **范围限制**：不写 SQL、不写路由、不写 daemon 侧；**不得**引入本仓自造列（`retry_count`/`source_task_id` 等 7 列，§2.2）；不碰 `agent_task_queue` 的 DDL（属 W0-B2）。
- **验收标准**：`bash scripts/gates.sh` 全绿；§2.3 的 8 条事件迁移逐条有对应用例；`docs/19` 的迁移表与上游 `events.go` L34–L42 逐行可核对。
- **晋升条件**：无依赖（与 M3-1/M3-2 并行）。

---

## 5. 子波二 W3b —— W0-B2 合入后并行 3 片（路由面）

三片共同前置：**W0-B2（LUM-1387）已合入**（apply-set 切到 upstream + 6 处 `text`→`uuid` 修正），且 `migrations/upstream/` 的 M3 表已在本仓可建（`mc-migrate` 的 `DEFAULT_REQUIRED_TABLES` 里 `"wakeup"` 需已被 W0-B2 改成 `issue_wakeup`，见 docs/25 §6.3.1）。

### M3-4 runtime-profile + runtimes 台账面（15 条）—— `feat/multica-rs-m3b-runtime-profiles`

- **Repo**：`crates/mc-repos/src/runtime.rs`（新文件，scaffold 预置 `pub mod`）；`Row/NewRuntimeProfile/UpdateRuntimeProfile/Filter` 照 M1/M2 约定 + Memory/Pg 双实现（`mc_core::Id` 无 sqlx impl ⇒ 手写 `FromRow`）。
- **路由**：`crates/mc-http/src/routes/runtimes.rs`，挂入 `mount_slice_runtime()`。覆盖 §1.1 全 6 条 + §1.2 的 9 条（list / patch / delete / activity / usage×3 / unbind / archive）。同时**删除** `mount.rs:50-53` 的 `/api/runtimes` 占位（`get().post()` 整块；上游只有 `GET /api/runtimes/`，`POST` 无对应物；删除导致 2 条 regression，集成时刷新，见 §7.3）。
- **不做**（留给 M3-7）：§1.2 的 8 条异步往返路由（`update`×2、`models`×2、`local-skills`×4）—— 它们等 daemon 侧回包。
- **测试**：repo ≥5（profile CRUD、`UNIQUE(workspace_id, display_name)` 冲突、`protocol_family` CHECK 拒绝非法值、删除 profile 后 runtime 行的应用层清理、按 workspace 过滤）；路由 e2e ≥4（CRUD 往返、usage 聚合、unbind/archive 的副作用差异、403/404）。DB 用例接 `MULTICA_TEST_DATABASE_URL`，**不允许静默跳过**。
- **文档**：`docs/26-M3-RUNTIME-PROFILES.md`。
- **范围限制**：不新增迁移；不改 `protocol_family` 白名单来源（读 `SupportedTypes` 的等价常量，§9.3）；不实现 runtime 的 `profile_id` 绑定逻辑（属 M3-7 注册路径）。
- **验收标准**：`bash scripts/gates.sh --with-db` 全绿（含 `schema-drift` 门）；§1.1 的 6 条逐条 e2e；§1.2 抽查 ≥3 条与上游行号一致。
- **晋升条件**：W0-B2 合入 + M3-1 合入。

### M3-5 agent 面（16 条）—— `feat/multica-rs-m3b-agents`

- **Repo**：`crates/mc-repos/src/agent.rs`（新文件）；`crates/mc-agent` 新 crate 只在需要领域逻辑时创建（否则本片仅 repo + 路由，避免空 crate）。
- **路由**：`crates/mc-http/src/routes/agents.rs`，挂入 `mount_slice_agent()`。覆盖 §1.3 全部 16 条（含 `labels` 3 条 → `agent_to_label`；`env` 2 条；`cancel-tasks`）。同时**删除** `mount.rs:44-47` 的 `/api/agents` 占位（`get().post()` 整块；删除导致 2 条 regression，集成时刷新，见 §7.3）。
- **不做**：`/api/agents/{id}/skills*`（6 条 → M6）、`mcp-servers*`（4 条 → M8）、`dingtalk/groups`（1 条 → M7）、`/api/agents/mika`（1 条 → M9）—— 见 §9.5。
- **测试**：repo ≥5（create 默认值 `max_concurrent_tasks=6`/`visibility='private'` 必须与上游一致 —— 正是 §2.2 的坑、archive/restore 幂等、labels 关联唯一、env 读写、stat 聚合）；路由 e2e ≥4（CRUD、archive→restore、cancel-tasks 与 M3-3 状态机联动、labels 三连）。
- **文档**：`docs/27-M3-AGENTS.md`。
- **范围限制**：不碰 `routes/issues.rs`（2224 行，见 §7.7）—— `agents/{id}/tasks` 用 `TaskRepo` 的查询接口，跨切片只读依赖 M3-3 的 trait。
- **验收标准**：`bash scripts/gates.sh --with-db` 全绿；默认值与上游列默认值逐条对齐（PR 里贴 `contracts/upstream-schema.sql` 的对应片段）。
- **晋升条件**：W0-B2 合入。

### M3-6 agent-builder + task 用户面（15 条）—— `feat/multica-rs-m3b-task-queue`

- **Repo**：`crates/mc-repos/src/task.rs`（新文件，`TaskRepo` 的 Pg 实现，兑现 M3-3 的 `TaskStore` port）；`agent_builder_draft` 的读写可并入同 repo 或独立小文件。
- **路由**：`crates/mc-http/src/routes/tasks.rs`，挂入 `mount_slice_task()`。覆盖 §1.4 全 4 条 + §1.6 全 11 条。**其中 6 条（preview-trigger / active-task / rerun / task-runs / `{id}/tasks/{taskId}/cancel` / `{id}/usage`）已在 `routes/issues.rs:91/123/124/125/126/133` 以 `not_implemented` stub 形式注册**：不能新增同名路由（axum 重复注册 panic），做法是把 handler 换成真实现并将路由移入本片 router（或保留在原处但换 handler）。
- **测试**：repo ≥5（入队→claim→完成、`idx_one_pending_task_per_issue` 的部分唯一索引冲突、重试链 `parent_task_id`、`task_message` 分页、usage 聚合）；路由 e2e ≥5（preview-trigger、rerun、cancel 两处入口幂等、active-task/task-runs、usage、client-usage 落 `client_usage_daily`）。
- **文档**：`docs/31-M3-TASK-QUEUE.md`。
- **范围限制**：不做 `…/gc-check` 5 条 + `recover-orphans`（daemon 面，M3-7）；不做计费；`retry-source-context` 只做「取回 source context」不触发实际重跑（重跑在 M3-7 的 daemon 回路）。
- **验收标准**：`bash scripts/gates.sh --with-db` 全绿；§1.6 的 11 条逐条 e2e；`022`/`055` 的约束与列在 DB 测试里被真实触发（不是 mock）。
- **晋升条件**：W0-B2 合入 + M3-3 合入（用其状态机与 port）。

---

## 6. 子波三 W3c —— daemon 面与适配器（串行两刀）

### M3-7 daemon 面 + ws 服务端（44 条）—— `feat/multica-rs-m3c-daemon`

- **Repo**：`crates/mc-daemon`（client + 注册/心跳/claim 状态）；`crates/mc-repos/src/daemon.rs`（`daemon_connection` / `daemon_token` / `task_message` / `task_usage` 读写）；ws 服务端放 `crates/mc-ws`（现 102 行，扩到 hub/notifier）+ `mc-realtime`（已有 envelope，165 行）。
- **路由**：`crates/mc-http/src/routes/daemon.rs` + 分包。覆盖 §1.5 全 36 条 + §1.2 的 8 条异步往返（`update`/`models`/`local-skills` 的发起端与轮询端）。
- **上游体量**：`internal/daemon/daemon.go` 6056 行、`internal/daemonws/*` 3 文件 1370 行、`daemon_ws.go` 127、`daemon_rpc.go` 90、`daemon_workspace.go` 87。**本片必然撞 800 行上限** ⇒ 预先按域拆 6 个文件（`daemon/{register,heartbeat,claims,tasks,requests,gc}.rs`），每个 ≤800 行。
- **测试**：ws 握手/心跳/断线重连 ≥3；claim 幂等 + 并发 claim 只发一次；`prepare-lease`/`record progress`/`complete`/`fail`/`cancel-ack` 各 ≥1；`gc-check` 5 条 ≥1；`recover-orphans` 1（孤儿任务按 `055` 的 `runtime_recovery` 分类）；异步请求-应答（`Initiate*` → `…/result` → `Get*`）≥1 条完整回路。
- **文档**：`docs/32-M3-DAEMON-FACE.md`。
- **范围限制**：plugin/skill 内容只做通道与降级（§1.5 注）；不做 `cloud-runtime`；不做 execenv（M3-8）。
- **验收标准**：`bash scripts/gates.sh --with-db` 全绿；§1.5 抽查 ≥3 条；**daemon 回路 e2e**：本机起一个 stub daemon（复用 M1 测试里 `TcpListener` + axum 的 stub 手法，docs/20 recipe）跑通 register → claim → start → progress → complete。
- **晋升条件**：W0-B2 + M3-1（协议冻结）+ M3-3（状态机）；**唯一写 `mc-ws`/`mc-realtime` 的切片**。

### M3-8 execenv + 25 个 adapter 分批 —— `feat/multica-rs-m3c-adapters`

- **Repo**：`crates/mc-daemon/src/execenv/`（上游 `server/internal/daemon/execenv/` **99 文件**，含 codex home 链接/沙箱/技能剥离/Windows 分支）+ `crates/mc-runtime/src/adapters/`。
- **路由**：0 条（这一片的产出是执行环境与后端，被 M3-7 的 handler 调用）。
- **分批（硬约束）**：**3 批 × 8/8/9**，每批一个 PR，批次内容按协议族聚类（例：批 1 = claude/codex/copilot/opencode/codebuddy/codearts/deveco/pi 线的 CLI+ACP；批 2 = cursor/kimi/kiro/antigravity/qoder/qoderclicn/traecli/grok；批 3 = qwen/qwenpaw/openclaw/hermes/reasonix/dsh/dim/mcode/zeroclaw）。每批交付 = 复用 M3-2 的宏 + 一致性套件 + 该批的真实 `--version` 探测 E2E。
- **测试**：一致性套件逐 adapter 全绿；批次内至少 1 个 adapter 做真实 CLI 探测（有二进制时），无二进制时用假 CLI（记 `#[ignore]` + 理由）。
- **文档**：`docs/33-M3-ADAPTERS.md`（25 项白名单 + 分 3 批的清单 + 每个 adapter 的 launch header）。
- **范围限制**：**不得**把 25 个 adapter 当一个切片（plan1 R4 + 本 issue 硬约束）；Linux-first，Windows 走 `#[cfg(windows)]` 独立模块延后（R3）；不引渠道 SDK（R5）。
- **验收标准**：一致性套件对全部已实现的 adapter 绿；每批 PR 互相独立可回滚；`bash scripts/gates.sh` 全绿。
- **晋升条件**：M3-2（宏/套件）+ M3-7（调用方）；批 2/3 在批 1 合入后逐批晋升。

---

## 7. 共享锚点与冲突热点表

### 7.1 锚点表（照 `docs/10 §3` 形式）

| 锚点 | 谁写 | 热点程度 | M3 规则 |
| --- | --- | --- | --- |
| `crates/mc-repos/src/lib.rs` 的 `pub mod` 三行 | scaffold | 🔴 三片相邻行 | scaffold 一次性加 `agent`/`runtime`/`task`，切片只填自己的文件 |
| `crates/mc-http/src/routes/mod.rs` 的 `pub mod` 四行 | scaffold | 🔴 | 同上（`agents`/`runtimes`/`tasks`/`daemon`） |
| `crates/mc-http/src/routes/mount.rs` `mount_slice_*` | scaffold（加）+ 各切片（填函数体） | 🟡 | 照 M1-D `4aa275a` 手法：一次性接好空切片；**占位删除**另按 §7.3 |
| `mount.rs` 的 M0 占位行 | 对应切片 | 🔴 每片都要删自己那两条 | `GET|POST /api/agents`、`GET|POST /api/runtimes`；**不同注册键（有无尾斜杠），不 panic**，但会留下恒 501 幽灵路由（§7.3） |
| `routes/issues.rs` 的 6 条 `not_implemented` stub | **仅 M3-6** | 🟡 | 从 L91/123/124/125/126/133 原地替换，不要新建同名路由（会重复注册 panic） |
| `crates/mc-http/src/state.rs` | scaffold + **仅 M3-2** | 🔴 | 其余切片**不得**改本文件；需要新字段走 §7.6 |
| `migrations/` 编号 | **M3 不写** | 🟢 | 全部表来自 `migrations/upstream/*`；compat 编号由 W0-B2 独占（§7.4） |
| `Cargo.toml`（根） | 无人（glob members） | 🟢 | `members = ["crates/*", "apps/*"]` ⇒ 新 crate 零冲突 |
| `Cargo.lock` | scaffold 一次 | 🟡 | scaffold 提交 5 个空 crate + **预声明**依赖后的 lock delta；之后切片禁新增三方依赖（§7.5） |
| `crates/mc-config/src/lib.rs` | 按 §7.6 | 🟡 | 只允许在 `RuntimeConfig` **末尾**追加字段 |
| `docs/fixtures/route-parity-baseline.json` | 集成任务 | 🟡 | 切片**不刷新**；集成时一次性 `--write-baseline`（§7.3） |
| `scripts/route-owners.tsv` / `docs/fixtures/upstream-routes.tsv` | 无人（M3 不改） | 🟢 | cloud-runtime 的 owner 单元格待 M9 立项时改（§9.1） |

### 7.2 M3 anchor scaffold（一次性预置，单 commit，先于 W3a 三片）

照 M2 的 `4aa275a chore(m2-anchor): M2 预扩展 scaffold` 手法，**一个 commit** 做完下列全部改动；做完后三片只写自己的文件：

1. 新 crate（空 `Cargo.toml` + `src/lib.rs`）：`mc-daemon-proto`、`mc-runtime`、`mc-task`、`mc-agent`、`mc-daemon`（后两者允许先不建，按 §5 实际需要，但**建议一次建齐**——glob members 下多一个空 crate 的成本是 0，少一次 `Cargo.lock` 冲突）。
2. `crates/mc-repos/src/{agent,runtime,task}.rs` 空文件 + `lib.rs` 三行 `pub mod`。
3. `crates/mc-http/src/routes/{agents,runtimes,tasks,daemon}.rs`（各自 `pub fn router() -> Router<Arc<AppState>> { Router::new() }`）+ `mod.rs` 四行 `pub mod`。
4. `mount.rs` 四个 `mount_slice_*()` + 四行 `.merge(...)`（**保留** M0 占位，占位由对应切片合入时删）。
5. 各空 crate 的依赖**预声明并锁定**（`serde`/`serde_json`/`uuid`/`chrono`/`tokio`/`thiserror`/`tracing` 等；`mc-daemon` 加 `axum` 的 `ws` feature 与 `tokio-tungstenite` 时**必须先确认版本已在 lock 中或由集成方加**）⇒ 提交 `Cargo.lock` delta。
6. `state.rs`：给 `ConfigSnapshot` 加 `#[derive(Default)]` 并把 11 处构造点收尾改成 `..Default::default()`（现状 `ConfigSnapshot {` 出现在 11 个文件里）—— 这样后续切片追加字段只改 `state.rs` 一处。**若不采纳**，则退回「追加字段 + 逐点补构造」，并在 PR 里列出全部 11 处。

   > **实测偏差（M3-0 / LUM-1406 已按「不采纳」分支执行，PR 里列出全部构造点）**：
   > `ConfigSnapshot` **已经**有一个**手写的 `impl Default`**（同文件 `state.rs`，带语义默认值 `dev_mode: true` / `port: 3500` / `host: 127.0.0.1` / `session_ttl_secs: 30d`，
   > 由 W1-Google / LUM-1399 在本计划写就之后加入）⇒ 再加 `#[derive(Default)]` 是 E0119
   > 重复 impl；改成 derive 则会把这些语义默认值一次清零，静默改变 `/api/auth/send-code`
   > 与 ⑨ 契约门的回放结论。因此 **不 derive**，改为：保留手写 impl，把剩余**两个字面量**
   > 调用点（`apps/mc-server/src/main.rs`、`crates/mc-http/src/routes/auth.rs` 的测试装配）也收敛到
   > `..ConfigSnapshot::default()`。效果与第 6 项的目标等价：**追加字段 = 只改 `state.rs` 两处**
   > （struct + impl），已用探针实测（临时加字段后 `cargo check --workspace --all-targets` 全绿）。
   > 另：实测 `crates/` 内是 **12 个文件**含该字面量（比计划的 11 多 `mc-http/tests/issue_table.rs`，
   > 见 `docs/35-M3-W3A-PREFLIGHT.md` §3.2），调用点共 **12 处**（crates 内 11 + `apps/mc-server` 1）。
7. 不建表、不写迁移、不刷新 parity baseline。

### 7.3 占位删除与 baseline 刷新

- `docs/fixtures/route-parity-baseline.json` 的 139 条里**包含** `GET /api/agents`、`POST /api/agents`、`GET /api/runtimes`、`POST /api/runtimes`。删占位会让 `route_parity.py` 报 **4 条 regression** ⇒ 门禁红。
- **规则**：切片**不跑** `--write-baseline`（`docs/22 §4.4/§4.5`：M2 是集成方统一刷新）。切片 PR 里应说明「占位退役，baseline 由 M3 集成统一刷新」，并把 `route-parity` 门的红解释清楚；集成 cycle 一次性 `python3 scripts/route_parity.py --write-baseline` + 提交 baseline。
- 另：上游是 `/api/agents/`、`GET /api/runtimes/`（带尾斜杠），M0 占位是**不带**尾斜杠。**axum 把两者注册成不同键（`/x` 与 `/x/` 并存合法），所以不会 panic** —— `route_parity.py` 的 `slash_aliases()` 也把这种并排注册判为 legal，并在比较时折叠成一个上游键。后果：①占位不删就是一条恒 501 幽灵路由；②折叠比较会让「占位满足 upstream 路由」这一假象在 parity 表里看不出来（只能靠 handler 名分辨，见 §9.7）。所以必须删。真正的 panic 场景是**同 path 同 method 重复注册**——M1-D/M2 删掉的 `/api/issues`、`/api/comments`、`/api/inbox` 占位就是这一类（`mount.rs:40-46` 的注释原文）。
- 路径参数写 **`:id`**，不写 `{id}`（matchit 0.7 会把 `{id}` 当字面量段：编译通过、恒 404，`docs/09 §7.4`）。

### 7.4 `migrations/` 编号

**M3 不新增任何本地迁移文件。** §2.1 的 12 张缺表与 §2.2 的 6 处 `text`→`uuid`/列缺失，全部由 **W0-B2（LUM-1387）** 通过「apply-set 切到 `migrations/upstream/`」解决；`migrations/compat/535_*` 及之后的编号由 W0-B2 独占（P4 + docs/25 §6.3.1 的 C1–C4）。M3 若在实现中发现缺列，**回评 W0-B2 而不是自己加迁移**。

### 7.5 `Cargo.toml` / `Cargo.lock`

根 manifest 已是 glob members（`crates/*`、`apps/*`，`27af427`），所以新 crate 不需要改根 manifest —— 这是 W0-1 给 M3 的现成红利。但 `cargo build --locked` 是第 2 道门，锁文件必须与 manifest 同步 ⇒ 由 scaffold 一次提交（§7.2.5）。此后任何切片要加三方依赖，走 §8.4 仲裁，由集成方加并说明理由（本仓已发生「workspace 依赖真相收敛」一次，见 `docs/28`）。

### 7.6 `mc-config` / `ConfigSnapshot` 字段追加

`mc_config::RuntimeConfig`（`default_runtime='claude-code'`、`max_concurrent_tasks_per_agent`、`lease_secs`、`retry_max`、`allow_local_daemon`、`allow_cloud_runtime`）**已存在但没接线**：`ConfigSnapshot` 里没有 runtime 段。M3 的正确做法是**接线**（scaffold 或 M3-2 一次），不是新增 env 变量。确实需要新字段时：追加在 `RuntimeConfig` **末尾**（`Option<T>`+调用方默认值的语义，照 `invitation_per_workspace_per_hour` 先例），`mc-config` 单测补 1 条，PR 注明变量名与默认值。

### 7.7 单文件 800 行（R7）—— 当前**没有**守门脚本

plan1 R7 把 `scripts/file_size_check.py` 列为缓解手段，**实测该脚本不存在**，`gates.sh` 的 8 道门里也没有它。当前已超限（`wc -l`）：`routes/issues.rs` **2224**、`mc-repos/issue.rs` 1947、`comment.rs` 1403、`inbox.rs` 1186、`routes/inbox.rs` 981、`routes/auth.rs` 936、`routes/comments.rs` 831。M3 的 daemon 面若照搬上游 `daemon.go`（6056 行）必然再造一个巨文件。

M3 规则：**每个新文件 ≤800 行**，daemon 面按域拆 6 文件（§6 M3-7）。建议（**不在本 issue 范围**）：M3-7 或集成方补 `scripts/file_size_check.py` 并接成第 9 道门；在那之前靠 PR 自查。

### 7.8 分支与工作区

分支名统一 `feat/multica-rs-m3{a,b,c}-<slug>`（§3 表）。**一个切片一个 `multica repo checkout` 工作区**（M1 出现过 LUM-1375 双工作区歧义）；工作区名与分支名不必相同（`docs/10 §6.2` 的教训），但必须在 issue 描述里写清。

---

## 8. 验证门与仲裁规则（照 `docs/09` 模式）

### 8.1 十道门（W0-A / W0-D / ⑨ 契约门 / ⑩ 尺寸门已落地，`bash scripts/gates.sh` 是唯一实现）

`ALL_GATES="fmt build clippy clippy-test-util test db schema-drift route-parity conformance file-size"`（@`d7639f0` 实测）；**默认集合 = ①–⑤ + ⑦`route-parity` + ⑨`conformance` + ⑩`file-size` = 8 门**，`--with-db` 追加 **⑥`db`** 与 **⑧`schema-drift`**（两者需要 `MULTICA_TEST_DATABASE_URL`，**没有 URL 直接 exit 1，不静默跳过**）。退出码 0 全绿 / 1 门红 / 2 用法错（`docs/24 §2`、`docs/30`）。

- ⑨ `conformance`（W0-C / LUM-1388 抽取器 + LUM-1413 接线）：回放 golden fixture 对快照，`crates/mc-conformance/report.json` 是唯一真值；
- ⑩ `file-size`（R7 / LUM-1416）：`python3 scripts/file_size_check.py`，单文件 800 行上限，存量违规在 `scripts/file_size_baseline.tsv` 里**只允许变短**——**M3 新代码不得进基线**。

M3 每切片交付门（**统一写这一条命令**，LUM-1357 要求）：

```bash
bash scripts/gates.sh --with-db          # 需要 DB 的切片
bash scripts/gates.sh                    # 纯库切片（M3-1/2/3 亦建议带 DB，确认没踩坏 schema_drift）
```

### 8.2 切片交付证据（每个 PR 必须贴）

1. `bash scripts/gates.sh --with-db` 的**尾部汇总**（各门 exit code + 耗时）；
2. `git log -1 --stat`；
3. **至少 3 条路由**与上游 `router.go` 行号/上游 handler 的对照（本 issue 的验收就是抽 3 条，切片自证更省事）；
4. 若有 DB 用例：建库命令与 `--ignored` 用例的实际结果（不得写"跳过"）。

### 8.3 M3 集成的验证门

1. 十道门全绿（`--with-db`）；
2. `python3 scripts/route_parity.py --write-baseline` 后**再跑一次**必须 exit 0、`regression 0`、`unclaimed 0`。gap 计数口径：集成后 `M3` 应为 **11**（= 已判给 M9 的 cloud-runtime，属 §9.1 的 owner 计数噪声；owner 单元格一改即为 0）；`M3+` 的 17 条不动（§9.4）。**不许**为了好看去改 owner 单元格凑账。另：先修 §9.7 的占位正则再把 `implemented_real` 当指标——否则该数永远虚高 23（集成报告应同时给 `placeholder + not_implemented` 两个计数）；
3. `python3 scripts/schema_drift.py --quiet` exit 0（W0-B2 后应只剩 `contracts/upstream-apply-exceptions.tsv` 里那 9 行不可应用的例外）；
4. 契约等价率：按 plan1 §8 的 W3 目标 **40%**（分母 = W0-C 抽出的 golden fixture 用例数）。**@`d7639f0` 状态**：`mc-conformance` crate + fixture 抽取器（W0-C / LUM-1388）与门 ⑨ 均已落地，基线口径 = `crates/mc-conformance/report.json`（stateless 层 `pass 4 / mismatch 1 / unevaluable 47`）与 `docs/27` §5.1 的真库层计数（`pass 43 / mismatch 1`，分层计数，**不要**把两层数字混用）；切片交付仍需「≥3 条路由抽样」作为路由级证据，但契约门要跑门 ⑨ 而不是用抽样代替。
5. **端到端**：pi-local adapter 的真实任务回路（M3-2 的 E2E 升级版：daemon stub → claim → start → progress → complete → usage 结算落库）。

### 8.4 仲裁规则（M3 增补）

1. **权威顺序**：`docs/plan1.md` > `docs/01-PLAN.md`；上游 `router.go` / `migrations/upstream/` > 其 Rust 等价物；**上游事实 > 本仓既有实现**（M2 的两处反向教训：`issue_label` 与 `personal_access_token.expires_at`）。
2. **同文件单写者**：见 §7.1；冲突由**后合并方 rebase** 解决，不改对方已定的接口签名（要改就开 issue）。
3. **契约分歧**（状态码 / body 形状 / 必填性）：以 golden fixture 为准；无 fixture 时**冻结在 `docs/16`**，并在 §1 的路由表里标 owner。
4. **schema 分歧**：一律回 W0-B2（LUM-1387），**任何切片不得新增迁移**。
5. **范围分歧**：本文件 §0/§9 为准；新发现的上游路由若既不在 M3 也不在任何 milestone，按 `scripts/route-owners.tsv` 的 first-match 规则补 owner，不擅自塞进 M3。
6. **协议冻结**：`docs/16` 声明冻结后，改协议必须走本节的仲裁（谁冻结谁签字），冻结前的讨论放在 M3-1 的 PR 里。

### 8.5 契约等价率（W0-C）接入点

`docs/fixtures/upstream-routes.tsv` 只覆盖「路由存在」。行为等价需要 golden fixture：抽取器（LUM-1388 / W0-C，编写时为 `in_progress`）落地后，M3 的接入点是——①M3-1 的协议 golden JSON 改为从抽取器产物读取；②M3-7 的 daemon 回路用例同样读 fixture；③集成门按 §8.3.4 换成真分母。

---

## 9. 覆盖缺口与事实更正（回评用，不改既有文档）

### 9.1 `cloud-runtime` 的 owner 单元格与 plan1 冲突（**已裁决**）

- 事实：`docs/fixtures/upstream-routes.tsv` 的 11 行 `cloud-runtime` 与 `scripts/route-owners.tsv` 里 `^/api/cloud-runtime` 规则都标 **M3**（后者理由写明"远程 runtime 节点池（执行后端，非计费）"）。
- 冲突：plan1 §5 的 W9 明确列 `cloud-runtime(11)`。
- 裁决：按 LUM-1357 的"**冲突处以 plan1 为准**"判给 **W9/M9**。本计划不含这 11 条。
- 待办（**不在本 issue 范围**）：改 owner 单元格要动 `docs/fixtures/upstream-routes.tsv` + `scripts/route-owners.tsv`（超出"单 commit 只写 `docs/15`"的范围），登记给 M9 立项时处理。现在的后果是 route_parity 的 `M3 gap` 里含 11 条实际属 M9 的路由 —— 这是**计数噪声**，不是未认领缺口（unclaimed 仍为 0）。

### 9.2 上游 head 表数：114 → **116**

`plan1 §1.5` 与 `§6.3.1` 写「净 114 张」，`docs/25 §5.4` 用两种独立方法（`pg_dump` 快照计数、560 份迁移里 `CREATE TABLE` 137 − 21 净退役）都得 **116**。本计划以 **116** 为分母（§2.1）。属既有文档的口径更正，回评登记；`docs/25` 是实测方，不改 `plan1` 原文。

### 9.3 adapter 数：26 → **25**，且 `runtime_profile` 不是它的目录

- 权威白名单在上游**代码**里：`server/pkg/agent/agent.go::SupportedTypes` = **25 项**（claude, codebuddy, codex, copilot, opencode, codearts, deveco, openclaw, hermes, pi, cursor, kimi, reasonix, dsh, kiro, antigravity, qoder, qoderclicn, traecli, grok, qwen, qwenpaw, mcode, dim, zeroclaw），与 head 的 `runtime_profile_protocol_family_check`（`contracts/upstream-schema.sql`）的 25 个值**逐字一致**。`New()` 的 switch 有 24 个 `case`，但 `case "qoder", "qoderclicn"` 共用一条，加上 `default` 报错 —— 仍是 25 个取值。
- 沿革：`120_runtime_profile` 初版 CHECK 是 13 值且**含 `gemini`**，此后由 `134/136/175/179/202/242/253/254/313/342/370/403/441` 逐次加宽（每次加一个，141 行迁移清单可核对），`gemini` 在某次 CHECK 重建中消失。
- `plan1 §5 W3 / R4` 写「26 个 adapter」与实测差 1，原因未查明（疑似把 `qoder`/`qoderclicn` 当成两个后端再多数一项）。**分批方案不受影响**：按 25 分 3 批（8/8/9，§6 M3-8）。
- **本地 `runtime_profile` 不是这份白名单**（§2.2）：白名单的运行时载体是代码常量；上游表的 `protocol_family` 只是**用户自定义 profile** 的校验字段。M0 把「代码里 25 个 provider」误当成「一张 profile 目录表」，是 §2.2 那张表形状全错的根因。

### 9.4 `M3+` ≠ M3（17 条，含义是「M3 之后未排期」）

| 族 | 条数 | 真实归属 |
| --- | ---: | --- |
| attachments（`/api/attachments/{id}`、`…/content`、`…/download`、`…/signed-download`、`/api/issues/{id}/attachments`） | 5 | W8 / M8（制品与存储） |
| `/api/upload-file`、`GET /uploads/*`、`GET /api/avatars/{sig}/*` | 3 | W8 / M8 |
| quick-actions（`/api/quick-actions/`、`/{id}/`、`/api/issues/{id}/quick-actions/{quickActionId}/{render,run}`） | 5 | **M3 完成后**才可立项（依赖 task queue） |
| `GET /api/comments/{commentId}/sub-issue-preview` | 1 | 未排期（人的交互面） |
| `GET /ws` | 1 | realtime（W2 尾部 / W10） |
| 其余 | 2 | 未排期 |

注意：本地 `local_only` 里的 `GET /api/issues/:id/quick-actions` 与上游 `M3+` 的 `GET /api/quick-actions/` **不是同一条**，别合并处理。

### 9.5 `/api/agents` 上游 25 条的归属拆分（M3 只做 13 条）

实测 owner 分布：**M3 13** / M6 6（`skills`、`runtime-skills/enabled`）/ M8 4（`mcp-servers*`）/ M9 1（`POST /api/agents/mika`）/ M7 1（`dingtalk/groups`）。plan1 §5 的「agent（25）」是**该前缀下的路由总数**，不是 M3 的实现范围。M3 的 agent 面 = 13（§1.3）+ agent-builder 4（§1.4）+ workspace 统计 3（§1.3 末三条）= **20 条**，已按 §3/§5 分配到 M3-5（16）与 M3-6（4）。

### 9.6 既有债与移交确认（不改既有文档，仅登记）

1. `crates/mc-http/src/routes/issues.rs` **2224 行**已超 R7；M3 不得往里追加（M3-5/M3-6 各自新建文件，仅在 repo 层复用）。
2. `docs/10 §2` M2-A「不做」的 6 条 + M2-B 的 `trigger-preview`：**M3 接收**（§1.6 / §5 M3-6）。但这 6 条**不是空白**——它们已以 `not_implemented` stub 形式注册在 `issues.rs:91/123/124/125/126/133`（§1.8），M3 要做的是替换而非新增。`docs/10` 原文保留，本文件即接收凭证。
3. `scripts/route-owners.tsv:60` 有一条 `^/api/comments/[^/]+/trigger-preview → M3`，但上游实际只有 `POST /api/issues/{id}/comments/trigger-preview`（fixture owner = M2-A）—— 该规则**永不匹配**，属死规则（不影响 unclaimed=0）。M3 不改它。
4. `state.rs::AdapterRegistryStub` 与 `ConfigSnapshot` 缺 runtime 段：M3-2 / scaffold 处理（§7.6）。
5. `scripts/file_size_check.py` 不存在（§7.7）——plan1 R7 的第 9 道门尚未落地，登记为建议。
6. `mount.rs:44-47` 的 `/api/agents` 与 `mount.rs:50-53` 的 `/api/runtimes` 占位**各是一次调用的 `get().post()` 双方法注册**，删除时必须整块删（只删一个方法会留一条 501）。

### 9.7 ⚠️ `route_parity.py` 的占位正则盲区（影响 M3 的完成度判定）

`scripts/route_parity.py:106` 的 `PLACEHOLDER_HANDLER = re.compile(r"\bplaceholder\b")` 只认字面量 `placeholder`。本仓另有 **23 条注册的 handler 叫 `not_implemented`**（定义在 `crates/mc-http/src/routes/issues.rs:2213`，返回 501 `not_implemented`）——它们**全部匹配上游路由**，却被计入了 `implemented_real`。实测归属：M3 **6**、M5 7（`issue-wakeups` 族）、M2-A 4（`labels`、`limit-usage`、`comments/trigger-preview`）、M2-D 3（`issues/table/*`）、M8 1（`pull-requests`）、M9 1（`timeline`）、M3+ 1（`issues/{id}/attachments`）。

后果：`implemented 112 real` 实际只有 **89** 条是真实现；`M3` 的 101 条里有 9 条已被“满足”，但**无一是真实现**（3 占位 + 6 stub）。

建议（**不在本 issue 范围**，属门禁维护方）：把正则改为 `placeholder|not_implemented`（或改成按返回码/白名单判定），并在 `docs/22-ROUTE-PARITY.md` 回填这个口径；在此之前，M3 切片的验收**不看** `implemented_real` 这个数，改看 §8.2 的抽样证据。

---

## 10. 立项与晋升 playbook

### 10.1 本文件所有数字的复算命令

> 上游基线（clone 配方见 `docs/20`，**不要**用 `--filter=blob:none`：实测死锁 74 分钟）：`/home/devbox/multica_workspaces/lumos-659117e3ca3d/lum-1355-0d4fadfb4836/workdir/upstream-multica`（`f41fae6b`）。

```bash
# ① M3 路由总数（应为 101）与 cloud-runtime 条数（应为 11）
grep -Pc '\tM3\t'   docs/fixtures/upstream-routes.tsv
grep -Pc 'cloud-runtime' docs/fixtures/upstream-routes.tsv
# ② 路由门禁与 M3 缺口（应 exit 0；gaps by owner 里 M3=92）
python3 scripts/route_parity.py
# ③ head 表集（应为 116）/ 本地表集（应为 28）
python3 -c "import re;s=open('contracts/upstream-schema.sql').read();print(len(set(re.findall(r'CREATE TABLE (?:IF NOT EXISTS )?(?:public\\.)?\\\"?([A-Za-z_][A-Za-z0-9_]*)\\\"?\\s*\\(',s))))"
# ④ 8 道门与 schema drift
bash scripts/gates.sh --list
MULTICA_TEST_DATABASE_URL=... bash scripts/gates.sh --with-db
python3 scripts/schema_drift.py --quiet
# ⑤ adapter 白名单条数（应为 25，见 §9.3）
cd $UPSTREAM && git show HEAD:server/pkg/agent/agent.go | grep -cE '^\s+"(claude|codebuddy|codex|copilot|opencode|codearts|deveco|openclaw|hermes|pi|cursor|kimi|reasonix|dsh|kiro|antigravity|qoder|qoderclicn|traecli|grok|qwen|qwenpaw|mcode|dim|zeroclaw)",$'
# ⑥ 800 行自查
find crates apps -name '*.rs' -exec wc -l {} + | sort -rn | head -10
# ⑦ 占位/501 stub 的方法级计数（§9.7：not_implemented 应 24 处方法注册，其中 23 条对应上游路由）
grep -rhoE '(placeholder|not_implemented)\)' crates/mc-http/src/routes/*.rs | sort | uniq -c
# ⑧ M3 归属 101 / 已“满足” 9（3 占位 + 6 stub）/ gap 92
python3 scripts/route_parity.py --json | python3 -c "import json,sys;r=json.load(sys.stdin);print('M3 gap',r['owners']['M3']);print('M3 satisfied',[(x['method'],x['path'],x['placeholder']) for x in r['implemented'] if x['owner']=='M3'])"
```

上述 ①②③⑤⑦⑧ 六组数字已在本文件编写时逐条实测过（均为 `f41fae6b` / `0af39ab` 口径）。

### 10.2 晋升顺序（每个子 issue 都带 §3/§4/§5/§6 的字段 + 本节的晋升条件）

1. **开工前置核对**（每次晋升前跑）：`multica issue runs LUM-1334 --siblings`（并发 ≤3）→ `ps aux | grep cargo`（无其它 cargo 抢 package-cache 锁）→ `df -h /`（`target/` 会堆，M2 期间曾压到 97%）。
2. **先落 scaffold**（§7.2，含代码 ⇒ 走分支 + PR，照 `4aa275a` 先例直接基于 `feat/multica-rs-initial`）。scaffold 未合入时 W3a 三片无法并行（会抢 `lib.rs`/`mod.rs`/`mount.rs`/`Cargo.lock`）。
3. **W3a 三片**（M3-1/2/3）同时以 `backlog` 建 issue，带 §4 的字段；晋升为 `todo` 时逐个 `multica issue rerun <id>`（`multica issue assign --to-id` **只记归属、不排 run**）。
4. **W3b 三片**在 **W0-B2（LUM-1387）合入后**立项（M3-6 另需 M3-3 合入）。
5. **W3c 两片**串行：M3-7 合入后 M3-8 的批 1 才能起（M3-8 的三批各自独立 PR 与晋升）。
   **2026-09-23 07:15 就地修订**：**只对批 1 解除**该前置 —— 批 1（`LUM-1441`，写集 `crates/mc-runtime/**`）与 M3-7 零文件相交，
   且 §37 §5.4 实测「`AgentType → ProtocolFamily` 映射不存在」而 M3-7 的 hub 协商要按族决策 ⇒ 该表是 M3-7 的**前置**，
   先落批 1 是拆前置。`LUM-1440`（execenv，与 M3-7 共用 `mc-daemon/src/lib.rs` + `Cargo.toml`）与批 2/3 仍按本条串行。
   依据与实测见 `docs/37-M3-W3C-PREFLIGHT.md` §16.1。
   **2026-09-23 08:00 状态更新**：批 1（`LUM-1441`）已在跑；**M3-4（PR #31）已合入 base** ⇒ `LUM-1438`（M3-7）的唯一阻塞解除，已在本 cycle 晋升。
   合并波（#31/#32/#33 → base `463eb3f`）与合并树真库 10/10 门的证据见 `docs/37` §18.2。
6. **M3 集成 cycle**：按 §8.3 出门禁 → 一次性刷新 parity baseline → 写 `docs/34-M3-INTEGRATION.md`（照 `docs/21-M2-INTEGRATION-RECIPE.md` 的配方）→ 契约等价率按 §8.5 口径提升。
   **2026-09-23 新增纪律**（见 `docs/37` §16.2）：**切片 PR 不刷 `docs/fixtures/route-parity-baseline.json`** —— 一次性刷新就是本条的活；
   切片各刷一遍会让每两个改路由的切片 PR 必冲突（PR #31 × #32 实测同一 JSON 列表尾部相邻追加）。切片只跑门禁：⑦ 是**下界锁**，只对**丢**路由判红。

### 10.3 本 issue（LUM-1357）范围说明

LUM-1357 的范围限制明确要求「**不创建 M3 切片的子 issue** —— 计划回评后再按 slice 立项（backlog，带晋升条件）」，而完成清单里又提到「据计划创建的 M3 子 issue 链接（backlog）」。**两处字面冲突，按范围限制处理**：本轮只在回评里给出 8 个切片的完整 payload（分支 / 基线 / Repo / 路由数 / 测试 / 文档号 / 范围限制 / 验收 / 晋升条件）。operator 决定立项时，直接按 §3–§6 的字段建 backlog 子 issue 即可，不需要再读一遍本文件全文。

### 10.4 本文件的编号预占

`docs/16`（协议）、`18`（runtime/adapter）、`19`（task 领域）、`26`（runtime 台账）、`27`（agent）、`31`（task 用户面）、`32`（daemon）、`33`（adapters）、`34`（M3 集成手册）。若某切片先落地并占用了其中一号（例如并行 cycle 里已有人写 `docs/18`），**顺延取下一个空号**并在 PR 说明，不要覆盖。
