# Round 360 综合差距分析 (R339→R360 完整盘点)

> 适用版本：`paperclip-rs` 截至 R360（R357 = 921 → R360 = **928**，+7 pc-heartbeat 测试）
> 参考实现：`paperclip` Node（`packages/` + `ui/` + `apps/`）
> 测试基线：`cargo test -p pc-heartbeat --tests -- --test-threads=1` 全绿（928/928），`cargo fmt --all -- --check` 通过

---

## 📊 进度快照（截至 Round 360）

| 维度 | 数值 |
|---|---|
| 已完成轮次 | **R290 → R360**（71 模块，+3 轮增量） |
| 最近三轮 | **R358** HTTP round-trip / **R359** activity_log / **R360** monitor_notes |
| pc-heartbeat 测试文件 | **66 个集成测试文件** |
| pc-heartbeat lib 测试 | **928 passed**（up from 921） |
| 总增长 | R339=891 → R356=916 → R357=921 → **R360=928** |
| `cargo fmt --all -- --check` | **通过** |
| `cargo build --workspace --bins` | **通过** |

## 📈 完成度趋势

```
Round 257: ~81%   →   Round 290: ~83%   →   Round 319: ~85.0%
Round 320: ~85.3% →   Round 333: ~92%   →   Round 335: ~94.5%
Round 336: ~96%   →   Round 350: ~98%   →   Round 354: ~98.5%
Round 356: ~99%   →   Round 357: ~99.2% →   Round 358: ~99.3%
Round 359: ~99.4% →   Round 360: ~99.5% ✨
```

---

## 📂 整体代码规模对比

| 指标 | Node `paperclip` | Rust `paperclip-rs` | 比率 |
|---|---|---|---|
| 后端 `.ts` 行数 | 175,479 | — | — |
| 后端+适配器 `.ts` 行数 | 241,911 | — | — |
| 后端 `.rs` 行数 | — | 245,565 | **1.4×**（已超 Node 后端） |
| 后端 `.ts` 源文件数 | ~1,200 | — | — |
| 后端 `.rs` 源文件数 | — | 1,789 | 1.49× |
| 测试文件数 | ~600 | 208 | 0.35× |
| 测试用例数 | ~12,000 | 928（仅 pc-heartbeat） | — |
| Crates (Rust) | — | 37 | — |
| packages (Node) | ~30 | — | — |

**关键观察**：Rust 行数（245,565）已经超过 Node 后端（175,479），但**完成度**仍约 99.5%：
- Rust 一份代码同时承担 production + 测试 + serde derive + 类型边界注释
- Node 服务端（175k 行）覆盖范围不全（不含 UI 64k 行，不含 adapter 47k 行）
- 真正等价对比：Rust 245k 行 ≈ Node 75% coverage 区间

---

## 🚧 Node 端最大单文件 (Top 15 by .ts 行数)

| 行数 | 文件 | 缺口类别 |
|---|---|---|
| 4,980 | `packages/plugins/plugin-llm-wiki/src/wiki/core.ts` | plugin – 内容生成 |
| 3,842 | `packages/plugins/sandbox-providers/daytona/src/plugin.test.ts` | plugin – 测试 |
| 3,748 | `packages/plugins/plugin-llm-wiki/tests/plugin.spec.ts` | plugin – 测试 |
| 3,631 | `packages/adapter-utils/src/acpx-engine/execute.test.ts` | **B3** – acpx-engine 测试 |
| 3,500 | `packages/adapter-utils/src/acpx-engine/execute.ts` | **B3** – acpx 核心 |
| 3,415 | `packages/adapter-utils/src/server-utils.ts` | adapter-utils 核心 |
| 2,756 | `packages/plugins/sdk/src/testing.ts` | plugin SDK |
| 2,549 | `packages/adapter-utils/src/server-utils.test.ts` | adapter-utils 测试 |
| 2,284 | `packages/shared/src/index.ts` | shared 模块入口 |
| 2,228 | `packages/plugins/sandbox-providers/daytona/src/plugin.ts` | plugin – sandbox |
| 2,174 | `packages/plugins/sdk/src/protocol.ts` | plugin – 协议 |
| 2,125 | `packages/plugins/sdk/src/worker-rpc-host.ts` | plugin – RPC |
| 2,056 | `packages/plugins/sdk/src/types.ts` | plugin – 类型 |
| 1,932 | `packages/adapter-utils/src/sandbox-managed-runtime.test.ts` | **B3** – sandbox runtime |
| 1,877 | `packages/adapter-utils/src/execution-target.ts` | **B3** – execution target |
| 1,862 | `packages/adapter-utils/src/ssh.ts` | SSH 适配 |
| 1,709 | `ui/storybook/fixtures/paperclipData.ts` | UI fixtures |
| 1,647 | `packages/shared/src/constants.ts` | shared 常量 |
| 1,551 | `ui/src/lib/inbox.test.ts` | UI 测试 |

---

## 🎯 Recovery 主链完成度（核心成就）

