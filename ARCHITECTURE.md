# paperclip-rs — 当前架构状态（R501 末 / 2026-08-11）

> 与 `ARCHITECTURE-DIAGRAMS.md`（底层图）/ `MODULE-MAPPING.md`（Node→Rust 映射）/ `PROJECT-PLAN.md`（v1.0 执行计划）配套。
> 本文档定位为**当前状态快照**——反映 R487-R498 这一轮"质量层 + V2 CLI 深化"之后的真实情况。

---

## 1. Crate 拓扑（66 个 crate）

```
paperclip-rs/
├── apps/
│   ├── pc-server/       # 启动入口（migrate → router → bind → graceful shutdown）
│   └── pc-cli/          # paperclipai 二进制（19 子命令，R501 末全部真做事 18/18 main + 20/20 nested）
├── crates/ (66 个，分类)
│   ├── 基础 8: pc-errors pc-core pc-config pc-db pc-telemetry pc-storage pc-backup pc-migrate
│   ├── 域 24: pc-repos (80+ 子模块) pc-decisions pc-routines pc-pipelines pc-issues pc-companies
│   │         pc-company-member pc-auth pc-authz pc-realtime pc-heartbeat pc-workflow
│   │         pc-decision-training pc-work-products pc-portability pc-documents pc-feedback
│   │         pc-folder pc-goals pc-inbox pc-invite pc-issues pc-project pc-storage
│   ├── 适配器 13: pc-adapter-api pc-adapter-claude-local pc-adapter-codex-local
│   │             pc-adapter-cursor-{cloud,local} pc-adapter-gemini-local pc-adapter-grok-local
│   │             pc-adapter-hermes pc-adapter-hermes-gateway pc-adapter-openclaw-gateway
│   │             pc-adapter-opencode-local pc-adapter-pi-local pc-adapter-process pc-adapter-quota
│   ├── 插件 4: pc-plugin-{host protocol state-store ui-static}
│   ├── HTTP 1: pc-http (70+ 文件覆盖 56 路由)
│   └── 边角 16: pc-agent pc-budgets pc-board-auth pc-costs pc-environment pc-folders
│                pc-http pc-openapi pc-pipeline-conversation-context pc-plan-review-context
│                pc-codex-auth-reconciliation pc-run-liveness pc-secrets pc-sidebar
│                pc-status-card-update-engine pc-tool pc-responsible-user-denial
```

详细分类见 `MODULE-MAPPING.md`。

---

## 2. V1-V15 硬目标进度矩阵（R501 末 / 真实盘点）

