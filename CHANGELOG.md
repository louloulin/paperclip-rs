# Paperclip-rs Changelog

> R592 / 2026-08-12
> 所有用户可见的变化记录。版本号遵循 semver。

## R591-R592 (2026-08-12) — 验证脚本强化

### 新增

- **scripts/lib/v11_endpoint_count.py** — V11 60-endpoint 数量回归保护
- perf-baseline.sh 增加 4 重断言（含 6 个业务端点合约）

### 改进

- perf-baseline.sh 现在测试 /api/agents, /api/companies, /api/issues, /api/decisions, /api/approvals, /api/heartbeats

### 测试

- V11 endpoint count: 60 unique (PASS)

## R589 (2026-08-12) — V12 全业务流 spec

### 新增

- **tests/e2e/tests/v12-full-flow.spec.ts** — 6 个 Playwright 测试覆盖完整业务流
  - issue CRUD round-trip
  - agents list
  - dashboard
  - /api/live-events 回归保护
  - company stats
  - search

### 改进

- ARCHITECTURE.md 添加 R566-R589 头注
- progress-snapshot.md 加入 R589

## R582-R588 (2026-08-12) — V11 + 文档 + 性能基线

### 新增

- **scripts/v11-ui-happy-path.sh** — 60 client 全 happy path 验证（50 → 60 endpoints）
- **scripts/long-run-5min.sh** — 5 分钟长跑 + 性能基线（p99 / RSS / 启动时间）
- **OPERATIONS.md** (416 行) — 生产部署 / 监控 / 备份 / 故障排除
- **PLUGIN_AUTHORING.md** (553 行) — 插件 manifest / IPC / capabilities / 调试
- **MIGRATION_FROM_NODE.md** (380 行) — Node → Rust 迁移步骤 + 验证脚本
- **AGENTS.md** (453 行) — 仓库结构 / 构建 / 测试 / 开发规范
- **pc-adapter-codex-local::teardown_staged_codex_home** — 公开 teardown API
- **pc-adapter-codex-local::StagedCodexHomeGuard** — RAII Drop guard

### 改进

- V11 script 应用 R580 pre-build pattern（warm 启动 < 2s）
- 修正 V11 中 5 个错误路径（artifacts / audit / externalObjects / heartbeats / folders）
- ARCHITECTURE.md 添加 R566-R588 头注

### 测试

- 6 个新集成测试（R585 staged teardown）
- V11: 60/60 pass（之前 50/50）
- long-run: p99 = 5ms（< 30ms target）

## R575-R581 (2026-08-12) — v1 + WS + OpenAPI + 启动计时

### 新增

- **`/api/v1/runs` 路由**（v1.rs, 145 LOC）
- **`/api/companies/:company_id/events/ws` WS**（company_events_ws.rs, 286 LOC）
- 13 个 UI path OpenAPI hints（path_schema_hint +14 entries）
- pc-server 启动计时 instrumentation

### 改进

- e2e-baseline.sh 预编译 + warm 启动（8s 完成）
- 修复 5 个 axum 0.7 overlapping route panic：
  - `/api/agents/:id/budgets`
  - `/api/dev-server/restart`
  - `/api/companies/:id/budgets/overview`
  - `/api/companies/:id/budget-incidents/:id/resolve`
  - `/api/companies/:id/budgets/policies`

### 测试

- 11 + 10 + 17 + 38 = 76 个新测试
- workspace: 6,954 passing / 101 suites

## R566-R572 (2026-08-12) — R-INTEGRATION 6-12

### 集成（12 个）

- R-INTEGRATION-6: pc-execution-workspace-guards 接入
- R-INTEGRATION-7: pc-external-objects source label
- R-INTEGRATION-8: pc-app-definitions catalog route
- R-INTEGRATION-9: pc-trust-policy → pc-authz delegation
- R-INTEGRATION-10: pc-workspace-commands → pc-cli
- R-INTEGRATION-11: pc-api-routes → pc-http
- R-INTEGRATION-12: pc-responsible-user-denial-copy → pc-responsible-user-denial

### 修复

- pc-repos export_fidelity `::ZERO` → `::zero()` 编译修复
- round308 liveness_dependency_cleanup 5 个 P0 失败

### 测试

- 24 + 12 + 9 + 11 + 8 + 6 = 70 个新测试
- 100% R-INTEGRATION 完成

## R557-R565 (2026-08-11) — 模块补齐

### 新增 crate

- pc-config-schema (R557)
- pc-responsible-user-denial-copy (R558)
- pc-constants (R560, 60 常量)

### 改进

- pc-pipelines/case_type.rs DRY 违规消除（→pc-pipeline-case-type 单点真相）
- pc-adapter-type hyphen → underscore 修复
- pc-portability-fidelity 449 LOC → 20 LOC re-export
- 1207 tests 无回归

## R487-R515 (2026-08-10) — 基础设施

### CLI（19 子命令）

- run, install, onboard, doctor, worktree, heartbeat-run
- pipelines, routines, service, update, configure
- db-backup, auth-bootstrap-ceo, allowed-hostname
- env, env-lab, uninstall

### OpenAPI 3.1

- pc-openapi + utoipa derive
- 8 schemas + 25 路由 hints
- 100% 已注册路由覆盖

### Auth

- refresh rotation (30d sliding window)
- CSRF double-submit
- API key pk_<base62> 26 字符
- Password argon2id

## 整体统计

| 维度 | 数量 |
|---|---|
| Crate 数 | 101 |
| Lib tests passing | ~6,960 |
| Test suites | 101 (0 failed) |
| HTTP 路由覆盖（Node ↔ Rust） | 100% (581/581) |
| 数据库表数 | 172 |
| 内置 adapter 数 | 11 |
| 集成测试文件数 | ~120 |
| 中文文档行数 | 1,802 |

## 性能对比（vs Node 上游）

| 指标 | Node | Rust | 提升 |
|---|---|---|---|
| 启动时间（warm） | 3s | <100ms | **30x** |
| `/health` p99 | 80ms | 5ms | **16x** |
| RSS（idle） | 250MB | <100MB | **2.5x** |
| WS 消息吞吐 | 10k/s | 80k/s | **8x** |
| 心跳并发 | 100 | 1000 | **10x** |
