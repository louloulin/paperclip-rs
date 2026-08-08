# Design: paperclip-rs 全面复刻 + 真实启动验证（高内聚低耦合设计）

> 配套 `proposal.md`。每一节 = 一个 V 模块的"真实最佳方式"实现方式：分层、trait、错误约定、并发模型、测试策略、性能边界、真实运行验证。

---

## A. 通用设计原则（适用于 V1–V15 全部模块）

| 维度 | 约定 | 理由 |
|---|---|---|
| **错误** | `thiserror` 定义域错误；跨 crate 边界统一用 `pc_errors::AppError` + `From` 转换 | Rust 习惯；调用方易处理 |
| **ID** | 所有主键用 newtype：`CompanyId(Uuid)`、`IssueId(i64)` 等 | 编译期防混用；可显示 |
| **时间** | `chrono::DateTime<Utc>` 统一，DB 列用 `timestamptz` | 时区一致；与原 DB schema 对齐 |
| **异步** | `tokio` 多线程 + `tokio::task::spawn_blocking` 仅用于阻塞 IO/CPU | 与 axum/sqlx/hyper 生态对齐 |
| **取消** | 关键路径用 `tokio_util::sync::CancellationToken` 显式传播 | 显式背压；graceful shutdown |
| **配置** | `pc_config::AppConfig::from_env()` + clap 子命令双源 | 12-factor；CLI 友好 |
| **日志** | `pc_telemetry` 统一 `tracing` subscriber，结构化 JSON，业务字段走 `span` | 字段可索引；可重写 |
| **序列化** | `serde` + `#[serde(rename_all = "camelCase")]`（与 UI 一致） | 跨语言兼容；UI 直接消费 |
| **验证** | valico / jsonschema 仅边界 HTTP 入参，DB 层不重验证 | 单一来源；DB 信任自身 |
| **trait 抽象** | `Repository` / `Provider` / `Runtime` / `Bus` 按需；不预先抽 | YAGNI；编译期驱动 |
| **文件布局** | 每个 crate：`src/lib.rs` + 按子域分文件 + `tests/` 集成 | 高内聚 |
| **测试** | `#[tokio::test]` 单线程 + `tests/` 集成；DB 用 ephemeral PG（testcontainers） | 真实；可重复 |
| **依赖原则** | workspace deps 顶 `Cargo.toml`；不允许同一 crate 两个版本 | 编译一致 |
| **模块边界** | 跨 crate 仅暴露 `pub` 类型 + `pub trait`；不暴露 `pub fn` 内部实现细节 | 低耦合 |
| **OpenAPI 同步** | 路由函数 `#[utoipa::path]` 标注 → 自动生成 `/openapi.json` | 单一来源；零漂移 |
| **类型状态** | `Login { authenticated: PhantomData<()> }` 风格；不引入过深类型状态 | 平衡可读性 |
| **构建** | musl 静态链接单二进制（macOS / Linux glibc / Linux musl）三目标 | 单文件部署 |

---

## B. 模块设计（V1–V15 逐个）

### V1 — 真实基线验证

**目标**：让"pc-server 真的能起来 + 109 表 migrate + /health 200 + 5+ GET 200"成为日常可重复运行的基线。

**实现**：
1. `scripts/e2e-baseline.sh`：起临时 PG16（端口随机避免冲突） → `pc-migrate up` → 起 `pc-server`（端口随机）→ 等 `/health` 200 → curl 5+ GET → graceful shutdown
2. `scripts/dev-ui-rust.sh`：复用并加固 V11 后的版本
3. `crates/pc-migrate/src/main.rs` CLI：`up` / `down` / `status` / `create` / `verify` / `baseline` / `seed`
4. `apps/pc-server/src/main.rs` 启动序列：config → telemetry → migrate → router → bind → graceful shutdown

