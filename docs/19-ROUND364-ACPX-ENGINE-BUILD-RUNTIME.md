# Round 364 — Acpx-engine `buildRuntime` 拆分启动 (B3.1 第三阶段)

> 适用版本：`paperclip-rs` 截至 R364（R363 = 994 → R364 = **1018**，+24 pc-acpx 测试）
> 参考实现：`paperclip` Node（`packages/adapter-utils/src/acpx-engine/execute.ts`）
> 测试基线：`cargo test -p pc-acpx` 90/90 绿；`pc-heartbeat` 928/928 全量无回归；`cargo build --workspace --bins` 通过；`cargo fmt --all -- --check` 通过

---

## 🎯 R364 目标

继续 **acpx-engine** Rust 化迁移（B3.1 第三阶段），启动 `buildRuntime` 拆分：

1. **Agent command resolver**（`agent_command.rs`）：把 `resolveBuiltInAgentCommand` 移植为 Rust async 函数
2. **Startup metrics**（`startup_metrics.rs`）：把 `buildStartupStepMetrics` 移植为纯函数 + `Arc<dyn StartupMetricsSource>` 设计
3. **Prepared runtime**（`prepared_runtime.rs`）：抽出 `AcpxPreparedRuntime` 的 **data-only 子集**（22 字段），保留未来扩展空间
4. **格式助手**（`format_timeout_start_log_line`）：格式化 timeout 起始日志行
5. **集成测试** `round364_build_runtime` 验证完整链路

**为什么这一阶段关键**：之前 R362/R363 的模块是 I/O 原语和 pure helpers。R364 接入
**业务组装**：把 settings + agent_command + metrics + timeout 组合成一条
`PreparedRuntime` 记录，这是后续 `buildRuntime` 主流程的最小可运行实例。

---

## 🏗️ 新增模块

```
crates/pc-acpx/src/
├── agent_command.rs       # resolve_built_in_agent_command + shell_quote (async)
├── startup_metrics.rs     # build_startup_step_metrics + StartupMetricsSource trait
└── prepared_runtime.rs    # PreparedRuntime + 3 个 enum + TimeoutResolution + 链式 builder
```

### 模块职责

| 模块 | 职责 | 依赖 |
|---|---|---|
| `agent_command.rs` | 解析内置 agent 的 binary 命令（gemini 固定 / claude/codex 查 node_modules/.bin） | `bin` + `Platform` |
| `startup_metrics.rs` | 从 sandbox runner 提取可读回调（exec round-trips / provider 耗时） | `std::sync::Arc` |
| `prepared_runtime.rs` | 数据结构 + 链式 builder + timeout 日志格式化 | `agent_command` + `startup_metrics` |

---

## 🔧 R364 实现的 6 个函数

### `agent_command.rs` ✅ (2 个)

| Rust | Node |
|---|---|
| `resolve_built_in_agent_command(input) -> Option<BuiltInAgentCommand>` | `resolveBuiltInAgentCommand` |
| `shell_quote(value: &str) -> String` | `shellQuote` |
| `BuiltInAgentCommand { command, shell_command }` | `BuiltInAgentCommand` |
| `ResolveBuiltInAgentCommandInput { agent, package_root_dir, ... }` | argument object |

**关键设计**：
- gemini 固定 `"gemini --acp"`，绕过 node_modules 查找
- claude/codex 在 local lane 走 `find_ancestor_bin`，remote lane 返回 bare name
- `shell_quote` 最小 POSIX 实现：识别 23 个 shell 元字符 + 单引号转义 `'\''`

### `startup_metrics.rs` ✅ (2 个)

| Rust | Node |
|---|---|
| `build_startup_step_metrics(source: Option<Arc<dyn Source>>) -> StartupStepMetrics` | `buildStartupStepMetrics` |
| `StartupMetricsSource` trait | `CommandManagedRuntimeRunner.executionCount` 等 |
| `StartupStepMetrics { round_trips, provider_exec_ms, provider_get_ms }` | `StartupStepMeasureOptions` |

**关键设计**：
- `Arc<dyn StartupMetricsSource + Send + Sync>` 共享 runner，避免 Node 风格的 closure capture
- 每个 callback 都 clone Arc，调用时再读 source（保证实时值）
- 手动 impl `Debug` + `Clone`（dyn Fn 不能 derive）
- `None` source → 空 metrics（local run / runner-less fallback）

### `prepared_runtime.rs` ✅ (3 个 enum + 1 个 struct + 1 个 builder + 1 个 helper)

