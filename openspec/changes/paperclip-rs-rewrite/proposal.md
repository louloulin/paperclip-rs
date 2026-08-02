# paperclip-rs-rewrite — 提案

## Why

Paperclip 当前后端基于 Node.js + TypeScript 单体（Express + ws + better-auth + Drizzle + embedded-postgres），共 **760 个 TS 源文件 / 约 44.4 万行代码**，覆盖 56 个路由模块、212 个服务模块、109 张数据库表、11 个内置适配器（Claude/Codex/Cursor/Gemini/Grok/Hermes/OpenClaw/OpenCode/Pi 等）和一个完整的插件 SDK（`@paperclipai/plugin-sdk`）。在长期演进中暴露出三类问题：

1. **运行时性能与资源占用**：Node 单进程承载 HTTP、WebSocket、适配器子进程编排、心跳调度、PostgreSQL 嵌入式实例以及多插件 Worker，常驻内存 400MB+，冷启动 3-5s，CPU 密集路径（OpenAPI 生成、zod 校验、心跳轮询）开销显著。
2. **类型与并发模型边界模糊**：TypeScript 虽补强了类型，但 async/await 难以表达真实的取消、超时、背压语义；adapter 进程编排（`heartbeat.ts` + `plugin-worker-manager.ts` + `live-events-ws.ts`）通过进程/事件/共享 DB 多路协作，缺乏编译期保证。
3. **部署与分发**：Node 运行时 + 原生模块（`embedded-postgres`、`sharp`、`ssh2`、`better-sqlite3`）在容器/单可执行文件场景下体积大、跨平台构建脆弱；CLI/服务器/适配器三端共享同一 ts 工具链但运行时分裂。

将后端重写为 **Rust 多 crate 工作区**，复用现有 React 前端（60 个 API 客户端模块、1168 个 UI 文件、约 34.4 万行），可在保留产品行为的同时获得：单二进制部署、强类型异步运行时（tokio）、零成本抽象、统一的进程/任务模型，以及更精确的资源边界。

## What Changes

- **新增 `paperclip-rs` 工作区**：在仓库根目录的同级 `paperclip-rs/` 下建立 Cargo workspace，按现有模块边界拆分为 ~30 个 crate，每个 crate 单一职责、对外暴露有限的 `pub` API。
- **后端整体重写**：Node.js 服务器的 HTTP 路由、服务层、数据访问、实时通信、认证、存储、密钥、插件 Worker 编排全部以 Rust 重新实现，对外保持与原 API 完全一致的 HTTP/WS 契约（含 OpenAPI、zod 等价的校验、错误码、WebSocket 消息 schema）。
- **数据库层替换**：Drizzle ORM 替换为 `sqlx`（编译期 SQL 校验）或 `sea-orm`；保留 PostgreSQL（嵌入式实例改用 `pg-embedded` 或外挂 `postgres` 二进制），109 张表 schema 一一映射。
- **认证与授权**：better-auth 替换为自研 crate，复用其 session/cookie/role 模型；权限矩阵（`middleware/authz.ts`、`services/authorization.ts`）以 Rust 类型表达。
- **适配器系统**：11 个 TS 适配器（`packages/adapters/*`）的 host 部分用 Rust 重新实现为 `paperclip-adapter-*` crate；worker 进程 IPC 协议（当前为 stdio JSON-RPC）保持稳定，使适配器作者可继续以 Node/Python 写 worker；或在未来版本用 Rust 重写 worker。
- **插件 SDK**：保持 `@paperclipai/plugin-sdk` 的协议不变（JSON-RPC over stdio），Rust 侧以 `paperclip-plugin-host` 承担 worker 池、调度、事件分发、数据库桥接。
- **CLI**：Node CLI（`cli/`）用 Rust 重写为 `paperclip-cli`，命令集合（`run`、`install`、`onboard`、`doctor`、`worktree`、`heartbeat-run`、`pipelines`、`routines`、`service`、`update` 等）一一对应。
- **前端复用**：现有 React UI（`paperclip/ui/`）**完全不动**，仅调整其 dev/prod 的 API base 指向 Rust 服务器；这意味着 paperclip-rs 与 paperclip-ui 可作为独立仓库并行存在，UI 通过标准 HTTP/WS 与 Rust 后端通信。
- **可观测性**：OpenTelemetry 接入、pino 等价日志（`tracing` + JSON 输出）、健康检查、数据库备份链路与原 server 对齐。

**BREAKING**：原 `paperclip/server`（Node）的内部模块边界被废止；外部插件（`paperclip-plugin-*` npm 包）若依赖 host 的私有 API 需迁移到公开的 RPC 协议；CLI 改为单二进制，但命令语义保持兼容。

## Capabilities

### New Capabilities

