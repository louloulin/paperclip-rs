# paperclip-rs-rewrite — 设计文档

## Context

Paperclip 当前为 pnpm monorepo：`server`（Node + Express，56 路由 / 212 服务 / 760 文件）、`ui`（React + Vite，60 API 客户端 / 1168 文件）、`cli`（Node CLI）、`packages/{adapters,db,shared,skills-catalog,plugins}`。后端在生产部署中承担 HTTP API、WebSocket live-events、心跳调度、适配器进程编排、插件 Worker 池、嵌入式 PostgreSQL、备份恢复、密钥管理等职责，单进程内存 400MB+。本次重写在仓库同级建立独立 Rust workspace `paperclip-rs/`，按现有模块边界切分为约 30 个 crate，前端通过 HTTP/WS 与之通信，**前端代码完全复用**。

### 关键事实（来自当前 paperclip 仓库）

| 维度 | 数量/规模 |
|---|---|
| server TS 源文件 | 760 |
| server TS 代码行 | ~444,000 |
| server 路由模块 | 56 |
| server 服务模块 | 212 |
| DB schema 表 | 109 |
| DB 迁移 | 持续增长（每次 schema 演进一条） |
| 内置适配器 | 11 |
| 插件 SDK 模块 | `definePlugin`、`runWorker`、`worker-rpc-host`、`host-client-factory`、`protocol`、`testing`、`bundlers`、`dev-server` |
| UI 源文件 | 1,168（含 .ts/.tsx） |
| UI 代码行 | ~344,000 |
| UI API 客户端模块 | 60 |
| CLI 命令 | 20+（run/install/onboard/doctor/worktree/heartbeat-run/pipelines/routines/service/update/...） |
| 实时协议 | WebSocket（`ws`）+ ping/pong + token 鉴权 |
| 数据库 | PostgreSQL（embedded-postgres 用于本地，外部 PG 用于部署） |
| 存储提供方 | local-disk、S3 |
| 密钥提供方 | local-encrypted、AWS Secrets Manager |
| 认证 | better-auth（session + cookie + API key） |
| 鉴权 | 资源/动作/主体三元组策略 |
| 可观测性 | pino + OpenTelemetry |

## Goals / Non-Goals

**Goals**

1. **行为等价**：API/WS 契约、数据库 schema、错误语义、可观测信号与原 server 一致；前端无需修改即可对接。
2. **模块化**：每个 crate 单一职责，`pub` API 最小化；crate 间通过 `paperclip-*` 路径依赖，无循环依赖。
3. **强类型**：领域类型贯穿 crate 边界，数据库行 → 仓储返回类型 → 服务层 → HTTP 响应全程不丢失类型。
4. **可生产**：单二进制、musl 静态链接、graceful shutdown、健康检查、结构化日志、OpenTelemetry trace。
5. **可测试**：以 trait 抽象 IO（DB、storage、secrets、realtime、adapter runtime），内存实现用于单元测试。
6. **增量迁移**：允许 Rust 后端与原 Node 后端并存（双栈），按模块逐步切换；UI 通过配置切换 base URL。

**Non-Goals**

- 不重写前端（React/UI 完全复用）。
- 不重写插件 worker 的运行时（仍可由 Node/Python 编写，只要遵循 JSON-RPC 协议）。
- 不改变数据库 schema（结构与数据兼容，无 ETL）。
- 不引入新的产品功能或 API 改动。
- 不替换 OpenAPI 文档机器可读来源（仍由 Rust 端生成）。
- 不解决嵌入式 PostgreSQL 在所有平台的预构建二进制可用性（沿用 `embedded-postgres` 思路，使用 `pg-embedded` Rust crate 或外部 `postgres` 二进制）。

## 目标架构（Rust Workspace）

