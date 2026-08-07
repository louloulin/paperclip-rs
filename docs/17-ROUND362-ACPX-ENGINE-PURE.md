# Round 362 — Acpx-engine 纯函数模块启动 (B3.1)

> 适用版本：`paperclip-rs` 截至 R362（R360 = 928 → R362 = **975**，+47 pc-acpx 测试）
> 参考实现：`paperclip` Node（`packages/adapter-utils/src/acpx-engine/`）
> 测试基线：`cargo test -p pc-acpx` 47/47 绿；`pc-heartbeat` 928/928 全量无回归；`cargo build --workspace --bins` 通过；`cargo fmt --all -- --check` 通过

---

## 🎯 R362 目标

启动 **acpx-engine** 的 Rust 化迁移（B3.1），从最大单一缺口 3500+ 行 Node 中
抽出**纯函数**层：

1. 创建新 crate `pc-acpx`（加入 workspace）
2. 移植 6 个 Node 文件中的所有 pure function：`constants.ts`、`session-codec.ts`、`execute.ts`（pure 部分）、`ui.ts`、`usage` 部分
3. TDD 流程：先写测试 → 看红 → 实现 → 看绿
4. 与 pc-heartbeat 现有 928 测试无回归

**为什么从纯函数开始**：这部分无 I/O、无副作用、可独立验证，移植风险最低，
可以为后续 I/O 部分（spawn / sandbox / staging）铺设数据模型基座。

---

## 🏗️ 新 Crate 架构

```
crates/pc-acpx/
├── Cargo.toml                         # 6 依赖 (serde/regex/sha2/hex/chrono/thiserror)
├── src/
│   ├── lib.rs                         # 模块入口 + 公开 API 列表
│   ├── constants.rs                   # DEFAULT_ACP_ENGINE_* + ACPX_ADAPTER_AGENT_IDS
│   ├── gemini_version.rs              # parse_gemini_version_parts / supports / rewrite / tokenize
│   ├── session_codec.rs               # AcpxSessionParams + deserialize/serialize/get_display_id
│   ├── hash.rs                        # stable_json + short_hash (SHA-256)
│   ├── normalize.rs                   # normalize_agent/mode/permission_mode/...
│   ├── transcript.rs                  # parse_acpx_stdout_line + TranscriptEntry
│   └── usage.rs                       # summarize_acpx_turn_usage + 友方输入/输出类型
└── tests/
    └── round362_milestone.rs          # 8 个跨模块集成测试
```

### 高内聚低耦合设计原则

- **每个模块单一职责**：6 个文件各自负责一个独立关注点
- **pure 函数无副作用**：所有公共函数均为 `&Value` → `T` 类型，不读 DB、不写文件
- **DB / I/O 完全隔离**：本 crate 不引入 `sqlx` / `tokio` / `fs`，零数据库依赖
- **类型边界明确**：序列化用 `serde::{Serialize, Deserialize}`，所有公开类型可round-trip
- **失败模式收敛**：所有"未知值"路径统一返回 `default` / `None`，不抛错

---

## 🔧 R362 实现的 14 个函数

### `constants.rs` ✅

| 函数 | Node 对应 |
|---|---|
| `DEFAULT_ACP_ENGINE_AGENT/MODE/...` 常量 | `index.ts` 默认值 |
| `acpx_agent_id_for_adapter_type(...)` | `acpxAgentIdForAdapterType` |
| `ACPX_ADAPTER_AGENT_IDS` 表 | `ACPX_ADAPTER_AGENT_IDS` |

### `gemini_version.rs` ✅ (4 个)

| Rust | Node |
|---|---|
| `parse_gemini_version_parts(output: Option<&str>) -> Option<[u32;3]>` | `parseGeminiVersionParts` |
| `gemini_version_supports_native_acp_flag(parts: Option<[u32;3]>) -> bool` | `geminiVersionSupportsNativeAcpFlag` |
| `rewrite_gemini_acp_flag_for_version(shell: &str, parts: Option<[u32;3]>) -> String` | `rewriteGeminiAcpFlagForVersion` |
| `gemini_acp_command_tokens(shell: &str) -> Option<Vec<&str>>` | `geminiAcpCommandTokens` |

### `session_codec.rs` ✅ (3 个)

