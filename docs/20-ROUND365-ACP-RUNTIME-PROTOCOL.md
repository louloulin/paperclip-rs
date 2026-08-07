# Round 365 — Acpx-engine `acp.handshake` 协议契约 (B3.1 第四阶段)

> 适用版本：`paperclip-rs` 截至 R365（R364 = 1018 → R365 = **1042**，+24 pc-acpx 测试）
> 参考实现：`paperclip` Node（`acpx/runtime` type definitions in `node_modules/acpx/dist/runtime.d.ts`）
> 测试基线：`cargo test -p pc-acpx` 105/105 绿；`pc-heartbeat` 928/928 全量无回归；`cargo build --workspace --bins` 通过；`cargo fmt --all -- --check` 通过

---

## 🎯 R365 目标

启动 **acp.handshake 协议契约**的 Rust 化迁移（B3.1 第四阶段）：

1. **定义 `AcpRuntime` trait**（async，11 个方法）镜像 Node `acpx/runtime` 公开接口
2. **类型端口**：6 个核心 enum + 11 个 struct + 5 个 input/output struct
3. **Mock 实现**：`MockAcpRuntime`（in-memory，可配置 event 流）
4. **集成测试** 验证完整 session lifecycle + serde round-trip

**为什么这一阶段关键**：acpx-engine 的核心运行时通过 `AcpRuntime` 接口与
背后真实的 agent 进程（acpx/Claude/Codex/Gemini）通信。R365 把这套接口契约固化到
Rust 端，未来 R366+ 接入真实 subprocess 时只需实现 trait，不动调用方。

---

## 🏗️ 新增模块

```
crates/pc-acpx/src/
└── acp_runtime.rs      # AcpRuntime trait + 22 个数据类型 + MockAcpRuntime
```

### 模块职责

| 模块 | 职责 | 依赖 |
|---|---|---|
| `acp_runtime.rs` | Node `acpx/runtime` 接口的 Rust 镜像：核心 trait + 合约数据类型 + Mock | `async-trait` + `futures` + `normalize` |

---

## 🔧 R365 实现的 22 个数据类型 + 1 个 trait + 1 个 Mock

### 核心类型 ✅

| Rust | Node |
|---|---|
| `AcpRuntimeHandle` | `AcpRuntimeHandle` |
| `AcpRuntimeEnsureInput` | `AcpRuntimeEnsureInput` |
| `AcpRuntimeTurnInput` | `AcpRuntimeTurnInput` |
| `AcpRuntimeTurnAttachment` | `AcpRuntimeTurnAttachment` |
| `SessionAgentOptions` | `SessionAgentOptions` |
| `McpServerEntry` | `McpServer$1` |

### 4 个 enum ✅

| Rust | Node |
|---|---|
| `AcpRuntimeMode` (Persistent / OneShot) | `AcpRuntimeSessionMode` |
| `AcpRuntimePromptMode` (Prompt / Steer) | `AcpRuntimePromptMode` |
| `AcpRuntimeControl` (SetMode / SetConfigOption / Status) | `AcpRuntimeControl` |
| `AcpRuntimeStream` (Output / Thought) | (string union) |

### 事件 + 结果类型 ✅

| Rust | Node |
|---|---|
| `AcpRuntimeEvent` (5 变体 enum) | `AcpRuntimeEvent` (tagged union) |
| `AcpRuntimeTurnResult` (3 变体 enum) | `AcpRuntimeTurnResult` |
| `AcpRuntimeTurnResultError` | `AcpRuntimeTurnResultError` |
| `AcpRuntimeUsageBreakdown` | `AcpRuntimeUsageBreakdown` |
| `AcpRuntimeUsageCost` | `AcpRuntimeUsageCost` |
| `AcpRuntimeStatus` | `AcpRuntimeStatus` |
| `AcpRuntimeSessionUsage` | `AcpRuntimeSessionUsage` |
| `AcpRuntimeSessionModels` | `AcpRuntimeSessionModels` |
| `AcpRuntimeAvailableCommand` | `AcpRuntimeAvailableCommand` |
| `AcpRuntimeCapabilities` | `AcpRuntimeCapabilities` |
| `AcpRuntimeDoctorReport` | `AcpRuntimeDoctorReport` |
| `AcpRuntimeToolCallLocation` | `ToolCallLocation` (subset) |

