# 综合差距分析 (Post-R404) — paperclip-rs vs paperclip Node

**生成时间**:R404 完成时 (2026-08-08)
**pc-acpx lib 测试数**:748 (R396 起 670 → +78)
**pc-acpx `pub mod` 数**:64 (R396 起 53 → +11)
**Node adapter-utils 累计行数**:~24 685 (21 portable 文件)
**Rust pc-acpx 累计行数**:~33 964 (含注释、测试、纯 helpers)

---

## 一、pc-acpx adapter-utils 适配层

### 1.1 文件级 parity 表

| Node 源文件 | Node 行数 | Rust 模块 | 行数 | 轮次 | 状态 |
|--|--:|--|--:|--|--|
| `billing.ts` | — | `billing.rs` | — | R396 | ✅ 完整 |
| `command-redaction.ts` | — | `command_redaction.rs` | — | R396 | ✅ 完整 |
| `exclude-patterns.ts` | — | `exclude_patterns.rs` | — | R396 | ✅ 完整 |
| `remote-execution-env.ts` | — | `remote_execution_env.rs` | — | R396 | ✅ 完整 |
| `sandbox-install-command.ts` | — | `sandbox_install_command.rs` | — | R396 | ✅ 完整 |
| `runtime-progress.ts` | — | `runtime_progress.rs` | — | R397 | ✅ 完整 |
| `session-compaction.ts` | — | `session_compaction.rs` | — | R397 | ✅ 完整 |
| `local-process-sandbox.ts` | — | `local_process_sandbox.rs` | — | R398 | ✅ 完整 |
| `workspace-restore-merge.ts` | — | `workspace_restore_merge.rs` | — | R398 | ✅ 完整 |
| `git-workspace-sync.ts` | — | `git_workspace_sync.rs` | — | R399 | ✅ 完整 |
| `remote-managed-runtime.ts` | 239 | `remote_managed_runtime.rs` | — | R399 | ✅ 完整 |
| `command-managed-runtime.ts` | — | `command_managed_runtime.rs` | — | R400 | ✅ 同步部分 |
| `sandbox-callback-bridge.ts` | — | `sandbox_callback_bridge.rs` | — | R400 | ✅ 同步部分 |
| `sandbox-managed-runtime.ts` | 1224 | `sandbox_managed_runtime.rs` | — | R401 | ✅ 同步部分 |
| `execution-target.ts` | 1877 | `execution_target.rs` | 1877 | R402 | ✅ 同步部分 |
| `ssh.ts` | 1862 | `ssh.rs` | 1862 | R403 | ✅ 同步部分(SSH runner 延后) |
| `sandbox-run-log-stream.ts` | 278 | `sandbox_run_log_stream.rs` | 814 | **R404** | ✅ **完整** |
| `sandbox-shell.ts` | — | `sandbox_shell.rs` | — | pre-existing | ✅ 完整 |
| `log-redaction.ts` | — | `log_redaction.rs` | — | pre-existing | ✅ 完整 |
| **`server-utils.ts`** | **3415** | — | — | **R405+** | ⏳ **最大单缺口** |
| `types.ts` | ~120 | (散布在多模块) | — | partial | ⏳ 类型别名跨模块 |
| `index.ts` | re-exports | — | — | n/a | n/a |

### 1.2 估算进度

- **已完整移植**:**17/19** 个核心文件(89% 行数 parity,~21 000/24 685 行)
- **待移植**:1 个 (`server-utils.ts`,3415 行)
- **分散类型**:1 个 (`types.ts`,已部分拆分到对应模块)

### 1.3 异步延后清单

pc-acpx 当前所有"延后 async"的子模块清单(都需要真实 runtime):

