# paperclip-rs-rewrite — 任务清单

> 任务编号约定：`<Phase>.<Group>.<Item>`。每个任务可在一个 session 内完成并验证。
> 验收依据：每个 task 末尾括号内给出"通过条件"。

---

## 1. Phase A — 工作区骨架与基础设施

### 1.1 Cargo workspace 与工具链

- [ ] 1.1.1 创建 `paperclip-rs/` 目录与 `Cargo.toml` workspace（成员路径 `crates/*`、`apps/*`）
- [ ] 1.1.2 添加 `rust-toolchain.toml`（stable 1.8x + rustfmt + clippy + rust-analyzer）
- [ ] 1.1.3 配置 `.cargo/config.toml`（target 目录、build cache、formatter 风格）
- [ ] 1.1.4 添加根 `README.md`、`LICENSE`（MIT，沿用上游）、`SECURITY.md`
- [ ] 1.1.5 添加 GitHub Actions：`ci.yml`（fmt + clippy + test + musl build + glibc build）
- [ ] 1.1.6 添加 `cargo-deny`、`cargo-audit`、`cargo-machete`（依赖治理）
- [ ] 1.1.7 配置 `Cargo.lock` 提交到仓库
- [ ] 1.1.8 添加 `mise.toml` 或 `rust-toolchain.toml` 文档说明本地开发

### 1.2 基础 crate

- [ ] 1.2.1 创建 `crates/pc-errors/`：`Error` 枚举 + HTTP 状态码映射 + JSON 错误体
- [ ] 1.2.2 创建 `crates/pc-telemetry/`：tracing 初始化 + JSON subscriber + OpenTelemetry layer（默认关闭）
- [ ] 1.2.3 创建 `crates/pc-config/`：env 加载 + `.env` 解析 + `Config` 结构 + 校验
- [ ] 1.2.4 创建 `crates/pc-core/`：`Company`/`Agent`/`Issue` 等最小领域类型 + 不变量校验（new 返回 Result）
- [ ] 1.2.5 在 `pc-core` 添加 `Id<T>` newtype、`Timestamp`、`Money` 等基础类型
- [ ] 1.2.6 为 `pc-errors/pc-telemetry/pc-config/pc-core` 写单元测试（覆盖率 ≥ 80%）

### 1.3 数据库 schema 与迁移

- [ ] 1.3.1 从 `paperclip/packages/db/src/schema/*.ts` 推导 109 张表的等价 SQL DDL
- [ ] 1.3.2 在 `crates/pc-db/migrations/` 创建 `0001_init.sql` 起始迁移（含全部 109 张表）
- [ ] 1.3.3 创建 `crates/pc-db/src/migrator.rs`：调用 `sqlx::migrate!()` + 版本记录
- [ ] 1.3.4 实现嵌入式 PG 启动（`pg-embedded`）+ 外部 PG 降级
- [ ] 1.3.5 在 fresh DB 上跑迁移至最新版本，dump schema 对比原 Drizzle 输出一致（`通过条件：pg_dump diff = ∅`）
- [ ] 1.3.6 实现 `pc-db::seed` 注入与原 `db/src/seed.ts` 等价的开发种子数据

### 1.4 pc-server 二进制骨架

- [ ] 1.4.1 创建 `crates/pc-server/`：`#[tokio::main]` 启动序列（config → telemetry → db → migrate → router → bind）
- [ ] 1.4.2 实现 `GET /health` 返回 200 + 版本 + DB ping 状态
- [ ] 1.4.3 实现 graceful shutdown：`tokio::signal::ctrl_c` + 超时排空
- [ ] 1.4.4 默认端口 3100（与原 server 一致）；通过 `PAPERCLIP_PORT` 覆盖
- [ ] 1.4.5 在 macOS 本地 `cargo run -p pc-server` 启动并 curl `/health` 返回 200

---

## 2. Phase B — 仓储层与认证授权

### 2.1 pc-repos 核心聚合

