# Paperclip Node → paperclip-rs 差距矩阵

更新时间：2026-08-05

本文以 `../paperclip/server/src/routes`、`../paperclip/server/src/services`、
`../paperclip/server/src/secrets` 与 `crates/pc-http`、`crates/pc-repos` 的当前源码为准。
“路由存在”不等于“行为等价”：本矩阵另外区分数据持久化、权限、外部 IO 和异步运行时深度。

## 当前已落地的高价值模块

| 模块 | Rust 现状 | 证据 |
|---|---|---|
| agent wakeup | 强类型状态机、幂等键、公司隔离、计数与状态迁移 | `crates/pc-repos/src/agent.rs` |
| cases | 六类 case 相关表、upsert、事件、标签原子替换 | `crates/pc-repos/src/case.rs` |
| heartbeat | 运行状态、并发安全事件序号、watchdog 决策 | `crates/pc-repos/src/heartbeat.rs` |
| storage | Local disk 与 S3 SigV4 | `crates/pc-storage/src` |
| adapters/process | 真实子进程、stdout/stderr、取消和超时 | `crates/pc-adapter-process/src/lib.rs` |
| secrets/provider registry | 四个 provider descriptor 与健康检查已接入 HTTP | `crates/pc-http/src/routes/secrets.rs` |
| catalog provenance | 16 字段可移植白名单、sourceRef/originHash fallback、auditCodes 原子校验与 17 个单测已完成 | `crates/pc-core/src/catalog_provenance.rs` |
| home paths (PAPERCLIP_HOME/INSTANCE_ID) | 11 路径规则、配置/config/.env/runtime 目录布局与 Node `home-paths.ts` 1:1 | `crates/pc-config/src/home_paths.rs` |
| decision signing | canonical JSON、HMAC-SHA256、并发原子 hard-link 发布、权限自愈、tamper 拒绝、3 个 Node 黄金签名向量 | `crates/pc-secrets/src/decision_signing/` + `pc_repos::decision` |
| tool profile binding precedence | 6 target precedence 表、3 键稳定排序、首次去重 1:1 复刻 | `crates/pc-core/src/tool_profile_binding.rs` |
| export fidelity (shared) | 10 count 字段 + warning 构造器 + 归一化校验与 Node `portability-fidelity.ts` 1:1 | `crates/pc-core/src/portability_fidelity.rs` |
| export fidelity (DB) | 10 `COUNT(*)` 聚合 + monitor 限定 + report 构造与 Node `export-fidelity.ts` 1:1 | `crates/pc-repos/src/export_fidelity.rs` |
| tool content guards | canonical JSON (key 排序) + 4 个 prompt injection regex + HMAC-SHA256 base64url envelope sign/verify + redact 摘要 + validate (sensitive redact/block + prompt block/ignore) | `crates/pc-core/src/tool_content_guards.rs` |
| issue continuation summary | mod/ 拆分（4 文件）+ 9 段 markdown 模板（Objective/Acceptance/Recent/Files/Commands/Blockers/Next Action）+ 字符截断 + 路径候选提取 + mode/next action 推断 + DB IO 复用 DocumentRepo | `crates/pc-repos/src/issue_continuation_summary/` |
| plugin state store | scoped KV（5 段复合主键）+ UPSERT + scope_id IS NULL 分支 + 5 个 CRUD 方法 + FK 校验 | `crates/pc-repos/src/plugin_state_store.rs` |
| home paths (PAPERCLIP_HOME/INSTANCE_ID) | 11 路径规则、配置/config/.env/runtime 目录布局与 Node `home-paths.ts` 1:1 | `crates/pc-config/src/home_paths.rs` |
| decision signing | canonical JSON、HMAC-SHA256、并发原子 hard-link 发布、权限自愈、tamper 拒绝、3 个 Node 黄金签名向量 | `crates/pc-secrets/src/decision_signing/` + `pc_repos::decision` |
| plugin stream bus | 内存 pub/sub + 三段式订阅键（pluginId:channel:companyId）+ 同步回调 + 多订阅者扇出 + unsubscribe 清理 | `crates/pc-plugin-host/src/plugin_stream_bus.rs` |
| plugin event bus | mod/ 拆分（5 文件）+ 精确 + 尾随 `.*` 通配 + 服务端 EventFilter（projectId/companyId/agentId AND）+ plugin 命名空间隔离 + 自动 namespace + 守卫 | `crates/pc-plugin-host/src/plugin_event_bus/` |
| plugin config validator | JSON Schema Draft 7 + 自定义 `secret-ref` 格式 + 结构化错误（field/message）+ Draft 7 默认 | `crates/pc-plugin-protocol/src/config_validator.rs` |

## 主要差距

| 优先级 | Node 模块 | paperclip-rs 当前状态 | 缺口类型 | 下一步 |
|---|---|---|---|---|
| P0 | `services/heartbeat.ts` | 持久化仓储、Kameo supervisor、queued claim、adapter sink、基础 1 秒 scheduler、每-agent 1–50 并发配额、retry 到期/promotion、bounded retry schedule、transient failure 子 run、issue monitor due claim、timer heartbeat、actionable-work gate 和 restore/worktree suppression 已接入 | Node 的依赖 readiness、staleness recovery、其他 retry reason、retry 幂等/wakeup 合并、timer cutoff/daily cap 策略与 suppression 数据库 override 仍未完整迁移 | 将剩余 scheduler 策略抽成 `pc-heartbeat` actor pipeline |
| P0 | `adapters/index.ts` + 11 adapters | 仅 process 有真实执行，其余多为 descriptor/占位执行 | 外部 CLI/API 适配器、usage、session、result 解析 | 先完成 codex/claude/cursor local 共用进程协议 |
| P0 | `routes/plugins.ts` | HTTP→worker 的 `getData`/`performAction`/`executeTool`/`runJob` 已接入 `pc-plugin-host` JSON-RPC | worker-to-host 回调、通知分发、崩溃监控与指数重启仍弱于 Node | 补齐双向 RPC handler、stderr/exit supervisor、backoff |
| P1 | `secrets.ts` + provider modules | provider 列表/health 已补；agent value、远端 provider、discovery 仍为空或简化 | 密钥解析、授权绑定、AES-GCM/AWS/GCP/Vault | 复用 `pc-secrets` trait 接入 route |
| P1 | `routes/auth.ts` | 简化 email 查找与 session | 密码哈希、refresh、OAuth/CSRF 语义不完整 | argon2 + session rotation |
| P1 | `routes/live-events.ts` | 路由形状存在 | WebSocket 订阅、token、重连缓冲未完全等价 | `pc-realtime` + `pc-ws` |
| P1 | `routes/execution-workspaces.ts` | 基础 CRUD | runtime service、lease、命令执行权限深度不足 | workspace actor + lease 状态机 |
| P2 | `routes/adapters.ts` | 仅 list/get 两个端点 | install/reload/reinstall/config-schema/ui-parser 缺失 | adapter registry persistence |
| P2 | `routes/board-chat.ts` | 基础接口/占位系统提示 | thread、消息持久化与 agent 执行闭环不足 | chat repo + actor |
| P2 | `routes/smoke-lab.ts` | 数据记录接口存在 | 服务启动、OAuth provider、fixture 安装仍是模拟响应 | smoke service process manager |
| P2 | `routes/status-cards.ts` | CRUD 与 revision 数据路径存在 | recompile/refresh/query/summary 生成器未连接真实模型 | status-card actor |
| P2 | `routes/tool-gateway.ts` | 基础路由存在 | OAuth、远程工具调用和审计链路不完整 | capability-gated gateway |
| P2 | `routes/company-skills.ts` + `services/company-portability.ts` | CRUD 基础实现；catalog provenance 纯规则已落在 `pc-core` | provenance 尚未接入导入/导出，manifest 构建、运行时 materialization 与 Node skill loader 仍有差异 | 建立 company portability 聚合模块并接入 provenance facade |
| P1 | `services/decision-signing.ts` | canonical + HMAC + 原子 hard-link + 启动 fail-fast 已在 `pc-secrets::decision_signing` 与 `pc_repos::decision` | `decide/dismiss` 仅在 HTTP 层守卫，仓储未强制校验；`company-portability` 仍消费旧字段 | 决策仓储统一入口签名校验 + portability 接入 |
| P1 | `services/decision-signing.ts` | canonical + HMAC + 原子 hard-link + 启动 fail-fast 已在 `pc-secrets::decision_signing` 与 `pc_repos::decision` | `decide/dismiss` 仅在 HTTP 层守卫，仓储未强制校验；`company-portability` 仍消费旧字段 | 决策仓储统一入口签名校验 + portability 接入 |