- `core-domain`：领域模型与业务规则（Company/Agent/Issue/Case/Project/Approval/Decision/Routine/Pipeline/Environment/ExecutionWorkspace/Heartbeat），纯 Rust 类型 + 不变量校验。
- `db-schema`：PostgreSQL schema 与迁移系统，109 张表的 DDL、索引、外键、check 约束、迁移版本管理。
- `data-access`：仓储层（CRUD/查询/事务），等价于原 `services/*.ts` 中的数据库读写；以 trait 抽象便于测试。
- `auth-session`：认证会话、cookie、JWT、API Key、board 用户/agent 双主体模型，等价于 `better-auth.ts`。
- `authz-policy`：基于资源/动作/主体的授权策略，等价于 `services/authorization.ts` 与 `middleware/authz.ts`。
- `http-api`：HTTP 路由与 zod 等价请求/响应校验、OpenAPI 生成、错误处理、压缩、CORS、信任代理。
- `realtime-ws`：WebSocket live-events 通道、订阅/退订、断线重连、ping/pong、token 鉴权。
- `storage-abstraction`：可插拔存储（本地磁盘、S3），等价于 `storage/{local-disk,s3}-provider.ts`。
- `secrets-abstraction`：可插拔密钥提供方（本地加密、AWS Secrets Manager），等价于 `secrets/*`。
- `adapter-runtime`：内置适配器 host 层（模型解析、token 计量、心跳驱动、quota 检查、配置 schema），等价于 `server/src/adapters/`。
- `adapter-claude-local` / `adapter-codex-local` / `adapter-cursor-cloud` / `adapter-cursor-local` / `adapter-gemini-local` / `adapter-grok-local` / `adapter-openclaw-gateway` / `adapter-opencode-local` / `adapter-pi-local`：11 个内置适配器 host 实现。
- `plugin-host`：插件 Worker 池、RPC 路由、事件总线、作业调度、状态读写、数据库桥接，等价于 `services/plugin-*`。
- `plugin-sdk-protocol`：协议 schema（JSON-RPC 方法、消息类型、能力声明），独立 crate 供 host 与 worker 共享。
- `heartbeat-engine`：agent 心跳/任务调度/唤醒/watchdog/monitor 状态机，等价于 `services/heartbeat.ts`。
- `workflow-engine`：routines 与 pipelines 的执行/调度，等价于 `services/routines.ts`、`services/pipelines.ts`。
- `activity-audit`：活动日志、审计、成本事件、决策训练样本，等价于 `services/activity-log.ts`、`services/costs.ts`。
- `realtime-broadcast`：跨进程的 live-event 广播（内存 + 可选 Redis pubsub），供多副本部署。
- `config-runtime`：配置加载、环境变量、`.env`、嵌入式 Postgres 启动、健康检查、版本信息，等价于 `server/src/config.ts`、`server/src/runtime-config.ts`。
- `telemetry-otel`：OpenTelemetry trace/metric/log 接入、启动横幅、请求日志重写。
- `cli-surface`：命令行入口（`paperclipai`），子命令与原 CLI 一一对应。
- `openapi-gen`：从路由+schema 自动生成 OpenAPI 3.1 文档，与 `routes/openapi.ts` 等价。
- `health-backup`：实例健康检查、数据库备份链路、迁移状态，等价于 `routes/health.ts`、`services/database-backup-health.ts`。
- `feature-flags`：特性开关、能力目录（与 `shared/feature-catalog.ts` 等价）。
- `doc-anchors`：文档锚点/批注模型，等价于 `shared/document-anchors.ts`。

### Modified Capabilities

无（这是首次引入 Rust 后端，OpenSpec 中无既有能力被"修改"）。

## Impact

- **受影响代码**：整个 `paperclip/server/`、`paperclip/cli/`、`paperclip/packages/{adapter-*,db,shared,skills-catalog,plugins/sdk}` 的服务端职责，全部由 Rust crate 重新实现。
- **不受影响**：`paperclip/ui/` 前端完整保留；`paperclip/packages/plugins/sandbox-providers/*`、`paperclip/packages/adapters/*/src/ui`、`paperclip/packages/adapters/*/src/cli` 中的 UI/CLI 代码继续随原适配器包分发（worker 仍可在 Node 中运行）。
- **API 契约**：HTTP 路由路径、方法、请求/响应 schema、WebSocket 消息类型、错误码、`X-Paperclip-*` 头部语义与原 server 一致；OpenAPI 文档可作为契约的机器可读来源。
- **依赖**：移除 `express`、`ws`、`better-auth`、`drizzle-orm`、`embedded-postgres`、`pino`、`zod`（运行时部分）、`multer`、`sharp`、`ssh2`、`@aws-sdk/client-s3` 在服务端的运行时依赖；新增 Rust crate 的等价实现。
- **数据库**：PostgreSQL schema 与现有 109 张表兼容；数据迁移通过 `paperclip-rs migrate` 复用同一 schema 文件（重写为 Rust 迁移），无需数据导出/导入。
- **部署产物**：单一可执行文件 `paperclip-server` + `paperclipai` CLI；Docker 镜像体积预计从 ~500MB（Node + 原生模块）下降到 ~80-120MB（musl 静态链接 + 必要动态库）。
- **性能**：HTTP 路由延迟预计下降 30-60%，内存占用下降 40-60%，冷启动 <200ms（无 V8 启动）。
- **组织**：新增 `paperclip-rs/` 仓库/目录；CI 需要新增 Rust toolchain（rustup + cargo），保留现有 Node CI 以构建 UI 与适配器 UI bundle。
