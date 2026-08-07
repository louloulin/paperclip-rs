# Paperclip-rs 全面差距分析（2026-08-07）

> 评估基线：R395 完成后
> 对照对象：Node paperclip `packages/adapter-utils/` + `packages/adapters/`
> 工作区根：`/Users/louloulin/Documents/lumosaipaperclip/paperclip-rs`

## 一、当前完成度快照（按模块族）

| 模块族 | 完成度 | 已迁移代码量 | Node 端总规模 |
|---|---|---|---|
| **pc-acpx (核心引擎)** | **~70%** | 25,273 行 / 47 模块 | 36,000+ 行 |
| **pc-heartbeat + recovery** | **~99%** | 29,642 行 | 30,000+ 行 |
| **pc-repos (数据访问)** | **~95%** | 49,085 行 | 51,500+ 行 |
| **pc-adapter-* (11 adapters)** | **~35%** | 3,728 行 | ~10,500 行 |
| **pc-adapter-acpx-engine** | **~15%** | 1,636 行 (1 module) | 8,450 行 |
| **pc-plugin-host** | **~60%** | 2,020 行 | ~3,400 行 |
| **pc-storage / pc-secrets** | **~70%** | 中等 | 中等 |
| **整体后端** | **~78%** | ~180K 行 | ~245K 行 |

## 二、关键缺口分析（按 ROI 排序）

### A. 高 ROI — 必须立刻补齐的核心模块

#### A1. **pc-acpx-engine 完整化**（最大单模块缺口）
- 现状：`pc-acpx::acpx_engine_executor` 1,636 行，覆盖 **factory + entry point**
- 缺口（Node 端 8,450 行 vs Rust 1,636 行）：
  - `execute.ts` 中剩余的 turn 调度、ACP handshake、billing identity、prompt options、run-result shaping 完整流程（约 2,000 行）
  - `startup-timing.ts` 304 行 → 已部分迁移（919 行）但函数未对齐
  - `session-codec.ts` 50 行 → 未迁移
  - `cli.ts` 121 行 → 未迁移
  - `ui.ts` 170 行 → 未迁移
- 影响：所有 adapter 无法真正"完整"执行 ACP 协议
- 估时：6-8 轮（每个子模块 1-2 轮）

#### A2. **pc-acpx::execution_target**（1877 行 Node，0% Rust）
- 缺失函数：
  - `adapterExecutionTargetIsRemote` / `adapterExecutionTargetRemoteCwd` / `adapterExecutionTargetSessionIdentity`
  - `adapterExecutionTargetSessionMatches` / `adapterExecutionTargetUsesManagedHome` / `adapterExecutionTargetUsesPaperclipBridge`
  - `describeAdapterExecutionTarget` / `resolveAdapterExecutionTargetCwd` / `resolveAdapterExecutionTargetTimeoutSec`
  - `runtimeAssetDir` / `parseAdapterExecutionTarget` / `readAdapterExecutionTarget`
  - `adapterExecutionTargetFromRemoteExecution` / `adapterExecutionTargetToRemoteSpec`
  - `overrideAdapterExecutionTargetRemoteCwd` / `formatAdapterExecutionTimeoutErrorMessage`
  - `formatAdapterExecutionTimeoutStartLogLine`
- 影响：**所有 11 adapter 的 execute.ts 都依赖这些函数**，未迁移意味着 adapter execute 不能真正工作
- 估时：2 轮

#### A3. **pc-acpx::sandbox_managed_runtime**（1224 行 Node，0% Rust）
- 缺失：4 个核心函数 + 大量 sandbox lifecycle 管理
- 影响：sandbox lane 完全无法使用
- 估时：2-3 轮

#### A4. **pc-acpx::sandbox_callback_bridge**（1262 行 Node，0% Rust）
- 缺失：7 个核心函数 + bridge 协议
- 影响：worker → host 双向通信不通
- 估时：2 轮

#### A5. **pc-acpx::command_managed_runtime**（570 行 Node，0% Rust）
- 缺失：2 个核心函数 + 完整 client 实现
- 影响：managed command runtime 客户端不可用
- 估时：1-2 轮

#### A6. **pc-acpx::ssh**（1862 行 Node，0% Rust）
- 缺失：9 个核心函数 + 完整 SSH 客户端
- 影响：remote execution 完全不可用
- 估时：3-4 轮

#### A7. **pc-acpx::local_process_sandbox**（509 行 Node，0% Rust）
- 缺失：5 个核心函数 + scope 解析逻辑
- 影响：local process sandbox scope 配置不通
- 估时：1 轮

#### A8. **pc-acpx::git_workspace_sync**（433 行 Node，0% Rust）
- 缺失：4 个核心函数 + git delta bundle 脚本
- 影响：git workspace sync 不可用
- 估时：1-2 轮

#### A9. **pc-acpx::remote_managed_runtime**（239 行 Node，0% Rust）
- 缺失：2 个核心函数
- 影响：remote managed runtime 不可用
- 估时：1 轮

#### A10. **各 adapter 的 parse.ts**（每个 ~600 行，11 个 adapter）
- claude-local: parse.ts (507 行) - `parseClaudeStreamJson`, `claudeModelUsageTotals`, `describeClaudeFailure`, `detectClaudeLoginRequired`, `isClaudeProviderQuotaError`, `isClaudeTransientUpstreamError`, `isClaudeUnknownSessionError`, `isClaudePoisonedPreviousMessageIdError`, `isClaudeImageProcessingError`, `isClaudeModelNotFoundError`, `isClaudeMaxTurnsResult`, `isClaudeRefusalResult`, `extractClaudeRetryNotBefore`
- codex-local: parse.ts + codex-home.ts (795 行) + codex-auth-merge.ts (~600 行)
- 其他 adapter 类似规模
- 估时：每个 1 轮