## 已确认的“非差距”

- 现有 `pc-adapter-*` crate 不再是统一的空 stub；需要区分 descriptor 缺失与 execute 深度缺失。
- `pc-storage` 的 S3 签名不是 mock；缺口主要在 registry 的少数未使用元数据接口。
- `pc-authz` 已有默认策略和测试，但尚未达到 Node authorization service 的完整策略覆盖。
- UI 复用路径已经存在；UI 构建仍受上游 `@assistant-ui` 依赖版本冲突影响。

## 本轮实施记录

- 将 `/api/companies/:company_id/secret-providers` 从 `{items: []}` 改为 Node 兼容的四项 provider descriptor。
- 将 `/api/companies/:company_id/secret-providers/health` 从空数组改为四项 provider health check。
- 增加 `secrets_contract` 对 provider 数量、字段和 health 形状的断言。
- 未宣称远端 provider 已可执行：GCP/Vault 明确返回未配置状态，AWS 只报告配置准备度。
- 复核 plugin bridge：Rust 已有真实 worker pool 与 HTTP→worker JSON-RPC 调用，不能把它误列为“全 stub”；差距收敛到双向回调与生命周期恢复。
- heartbeat scheduler 已接入 `pc-server`：每秒查询 recoverable runs，条件 claim queued/scheduled_retry，并复用 HTTP agent 的 adapter dispatch/sink；尚缺 Node 的复杂 readiness/staleness 策略。
- agent 并发配额已在 `HeartbeatRepo::claim_for_agent_with_limit` 中使用 PostgreSQL advisory transaction lock + running count + conditional update，避免多 scheduler 实例超配。
- scheduler 只在 `scheduled_retry_at <= now` 时先将 `scheduled_retry` 原子提升为 `queued`，再进入普通 claim；`PAPERCLIP_IN_WORKTREE`、`PAPERCLIP_DATABASE_RESTORE_IN_PROGRESS` 和 `PAPERCLIP_RESTORE_IN_PROGRESS` 默认抑制后台调度。
- `pc-heartbeat::compute_bounded_transient_retry_schedule` 已对齐 Node 的 2/10/30/120 分钟基础延迟、±25% jitter、最小 1 秒和 4 次上限；业务层何时创建 retry run 仍待接入。
- `SqlHeartbeatExecutionSink` 已对 `error_code=transient_failure` 创建 `scheduled_retry` 子 run，保留上下文、责任人和 `retry_of_run_id`，并写入 `run.retry_scheduled` 事件；其他 Node retry reason 仍未接入。
- `IssueRepo::claim_due_monitors` 使用 `FOR UPDATE SKIP LOCKED`、5 分钟 stale claim 窗口和 `in_progress/in_review + agent assignee` 条件；scheduler 创建 `automation` heartbeat 并在成功 claim 后清理 monitor 调度字段。
- `AgentRepo::claim_due_timer_heartbeat` 使用条件 UPDATE 原子推进 `last_heartbeat_at`；scheduler 根据 `runtime_config.heartbeat.enabled/intervalSec` 创建 `timer` heartbeat，并保留首次 heartbeat 标记。
- timer scheduler 已支持 `skipTimerWhenNoActionableWork`、`requireActionableTimerWork`、`issueOnlyTimer` 三个兼容配置名，通过 `IssueRepo::has_actionable_timer_work` 检查 `todo/in_progress` agent issue。
- catalog provenance 已从 Node `services/catalog-provenance.ts` 迁移到 `pc-core`：16 项白名单、canonical key、sourceRef/originHash fallback、auditCodes 全量校验均有真实单测；消费端接线仍明确保留为差距。

- plugin stream bus 已从 Node `services/plugin-stream-bus.ts` 迁移到 `pc-plugin-host`：subscribe 返回 `Box<dyn FnOnce() + Send + Sync + 'a>` 绑定 `&self` 生命周期；publish 多订阅者扇出 + unsubscribe 清理空 key。测试 15/15 通过。
- plugin config validator 已从 Node `services/plugin-config-validator.ts` 迁移到 `pc-plugin-protocol`：jsonschema 0.30 Draft 7 + 自定义 `secret-ref` 格式（恒真，UI hint）+ 结构化 `{field, message}` 错误。测试 14/14 通过。
- plugin event bus 已从 Node `services/plugin-event-bus.ts` 迁移到 `pc-plugin-host::plugin_event_bus/`（mod/ 拆分，5 文件 970 行）：types/pattern/filter/bus/tests 职责分离，Arc<dyn AsyncHandler> 跨锁共享，tokio::spawn 并发投递。测试 31/31 通过。
- tool content guards 已从 Node `services/tool-content-guards.ts` 迁移到 `pc-core::tool_content_guards`（单文件 955 行）：canonical JSON / prompt injection / HMAC 签名 / redact 摘要 / validate 主入口。测试 27/27 通过。
- issue continuation summary 已从 Node `services/issue-continuation-summary.ts` 迁移到 `pc-repos::issue_continuation_summary/`（mod/ 拆分，4 文件 1027 行）：types/markdown/queries 职责分离，手动行扫描替代 regex lookahead（Rust regex crate 不支持）。测试 38/38 通过。
- plugin state store 已从 Node `services/plugin-state-store.ts` 迁移到 `pc-repos::plugin_state_store`（单文件 406 行）：5 段复合主键 + QueryBuilder 动态 SQL + UPSERT + scope_id IS NULL 分支 + FK 校验。测试 9/9 通过。