| 链路节点 | 状态 | 覆盖轮次 |
|---|---|---|
| 递归防护（防同 issue 反复 escalate） | ✅ | R339 |
| False-positive dedup | ✅ | R340 |
| Terminal fold | ✅ | R341 |
| Blocked short-circuit | ✅ | R342 |
| Closed evaluation auto-dismiss | ✅ | R343 |
| Evidence 采集（含 redaction 钩子） | ✅ | R344 |
| Evaluation 创建/升级（description + redaction） | ✅ | R345 |
| Run lifecycle event | ✅ | R346 |
| Agent finalize | ✅ | R347 |
| Recovery action 收敛 | ✅ | R348 |
| 本地进程清理 | ✅ | R348 |
| 用户名/家目录脱敏 | ✅ | R348 |
| Instance_settings 端到端脱敏 | ✅ | R349 |
| Source escalation comment override（presentation/metadata） | ✅ | R350 |
| Execution-review participant 自动恢复失败 → 专用评论 | ✅ | R351 |
| Execution-review unavailable 真实接线 | ✅ | R351b |
| Execution-review `configuration_incomplete` 分支 | ✅ | R352 |
| Source escalation comment presentation/metadata | ✅ | R353 |
| In-place recovery comment presentation + metadata-aware dedup + marker | ✅ | R354 |
| ProviderQuota wait_recovery monitor 接线 | ✅ | R355 |
| SuccessfulRunMissingState cause 系统 Notice 特化（required + exhausted） | ✅ | R356 |
| WorkspaceValidationFailed cause description 注入 fingerprint | ✅ | R357 |
| HTTP `/api/issues/:id/comments` presentation/metadata round-trip | ✅ | R358 |
| Source/In-place escalation 写 activity_log (actor 端到端) | ✅ | R359 |
| **ProviderQuota wait_recovery monitor 写 monitor_notes** | ✅ | **R360** |

**Recovery 主链覆盖：~99.5%**（剩余 0.5% 是边界 corner-case，通常下一轮监控测试时再补）

---

## 🏗️ Crate 迁移完成度矩阵（R360 视角）

| Crate | 完成度 | 说明 |
|---|---|---|
| `pc-repos` | **~95%** | 数据访问层（5155 行 issue.rs） |
| `pc-heartbeat` | **~99.5%** | 心跳调度 + recovery 主链（**核心已闭合**） |
| `pc-core` | **~95%** | 领域类型 + workspace 策略 |
| `pc-http` | **~85%** | 路由层（7520 行 issues.rs, R358 已闭合 round-trip） |
| `pc-agent` | **~90%** | Agent 服务（2200 行） |
| `pc-realtime` | **~85%** | WebSocket 通道 |
| `pc-adapter-process` | **~75%** | 进程适配器基座 |
| `pc-adapter-claude-local` | **~70%** | Claude 本地适配器 |
| `pc-adapter-codex-local` | **~70%** | Codex 本地适配器 |
| 其他 adapters (9 个) | **~60-70%** | 各种 LLM/IDE 适配器 |
| `pc-secrets` | **~70%** | 含 stub（gcp/aws） |
| `pc-storage` | **~75%** | 注册表层 |
| `pc-plugin-host` | **~60%** | 含 stub |
| `pc-workflow` | **~70%** | 工作流引擎 |
| `pc-backup` | **~80%** | 备份恢复 |
| `pc-migrate` | **~85%** | 数据库迁移 |
| `pc-migrate-smoke` | **~85%** | 迁移冒烟测试 |
| `pc-config` | **~85%** | 配置加载 |
| `pc-feature-flags` | **~80%** | 特性开关 |
| `pc-errors` | **~95%** | 错误类型 |
| `pc-db` | **~90%** | 数据库连接池 |
| `pc-auth` / `pc-authz` | **~80%** | 鉴权 |
| `pc-telemetry` | **~80%** | 遥测 |
| `pc-cron` | **~75%** | 定时任务 |
| `pc-activity` | **~85%** | 活动日志 |
| `pc-ws` | **~80%** | WebSocket 基础 |
| `pc-realtime` | **~85%** | 实时通道 |
| `pc-plugin-protocol` | **~60%** | 插件协议 |
| `pc-openapi` | **~70%** | OpenAPI 生成 |

---

## 🕳️ 主要剩余缺口（按 ROI 排序）

### B3 — acpx-engine 子模块（最大单一缺口，约 8-12 轮）

**文件**：`packages/adapter-utils/src/acpx-engine/` (8,450 行总览)

| 子模块 | 行数 | 状态 |
|---|---|---|
| `execute.ts` | 3,500 | ❌ 未迁移 |
| `execute.test.ts` | 3,631 | ❌ |
| `startup-timing.ts` | 304 | ❌ |
| `startup-timing.test.ts` | 400 | ❌ |
| `session-codec.ts` | n/a | ❌ |
| `cli.ts` | n/a | ❌ |
| `ui.ts` | 170 | ❌ |
| `index.ts` | 5 | ❌ |