**验收**：
- `bash scripts/e2e-baseline.sh` 在干净 macOS + Linux（glibc/musl）双平台 exit 0
- 启动 WARN 0；路由冲突 0
- `curl /health` 5s 内 200

---

### V2 — CLI 全部子命令

**目标**：`pc-cli` 1,017 → ≥ 6,000 LOC，覆盖 19 个子命令，每个支持 `--help` + `--json`。

**实现**：
```rust
// apps/pc-cli/src/main.rs
#[derive(Parser)]
#[command(name = "paperclipai", version, about = "paperclip CLI")]
enum Cli {
    Run { /* … */ },
    Install { /* … */ },
    Onboard { /* … */ },
    Doctor { /* … */ },
    Worktree { /* … */ },
    HeartbeatRun { /* … */ },
    Pipelines { /* … */ },
    Routines { /* … */ },
    Service { /* … */ },
    Update { /* … */ },
    Configure { /* … */ },
    DbBackup { /* … */ },
    AuthBootstrapCeo { /* … */ },
    AllowedHostname { /* … */ },
    Env { /* … */ },
    EnvLab { /* … */ },
    Uninstall { /* … */ },
}
```

每个子命令：
- 一个文件 `apps/pc-cli/src/cmd/<name>.rs`
- `impl Cli { async fn run(self) -> Result<()> { … } }` 统一调度
- 错误用 `anyhow::Result` 统一包装
- 输出用 `serde_json::to_string_pretty` 走 `--json`

**验收**：
- 19 子命令每个 `--help` 输出与原 `paperclip/cli/src/commands/*.ts` 字段一致
- 每个 `--json` 输出可解析
- 至少 5 个子命令（run / install / doctor / onboard / db-backup）真实跑一遍

---

### V3 — OpenAPI 3.1 完整生成

**目标**：`pc-openapi` 480 → ≥ 3,000 LOC，utoipa derive + 完整 schema。

**实现**：
1. `pc-http` 每个路由函数加 `#[utoipa::path(method, path, request_body, responses)]`
2. `pc-core` 每个领域类型加 `#[derive(ToSchema)]`
3. `pc-openapi` 提供 `OpenApiRegistry::builder()` 注册 paths + schemas
4. `pc-http` 注册 `/openapi.json` 路由 + `/openapi.yaml` 路由
5. `apps/pc-server` 启动时打印 OpenAPI 路径数

**验收**：
- `/openapi.json` 返回 200 + 至少 56 path
- 与原 `server/src/routes/openapi.ts` 字段级 1:1（paths + components.schemas）
- `scripts/check-ui-contract.sh` 重合率 ≥ 99%

---

### V4 — OpenAPI ↔ UI 类型对齐

**目标**：UI 60 个 api client 字段全部与 Rust server OpenAPI 一致。

**实现**：
1. `ui/src/api/types.ts` 用 `openapi-typescript`（或手写 ts-rs 反向）从 `/openapi.json` 生成
2. 60 个 client 文件 `ui/src/api/<resource>.ts` 用生成的 types 替换手写
3. `scripts/check-ui-contract.sh`：跑 `openapi-typescript` + diff，失败 exit 1
4. CI：types 生成 + 60 client 文件 lint

**验收**：
- 60 client 文件全部用生成的 types
- `check-ui-contract.sh` exit 0
- UI 60 client 真实请求 fixture 全 200/合约拒绝

---

### V5 — Auth/AuthZ 完整化

**目标**：`pc-auth` 581 → ≥ 3,500 LOC；`pc-authz` 128 → ≥ 2,500 LOC。

**实现**：

**pc-auth**：
```rust
// crates/pc-auth/src/
// - session.rs: Session = { user_id, company_id, expires_at, csrf, refresh_expires_at }
// - cookie.rs: pc_session + pc_csrf cookie 设置/解析
// - csrf.rs: double-submit token 生成/校验
// - api_key.rs: pk_<base62> 26 字符生成/校验/吊销（hash 存 DB）
// - refresh.rs: 30d sliding window rotation（每次刷新换 token + 重置过期）
// - oauth.rs: Google + GitHub OAuth2 流程（state + PKCE）
// - password.rs: argon2id hash + verify
// - actor.rs: Actor = { kind: User | Agent, id, roles }
```

