# Paperclip-rs 开发指南（AGENTS.md）

> R587 / 2026-08-12
> 范围：仓库结构 / 构建 / 运行 / 测试 / 开发规范
> 配套：`ARCHITECTURE.md` / `OPERATIONS.md` / `PLUGIN_AUTHORING.md` / `MIGRATION_FROM_NODE.md`

## 1. 仓库结构

```
paperclip-rs/
├── apps/
│   ├── pc-server/         # 主服务入口（migrate → router → bind）
│   └── pc-cli/            # paperclipai 二进制（19 子命令）
├── crates/                # 101 个 crate
│   ├── 基础层 (8)
│   │   ├── pc-errors pc-core pc-config pc-db
│   │   ├── pc-telemetry pc-storage pc-backup pc-migrate
│   ├── 工具层 (10+)
│   │   ├── pc-github-fetch pc-github-external-objects
│   │   ├── pc-log-redaction pc-secret-redaction
│   │   └── ... (其他工具)
│   ├── 域层 (~30)
│   │   ├── pc-repos (80+ 子模块) pc-decisions pc-routines
│   │   ├── pc-pipelines pc-issues pc-companies ...
│   ├── 适配器层 (13)
│   │   └── pc-adapter-{claude,codex,cursor,...}-local
│   ├── 插件层 (4)
│   │   └── pc-plugin-{host,protocol,state-store,ui-static}
│   ├── HTTP 层 (1)
│   │   └── pc-http (74 文件 / 56 路由)
│   └── 边角 (~15)
├── ui/                    # 复用 paperclip/ui/（React）
├── openspec/              # OpenSpec 变更管理
│   └── changes/
│       └── paperclip-rs-comprehensive-validation/
│           ├── progress-snapshot.md
│           ├── tasks.md
│           └── evidence/  # R{472-587}-*.md
├── scripts/               # 运维脚本
│   ├── e2e-baseline.sh    # PG + migrate + server 冒烟
│   ├── v11-ui-happy-path.sh # 60 client happy path
│   ├── dev-ui-rust.sh     # Vite dev server + Rust
│   └── ...
├── tests/e2e/             # Playwright specs
└── docs/                  # 历史变更日志 (100+ docs)
```

## 2. 快速上手

### 2.1 准备开发环境

```bash
# 1. 安装 Rust（推荐 rustup）
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env

# 2. 安装 PostgreSQL 16（macOS）
brew install postgresql@16

# 3. 安装 Node.js 20+（UI）
brew install node@20

# 4. 安装 pnpm（UI 依赖）
npm install -g pnpm

# 5. 克隆仓库
git clone https://github.com/your-org/paperclip-rs.git
cd paperclip-rs
```

### 2.2 构建

```bash
# 全 workspace 编译
cargo build

# 单 crate
cargo build -p pc-http

# 发布版
cargo build --release -p pc-server -p pc-cli -p pc-migrate

# Clippy（严格）
cargo clippy --workspace --all-targets -- -D warnings
```

### 2.3 启动开发 server

```bash
# 1. 启动 PG
brew services start postgresql@16
createdb paperclip_dev

# 2. 跑迁移
cargo run -p pc-migrate -- up

# 3. 启动 server（开发模式）
PAPERCLIP_DATABASE_URL=postgres://localhost/paperclip_dev \
  PAPERCLIP_RUN_MODE=development \
  RUST_LOG=paperclip=debug \
  cargo run -p pc-server

# 4. 或 Vite dev server + Rust 后端（dev-ui-rust.sh）
bash scripts/dev-ui-rust.sh
```

### 2.4 测试

```bash
# 所有 lib 测试
cargo test --workspace --lib

# 单 crate
cargo test -p pc-heartbeat

# 集成测试
cargo test -p pc-http --tests

# E2E baseline（PG + migrate + server + curl）
bash scripts/e2e-baseline.sh

# V11 UI 60 client happy path
bash scripts/v11-ui-happy-path.sh

# E2E full stack（Vite + Playwright）
bash scripts/e2e-full-stack.sh
```

## 3. 开发规范

### 3.1 代码风格

- **Rust edition 2021**
- **`forbid(unsafe_code)`** 在 workspace 级别强制
- **命名**：snake_case (Rust) ↔ camelCase (JSON via serde)
- **模块文档**：每个模块顶部 `//!` 注释，对齐 Node 上游
- **公开函数**：必须有中文 doc comment

### 3.2 测试要求

每个新功能必须有：
1. ✅ 单元测试（happy + ≥3 edge case）
2. ✅ 集成测试（真实 PG / 真实 HTTP / 真实 WS）
3. ✅ DoD 验证（`cargo clippy -D warnings` + `cargo test` + `e2e-baseline.sh`）