- [ ] 2.1.1 `pc_repos::company`：companies / company_memberships / company_logos
- [ ] 2.1.2 `pc_repos::agent`：agents / agent_memberships / agent_config_revisions / agent_api_keys / agent_runtime_state / agent_task_sessions / agent_wakeup_requests
- [ ] 2.1.3 `pc_repos::issue`：issues / issue_comments / issue_labels / issue_attachments / issue_documents / issue_work_products / issue_plan_decompositions / issue_relations / issue_read_states / issue_recovery_actions / issue_watchdogs / issue_approvals / issue_thread_interactions / issue_reference_mentions / issue_create_idempotency_keys / issue_execution_decisions / issue_inbox_archives / issue_tree_holds / issue_tree_hold_members
- [ ] 2.1.4 `pc_repos::case`：cases / pipeline_cases / pipeline_case_events
- [ ] 2.1.5 `pc_repos::project`：projects / project_memberships / project_workspaces / project_goals
- [ ] 2.1.6 `pc_repos::approval`：approvals / approval_comments
- [ ] 2.1.7 `pc_repos::decision`：decisions / decision_training_examples
- [ ] 2.1.8 `pc_repos::routine`：routines / routine_documents / routine_revisions
- [ ] 2.1.9 `pc_repos::pipeline`：pipelines
- [ ] 2.1.10 `pc_repos::environment`：environments / environment_leases / environment_custom_image_*（templates + setup_sessions）
- [ ] 2.1.11 `pc_repos::execution`：execution_workspaces / workspace_operations / workspace_runtime_services
- [ ] 2.1.12 `pc_repos::heartbeat`：heartbeat_runs / heartbeat_run_events / heartbeat_run_watchdog_decisions
- [ ] 2.1.13 `pc_repos::plugin`：plugins / plugin_database / plugin_entities / plugin_jobs / plugin_logs / plugin_managed_resources / plugin_state / plugin_webhooks / plugin_company_settings / plugin_config / built_in_managed_resources
- [ ] 2.1.14 `pc_repos::auth`：auth / instance_user_roles / invites / join_requests / board_api_keys / cli_auth_challenges / principal_permission_grants
- [ ] 2.1.15 `pc_repos::activity`：activity_log / cost_events / budget_incidents / budget_policies / finance_events / secret_access_events / feedback_votes / feedback_exports
- [ ] 2.1.16 `pc_repos::document`：documents / document_memberships / document_revisions / document_annotation_threads / document_annotation_comments / document_annotation_anchor_snapshots
- [ ] 2.1.17 `pc_repos::goal`：goals
- [ ] 2.1.18 `pc_repos::folder`：folders
- [ ] 2.1.19 `pc_repos::sidebar`：user_sidebar_preferences / company_user_sidebar_preferences
- [ ] 2.1.20 `pc_repos::inbox`：user_inbox_agent_policies / inbox_dismissals
- [ ] 2.1.21 `pc_repos::summary`：summary_slots / status_cards
- [ ] 2.1.22 `pc_repos::tool`：tool_access / tool_mcp_gateways / tool_mcp_gateway_tokens / tool_connections / tool_profiles
- [ ] 2.1.23 `pc_repos::smoke`：smoke_lab
- [ ] 2.1.24 `pc_repos::settings`：instance_settings
- [ ] 2.1.25 `pc_repos::skill`：company_skills（关联 skills-catalog 包）
- [ ] 2.1.26 为每个 repo 模块写单元测试（in-memory PG + sqlx-test）

### 2.2 pc-auth

- [ ] 2.2.1 实现 session 存储（cookie + DB 行）
- [ ] 2.2.2 实现 CSRF token 生成与校验（`x-paperclip-csrf` 头）
- [ ] 2.2.3 实现 API key 校验（`x-paperclip-agent-key` 头，hash 比对）
- [ ] 2.2.4 实现 board 用户/agent 双主体模型 `Actor`
- [ ] 2.2.5 移植 better-auth 的对外行为（路由形状、cookie 名、过期时间）
- [ ] 2.2.6 与原 `routes/auth.ts` 行为对等：注册/登录/登出/会话续期
- [ ] 2.2.7 单元测试：session 生命周期、CSRF 缺失/错误、API key 错误

### 2.3 pc-authz

- [ ] 2.3.1 设计 `Policy<S, A, R>` trait + `check()` 方法
- [ ] 2.3.2 移植 `services/authorization.ts` 中的策略表（公司/代理/议题/案例/审批等）
- [ ] 2.3.3 实现 board-mutation guard（与原 `middleware/board-mutation-guard.ts` 等价）
- [ ] 2.3.4 单元测试：每个策略的 allow/deny 用例

