# Proposal: paperclip-rs 全面对比与剩余复刻 + 前后端真实启动验证

> Change 名称：`paperclip-rs-comprehensive-validation`
> 范围：基于 `paperclip-rs-modules-replica` 已完成的 75-80% 成果，对 `paperclip/` (Node) 与 `paperclip-rs/` (Rust) 仓库做一次全面逐模块对比，找出仍存在的真实差距，按业务域一个个模块继续复刻，每个模块完成都"真实启动前后端验证"，并最终交付一个"能登录、能建公司、能跑 heartbeat、能看 live-event"的完整端到端链路。
> 全部使用中文说明。

---

## 1. 背景与现状

### 1.1 仓库规模（2026-08-09 真实盘点）

| 维度 | paperclip (Node + TS) | paperclip-rs (Rust) | 完成度 |
|---|---|---|---|
| 源文件数 | 760 server + 1053 packages = **1813 TS 源文件** | **923 .rs 源文件** | 51% 文件数 |
| 代码行数 | server **444,278 LOC** + packages ≈ 30万 LOC | **355,349 LOC** (含 tests + fixtures) | ≈ 47% 体积 |
| HTTP 路由 | 56 个 | 56 个（pc-http 70+ 文件覆盖） | 100% 覆盖 |
| 服务模块 | 212 个 TS services | 80+ 个 pc-repos 子模块 | ≈ 90% 覆盖 |
| 数据库表 | 109 张 | 172 张（pc-db/migrations，含衍生） | schema 兼容 |
| 内置适配器 | 11 个 | 11 个 crate | 11/11 crate 已就位 |
| UI 文件 | 1168 React 文件 | 复用 `paperclip/ui/` | 100% 复用 |
| UI API client | 60 个 client 模块 | 复用，client.ts 接受 `VITE_API_BASE` | 100% 切流可 |
| WebSocket | live-events 端点 | `pc-realtime` 1,334 LOC + ws | 已实现 |
| Heartbeat | 状态机 + recovery | `pc-heartbeat` 29,642 LOC | 主路径完成 + 4 个 stale lock 失败待修 |
| Auth/AuthZ | better-auth 完整 | `pc-auth` 581 + `pc-authz` 128 LOC | **55%**，refresh / OAuth / CSRF 简化 |
| OpenAPI | routes/openapi.ts | `pc-openapi` 480 LOC | **最小可用，未对接** |
| Plugin SDK | 完整 npm SDK | `pc-plugin-host` 4,986 LOC | 已实现 worker 池 |
| CLI | 19+ 子命令 | `pc-cli` 1,017 LOC | **未实装**子命令 |
| Workflow / Cron | routines + pipelines + cron | `pc-workflow` 1,358 + `pc-cron` 840 | **部分** |
| Backup | db backup | `pc-backup` 1,445 LOC | 已实现 |
| Storage | local-disk + s3 | `pc-storage` 1,212 LOC | 已实现 |
| Secrets | local-encrypted + aws-sm | `pc-secrets` 2,535 LOC | 已实现 |
| 测试 | vitest 套件 | cargo test ≈ **1684 passing** | 88% 覆盖 |

### 1.2 已完成工作（paperclip-rs-modules-replica）