| 模块 | 延后函数 | 真实 runtime |
|--|--|--|
| `sandbox_callback_bridge` | 桥 server / worker / queue client | SSH/bridge runtime |
| `sandbox_managed_runtime` | tar/tempfs/sync_in/sync_out | bubblewrap + SSH |
| `command_managed_runtime` | createClient / prepareClient | SSH sandbox |
| `remote_managed_runtime` | syncOut / syncIn 等 | SSH |
| `ssh` | createSshCommandManagedRuntimeRunner | tokio SSH |
| `execution_target` | 远程执行 runtime | SSH/sandbox |
| `sandbox_run_log_stream` | (R404 已实现 tail loop) | — |
| `local_process_sandbox` | spawn bwrap 子进程 | bubblewrap |
| `workspace_restore_merge` | 真实 fs I/O | tokio fs |

这些延后函数需要 SSH runtime + bubblewrap + 真实文件系统,无法在 pc-acpx 单元测试里完整验证。**它们最终要在 `pc-agent` 或某个 `pc-adapter-*` crate 里 wire 起来**(R405+ 后逐步)。

---

## 二、超出 adapter-utils 的范围

pc-acpx 只覆盖 `packages/adapter-utils/src/`。paperclip-rs 还有大量其他子系统需要从 Node 复刻:

### 2.1 当前 paperclip-rs workspace 状态

```
39 crates 总数
├── pc-acpx (103 .rs 文件,64 pub mod, 748 lib 测试)   ← 当前重点
├── 11 个 pc-adapter-* crate                                ← adapter 实现层
├── pc-agent (13 文件)                                     ← agent 核心
├── pc-core (61 文件)                                      ← 核心抽象
├── pc-heartbeat (142 文件)                                ← 心跳系统
├── pc-http (126 文件)                                     ← HTTP 服务
├── pc-repos (189 文件)                                    ← 仓库层
├── pc-plugin-host (21 文件)                               ← 插件宿主
├── 其他 (~10 个 crate,1-8 文件)                          ← 大多 stub
```

### 2.2 关键缺口

| 系统 | Node 源 | 状态 | 说明 |
|--|--|--|--|
| `server-utils.ts` | 3415 行 | ⏳ 0% | **最大单缺口**,需要多轮 |
| 11 个 `adapter-*/parse.ts` | ~4400 行 | ⏳ 0% | adapter 解析层 |
| 11 个 `adapter-*/execute.ts` | ~5500 行 | ⏳ 0% | adapter 执行层 |
| `gateway` runtime | 大量 | ⏳ | gateway crate |
| `auth` / `authz` / `db` schema | 大量 | 部分 | heartbeat 已用 |
| `plugin-host` 协议 | 大量 | 部分 | |
| `realtime` / `ws` | 中量 | 部分 | |

---

## 三、后续计划

### 3.1 短期(继续在 pc-acpx 内,1-2 天)

**R405** — `server-utils.ts` Part 1: 同步纯 helpers(~800 行)
- `RunProcessResult` / `RunningProcess` / `SpawnTarget` 类型
- `signalRunningProcess` (纯函数)
- `UNMANAGED_BACKGROUND_TASK_*` 常量
- `parseObject` / `asString` / `asNumber` / `asBoolean` / `asStringArray` / `parseJson`
- `appendWithCap` / `appendWithByteCap`
- `resolvePathValue` / `renderTemplate` / `joinPromptSections`
- `isPaperclipRuntimeEnvKey` / `isForbiddenConfigEnvKey`
- `MAX_CAPTURE_BYTES` / `MAX_EXCERPT_BYTES` / `PATH_SEGMENT_RE` 等常量

**R406** — `server-utils.ts` Part 2: env helpers(~700 行)
- `redactEnvForLogs` / `redactCommandTextForLogs`
- `buildInvocationEnvForLogs` / `buildPaperclipEnv`
- `applyPaperclipWorkspaceEnv` / `shapePaperclipWorkspaceEnvForExecution`
- `rewriteWorkspaceCwdEnvVarsForExecution`
- `refreshPaperclipWorkspaceEnvForExecution`
- `sanitizeInheritedPaperclipEnv` / `defaultPathForPlatform`
- `sanitizeSshRemoteEnv` / `ensurePathInEnv`