### 2.4 pc-storage + pc-secrets

- [ ] 2.4.1 `StorageProvider` trait：`put/get/delete/signed_url/list`
- [ ] 2.4.2 实现 `local_disk` provider（基于 `tokio::fs`，路径与 `storage/local-disk-provider.ts` 等价）
- [ ] 2.4.3 实现 `s3` provider（基于 `aws-sdk-s3`，等价于 `storage/s3-provider.ts`）
- [ ] 2.4.3a 实现 `StorageService` 聚合（等价于 `storage/service.ts`、`storage/index.ts`、`storage/provider-registry.ts`、`storage/types.ts`）
- [ ] 2.4.4 `SecretsProvider` trait：`get/put/delete/list`
- [ ] 2.4.5 实现 `local_encrypted`（AES-GCM，主密钥来自 `PAPERCLIP_MASTER_KEY`，等价于 `secrets/local-encrypted-provider.ts`）
- [ ] 2.4.6 实现 `aws_sm` provider（基于 `aws-sdk-secretsmanager`，等价于 `secrets/aws-secrets-manager-provider.ts`）
- [ ] 2.4.6a 实现 `configured_provider`（等价于 `secrets/configured-provider.ts`、`secrets/external-stub-providers.ts`、`secrets/provider-registry.ts`、`secrets/types.ts`）
- [ ] 2.4.7 单元测试：每个 provider round-trip

---

## 3. Phase C — HTTP 路由全覆盖（56 模块）

### 3.1 pc-http 基础

- [ ] 3.1.1 创建 `crates/pc-http/`：axum Router 组装 + middleware stack
- [ ] 3.1.2 middleware 顺序：trace → compression → cors → body-limits → access log → actor → board-mutation guard → private-hostname guard → trust-proxy → redact-sensitive → handler
- [ ] 3.1.2a `pc-http::middleware::body_limits`：等价于 `http/body-limits.ts`
- [ ] 3.1.2b `pc-http::middleware::trust_proxy`：等价于 `middleware/trust-proxy.ts`
- [ ] 3.1.2c `pc-http::middleware::http_log_policy`：等价于 `middleware/http-log-policy.ts`
- [ ] 3.1.2d `pc-http::middleware::redact_sensitive`：等价于 `middleware/redact-sensitive.ts`
- [ ] 3.1.2e `pc-core::lib::join_request_dedupe`：等价于 `lib/join-request-dedupe.ts`
- [ ] 3.1.2f `pc-core::lib::objects`：等价于 `lib/objects.ts`
- [ ] 3.1.3 错误统一映射 `pc_errors::Error → (StatusCode, Json<ErrorBody>)`
- [ ] 3.1.4 实现请求体验证层（zod 等价）：引入 `validator` crate 或自研 derive 宏
- [ ] 3.1.5 移植 `routes/index.ts` 路由注册表到 Rust

### 3.2 核心路由（前 10 个，按业务重要性）

- [ ] 3.2.1 `routes::companies`：CRUD + 成员 + logo
- [ ] 3.2.1a `routes::company_import_paths`：公司导入路径清单（与 `routes/company-import-paths.ts` 等价）
- [ ] 3.2.2 `routes::agents`：CRUD + 配置修订 + hire + permissions + instructions
- [ ] 3.2.3 `routes::issues`：CRUD + 评论 + checkout + 文档 + watchdog + tree-control
- [ ] 3.2.4 `routes::projects`：CRUD + 成员 + 工作区
- [ ] 3.2.5 `routes::cases`：CRUD + 详情
- [ ] 3.2.6 `routes::approvals`：CRUD + 评论 + 链接
- [ ] 3.2.7 `routes::decisions`：CRUD + training
- [ ] 3.2.8 `routes::routines`：CRUD + 文档
- [ ] 3.2.9 `routes::pipelines`：CRUD + 案例 + 事件
- [ ] 3.2.10 `routes::environments`：CRUD + 自定义镜像 + lease

### 3.3 工作流与执行

- [ ] 3.3.1 `routes::execution_workspaces`：CRUD + 运行时服务
- [ ] 3.3.2 `routes::goals`：CRUD
- [ ] 3.3.3 `routes::board_chat`：聊天 + 系统提示
- [ ] 3.3.4 `routes::file_resources`：上传/下载/列表
- [ ] 3.3.5 `routes::company_skill_policy` / `company_skills`：策略与技能 CRUD

