# Tasks: paperclip-rs 全面复刻 + 真实启动验证

> 配套 `proposal.md` + `design.md`。每行一个原子动作，勾完即视为该任务"真实验证"通过。
> 全部真实操作：源码写入、`cargo build` 通过、回归 `scripts/e2e-baseline.sh` 通过、模块自带的 happy + ≥3 edge case 集成测试通过。

---

## V1 — 真实基线验证（硬阻塞，P0）

- [ ] `scripts/e2e-baseline.sh` 端到端：临时 PG16 → `pc-migrate up` 172 表 → 起 `pc-server` → `curl /health` 200 → curl 5+ GET → graceful shutdown
- [ ] 启动 WARN 0；路由冲突 0
- [ ] macOS + Linux（glibc/musl）双平台 exit 0
- [ ] `scripts/dev-ui-rust.sh` 复用并加固
- [ ] `crates/pc-migrate/src/main.rs` CLI：`up` / `down` / `status` / `create` / `verify` / `baseline` / `seed`
- [ ] `apps/pc-server/src/main.rs` 启动序列：config → telemetry → migrate → router → bind → graceful shutdown
- [ ] evidence: `evidence/v1-baseline.md`

## V2 — CLI 全部子命令（P0 运维必需）

- [ ] `apps/pc-cli/src/main.rs` clap v4 derive 骨架
- [ ] 19 子命令文件 `apps/pc-cli/src/cmd/<name>.rs`：
  - [ ] `run`
  - [ ] `install`
  - [ ] `onboard`
  - [ ] `doctor`
  - [ ] `worktree`
  - [ ] `heartbeat-run`
  - [ ] `pipelines`
  - [ ] `routines`
  - [ ] `service`
  - [ ] `update`
  - [ ] `configure`
  - [ ] `db-backup`
  - [ ] `auth-bootstrap-ceo`
  - [ ] `allowed-hostname`
  - [ ] `env`
  - [ ] `env-lab`
  - [ ] `uninstall`
- [ ] 每个子命令 `--help` 输出与原 `paperclip/cli/src/commands/*.ts` 字段一致
- [ ] 每个子命令 `--json` 输出可解析
- [ ] 至少 5 个子命令（run / install / doctor / onboard / db-backup）真实跑一遍
- [ ] evidence: `evidence/v2-cli.md`

## V3 — OpenAPI 3.1 完整生成（P0 契约）

- [ ] `pc-openapi` 引入 `utoipa` v4 derive
- [ ] `pc-http` 每个路由函数加 `#[utoipa::path(method, path, request_body, responses)]`
- [ ] `pc-core` 每个领域类型加 `#[derive(ToSchema)]`
- [ ] `pc-openapi` 提供 `OpenApiRegistry::builder()` 注册 paths + schemas
- [ ] `pc-http` 注册 `/openapi.json` 路由 + `/openapi.yaml` 路由
- [ ] `apps/pc-server` 启动时打印 OpenAPI 路径数
- [ ] `/openapi.json` 返回 200 + 至少 56 path
- [ ] 与原 `server/src/routes/openapi.ts` 字段级 1:1（paths + components.schemas）
- [ ] `scripts/check-ui-contract.sh` 重合率 ≥ 99%
- [ ] evidence: `evidence/v3-openapi.md`

## V4 — OpenAPI ↔ UI 类型对齐（P0 契约）

- [ ] `ui/src/api/types.ts` 用 `openapi-typescript` 从 `/openapi.json` 生成
- [ ] 60 个 client 文件 `ui/src/api/<resource>.ts` 用生成的 types 替换手写
- [ ] `scripts/check-ui-contract.sh`：跑 `openapi-typescript` + diff，失败 exit 1
- [ ] CI：types 生成 + 60 client 文件 lint
- [ ] 60 client 全部用生成的 types
- [ ] `check-ui-contract.sh` exit 0
- [ ] evidence: `evidence/v4-openapi-ui.md`

## V5 — Auth/AuthZ 完整化（P0 用户面）

### pc-auth
- [ ] `crates/pc-auth/src/session.rs`：Session = { user_id, company_id, expires_at, csrf, refresh_expires_at }
- [ ] `crates/pc-auth/src/cookie.rs`：pc_session + pc_csrf cookie 设置/解析
- [ ] `crates/pc-auth/src/csrf.rs`：double-submit token 生成/校验
- [ ] `crates/pc-auth/src/api_key.rs`：pk_<base62> 26 字符生成/校验/吊销（hash 存 DB）
- [ ] `crates/pc-auth/src/refresh.rs`：30d sliding window rotation
- [ ] `crates/pc-auth/src/oauth.rs`：Google + GitHub OAuth2 流程（state + PKCE）
- [ ] `crates/pc-auth/src/password.rs`：argon2id hash + verify
- [ ] `crates/pc-auth/src/actor.rs`：Actor = { kind: User | Agent, id, roles }