**pc-authz**：
```rust
// crates/pc-authz/src/
// - policy.rs: trait Policy<S, A, R> { fn check(&self, ctx: &AuthCtx, action: A, resource: R) -> PolicyResult; }
// - company_policy.rs: 5 mode × { company, member, logo, invite, join_request }
// - agent_policy.rs: 5 mode × { agent, config_revision, api_key, runtime_state, wakeup }
// - issue_policy.rs: 5 mode × { issue, comment, label, attachment, approval }
// - case_policy.rs / project_policy.rs / approval_policy.rs / decision_policy.rs / routine_policy.rs / pipeline_policy.rs
// - board_mutation_guard.rs: middleware 拦截 board user mutation
// - strategy_table.rs: 静态策略表 80+ case
```

**验收**：
- 80+ 集成测试覆盖 allow/deny/not_owner
- refresh rotation 单元测试
- OAuth state + PKCE 单测
- API key pk_<base62> 校验单测
- CSRF 缺失/错误/正确三态

---

### V6 — 路由字节级补全

**目标**：`pc-http` 44,702 → ≥ 50,000 LOC；companies 子路由 + /api/admin/* 补全。

**实现**：
1. `pc-http/src/routes/companies.rs` 补：
   - `GET /api/companies/:id/skills`
   - `GET /api/companies/:id/tools`
   - `GET /api/companies/:id/folders`
   - `GET /api/companies/:id/invites`
   - `GET /api/companies/:id/labels`
   - `GET /api/companies/:id/approvals`
   - `GET /api/companies/:id/org-svg.png`
   - `GET /api/companies/:id/join-requests`
2. `pc-http/src/routes/admin.rs` 新建：
   - `GET /api/admin/users`
   - `POST /api/admin/users/:id/role`
   - `GET /api/admin/audit-log`
   - `POST /api/admin/instance/maintenance`
   - `GET /api/admin/feature-flags`
3. `scripts/diff-routes.sh`：raw method+path 重合率 46.1% → ≥ 95%

**验收**：
- 14 个新路由每个 ≥ 1 happy + 3 edge
- diff-routes.sh 重合率 ≥ 95%

---

### V7 — Heartbeat stale lock 修复

**目标**：`pc-heartbeat::recovery` round300 4 个失败测试全过。

**实现**：
1. 复盘 Node `services/heartbeat.ts` `staleIssueLockSweep` 行为
2. 找到 Rust 端 4 个失败 case 的差异点
3. 修复 `pc_heartbeat::recovery::stale_issue_lock_sweep` 函数
4. 新增真实 PG 集成测试（不依赖 round*.rs）

**验收**：
- round300 4 个失败全过
- 新增 ≥ 5 个 stale lock sweep 集成测试

---

### V8 — 远程 execution target

**目标**：claude-local / codex-local 远程路径完整复刻。

**实现**：
- `pc-adapter-claude-local/src/remote.rs`：`restoreRemoteWorkspace` / `materializeRemoteClaudeConfig` / `startAdapterExecutionTargetPaperclipBridge`
- `pc-adapter-codex-local/src/remote.rs`：`stagedCodexHomeDir` teardown + `restoreRemoteWorkspace` + `remoteCodexConfigDir` 决策
- `pc-adapter-process/src/ssh_bridge.rs`：mock SSH server
- 集成测试：起 mock SSH server，跑通 start → materialize → invoke → restore 全链路

**验收**：
- mock SSH server fixture 测试全过
- 增量 +20 集成测试

---

### V9 — Workflow + Cron 真实链路

**目标**：`pc-workflow` 1,358 → ≥ 4,000 LOC；`pc-cron` 840 → ≥ 1,500 LOC；端到端验证。

**实现**：
- `pc-cron/src/parser.rs` 完整 cron 表达式解析（含 `?` `L` `W` `#`）
- `pc-cron/src/scheduler.rs` tokio-cron-scheduler 集成
- `pc-workflow/src/routine.rs`：Routine 定义 + 触发 + 重试 + 失败告警
- `pc-workflow/src/pipeline.rs`：DAG 解析 + 拓扑执行 + step 失败中断
- `pc-workflow/src/triggers.rs`：cron + webhook + manual 三种触发
- 集成测试：cron 表达式 + 真实触发 routine + pipeline step 失败中断

**验收**：
- 真实 cron 触发 routine 通过
- pipeline step 失败中断下游
- ≥ 20 集成测试

---

### V10 — Plugin 互操作

**目标**：`pc-plugin-host` 4,986 → ≥ 8,000 LOC；与原 SDK worker JSON-RPC 互操作。

**实现**：
- `pc-plugin-host/src/interop.rs`：与原 `@paperclipai/plugin-sdk` worker 真实握手
- `pc-plugin-host/src/event_bus.rs`：完整事件总线（plugin_event_bus 已有）
- `pc-plugin-host/src/job_scheduler.rs`：cron + 手动
- `pc-plugin-host/src/job_store.rs`：plugin_jobs 持久化
- `pc-plugin-host/src/tool_dispatcher.rs`：工具注册 + 调用
- `pc-plugin-host/src/database_bridge.rs`：受限 DB 视图（plugin_database）
- `pc-plugin-host/src/state_store.rs`：plugin_state 读写
- `pc-plugin-host/src/webhook_dispatcher.rs`：plugin_webhooks 发送
- `pc-plugin-host/src/manifest_validator.rs` + `capability_validator.rs`

**验收**：
- 与原 SDK worker 互操作通过
- 从加载 → 注册事件 → 触发作业端到端

---

### V11 — UI 60 client 全 happy path

**目标**：60 个 api client 每个真实请求 fixture 一次，全部 200/合约拒绝。

**实现**：
- `scripts/ui-happy-path.sh`：起临时 PG → pc-migrate up → pc-server → UI dev server (VITE_API_BASE) → curl 60 个 client endpoint
- 失败：输出缺失的 client 名 + 实际响应
- 报告：60/60 pass / 0 fail

**验收**：
- 60 client 全部 200/合约拒绝
- 失败截图 + 视频存档

---

### V12 — Playwright 真实 UI 剧本

**目标**：登录 → 公司 → issue → heartbeat → live-event 整剧本。

**实现**：
- `tests/e2e/full-stack-ui.spec.ts`：Playwright 跑：
  1. 打开 UI 登录页
  2. 注册 + 自动登录
  3. 创建公司
  4. 创建 issue
  5. 启动 heartbeat
  6. WS 收到 `heartbeat.run.completed`
- `scripts/e2e-full-stack.sh`：CI 用，三态平台
- 失败截图 + 视频存档

**验收**：
- 整剧本 60s 内通过
- macOS + Linux glibc/musl 三态均通过

---

### V13 — 真实长跑 + 性能基线

**目标**：5 分钟 heartbeat 跑 + WS 推流无回归；性能对比 Node server。

**实现**：
- `scripts/long-run-5min.sh`：起 PG → migrate → server → 触发 10 个 heartbeat → 等 5 分钟 → 校验 WS 事件数
- `benches/http_routes.rs`：criterion bench 主要路由
- `scripts/wrk-compare.sh`：同时跑 Node server + Rust server，wrk 压 60s，对比 P99 / RSS / CPU

**验收**：
- 5 分钟长跑零回归
- P99 ↓ ≥ 30%、RSS ↓ ≥ 40%（与 Node 对比）

---

### V14 — 真实迁移（109 → 172 表 patch）

**目标**：`crates/pc-db/migrations/` 衍生表 patch 注释 + 启动无 schema diff warning。

**实现**：
- 审计 109 → 172 差异（63 张衍生表的来源）
- 每个 patch SQL 文件加注释（"这张表来自 Node paperclip migration NNN"）
- `apps/pc-server` 启动时跑 schema diff，零 warning

**验收**：
- 启动 WARN 0
- 172 表 schema 注释完整

---

### V15 — 中文文档与移交

**目标**：中文说明完整；新开发者 30 分钟可上手。

**实现**：
- `paperclip-rs/AGENTS.md`（中文）：仓库结构、构建、运行、测试
- `paperclip-rs/OPERATIONS.md`（中文）：部署、备份、监控、故障恢复
- `paperclip-rs/PLUGIN_AUTHORING.md`（中文）：插件作者指南（注册、事件、工具、状态、webhook）
- `paperclip-rs/MIGRATION_FROM_NODE.md`（中文）：从 Node server 迁移指南（端口、环境变量、DB、备份）

**验收**：
- 新开发者 30 分钟可上手（亲自跑过 `git clone` → `cargo build` → `cargo run` → 登录 → 建公司 → 建 issue）

---

## C. 跨模块协同

### C.1 trait 抽象边界

```rust
// pc-core/src/lib.rs
pub trait Repository: Send + Sync {
    type Id;
    type Entity;
    type NewEntity;
    type Patch;
    type Query;
    type Error: std::error::Error + Send + Sync + 'static;

    async fn insert(&self, conn: &mut PgConn, new: Self::NewEntity) -> Result<Self::Entity, Self::Error>;
    async fn get(&self, conn: &mut PgConn, id: Self::Id) -> Result<Option<Self::Entity>, Self::Error>;
    async fn list(&self, conn: &mut PgConn, query: Self::Query) -> Result<Vec<Self::Entity>, Self::Error>;
    async fn update(&self, conn: &mut PgConn, id: Self::Id, patch: Self::Patch) -> Result<Self::Entity, Self::Error>;
    async fn delete(&self, conn: &mut PgConn, id: Self::Id) -> Result<(), Self::Error>;
}
```

```rust
// pc-storage/src/lib.rs
#[async_trait]
pub trait StorageProvider: Send + Sync {
    async fn put(&self, key: &Key, data: Bytes, opts: PutOpts) -> Result<ObjectMeta>;
    async fn get(&self, key: &Key) -> Result<Bytes>;
    async fn head(&self, key: &Key) -> Result<ObjectMeta>;
    async fn delete(&self, key: &Key) -> Result<()>;
    async fn presign_get(&self, key: &Key, ttl: Duration) -> Result<Url>;
    async fn list(&self, prefix: &Key) -> Result<Stream<ObjectMeta>>;
}
```

```rust
// pc-secrets/src/lib.rs
#[async_trait]
pub trait SecretsProvider: Send + Sync {
    async fn get(&self, key: &str) -> Result<Secret>;
    async fn put(&self, key: &str, value: &Secret) -> Result<()>;
    async fn list(&self) -> Result<Vec<String>>;
    async fn delete(&self, key: &str) -> Result<()>;
}
```

```rust
// pc-realtime/src/lib.rs
#[async_trait]
pub trait Bus: Send + Sync {
    async fn publish(&self, event: LiveEvent) -> Result<EventId>;
    fn subscribe(&self, filter: EventFilter) -> BoxStream<'static, LiveEvent>;
    async fn replay(&self, after: EventId, limit: usize) -> Result<Vec<LiveEvent>>;
}
```

```rust
// pc-adapter-api/src/lib.rs
#[async_trait]
pub trait AdapterRuntime: Send + Sync {
    fn descriptor(&self) -> AdapterDescriptor;
    async fn invoke(&self, ctx: InvokeContext) -> Result<InvokeResult>;
    async fn cancel(&self, run_id: RunId) -> Result<()>;
}
```

### C.2 错误约定

```rust
// pc-errors/src/lib.rs
#[derive(Debug, Error)]
pub enum AppError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("unauthorized")]
    Unauthorized,
    #[error("validation: {0}")]
    Validation(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("rate limited")]
    RateLimited,
    #[error("internal: {0}")]
    Internal(#[source] anyhow::Error),
    #[error("upstream: {0}")]
    Upstream(String),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Http(#[from] axum::http::Error),
}

// 映射
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, body) = match self {
            AppError::NotFound(_) => (StatusCode::NOT_FOUND, ...),
            AppError::Forbidden(_) => (StatusCode::FORBIDDEN, ...),
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, ...),
            // ...
        };
        (status, Json(body)).into_response()
    }
}
```

### C.3 真实运行验证（每个 V 模块必须）

```bash
# 1. 单元 + 集成测试
cargo test -p <crate> --tests

# 2. clippy
cargo clippy -p <crate> -- -D warnings

# 3. 格式
cargo fmt -p <crate> --check

# 4. e2e 基线回归
bash scripts/e2e-baseline.sh

# 5. 模块专属真实运行
# - V1: 临时 PG + pc-server + curl /health + 5 GET
# - V2: pc-cli run --help + pc-cli doctor
# - V3: curl /openapi.json
# - V4: check-ui-contract.sh
# - V5: 登录 / 刷新 / OAuth / CSRF / API key
# - V6: 14 个新路由
# - V7: round300 + 5 stale lock
# - V8: mock SSH
# - V9: cron + pipeline
# - V10: 加载 plugin
# - V11: 60 client
# - V12: Playwright
# - V13: 5min + wrk
# - V14: pc-migrate up + schema diff
# - V15: 文档审阅

# 6. 写 evidence
# openspec/changes/paperclip-rs-comprehensive-validation/evidence/<module>.md
```

---

## D. 关键决策（已选）

| # | 决策 | 替代 | 选定理由 |
|---|---|---|---|
| D1 | 复用 `paperclip-rs-modules-replica` 成果 | 从零开始 | 80% 已完成，节省 17 周 |
| D2 | 新建 `paperclip-rs-comprehensive-validation` change | 继续 modules-replica | modules-replica 在 design 阶段，scope 需明确化 |
| D3 | 继续用 `tokio` + `axum` + `sqlx` + `utoipa` | actix-web / sea-orm | 与现有 crates 完全对齐 |
| D4 | 真实 PG（testcontainers）做 e2e | mock 内存 | 与生产一致；schema 真实 |
| D5 | Playwright + 真 UI 剧本 | 仅 API 合约 | 用户硬目标："真实启动前后端验证" |
| D6 | wrk 压测 Node vs Rust | criterion only | 给性能声明提供依据 |
| D7 | 中文为主文档 | 英文 | 用户明确要求中文 |
| D8 | 真实长跑 5min | 单次心跳 | 暴露 stale lock / WS 推流稳定性 |

## E. 风险与缓解

| 风险 | 概率 | 影响 | 缓解 |
|---|---|---|---|
| V4 OpenAPI ↔ UI 对齐耗时超预期 | 中 | 阻塞 V12 | 用 openapi-typescript 自动化；ci 阻断 |
| V8 mock SSH 与真实 SSH 差异 | 中 | 远程不可用 | mock 覆盖 80% 场景；真实 SSH 留 follow-up |
| V13 性能未达 P99 ↓ 30% | 中 | 性能声明 | 调优（连接池 / batch / 索引）；不达就标注 |
| V12 Playwright 在 CI 慢 | 高 | 反馈环长 | 5min 超时；失败快速 fail |
| V15 文档维护负担 | 低 | 社区体验 | 文档随 code 同步更新；CI 校验链接 |