```
paperclip-rs/
├── Cargo.toml                          # workspace 根
├── rust-toolchain.toml                 # 固定 stable 1.8x + rustfmt + clippy
├── crates/
│   ├── pc-core/                        # 领域模型（Company/Agent/Issue/Case/...）
│   ├── pc-errors/                      # 统一错误类型 → HTTP 状态码
│   ├── pc-telemetry/                   # tracing/OpenTelemetry/启动横幅
│   ├── pc-config/                      # 配置加载、env、.env、健康检查配置
│   ├── pc-db/                          # sqlx 连接池、迁移系统、schema DDL
│   │   ├── migrations/                 # 001_init.sql ... NNN_*.sql
│   │   └── schema/                     # 可选：Rust 结构化迁移定义
│   ├── pc-repos/                       # 仓储层（依赖 pc-core + pc-db）
│   ├── pc-auth/                        # session/cookie/API key、JWT
│   ├── pc-authz/                       # 授权策略引擎
│   ├── pc-storage/                     # StorageProvider trait + local-disk + s3
│   ├── pc-secrets/                     # SecretsProvider trait + local-encrypted + aws-sm
│   ├── pc-realtime/                    # 进程内 live-event 总线 + 可选 Redis pubsub
│   ├── pc-http/                        # axum 路由、middleware、OpenAPI、错误处理
│   ├── pc-ws/                          # WebSocket 升级、live-events handler、token 校验
│   ├── pc-activity/                    # 活动日志、成本事件、决策训练样本
│   ├── pc-workflow/                    # routines、pipelines、scheduler
│   ├── pc-heartbeat/                   # 心跳引擎：pick → schedule → invoke → collect
│   ├── pc-adapter-api/                 # 适配器 host 公共 trait + types
│   ├── pc-adapter-claude-local/        # 11 个内置适配器 host 实现
│   ├── pc-adapter-codex-local/
│   ├── pc-adapter-cursor-cloud/
│   ├── pc-adapter-cursor-local/
│   ├── pc-adapter-gemini-local/
│   ├── pc-adapter-grok-local/
│   ├── pc-adapter-openclaw-gateway/
│   ├── pc-adapter-opencode-local/
│   ├── pc-adapter-pi-local/
│   ├── pc-adapter-hermes-gateway/      # 如存在则映射
│   ├── pc-plugin-host/                 # 插件 Worker 池、RPC、事件总线、作业调度
│   ├── pc-plugin-protocol/             # JSON-RPC schema、消息类型、能力声明（host/worker 共享）
│   ├── pc-doc-anchors/                 # 文档锚点/批注
│   ├── pc-feature-flags/               # feature catalog
│   ├── pc-backup/                      # 数据库备份链路
│   ├── pc-openapi/                     # OpenAPI 生成器
│   └── pc-server/                      # 二进制入口：装配所有 crate，bin = paperclip-server
├── apps/
│   ├── pc-cli/                         # 二进制 paperclipai（命令集合）
│   └── pc-migrate/                     # 二进制 paperclip-migrate（独立 DB 迁移工具）
└── tests/
    └── integration/                    # 跨 crate 集成测试
```

**顶层依赖图**：

```
                       ┌─────────────────┐
                       │   pc-server     │  ← 二进制入口
                       │   pc-cli        │
                       └────────┬────────┘
                                │
        ┌───────────────────────┼──────────────────────────┐
        │                       │                          │
        ▼                       ▼                          ▼
  ┌───────────┐          ┌────────────┐             ┌──────────────┐
  │ pc-http   │          │ pc-ws      │             │ pc-heartbeat │
  │ pc-openapi│          │ pc-realtime│             │ pc-workflow  │
  └─────┬─────┘          └─────┬──────┘             └──────┬───────┘
        │                      │                           │
        ▼                      ▼                           ▼
  ┌──────────────────────────────────────────────────────────────┐
  │                       pc-repos                              │
  └────────────────────────────┬─────────────────────────────────┘
                               │
                ┌──────────────┼──────────────┐
                ▼              ▼              ▼
          ┌─────────┐    ┌─────────┐    ┌──────────┐
          │ pc-db   │    │ pc-core │    │ pc-errors│
          └─────────┘    └─────────┘    └──────────┘

  ┌───────────────┐  ┌──────────────┐  ┌──────────────────┐
  │ pc-auth/authz │  │ pc-storage   │  │ pc-secrets       │
  └───────────────┘  └──────────────┘  └──────────────────┘

  ┌────────────────────────────┐  ┌─────────────────────────┐
  │ pc-adapter-api + 11 impls  │  │ pc-plugin-host/protocol │
  └────────────────────────────┘  └─────────────────────────┘
```

## 关键设计决策

### D1. 异步运行时：tokio 多线程 + selective single-thread

- **选择**：`tokio = "1"` 多线程 runtime，CPU 密集任务（zod 等价校验、OpenAPI 生成、心跳策略评估）通过 `tokio::task::spawn_blocking` 隔离。
- **理由**：与 axum、sqlx、tower、hyper、reqwest 生态完全对齐；取消/超时/背压由 tokio 原生提供。
- **取舍**：不引入 async-std 或 smol；不引入重型 actor 框架（暂不需要）。