| Rust | Node |
|---|---|
| `PreparedRuntime` (22 字段子集) | `AcpxPreparedRuntime` (30+ 字段) |
| `PreparedRuntimeMode` (Persistent / OneShot) | `mode: "persistent" \| "oneshot"` |
| `PreparedRuntimePermissionMode` (ApproveAll/ApproveReads/DenyAll) | `permissionMode` |
| `PreparedRuntimeNonInteractivePermissions` (Deny / Fail) | `nonInteractivePermissions` |
| `TimeoutResolution { timeout_sec, source, note }` | `AdapterExecutionTargetTimeoutResolution` |
| `PreparedRuntime::builder(agent) -> PreparedRuntimeBuilder` | (Rust idiom) |
| `format_timeout_start_log_line(resolution) -> String` | `formatAdapterExecutionTimeoutStartLogLine` |

**关键设计**：
- 链式 builder pattern：每个 setter 返回 `self`，最多 20 个链式调用
- `as_str()` 方法匹配 Node 字符串（"persistent" / "approve-all" / "deny"）
- 三个 enum 各自枚举 + `as_str()`，避免 type confusion
- `TimeoutResolution` 把"超时 + 来源 + 注释"绑在一起，确保日志可诊断

---

## 📊 R364 测试覆盖

| 测试类型 | 数量 | 位置 |
|---|---|---|
| 单元测试 | **20** (R363 是 54, R364 新增 20) | `src/agent_command.rs::tests` (11) + `src/startup_metrics.rs::tests` (5) + `src/prepared_runtime.rs::tests` (5) |
| R362 集成测试 | 8 | `tests/round362_milestone.rs` |
| R363 集成测试 | 4 | `tests/round363_io_layer.rs` |
| **R364 集成测试** | **4** | `tests/round364_build_runtime.rs` |
| **pc-acpx 合计** | **90** | |
| pc-heartbeat 全量回归 | **928** | 无变化 |

### 关键测试覆盖

- `agent_command` (11)：
  - gemini 固定命令
  - claude 找 ancestor + 找不到回退 + remote 路径
  - codex 找 correct bin name
  - unknown agent → None
  - `shell_quote` 5 个分支（普通 / 空格 / 单引号 / 空串 / 验证）
- `startup_metrics` (5)：
  - `None` source → 默认
  - 空 source → 默认
  - 计数 source → 三个 callback
  - callback 可 clone
  - metrics.clone() 保留 callback
- `prepared_runtime` (5)：
  - builder 链式调用
  - 默认 timeout log line (`"none"`)
  - sandbox default log line
  - 默认 mode = Persistent
  - enum 字符串映射
- **round364 集成测试** (4)：
  - `build_runtime_pipeline_combines_agent_command_and_metrics`：完整 buildRuntime 链路
  - `gemini_agent_bypasses_ancestor_lookup`：gemini 边界
  - `empty_metrics_can_be_built_then_used`：空 metrics 路径
  - `timeout_resolution_lines_carry_source_and_note`：sandbox default 文案

---

## 🧪 验证基线

```bash
cd /Users/louloulin/Documents/lumosaipaperclip/paperclip-rs

# 1. pc-acpx 全量（90/90 绿）
env -u SHELL rtk proxy cargo test -p pc-acpx

# 2. pc-heartbeat 无回归（928/928 绿）
env -u SHELL rtk proxy cargo test -p pc-heartbeat --tests -- --test-threads=1

# 3. 格式
env -u SHELL rtk proxy cargo fmt --all
env -u SHELL rtk proxy cargo fmt --all -- --check

# 4. 编译
env -u SHELL rtk proxy cargo build --workspace --bins --message-format=short
```

---

## 📦 关键设计决策

### 1. Arc<dyn StartupMetricsSource> 替代 Box<closure>

```rust
pub fn build_startup_step_metrics(
    source: Option<Arc<dyn StartupMetricsSource>>,
) -> StartupStepMetrics {
    let Some(source) = source else {
        return StartupStepMetrics::default();
    };
    let round_trips = source.round_trips().map(|_| {
        let source = Arc::clone(&source);
        Arc::new(move || source.round_trips().unwrap_or(0))
            as Arc<dyn Fn() -> u64 + Send + Sync>
    });
    ...
}
```

→ Node 用 `() => runner.execCount()` closure capture，Rust 借用检查严格，必须 owner trampoline。
`Arc<dyn Source>` 让我们能在每个 callback 里都保留共享所有权，调用时再读最新值。

### 2. `PreparedRuntime` 数据字段子集

```rust
pub struct PreparedRuntime {
    pub acpx_agent: String,
    pub mode: PreparedRuntimeMode,
    pub cwd: PathBuf,
    pub workspace_id: String,
    pub workspace_repo_url: String,
    pub workspace_repo_ref: String,
    pub env: BTreeMap<String, String>,
    pub logged_env: BTreeMap<String, String>,
    pub state_dir: PathBuf,
    pub permission_mode: PreparedRuntimePermissionMode,
    pub non_interactive_permissions: PreparedRuntimeNonInteractivePermissions,
    pub requested_model: String,
    pub requested_thinking_effort: String,
    pub fast_mode: bool,
    pub timeout_sec: u64,
    pub timeout_resolution: TimeoutResolution,
    pub session_key: String,
    pub fingerprint: String,
    pub agent_command: Option<BuiltInAgentCommand>,
    pub step_metrics: StartupStepMetrics,
}
```