### pc-authz
- [ ] `crates/pc-authz/src/policy.rs`：trait Policy<S, A, R> { fn check(&self, ctx: &AuthCtx, action: A, resource: R) -> PolicyResult; }
- [ ] 策略表：5 mode × N resource（80+ case）
  - [ ] company / member / logo / invite / join_request
  - [ ] agent / config_revision / api_key / runtime_state / wakeup
  - [ ] issue / comment / label / attachment / approval
  - [ ] case / project / approval / decision / routine / pipeline
- [ ] `crates/pc-authz/src/board_mutation_guard.rs`：middleware 拦截 board user mutation

### 验收
- [ ] 80+ 集成测试覆盖 allow/deny/not_owner
- [ ] refresh rotation 单元测试
- [ ] OAuth state + PKCE 单测
- [ ] API key pk_<base62> 校验单测
- [ ] CSRF 缺失/错误/正确三态
- [ ] evidence: `evidence/v5-auth.md`

## V6 — 路由字节级补全（P0 用户面）

### companies 子路由
- [ ] `pc-http/src/routes/companies.rs` 补：
  - [ ] `GET /api/companies/:id/skills`
  - [ ] `GET /api/companies/:id/tools`
  - [ ] `GET /api/companies/:id/folders`
  - [ ] `GET /api/companies/:id/invites`
  - [ ] `GET /api/companies/:id/labels`
  - [ ] `GET /api/companies/:id/approvals`
  - [ ] `GET /api/companies/:id/org-svg.png`
  - [ ] `GET /api/companies/:id/join-requests`

### /api/admin/*
- [ ] `pc-http/src/routes/admin.rs` 新建：
  - [ ] `GET /api/admin/users`
  - [ ] `POST /api/admin/users/:id/role`
  - [ ] `GET /api/admin/audit-log`
  - [ ] `POST /api/admin/instance/maintenance`
  - [ ] `GET /api/admin/feature-flags`

### 验收
- [ ] 14 个新路由每个 ≥ 1 happy + 3 edge
- [ ] `scripts/diff-routes.sh` raw method+path 重合率 ≥ 95%
- [ ] evidence: `evidence/v6-routes.md`

## V7 — Heartbeat stale lock 修复（P0 稳定性）

- [ ] 复盘 Node `services/heartbeat.ts` `staleIssueLockSweep` 行为
- [ ] 找到 Rust 端 4 个失败 case 的差异点
- [ ] 修复 `pc_heartbeat::recovery::stale_issue_lock_sweep` 函数
- [ ] 新增真实 PG 集成测试（不依赖 round*.rs）
- [ ] round300 4 个失败全过
- [ ] 新增 ≥ 5 个 stale lock sweep 集成测试
- [ ] evidence: `evidence/v7-heartbeat.md`

## V8 — 远程 execution target（P1 分布式）

- [ ] `pc-adapter-claude-local/src/remote.rs`：
  - [ ] `restoreRemoteWorkspace`
  - [ ] `materializeRemoteClaudeConfig`
  - [ ] `startAdapterExecutionTargetPaperclipBridge`
- [ ] `pc-adapter-codex-local/src/remote.rs`：
  - [ ] `stagedCodexHomeDir` teardown
  - [ ] `restoreRemoteWorkspace`
  - [ ] `remoteCodexConfigDir` 决策
- [ ] `pc-adapter-process/src/ssh_bridge.rs`：mock SSH server
- [ ] 集成测试：起 mock SSH server，跑通 start → materialize → invoke → restore 全链路
- [ ] 增量 +20 集成测试
- [ ] evidence: `evidence/v8-remote-execution.md`

## V9 — Workflow + Cron 真实链路（P1 自动化）

- [ ] `pc-cron/src/parser.rs`：完整 cron 表达式解析（含 `?` `L` `W` `#`）
- [ ] `pc-cron/src/scheduler.rs`：`tokio-cron-scheduler` 集成
- [ ] `pc-workflow/src/routine.rs`：Routine 定义 + 触发 + 重试 + 失败告警
- [ ] `pc-workflow/src/pipeline.rs`：DAG 解析 + 拓扑执行 + step 失败中断
- [ ] `pc-workflow/src/triggers.rs`：cron + webhook + manual 三种触发
- [ ] 集成测试：cron 表达式 + 真实触发 routine + pipeline step 失败中断
- [ ] ≥ 20 集成测试
- [ ] evidence: `evidence/v9-workflow.md`

## V10 — Plugin 互操作（P1 生态）

- [ ] `pc-plugin-host/src/interop.rs`：与原 `@paperclipai/plugin-sdk` worker 真实握手
- [ ] `pc-plugin-host/src/event_bus.rs`：完整事件总线
- [ ] `pc-plugin-host/src/job_scheduler.rs`：cron + 手动
- [ ] `pc-plugin-host/src/job_store.rs`：plugin_jobs 持久化
- [ ] `pc-plugin-host/src/tool_dispatcher.rs`：工具注册 + 调用
- [ ] `pc-plugin-host/src/database_bridge.rs`：受限 DB 视图
- [ ] `pc-plugin-host/src/state_store.rs`：plugin_state 读写
- [ ] `pc-plugin-host/src/webhook_dispatcher.rs`：plugin_webhooks 发送
- [ ] `pc-plugin-host/src/manifest_validator.rs` + `capability_validator.rs`
- [ ] 与原 SDK worker 互操作通过
- [ ] 从加载 → 注册事件 → 触发作业端到端
- [ ] evidence: `evidence/v10-plugin.md`