### D2. HTTP 框架：axum 0.7 + tower

- **选择**：`axum` 路由 + `tower` middleware + `tower-http`（compression、cors、trace、timeout）。
- **理由**：类型安全的 handler 签名；与 hyper 1.x 对齐；社区活跃；OpenAPI 通过 `aide` 或 `utoipa` 自动生成。
- **替代考虑**：actix-web（生态较小且自实现 runtime）；warp（维护减弱）。

### D3. 数据库访问：sqlx 0.8（compile-time checked queries）

- **选择**：`sqlx` 配合 Postgres driver，`query!`/`query_as!` 宏在编译期校验 SQL 与结果映射。
- **理由**：保留 SQL 表达力；编译期校验避免运行时 schema 漂移；与 Rust async 栈天然集成。
- **替代考虑**：`sea-orm`（实体抽象更厚但失去 SQL 灵活）；`diesel`（同步为主、async 支持弱）。

### D4. 数据库 schema：迁移系统复用 DDL

- **策略**：将 `paperclip/packages/db/src/migrations/*.sql`（或从 Drizzle TypeScript schema 推导的等价 SQL）作为 Rust 端迁移来源；`pc-db::Migrator` 复用 `sqlx::migrate!`。
- **理由**：原 109 张表已沉淀为生产 schema；保持 schema 不变可零数据迁移。

### D5. 实时通信：进程内 broadcast + 可选 Redis

- **选择**：`tokio::sync::broadcast` 作为单节点 live-event 总线；抽象 `RealtimeBus` trait，未来可注入 Redis pubsub 实现支持多副本。
- **理由**：原 server 单进程架构下 broadcast 足够；多副本部署是未来增量，本次不强制要求。

### D6. 适配器 host：保留 worker 子进程模型

- **策略**：适配器 worker 仍以独立子进程运行（Node/Python/Rust 任意），与 host 通过 stdio JSON-RPC 通信；host 侧由 `pc-adapter-*` crate 提供 11 个实现。
- **理由**：适配器作者可继续使用熟悉的运行时；协议稳定使 host 与 worker 解耦。
- **替代考虑**：将 worker 重写为 Rust（性能更好但工作量巨大且不属于本次目标）。

### D7. 插件 SDK：协议稳定 + host 重写

- **策略**：`@paperclipai/plugin-sdk` 的协议 schema（JSON-RPC 方法名、消息类型、能力声明）抽取为 `pc-plugin-protocol`（serde 派生 Rust 类型 + JSON schema），host 端以 `pc-plugin-host` 替换原 Node 实现。
- **理由**：保持插件作者体验不变；host 性能与可靠性大幅提升。
- **增量**：未来可选提供 Rust 版本的 plugin worker SDK。

### D8. 认证：自研 session/cookie/API key

- **选择**：`pc-auth` crate 实现 session（cookie + DB 存储）、CSRF、API key（agent_api_keys 表）、board 用户主体；不直接复刻 better-auth 内部，而是按其对外行为重建。
- **理由**：better-auth 是 Node 库，无法在 Rust 中直接复用；行为重建可控。
- **契约**：cookie 名 `paperclip.session`、CSRF token 头 `x-paperclip-csrf`、API key 头 `x-paperclip-agent-key` 与原 server 一致。

### D9. 配置与启动：12-factor + 嵌入式 PG

- **选择**：`.env` 文件 + 环境变量；本地首次启动时通过 `embedded-postgres` Rust crate（若可用）或外挂 `postgres` 二进制启动 PG。
- **降级**：若嵌入式 crate 不可用，自动回退到检测外部 `PG*` 环境变量。
- **契约**：与原 server 一致的 `PAPERCLIP_*` 环境变量集合。

### D10. 可观测性：tracing → OpenTelemetry → 控制台/OTLP

- **选择**：`tracing` + `tracing-subscriber`（JSON 输出）+ `tracing-opentelemetry`；HTTP middleware 输出 access log（与 pino-http 等价字段）；启动横幅携带版本/构建时间/数据库状态。
- **理由**：`tracing` 是 Rust 生态事实标准；OpenTelemetry exporter 可对接现有后端。

### D11. 前端复用：HTTP/WS 契约冻结