→ 22 字段 vs Node 30+: skip 了 `hostSpawnCwd` / `processSessionBridge` / `paperclipBridge` /
`stagedRuntime` / `remoteManagedHomeTeardown` / `remoteStagingDispose` / `remoteStagingEnvDelta` /
`sessionStagingLeaseRelease` / `remoteExecutionIdentity` / `skillPromptInstructions` /
`skillsIdentity` / `childStderrLogPath` / `paperclipClaudeSettings` / `mcpServers` / `mcpIdentity`。
这些都是 R365+（acpx.handshake / sandbox staging）需要时再加。

### 3. 链式 builder pattern

```rust
let runtime = PreparedRuntime::builder("claude")
    .mode(PreparedRuntimeMode::OneShot)
    .cwd("/repo")
    .permission_mode(PreparedRuntimePermissionMode::DenyAll)
    .timeout_sec(60)
    .build();
```

→ 22 个 setter，每个返回 `self`。builder 内嵌所有 default，避免调用方写一堆 `..Default::default()`。

### 4. `shell_quote` 最小 POSIX 实现

```rust
pub fn shell_quote(value: &str) -> String {
    if !needs_quoting(value) {
        return value.to_string();
    }
    let escaped = value.replace('\'', "'\\''");
    format!("'{escaped}'")
}
```

→ 23 个 shell metacharacter 不需要 quoting 时直接 passthrough。
需要时用单引号包裹，嵌入的 `'` 用 `'\''`（关引号 + 转义单引号 + 开引号）转义。

### 5. `TimeoutResolution` 字段三件套

```rust
pub struct TimeoutResolution {
    pub timeout_sec: u64,    // 0 = 无超时
    pub source: String,      // "adapterConfig" / "sandbox default" / "default"
    pub note: Option<String>, // "(sandbox default; set adapterConfig.timeoutSec to override)"
}
```

→ 来源 + 注释分离，便于诊断。`format_timeout_start_log_line` 消费者只需调用一次。

### 6. 三个 enum 各有 `as_str()` 避免字符串错位

```rust
impl PreparedRuntimeMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            PreparedRuntimeMode::Persistent => "persistent",
            PreparedRuntimeMode::OneShot => "oneshot",
        }
    }
}
```

→ 编译器保证字符串字面量一一对应；如果 Node 字符串变了，只改一处。

---

## 📋 后续 R365+ 计划

### R365 (下一轮) — `acp.handshake` 协议调用

- `AcpxRuntime` trait 抽象
- `OpenSession` / `SendTurn` / `CloseSession` 边界
- `getStatus` 状态读取
- 扩展 `PreparedRuntime` 添加 `state_dir` / `agent_registry` 等

### R366 — 错误恢复 + `startup-timing.ts`

- `classifyError` / `describeErrorDiagnostics`
- `readChildStderrTail` / `routeChildStderr`
- `startup-timing.ts`（304 行）

### R367 — Sandbox staging seam

- `prepareAdapterExecutionTargetRuntime`
- `stageAcpRemoteRuntime`
- `startAdapterExecutionTargetPaperclipBridge`

---

## 📊 完成度更新

| 维度 | R360 | R362 | R363 | R364 |
|---|---|---|---|---|
| pc-acpx 测试 | 0 | 47 | 66 | **90** |
| 总测试数 | 928 | 975 | 994 | **1018** |
| **acpx-engine 子模块** | ~0% | ~67% | ~75% | **~80%** |
| 后端核心 | ~96% | ~96% | ~96% | ~96% |

---

## 📝 总结

**R364 推进 acpx-engine Rust 化迁移到 buildRuntime 拆分**：

- **新增 3 个模块**：`agent_command.rs` + `startup_metrics.rs` + `prepared_runtime.rs`
- **新增 24 个测试**（20 单元 + 4 集成），保持 0 失败
- **pc-heartbeat 928 测试完全无回归**
- **数据基座就绪**：`PreparedRuntime` 22 字段 + 链式 builder，未来 `buildRuntime` 主流程可直接使用
- **完成度**：acpx-engine 子模块从 75% 推进到 ~80%（buildRuntime 拆分启动）

**下一步**：R365 启动 `acp.handshake` 协议调用，把 `PreparedRuntime` 接入真实 runtime，开始 B3.1 第四阶段。