| Rust | Node |
|---|---|
| `deserialize(raw: &Value) -> Option<AcpxSessionParams>` | `sessionCodec.deserialize` |
| `serialize(params: Option<&AcpxSessionParams>) -> Option<Value>` | `sessionCodec.serialize` |
| `get_display_id(params: Option<&AcpxSessionParams>) -> Option<String>` | `sessionCodec.getDisplayId` |
| `AcpxSessionParams` 类型 | 内嵌 interface |

### `hash.rs` ✅ (2 个)

| Rust | Node |
|---|---|
| `stable_json(value: &Value) -> String` | `stableJson` |
| `short_hash(value: &Value) -> String` | `shortHash` (SHA-256 hex) |

### `normalize.rs` ✅ (5 个 + 3 个 enum)

| Rust | Node |
|---|---|
| `normalize_agent(config: &Value) -> String` | `normalizeAgent` |
| `normalize_mode(config: &Value) -> NormalizedMode` | `normalizeMode` |
| `normalize_permission_mode(config: &Value) -> NormalizedPermissionMode` | `normalizePermissionMode` |
| `normalize_non_interactive_permissions(config: &Value) -> NormalizedNonInteractivePermissions` | `normalizeNonInteractivePermissions` |
| `normalize_requested_thinking_effort(config: &Value) -> Option<String>` | `normalizeRequestedThinkingEffort` |

### `transcript.rs` ✅ (1 个 + 1 个)

| Rust | Node |
|---|---|
| `parse_acpx_stdout_line(line: &str, ts: &str) -> Vec<TranscriptEntry>` | `parseAcpxStdoutLine` |
| `summarize_tool_call(entry: &TranscriptEntry) -> Option<ToolCallSummary>` | 新增 helper |
| `TranscriptEntry` enum (8 变体) | `TranscriptEntry` (re-tagged) |

### `usage.rs` ✅ (1 个 + 1 个)

| Rust | Node |
|---|---|
| `summarize_acpx_turn_usage(input: &SummarizeAcpxTurnUsageInput) -> SummarizeAcpxTurnUsageOutput` | `summarizeAcpxTurnUsage` |
| `summarize_from_value(pre, post, event, cost_usd) -> ...` | 新增 (raw serde_json::Value 入口) |
| `AcpxRuntimeStatusView` / `AcpxRuntimeUsageView` / `AcpxTurnUsageBreakdown` / `AcpxTurnUsageCost` | 内嵌 interface |
| `SummarizeAcpxTurnUsageInput` / `SummarizeAcpxTurnUsageOutput` / `UsageSummary` | 镜像输出 |

---

## 📊 R362 测试覆盖

| 测试类型 | 数量 | 位置 |
|---|---|---|
| 单元测试 | **39** | `src/*/tests` 内联 |
| 集成测试 | **8** | `tests/round362_milestone.rs` |
| **pc-acpx 合计** | **47** | |
| pc-heartbeat 全量回归 | **928** | 无变化 |

### 关键测试覆盖

- `parse_gemini_version_parts`：`"0.30.0"` → `[0,30,0]`；"`gemini-cli v1.2.3\n`" → `[1,2,3]`；乱码 → `None`；`null` 输入 → `None`
- `gemini_version_supports_native_acp_flag`：`>=0.33.0` → `true`；`<0.33.0` → `false`；`None` → `true`
- `rewrite_gemini_acp_flag_for_version`：旧版 `--acp` → `--experimental-acp`；新版保留 `--acp`
- `summarize_acpx_turn_usage`：6 个场景覆盖 stale / event fallback / non-USD 拒绝 / cost 单一报告
- `parse_acpx_stdout_line`：8 种事件类型 + 非 JSON 回退 + unknown acpx.* → system
- `session_codec.round_trip`：14 字段完整往返
- `stable_json` 跨 key 顺序稳定、数组保留顺序、嵌套对象递归

---

## 🧪 验证基线