- **策略**：`paperclip/ui` 的 `API_PREFIX`、所有路由路径、查询参数、响应 schema 维持不变；`paperclip-rs` 在 dev/prod 配置中通过 `PAPERCLIP_API_BASE`（或 Vite proxy）指向 Rust 服务器。
- **理由**：60 个 UI API 客户端模块、1168 个 UI 文件无需改动。

## 数据流（核心链路：心跳 → 适配器 → live-events → UI）

```
┌─────────────────┐  HTTP POST /heartbeat   ┌──────────────────────────┐
│  UI / CLI / cron │ ───────────────────────▶│  pc-http (axum router)   │
└─────────────────┘                          └────────────┬─────────────┘
                                                          │
                                                validate (zod 等价)
                                                          │
                                                          ▼
                                                ┌─────────────────────┐
                                                │  pc-heartbeat       │
                                                │  1. pick runnable   │
                                                │  2. lock + schedule │
                                                │  3. spawn adapter   │
                                                └──────────┬──────────┘
                                                           │
                                          spawn subprocess (claude/codex/...)
                                                           │
                                                           ▼
                                                ┌─────────────────────┐
                                                │ pc-adapter-<impl>   │
                                                │ (host-side state)   │
                                                └──────────┬──────────┘
                                                           │
                                          JSON-RPC over stdio
                                                           │
                                                           ▼
                                                ┌─────────────────────┐
                                                │ adapter worker      │
                                                │ (Node/Py/Rust)      │
                                                └──────────┬──────────┘
                                                           │
                              stream events: assistant_text / tool_call /
                                             tool_result / usage / done
                                                           │
                                                           ▼
                                                ┌─────────────────────┐
                                                │ pc-realtime         │
                                                │ (broadcast bus)     │
                                                └──────────┬──────────┘
                                                           │
                                                          WS
                                                           ▼
                                                ┌─────────────────────┐
                                                │ pc-ws handler       │
                                                │  (token-validated)  │
                                                └──────────┬──────────┘
                                                           │
                                                           ▼
                                                   ┌─────────────┐
                                                   │  UI (React) │
                                                   └─────────────┘
```

并行的持久化路径：心跳每完成一个阶段都写入 `heartbeat_runs` / `heartbeat_run_events` / `activity_log` / `cost_events` 表，供 UI 的非实时视图查询。

## 关键 crate 详细设计

### `pc-core`

- 纯领域模型：`Company`、`Agent`、`AgentConfigRevision`、`Issue`、`IssueComment`、`Case`、`Project`、`ProjectMembership`、`Approval`、`Decision`、`Routine`、`RoutineRun`、`Pipeline`、`PipelineCase`、`Environment`、`ExecutionWorkspace`、`HeartbeatRun`、`HeartbeatRunEvent`、`PluginRecord`、`PluginJob` 等。
- 每个实体提供 `pub struct` + 构造器 + 不变量校验（`new()` 返回 `Result`）。
- 跨实体关系以 ID（`Uuid`）而非引用表达，避免循环依赖。

### `pc-db`

- `pub struct Db { pool: PgPool }`，封装 `sqlx::PgPool`。
- `pc_db::migrate()` 调用 `sqlx::migrate!("./migrations")`；迁移文件为 109 张表对应的 SQL DDL（由原 Drizzle schema 推导）。
- 提供 `Db::connect(url: &str) -> Result<Self>`，启动时指数退避重试。

### `pc-repos`

- 为每个聚合根提供 `Repo<T>`：`agents::Repo`、`issues::Repo`、`companies::Repo`、`heartbeat_runs::Repo` 等。
- 方法以领域动词命名：`create`、`get`、`list`、`update`、`transition`、`archive`。
- 返回 `pc_core::Entity` 强类型，不暴露 `sqlx` 行。

### `pc-auth` + `pc-authz`

- `pc-auth` 实现 session（cookie + DB）、API key、CSRF；`auth_middleware` 解析 `PaperclipActor`（`User | Agent`）。
- `pc-authz` 提供 `Policy<S, A, R>` trait；`check(actor, action, resource)` 返回 `Result<(), AuthzError>`。
- 与原 `services/authorization.ts` 的策略等价（如 issue.assign 要求 issue.assignee 或 board）。

### `pc-http`