**R407** — `server-utils.ts` Part 3: skill entries(~1000 行)
- `PaperclipSkillEntry` / `PaperclipDesiredSkillEntry` / `InstalledSkillTarget`
- `resolvePaperclipInstanceRootForAdapter`
- `buildRuntimeMountedSkillSnapshot` / `buildPersistentSkillSnapshot`
- `parseObject` 等已 R405 覆盖

**R408** — `server-utils.ts` Part 4: wake payload + watchdog(~900 行)
- `PaperclipWakePayload` / `normalizePaperclipWakePayload` / `stringifyPaperclipWakePayload`
- `isPaperclipRecoveryWakePayload`
- `selectPaperclipTaskMarkdown` / `renderPaperclipWakePrompt`
- `WATCHDOG_DEFAULT_MANDATE` 等常量

### 3.2 中期(扩展 pc-acpx 或新建 crate)

**R409-R420** — 11 个 `adapter-*/parse.ts`(~400 行 × 11)
- 每个 adapter 一个 round
- 高度同构,大量复用

**R421-R432** — 11 个 `adapter-*/execute.ts`(~500 行 × 11)
- 真实 runtime 调用,需要 SSH/bubblewrap 已 wire 起来

### 3.3 长期(架构层)

- **wire SSH runner**:把 `createSshCommandManagedRuntimeRunner` 在 pc-acpx 内实现完,所有延后模块就能用真实 runner 测试
- **wire bubblewrap sandbox**:把 `local_process_sandbox` 的 bwrap spawn 真实接通
- **gateway crate 完整化**:`pc-hermes-gateway` / `pc-openclaw-gateway` 当前只是 stub
- **pc-agent 完整化**:agent 核心逻辑(~13 文件,大量 stub)

---

## 四、当前累计数据

| 维度 | 数据 |
|--|--|
| 累计移植 pc-acpx 模块 | 64 个 |
| 累计移植 Node 源 | ~21 000 行 |
| 累计添加 Rust 代码 | ~33 964 行(注释+测试+helper) |
| 累计 lib 测试 | 748 |
| 累计集成测试文件 | 37 个 |
| 证据文档 | 61 个(`docs/01-..`-`docs/61-..`) |
| 综合分析文档 | 2 个(`56-COMPREHENSIVE-GAP-ANALYSIS-2026-08-08.md`, `59-GAP-UPDATE-POST-R402.md`) |

---

## 五、风险与决策记录

1. **shell_quote 重复**:3 模块各持一份(R401/R403/R404)。Node 源也有相同重复。提取到 `pc_acpx::shell_quote` 是低风险重构,但需要 3 个模块 + 单元测试 + 集成测试同时改,工作量 ~150 行。**延后到 R405 后批量重构**。

2. **`CommandManagedRuntimeRunner` trait**:R404 引入最小 `SandboxRunLogRunner` trait 作为替代。完整的 Node trait 应该定义在 `command_managed_runtime.rs`,但需要 SSH runner wire 起来才能验证。**计划 R409 之前完成 trait 重构**。

3. **`RunProcessResult` 类型**:目前只有 `SandboxRunLogTickResult` 用到。R405 会把它正式放到 `pc_acpx::server_utils::process` 下,作为通用 spawn result。

4. **`types.ts` 拆分**:Node 的 `types.ts` 是 ~120 行类型别名,大部分已经被各模块直接镜像了(`SshRemoteExecutionSpec` 等)。剩余的纯类型别名在 R405 一起搬。

5. **`@paperclip` 风格注释**:Node 源码用了 JSDoc 块;Rust 用 `///` doc comments。R396-R404 全部 1:1 保留了关键 doc(常量含义、参数说明、defer 备注)。