### B. 中 ROI — 已经存在的功能需补完

#### B1. **heartbeat readiness 完整化**（R290 部分完成）
- 缺口：staleness / idempotent wake / 抑制 DB override

#### B2. **company-skills 深度**（routes 100% / 仓储 70%）
- fork / test-run 状态机

#### B3. **plugin worker→host 回调 + 生命周期恢复**

#### B4. **decisions / decision-training 仓储化**

### C. 低 ROI — 边缘功能

- folders / labels / routines / pipelines 完整迁移
- secrets AWS / GCP / Vault 真实解密
- cli auth bridge 完整化
- UI e2e 冒烟

## 三、模块大小 vs 完成度对比

### Node `adapter-utils` 完整模块盘点

| Node 模块 | 行数 | Rust 状态 | 缺口 |
|---|---|---|---|
| **execution-target.ts** | **1877** | 0% | **100%** |
| **ssh.ts** | **1862** | 0% | **100%** |
| **sandbox-callback-bridge.ts** | **1262** | 0% | **100%** |
| **sandbox-managed-runtime.ts** | **1224** | 0% | **100%** |
| **command-managed-runtime.ts** | **570** | 0% | **100%** |
| **local-process-sandbox.ts** | **509** | 0% | **100%** |
| **git-workspace-sync.ts** | **433** | 0% | **100%** |
| **sandbox-run-log-stream.ts** | **278** | 0% | **100%** |
| **workspace-restore-merge.ts** | **259** | 0% | **100%** |
| **remote-managed-runtime.ts** | **239** | 0% | **100%** |
| **session-compaction.ts** | **187** | 0% | **100%** |
| **runtime-progress.ts** | **170** | 0% | **100%** |
| **command-redaction.ts** | **58** | 0% | **100%** |
| **remote-execution-env.ts** | **49** | 0% | **100%** |
| **sandbox-install-command.ts** | **46** | 0% | **100%** |
| **exclude-patterns.ts** | **28** | 0% | **100%** |
| **sandbox-shell.ts** | **7** | 0% | **100%** |
| **billing.ts** | **20** | 0% | **100%** |
| server-utils.ts | 3415 | ~90% | 函数级差异若干 |
| **小计** | **~12,500 行** | | **~9,800 行未迁移** |

### 各 adapter parse.ts 缺口

| Adapter | Node parse.ts | Rust 状态 | 差距 |
|---|---|---|---|
| claude-local | 507 行 | 简易 parse | 大部分缺失 |
| codex-local | ~400 行 | 简易 parse | 大部分缺失 |
| gemini-local | ~200 行 | 简易 parse | 大部分缺失 |
| cursor-local | ~300 行 | 简易 parse | 大部分缺失 |
| opencode-local | ~200 行 | 简易 parse | 大部分缺失 |
| pi-local | ~300 行 | 简易 parse | 大部分缺失 |
| grok-local | ~200 行 | 简易 parse | 大部分缺失 |
| hermes | ~300 行 | 简易 parse | 大部分缺失 |
| cursor-cloud | ~300 行 | 简易 parse | 大部分缺失 |
| hermes-gateway | ~200 行 | 简易 parse | 大部分缺失 |
| openclaw-gateway | ~200 行 | 简易 parse | 大部分缺失 |
| **小计** | **~3,100 行** | | **~2,500 行未迁移** |

## 四、后续轮次建议

### 立即行动（3 轮内）

1. **R396**: 迁移 `pc-acpx::execution_target.rs` (Node 1877 行) — 最高 ROI，影响所有 11 adapter
2. **R397**: 迁移 `pc-acpx::local_process_sandbox.rs` (Node 509 行) + `pc-acpx::remote_execution_env.rs` (49 行) — sandbox scope
3. **R398**: 迁移 `pc-acpx::git_workspace_sync.rs` (Node 433 行) — git sync

### 中期（5-10 轮）

4. **R399-R402**: 各 adapter parse.ts 完整迁移（11 个 adapter × 1-2 轮）
5. **R403-R404**: `pc-acpx::sandbox_managed_runtime.rs` + `pc-acpx::sandbox_callback_bridge.rs`
6. **R405-R407**: `pc-acpx::command_managed_runtime.rs` + `pc-acpx::remote_managed_runtime.rs`

### 长期（15+ 轮）

7. **R408-R410**: `pc-acpx::ssh.rs` (1862 行)
8. **R411+**: pc-acpx-engine 剩余部分（turn 调度、ACP handshake、billing）

## 五、总体进度推算

| 时点 | 完成度 |
|---|---|
| R1-R257 | 0% → 81% |
| R257-R290 | 81% → 83% |
| R290-R319 | 83% → 85% |
| R319-R350 | 85% → 98% |
| R350-R395 | 98% → ~78%（注：发现更多缺口导致重新校准） |
| **R395 当前** | **~78%** |
| R396-R410 目标 | **~90%** |
| R420+ | **~95%** |

## 六、设计原则（已确立）

1. **高内聚低耦合**：每个模块独立、可单独测试
2. **零 I/O 优先**：pure 函数 / 零 async / 零 unsafe
3. **类型安全**：serde + newtype ID + Result<T, E>
4. **测试覆盖**：每个模块 ≥ 80% 覆盖，含 happy + ≥ 3 edge
5. **真实 PostgreSQL 验证**：涉及 DB 的模块必须跑真实 PG
6. **不破坏现有**：refactor 不引入 regression
