# paperclip-rs

> Rust 重写的 Paperclip 后端。协议、API、WebSocket、数据库 schema、插件 IPC
> 与上游 [paperclip](https://github.com/paperclipai/paperclip) **保持一致**——
> 只是把 Node.js + TypeScript 单体（760 个 TS 源文件、约 44.4 万行）换成
> Rust crate 工作区。

| paperclip（上游） | paperclip-rs（本仓库） |
| --- | --- |
| Node + Express + ws + better-auth + Drizzle + embedded-postgres | Rust + axum + tokio + sqlx + kameo + 外部 PostgreSQL |
| 56 个路由模块、212 个服务模块、109 张表、11 个内置适配器 | 38 个 crate，复刻同一份契约 |
| `@paperclipai/plugin-sdk`（JSON-RPC over stdio） | `pc-plugin-protocol` + `pc-plugin-host`，**协议 schema 不变** |
| React UI（`paperclip/ui/`） | 同上 UI，**完全不动**，只指向 Rust 服务器的 base URL |

API 兼容性是硬约束：现有 dashboard、CLI 调用、第三方插件、`companies.sh`
导入导出、`@paperclipai/shared` 客户端在两个实现之间可以互换使用。

## 仓库布局

```text
paperclip-rs/
├── crates/                # Cargo workspace，38 个 crate
│   ├── pc-core            # 领域类型、不变量、actor 抽象（kameo）
│   ├── pc-errors          # 统一错误模型 + 错误码
│   ├── pc-config          # 环境变量 + .env → 强类型 Config
│   ├── pc-telemetry       # tracing + 启动横幅 + 可选 OTLP
│   ├── pc-db              # sqlx 连接池 + 迁移 runner
│   ├── pc-repos           # 仓储层（76 个文件，与原 services/* 一一映射）
│   ├── pc-migrate         # PostgreSQL schema 迁移（109 张表）
│   ├── pc-migrate-smoke   # 迁移冒烟测试
│   ├── pc-backup          # 实例数据库备份
│   ├── pc-storage         # local-disk / s3 provider trait
│   ├── pc-secrets         # 本地加密 / AWS Secrets Manager provider
│   ├── pc-auth            # 自研 session / cookie / API key（行为等价 better-auth）
│   ├── pc-authz           # 资源 × 动作 × 主体授权矩阵
│   ├── pc-http            # axum 路由（56 个）+ middleware（auth、redaction、body-limit…）
│   ├── pc-openapi         # OpenAPI 3.1 规范生成器 + /openapi.json
│   ├── pc-realtime        # tokio broadcast 事件总线 + live-events WS
│   ├── pc-ws              # WebSocket handler
│   ├── pc-heartbeat       # 心跳调度 / 唤醒 / watchdog / 恢复
│   ├── pc-agent           # agent supervisor + 权限
│   ├── pc-cron            # scheduled routines
│   ├── pc-workflow        # routines + pipelines 执行引擎
│   ├── pc-feature-flags   # 能力目录
│   ├── pc-activity        # 活动日志 / 审计 / 成本事件
│   ├── pc-adapter-api     # 适配器 trait + registry
│   ├── pc-adapter-process # 子进程 helper（spawn / stdio）
│   ├── pc-adapter-{claude,codex,cursor-cloud,cursor-local,
│   │                  gemini,grok,hermes,hermes-gateway,
│   │                  openclaw-gateway,opencode,pi}-local
│   │                     # 11 个内置适配器 host 实现
│   ├── pc-plugin-protocol # JSON-RPC schema（host ↔ worker，协议稳定）
│   ├── pc-plugin-host     # Worker 池、调度、事件分发、DB 桥接
│   ├── pc-server          # `paperclip-server` 二进制入口
│   └── pc-cli             # `paperclipai` CLI
├── ui/                    # 复用上游 React UI（@paperclipai/ui），未修改
├── packages/              # TS adapter-utils / skills-catalog / teams-catalog
├── docs/                  # 架构、计划、Node↔Rust 对照表
├── openspec/              # OpenSpec 提案 + 19 个契约 spec
└── Cargo.toml             # workspace root
```

完整对照见 [`MODULE-MAPPING.md`](MODULE-MAPPING.md)（Node/TS 文件 → Rust crate
逐项映射）与 [`docs/02-PAPERCLIP-ARCHITECTURE.md`](docs/02-PAPERCLIP-ARCHITECTURE.md)。

## 协议一致性（硬约束）

所有外部契约与上游 paperclip **保持一致**：

- **HTTP**：56 个路由模块，路径 / 方法 / 请求体 schema / 响应 schema / 错误码
  与 `paperclip/server/src/routes/*.ts` 一一对应。`/openapi.json` 由
  `pc-openapi` 生成，结构兼容。
- **WebSocket**：`/live-events` 通道、`last_event_id` resume、`event_id` /
  `resource` / `resource_id` / `actor` / `at` 字段与原 server 对齐。
- **数据库**：109 张表 schema（`pc-db` + `pc-migrate`）保留原 DDL、索引、
  外键与 check 约束；`PAPERCLIP_DB_RUN_MIGRATIONS=false` 时跳过迁移。
- **插件 IPC**：JSON-RPC 2.0 over stdio，方法名（`initialize` / `health` /
  `runJob` / `handleWebhook` / `getData` / `performAction` / `executeTool` /
  `onEvent` / `shutdown`）、envelope、错误码与 `@paperclipai/plugin-sdk`
  完全相同。这意味着已发布的 npm 插件进程可继续被 Rust host 拉起。
- **认证**：session / cookie / API key / 双主体（user + agent）模型与
  `better-auth.ts` 行为等价；`X-Paperclip-*` 头部语义不变。
- **适配器**：11 个内置适配器的 host 部分重新实现，但 worker 子进程
  IPC（model resolve、token 计量、心跳、quota、config schema）保持兼容。
- **CLI**：`paperclipai` 子命令（`install` / `onboard` / `doctor` / `env` /
  `env-lab` / `configure` / `db:backup` / `worktree` / `service` / `run` /
  `heartbeat-run` / `auth bootstrap-ceo` / `client {whoami, live-events,
  companies, agents, issues, get, post}`）与 `paperclip/cli/src/index.ts`
  一一对应。

迁移路径：把现有 Node 部署指向新端口（默认 `127.0.0.1:3100`），数据库 URL
不变，UI base URL 切换即可。详见 [`openspec/changes/paperclip-rs-rewrite/`](openspec/changes/paperclip-rs-rewrite/)。

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

构建配置见根目录 [`Cargo.toml`](Cargo.toml)：workspace 38 个成员，
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

服务器装配顺序（见 `crates/pc-server/src/main.rs`）：
1. 加载 `pc-config`（环境变量 + `.env`）
2. 初始化 `pc-telemetry`（JSON 日志 + 启动横幅；可选 OTLP via
   `--features otlp` 或 `pc-telemetry::install_global`）
3. 连接 `pc-db` 并按需执行迁移
4. 启动 `pc-core` actor 根运行时 + `pc-heartbeat` supervisor +
   `pc-agent` supervisor
5. 注册 11 个内置适配器到 `pc-adapter-api::AdapterRegistry`
6. 装配 `pc-http::routes::router()`（56 个路由）+ 默认 middleware
7. 探测 `UI_DIR` / `ui/dist` / `../ui/dist`，若存在则用
   `tower_http::services::ServeDir` 提供静态 UI 资源
8. `axum::serve` 监听，SIGTERM / Ctrl-C 触发 graceful shutdown

CLI 客户端默认指向 `http://127.0.0.1:3100`，可通过 `PAPERCLIP_BASE_URL`
覆盖；可通过 `PAPERCLIP_API_KEY` 或 session token 鉴权。

## 架构要点

- **actor 抽象**：`pc-core::actor_runtime::kameo_api` 用 `kameo = 0.22`
  做进程内 actor 化扩展（每个 WS 连接、每个插件 worker、每个 agent 都
  可成为被监管 actor）；`pc-realtime` 内部用 `tokio::sync::broadcast`
  做高频 fan-out。
- **仓储化重构**：`pc-repos` 把上游 `services/*.ts` 中的数据库读写拆为
  76 个 `*Repo` 结构体，每个文件单一职责，便于 trait mock 测试。
- **错误模型**：`pc-errors` 定义跨 crate 错误码 + `thiserror` 派生；
  `pc-http::error` 在 middleware 中将内部错误映射为 OpenAPI 中的
  HTTP 响应码与 schema。
- **可观测性**：tracing JSON 日志（开发模式 pretty）、可选
  OpenTelemetry exporter（`pc-telemetry` 的 `otlp` feature）、`/health`
  路由与 `live-events` 监控事件。
- **配置分层**：`pc-config::home_paths` 解析 `PAPERCLIP_HOME` /
  `PAPERCLIP_INSTANCE_ID` / `PAPERCLIP_CONFIG_BASENAME` / `PAPERCLIP_ENV_FILENAME`
  等；测试用 `build_with` 接受 env lookup 函数避免并行污染。

## 状态

项目处于持续重写阶段：以 17 周 / 7 阶段为执行蓝图（见
[`PROJECT-PLAN.md`](PROJECT-PLAN.md) 与 [`docs/04-EXECUTION-PLAN.md`](docs/04-EXECUTION-PLAN.md)），
当前进度见 [`docs/05-PROGRESS-AUDIT.md`](docs/05-PROGRESS-AUDIT.md) 与
[`docs/07-COMPREHENSIVE-GAP-ANALYSIS.md`](docs/07-COMPREHENSIVE-GAP-ANALYSIS.md)
（Node↔Rust 逐项 gap matrix）。

Rust 后端目前可独立编译；前端（`paperclip/ui/`）作为 `@paperclipai/ui`
npm 包复用，base URL 切换后即可对接 Rust 服务器。

## 贡献

仓库内 `docs/` 目录提供 Node 端基线架构、Actor 分析、迁移计划与逐项
对照表，是贡献者上手最快的入口。OpenSpec 提案、19 个契约 spec 与
275 项 checkbox 任务清单在 `openspec/changes/paperclip-rs-rewrite/`。

> 还在写什么文档？见 [`docs/DOCUMENTATION-ROADMAP.md`](docs/DOCUMENTATION-ROADMAP.md)
> （78 项缺口清单、优先级矩阵与完成判据）。

## 协议与许可

本仓库源码采用 **MIT License**（与上游 paperclip 一致），见 workspace
根 `Cargo.toml` 中 `license.workspace = true` 的 `MIT` 声明。
Paperclip 与 Paperclip Labs, Inc. 的商标与产品名称归属上游；本仓库为
独立实现，不属于上游组织，除非另行说明。