## 验证基线（Round 113 末）

```text
cargo check --workspace                        0 errors, 61 warnings
cargo test -p pc-repos --lib                   430 passed (含 38 个 issue_continuation_summary + 9 个 plugin_state_store)
cargo test -p pc-core --lib                    187 passed (含 27 个 tool_content_guards)
cargo test -p pc-plugin-protocol --lib         33 passed (含 14 个 config_validator)
cargo test -p pc-plugin-host --lib             70 passed (含 15 plugin_stream_bus + 31 plugin_event_bus + 24 既有；1 pre-existing flaky)
cargo test -p pc-http --test secrets_contract  5 passed
```

## 本轮增量

## 本轮实施记录

- heartbeat daily cap 已接入：`HeartbeatPolicy::from_runtime_config` + `evaluate_daily_cap` + `evaluate_daily_cap_for_agent` 配合 `dispatch_queued_heartbeat` 在 claim 之前取消当日超出 `maxDailyRuns` / `maxDailyCostCents` 限制的 queued run，并写入 `run.cancelled` 事件并发布 `heartbeat.run.cancelled` 实时事件。`CostRepo::sum_agent_window_cost_cents` 复用 Node `currentUtcDayWindow` 语义。
- dependency readiness 已接入：`IssueRepo::unresolved_blocker_ids` / `unresolved_blockers_for` 实现 Node `evaluateIssueExecutionReadiness` 的 `blocks` 类型 blocker 查询；`dispatch_queued_heartbeat` 在 issueId 存在时先查询 blocker，有未解决 blocker 直接取消 queued run 并写入 `run.blocked` 事件。
- suppression DB override 已接入：`SettingsRepo::resolve_worktree_run_execution_activation` 读取 `instance_settings.experimental` 的 `enableWorktreeRunExecution` / `worktreeRunExecutionActivatedAt` / `worktreeRunExecutionActivationInstanceId` 三元组，对齐 Node `resolveWorktreeRunExecutionActivation`；`pc-server` 的 scheduler 抑制检查现已在 `PAPERCLIP_IN_WORKTREE` 默认抑制之外把 `armed` 状态视为放行；read failure 失败关闭。
- wakeup 幂等 / stale claim 恢复已接入：`AgentRepo::find_active_wakeup_request` 返回 agent 当前 active wakeup；`recover_stale_wakeup_claims` 在 5 分钟 stale 阈值上把 `claimed` 状态重置为 `requested`，scheduler 每秒 tick 调用。

## 之前实施记录

- 将 `/api/companies/:company_id/secret-providers` 从 `{items: []}` 改为 Node 兼容的四项 provider descriptor。
- 将 `/api/companies/:company_id/secret-providers/health` 从空数组改为四项 provider health check。
- 增加 `secrets_contract` 对 provider 数量、字段和 health 形状的断言。
- 未宣称远端 provider 已可执行：GCP/Vault 明确返回未配置状态，AWS 只报告配置准备度。
- 复核 plugin bridge：Rust 已有真实 worker pool 与 HTTP→worker JSON-RPC 调用，不能把它误列为"全 stub"；差距收敛到双向回调与生命周期恢复。
- heartbeat scheduler 已接入 `pc-server`：每秒查询 recoverable runs，条件 claim queued/scheduled_retry，并复用 HTTP agent 的 adapter dispatch/sink；尚缺 Node 的复杂 readiness/staleness 策略。

6. **plugin worker 双向 RPC**（已部分完成）：`WorkerToHostHandler` trait + `JsonRpcStream::set_worker_to_host_handler` 注册 + `read_loop` 在 JSON-RPC 响应之外优先解析 `WORKER_TO_HOST_METHODS` 请求，dispatch 到 handler 后把响应回写 worker stdin。还需补齐 worker → host 通知分发、崩溃监控与指数重启。

7. **auth 密码哈希 + session rotation**（已部分完成）：
   - `pc-auth::hash_password` 使用 argon2id（19_456 KiB 内存 / 2 iters / 1 parallelism）+ 随机 salt 生成 PHC 字符串。
   - `pc-auth::verify_password` 解析 PHC 字符串并验证。
   - `pc-auth::generate_session_token` 生成 32 字节 URL-safe base64 token（无 `+`/`/`/`=` 填充）。
   - 还需把 sign-in 路由切换到 verify_password 并写入 session rotation。

## 本轮增量（v3 - 2026-08-04）

8. **live_events WebSocket auth**（P1 完成）：
   - `pc-http::routes::live_events` 接受 `?token=...&company_id=...` 查询参数；
   - `authorize_ws` 检查 bearer token → `board_api_keys.key_hash`；session fallback → `company_memberships`；
   - `local_trusted` 模式下 anonymous board context 放行；
   - `parse_bearer_token` 纯函数 + 3 单元测试覆盖（带 Bearer 前缀 / 纯 token / 空字符串）。

9. **status card watcher**（P2 完成）：
   - `pc-http::routes::status_cards::claim_due_status_card_updates` 用 `FOR UPDATE SKIP LOCKED` 原子认领 `next_eval_at <= now()` 且非 archived/非 generating 的卡片，把 state 切到 `pending_refresh`；
   - `pc-server` scheduler 每秒调用一次，发出 `status_card.tick.claimed` live event。

10. **tool gateway MCP 协议**（P2 完成）：
    - `pc-http::routes::tool_gateway::authorize_gateway` 校验 `tool_mcp_gateway_tokens.token_hash` + `revoked_at IS NULL` + `expires_at > now()`；
    - `post_gateway` 实现 MCP JSON-RPC 2.0 方法：`initialize` / `notifications/initialized` / `tools/list` / `tools/call`，未知方法返回 -32601；
    - `tools/call` 通过 `tool_gateway.call_requested` live event 把工具名 + params 推送给 worker pipeline；
    - bearer 缺失或无效时返回 401。

---

## Round 87 增量（pc-telemetry::feedback_share）