### 3.4 协作与可见性

- [ ] 3.4.1 `routes::board_chat`：董事会聊天（与原 server 一致）
- [ ] 3.4.2 `routes::user_profiles`：个人资料
- [ ] 3.4.3 `routes::resource_memberships`：资源成员
- [ ] 3.4.4 `routes::sidebar_badges` / `sidebar_preferences`：侧边栏
- [ ] 3.4.5 `routes::inbox_dismissals` / `inbox_agent_policy`：收件箱策略
- [ ] 3.4.6 `routes::invites` / `join_requests`：加入与请求

### 3.5 集成与外部

- [ ] 3.5.1 `routes::secrets`：密钥 CRUD
- [ ] 3.5.2 `routes::tool_access` / `tool_gateway`：工具访问与网关
- [ ] 3.5.3 `routes::costs` / `activity` / `dashboard`：成本/活动/仪表盘
- [ ] 3.5.4 `routes::attention`：关注项
- [ ] 3.5.5 `routes::auth` / `authz`：认证授权端点
- [ ] 3.5.6 `routes::access`：访问控制

### 3.6 平台与运维

- [ ] 3.6.1 `routes::instance_settings`：实例设置
- [ ] 3.6.2 `routes::instance_database_backups`：备份链路
- [ ] 3.6.3 `routes::health`：健康检查
- [ ] 3.6.4 `routes::openapi`：OpenAPI 文档端点
- [ ] 3.6.5 `routes::llms`：LLM 元数据
- [ ] 3.6.6 `routes::org_chart_svg`：组织架构图
- [ ] 3.6.7 `routes::plugin_ui_static`：插件 UI 静态资源

### 3.7 长尾与实验

- [ ] 3.7.1 `routes::adapters`：适配器注册表与配置
- [ ] 3.7.2 `routes::built_in_agents`：内置 agent
- [ ] 3.7.3 `routes::plugins`：插件管理
- [ ] 3.7.4 `routes::assets`：资产
- [ ] 3.7.5 `routes::decision_training`：决策训练
- [ ] 3.7.6 `routes::document_annotations`：文档批注（与 pc-doc-anchors 协作）
- [ ] 3.7.7 `routes::environment_selection`：环境选择
- [ ] 3.7.8 `routes::folders`：文件夹
- [ ] 3.7.8 `routes::inbox_agent_policy`：收件箱策略
- [ ] 3.7.9 `routes::issue_tree_control`：议题树控制
- [ ] 3.7.10 `routes::issues_checkout_wakeup`：议题检出入唤醒
- [ ] 3.7.11 `routes::llms`：LLM 配置
- [ ] 3.7.12 `routes::org_chart_svg`：组织图
- [ ] 3.7.13 `routes::pipelines`：管道
- [ ] 3.7.14 `routes::plugin_ui_static`：插件 UI 静态
- [ ] 3.7.15 `routes::projects`：项目
- [ ] 3.7.16 `routes::resource_memberships`：资源成员
- [ ] 3.7.17 `routes::routines`：例程
- [ ] 3.7.18 `routes::secrets`：密钥
- [ ] 3.7.19 `routes::sidebar_badges` / `sidebar_preferences`：侧边栏
- [ ] 3.7.20 `routes::smoke_lab`：冒烟测试
- [ ] 3.7.21 `routes::status_cards`：状态卡片
- [ ] 3.7.22 `routes::summary_slots`：摘要槽
- [ ] 3.7.23 `routes::teams_catalog`：团队目录
- [ ] 3.7.24 `routes::tool_access`：工具访问
- [ ] 3.7.25 `routes::tool_gateway`：工具网关
- [ ] 3.7.26 `routes::user_profiles`：用户档案
- [ ] 3.7.27 `routes::workspace_command_authz`：工作空间命令授权
- [ ] 3.7.28 `routes::workspace_runtime_service_authz`：工作空间运行时服务授权

### 3.8 pc-openapi

- [ ] 3.8.1 选型 `utoipa` + axum 集成
- [ ] 3.8.2 从路由 derive 自动生成 OpenAPI 3.1
- [ ] 3.8.3 与原 `routes/openapi.ts` 输出字段对齐
- [ ] 3.8.4 在 `/openapi.json` 与 `/openapi.yaml` 同时暴露

