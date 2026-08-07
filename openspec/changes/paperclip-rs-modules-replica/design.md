# Design: paperclip-rs Modules Replica

> 配套 proposal.md。每一节 = 一个模块的"真实最佳方式"实现方式：分层、trait、错误约定、并发模型、测试策略、性能边界。

---

## 通用设计原则（适用于所有 M1–M16 + U1–U3）

| 维度 | 约定 |
|---|---|
| 错误 | `thiserror` 定义域错误；跨 crate 边界统一用 `pc_errors::AppError` + From 转换 |
| ID | 所有主键用 newtype 强类型：`CompanyId(Uuid)`、`IssueId(i64)` etc. |
| 时间 | `chrono::DateTime<Utc>` 统一，DB 列用 timestamptz |
| 异步 | tokio 多线程 + `tokio::task::spawn_blocking` 仅用于阻塞 IO/CPU |
| 取消 | 关键路径用 `tokio_util::sync::CancellationToken` 显式传播 |
| 配置 | `pc_config::AppConfig::from_env()` + clap 子命令双源 |
| 日志 | `pc_telemetry` 统一 tracing subscriber，结构化 JSON，业务字段走 span |
| 序列化 | serde + `#[serde(rename_all = "camelCase")]`（与 UI 一致） |
| 验证 | valico / jsonschema 仅边界 HTTP 入参，DB 层不重验证 |
| trait 抽象 | `Repository` / `Provider` / `Runtime` / `Bus` 按需；不预先抽 |
| 文件布局 | 每个 crate：`src/lib.rs` + 按子域分文件 (`company.rs`, `agent.rs`...) + `tests/` 放集成 |
| 测试 | `#[tokio::test]` 单线程 + `tests/` 集成；DB 用 ephemeral PG（testcontainers） |
| 依赖原则 | workspace deps 顶 `Cargo.toml`；不允许同一 crate 两个版本 |

---

## M1 — apps/ 目录契约

**目标**：把 `crates/pc-server`、`crates/pc-cli` 物理独立到 `apps/` 下。零行为改动。

**实现**：
1. `mkdir apps/pc-server apps/pc-cli`
2. `mv crates/pc-server/* apps/pc-server/`；`crates/pc-server/Cargo.toml` 路径调整
3. `mv crates/pc-cli/* apps/pc-cli/`
4. 根 `Cargo.toml`：删除旧 member，加新 member `apps/pc-server`、`apps/pc-cli`
5. 修正 `pc-server` 依赖：原来 `path = "../crates/pc-X"` 改为 `path = "../crates/pc-X"`（相对 path 不变，但要确认相对目录深度正确）
6. CI：保留所有 workflow，验证 `cargo check --workspace`

**验收**：`cargo build -p pc-server` 仍成功；`cargo run -p pc-server -- --help` 输出与改动前一致。

---

## M2 — E2E 基线

**目标**：让 `pc-server` 真的能起来，提供一组 baseline 校验脚本。后续每个模块用这个脚本做回归。

**实现**：
1. `apps/pc-server` 默认 `--db-url` 可走外部 PG（testcontainers-rs 自动起一个 PG 16）
2. `apps/pc-server` 启动后调用 `pc_migrate::run_up()` 自跑迁移
3. `/health` 简单返回 `{"status":"ok"}`，DB ping
4. `/ready` 在 migrate 完成后才返回 200
5. `apps/pc-server` 接受 `TRUST_PROXY` / `STORAGE_KIND` / `SECRETS_KIND` 等环境变量
6. 引入 `scripts/e2e-baseline.sh`：
   - 起外部 PG（docker 或 testcontainers）
   - `pc-migrate up`
   - `pc-server &`
   - 等 `/health` 200
   - `curl -fsS POST /api/auth/sign-in`（最小）
   - 关停

**验收**：在干净 macOS + Linux（glibc/musl）都能跑通 0-200 不报错。

---

## M3 — Storage 真实链路

**目标**：`pc-storage` 的 `StorageProvider` trait + `local_disk` + `s3` 真实 SDK 接入。