- feedback trace 上传客户端已落地：`crates/pc-telemetry/src/feedback_share.rs` 完整 1:1 port Node `server/src/services/feedback-share-client.ts`。
- 公开 API：`FeedbackTraceBundle` / `FeedbackTraceShareClient` trait / `HttpFeedbackTraceShareClient` / `FeedbackShareConfig` / `create_feedback_trace_share_client_from_config` / `build_feedback_share_object_key` / `encode_feedback_share_payload` / `decode_feedback_share_payload` / `FeedbackTraceShareError` / `UploadTraceBundleResponse` / `DEFAULT_FEEDBACK_EXPORT_BACKEND_URL` / `FEEDBACK_SHARE_ENCODING`。
- 行为对齐：`gzip+base64+json` 编码、UTC 日期分段对象键、`exportId ?? traceId`、Bearer token trim 化、响应 `objectKey` 优先回退、空 body 错误文案 fallback。
- 测试：14/14 新增（3 异步集成 + 11 同步单测）；用 `tokio::net::TcpListener` 本地 mock server 校验 POST/headers/payload/响应解析三种场景。
- 依赖：`reqwest 0.12` (rustls+json) / `flate2 1` / `base64 0.22` / `thiserror 1` / `async-trait 0.1` / `serde_json 1`（lib 必需）。
- 尚存差距：触发端点（`feedback-export.ts` → HTTP route）未接；`FeedbackTraceBundle` 在 pc-telemetry 局部建模，未共享到 pc-core（设计选择：避免业务层反向依赖遥测）。

---

## Round 88 增量（pc-core::agent_eligibility + pc-repos::agent_assignability）

- agent eligibility 纯规则层已落地：`crates/pc-core/src/agent_eligibility.rs` 完整 1:1 port Node `packages/shared/src/agent-eligibility.ts`（245 行）。
- 公开 API：4 个枚举（`AgentEligibilityLifecycleReason` / `AgentOrgChainInvalidReason` / `AgentOrgChainHealthStatus` / `AgentOrgChainRelation`）+ 5 个结构体（`AgentEligibilityAgent` / `AgentOrgChainEntry` / `AgentInvalidOrgChainAncestor` / `AgentOrgChainHealth` / `AgentWorkEligibility`）+ 6 个公开函数（`is_agent_status_assignable_to_work` / `is_agent_status_invokable` / `get_agent_org_chain_health` / `get_agent_work_eligibility` / `is_agent_assignable_to_work` / `is_agent_invokable`）+ 4 个 status 集合常量。
- 行为对齐：4 个 status 集合 + org chain 遍历（seen 防环 / 跨 company missing / 终止态收集 / repair guidance）+ 优先级（先 status 再 org chain）+ serde 字节级 snake_case 一致。
- agent assignability DB 适配层已落地：`crates/pc-repos/src/agent_assignability.rs` 完整 1:1 port Node `services/agent-assignability.ts`（171 行）。
- 公开 API：2 个枚举（`AgentAssignmentKind` / `AgentAssignmentConflictReason`）+ 2 个结构体（`AgentAssignabilityConflictDetails` / `ConflictChainEntry`）+ 1 个错误类型（`AgentAssignabilityError`）+ 1 个 options 类型（`AssertAssignableAgentOptions`）+ 1 个入口（`assert_assignable_agent`）+ 6 个 pure 助手（`to_eligibility_agent` / `to_eligibility_agents` / `chain_to_conflict_entries` / `make_conflict_details` / `assignment_message` / `assignment_reason_from_health`）。
- 行为对齐：4 个失败分支（null agentId / 跨公司 / 未找到 / 冲突）+ 7 种文案 1:1 + `code: "agent_not_assignable"` + 缺省回退 `ancestor_missing`。
- 测试：19/19 新增（pc-core 11 + pc-repos 8）；纯规则单测覆盖 7 个 Node 测试用例 + 跨 company 边界 + 根节点边界；pc-repos 单测覆盖文案 + details 形状 + reason 映射 + chain 转换。
- 跨 crate 复用：纯规则层 pc-core / DB 适配层 pc-repos，与既有 `tool_profile_binding / portability_fidelity` 风格一致。
- 尚存差距：DB IO 集成测试需要 `DATABASE_URL`；HTTP 路由未暴露 `assert_assignable_agent` 调用方。

---

## Round 89 增量（pc-repos::agent_invokability）

- agent invokability 校验层已落地：`crates/pc-repos/src/agent_invokability.rs` 完整 1:1 port Node `services/agent-invokability.ts`（164 行）。
- 公开 API：1 个 row 类型（`AgentOrgRow`）/ 1 个 reason 枚举（`AgentInvokabilityBlockReason` 10 项）/ 1 个 details 结构体（带 `#[serde(flatten)] extra` 兼容 Node free-form）/ 1 个判别式 enum（`AgentInvokability` tag=`invokable`）/ 1 个 status 集合常量（`DIRECT_NON_INVOKABLE_STATUSES`）/ 4 个公开函数（`evaluate_agent_invokability` / `evaluate_agent_invokability_from_db` / `list_invalid_org_chain_descendant_ids` / `should_cancel_runs_for_non_invokable_agent`）。
- 行为对齐：4 个评估分支（null / status / 正常 / invalid chain）+ 3 种 status → reason + terminated/cycle/missing 3 种 chain → reason + details 6 个命名字段键名 + reporting_chain_agent_ids 仅 ancestor 项 + DFS seen 防环 + should_cancel 判定公式。
- 与 assignability 共享 pc-core 纯规则层：两个 module 都用 `pc_core::agent_eligibility::get_agent_work_eligibility`，但上层业务语义不同（assignable = 可分配 / paused 仍可；invokable = 可启动 / paused 不可）。
- 测试：18/18 新增；覆盖 3 个 Node 测试用例 + 6 种状态分支 + 4 种取消决策 + 序列化 discriminator + reason 字符串 round-trip。
- 跨 crate 复用：纯规则 pc-core / DB 适配 pc-repos / 判别式 enum 严格对齐 Node 判别式。
- 尚存差距：DB IO 集成测试需要 `DATABASE_URL`；`ManagerCompanyMismatch` / `ReportingChainTooDeep` 当前无触发路径（保留枚举项便于扩展）；HTTP 路由未暴露 invokability endpoint。

---

## Round 90 增量（pc-core::routable_blocked）

- routable blocked 通知投递已落地：`crates/pc-core/src/routable_blocked.rs` 完整 1:1 port Node `services/routable-blocked.ts`（54 行）。
- 公开 API：1 个 owner 判别式 enum（`IssueUnblockOwner`）/ 1 个 descriptor 结构体（`IssueUnblockDescriptor`）/ 1 个 issue 形状结构体（`RoutableBlockedIssue` + `is_prospective_blocked_transition` 助手）/ 1 个 wakeup 请求结构体（`AgentWakeupRequest` + 2 个子结构体 `IssueUnblockPayload` / `IssueUnblockContextSnapshot`）/ 2 个 async trait（`WakeupNotifier` / `NotifiedMarker`）/ 1 个 input 类型（`DeliverAgentUnblockNotificationInput`）/ 1 个 rollout 时间函数（`routable_blocked_rollout_at()` 用 `OnceLock` 懒初始化）。
- 行为对齐：3 条 ALL 条件（status / transitionAt / rollout）+ 4 个短路条件（非 prospective / 无 descriptor / 已 notified / 非 agent owner）+ 6 个 wakeup 字段 + `idempotencyKey` 格式（含毫秒精度）+ `taskId === issueId`。
- 设计选择：trait DI（`WakeupNotifier` / `NotifiedMarker`）替代 Node 内联函数注入，便于 mockall 自动 fake + 类型清晰；`OnceLock` 替代 chrono 不支持的 const 构造器；`is_prospective_blocked_transition` 作为公开 bool 方法替代 Node type guard。
- 测试：10/10 新增；覆盖 3 个 Node 测试用例 + 5 个判定边界 + 3 种 owner 短路 + Utc::now 默认时钟 + helper 单元测试。
- 跨 crate 复用：纯逻辑放 pc-core，副作用由 trait 注入；HTTP 层后续只需实现 2 个 trait 即可接通。
- 尚存差距：HTTP route 层接线未做；`WakeupNotifier` / `NotifiedMarker` 的真实实现留待路由层 wiring 时提供。