---

## 4. Phase D — 实时通信与心跳引擎

### 4.1 pc-realtime（进程内总线）

- [ ] 4.1.1 `RealtimeBus` trait + `InMemoryBus` 实现（基于 `tokio::sync::broadcast`）
- [ ] 4.1.2 事件类型 `LiveEvent`：issue.created / issue.updated / heartbeat.* / agent.* 等
- [ ] 4.1.3 订阅/退订按 `company_id` 过滤
- [ ] 4.1.4 单元测试：广播与订阅的对称性

### 4.2 pc-ws

- [ ] 4.2.1 WebSocket 升级 `GET /live-events`
- [ ] 4.2.2 token 校验（query string + 首包）
- [ ] 4.2.3 消息协议：`subscribe` / `unsubscribe` / `ping`；服务端 `event` / `pong` / `error`
- [ ] 4.2.4 ping/pong 心跳（30s 超时）
- [ ] 4.2.5 断线重连不丢事件（最近 N 条缓冲）
- [ ] 4.2.6 与原 `server/src/realtime/live-events-ws.ts` 行为对齐的集成测试
- [ ] 4.2.7 自定义镜像终端 WebSocket（与 `realtime/environment-custom-image-terminal-ws.ts` 等价）

### 4.3 pc-heartbeat

- [ ] 4.3.1 心跳调度循环：基于 `monitor_next_check_at` 触发
- [ ] 4.3.2 状态机：`PickRunnable → AcquireLock → ScheduleInvocation → SpawnAdapterWorker → StreamEvents → PersistRunEvent → Finalize → NotifyLiveBus`
- [ ] 4.3.3 并发上限 `agent.max_concurrent_runs`
- [ ] 4.3.4 monitor / watchdog / recovery actions 完整覆盖
- [ ] 4.3.5 单元测试：状态机迁移（用 `tokio::time::pause`）
- [ ] 4.3.6 集成测试：从心跳启动到 live-events 推送的端到端

### 4.4 pc-workflow

- [ ] 4.4.1 `pc-workflow::routines`：定义 + 修订 + 运行
- [ ] 4.4.2 `pc-workflow::pipelines`：定义 + 案例 + 事件流
- [ ] 4.4.3 cron 触发器（基于 `tokio-cron-scheduler`）
- [ ] 4.4.4 与原 `services/routines.ts` / `services/pipelines.ts` 行为对齐

---

## 5. Phase E — 适配器与插件系统

### 5.1 pc-adapter-api

- [ ] 5.1.1 `AdapterRuntime` trait：`meta / list_models / test_environment / invoke`
- [ ] 5.1.2 `AdapterStream`：assistant_text / tool_call / tool_result / usage / done
- [ ] 5.1.3 配置 schema（与 `config-schema.ts` 等价）
- [ ] 5.1.4 模型 profile 与 quota 检查

### 5.2 11 个内置适配器

- [ ] 5.2.1 `pc-adapter-claude-local`：等价于 `packages/adapters/claude-local/src/server/`
- [ ] 5.2.2 `pc-adapter-codex-local`：等价于 `packages/adapters/codex-local/src/server/`
- [ ] 5.2.3 `pc-adapter-cursor-cloud`：等价于 `packages/adapters/cursor-cloud/src/server/`
- [ ] 5.2.4 `pc-adapter-cursor-local`：等价于 `packages/adapters/cursor-local/src/server/`
- [ ] 5.2.5 `pc-adapter-gemini-local`：等价于 `packages/adapters/gemini-local/src/server/`
- [ ] 5.2.6 `pc-adapter-grok-local`：等价于 `packages/adapters/grok-local/src/server/`
- [ ] 5.2.7 `pc-adapter-hermes-gateway`：等价于 `packages/adapters/hermes-gateway/src/server/`
- [ ] 5.2.8 `pc-adapter-openclaw-gateway`：等价于 `packages/adapters/openclaw-gateway/src/server/`
- [ ] 5.2.9 `pc-adapter-opencode-local`：等价于 `packages/adapters/opencode-local/src/server/`
- [ ] 5.2.10 `pc-adapter-pi-local`：等价于 `packages/adapters/pi-local/src/server/`
- [ ] 5.2.11 集成测试：每个 adapter 与原 Node adapter 在同一 fixture 下输出等价

### 5.3 pc-plugin-protocol