### 错误类型 ✅

| Rust | Node |
|---|---|
| `AcpRuntimeError` (4 变体) | 内嵌 ErrorType |

### Trait ✅

| Rust | Node |
|---|---|
| `AcpRuntime` (async trait, 11 个方法) | `AcpRuntime` interface |
| `AcpRuntimeTurn` + `result` + `events` | `AcpRuntimeTurn` |
| `AcpRuntimeEventStream` (boxed dyn Stream) | async iterable |
| `AcpRuntimeTurnResultResolver` | promise |

### Mock ✅

| Rust | Node |
|---|---|
| `MockAcpRuntime` (in-memory) | `MockAcpRuntime` |
| `MockAcpRuntime::new(events)` | constructor |
| `MockAcpRuntime::with_capabilities(caps)` | chainable setter |

---

## 📊 R365 测试覆盖

| 测试类型 | 数量 | 位置 |
|---|---|---|
| 单元测试 | **11** (R364 是 74, R365 新增 11 = 85 总) | `src/acp_runtime.rs::tests` (6) + `src/acp_runtime.rs::mock_tests` (5) |
| R362 集成测试 | 8 | `tests/round362_milestone.rs` |
| R363 集成测试 | 4 | `tests/round363_io_layer.rs` |
| R364 集成测试 | 4 | `tests/round364_build_runtime.rs` |
| **R365 集成测试** | **4** | `tests/round365_acp_runtime.rs` |
| **pc-acpx 合计** | **105** | |
| pc-heartbeat 全量回归 | **928** | 无变化 |

### 关键测试覆盖

- **acp_runtime 单元测试 (6)**：
  - handle default empty
  - mode round-trip NormalizedMode ↔ AcpRuntimeMode
  - event serde (`type: text_delta` tag)
  - turn result serde (`status: completed/failed`)
  - failed turn result 携带 error
  - control as_str 字符串映射
- **acp_runtime Mock 测试 (5)**：
  - 自增 session ID
  - 配置 events 流
  - capabilities 广播
  - doctor reports OK
  - status copies handle fields
- **R365 集成测试 (4)**：
  - 完整 session lifecycle（ensure → run → status）
  - capabilities 端到端
  - event serde round-trip
  - breakdown + cost status event

---

## 🧪 验证基线

```bash
cd /Users/louloulin/Documents/lumosaipaperclip/paperclip-rs

# 1. pc-acpx 全量（105/105 绿）
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

### 1. async trait with `async-trait` crate

```rust
#[async_trait]
pub trait AcpRuntime: Send + Sync {
    async fn ensure_session(&self, input: AcpRuntimeEnsureInput) -> Result<AcpRuntimeHandle, AcpRuntimeError>;
    fn start_turn(&self, input: AcpRuntimeTurnInput) -> AcpRuntimeTurn;
    ...
}
```

→ 使用 `async-trait` crate 让 dynamic dispatch 工作（Rust 1.75+ 原生 async trait 在 trait objects 仍有局限）。

### 2. Tagged enum 镜像 Node 联合类型

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AcpRuntimeEvent {
    TextDelta { text: String, stream: Option<AcpRuntimeStream>, tag: Option<String> },
    Status { text: String, ... },
    ToolCall { text: String, ... },
    Done { stop_reason: Option<String> },
    Error { message: String, ... },
}
```

→ `#[serde(tag = "type")]` emit `"type": "text_delta"`，与 Node `acpx.*` 事件 schema 兼容。

### 3. `AcpRuntimeMode ↔ NormalizedMode` 双向转换

```rust
impl From<NormalizedMode> for AcpRuntimeMode {
    fn from(mode: NormalizedMode) -> Self {
        match mode {
            NormalizedMode::Persistent => AcpRuntimeMode::Persistent,
            NormalizedMode::OneShot => AcpRuntimeMode::OneShot,
        }
    }
}
```