---

## Round 91 增量（pc-repos::sidebar_badges）

- sidebar badges 聚合已落地：`crates/pc-repos/src/sidebar_badges.rs` 完整 1:1 port Node `services/sidebar-badges.ts`（86 行）。
- 公开 API：2 个 status 集合常量（`ACTIONABLE_APPROVAL_STATUSES` / `FAILED_HEARTBEAT_STATUSES`）/ 1 个输出结构体（`SidebarBadges` + `zero()`）/ 1 个注入项结构体（`JoinRequestEntry`）/ 1 个 extra 注入类型（`SidebarBadgesExtra`）/ 3 个 pure 函数（`normalize_timestamp` / `normalize_timestamp_millis` / `is_dismissed`）/ 1 个服务结构体（`SidebarBadgesService`）+ 1 个 async 方法（`get(company_id, extra?)`）。
- 行为对齐：4 个输出字段（inbox / approvals / failedRuns / joinRequests）+ inbox 公式 + dismiss 抑制（dismissedAt >= activityAt）+ 2 个 status 集合 + DISTINCT ON + 非 terminated agent 过滤。
- 与 pc-http 路由 `routes/sidebar_badges.rs` 互补：HTTP route 输出扩展形状（agents/issues/costs/runs 细分），本 module 输出 Node `SidebarBadges` 形状；可同时存在。
- 测试：10/10 新增；覆盖 pure helper 5 个 + 常量 3 个 + 公式 1 个 + 类型 1 个。
- 尚存差距：DB 聚合集成测试需要 `DATABASE_URL`；HTTP 路由未暴露本 module 的 `SidebarBadges` 形状（既有路由输出不同形状）。

---

## Round 92 增量（pc-core::runtime_skill_selections）

- runtime skill version selection map 已落地：`crates/pc-core/src/runtime_skill_selections.rs` 完整 1:1 port Node `services/runtime-skill-selections.ts`（7 行）。
- 公开 API：1 个 entry 结构体（`SkillVersionSelectionEntry` + `new` 构造器）/ 1 个 options 结构体（`SkillVersionSelectionOptions` + `new(bool)` + `Default`）/ 1 个公开函数（`skill_version_selection_map`）。
- 行为对齐：`versionPinsEnabled` 缺省 `true` + 关闭时强制 `version_id = null` + 返回 `Map<key, versionId|null>` 1:1。
- 测试：7/7 新增；覆盖默认 / 显式启用 / 显式关闭 / 空 entries / 重复 key / 构造器。
- 尚存差距：调用方（plugin / skill runtime）尚未在 pc-repos / pc-http 中接线。

---

## Round 93 增量（pc-core::source_trust + pc-repos::source_trust）

- source trust 跨层已落地：纯规则（pc-core）+ DB 适配（pc-repos），对齐 Node `services/source-trust.ts`（173 行）。
- 公开 API（pc-core）：4 个常量（preset / disposition / placeholder body）/ 5 个枚举（disposition / artifact_kind / actor_type / preset alias / promoted_at input）/ 2 个 metadata 结构体（SourceTrustMetadata / SourceTrustPromotionSource）/ 2 个 build 输入结构体 / 5 个公开函数（is_low_trust_quarantined / redact / sanitize / build_low_trust / build_promoted）/ 2 个 trait（SourceTrustRedactable / SourceTrustCommentSanitizable）。
- 公开 API（pc-repos）：3 个枚举（actor_type / preset_resolution）/ 4 个结构体（SourceTrustActor / SourceTrustIssueContext / ResolveCoreTrustPresetInput + 4 slice）/ 1 个 async trait（TrustPresetResolver）/ 1 个错误类型（SourceTrustError）/ 1 个公开 async fn（resolve_actor_source_trust_for_issue）。
- 行为对齐：4 个构建路径（isLowTrustQuarantined / redact / sanitize / build_promoted）/ fail-closed 语义 / Promise.all → tokio::try_join 并发 / 5 个 promote artifact kind / 3 个 promote actor type。
- 设计选择：`TrustPresetResolver` trait 注入 → 不依赖未 port 的 `trust-preset-resolver.ts`（349 行），可独立演进；`SourceTrustRedactable` / `SourceTrustCommentSanitizable` trait → 表达 Node 端泛型 `T extends { body?, sourceTrust? }`。
- 测试：20/20 新增（pc-core 11 + pc-repos 9）；pc-repos 通过 wrapper 函数测 guard clause 避免依赖真实 DB。
- 尚存差距：`TrustPresetResolver` trait impl 待 port `trust-preset-resolver.ts`；DB IO 集成测试需要 `DATABASE_URL`；HTTP route 层未暴露 `resolve_actor_source_trust_for_issue` 调用方。

---

## Round 94 增量（pc-repos::task_watchdog_scope）

- task watchdog mutation scope 解析 + 子树校验已落地：`crates/pc-repos/src/task_watchdog_scope/`（mod/ 拆分 4 个 sub-files）完整 1:1 port Node `services/task-watchdog-scope.ts`（174 行）。
- 子文件结构（mod/ 拆分）：
  - `mod.rs`：facade + re-exports
  - `types.rs`：3 个公开类型（`AgentRunActor` / `IssueScopeTarget` / `TaskWatchdogMutationScope` 判别式 enum）+ 1 个 kind 标签 enum（`TaskWatchdogMutationScopeKind`）+ `TASK_WATCHDOG_ORIGIN_KIND` 常量
  - `helpers.rs`：4 个纯助手（`is_plain_record` / `as_plain_record` / `read_string` / `read_task_watchdog_context`）+ 9 个内联单测
  - `resolver.rs`：3 个 async fn（`resolve_task_watchdog_mutation_scope` / `issue_is_in_task_watchdog_subtree` / `task_watchdog_scope_allows_issue_mutation`）+ 1 个 options 类型（`TaskWatchdogScopeAllowsOptions`）+ `MAX_WATCHDOG_SCOPE_ANCESTRY_DEPTH = 100` 常量
  - `tests.rs`：10 个 mod 级单测