- [ ] 5.3.1 serde 派生所有 RPC 消息类型（与 `@paperclipai/plugin-sdk/src/protocol.ts` 对齐）
- [ ] 5.3.2 JSON schema 导出（供 TypeScript/Python SDK 参考）
- [ ] 5.3.3 能力声明（`PaperclipPluginManifestV1` 等价）

### 5.4 pc-plugin-host

- [ ] 5.4.1 `WorkerPool`：子进程池（基于 `tokio::process::Command`）+ 健康探针
- [ ] 5.4.2 `EventBus`：host → worker 事件分发
- [ ] 5.4.3 `JobScheduler`：cron + 手动触发
- [ ] 5.4.4 `JobStore`：作业持久化（plugin_jobs 表）
- [ ] 5.4.5 `ToolDispatcher`：工具注册与调用
- [ ] 5.4.6 `DatabaseBridge`：受限 DB 视图（plugin_database 表）
- [ ] 5.4.7 `StateStore`：状态读写（plugin_state 表）
- [ ] 5.4.8 `WebhookDispatcher`：webhook 发送（plugin_webhooks 表）
- [ ] 5.4.9 `ManifestValidator` + `CapabilityValidator`：与原 `services/plugin-manifest-validator.ts` / `plugin-capability-validator.ts` 等价
- [ ] 5.4.10 集成测试：从插件加载 → 注册事件 → 触发作业的端到端

---

## 6. Phase F — CLI、可观测性与打磨

### 6.1 pc-cli

- [ ] 6.1.1 `clap` v4 derive 子命令骨架
- [ ] 6.1.2 `run`：执行一次 run（等价于 `cli run`）
- [ ] 6.1.3 `install`：安装实例（等价于 `cli install`）
- [ ] 6.1.4 `onboard`：首次登录（等价于 `cli onboard`）
- [ ] 6.1.5 `doctor`：诊断（等价于 `cli doctor`）
- [ ] 6.1.6 `worktree`：工作树管理（`worktree`/`merge-history`）
- [ ] 6.1.7 `heartbeat-run`：心跳运行
- [ ] 6.1.8 `pipelines`：管道
- [ ] 6.1.9 `routines`：例程
- [ ] 6.1.10 `service`：服务模式（launchd/systemd）
- [ ] 6.1.11 `update`：自更新
- [ ] 6.1.12 `configure`：配置
- [ ] 6.1.13 `db backup` / `db-backup`：备份（与 `cli/src/commands/db-backup.ts` 等价）
- [ ] 6.1.14 `auth-bootstrap-ceo`：CEO 引导
- [ ] 6.1.15 `allowed-hostname`：允许主机名
- [ ] 6.1.16 `env` / `env-lab`：环境变量工具
- [ ] 6.1.17 `uninstall`：卸载
- [ ] 6.1.18 所有命令支持 `--json` 输出
- [ ] 6.1.19 单元 + 集成测试：每条命令的 happy path

### 6.2 pc-migrate（独立迁移工具）

- [ ] 6.2.1 `paperclip-migrate up/down/status/create`
- [ ] 6.2.2 与 `pc-server` 启动时的自动迁移对齐

### 6.3 pc-telemetry 完善

- [ ] 6.3.1 OpenTelemetry OTLP exporter（通过 `OTEL_EXPORTER_OTLP_ENDPOINT` 启用）
- [ ] 6.3.2 启动横幅：版本 / 构建时间 / 数据库 / 端口
- [ ] 6.3.3 access log：与 pino-http 字段一致（method / path / status / duration / size / req_id）
- [ ] 6.3.4 log 重写：与原 `middleware/http-log-redaction.ts` 等价

### 6.4 pc-backup

- [ ] 6.4.1 数据库备份链路（与 `services/database-backup-health.ts` 等价）
- [ ] 6.4.2 备份健康检查与告警

### 6.5 pc-feature-flags

- [ ] 6.5.1 `pc-feature-flags`：feature catalog（与 `shared/feature-catalog.ts` 等价）
- [ ] 6.5.2 暴露给前端：`GET /feature-flags`

### 6.6 pc-doc-anchors

- [ ] 6.6.1 文档锚点 + 批注模型（与 `shared/document-anchors.ts` 等价）
- [ ] 6.6.2 与 `routes/document-annotations` 集成

