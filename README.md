# paperclip-rs

> Rust 重写的 Paperclip 后端。协议、API、WebSocket、数据库 schema、插件 IPC
> 与上游 [paperclip](https://github.com/paperclipai/paperclip) **保持一致**——
> 只是把 Node.js + TypeScript 单体（760 个 TS 源文件、约 44.4 万行）换成
> Rust crate 工作区。

> **当前快照（2026-09-03，commit `861efc1`）**：
> 108 个 crate · 595,845 行 Rust · 12,025 个 pub API · 207 个 migration（最高 0208） ·
> 路由 method+path 覆盖 **90.33%** · 综合行为等价 ≈ **85%**

| paperclip（上游） | paperclip-rs（本仓库） |
| --- | --- |
| Node + Express + ws + better-auth + Drizzle + embedded-postgres | Rust + axum + tokio + sqlx + kameo + 外部 PostgreSQL |
| 56 个路由模块、212 个服务模块、534 个源文件、11 个内置适配器 | **108 个 crate**，复刻同一份契约 |
| `@paperclipai/plugin-sdk`（JSON-RPC over stdio） | `pc-plugin-protocol` + `pc-plugin-host`，**协议 schema 不变** + R877 sidecar proxy 让现有 JS 插件可被 Rust host 加载 |
| React UI（`paperclip/ui/`） | 同上 UI，**完全不动**，只指向 Rust 服务器的 base URL |

API 兼容性是硬约束：现有 dashboard、CLI 调用、第三方插件、`companies.sh`
导入导出、`@paperclipai/shared` 客户端在两个实现之间可以互换使用。

## 协议一致性（硬约束）

所有外部契约与上游 paperclip **保持一致**：

- **HTTP**：56 个路由模块，路径 / 方法 / 请求体 schema / 响应 schema / 错误码
  与 `paperclip/server/src/routes/*.ts` 一一对应。`/openapi.json` 由
  `pc-openapi` 生成（828 KB / 32,664 行），结构兼容。路由 method+path 覆盖率
  **90.33%**（579/641 Node 路由在 Rust 端有对应）。
- **WebSocket**：`/live-events` 通道、`last_event_id` resume、`event_id` /
  `resource` / `resource_id` / `actor` / `at` 字段与原 server 对齐；R252-R257
  完成 subscriber trait + channel filter + per-IP rate limit + per-company
  connection limit + since-until + replay + `/api/realtime/stats`。
- **数据库**：207 个 migration 文件，最高 0208；保留原 DDL、索引、外键与 check
  约束；`PAPERCLIP_DB_RUN_MIGRATIONS=false` 时跳过迁移。
- **插件 IPC**：JSON-RPC 2.0 over stdio，**24 个 host→worker 方法**
  （`initialize` / `health` / `shutdown` / `validateConfig` / `configChanged` /
  `onEvent` / `runJob` / `handleWebhook` / `handleApiRequest` / `getData` /
  `performAction` / `executeTool` / `detectExternalObjects` /
  `resolveExternalObject` / `refreshExternalObjects` / 9 个 `environment*`
  方法），envelope、错误码与 `@paperclipai/plugin-sdk` 完全相同。worker→host
  10 个方法（`progress` / `log` / `emitEvent` / `getState` / `setState` /
  `dataQuery` / `dataMutate` / `toolInvoke` / `activityLog` / `notify`）。
  **OPERATION_CAPABILITIES map 49 个 operation + Node upstream drift detection
  fixture**。
- **认证**：session / cookie / API key / 双主体（user + agent）模型与
  `better-auth.ts` 行为等价；`X-Paperclip-*` 头部语义不变；argon2id 密码哈希
  （19_456 KiB 内存 / 2 iters / 1 parallelism）；refresh rotation / OAuth
  provider / CSRF / first-admin-claim 简化实现（R865 待完整化）。
- **适配器**：11 个内置适配器的 host 部分重新实现（4 个完整 + 4 个 stub + 3 个
  HTTP API），worker 子进程 IPC（model resolve、token 计量、心跳、quota、
  config schema）保持兼容。
- **CLI**：`paperclipai` 19 个子命令（`install` / `onboard` / `doctor` /
  `env` / `env-lab` / `configure` / `db:backup` / `worktree` / `service` /
  `run` / `heartbeat-run` / `auth bootstrap-ceo` / `client {whoami, live-events,
  companies, agents, issues, get, post}`）与 `paperclip/cli/src/index.ts`
  一一对应。

迁移路径：把现有 Node 部署指向新端口（默认 `127.0.0.1:3100`），数据库 URL
不变，UI base URL 切换即可。详见 [`PARITY.md`](PARITY.md)（全面对标文档）
与 [`openspec/changes/paperclip-rs-rewrite/`](openspec/changes/paperclip-rs-rewrite/)。

## 仓库布局

```text
paperclip-rs/
├── crates/                # Cargo workspace，108 个 crate
│   ├── pc-core            # 领域类型、不变量、actor 抽象（kameo）+ typed IDs
│   ├── pc-errors          # 统一错误模型 + 错误码（thiserror）
│   ├── pc-config          # 环境变量 + .env → 强类型 Config
│   ├── pc-telemetry       # tracing + 启动横幅 + 可选 OTLP
│   ├── pc-db              # sqlx 连接池 + 迁移 runner（207 个 SQL 文件）
│   ├── pc-repos           # 仓储层（114 个 *Repo 结构体文件）
│   ├── pc-migrate         # PostgreSQL schema 迁移
│   ├── pc-backup          # 实例数据库备份（pg_dump + restore）
│   ├── pc-storage         # local-disk / s3 provider trait
│   ├── pc-secrets         # 本地加密 / AWS / GCP / Vault provider + decision_signing
│   ├── pc-auth            # 自研 session / cookie / API key（行为等价 better-auth）
│   ├── pc-authz           # 资源 × 动作 × 主体授权矩阵
│   ├── pc-http            # axum 路由（75 个文件覆盖 56 路由）+ 11 个 middleware
│   ├── pc-openapi         # OpenAPI 3.1 规范生成器 + /openapi.json
│   ├── pc-realtime        # tokio broadcast 事件总线 + live-events WS
│   ├── pc-heartbeat       # 心跳调度 / 唤醒 / readiness / staleness recovery
│   ├── pc-agent           # agent supervisor + 权限 + assignability + invokability
│   ├── pc-issues          # issue CRUD + tree_control + continuation_summary
│   ├── pc-routines        # scheduled routines + lifecycle
│   ├── pc-pipelines       # workflow pipelines 执行引擎
│   ├── pc-decisions       # decision CRUD + bundle + effect_executor + signing
│   ├── pc-companies       # company 域服务（search / search_rate_limit）
│   ├── pc-company-member  # company_memberships 仓储（修复 4 个 hidden bug）
│   ├── pc-workflow        # routines + pipelines 执行引擎
│   ├── pc-acpx            # acpx-engine Rust 镜像（540+ tests）
│   ├── pc-plugin-protocol # JSON-RPC schema（49 operation + drift fixture）
│   ├── pc-plugin-host     # Worker 池、调度、事件分发、DB 桥接 + R877 sidecar proxy
│   ├── pc-server          # `paperclip-server` 二进制入口
│   ├── pc-cli             # `paperclipai` CLI
│   ├── pc-adapter-api     # 适配器 trait + registry
│   ├── pc-adapter-process # 子进程 helper（spawn / stdio / timeout）
│   └── pc-adapter-{claude,codex,cursor-cloud,cursor-local,
│                    gemini,grok,hermes,hermes-gateway,
│                    openclaw-gateway,opencode,pi}-local/cloud
│                       # 11 个内置适配器 host 实现
├── ui/                    # 复用上游 React UI（@paperclipai/ui），未修改
├── packages/              # TS adapter-utils / skills-catalog / teams-catalog
├── docs/                  # 架构、计划、Node↔Rust 对照表、progress audit
├── openspec/              # OpenSpec 提案 + 19 个契约 spec
└── Cargo.toml             # workspace root
```

完整对照见：
- [`PARITY.md`](PARITY.md) — **新增**：paperclip-rs vs paperclip 全面对标文档（覆盖率、模块映射、协议等价性、架构差异、演进路线）
- [`MODULE-MAPPING.md`](MODULE-MAPPING.md) — Node/TS 文件 → Rust crate 逐项映射
- [`ARCHITECTURE.md`](ARCHITECTURE.md) — 当前架构状态
- [`docs/02-PAPERCLIP-ARCHITECTURE.md`](docs/02-PAPERCLIP-ARCHITECTURE.md) — 底层架构图

## 构建

需要 Rust **stable ≥ 1.80**（见 `rust-toolchain.toml`）与 PostgreSQL ≥ 14。
UI 产物（可选）需要 Node 20+ 与 pnpm。

```bash
# 仅 Rust 后端
cargo build --release

# 服务器二进制
./target/release/paperclip-server

# CLI
./target/release/paperclipai --help
```

构建配置见根目录 [`Cargo.toml`](Cargo.toml)：workspace **108 个成员**，
共享依赖（tokio / axum / sqlx / serde / chrono / uuid / thiserror / clap /
tracing / kameo 0.22）。`[profile.release]` 启用 `lto = "thin"`、
`codegen-units = 1`、`strip = "symbols"`，目标产物为单个静态二进制；
`[profile.dev]` 开启增量编译 + `debug = 1`。workspace 全局
`unsafe_code = "forbid"`，clippy `pedantic` 开启。

## 运行

最小启动：

```bash
export PAPERCLIP_DATABASE_URL='postgres://paperclip:paperclip@127.0.0.1:5432/paperclip'
export PAPERCLIP_PORT=3100
./target/release/paperclip-server
```

服务器装配顺序（见 `apps/pc-server/src/main.rs`）：
1. 加载 `pc-config`（环境变量 + `.env`）
2. 初始化 `pc-telemetry`（JSON 日志 + 启动横幅；可选 OTLP via
   `--features otlp` 或 `pc-telemetry::install_global`）
3. 连接 `pc-db` 并按需执行迁移（207 个 migration）
4. 启动 `pc-core` actor 根运行时 + `pc-heartbeat` supervisor（含 readiness
   检查 + staleness recovery） + `pc-agent` supervisor
5. 注册 11 个内置适配器到 `pc-adapter-api::AdapterRegistry`
6. 装配 `pc-http::routes::router()`（75 个路由文件）+ 11 个默认 middleware
7. 探测 `UI_DIR` / `ui/dist` / `../ui/dist`，若存在则用
   `tower_http::services::ServeDir` 提供静态 UI 资源
8. `axum::serve` 监听，SIGTERM / Ctrl-C 触发 graceful shutdown

CLI 客户端默认指向 `http://127.0.0.1:3100`，可通过 `PAPERCLIP_BASE_URL`
覆盖；可通过 `PAPERCLIP_API_KEY` 或 session token 鉴权。

## 架构要点

### 物理分层

```text
┌─────────────────────────────────────────────────────────┐
│ HTTP 入口（pc-http 75 路由文件）                         │
│   ↓ ↓ ↓                                                │
│ 域服务（pc-companies / pc-issues / pc-heartbeat / ...） │
│   ↓ ↓ ↓                                                │
│ 仓储层（pc-repos 114 *Repo 结构体）                      │
│   ↓                                                    │
│ DB（sqlx + PostgreSQL 14+，207 migrations）              │
└─────────────────────────────────────────────────────────┘
┌─────────────────────────────────────────────────────────┐
│ 实时事件（pc-realtime tokio broadcast）                   │
│  ←  域服务通过 RealtimeHandle::publish 推事件            │
│  →  WS/SSE handler 订阅 + 重连 resume + rate limit      │
└─────────────────────────────────────────────────────────┘
┌─────────────────────────────────────────────────────────┐
│ 后台调度（pc-heartbeat + pc-routines + pc-cron）        │
│  →  tick → claim run → readiness 6 项检查 → 调 adapter  │
│  →  pc-acpx::execute() 拼装 prompt + 子进程 runtime     │
└─────────────────────────────────────────────────────────┘
┌─────────────────────────────────────────────────────────┐
│ 插件运行时（pc-plugin-host worker pool）                 │
│  →  spawn 子进程 → JSON-RPC over stdio                  │
│  →  R877 Node sidecar proxy 让现有 JS 插件可加载        │
└─────────────────────────────────────────────────────────┘
```

### Rust idiom 充分利用

- **actor 抽象**：`pc-core::actor_runtime::kameo_api` 用 `kameo = 0.22`
  做进程内 actor 化扩展；仅"需要被监管的有状态对象"用 actor
  （AgentSupervisor / IssueTreeControlActor / WorkerSupervisor /
  HeartbeatScheduler）。`pc-realtime` 内部用 `tokio::sync::broadcast`
  做高频 fan-out。
- **类型安全 ID**：`pc-repos::typed_ids` 用 `pc_core::Id<T>` newtype + 10 个
  marker 类型（`CompanyMarker` / `DecisionMarker` / ...），编译期防止
  `find_by_company(agent_id)` 这类语义错误，零运行时开销。
- **仓储 + Hook 链**：`pc-issues` / `pc-decisions` / `pc-routines` 等域服务
  都有 `*Hook` trait + `Recording*Hook` 测试 mock + `Noop*Hook` 默认实现。
- **错误模型**：`pc-errors` 定义跨 crate 错误码 + `thiserror` 派生；
  `pc-http::error` 在 middleware 中将内部错误映射为 OpenAPI 中的
  HTTP 响应码与 schema；`SidecarError` 用 `thiserror` 派生 5 个错误变体。
- **Provider 模式**：R877 `SidecarLauncher` trait（async）+ `NodeSidecarLauncher`
  默认实现 + `SidecarLauncherRegistry` 多 launcher 路由 — 同一接口支持
  Rust/Node/Python 多种 plugin runtime。
- **配置分层**：`pc-config::home_paths` 解析 `PAPERCLIP_HOME` /
  `PAPERCLIP_INSTANCE_ID` / `PAPERCLIP_CONFIG_BASENAME` /
  `PAPERCLIP_ENV_FILENAME` 等；测试用 `build_with` 接受 env lookup 函数
  避免并行污染。
- **可观测性**：tracing JSON 日志（开发模式 pretty）、可选 OpenTelemetry
  exporter（`pc-telemetry` 的 `otlp` feature）、`/health` 路由与
  `live-events` 监控事件。

## 状态

项目处于持续重写阶段：以 17 周 / 7 阶段为执行蓝图（见
[`PROJECT-PLAN.md`](PROJECT-PLAN.md) 与 [`docs/04-EXECUTION-PLAN.md`](docs/04-EXECUTION-PLAN.md)），
当前进度见：

| 文档 | 内容 |
|---|---|
| [`PARITY.md`](PARITY.md) | **全面对标文档**：覆盖率、模块映射、协议等价性、架构差异、演进路线 |
| [`docs/05-PROGRESS-AUDIT.md`](docs/05-PROGRESS-AUDIT.md) | 445K 行 R1-R19 进度审计 |
| [`docs/07-COMPREHENSIVE-GAP-ANALYSIS.md`](docs/07-COMPREHENSIVE-GAP-ANALYSIS.md) | 6210 行 R1-R863 详细增量史 |
| [`docs/09-CURRENT-STATE-AND-NEXT-PLAN.md`](docs/09-CURRENT-STATE-AND-NEXT-PLAN.md) | 当前状态 + 下一阶段计划 |
| [`docs/parity-gap-report.md`](docs/parity-gap-report.md) | 最新 parity 脚本分类 gap |
| [`docs/06-NODE-RUST-GAP-MATRIX.md`](docs/06-NODE-RUST-GAP-MATRIX.md) | 行为等价深度分析 |

### 覆盖率快照（实跑 parity-check.sh + diff-routes.sh）

| 维度 | 数值 |
|---|---|
| 模块层（脚本文件名匹配） | **30.5%**（163/534 — 严重低估） |
| 路由 method+path | **90.33%**（579/641） |
| HTTP 路由形状 | 100%（56/56） |
| 数据库 schema | ~95%（207 migrations / 最高 0208） |
| 插件 IPC 协议 | 100%（方法名 / envelope / 错误码） |
| 插件能力校验 | 100%（49 operation + Node drift detection） |
| 适配器 CLI | ~60%（4 完整 + 4 stub + 3 HTTP API） |
| 综合行为等价 | **~85%** |

## 贡献

仓库内 `docs/` 目录提供 Node 端基线架构、Actor 分析、迁移计划与逐项
对照表，是贡献者上手最快的入口。OpenSpec 提案、19 个契约 spec 与
275 项 checkbox 任务清单在 `openspec/changes/paperclip-rs-rewrite/`。

**中文文档**（已落地）：
- [`AGENTS.md`](AGENTS.md) — 453 行开发指南
- [`PLUGIN_AUTHORING.md`](PLUGIN_AUTHORING.md) — 553 行插件作者指南
- [`OPERATIONS.md`](OPERATIONS.md) — 416 行运维手册
- [`MIGRATION_FROM_NODE.md`](MIGRATION_FROM_NODE.md) — 380 行迁移指南

> 还在写什么文档？见 [`docs/DOCUMENTATION-ROADMAP.md`](docs/DOCUMENTATION-ROADMAP.md)
> （78 项缺口清单、优先级矩阵与完成判据）。

## 协议与许可

本仓库源码采用 **MIT License**（与上游 paperclip 一致），见 workspace
根 `Cargo.toml` 中 `license.workspace = true` 的 `MIT` 声明。
Paperclip 与 Paperclip Labs, Inc. 的商标与产品名称归属上游；本仓库为
独立实现，不属于上游组织除非另行说明。