- axum Router 组装：每条路由 `Routers::company/agent/issue/...` 与原 56 个 route 模块一一对应。
- middleware 顺序：trace → compression → cors → access log → actor 解析 → board-mutation guard → private-hostname guard → handler。
- 错误统一映射 `pc_errors::Error → (StatusCode, Json<ErrorBody>)`，与原 `middleware/error-handler.ts` 形状一致。

### `pc-ws`

- `GET /live-events` 升级为 WebSocket；token 通过 `?token=` 或首个消息验证。
- `subscribe(company_id)` / `unsubscribe` / `ping`；服务端发送 `event`、`pong`、`error` 消息。
- 与 `server/src/realtime/live-events-ws.ts` 行为一致。

### `pc-heartbeat`

- 引擎状态机：`PickRunnable → AcquireLock → ScheduleInvocation → SpawnAdapterWorker → StreamEvents → PersistRunEvent → Finalize → NotifyLiveBus`。
- 周期：`monitor_next_check_at` / `monitor_wake_requested_at` 触发；并发上限由 `agent_config.max_concurrent_runs` 控制。
- 与原 `services/heartbeat.ts` 等价；测试用 `tokio::time::pause()` 驱动确定性。

### `pc-adapter-api` + 11 个内置适配器

- `AdapterRuntime` trait：
  ```rust
  #[async_trait]
  pub trait AdapterRuntime: Send + Sync {
      fn meta(&self) -> &AdapterMeta;
      async fn list_models(&self, env: &AdapterEnv) -> Result<Vec<AdapterModel>>;
      async fn test_environment(&self, env: &AdapterEnv) -> Result<AdapterEnvTest>;
      async fn invoke(&self, ctx: AdapterInvocation) -> Result<AdapterStream>;
      // ...
  }
  ```
- 11 个 `pc-adapter-*` crate 实现该 trait，每个等价于 `packages/adapters/<name>/src/server/index.ts`。
- worker 子进程仍由 host 启动，JSON-RPC 协议保持不变。

### `pc-plugin-host` + `pc-plugin-protocol`

- `pc-plugin-protocol`：`serde` 派生所有 RPC 消息类型；导出 `#[derive(Message)]` 等宏。
- `pc-plugin-host`：`WorkerPool` 管理 stdio 子进程；`EventBus` 多路广播 host 事件；`JobScheduler` 按 cron/手动触发；`ToolDispatcher` 暴露插件工具给 agent runtime；`DatabaseBridge` 提供 `pc-db` 受限视图。

### `pc-storage` + `pc-secrets`

- `StorageProvider` trait：`put/get/delete/signed_url/list`；`local_disk` 与 `s3` 两个实现，配置驱动选择。
- `SecretsProvider` trait：`get/put/delete/list`；`local_encrypted`（AES-GCM，密钥来自 `PAPERCLIP_MASTER_KEY`）与 `aws_sm` 两个实现。

### `pc-cli`

- `clap` v4 derive 子命令：`run`、`install`、`onboard`、`doctor`、`worktree`、`heartbeat-run`、`pipelines`、`routines`、`service`、`update`、`db backup`、`configure`。
- 与 `cli/src/commands/*` 一一对应；输出格式（人类可读 + `--json`）保留。

### `pc-server`（二进制）

- `tokio::main` 启动序列：
  1. 解析 config（`pc-config`）。
  2. 初始化 tracing（`pc-telemetry`）。
  3. 启动嵌入式 PG 或连接外部 PG（`pc-db`）。
  4. 执行迁移（`pc-db::migrate`）。
  5. 构建 Router + WS（`pc-http`、`pc-ws`）。
  6. 启动插件 Worker 池（`pc-plugin-host`）。
  7. 启动心跳引擎（`pc-heartbeat`）。
  8. 启动实时广播（`pc-realtime`）。
  9. 绑定端口（默认 3100，与原 server 一致），等待 SIGTERM/SIGINT，graceful shutdown（停止心跳 → 排空请求 → 关闭 DB → 退出）。

## 数据库 Schema 映射

109 张表按聚合根分到 `pc-repos` 的子模块：