**trait 设计**：
```rust
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

**实现**：
- `local_disk`: tokio fs + SHA-256 + 文件锁
- `s3`: `aws-sdk-s3` + 自动 retry + multipart（≥8MB 触发）
- `provider-registry`: 根据 `STORAGE_KIND` 选实现
- 错误转换：`StorageError → AppError::Storage`

**验收**：集成测试 put/get/lifecycle + presign + multipart；aws-sdk 走 localstack。

---

## M4 — Secrets 真实链路

**目标**：`pc-secrets` AES-256-GCM 加解密 + aws-secrets-manager + provider 链。

**trait**：
```rust
#[async_trait]
pub trait SecretsProvider: Send + Sync {
    async fn get(&self, key: &str) -> Result<Secret>;
    async fn put(&self, key: &str, value: &Secret) -> Result<()>;
    async fn list(&self) -> Result<Vec<String>>;
}
```

**实现**：
- `local_encrypted`: 主密钥来自 `PC_MASTER_KEY`（base64 32-byte），AES-256-GCM+随机 nonce+版本号 salt
- `aws_sm`: `aws-sdk-secretsmanager`，含 KMS 可选
- `configured_provider`: 多 provider 链（含 fallback / 灰度）
- 类型：`Secret = Zeroizing<Vec<u8>>` 防日志泄露

**验收**：加解密 roundtrip；provider 切换；密钥泄露到 logs 必须 0。

---

## M5 — Backup

**pg_dump / pg_restore 一致性**
- `pc-backup::dump(db_url, dir) -> Backup`: tokio::process 调 `pg_dump --format=custom`
- `restore(backup, db_url)`: 调 `pg_restore --clean --if-exists`
- 备份清单：SQL DDL 不可变 + 实际 sha256 + 时间戳 + size
- retention: cron-like（`pc-workflow` 调）

**验收**：真实 PG dump → restore，比对两侧 row count + checksum。

---

## M6 — Migrate 工具

**`pc-migrate` sub-commands**: `up`、`down`、`status`、`create`、`verify`、`baseline`、`seed`
- 同 `pc-db::SqlxMigrator` 包装
- 输出 `--json` 可解析
- 安全：dry-run + lock 文件

---

## M7 — Auth + AuthZ

**pc-auth**：
- `Session = { user_id, company_id, expires_at, csrf }`
- Cookie: `pc_session` + `pc_csrf`
- API Key: `Bearer pk_<base62>`
- actor 中间件：`axum::middleware::from_fn_with_state`

**pc-authz**：
```rust
pub trait Policy {
    fn check(&self, ctx: &AuthCtx, action: &Action, resource: &Resource) -> PolicyResult;
}
```
- 每个 service 调用前 `authz.check(ctx, action, resource)`；不通过 → AppError::Forbidden
- 策略表原样迁：基于 `auth/authorization.ts` 的 5 个 mode × N resource

**验收**：80+ 集成测试覆盖（每个 resource × allow/deny/not_owner）

---

## M8 — DB Schema + Repos 25 子模块

**DDL**：1.3.2（已存在于 `pc-db::migrations/`）。所有 109 张表 → Postgres DDL 文件 `crates/pc-db/migrations/0001NN_*.sql`。

**Repos**：按表主题分文件：
```
crates/pc-repos/src/
├── lib.rs
├── company.rs         # companies / memberships / logos
├── agent.rs           # agents / memberships / api_keys / config_revisions
├── issue.rs           # issues / comments / approvals / assignments
├── case.rs            # cases
├── project.rs         # projects
├── approval.rs        # approvals / approval_comments
├── decision.rs        # decisions / decision_training_examples
├── routine.rs         # routines
├── pipeline.rs        # pipelines / pipeline_steps / pipeline_runs
├── environment.rs     # environments / environment_secrets / custom_images
├── execution.rs       # execution_workspaces / workspace_files / operation_log
├── heartbeat.rs       # heartbeat_runs / wakeup_requests / runtime_state
├── plugin.rs          # plugin_installs / plugin_secrets / plugin_jobs
├── auth.rs            # sessions / api_keys / login_state
├── activity.rs        # activity_log
├── document.rs        # documents / document_annotations / anchors
├── goal.rs            # goals
├── folder.rs          # folders
├── sidebar.rs         # sidebar_preferences / sidebar_badges
├── inbox.rs           # inbox_dismissals / inbox_agent_policies
├── summary.rs         # summary_slots
├── tool.rs            # tool_access_policies / tool_runtime_metrics
├── smoke.rs           # smoke_tests
├── settings.rs        # instance_settings / company_settings
├── skill.rs           # company_skills / skill_catalog
└── ...                # 其余按 schema 表主题补
```

每个 repo 模块提供：
```rust
pub trait CompanyRepo: Send + Sync {
    fn insert(&self, conn: &mut PgConn, c: NewCompany) -> Result<Company>;
    fn get(&self, conn: &mut PgConn, id: CompanyId) -> Result<Company>;
    fn list(&self, conn: &mut PgConn, q: CompanyQuery) -> Result<Vec<Company>>;
    fn update(&self, conn: &mut PgConn, id: CompanyId, patch: CompanyPatch) -> Result<Company>;
    fn delete(&self, conn: &mut PgConn, id: CompanyId) -> Result<()>;
}
```
- sqlx Postgres backend
- 错误统一 `RepoError -> AppError::Repo`
- 单元 + 集成测试（testcontainers PG）

**验收**：每个 repo 子模块 ≥ 3 happy + ≥ 1 edge case，全 25 子模块。

---

## M9 — HTTP 路由 56

**分层**：
```
crates/pc-http/src/
├── lib.rs                     # Router 组装
├── error.rs                   # 错误 → HTTP StatusCode
├── state.rs                   # AppState { db, bus, providers, config }
├── routes/                    # 每文件 = 一个原 routes/*.ts
│   ├── companies.rs
│   ├── agents.rs
│   ├── issues.rs
│   └── ...
├── middleware/
│   ├── actor.rs               # 解析 session/api_key
│   ├── request_id.rs
│   ├── log.rs
│   ├── cors.rs
│   ├── compression.rs
│   └── body_limits.rs
└── extract/                   # 自定义 Json<T>、ValidatedJson<T>、AuthUser
```

**axum 0.7 模式**：
- 路由 closure 接受 `State<AppState>` + 自定义 extractor
- handler 内最多 3 层（auth → repo → render）
- 业务错误用 `Result<Json<T>, AppError>`，axum 自动转

**验收**：每个路由 ≥ happy + 3 edge，与 Node 同 fixture 下字节级一致。

---

## M10 — OpenAPI 3.1

**`pc-openapi`**：
- `utoipa` 从 handler derive OpenAPI
- 输出 `/openapi.json` + `/openapi.yaml`
- 字段命名 snake → camel

**验收**：与 Node `routes/openapi.ts` 产物字段一致；UI 用同一 spec 验证。

---

## M11 — Realtime + WS

**`pc-realtime`**：
```rust
pub trait Bus: Send + Sync {
    fn publish(&self, evt: LiveEvent) -> Result<()>;
    fn subscribe(&self) -> BoxStream<'static, LiveEvent>;
}
```
- `InMemoryBus`: `tokio::sync::broadcast(1024)`，慢消费者丢事件不阻塞

**`pc-ws`**：
- 路径：`/api/live-events`
- 协议：subscribe 时带 last-event-id，bus.seek 回放；30s 心跳 ping
- token 校验：同 cookie 或 api key

**验收**：开 WSCat 客户端收 1 条 live-event。

---

## M12 — Heartbeat 状态机

**`pc-heartbeat`**：
- 已经 52k 行 Rust，大致覆盖，但对照 Node `services/heartbeat.ts` + `recovery/*` 列表查缺补漏
- 关键路径：`tick → pick_runnable → invoke_adapter → finalize`
- 状态机用 kameo / 自实现 enum + transition table
- 测试：recovery round320–357 已大量存在，逐一跑

**验收**：端到端剧本（已有 `tests/round*.rs`），按现状 0 失败。

---

## M13 — Adapter-API + 11 适配器

**`pc-adapter-api`**：
```rust
#[async_trait]
pub trait AdapterRuntime: Send + Sync {
    fn kind(&self) -> AdapterKind;
    async fn invoke(&self, req: InvokeRequest) -> Result<InvokeHandle>;
    fn stream(&self, handle: InvokeHandle) -> BoxStream<'static, AdapterEvent>;
    async fn cancel(&self, handle: InvokeHandle) -> Result<()>;
}
```
- 子进程走 `pc-adapter-process`：`tokio::process` + JSON-RPC over stdio
- HTTP 走 reqwest + SSE
- 事件类型：`AdapterEvent::Stdout | AdapterEvent::ToolCall | AdapterEvent::ToolResult | AdapterEvent::Done | AdapterEvent::Error`

**11 适配器 crate**：
- `pc-adapter-claude-local` ≥ 8k 行：完整 acpx 引擎 + billing + sandbox + redaction + git sync + remote + progress
- 其余按 Node 体积同比例实现
- 每个 crate ≥ 1 happy + ≥ 1 failure 集成测试

**验收**：每 crate 全跑过。

---

## M14 — Plugin Host

**`pc-plugin-protocol`**：JSON-RPC 2.0 schema 完整覆盖 `define-plugin.ts` 的所有方法。

**`pc-plugin-host`**：
- `WorkerPool`: tokio 进程池；每插件 1 worker，可扩
- `EventBus` / `JobScheduler` / `JobStore` / `ToolDispatcher` / `DatabaseBridge` / `StateStore` / `WebhookDispatcher` / `ManifestValidator` / `CapabilityValidator` 对应 Node 9 个 service

**验收**：与原 SDK worker 互操作通过（同一个 plugin 包，Rust host 启动能跑到 invoke 一次）。

---

## M15 — Workflow + Cron

**`pc-cron`**：`croner`-style parser；`next_after(cron, from)` 纯函数。

**`pc-workflow`**：
- Routine: 间隔 + 触发条件；Pipeline: 步骤 DAG
- `tokio-cron-scheduler` 触发器；从 DB 读 enabled routines/pipelines 拉起 tick
- 与 heartbeat 分工：heartbeat 调度 issue run；workflow 调度 routine/pipeline

**验收**：cron 表达式计算 + 一次 routine 被按时触发 + pipeline 一个 step 失败正确中断。

---

## M16 — CLI 全子命令

19 个 Node 子命令 1:1 对应：

| Node | Rust |
|---|---|
| `run` | `run` |
| `install` | `install` |
| `onboard` | `onboard` |
| `doctor` | `doctor` |
| `worktree` | `worktree` + `worktree-lib` |
| `heartbeat-run` | `heartbeat-run` |
| `pipelines` | `pipelines` |
| `routines` | `routines` |
| `service` | `service` |
| `update` | `update` |
| `configure` | `configure` |
| `db-backup` | `db-backup` |
| `auth-bootstrap-ceo` | `auth-bootstrap-ceo` |
| `allowed-hostname` | `allowed-hostname` |
| `env` / `env-lab` | `env` / `env-lab` |
| `uninstall` | `uninstall` |

`clap` v4 derive，每个子命令 support `--json`，输出 schema 与 Node 命令一致。

**验收**：每个子命令 `--help` + 真跑 0 错。

---

## U1 — UI 切流与契约冻结

- `paperclip-rs/ui/` 是从 `paperclip/ui/` 直接 `git checkout` 的 checkout（不修改源文件）
- UI 默认 `VITE_API_BASE=http://localhost:3100`
- 30 个 api client happy path 全过

---

## U2 — Playwright e2e

`tests/e2e/` 真实：
1. 启动 PG / 起 pc-server
2. 启动 UI（Vite preview）
3. Playwright：登录 → 创建公司 → 创建 issue → 启动 heartbeat → 收 live-event

---

## U3 — OpenAPI ↔ UI 类型对齐

- 服务端生成 `openapi.json`；客户端 ts-rs 反向
- UI 的 60 个 api client 文件签名与 OpenAPI 对齐

---

## 跨模块验证基线

每个模块完成 = 三件事齐全：
1. **实现**：Rust 源码落到 `paperclip-rs/<target>`，高内聚低耦合
2. **构建通过**：`cargo clippy --workspace -- -D warnings`，`cargo fmt --check`
3. **真实验证**：回归 `scripts/e2e-baseline.sh` + 模块自身的 happy + ≥3 edge case

不通过的模块不算"完成"，继续迭代直至真实验证绿。