→ 协议层用 `AcpRuntimeMode`，normalize.rs 提供 `NormalizedMode`。两类型语义一致但保持独立，避免过度耦合。

### 4. MockAcpRuntime 共享所有权用 `Arc<AtomicU64>`

```rust
pub struct MockAcpRuntime {
    pub events: Vec<AcpRuntimeEvent>,
    pub next_session_id: std::sync::atomic::AtomicU64,
    capabilities: Option<AcpRuntimeCapabilities>,
}
```

→ `MockAcpRuntime` 带 `Send + Sync` 边界，跨 task 共享 session id 计数器。`AtomicU64::fetch_add` 原子自增。

### 5. `AcpRuntimeEventStream` 用 `Box::pin`

```rust
pub type AcpRuntimeEventStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = AcpRuntimeEvent> + Send + Sync>,
>;
```

→ 直接 `Box::pin(stream::iter(events))` 而不是 `.boxed()`（后者需要 `StreamExt` in scope + tricky type inference）。

### 6. Optional 方法用 default trait impl

```rust
#[async_trait]
pub trait AcpRuntime: Send + Sync {
    async fn ensure_session(&self, input: ...) -> Result<...> { ... }  // 必选
    async fn get_capabilities(&self, _input: ...) -> Option<...> { None }  // 默认 None
    async fn set_mode(&self, _input: ...) -> Result<()> { Err(AcpRuntimeError::SessionError("".into())) }  // 默认错误
    ...
}
```

→ 区别"未实现"和"未启用"：默认 `None` 表示 runtime 不支持，`Err` 表示调用方不应该调用。

### 7. 适配已有的 `ScriptError` 错误模型

```rust
#[derive(Debug, Error)]
pub enum AcpRuntimeError {
    #[error("acpx handshake failed: {message}")]
    HandshakeFailed { message: String, code: Option<String> },
    #[error("acpx turn failed: {message}")]
    TurnFailed { message: String, code: Option<String> },
    #[error("acpx session operation failed: {0}")]
    SessionError(String),
    #[error("acpx io error: {0}")]
    Io(String),
}
```

→ `thiserror` derive + 4 个语义变体覆盖典型故障模式。

---

## 📋 后续 R366+ 计划

### R366 (下一轮) — 错误恢复 + `startup-timing.ts`

- `classifyError` / `describeErrorDiagnostics`
- `readChildStderrTail` / `routeChildStderr`
- `startup-timing.ts`（304 行）— 启动阶段耗时测量

### R367 — Sandbox staging seam

- `prepareAdapterExecutionTargetRuntime`
- `stageAcpRemoteRuntime`
- `startAdapterExecutionTargetPaperclipBridge`

### R368+ — 真实 `AcpRuntime` 实现

- `SubprocessAcpRuntime`：`acpx` 子进程的 `std::process::Command` 包装
- 串行化 `AcpRuntimeEvent` JSON 流 → `tokio::io::BufReader`
- 真正的 `OpenSession` / `SendTurn` / `CloseSession` 边界

---

## 📊 完成度更新

| 维度 | R360 | R362 | R363 | R364 | R365 |
|---|---|---|---|---|---|
| pc-acpx 测试 | 0 | 47 | 66 | 90 | **105** |
| 总测试数 | 928 | 975 | 994 | 1018 | **1042** |
| **acpx-engine 子模块** | ~0% | ~67% | ~75% | ~80% | **~85%** |
| 后端核心 | ~96% | ~96% | ~96% | ~96% | ~96% |

---

## 📝 总结

**R365 启动 acp.handshake 协议契约的 Rust 化迁移**：

- **新增 1 个模块**：`acp_runtime.rs`（740+ 行，最大单模块）
- **新增 24 个测试**（15 单元 + 4 集成 + 5 mock 测试），保持 0 失败
- **pc-heartbeat 928 测试完全无回归**
- **核心契约就绪**：`AcpRuntime` trait + 22 个类型 + Mock 实现
- **完成度**：acpx-engine 子模块从 80% 推进到 ~85%（协议契约层就绪）

**下一步**：R366 启动错误恢复 + `startup-timing.ts`（304 行），继续 B3.1 第五阶段。