### 6.7 端到端验证

- [ ] 6.7.1 用原 server 与 Rust server 跑同一组 curl/集成测试，对比响应字节级一致
- [ ] 6.7.2 用原 UI（`VITE_API_BASE` 指向 Rust server）完整冒烟测试
- [ ] 6.7.3 启动 → 创建公司 → 创建 agent → 触发心跳 → live-events 推送的端到端剧本
- [ ] 6.7.4 性能基准：`wrk` 对核心路径压测，对比 Node server（目标：延迟 -30%、内存 -40%）

---

## 7. Phase G — 切流量与归档

### 7.1 灰度

- [ ] 7.1.1 UI 默认 `VITE_API_BASE` 切换到 Rust server
- [ ] 7.1.2 保留 Node server 作为只读回滚（监听 3101）
- [ ] 7.1.3 监控错误率、延迟、内存 7 天

### 7.2 全量与归档

- [ ] 7.2.1 移除 `paperclip/server/` 运行依赖（保留为归档快照）
- [ ] 7.2.2 移除 `paperclip/cli/`（保留为归档）
- [ ] 7.2.3 更新根 README 与 AGENTS.md，指向 `paperclip-rs/`
- [ ] 7.2.4 调整 CI：Rust 构建为默认；Node 构建仅保留 UI 与适配器 UI bundle
- [ ] 7.2.5 发布 `paperclip-server:1.0.0` 与 `paperclipai:1.0.0` 容器镜像

### 7.3 文档与移交

- [ ] 7.3.1 编写 `paperclip-rs/ARCHITECTURE.md`（基于 design.md）
- [ ] 7.3.2 编写 `paperclip-rs/OPERATIONS.md`（部署、备份、监控）
- [ ] 7.3.3 编写 `paperclip-rs/PLUGIN_AUTHORING.md`（插件作者指南）
- [ ] 7.3.4 编写 `paperclip-rs/MIGRATION_FROM_NODE.md`（从 Node server 迁移指南）
- [ ] 7.3.5 录制开发者导览视频（可选）

---

## 8. 持续质量保障

### 8.1 测试

- [ ] 8.1.1 单元测试覆盖率 ≥ 80%（`cargo-llvm-cov`）
- [ ] 8.1.2 集成测试：每个 HTTP 路由一个 happy + 3 个 edge case
- [ ] 8.1.3 端到端测试：playwright 复用 paperclip 原 e2e 套件
- [ ] 8.1.4 性能基准：criterion 基准与 wrk 压测脚本
- [ ] 8.1.5 模糊测试：HTTP handler + DB 查询（`cargo-fuzz`）

### 8.2 安全

- [ ] 8.2.1 `cargo-audit` 在 CI 阻断高危漏洞
- [ ] 8.2.2 `cargo-deny` 阻断未授权 license
- [ ] 8.2.3 密钥管理 review：避免日志泄露、错误回显
- [ ] 8.2.4 依赖 SBOM 生成（`cargo-cyclonedx`）
- [ ] 8.2.5 威胁模型文档

### 8.3 可观测性

- [ ] 8.3.1 Prometheus metrics 端点（`/metrics`）
- [ ] 8.3.2 结构化日志采样率可配置
- [ ] 8.3.3 健康检查分级：`/health` (liveness) vs `/ready` (readiness)
- [ ] 8.3.4 关键路径 trace 采样率

### 8.4 发布

- [ ] 8.4.1 release-plz 自动版本与 changelog
- [ ] 8.4.2 musl + glibc 双构建产物
- [ ] 8.4.3 容器镜像多架构（amd64 + arm64）
- [ ] 8.4.4 Homebrew formula（macOS）
- [ ] 8.4.5 Debian/RPM 包
- [ ] 8.4.6 npm 包 `@paperclipai/server`（指向原生二进制）

---

## 9. 验证门禁（每个 Phase 结束时）

- [ ] 9.1 所有单元测试通过
- [ ] 9.2 所有集成测试通过
- [ ] 9.3 clippy 无 warning（`-D warnings`）
- [ ] 9.4 rustfmt 无 diff
- [ ] 9.5 原 UI 冒烟测试通过
- [ ] 9.6 端到端剧本通过（启动 → 创建 → 触发 → 推送）
- [ ] 9.7 文档已更新
- [ ] 9.8 性能基准已记录