| 聚合 | 表（部分） | Rust 模块 |
|---|---|---|
| Company | companies, company_memberships, company_logos, company_skill_policies, company_secrets, company_secret_versions, company_secret_bindings, company_secret_provider_configs | `pc_repos::company` |
| Agent | agents, agent_memberships, agent_config_revisions, agent_api_keys, agent_runtime_state, agent_task_sessions, agent_wakeup_requests | `pc_repos::agent` |
| Issue | issues, issue_comments, issue_labels, issue_attachments, issue_documents, issue_work_products, issue_plan_decompositions, issue_relations, issue_read_states, issue_recovery_actions, issue_watchdogs, issue_approvals, issue_thread_interactions, issue_reference_mentions, issue_create_idempotency_keys, issue_execution_decisions, issue_inbox_archives, issue_tree_holds, issue_tree_hold_members | `pc_repos::issue` |
| Case | cases, pipeline_cases, pipeline_case_events | `pc_repos::case` |
| Project | projects, project_memberships, project_workspaces, project_goals | `pc_repos::project` |
| Approval | approvals, approval_comments | `pc_repos::approval` |
| Decision | decisions, decision_training_examples | `pc_repos::decision` |
| Routine | routines, routine_documents | `pc_repos::routine` |
| Pipeline | pipelines | `pc_repos::pipeline` |
| Environment | environments, environment_leases, environment_custom_image_templates, environment_custom_image_setup_sessions | `pc_repos::environment` |
| Execution | execution_workspaces, workspace_operations, workspace_runtime_services | `pc_repos::execution` |
| Heartbeat | heartbeat_runs, heartbeat_run_events, heartbeat_run_watchdog_decisions | `pc_repos::heartbeat` |
| Plugin | plugins, plugin_database, plugin_entities, plugin_jobs, plugin_logs, plugin_managed_resources, plugin_state, plugin_webhooks, plugin_company_settings, plugin_config, built_in_managed_resources | `pc_repos::plugin` |
| Auth | auth, instance_user_roles, invites, join_requests, board_api_keys, cli_auth_challenges, principal_permission_grants | `pc_repos::auth` |
| Activity | activity_log, cost_events, budget_incidents, budget_policies, finance_events, secret_access_events, feedback_votes, feedback_exports | `pc_repos::activity` |
| Document | documents, document_memberships, document_revisions, document_annotation_threads, document_annotation_comments, document_annotation_anchor_snapshots | `pc_repos::document` |
| Goal | goals | `pc_repos::goal` |
| Folder | folders | `pc_repos::folder` |
| Sidebar | user_sidebar_preferences, company_user_sidebar_preferences | `pc_repos::sidebar` |
| Inbox | user_inbox_agent_policies, inbox_dismissals | `pc_repos::inbox` |
| Summary | summary_slots, status_cards | `pc_repos::summary` |
| Tool | tool_access, tool_mcp_gateways, tool_mcp_gateway_tokens, tool_connections, tool_profiles | `pc_repos::tool` |
| Smoke | smoke_lab | `pc_repos::smoke` |
| Settings | instance_settings | `pc_repos::settings` |
| Skill | company_skills, skills-catalog 包关联 | `pc_repos::skill` |

## 风险 / Trade-offs

- **[R1] 行为偏差风险**（API/WS 端点行为在 Rust 重写后与原 Node server 不一致）→ 缓解：以原 OpenAPI 文档 + 现有 Vitest 集成测试为契约；Rust 端每个路由对应一个集成测试，逐路由对齐。
- **[R2] 数据库 schema 漂移** → 缓解：迁移文件以 SQL DDL 形式直接来自原 Drizzle schema 推导；CI 跑 `pc-migrate up` 在 fresh DB 上验证可达终态。
- **[R3] 嵌入式 PostgreSQL 在 Rust 端的二进制可用性** → 缓解：保留外部 PG 优先；嵌入式失败时回退到外部 PG 并发出警告。
- **[R4] async/await 取消语义差异**（原 server 多处依赖 Node 的事件循环与 setTimeout/timer；Rust 端用 tokio 的 select/cancel 替代）→ 缓解：每个服务方法增加 `tokio::time::timeout` 与 `CancellationToken` 显式传播；以集成测试验证取消路径。
- **[R5] 第三方 SDK 不可用**（如 `better-auth` 的内部实现细节无法复刻）→ 缓解：仅复刻对外行为；UI 通过 `pc-auth` 的 cookie/CSRF/API key 头部契约验证。
- **[R6] 适配器 worker 兼容**（如果 Rust host 启动 worker 的协议与 Node host 不一致）→ 缓解：`pc-plugin-protocol` 与 `pc-adapter-api` 的 IPC schema 单元测试与原 Node host 互操作测试。
- **[R7] 性能回归**（个别路径在 Rust 实现下可能不如 V8 JIT）→ 缓解：建立基准（`criterion`）+ 持续 profile；对热点路径保留 `spawn_blocking`。
- **[R8] 单二进制依赖 glibc** → 缓解：CI 同时构建 musl 静态链接版本与 glibc 动态版本；Docker 镜像采用 distroless + musl 构建。
- **[R9] 重写周期长**（760 文件 / 44 万行非平凡映射） → 缓解：增量迁移，按 `pc-core → pc-db → pc-repos → pc-auth/authz → pc-http → 路由逐个模块 → pc-heartbeat → pc-ws → pc-plugin-host → pc-adapter-* → pc-cli` 顺序，每个模块独立可发布。
- **[R10] OpenTelemetry 在 Rust 端的开销** → 缓解：默认关闭 OTLP exporter，仅 console JSON；按需启用。