### 3.3 提交规范

```bash
# Commit 格式
git commit -m "R### 短描述

详细说明：
- 改动 1
- 改动 2

测试：
- 新增 N 个测试
- 修复的 bug

证据：openspec/.../r###-xxx.md"

# 不直接 commit 到 main
git checkout -b feature/r###-xxx
```

### 3.4 模块设计原则

#### 高内聚低耦合

每个 crate 单一职责：
- `pc-heartbeat` 只管心跳
- `pc-issues` 只管 issue CRUD
- `pc-adapter-claude-local` 只管 Claude adapter

crate 间通过 trait 交互，不直接依赖实现。

#### 决策函数与 I/O 分离

每个 crate 都遵循：
- `*_decision.rs` / `*_pure.rs` — 纯函数（无 I/O）
- `*_db.rs` / `*_repo.rs` — DB I/O
- `*_http.rs` / `*_api.rs` — HTTP handlers

#### 错误处理

- 用 `thiserror` 定义 crate 级错误
- 错误向上传时显式 map（`#[from]`）
- HTTP 层统一转 `ApiError`（`pc-http::ApiError`）

#### 异步运行时

- 全 tokio
- 不阻塞 runtime（DB I/O 用 sqlx async）
- 测试用 `#[tokio::test(flavor = "current_thread")]`

## 4. 常见任务

### 4.1 加新 HTTP 路由

```rust
// 1. 在 crates/pc-http/src/routes/<resource>.rs 添加 handler
pub async fn list_foos(State(state): State<AppState>) -> ApiResult<Json<Vec<Foo>>> {
    let foos = FooRepo::list(&state.db).await?;
    Ok(Json(foos))
}

// 2. 注册路由
pub fn router() -> Router<AppState> {
    Router::new().route("/api/foos", get(list_foos))
}

// 3. 在 mod.rs 注册
pub mod foos;

// 4. 添加 OpenAPI hint
#[path = "openapi.rs"]
mod openapi;
// 在 openapi.rs 里 path_schema_hint!("/api/foos", ...)

// 5. 添加测试
// crates/pc-http/tests/r###_foos.rs
```

### 4.2 加新 DB 表

```bash
# 1. 在 crates/pc-migrate/migrations/ 加新 SQL 文件
ls crates/pc-migrate/migrations/ | tail -1
# 应该是 0001_init.sql 之类
# 加新文件：0017_add_foo_table.sql

# 2. 写 SQL
# 0017_add_foo_table.sql
CREATE TABLE foo (...);

# 3. 应用迁移
cargo run -p pc-migrate -- up

# 4. 在 pc-repos 加 repo
// crates/pc-repos/src/foo.rs
pub struct FooRepo;
impl FooRepo {
    pub async fn list(db: &Db) -> Result<Vec<Foo>> { ... }
    pub async fn create(db: &Db, ...) -> Result<Foo> { ... }
}
```

### 4.3 加新 adapter

参考 `pc-adapter-claude-local`（最完整）。

```bash
# 1. 复制模板
cp -r crates/pc-adapter-claude-local crates/pc-adapter-XXX-local

# 2. 改名（Cargo.toml + lib.rs + module names）

# 3. 在 apps/pc-server/src/main.rs 注册
use pc_adapter_xxx_local::XxxLocalAdapter;
let xxx = XxxLocalAdapter::new();
registry.register(Arc::new(xxx));

# 4. 在 pc-adapter-type 添加 ADAPTER_TYPE 常量
```

### 4.4 加新 plugin IPC 方法

```rust
// 1. 在 crates/pc-plugin-protocol/src/methods.rs
pub const WORKER_NEW_METHOD: &str = "worker/new-method";

// 2. 在 crates/pc-plugin-protocol/src/types.rs 加参数/返回类型
#[derive(Serialize, Deserialize)]
pub struct NewMethodParams { ... }
pub struct NewMethodResult { ... }

// 3. 在 crates/pc-plugin-host/src/registry.rs 加 handler
match method {
    WORKER_NEW_METHOD => handle_new_method(...),
    ...
}

// 4. 加集成测试
// crates/pc-plugin-host/tests/r###_new_method.rs
```

## 5. 测试基础设施

### 5.1 测试 PG

- 默认：`postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos`
- 需要运行：PG 16 server，user `paperclip` / password `paperclip`，DB `paperclip_repos`
- 创建脚本：