- 行为对齐：8 个 resolve 分支 + 5 个 subtree 终止条件 + 4 个 allows 分支 + Node 全部 helper 函数。
- 设计选择：mod/ 拆分（按 `docs/08-RUST-MODULAR-ARCHITECTURE.md` 门槛 3 类职责）；判别式 enum + 标签 enum 双层；`scope_to_watchdog` 重建解决 enum 返回 + 字段保留问题。
- 测试：19/19 新增；helpers 9 个 + mod-level 10 个。
- 尚存差距：DB IO 集成测试需要 `DATABASE_URL`；`task_watchdog_scope_allows_issue_mutation` 在子树内分支用重建 scope 失去 `watchdog_id` / `stop_fingerprint`；HTTP route 层未暴露。

---

## Round 95 增量（pc-repos::successful_run_handoff_state）

- successful run handoff 状态 hydrate + resolve 已落地：`crates/pc-repos/src/successful_run_handoff_state.rs` 完整 1:1 port Node `services/successful-run-handoff-state.ts`（128 行）。
- 公开 API：2 个 status 集合常量 / 1 个 kind 枚举 / 1 个 DateTime 兼容枚举 / 1 个 state 结构体（9 字段 camelCase）/ 1 个 input 结构体 / 2 个 async fn。
- 行为对齐：4 步 hydrate（过滤 required / 并发拉 / 构造 live map / 原地更新）+ `hasLiveContinuation = liveRun || liveWake` 公式 + 3 步 resolve（查 latest / 验证 required / 写 resolved 日志）+ 6 字段 details 写入。
- 设计选择：单文件（128 行 + ~280 行含测试）/ `JsonDateTime` untagged 兼容 Node 端 `Date | string | null` / `tokio::try_join!` 并发拉取 / `coalesce(...)` JSON path 提取 / `ActivityRepo::record` 通过 `map_err` 适配 `RepoError → sqlx::Error`。
- 测试：8/8 新增；覆盖 2 个 status 集合 + 1 个 kind 枚举 + 1 个 DateTime 兼容 + 1 个 state 字段 + 1 个 input 字段。
- 尚存差距：DB IO 集成测试需要 `DATABASE_URL`；HTTP route 层未暴露。

---

## Round 114 增量（pc-plugin-host::bundled_plugins）

- bundled plugin sandbox provider auto-install 解析 + 安装已落地：`crates/pc-plugin-host/src/bundled_plugins/`（mod/ 拆分 5 个 sub-files）完整 1:1 port Node `services/bundled-plugins.ts`（297 行）。
- 子文件结构（mod/ 拆分）：
  - `types.rs`：3 个核心 struct + 3 个 trait（async） + typed logger（trait + LogFields + LogValue） + DI 容器（`BundledPluginProvisionerDeps<L, R, Li>`）+ 3 个错误类型 + `EnvMap` 别名
  - `catalog.rs`：`DEFAULT_BUNDLED_CATALOG_ROOT` / `BUNDLED_CATALOG_ROOT_ENV_VAR` / `KUBERNETES_PLUGIN_PATH_ENV_VAR` / `BUNDLED_PLUGIN_CATALOG`（LazyLock<Vec>，7 项）/ `SELF_HOSTED_AUTO_INSTALL_KEYS` / `resolve_bundled_catalog_root`
  - `resolve.rs`：`BundledPluginError` enum / `lexical_resolve` / `canonicalize` / `is_inside_root` / `ResolveBundledPluginOptions` / `resolve_bundled_plugin_installs`
  - `provision.rs`：`ProvisionError` enum / `default_bundle_manifest_exists` / `EnsureBundledPluginsOptions` / `ensure_bundled_plugins`（async + fail-safe per entry）
  - `mod.rs`：facade + 22 项 re-export
- 行为对齐：fail-fast 解析（未知 key / path escape throw）+ fail-safe 安装（disk missing / install error / load error → log + swallow）+ 跳过语义四分支（status≠uninstalled / uninstalled+!reinstall / bundle missing / reinstall）1:1
- 设计选择：`LazyLock<Vec<_>>` 替代 `const Vec`（Rust stable 不支持 const String 构造）/ `canonicalize` IO-free（与 Node `realpathSync` 同步阻塞 IO 的取舍）/ 4 个 trait async-trait 解耦 loader/registry/lifecycle/logger 注入
- 测试：39/39 新增（catalog 10 + resolve 18 + provision 11）
- 尚存差距：`createApp` boot hook 待 wiring（属于 wiring 任务）/ `canonicalize` 未做 symlink resolve / `ensure_bundled_plugins` 不抛 `ProvisionError` 给调用者（fail-safe 设计）

---

## Round 96–113 增量（gap-matrix 补登）

> 以下轮次在 Round 114 之前完成、但在 gap matrix 中未独立列条目，本节集中补登。
> 每个条目一行概括，详细测试 / 模块结构 / 行为对齐见 `docs/05-PROGRESS-AUDIT.md`。

- **Round 96**：`pc-core::portable_path` — portable path 归一化（12 行 Node）
- **Round 97**：`pc-agent::built_in_agent_metadata` — built-in agent marker 解析与比较
- **Round 98**：`pc-repos::issue_visibility` — issue 可见性谓词
- **Round 99**：`pc-repos::asset` — assets 表 CRUD
- **Round 100**：`pc-repos::issue_goal_fallback` — issue goal 解析
- **Round 101**：`pc-repos::issue_assignment_wakeup` — issue 分配 wakeup 派发
- **Round 102**：`pc-repos::inbox_agent_policy` — inbox agent 政策 CRUD
- **Round 103**：`pc-repos::session_workspace_cwd` — session workspace CWD 安全性校验
- **Round 104**：`pc-adapter-api::models_env` — adapter models 环境变量解析
- **Round 105**：`pc-repos::decision_training` — decision training 域 mod/ 拆分（+46 tests）
- **Round 106**：`pc-repos::tool_runtime_metrics` — tool runtime metric 计数（+7 tests）
- **Round 107**：`pc-repos::plugin_log_retention` — plugin log 周期清理（+9 tests）
- **Round 108**：`pc-plugin-host::plugin_stream_bus` — plugin 流事件总线（+15 tests）
- **Round 109**：`pc-plugin-protocol::config_validator` — plugin config JSON Schema 校验（+14 tests）
- **Round 110**：`pc-plugin-host::plugin_event_bus` — plugin 事件总线 mod/ 拆分（+31 tests）
- **Round 111**：`pc-core::tool_content_guards` — tool content 校验 + HMAC 签名（+27 tests）
- **Round 112**：`pc-repos::issue_continuation_summary` — issue continuation summary mod/ 拆分（+38 tests）
- **Round 113**：`pc-repos::plugin_state_store` — plugin state scoped KV 持久化（+9 tests）