```bash
cd /Users/louloulin/Documents/lumosaipaperclip/paperclip-rs

# 1. pc-acpx 全量（47/47 绿）
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

### 1. `OnceLock` 替代 `once_cell::Lazy`

```rust
fn version_regex() -> &'static regex::Regex {
    static REGEX: OnceLock<regex::Regex> = OnceLock::new();
    REGEX.get_or_init(|| regex::Regex::new(r"(\d+)\.(\d+)\.(\d+)").expect("static"))
}
```

→ `std::sync::OnceLock` 自 Rust 1.70 起稳定，无需引入 `once_cell` 依赖。

### 2. `TranscriptEntry` 改为 `enum` 而非 `struct` with `kind`

```rust
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TranscriptEntry {
    Init { ts, model, session_id },
    Thinking { ts, text, delta },
    Assistant { ts, text, delta },
    ToolCall { ts, name, tool_use_id, input },
    ToolResult { ts, tool_use_id, tool_name, content, is_error },
    System { ts, text },
    Result { ts, text, input_tokens, output_tokens, ... },
    Stderr { ts, text },
    Stdout { ts, text },
}
```

→ Rust idiom、模式匹配穷尽性、序列化自动应用 `kind` tag。

### 3. `summarize_acpx_turn_usage` 提供两层 API

- **强类型 API**：`summarize_acpx_turn_usage(&SummarizeAcpxTurnUsageInput)`
- **弱类型 API**：`summarize_from_value(pre, post, event, cost_usd)` — 直接吃 `serde_json::Value`

→ 解耦核心逻辑 vs 外部集成：heartbeat_runs.result_json 可以直接走弱类型；新调用方推荐强类型。

### 4. `BTreeMap<String, Value>` 替代 `serde_json::Map`

在 `AcpxSessionParams.remote_execution` 和 `summarize_acpx_turn_usage.usage_detail` 中，
为了稳定迭代顺序 + 与 `serde_json::Value` 兼容性，选用 `BTreeMap` 而非 `HashMap`。

### 5. `parse_gemini_version_parts` 返回 `[u32; 3]` 而非 `Vec<u32>`

栈分配的固定大小数组，零分配开销，比 `Vec<u32>` 更严格：缺失字段会编译时报错。

### 6. 公共 API 集中再导出

```rust
// lib.rs
pub use constants::{acpx_agent_id_for_adapter_type, ...};
pub use gemini_version::{...};
pub use hash::{short_hash, stable_json};
pub use normalize::{...};
pub use session_codec::{...};
pub use transcript::{...};
pub use usage::{...};
```

→ 调用方 `use pc_acpx::summarize_acpx_turn_usage;` 即可，不必深入子模块。

---

## 📋 后续 R363+ 计划

### R363 (下一轮) — acpx-engine I/O 入口（最小可集成）

- `prepare_engine_settings` (`resolveEngineSettings`) — 合并 caller / injected / defaults
- `find_ancestor_bin` — 简单 I/O，向上查找 PATH 中的 binary
- `write_file_atomically` — 唯一需要的 atomic write 工具
- `path_exists` / `ensure_parent_dir` — 文件系统基础

### R364+ — execute.ts 主流程

- `buildRuntime` 流程拆解
- `warmHandles` / `stagedRuntimes` 缓存层（带 OnceLock）
- `acp.handshake` 协议调用
- `summarizeAcpxStreamEvent` 替代 Node `printAcpxStreamEvent`

### R365+ — 错误恢复 + 启动 timing

- `classifyError` / `describeErrorDiagnostics`
- `readChildStderrTail` / `routeChildStderr`
- `buildStartupStepMetrics` / `openStartupRootSpan`
- `startup-timing.ts`（304 行）

### R366+ — Sandbox staging seam

- `prepareAdapterExecutionTargetRuntime`
- `stageAcpRemoteRuntime`
- `startAdapterExecutionTargetPaperclipBridge` / `startAdapterExecutionTargetProcessSessionBridge`

---

## 📝 总结

**R362 启动 acpx-engine Rust 化迁移（B3.1 第一阶段），从 3500+ 行 Node 中抽出 14 个 pure 函数**，按 6 个独立模块拆分，全部 TDD 验证：

- **新增 crate**：`pc-acpx`（加入 workspace，6 依赖）
- **代码规模**：47 个测试覆盖 14 个函数 + 9 个 enum + 6 个模块
- **毫无回归**：pc-heartbeat 928 测试完全保持
- **高内聚低耦合**：6 个模块各自独立，pure 函数无 I/O
- **最佳 Rust 实践**：`enum` + `serde(tag)` 替代 `interface`；`OnceLock` 替代 `once_cell`；`BTreeMap` 稳定迭代
- **完成度**：acpx-engine 14/21+ 关键函数已迁移（~67% 纯函数层）

**下一步**：R363 启动 I/O 层（`find_ancestor_bin` + `resolve_engine_settings` + `write_file_atomically`），继续 B3.1 第二阶段。
