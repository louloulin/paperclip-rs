# Repository Instructions

`multica-rs` is the Rust rewrite of the [Multica](https://github.com/louloulin/multica) backend. These instructions apply to all coding agents working in this repository.

## Scope and Reading Order

- 计划文档：`docs/01-PLAN.md`（先看）
- 复用分析：`docs/02-ANALYSIS.md`（理解为何这样 crate 切分）
- crate 映射：`docs/03-CRATE-MAPPING.md`（按 crate 找对应 upstream 文件）

## Module Boundaries

| 路径 | 职责 |
| --- | --- |
| `crates/mc-config` | env + .env → 强类型 `Config`；单一职责，不持有 IO |
| `crates/mc-errors` | 跨 crate 统一错误 + HTTP 状态映射 + JSON 错误响应 |
| `crates/mc-telemetry` | tracing 初始化 + 启动横幅 + 实例遥测 stub + log redaction |
| `crates/mc-db` | sqlx 连接池 + 迁移 runner + 健康检查 |
| `crates/mc-core` | 领域类型（不依赖 sqlx）/ 不变量 / actor 抽象 |
| `crates/mc-auth` | session / cookie / API key / PAT / verification / password hash（容器是 in-memory 抽象） |
| `crates/mc-authz` | 资源 × 动作 × 主体授权 |
| `crates/mc-realtime` | tokio broadcast 事件总线 + envelope |
| `crates/mc-ws` | `/live-events` WebSocket handler |
| `crates/mc-storage` | local-disk / s3 provider trait |
| `crates/mc-secrets` | AES-GCM 加密 + secret store trait |
| `crates/mc-feature-flags` | feature flag catalog |
| `crates/mc-openapi` | OpenAPI 3.1 spec generator |
| `crates/mc-repos` | 数据库仓储层：workspace / member / invitation / share_link / verification / pat / user（in-memory + Postgres 双实现） |
| `crates/mc-plugin-protocol` | JSON-RPC 2.0 over stdio |
| `crates/mc-http` | axum 路由 + middleware + state |
| `apps/mc-server` | `multica-server` 二进制 |
| `apps/mc-cli` | `multica` CLI 二进制 |

依赖方向：`mc-http → mc-repos → mc-core → mc-config / mc-errors / mc-telemetry`；
其余为水平依赖。

## Build / Lint / Test

```bash
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo fmt --all
```

CI（GitHub Actions）跑：
- `cargo build --workspace --locked`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- `cargo fmt --all --check`

## Coding Rules

- `unsafe_code = "forbid"`、`clippy pedantic` 与 paperclip-rs 一致。
- 一个文件一个 Repo / 一个文件一个领域类型。
- 不要在领域 crate 直接依赖 sqlx — 用 `mc-repos` 桥接。
- 不要把外部 IO 写在 `mc-core` 里。
- 不在 git 提交信息中暴露 secret / token / key。
- 跨 crate 错误用 `mc_errors::Error`，不要重新定义 error enum。
- API 错误响应必须用 `mc_errors::ErrorResponse`，**禁止 `as T` cast**。

## Database Migrations

- 文件名格式：`NNNN_<name>.up.sql`，N 从 `0001` 起。
- 每个文件一个事务性变更（multica-rs runner 默认包在一个 transaction 里）；
  对于 DDL（如 `CREATE INDEX CONCURRENTLY`）放到独立文件。
- 加列必须 `DEFAULT ... NOT NULL` 兼容旧数据。
- 不要外键约束到 soft-delete 列。

## Repo Layer Conventions（mc-repos）

每个领域 crate 在 `mc-repos` 里有同名文件，提供：

1. `Row` 结构体：与 DB 行 1:1，`created_at` 用 `chrono::DateTime<Utc>`，
   `Id` 直接复用 `mc_core::Id`。
2. `New*` 结构体：创建入参，`Option<Id>` 为 None 时由 DB 默认生成。
3. `*Update` 结构体：用 `Option<Option<T>>` 表示「key 是否出现 / 值 / 是否清除」。
4. `*Filter` 结构体：query 用；`Option<T>` = 字段不参与过滤。
5. `Memory*Repo`：单元测试 / in-memory 单进程部署。
6. `Pg*Repo`：真实 Postgres 实现，使用 `sqlx::query_as`（**非宏**）避免编译时连接 DB。
7. 每个 Repo 的 in-memory 测试至少覆盖：create+get、duplicate conflict、cascade。

## M1 进度（workspace / member / auth）

- ✅ `0002_auth_and_invitations.up.sql`：新增 `workspace_share_link` 与
  `workspace_invitation` 状态机字段，加索引。
- ✅ `mc-repos::workspace` / `member` / `invitation` / `share_link` /
  `verification` / `pat` / `user`：双实现（in-memory + Postgres）。
- 🟡 `mc-http::routes::auth` / `workspace` / `member` / `invitation` /
  `share_link` / `pat`：路由入口 — 见 `mc-http/src/routes/`。

## Commit / PR

- 标题：conventional commits（`feat:` / `fix:` / `refactor:` / `docs:` / `test:` / `chore:`）。
- 单个 PR 一次只做一件事；500 行以上 PR 需要 review by louloulin。
- 任何 crate 顶层 API 改动需要更新 `docs/03-CRATE-MAPPING.md`。
