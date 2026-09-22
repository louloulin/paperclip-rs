# multica-rs

> Rust 重写的 Multica 后端。协议、API、WebSocket、数据库 schema、插件 IPC
> 与上游 [multica](https://github.com/louloulin/multica) **保持一致**——
> 只是把 Go + TypeScript 单体（1891 个 Go 源文件、约 23.5 万行 TS/TSX）换成
> Rust crate 工作区。

| multica（上游） | multica-rs（本仓库） |
| --- | --- |
| Go (Chi) + Next.js + gorilla/websocket + pgx | Rust + axum + tokio + sqlx + kameo + PostgreSQL 17 |
| 126 个 handler + 120 个 service | 多 crate，复刻同一份契约 |
| 534 个迁移文件 | 同步实现；当前已包含 0001_init |
| 26 个 runtime profile + 6 个 channel | 26 个 runtime adapter + 6 个 channel crate |
| `multica/packages/plugin-sdk`（JSON-RPC over stdio） | `mc-plugin-protocol` + `mc-plugin-host`，**协议 schema 不变** |
| React UI（`multica/apps/web/`） | 同上 UI，**完全不动**，只指向 Rust 服务器的 base URL |

API 兼容性是硬约束：现有 dashboard、CLI 调用、第三方插件、`companies.sh`
导入导出、multica TS 客户端在两个实现之间可以互换使用。

## 仓库布局

```text
multica-rs/
├── crates/                # Cargo workspace
│   ├── mc-config          # 强类型 Config（env + .env）
│   ├── mc-errors          # 统一错误 + HTTP 状态映射
│   ├── mc-telemetry       # tracing + 启动横幅
│   ├── mc-db              # sqlx 连接池 + 迁移 runner
│   ├── mc-core            # 领域类型 / 不变量 / actor 抽象
│   ├── mc-auth            # session / cookie / API key / PAT / verification
│   ├── mc-authz           # 资源 × 动作 × 主体授权
│   ├── mc-realtime        # tokio broadcast + WS
│   ├── mc-ws              # WebSocket handler
│   ├── mc-storage         # local-disk / s3
│   ├── mc-secrets         # 本地加密 / AWS Secrets Manager
│   ├── mc-feature-flags   # 能力目录
│   ├── mc-openapi         # OpenAPI 3.1 生成
│   ├── mc-repos           # 仓储层（M0 占位）
│   ├── mc-plugin-protocol # JSON-RPC schema（host ↔ worker，协议稳定）
│   └── mc-http            # axum 路由 + middleware + state
├── apps/
│   ├── mc-server          # multica-server 二进制入口
│   └── mc-cli             # multica CLI
├── migrations/            # 534 个迁移文件（M0：0001_init）
├── docs/                  # 架构、计划、复用分析、crate 映射
└── Cargo.toml             # workspace 根
```

完整映射见 [`docs/03-CRATE-MAPPING.md`](docs/03-CRATE-MAPPING.md) 与
[`docs/02-ANALYSIS.md`](docs/02-ANALYSIS.md)。

## 协议一致性（硬约束）

所有外部契约与上游 multica **保持一致**：

- **HTTP**：路径 / 方法 / 请求体 schema / 响应 schema / 错误码
  与 `multica/server/internal/handler/*.go` 一一对应。`/openapi.json` 由
  `mc-openapi` 生成，结构兼容。
- **WebSocket**：`/live-events` 通道、`last_event_id` resume、`event_id` /
  `resource` / `resource_id` / `actor` / `at` 字段与原 server 对齐。
- **数据库**：534 张表的 DDL、索引、外键、check 约束；`MULTICA_DB_RUN_MIGRATIONS=false` 跳过。
- **插件 IPC**：JSON-RPC 2.0 over stdio，方法名（`initialize` / `health` /
  `runJob` / `handleWebhook` / `getData` / `performAction` / `executeTool` /
  `onEvent` / `shutdown`）、envelope、错误码与 `multica/packages/plugin-sdk`
  完全相同。
- **认证**：session / cookie / API key / PAT / verification code / 双因素；
  `X-Multica-*` 头部语义不变。
- **Runtime**：26 个 runtime profile 与上游一一对应。
- **Channel**：6 个内置 channel（Slack / Lark / DingTalk / WeCom / Telegram / 自定义）与上游一一对应。
- **CLI**：`multica` 子命令（`whoami` / `live-events` / `version` / `health`）与
  `multica` CLI 一一对应。

迁移路径：把现有 multica 部署指向新端口（默认 `127.0.0.1:3500`），数据库 URL
不变，UI base URL 切换即可。

## 构建

需要 Rust **stable ≥ 1.80**（见 `rust-toolchain.toml`）与 PostgreSQL ≥ 14。
UI 产物（可选）需要 Node 20+ 与 pnpm。

```bash
# 仅 Rust 后端
cargo build --release

# 服务器二进制
./target/release/multica-server

# CLI
./target/release/multica --help
```

构建配置见根目录 [`Cargo.toml`](Cargo.toml)：workspace 当前 16 个成员（M0），
共享依赖（tokio / axum / sqlx / serde / chrono / uuid / thiserror / clap /
tracing / kameo）。`[profile.release]` 启用 `lto = "thin"`、
`codegen-units = 1`、`strip = "symbols"`，目标产物为单个静态二进制；
`[profile.dev]` 开启增量编译 + `debug = 1`。workspace 全局
`unsafe_code = "forbid"`，clippy `pedantic` 开启。

## 运行

最小启动：

```bash
export MULTICA_DATABASE_URL='postgres://multica:multica@127.0.0.1:5432/multica'
export MULTICA_PORT=3500
./target/release/multica-server
```

服务器装配顺序（见 `apps/mc-server/src/main.rs`）：
1. 加载 `mc-config`（环境变量 + `.env`）
2. 初始化 `mc-telemetry`（JSON 日志 + 启动横幅；可选 OTLP via
   `--features otlp` 或 `mc-telemetry::install_global`）
3. 连接 `mc-db` 并按需执行迁移
4. 启动 `mc-core` actor 根运行时
5. 装配 `mc-http::routes::router()` + 默认 middleware
6. `axum::serve` 监听，SIGTERM / Ctrl-C 触发 graceful shutdown

CLI 客户端默认指向 `http://127.0.0.1:3500`，可通过 `MULTICA_BASE_URL`
覆盖；可通过 `MULTICA_API_KEY` 鉴权。

## 架构要点

- **actor 抽象**：`mc-core::actor` 提供类型擦除的 `AnyActor` trait；
  `ActorRegistry` 用 key 索引 typed actor；上层 crate 按需实现具体类型。
- **错误模型**：`mc-errors` 定义跨 crate 错误码 + `thiserror` 派生；
  `mc-http::error` 在 axum IntoResponse 中将内部错误映射为 HTTP 状态码。
- **可观测性**：tracing JSON 日志（开发模式 pretty）、可选 OTLP exporter。
- **配置分层**：`mc-config::home_paths` 解析 `MULTICA_HOME` /
  `MULTICA_INSTANCE_ID` 等；测试用 `build_with` 接受 env lookup 函数。

## 状态

项目处于 **M0**（基础设施搬迁）阶段。当前已经完成：

- ✅ workspace / Cargo.toml 骨架
- ✅ 16 个 crate 基础设施：mc-config / mc-errors / mc-telemetry / mc-db /
  mc-core / mc-auth / mc-authz / mc-realtime / mc-ws / mc-storage /
  mc-secrets / mc-feature-flags / mc-openapi / mc-plugin-protocol / mc-repos
  / mc-http / mc-migrate
- ✅ 2 个二进制：multica-server / multica CLI
- ✅ 1 个迁移文件（0001_init.up.sql，对应 multica 001_init 的核心实体）
- ✅ 单元测试覆盖各 crate 的核心不变量

下一步（M1）目标：完成 workspace / member / invitation / PAT / verification 路由与服务。

## 贡献

仓库内 `docs/` 目录提供迁移蓝图、复用分析、crate 映射与执行计划。

## 协议与许可

本仓库源码采用 **MIT License**（与上游 multica 一致），见 workspace 根
`Cargo.toml` 中 `license.workspace = true` 的 `MIT` 声明。
Multica 与 Multica Labs, Inc. 的商标与产品名称归属上游；本仓库为
独立实现，不属于上游组织，除非另行说明。