## 迁移计划（增量）

### Phase A：骨架（Week 1-2）

1. 建立 `paperclip-rs/` Cargo workspace + rust-toolchain + CI。
2. `pc-errors`、`pc-telemetry`、`pc-config`、`pc-core`（最小实体）。
3. `pc-db` + 109 张表 SQL 迁移（从原 Drizzle 推导）。
4. `pc-server` 启动并 `GET /health` 返回 200，与原 server 同端口（3100）。

### Phase B：仓储 + 认证（Week 3-4）

5. `pc-repos` 全量实现（公司/代理/议题核心表）。
6. `pc-auth` + `pc-authz` 实现 session/cookie/API key + 策略。
7. `pc-http` 装配健康/认证/公司/代理/议题核心路由。
8. 双栈切换：UI 通过 `VITE_API_BASE` 切换到 Rust 服务器；与原 Node server 并行运行做对比。

### Phase C：路由全覆盖（Week 5-8）

9. 56 个路由模块逐个迁移：cases/approvals/decisions/routines/pipelines/environments/execution-workspaces/goals/board-chat/secrets/tool-access/costs/activity/dashboard/attention/user-profiles/sidebar/inbox/instance-settings/plugin-ui-static/...。
10. 集成测试每个路由对齐原 OpenAPI。

### Phase D：实时 + 心跳（Week 9-10）

11. `pc-ws` live-events WebSocket；迁移 `live-events-ws.ts` 行为。
12. `pc-heartbeat` 心跳引擎；适配器子进程编排；`agent_runtime_state` 状态机。

### Phase E：适配器与插件（Week 11-14）

13. `pc-adapter-api` trait + 11 个内置适配器 host。
14. `pc-plugin-protocol` 共享 schema；`pc-plugin-host` Worker 池/事件总线/作业调度/工具分发。

### Phase F：CLI + 可观测性 + 打磨（Week 15-16）

15. `pc-cli` 全部子命令；`pc-migrate` 独立迁移工具。
16. OpenTelemetry OTLP exporter、pino 等价日志、启动横幅、备份链路。

### Phase G：切流量（Week 17）

17. 灰度：将 UI 默认 base 切换到 Rust 服务器；保留 Node server 作为只读回滚。
18. 全量：移除 Node server 运行依赖；保留 `paperclip/server` 目录为归档快照。

### 回滚策略

- 任何阶段发现阻塞问题，UI 的 `VITE_API_BASE` 切回 Node server 即可，业务数据零迁移。
- 109 张表 schema 不变，无需数据回灌。

## Open Questions

1. **musl vs glibc 默认构建**：macOS 开发体验优先 glibc 动态；Linux 生产优先 musl 静态。是否需要在两个目标平台各产一份发布产物？
2. **嵌入式 PostgreSQL crate 选择**：`pg-embedded`（维护活跃但功能较窄）vs 自管 `postgres` 二进制（控制力强但需打包）。倾向前者 + 外部 PG 降级。
3. **OpenTelemetry 导出器是否默认启用**：倾向默认关闭（仅 console JSON），需要时通过 `OTEL_EXPORTER_OTLP_ENDPOINT` 启用。
4. **多副本部署与 Redis pubsub**：是否在本期范围内？倾向不在，保留 trait 抽象未来注入。
5. **plugin worker Rust SDK**：是否同步产出 Rust 版 `definePlugin`？倾向不在本期；提供 Rust host 即可，worker 仍以 Node/Python 写。
6. **`aide` vs `utoipa` OpenAPI 生成**：前者类型更现代，后者生态更成熟。倾向 `utoipa` 以减小风险。