```sql
CREATE USER paperclip WITH PASSWORD 'paperclip';
CREATE DATABASE paperclip_repos OWNER paperclip;
GRANT ALL PRIVILEGES ON DATABASE paperclip_repos TO paperclip;
```

### 5.2 Mock 模式

集成测试通常用真实 PG（不用 mock）。少数情况（如 WS / 时间相关）用 mock：

- `pc-realtime::mock_clock`
- `pc-heartbeat::mock_ticker`

### 5.3 测试工具

- `pc_test_helpers::temp_pg` — 启动临时 PG
- `pc_test_helpers::TestApp` — 启动测试 app（带路由）
- `pc_test_helpers::seed_*` — 数据 seed

## 6. Debug 技巧

### 6.1 日志

```bash
# 全部 info
RUST_LOG=info cargo run -p pc-server

# 单 crate debug
RUST_LOG=pc_heartbeat=debug cargo run -p pc-server

# tracing filter
RUST_LOG=info,paperclip_server=debug,sqlx=warn cargo run -p pc-server
```

### 6.2 DB 查询调试

```bash
# 启用 sqlx 日志
RUST_LOG=sqlx=debug cargo run -p pc-server

# 或环境变量
DATABASE_URL=postgres://... psql -c "SELECT * FROM pg_stat_activity WHERE state='active';"
```

### 6.3 性能分析

```bash
# 1. 启用 release profile
cargo build --release

# 2. flamegraph
cargo install flamegraph
cargo flamegraph -p pc-server

# 3. perf + flamegraph
perf record -F 99 -g ./target/release/paperclip-server
perf script | flamegraph.pl > flame.svg
```

## 7. 部署与发布

### 7.1 升级流程

```bash
# 1. 备份
paperclipai db-backup --output /var/backups/pre-upgrade-$(date +%Y%m%d).dump

# 2. 拉取新代码
git fetch && git checkout v0.X.0

# 3. 编译
cargo build --release -p pc-server -p pc-cli -p pc-migrate

# 4. 应用 schema 迁移
paperclip-migrate up

# 5. 重启
systemctl restart paperclip-server

# 6. 验证
curl -fsS http://localhost:8080/health
```

### 7.2 CI / CD

```yaml
# .github/workflows/ci.yml
- cargo test --workspace --lib
- cargo clippy --workspace --all-targets -- -D warnings
- cargo fmt --check
- bash scripts/e2e-baseline.sh
- bash scripts/v11-ui-happy-path.sh
```

## 8. 常见问题

### Q1: 编译报错 "cannot find crate `tracing`"

```bash
# 检查 Cargo.toml 是否有 tracing 直接依赖
# pc-adapter-* 通常没直接依赖 → 用 eprintln! 替代 tracing::warn
# 或者：
grep -A 1 "tracing" crates/<crate>/Cargo.toml
```

### Q2: 测试 flaky

```bash
# 1. 单独跑该测试
cargo test -p <crate> --test <test-file> <test-name>

# 2. 加 --test-threads=1
cargo test -p <crate> -- --test-threads=1

# 3. 检查 DB 隔离（每个测试 fixture 用 unique UUID）
```

### Q3: 启动 panic "Overlapping method route"

axum 0.7 检测重叠路由。解决方案：

```rust
// 错误：两个 router 都有 /api/foo
Router::new().route("/api/foo", get(...))  // routes::foo
Router::new().route("/api/foo", get(...))  // routes::bar ← panic

// 正确：拆分子路径
Router::new().route("/api/foo", get(...))  // routes::foo
Router::new().route("/api/foo/bar", get(...))  // routes::bar
```

参考 `crates/pc-http/src/routes/budgets.rs` 和 `costs.rs` 的拆分示例。

## 9. 贡献流程

```bash
# 1. Fork
# 2. 创建特性分支
git checkout -b feature/r###-xxx
# 3. 写代码 + 测试
# 4. 跑全套验证
cargo test --workspace --lib
cargo clippy --workspace --all-targets -- -D warnings
bash scripts/e2e-baseline.sh
bash scripts/v11-ui-happy-path.sh
# 5. 写 evidence（openspec/changes/.../evidence/r###-xxx.md）
# 6. Commit + push
# 7. 开 PR
```

## 10. 资源

- `ARCHITECTURE.md` — 架构状态
- `OPERATIONS.md` — 运维手册
- `PLUGIN_AUTHORING.md` — 插件开发
- `MIGRATION_FROM_NODE.md` — Node 迁移
- `MODULE-MAPPING.md` — Node → Rust 模块映射
- `PROJECT-PLAN.md` — v1.0 计划
- 进度快照：`openspec/changes/paperclip-rs-comprehensive-validation/progress-snapshot.md`