- ✅ M1 apps/ 目录契约：pc-server / pc-cli 物理独立
- ✅ M2 E2E 基线：临时 PG16 → pc-migrate up → pc-server → /health 200
- ✅ M3 Storage 真实链路：local_disk + s3 + registry
- ✅ M4 Secrets 真实链路：AES-256-GCM + aws-sm
- ✅ M5 Backup：pg_dump/pg_restore 链路 + SHA256
- ✅ M6 Migrate：up/down/status/create/verify/baseline/seed
- ✅ M7 Auth + AuthZ：基础 session + API key + Policy
- ✅ M8 Repos：80+ 子模块
- ✅ M9 HTTP 路由：56 路由全部就位
- 🔶 M10 OpenAPI：最小可用，未对接 UI 类型
- ✅ M11 Realtime WS：6 真实集成测试
- 🔶 M12 Heartbeat：498 单元测试通过，round300 stale lock 4 个失败
- ✅ M13 Adapter：11 adapter × 描述符测试
- 🔶 M14 Plugin：worker 池 + JSON-RPC 已实现，缺互操作测试
- 🔶 M15 Workflow + Cron：cron 10 测试通过，workflow 部分
- 🔶 M16 CLI：未实装
- ✅ M17 UI 切流：5 GET endpoint happy path
- ✅ M18 前后端 e2e：5 用例 Playwright API 合约
- 🔶 M19 OpenAPI ↔ UI：path-level diff，字段级未对齐
- ❌ M20 远程 execution target：未复刻
- 🔶 M21 路由字节级对齐：14% 缺口（companies 子路由 + /api/admin/*）
- 🔶 M22 Auth/AuthZ 完整：refresh rotation / OAuth / CSRF / API key pk_ 简化
- ❌ M23 Heartbeat stale lock sweep 回归修复

### 1.3 真实差距（综合盘点 2026-08-09）

| 差距编号 | 类别 | 描述 | 阻塞范围 |
|---|---|---|---|
| **G1** | CLI | `pc-cli` 仅 1017 LOC，19 个子命令几乎全空 | 部署 / 运维 |
| **G2** | OpenAPI ↔ UI | `pc-openapi` 480 LOC，只生成 metadata + path，未生成完整 schema；UI 60 个 client 字段未对齐 | 前后端类型契约 |
| **G3** | Heartbeat | round300 stale lock sweep 4 个失败 | 真实长跑任务稳定性 |
| **G4** | Auth 完整 | refresh token rotation / OAuth (Google, GitHub) / CSRF double-submit / API key `pk_<base62>` 简化 | 真实多用户登录 |
| **G5** | 远程 execution | claude-local / codex-local 远程路径未复刻（`restoreRemoteWorkspace`、`materializeRemoteClaudeConfig`、SSH bridge） | 远程分布式 agent |
| **G6** | 路由字节级 | companies 子路由（skills/tools/folders/invites/labels/approvals/org-svg.png/join-requests）+ `/api/admin/*` | UI 60 client 字段对不上 |
| **G7** | Plugin 互操作 | pc-plugin-host 与原 SDK worker JSON-RPC 互操作未测 | 第三方插件 |
| **G8** | Workflow 真实 | routines + pipelines DAG + cron 触发链路未端到端验证 | 自动化 |
| **G9** | UI 真实启动 | `scripts/dev-ui-rust.sh` 仅验证 5 个 GET，UI 60 client 全 happy path 未跑 | 用户目标硬阻塞 |
| **G10** | 端到端 Playwright | `tests/e2e/` 仅 5 API 用例，无真实 UI 剧本（登录 → 公司 → issue → heartbeat → live-event） | 用户目标硬阻塞 |
| **G11** | 真实长跑 | 启动后跑 5 分钟 heartbeat 跑 + WS 推流无回归 | 稳定性 |
| **G12** | 真实迁移 | 109 表 → 172 表 SQL 差异 patch + 衍生表说明 | 部署 |
| **G13** | 性能基线 | 无 wrk/criterion 真实压测数据 | 性能声明依据 |
| **G14** | 文档 | 中文 AGENTS.md / OPERATIONS.md / 插件作者指南 / 迁移指南 缺失 | 移交 / 社区 |

---

## 2. Why（为什么做）

1. **用户硬目标**：用户明确要求"真实启动前后端验证"——目前 G9 + G10 未完整跑通，UI 60 个 api client 的 happy path 没全验证，Playwright 没真实 UI 剧本。
2. **能力补全**：11 个 adapter + 80+ repo + 56 路由已有，但 CLI / OpenAPI ↔ UI / Auth 完整 / 远程 execution / plugin 互操作 / workflow 端到端是真正"可交付完整 paperclip-rs"的硬骨头。
3. **真实运行**：每个模块完成后必须"真实启动前后端验证"——不是 `cargo test` 通过就完事。
4. **rust 最佳方式**：用 newtype ID / Result 错误 / trait 抽象 / tokio 取消 / sqlx 编译期校验，零成本抽象。
5. **高内聚低耦合**：每个 crate 单一职责，跨 crate 仅通过 trait + 类型边界。

## 3. What（做什么）

按 15 个可独立交付的模块（V1–V15）从 open → design → build → verify 顺序推进。每个模块满足：
- **复刻**：`paperclip/<dir>` → `paperclip-rs/<crate>` 行为等价。
- **真实验证**：每个模块完成后跑一次真实运行（启动 + curl + DB roundtrip + e2e + 集成测试）。
- **高内聚低耦合**：通过 trait 抽象隔离边界。
- **rust 最佳方式**：newtype、Result、tokio、sqlx 编译期校验、tracing 结构化日志。

### 3.1 模块顺序（V1 → V15）

| # | 模块 | 目标 crate / 文件 | 验收 |
|---|---|---|---|
| **V1** | 真实基线验证 | `scripts/e2e-baseline.sh` + 端到端 | 临时 PG 起来，pc-migrate up 172 表，pc-server 起来，/health 200，启动 WARN 0，UI 5+ GET 全过 |
| **V2** | CLI 全部子命令 | `pc-cli` 1,017 → ≥ 6,000 LOC | run / install / onboard / doctor / worktree / heartbeat-run / pipelines / routines / service / update / configure / db-backup / auth-bootstrap-ceo / allowed-hostname / env / env-lab / uninstall，每个 `--help` 真实跑过 |
| **V3** | OpenAPI 3.1 完整生成 | `pc-openapi` 480 → ≥ 3,000 LOC | utoipa derive + /openapi.json + /openapi.yaml；与 Node `routes/openapi.ts` 字段级 1:1 |
| **V4** | OpenAPI ↔ UI 类型对齐 | `ui/src/api/*` + `scripts/check-ui-contract.sh` | 60 个 api client 字段全部一致；ts-rs 或等效反向生成 |
| **V5** | Auth/AuthZ 完整化 | `pc-auth` 581 → ≥ 3,500 LOC, `pc-authz` 128 → ≥ 2,500 LOC | refresh token rotation 30d sliding + OAuth Google/GitHub + CSRF double-submit + API key `pk_<base62>` 生成/校验/吊销 + 80+ 策略 case |
| **V6** | 路由字节级补全 | `pc-http` 44,702 → ≥ 50,000 LOC | companies 子路由（skills/tools/folders/invites/labels/approvals/org-svg.png/join-requests） + /api/admin/*；raw method+path 重合率 46.1% → ≥ 95% |
| **V7** | Heartbeat stale lock 修复 | `pc-heartbeat::recovery` | round300 4 个失败测试全过；新增 `stale_issue_lock_sweep` 真实 PG 集成测试 |
| **V8** | 远程 execution target | `pc-adapter-claude-local` + `pc-adapter-codex-local` | `restoreRemoteWorkspace` / `materializeRemoteClaudeConfig` / SSH bridge / `startAdapterExecutionTargetPaperclipBridge`；mock SSH server 跑通全链路 |
| **V9** | Workflow + Cron 真实链路 | `pc-workflow` 1,358 → ≥ 4,000 LOC, `pc-cron` 840 → ≥ 1,500 LOC | 真实 cron 触发 routine + pipeline step 失败中断 + DAG 验证；新增 ≥ 20 集成测试 |
| **V10** | Plugin 互操作 | `pc-plugin-host` 4,986 → ≥ 8,000 LOC | 与原 SDK worker 真实 JSON-RPC 互操作；从加载 → 注册事件 → 触发作业端到端 |
| **V11** | UI 60 client 全 happy path | `ui/src/api/*` + `scripts/ui-happy-path.sh` | 60 个 client 每个真实请求 fixture 一次，全部 200/合约拒绝 |
| **V12** | Playwright 真实 UI 剧本 | `tests/e2e/` + `scripts/e2e-full-stack.sh` | 登录 → 公司 → issue → heartbeat → live-event 整剧本；macOS + Linux glibc/musl 三态 |
| **V13** | 真实长跑 + 性能基线 | `scripts/long-run-5min.sh` + `benches/` | 5 分钟 heartbeat 跑 + WS 推流；wrk 压测数据：P99 ↓ 30%、RSS ↓ 40%（与 Node 对比） |
| **V14** | 真实迁移（109 → 172 表 patch） | `crates/pc-db/migrations/` | 衍生表 patch 注释 + 启动时无 schema diff warning |
| **V15** | 中文文档与移交 | `paperclip-rs/AGENTS.md` + `OPERATIONS.md` + `PLUGIN_AUTHORING.md` + `MIGRATION_FROM_NODE.md` | 中文说明完整；新开发者 30 分钟可上手 |

### 3.2 跨模块依赖

```
V1 基线 ──┬─→ V2 CLI ───────────────────────────┐
          ├─→ V3 OpenAPI ──→ V4 OpenAPI ↔ UI ──┤
          ├─→ V5 Auth ─────────────────────────┤
          ├─→ V6 路由字节级 ────────────────────┤
          ├─→ V7 Heartbeat 修复 ────────────────┤
          ├─→ V8 远程 execution ────────────────┤
          ├─→ V9 Workflow + Cron ──────────────┤
          ├─→ V10 Plugin 互操作 ───────────────┤
          ├─→ V11 UI 60 client ──→ V12 Playwright ┤
          ├─→ V13 长跑 + 性能 ──────────────────┤
          ├─→ V14 真实迁移 ─────────────────────┤
          └─→ V15 中文文档 ─────────────────────┴─→ 全部完成
```

V1 必须最先（基线 + 真实运行），其余可按用户优先级灵活穿插。

### 3.3 验收口径（DoD）

每个 V 模块完成后必须满足：

1. ✅ Rust 源码写到对应 crate（高内聚低耦合）
2. ✅ `cargo check -p <crate>` + `cargo clippy -p <crate> -- -D warnings` 通过
3. ✅ `cargo test -p <crate>` 通过（happy + ≥ 3 edge 用例）
4. ✅ 回归 `scripts/e2e-baseline.sh` 通过
5. ✅ 真实运行一次（起 server、推 WS、调 CLI、跑 cron、跑 backup、跑 long-run）
6. ✅ Markdown 证据写入 `openspec/changes/paperclip-rs-comprehensive-validation/evidence/<module>.md`
7. ✅ 中文说明完整
8. ✅ `cargo fmt --check` 无 diff

任何一项不达标，本模块未完成。

## 4. Out of Scope（不属于本 change）

- 嵌入式 PG 跨平台完善（涉及 binary 分发 + CI 矩阵，单独 change）
- 11 个 adapter 内部的协议解析细节（已知已基本完成）
- 旧 Node server 物理删除（合并切流量完成时做）
- 前端 React 组件重写（仅做 VITE_API_BASE 切流 + 真实 UI 剧本）

## 5. Success Criteria

- ✅ V1–V15 全部"复刻 → 真实验证"通过
- ✅ UI 60 个 api client 全 happy path 通过（V11）
- ✅ Playwright 真实 UI 剧本通过（V12）
- ✅ 启动 pc-server 后 5 分钟 heartbeat 跑 + WS 推流无回归（V13）
- ✅ `cargo clippy -- -D warnings` 无警告
- ✅ `cargo fmt --check` 无 diff
- ✅ 性能：wrk 同 fixture 下 P99 延迟较 Node server ↓ ≥ 30%、RSS ↓ ≥ 40%
- ✅ 全部中文 AGENTS.md / OPERATIONS.md / 插件作者指南 / 迁移指南

## 6. Sequencing Rationale

按"基线先、契约次、细节后、文档收尾"：

```
V1 真实基线 (硬阻塞)
  ├─→ V2 CLI（运维必需）
  ├─→ V3 OpenAPI（契约先定）→ V4 OpenAPI ↔ UI
  ├─→ V5 Auth（用户面）
  ├─→ V6 路由字节级（用户面）
  ├─→ V7 Heartbeat 修复（真实长跑前置）
  ├─→ V8 远程 execution
  ├─→ V9 Workflow + Cron
  ├─→ V10 Plugin 互操作
  ├─→ V11 UI 60 client（用户面）
  ├─→ V12 Playwright（用户面）
  ├─→ V13 长跑 + 性能
  ├─→ V14 真实迁移
  └─→ V15 中文文档
```

V1 必先做；V2/V3/V5/V6 可并行；V7 必在 V13 前；V4 必在 V12 前；V15 必最后。

## 7. Open Questions（设计阶段回答）

- V2 CLI：哪些子命令必先做？答：`run` / `install` / `doctor` / `onboard` / `worktree` / `heartbeat-run` 是运维面优先；其余后置。
- V3 OpenAPI：用 utoipa 还是手写 schema？答：utoipa derive（与 axum 类型天然对齐），手写 fallback 处理 dynamic shape。
- V5 Auth：复刻 better-auth 还是仅行为复刻？答：仅行为复刻（cookie 名 / 过期时间 / CSRF 头）。
- V8 远程 execution：mock SSH 还是 testcontainers？答：mock SSH（更轻、可重复、CI 友好）。
- V13 性能：是否对比 Node server？答：必对比（同时启动两套），输出 P99 / RSS / CPU。
- V15 文档：写中文还是双语？答：中文为主，关键英文术语保留（OpenAPI / JSON-RPC / OAuth / DAG 等）。