---

## Round 115 增量（pc-core::feature_catalog + pc-core::managed_config）

- cloud managed-config bootstrap（harness → app contract）已落地：
  - `crates/pc-core/src/feature_catalog.rs` 完整 1:1 port Node `packages/shared/src/feature-catalog.ts`（282 行）：`FeatureTier` enum / `InstanceFeatureKey` enum（26 项）/ `FeatureCatalogEntry` struct / `INSTANCE_FEATURE_CATALOG`（LazyLock<HashMap>）/ `INSTANCE_FEATURE_KEYS` / `tier_of` / `is_managed`
  - `crates/pc-core/src/managed_config/`（mod/ 拆分 4 个 sub-files）完整 1:1 port Node `server/src/services/managed-config.ts`（354 行）：
    - `types.rs`：`ManagedInstanceConfig` / `ManagedEnvironmentSpec` / `ManagedConfigEnv` / `MANAGED_CONFIG_ENV_KEY` / `SUPPORTED_MANAGED_CONFIG_VERSION`
    - `secrets.rs`：`SECRET_LIKE_CONFIG_KEY_PATTERN`（LazyLock<regex::Regex>）+ `find_secret_like_config_key`（递归扫描任意 JSON value）
    - `parser.rs`：`parse_managed_config_env`（fail-closed 解析）/ `get_managed_instance_config`（parse-once cache，仅缓存成功）/ `clear_managed_config_cache`（测试隔离）
    - `mod.rs`：facade + 11 项 re-export
- 行为对齐：env var 缺失 → None（self-hosted）；env var 存在但任何字段错误 → throw（fail-closed）；features / plugins.autoInstall 缺失 → throw；tier ≠ "managed" 的 feature key → throw；environment provider 不在 auto_install → throw；environment config 含 secret-like key → throw（含嵌套 + 数组下标路径）
- 设计选择：`LazyLock<HashMap>` 替代 `const HashMap`（stable Rust 不支持 const HashMap 构造）/ `OnceLock<regex::Regex>` 自定义 `CompiledPattern` 避免 `once_cell` 依赖 / `OnceLock<Mutex<CacheEntry>>` 与既有 `routable_blocked` 模式一致 / `serde_json::Value` 替代 typed `unknown`（保留原始 JSON 形状）
- 测试：66/66 新增（feature_catalog 11 + secrets 11 + parser 44）
- 尚存差距：`instanceExperimentalSettingsSchema` 非 boolean 字段未独立 port / `buildFeatureCatalogArtifact` + `renderFeatureCatalogArtifact` 未 port（release artifact 生成）/ HTTP route / `createApp` boot hook 未 wiring

---

## Round 116 增量（pc-core::execution_workspace_policy）

- execution workspace 策略层已落地：`crates/pc-core/src/execution_workspace_policy/`（mod/ 拆分 6 个 sub-files）完整 1:1 port Node `services/execution-workspace-policy.ts`（347 行）：
  - `types.rs`：3 个字符串字面量模块（mode / default_mode / strategy_type）+ `ExecutionWorkspaceStrategy`（含 Serialize + skip_serializing_if）/ `ProjectExecutionWorkspacePolicy` / `IssueExecutionWorkspaceSettings` / `NetworkEgress` / `ParsedExecutionWorkspaceMode` + `is_parsed_mode` / `UnrunnableWorktreeIssueRef` / `ExecutionWorkspaceEnvironmentResolution` / `environment_source`
  - `parse.rs`：`parse_object` / `as_string` helper + `parse_execution_workspace_strategy` + `parse_project_execution_workspace_policy` + `parse_issue_execution_workspace_settings` + `select_environment_execution_workspace_settings`
  - `resolve.rs`：`resolve_effective_workspace_strategy_type` / `resolve_pinned_issue_workspace_strategy_type` / `default_issue_execution_workspace_settings_for_project` / `issue_execution_workspace_mode_for_persisted_workspace` / `resolve_execution_workspace_mode` / `resolve_execution_workspace_environment_id`
  - `guard.rs`：`WORKSPACE_WORKTREE_REQUIRES_PROJECT_CODE` / `_REMEDIATION` / `_MESSAGE` + `has_reusable_execution_workspace_binding` + `is_unrunnable_worktree_combo` + `IsUnrunnableWorktreeComboInput`
  - `build.rs`：`build_execution_workspace_adapter_config` + `BuildExecutionWorkspaceAdapterConfigInput`
  - `mod.rs`：facade + 22 项 re-export
- 行为对齐：字符串字面量 union 全 4 个值集合、type 白名单、mode 归一化（project_primary → shared_workspace + isolated → isolated_workspace）、5 条件 AND 守卫、3 级 environment ID 优先级、4 级 mode 优先级、完整 adapter config 构造逻辑 1:1
- 设计选择：`&'static str` 常量而非 enum（与 `serde_json::Value` 互通 + 允许 forward-compatibility）/ `Serialize + skip_serializing_if = "Option::is_none"`（wire 格式 camelCase 与 Node 1:1）/ `&'a str` 而非 `&'a String`（让调用方可传字面量）/ 5 级 mod/ 拆分（types / parse / resolve / guard / build）
- 测试：89/89 新增（types 9 + parse 31 + resolve 29 + guard 10 + build 10）
- 尚存差距：`ParsedExecutionWorkspaceMode` 编译期不保证合法 value（运行时校验）/ `workspaceRuntime` 保留为 `HashMap<String, Value>` 而非 typed projection / `gateProjectExecutionWorkspacePolicy` 未单独 port（其功能已被 `parse_project_execution_workspace_policy` 涵盖）/ HTTP route / DB 层 wiring 后续

---

## Round 117 增量（pc-plugin-host::plugin_install_guard）

- cloud install floor + localPath canonicalization 已落地：`crates/pc-plugin-host/src/plugin_install_guard/`（单文件 417 行含测试）完整 1:1 port Node `services/plugin-install-guard.ts`（132 行）：
  - `MANAGED_CONFIG_ENV_KEY` + `BUNDLED_LOCAL_PLUGIN_ROOT` 常量
  - `EnvMap` type alias
  - `is_cloud_managed_instance(env)` — presence-based 检测（不读文档内容）
  - `LocalPluginPathValidation` 判别式 enum（Ok / Failed）
  - `canonicalize_local_plugin_path(raw_path)` — async：null byte 拒绝 + lexical resolve + realpath + dir check
  - `is_within_bundled_plugin_root(canonical_path, override?)` — async：bundled root 存在 + segment-based containment
  - `lexical_resolve` helper — `std::path::Path::components` 实现