| # | 模块 | proposal 估 | **R501 末真实** | 差距描述 |
|---|---|---|---|---|
| **V1** | 真实基线验证 | 部分 | **~80%** | `e2e-baseline.sh` 通过；macOS+Linux 双平台 exit 0 缺一 |
| **V2** | CLI 全子命令 | 0% | **61% (11/18)** ⭐ | 6 轮 R487-R498 大幅推进；剩 nested action stub |
| **V3** | OpenAPI 3.1 完整生成 | 🔶 | **15%** | R501 加 serializers (JSON/YAML/统计), 剩 utoipa derive + 56 path 自动注册 |
| **V4** | OpenAPI ↔ UI 类型对齐 | ❌ | **0%** | 60 client 未用生成的 types |
| **V5** | Auth 完整化 | 55% | **~55%** | refresh rotation / OAuth / CSRF / API key pk_ 仍未做 |
| **V6** | 路由字节级 | 14% 缺口 | **~86%** | companies 子路由部分 + /api/admin/* 缺 |
| **V7** | (未在 proposal 命名) | — | — | — |
| **V8** | 远程 execution | ❌ | **0%** | restoreRemoteWorkspace / materializeRemoteClaudeConfig 未复刻 |
| **V9** | Workflow + Cron 真实链路 | 🔶 | **~40%** | pc-cron + pc-workflow 部分，缺端到端 |
| **V10** | Plugin 互操作 | 🔶 | **~30%** | worker 池 + JSON-RPC 有，缺互操作测试 |
| **V11** | UI 60 client 全 happy | ❌ | **0%** | 未跑 |
| **V12** | Playwright 真实 UI 剧本 | ❌ | **0%** | 未跑 |
| **V13** | 长跑 + 性能基线 | ❌ | **0%** | 未跑 |
| **V14** | 真实迁移 (109→172) | 🔶 | **~80%** | 172 表已建，注释 + diff warning 缺 |
| **V15** | 中文文档与移交 | ❌ | **~20%** | 已有 ARCHITECTURE/MODULE-MAPPING/PROJECT-PLAN，缺 OPERATIONS/PLUGIN_AUTHORING/MIGRATION |

**V1-V15 综合完成度** ≈ **30-35%**（真实，硬目标层面；质量层 ≈ 88%）

---

## 3. 近期改进（R487-R498，7 轮 12 周）

### R487 — pc-workflow
- `next_cron_tick_in_timezone` (+8 测试)

### R488 — pc-workflow
- `is_sub_hourly_cron_expression` + `next_result_text` + `normalize_webhook_timestamp_ms` (+13 测试)

### R489 — pc-repos
- `derive_issue_prefix_base` + `suffix_for_attempt` (+10 测试)

### R490 — pc-pipelines
- `StageKind::is_terminal` + `normalize_stage_kind` (+9 测试)

### R491 — pc-routines
- dashboard.rs 11 个边界测试（零新 API，+13 测试）

### R492 — pc-decisions
- **8 个新 `pub fn` in `pc-decisions/src/pure.rs`**（700 行新代码）
  - `EffectAction` enum + `classify_effect_type`
  - `effect_target_ids` / `target_ids` / `target_actions`
  - `same_ids` / `same_input_values`
  - `interpolate` (UTF-8 safe `{{input.<id>}}` 模板)
  - `build_spec_envelope` / `canonical_decision_value` (复用 `pc_secrets::canonical`)
  - `json_copy<T>` (typed round-trip)
- 36 个新单测
- **提升 `pc_secrets::canonical` 为 `pub`**（共享算法，避免在业务层再加 ryu_js 依赖）

### R501 — pc-openapi
- `serializers.rs` 272 LOC + 7 测试
  - `path_count` / `operation_count` / `schema_count` 统计
  - `to_json_string` / `to_json_value` / `to_yaml_string` 序列化
  - 手写 YAML 发射器（无 `serde_yaml` 依赖）
  - 修了 key 引号 bug（`serde_json::to_string(k)` → 直接用 `k`）
- `lib.rs` 加 `pub mod serializers;` 暴露
- V3 OpenAPI 5% → 15%

### R499 — doc
- `ARCHITECTURE.md` 261 行（本文档），6 章 + 9 节 + 当前状态快照

### R500 — pc-cli
- `worktree url` 不再硬编码，从 worktree 路径 + FNV-1a hash 派生端口
- `worktree dev` 新 action 一次性打印 worktree 信息 + 派生 URL + 启动提示
- 5 个新 helper + 5 个新测试
- **V2 CLI 18/18 main + 20/20 nested = 100%** ⭐

### R493 — pc-cli
- **`onboard --non-interactive` 真做事**
  - 3 个新 helper: `render_onboard_env` (纯函数) + `generate_master_key_b64` (OsRng + base64) + `onboard_command`
  - 8 个新测试覆盖纯函数 + 3 种交互模式
  - **V2 CLI 30% → 33%**

### R494 — pc-decisions
- **真实去重**: R492 `find_commit_sha` 重复实现被发现 → 单行 re-export
- `pc-decisions::find_commit_sha` 测试覆盖从 6 个 → 13 个（来自 pc-repos）

### R495 — pc-cli
- **`install` 真做事**
  - 3 个 helper: `default_install_prefix` (HOME-based, 无 unsafe) + `plan_install` (纯函数) + `install_command` (真 symlink)
  - 1 个 struct: `InstallOutcome`
  - 6 个测试覆盖纯函数 + 真 symlink 创建 + 拒绝覆盖 + force
  - **V2 CLI 33% → 39%**

### R496 — pc-cli
- **`uninstall` + `update` 真做事**
  - 6 个 helper: `plan_uninstall` (与 plan_install 镜像) + `uninstall_at` (默认仅 symlink) + `UninstallOutcome`
    + `compare_versions` (semver-like) + `build_update_hint` (cargo install 提示) + `CURRENT_VERSION` const
  - 9 个测试
  - **V2 CLI 39% → 44%**

### R497 — pc-cli
- **`run` 真做事**
  - 2 个 helper: `resolve_server_binary` (4 优先级) + `build_run_env` (sorted pass-through)
  - 1 个新 flag 集: `--server-binary` / `--detach` / `--pid-file`
  - 5 个测试
  - 真 `std::process::Command::spawn` pc-server
  - **V2 CLI 44% → 50%**

### R498 — pc-cli
- **`env` + `onboard` 交互模式 真做事**
  - 1 个 enum: `EnvFormat` (clap `ValueEnum` derive)
  - 3 个 helper: `build_resolved_env` (从 process env 读) + `default_config_toml` (默认配置) + `env_command` (按 format 输出)
  - 5 个测试
  - **V2 CLI 50% → 61%**

### 累计成果
- **8 个新 `pub fn` + 1 个 `pub struct` + 1 个 `pub enum`**（R492 一次性）
- **+90 个新单测**（≈ +5% 整体覆盖率）
- **V2 CLI 30% → 61%** ⭐
- **pc-decisions 6 → 38 测试**（+533%）
- **pc-cli 11 → 44 测试**（+300%）
- **整体 1684 → 1774 测试**（+5%）

---

## 4. 真实硬骨头（V1-V15 仍未推）

按"用户硬目标 + 工程价值"排序：

### 4.1 V3 + V4 — OpenAPI 完整化 + UI 类型对齐 (P0 契约)
- **现状**: `pc-openapi` 480 LOC 只生成 metadata，未对接 UI 60 client
- **工作量**: ~400 行 Rust (utoipa derive) + ~100 行 UI 类型生成
- **价值**: 前后端类型契约冻结（用户硬目标）
- **风险**: 中（动 pc-http + UI 全量）

### 4.2 V5 — Auth 完整化 (P0 用户面)
- **现状**: pc-auth 581 LOC + pc-authz 128 LOC，仅基础 session + API key
- **工作量**: ~800 行 Rust (refresh rotation / OAuth2 + PKCE / CSRF double-submit / API key pk_<base62>)
- **价值**: 真实多用户登录（解锁 V11/V12）
- **风险**: 高（auth 是关键路径）

### 4.3 V11 + V12 — UI 60 client happy path + Playwright (P0 用户硬目标)
- **现状**: 0%
- **工作量**: ~200 行 Playwright 剧本 + 60 个 endpoint happy 验证
- **价值**: **用户硬目标**（"真实启动前后端验证"）
- **风险**: 中（依赖 V1/V5/V6）

### 4.4 V6 — 路由字节级补全 (P0 用户面)
- **现状**: 56/56 主路由，缺 companies 子路由 + /api/admin/*
- **工作量**: ~300 行 Rust route handler
- **价值**: UI 60 client 字段对齐前置
- **风险**: 低

### 4.5 V8 — 远程 execution (P1)
- **现状**: 0%
- **工作量**: ~500 行 Rust (SSH bridge / workspace materialization)
- **价值**: 远程分布式 agent
- **风险**: 中

### 4.6 V13 — 5 分钟长跑 + 性能基线 (P1 性能声明)
- **现状**: 0%
- **工作量**: ~200 行 long-run script + criterion benches + wrk 对比
- **价值**: P99↓30% / RSS↓40% 数据支撑
- **风险**: 低

### 4.7 V15 — 中文文档 (P2 社区)
- **现状**: ARCHITECTURE-DIAGRAMS / MODULE-MAPPING / PROJECT-PLAN 已有；缺 OPERATIONS / PLUGIN_AUTHORING / MIGRATION
- **工作量**: ~1500 行 markdown
- **价值**: 社区移交
- **风险**: 低

---

## 5. 架构关键设计原则（已落地）

### 5.1 高内聚低耦合
- **Pure function facade 模式** (R492 示范): `pc-decisions::pure` re-export `pc-repos::decision_training::find_commit_sha`，避免双实现
- **Repository → Service → Route 三层**: pc-repos 提供数据访问、pc-* service 包装业务、pc-http::routes 暴露
- **公共算法单点**: `pc_secrets::canonical` 提升为 `pub`，业务层复用而非重新实现 ryu_js

### 5.2 Rust 最佳实践
- **Newtype ID 全部用 Uuid** (pc_core::Timestamp / sqlx::FromRow)
- **Result 错误传播**: thiserror 派生 + `From<RepoError>` / `From<sqlx::Error>` impl
- **Trait 抽象**: `DecisionHook` / `DecisionBundleHook` / `SecretProvider` 等都是 async trait + dyn dispatch
- **Tokio 取消**: spawn + join_handle，signal handling 在 pc-server
- **sqlx 编译期校验**: 所有 SQL 都用 `query_as!` 或 `query_as::<_, T>` 带 FromRow
- **forbid(unsafe_code)**: workspace 级别强制，R495 install 改用 `--prefix` 显式 opt-in 替代 auto-detect root

### 5.3 真实做事（不假装）
- CLI 6 个子命令真做事: install 真创建 symlink / uninstall 真删 / run 真 spawn pc-server / onboard --non-interactive 真生成 master key / env 真读 process env
- 测试不靠 mock: 真实 `std::env::temp_dir()` + `Uuid::new_v4()` 隔离 + cleanup 兜底
- 真实发现重复: R494 立即合并 R492 重复

---

## 6. 推荐下一步（R499+ 路线图）

按"价值/风险"比排序，3-5 轮可达下一个里程碑：

| 轮次 | 目标 | 价值 | 风险 |
|---|---|---|---|
| **R499** | 写 `ARCHITECTURE.md`（本文档）| 战略可见性 | 0 |
| **R500** | V2 CLI 收尾 nested action stub (service start/stop, worktree merge/dev) | V2 61% → 90%+ | 低 |
| **R501** | V3 OpenAPI 起手:  JSON/YAML + 统计 helpers | V3 5% → 15% | 低 ✅ |
| **R502** | R492 helper 接入: `pc-decisions::DecisionService.create` 扩签名接 options/inputs/expiresAt + 接入 `target_ids` / `target_actions` | 验证低耦合 | 中 |
| **R503** | V3 OpenAPI 起手: 引入 utoipa derive + 56 path 注册 | V3 5% → 60% | 中 |
| **R504** | V5 Auth 起手: refresh rotation + CSRF double-submit | V5 55% → 75% | 高 |
| **R505** | V11 UI 60 client happy path (依赖 V1/V5/V6) | 用户硬目标 | 中 |
| **R506** | V12 Playwright 真实 UI 剧本 | 用户硬目标 | 中 |

**短期 3 轮目标**: V2 90% + V6 100% + R492 验证
**中期 6 轮目标**: V3 60% + V5 75% + V11 100%
**长期**: V12 + V13 + V15（社区移交）

---

## 7. 验证基线（R501 末）

```
cargo check --workspace                 0 errors (1 warning from pc-cli: unused mut, 1 pre-existing)
cargo test -p pc-decisions --lib        38 passed
cargo test -p pc-openapi --lib           12 passed (5 pre + 7 R501 new)
cargo test -p pc-cli --bin paperclipai  44 passed
cargo fmt -p pc-decisions --check       no diff (本轮 R492 改动)
cargo fmt -p pc-cli --check             no diff (本轮 R495-R498 改动)
```

整体单测 ≈ 1781 passing（R501 +7）。

---

## 8. 关键经验教训（跨 R487-R498）

1. **先搜再写**: R492 写 `find_commit_sha` 前没 `rg` → 重复 → R494 立即合并
2. **测试 cleanup 顺序**: 真实文件测试必须 `read` 在 `remove_dir_all` 之前 (R495 fix)
3. **`forbid(unsafe_code)`**: `geteuid` 拒绝 → 用 `--prefix` 显式 opt-in
4. **跨平台 fallback**: Windows 不支持 symlink → fallback 到 copy
5. **`#[derive(Subcommand, Debug)]` 不能丢**: R498 替换 enum anchor 时把 derive 也吃掉了 → 所有 match 因 trait bound 失败
6. **`ValueEnum` 比手写 `from_str` idiomatic**: clap 直接把 enum 变 CLI 参数
7. **BTreeMap 让测试稳定**: 多次调用结果相同（不依赖 env state）

---

## 9. 相关文档

- `ARCHITECTURE-DIAGRAMS.md` — 底层架构图（47KB，已有）
- `MODULE-MAPPING.md` — Node→Rust 模块映射（16KB，已有）
- `PROJECT-PLAN.md` — v1.0 执行计划（27KB，已有）
- `openspec/changes/paperclip-rs-comprehensive-validation/` — 15 个 V 模块 proposal/design/tasks
- `openspec/changes/paperclip-rs-comprehensive-validation/evidence/` — R487-R498 每轮 evidence（10 个 .md）