## V11 — UI 60 client 全 happy path（P0 用户硬目标）

- [ ] `scripts/ui-happy-path.sh`：起临时 PG → pc-migrate up → pc-server → UI dev server (VITE_API_BASE) → curl 60 个 client endpoint
- [ ] 失败：输出缺失的 client 名 + 实际响应
- [ ] 报告：60/60 pass / 0 fail
- [ ] 失败截图 + 视频存档
- [ ] evidence: `evidence/v11-ui-happy.md`

## V12 — Playwright 真实 UI 剧本（P0 用户硬目标）

- [ ] `tests/e2e/full-stack-ui.spec.ts`：Playwright 跑：
  - [ ] 打开 UI 登录页
  - [ ] 注册 + 自动登录
  - [ ] 创建公司
  - [ ] 创建 issue
  - [ ] 启动 heartbeat
  - [ ] WS 收到 `heartbeat.run.completed`
- [ ] `scripts/e2e-full-stack.sh`：CI 用，三态平台
- [ ] 失败截图 + 视频存档
- [ ] 整剧本 60s 内通过
- [ ] macOS + Linux glibc/musl 三态均通过
- [ ] evidence: `evidence/v12-playwright.md`

## V13 — 真实长跑 + 性能基线（P1 性能声明）

- [ ] `scripts/long-run-5min.sh`：起 PG → migrate → server → 触发 10 个 heartbeat → 等 5 分钟 → 校验 WS 事件数
- [ ] `benches/http_routes.rs`：criterion bench 主要路由
- [ ] `scripts/wrk-compare.sh`：同时跑 Node server + Rust server，wrk 压 60s，对比 P99 / RSS / CPU
- [ ] 5 分钟长跑零回归
- [ ] P99 ↓ ≥ 30%、RSS ↓ ≥ 40%（与 Node 对比）
- [ ] evidence: `evidence/v13-perf.md`

## V14 — 真实迁移（109 → 172 表 patch）（P1 部署）

- [ ] 审计 109 → 172 差异（63 张衍生表的来源）
- [ ] 每个 patch SQL 文件加注释（"这张表来自 Node paperclip migration NNN"）
- [ ] `apps/pc-server` 启动时跑 schema diff，零 warning
- [ ] 启动 WARN 0
- [ ] 172 表 schema 注释完整
- [ ] evidence: `evidence/v14-migrate.md`

## V15 — 中文文档与移交（P2 社区）

- [ ] `paperclip-rs/AGENTS.md`（中文）：仓库结构、构建、运行、测试
- [ ] `paperclip-rs/OPERATIONS.md`（中文）：部署、备份、监控、故障恢复
- [ ] `paperclip-rs/PLUGIN_AUTHORING.md`（中文）：插件作者指南（注册、事件、工具、状态、webhook）
- [ ] `paperclip-rs/MIGRATION_FROM_NODE.md`（中文）：从 Node server 迁移指南（端口、环境变量、DB、备份）
- [ ] 新开发者 30 分钟可上手（亲自跑过 `git clone` → `cargo build` → `cargo run` → 登录 → 建公司 → 建 issue）
- [ ] evidence: `evidence/v15-docs.md`

---

## DoD（一个模块即视为完成）

1. ✅ Rust 源码写到对应 crate（高内聚低耦合）
2. ✅ `cargo check -p <crate>` + `cargo clippy -p <crate> -- -D warnings` 通过
3. ✅ `cargo test -p <crate>` 通过（含 happy + ≥3 edge 用例）
4. ✅ 回归 `scripts/e2e-baseline.sh` 通过
5. ✅ 真实运行一次（起 server、推 WS、调 CLI、跑 cron、跑 backup、跑 long-run）
6. ✅ Markdown 证据写入 `openspec/changes/paperclip-rs-comprehensive-validation/evidence/<module>.md`
7. ✅ 中文说明完整
8. ✅ `cargo fmt --check` 无 diff

任何一项不达标，本模块未完成。

---

## 整体完成度检查

- [ ] 15 个 V 模块全部勾完
- [ ] UI 60 client 全 happy path（V11）
- [ ] Playwright 真实 UI 剧本（V12）
- [ ] 5 分钟长跑无回归（V13）
- [ ] P99 ↓ ≥ 30% / RSS ↓ ≥ 40%（V13）
- [ ] `cargo clippy -- -D warnings` 0
- [ ] `cargo fmt --check` 0 diff
- [ ] 中文文档完整（V15）
- [ ] 最终报告 `openspec/changes/paperclip-rs-comprehensive-validation/FINAL-REPORT.md`
