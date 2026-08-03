# Paperclip Node → paperclip-rs 差距矩阵

更新时间：2026-08-04

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
| P2 | `routes/company-skills.ts` | CRUD 基础实现 | manifest 构建、运行时 materialization 与 Node skill loader 差异 | skill materializer |

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

## 验证基线

```text
cargo check -p pc-http                         0 errors
cargo test -p pc-http --test secrets_contract  5 passed
cargo test -p pc-repos --lib                   57 passed
cargo test -p pc-http --lib routes::plugins::tests 1 passed
```
