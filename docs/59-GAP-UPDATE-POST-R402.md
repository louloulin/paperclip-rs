# Paperclip-rs 差距更新 (Post-R402, 2026-08-08)

> 增量更新基于 `56-COMPREHENSIVE-GAP-ANALYSIS-2026-08-08.md`(R400 完成)。
> 本次快照新增 R401 + R402 两轮。

## 本轮新增 (R401 + R402)

| Round | 模块 | Node 行数 | Rust 行数 | lib 测试 | 集成测试 |
|---|---|---|---|---|---|
| R401 | `sandbox_managed_runtime.rs` | 1224 | 1108 | +39 | +25 |
| R402 | `execution_target.rs` (最大) | 1877 | 1638 | +51 | +38 |
| 累计 | — | **3101** | **2746** | **+90** | **+63** |

## 当前快照 (Post-R402)

| 维度 | 数值 |
|---|---|
| pc-acpx `pub mod` 模块数 | **62** (R400: 60 → R401 +1 sandbox_managed → R402 +1 execution_target) |
| pc-acpx lib 测试通过数 | **709** (R400: 619 → R401 +39 → R402 +51) |
| pc-acpx 集成测试文件数 | **36** |
| 总 test 计数 | lib 709 + 集成 36 文件 ≈ **1200+ 测试** |

## adapter-utils 移植进度

| Node 模块 | 行数 | R-port 状态 |
|---|---|---|
| `billing.ts` | ~ | ✅ R396 |
| `command-redaction.ts` | ~ | ✅ R396 |
| `exclude-patterns.ts` | ~ | ✅ R396 |
| `sandbox-shell.ts` | 7 | ✅ R396 |
| `sandbox-install-command.ts` | 46 | ✅ R396 |
| `remote-execution-env.ts` | 49 | ✅ R396 |
| `runtime-progress.ts` | 170 | ✅ R397 |
| `session-compaction.ts` | 187 | ✅ R397 |
| `local-process-sandbox.ts` | 509 | ✅ R398 |
| `workspace-restore-merge.ts` | 259 | ✅ R398 |
| `git-workspace-sync.ts` | 433 | ✅ R399 |
| `remote-managed-runtime.ts` | 239 | ✅ R399 |
| `command-managed-runtime.ts` | 319 | ✅ R400 |
| `sandbox-callback-bridge.ts` | 1262 | ✅ R400 |
| `sandbox-managed-runtime.ts` | 1224 | ✅ R401 |
| `execution-target.ts` | 1877 | ✅ R402 (本轮) |
| **`ssh.ts`** | **1862** | **❌ R403 计划** |
| **`server-utils.ts`** | **3415** | **❌ 大单缺口,需拆分** |
| `sandbox-run-log-stream.ts` | 278 | ❌ 延后(依赖 stream) |
| `types.ts` | 609 | 部分分散(~30%) |
| `index.ts` | 92 | (re-export,不需要移植) |

**核心统计**:
- ✅ **16/22 Node adapter-utils 模块已移植**(73% 文件数)
- ❌ **3 个核心模块未移植**(`ssh.ts` 1862、`server-utils.ts` 3415、`sandbox-run-log-stream.ts` 278)
- 合计未移植 **5,555 行 Node** (约 adapter-utils 23%)

## 下一轮计划 (R403)

### 🔵 R403 — `pc-acpx::ssh.rs` 纯函数层 (Node 1862 行)
**目标**:SSH 客户端的纯函数部分:
- 路径 quoting、protocol 字符串、env 拼接
- `parseSshRemoteExecutionSpec` (已于 R402 内联,这里正式抽出独立模块)
- `shellQuote` (SSH 内部 helper,可能与 `command_managed_runtime::shell_quote` 同名)
- error classification (host unreachable, auth fail, timeout)
- `buildKnownHostsEntry`

**估时**:1 轮
**交付**:
- `ssh.rs` (~600 行纯函数)
- `pc_acpx::execution_target::SshRemoteExecutionSpec` 等类型迁移到 `pc_acpx::ssh`
- `pc_acpx::remote_managed_runtime` 同步调整
- 集成测试文件 `tests/round403_ssh_pure.rs`
- `docs/60-ROUND403-SSH-PURE.md`

### 🔵 R404 — `pc-acpx::sandbox_run_log_stream.rs` (Node 278 行)
**目标**:sandbox run log 增量 streaming,使用 `tokio::sync::mpsc` 异步实现。
**估时**:1 轮
**交付**:
- `sandbox_run_log_stream.rs` (~250 行)
- 集成测试文件 `tests/round404_run_log_stream.rs`
- `docs/61-ROUND404-RUN-LOG-STREAM.md`

### 🔵 R405 — `pc-acpx::server_utils.rs` (Node 3415 行)
**目标**:server-utils 大单分批移植 — 第一批聚焦 HTTP routing + 中间件 + 会话工具。
**估时**:2-3 轮
**交付**:
- `server_utils.rs` 分批 (mid-batch)
- 集成测试文件 `tests/round405_server_utils_part1.rs`
- `docs/62-ROUND405-SERVER-UTILS-PART1.md`

## 长期路线图

完成 R403-R405 后:

| 阶段 | 内容 | 估时 |
|---|---|---|
| R403-R405 | ssh + run-log-stream + server-utils 第一批 ~3500 行 | 4-5 轮 |
| R406-R410 | server-utils 第二、三批(HTTP / middleware / session) | 6-8 轮 |
| R411-R415 | 各 adapter `parse.ts` 完整实现(~400 行 × 11) | 8-10 轮 |
| R416-R420 | adapter execute / start / stream 部分 | 10-15 轮 |
| 平行 | pc-adapter-acpx-engine 完整化、pc-plugin-host 完成 | 持续 |

**预计整体进度**:
- **2026 Q4 中**:~88% (pc-acpx 完整、SSH 协议层 ready、所有 11 adapter parse.ts 完成)
- **2027 Q1 末**:~95% (所有 remote / sandbox lane work)
- **2027 Q2 末**:~99% (server.ts 完整 + 全 parity)

## 关键决策持续

1. **Async 路径**:继续 pc-acpx 纯 helper 风格,async 在 pc-core / pc-adapter-* 层
2. **测试覆盖**:每个模块 ≥10 单元 + ≥6 集成测试 (happy + ≥3 edge case)
3. **结构**:每个模块独立 .rs 文件 + 单 `pub mod`,无相互依赖
4. **Node 优先 100% parity**:纯函数部分严格 1:1 (async 边界明确注释)
5. **文档同步**:每轮 docs/RR-ROUND{NNN}-*.md 证据文档

## 答用户问题 (Post-R402)

> 与 paperclip Node 比较,差距多大?

**最新量化**:
- **adapter-utils**:Node 22 模块,Rust 已移植 **16/22 = 73% 文件、~77% 行数**
- **整体后端**:~**82% parity** (Post-R400: 80%, Post-R402: +2pp)

**剩余 ~18% 分布**:
- **~5%**: ssh + server-utils runtime 路径 (async 重活)
- **~5%**: adapter execute / start / stream 完整化
- **~3%**: pc-adapter-acpx-engine 完整化
- **~2%**: test infrastructure / fixture 完整化
- **~1%**: edge case / 文档同步

**结论**:剩余 ~18% 主要集中在 async runtime 层和 adapter 完整化。
第一阶段目标 (年底前 ~88%) 已具备条件,所有 14 个 adapter parse + 8 个
核心 helper 已就位。