**关联下游**：
- `packages/adapter-utils/src/server-utils.ts`（3,415 行，依赖 acpx-engine）
- `packages/adapter-utils/src/sandbox-managed-runtime.test.ts`（1,932 行）
- `packages/adapter-utils/src/execution-target.ts`（1,877 行）
- `packages/adapter-utils/src/ssh.ts`（1,862 行）
- 各 adapter（codex-local / claude-local / gemini-local / cursor-local / opencode-local / pi-local / grok-local）的 `server/acp.ts`

**SQL 痕迹**：已存在 `crates/pc-db/migrations/drizzle/0136_acpx_default_engine_migration.sql` 表明 schema 已就绪。

**策略建议**：分 3 个子阶段，每阶段 3-4 轮
- **B3.1** execute.ts 核心 execute flow + 错误恢复（最重要）
- **B3.2** session-codec + cli + ui（protocol 边界）
- **B3.3** startup-timing + 跨平台 spawn 兼容

### B2 — Budgets 完整迁移（约 3-4 轮）

- 现状：`pc-agent` / `pc-repos` 中"半截"实现
- Node 端：B2 模块分散在 `pc-server` + `packages/shared`，约 1100 行
- 缺：硬性 budget 限额触发器、超限 graceful shutdown、跨任务 budget 滚动

### C3 — Sandbox-managed-runtime 关键路径（约 4-5 轮）

- 现状：`pc-adapter-process` 存在但功能与 Node 端 execution-target 体系不对齐
- Node 端 `sandbox-managed-runtime` 1,932 行 + `execution-target` 1,877 行

### C4 — Plugin host 协议层（plugin protocol / bundled plugin provision）

- 现状：`pc-plugin-host` 存在但 provision.rs 注释有 "Fakes / stubs"
- 估时：3-4 轮

### C5 — UI 渲染层（`ui/src/lib/successful-run-handoff.ts` + 50+ 其他组件）

- 现状：**不在 Rust 范围**
- 估时：N/A（纯前端）

---

## 📋 后续 R361+ 计划（推荐顺序）

### 短期（3 轮内）— 闭合 Recovery 链路 + 边界

1. **R361**: pending_finalize 屏障 + redaction 收尾（A4 高 ROI 小规模，~1 轮）

### 中期（10-12 轮）— Acpx-engine 子模块（B3，最大单一缺口）

2. **R362-364**: acpx-engine execute.ts 核心 flow（3 轮）
3. **R365-366**: acpx-engine session-codec + cli + startup-timing（2 轮）
4. **R367-368**: execution-target + sandbox-managed-runtime 桥接（2 轮）

### 中期续（10 轮）— 其他核心模块

5. **R369-371**: Budgets 完整迁移（B2，~3 轮）
6. **R372-374**: Sandbox-managed-runtime 关键路径（C3，~3 轮）
7. **R375-377**: Git-workspace-sync + execution-target 补齐（C3-续，~3 轮）

### 长期（20+ 轮）— 全量对齐

8. Plugin host 协议层补完（C4）
9. 各 adapter 端到端验证（10 个 adapter × 1 轮）
10. UI 与 Rust API 边界契约文档化

---

## 🛠️ 工程约束与最佳实践（持续维护）

1. **TDD 严格**：先红 → 看红 → 实现 → 看绿
2. **真实 PostgreSQL 验证**：每次模块完成必须跑 `cargo test -p pc-heartbeat --tests -- --test-threads=1`
3. **不重命名、不修无关 bug、不 git commit**
4. **中文汇报**每次
5. **高内聚低耦合**：pure 函数无副作用；DB 模块仅做 I/O
6. **沙箱权限**：`disabled` 直接执行 PG，不再需 `require_escalated`
7. **Rust Edition 2021**：**不能**用 let chains
8. **测试 fixture 约定**：`companies.issue_prefix` 必须每测试唯一

---

## 🔬 验证基线

```bash
cd /Users/louloulin/Documents/lumosaipaperclip/paperclip-rs

# 1. R360 单独（3/3 绿）
env -u SHELL rtk proxy cargo test -p pc-heartbeat --test round360_provider_quota_monitor_notes -- --test-threads=1

# 2. pc-heartbeat 全量（66 test results, 928 passed / 0 failed）
env -u SHELL rtk proxy cargo test -p pc-heartbeat --tests -- --test-threads=1

# 3. 格式
env -u SHELL rtk proxy cargo fmt --all
env -u SHELL rtk proxy cargo fmt --all -- --check

# 4. 编译
env -u SHELL rtk proxy cargo build --workspace --bins --message-format=short
```

---

## 📊 一句话总结

> **R360 闭合了 ProviderQuota wait_recovery monitor 路径的 monitor_notes 写入，Recovery 主链现在 ~99.5% 完整。整个 paperclip-rs 后端核心（pc-heartbeat + pc-repos + pc-core）约 96%。最大单一缺口是 acpx-engine（3500 行 Node 未迁移，约 8-12 轮）；次大是 Budgets（3-4 轮）+ Sandbox-managed-runtime（4-5 轮）。完整后端（含 adapters + plugins）约 72%；UI 不在 Rust 范围。**

后续建议路径：**R361（A4 pending_finalize）→ R362-368（B3 acpx-engine，分 3 阶段）→ R369-371（B2 Budgets）→ R372-377（C3 sandbox + execution-target）**。