- 行为对齐：presence-based cloud detection（corrupted document 不能 widen install surface）/ null byte injection 防护 / fail-closed semantics（bundled root 不存在 → 全部 deny；root 本身不视为内部）/ segment-based containment（不是字符串前缀）
- 设计选择：async IO via tokio（替代 Node 同步阻塞 IO）/ `Path::strip_prefix` 替代字符串前缀（防止 `prefix_attack`）/ alias re-export 避免与 bundled_plugins 重名冲突
- 测试：17/17 新增（is_cloud_managed_instance 3 + lexical_resolve 5 + canonicalize 4 + within_root 4 + 1 常量）
- 尚存差距：`BUNDLED_LOCAL_PLUGIN_ROOT` 与 bundled_plugins::DEFAULT_BUNDLED_CATALOG_ROOT 同值但不同语义（保留 Node 的双重设计）/ 测试用 temp_dir + 手工 cleanup（非 atomic）/ HTTP route `POST /api/plugins/install` 未 wiring

---

## Round 118 增量（pc-heartbeat::run_scratch）

- heartbeat run scratch 目录管理已落地：`crates/pc-heartbeat/src/run_scratch.rs`（单文件 806 行含测试）完整 1:1 port Node `services/run-scratch.ts`（157 行）：
  - 常量：`HEARTBEAT_RUN_SCRATCH_MARKER` + 4 Paperclip env vars + 3 TMPDIR env vars
  - 类型：`HeartbeatRunScratchMetadata`（含 `rename_all = "camelCase"` serde）/ `HeartbeatRunScratch` / `HeartbeatRunScratchEnvResult` / `HeartbeatRunScratchCleanupResult` / `CleanupFailureReason`
  - async 函数：`prepare_heartbeat_run_scratch`（sanitize + create_dir + write marker + chmod 0o600）/ `cleanup_heartbeat_run_scratch`（4 步 fail-closed）
  - pure 函数：`build_heartbeat_run_scratch_env`（4 + 3 env vars 注入策略）
  - helper：`sanitize_path_segment`（7 规则）/ `is_path_inside`（segment-based）/ `read_marker`（JSON + 字段校验）
- 行为对齐：fail-closed cleanup 4 步 AND 检查（containment + prefix + owner + process group alive）/ `rename_all = "camelCase"` 与 Node 字段命名 1:1 / sanitize 7 规则 / TMPDIR 保留已有值
- 设计选择：`tokio::fs::create_dir_all` 替代 `mkdtemp_in`（tokio 没有带前缀的 mkdtemp）/ `std::path::absolute` 规范化 dir / `#[cfg(unix)]` 守卫 0o600 mode / `now: Option<DateTime<Utc>>` 测试可注入
- 测试：21/21 新增（sanitize 7 + is_inside 3 + env 2 + prepare+cleanup integration 7 + read_marker 2）
- 尚存差距：`tmp_dir()` 不解析 symlink（macOS `/var/folders/...` 与 `/private/var/folders/...` 通过 segment 比较兼容）/ 测试 `tempdir()` helper 同步创建目录（与 async 测试串行 OK）/ `process_group_id: Option<i32>` 简化（与 Node `number` 等价）/ `eprintln!` 而非 `tracing` 日志 / HeartbeatRunActor wiring 待办

---

## Round 119 增量（pc-core::execution_policy_bootstrap）

- cloud forced-execution-mode env 解析（pure 部分）已落地：`crates/pc-core/src/execution_policy_bootstrap.rs`（单文件 820 行含测试）完整 1:1 port Node `services/execution-policy-bootstrap.ts`（194 行）的 pure 解析部分：
  - 11 个 env var 常量 + `EnvMap` type alias
  - 3 个 enum：`ExecutionMode`（仅 Kubernetes 一变体）/ `KubernetesBackend`（Job / SandboxCr）/ `KubernetesEgressMode`（Cilium / Standard）
  - `KubernetesEnvironmentConfigInput` struct（含 `Serialize` + `Deserialize` + `rename_all = "camelCase"` + `skip_serializing_if`）/ `ExecutionPolicyBootstrap` struct
  - `ExecutionPolicyBootstrapError` enum（4 变体 thiserror-based）
  - 3 个 helper：`parse_bool` / `parse_positive_int_ms`（返回 Result）/ `parse_list`
  - 主函数 `parse_execution_policy_bootstrap_env(env)` 返回 `Result<Option<...>, _>`
- 行为对齐：fail-loud on misconfig（未知 mode / backend / egress / 不合法 timeout 抛错）/ 空 / `="any"` / `="kubernetes"` 三态 1:1 / 10 个 K8s 字段 passthrough 1:1 / `in_cluster` 默认 `false` 1:1
- 设计选择：pure parsing only（DB-dependent `applyExecutionPolicyBootstrap` 属于 wiring 任务）/ `rename_all = "camelCase"` wire 格式 1:1 / `skip_serializing_if` 序列化时 None 字段被跳过 / enum + as_str/parse 双函数（与 zod enum 等价）/ `adapters` 字段保留 `serde_json::Value`（与 `parseAdapterRegistryEnv` 输出对齐，待后续 port）
- 测试：43/43 新增（parse_bool 4 + parse_positive_int_ms 6 + parse_list 5 + parse_execution_policy_bootstrap_env 21 + type-level 7）
- 尚存差距：`adapters` 字段仍保留 JSON Value（Round 120 已提供强类型 `AdapterRegistryEntry`，待 bootstrap wiring 时替换）/ DB-dependent `applyExecutionPolicyBootstrap` 未 port（属于 wiring 任务）/ `KubernetesEnvironmentConfigInput` 不带 `[key: string]: unknown` 索引签名（Rust strict 11 字段）/ createApp boot hook 未 wiring

## Round 120 增量（pc-core::adapter_registry_bootstrap）

- declarative adapter registry 已落地：`crates/pc-core/src/adapter_registry_bootstrap.rs` 完整迁移 Node `services/adapter-registry-bootstrap.ts` 的配置解析与 availability reconciliation 规则。
- `AdapterRegistryEntry` 严格对齐共享 Zod schema：camelCase、拒绝未知字段、`enabled=true` 默认值、六类可选 runtime 字段和 `defaultEnv: Record<string,string>`。
- `parse_adapter_registry_env` 对齐 env 优先级：trim 后的 `PAPERCLIP_ADAPTERS` 优先于 `PAPERCLIP_ADAPTERS_FILE`；均为空时返回 `None`；文件不可读、JSON 语法错误、schema 校验失败均 fail-loud。
- `reconcile_adapter_availability` 保持 Node `Map` 语义：重复声明后者覆盖前者；未声明的已安装 adapter 禁用；声明但未安装的 adapter 集合报错；输出顺序保持 server adapter registry 顺序。
- 新增 22 个契约测试；`pc-core` 385 → 407；workspace 约 1235 → 1257 passing。
- 尚存差距：disabled-set 状态写入、logger 与 createApp boot hook 尚未 wiring；`execution_policy_bootstrap::KubernetesEnvironmentConfigInput.adapters` 尚未由 JSON Value 收紧为 `Vec<AdapterRegistryEntry>`。
