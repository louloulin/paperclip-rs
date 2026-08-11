# R622 — Hermes Gateway SSE + Dashboard REST 集成

## 背景

R622 关闭 `pc-adapter-hermes-gateway` 自报的 "未覆盖" gap：
- SSE 事件流消费（lib.rs R608 标记）
- dashboard REST 集成（`/api/runs`、轮询）
- 重新连接 / 退避

之前 hermes-gateway 仅 25 tests / 788 行 / 3 模块。R622 加 3 模块 + 1 retry 策略。

## 模块拆分

**新增文件**：
- `crates/pc-adapter-hermes-gateway/src/sse_client.rs` (423 行) —— SSE 消费 + 事件解析
- `crates/pc-adapter-hermes-gateway/src/dashboard.rs` (314 行) —— REST 客户端
- `crates/pc-adapter-hermes-gateway/src/retry_policy.rs` (40 行) —— 指数退避 + jitter
- `crates/pc-adapter-hermes-gateway/tests/sse_e2e.rs` (215 行) —— 真 axum mock server

**修改文件**：
- `crates/pc-adapter-hermes-gateway/Cargo.toml`：加 `reqwest` + `futures-util` + `tokio` + `tracing` + `axum` (dev)
- `crates/pc-adapter-hermes-gateway/src/lib.rs`：暴露 3 新模块

## 关键设计

### SSE 事件模型 (`SseEvent`)

```rust
pub enum SseEvent {
    AgentMessage { text: String, delta: bool },
    ToolCall { name: String, args: Option<Value> },
    ToolResult { name: String, is_error: bool, content: Option<Value> },
    Status { status: String, message: Option<String> },
    TaskComplete { summary: Option<String> },   // terminal
    TaskFailed { error: String },               // terminal
    Unknown { raw_type: String, payload: Value }, // forward compat
}
```

- `is_terminal()` —— TaskComplete / TaskFailed
- `extract_text()` —— 提取 user-facing 文本（agent_message 直接、status 拼 `[status] message`）

### SSE wire format 解析 (`parse_sse_chunk`)

标准 SSE wire format：
```
data: {"type":"agent_message","text":"hello"}\n
\n                                       ← 空行分隔事件
data: {"type":"tool_call","name":"bash"}\n
\n
```

支持：
- 单 chunk 多 events（按 `\n\n` 分隔）
- 单 event 多行（多 `data:` 行合并到同一 JSON）
- 注释行（`: ...`）忽略

### HermesSseClient

```rust
pub async fn consume_until_terminal(
    &self,
    path: &str,
    sink: &dyn SseEventSink,
    max_reconnects: u32,
) -> Result<SseStreamResult, String>;
```

- **Read full body**（`resp.bytes()`）→ buffer → 解析 events
- **Reconnect on stream end**：用 `backoff_with_jitter` 计算下次重试间隔（250ms base, 30s max）
- **Terminal detection**：收到 `task_complete` / `task_failed` 立即返回

### DashboardClient

```rust
pub async fn create_run(&self, req: &CreateRunRequest) -> Result<HermesRun, String>;
pub async fn get_run(&self, run_id: &str) -> Result<HermesRun, String>;
pub async fn poll_until_terminal(&self, run_id: &str, interval_ms: u64, timeout_ms: u64)
    -> Result<HermesRun, String>;
```

- 3 REST endpoints：POST `/v1/runs`、GET `/v1/runs/{id}`、GET `/v1/runs/{id}` (poll loop)
- Auth headers：`Authorization: Bearer <api_key>` + `X-Hermes-Session-Key` + `Accept: application/json`
- 404 → Err（vs SSE 流的 connected 判断）
- `poll_until_terminal` 自带 backoff + 超时

### RetryPolicy (`backoff_with_jitter`)

```rust
pub fn backoff_with_jitter(attempt: u32, base_ms: u64, max_ms: u64) -> u64
```

- `exp = attempt * base_ms`
- `bounded = min(exp, max_ms)`
- `bounded + jitter (0..base_ms)`
- 简单实现：用 `SystemTime::now().subsec_nanos() % base_ms`

## 测试覆盖

### Lib 单测 (19 个新增)

**sse_client** (9):
- `parse_sse_chunk_single_event` —— 单事件解析
- `parse_sse_chunk_multiple_events` —— 多事件
- `parse_sse_chunk_terminal_event` —— task_complete 标记 terminal
- `parse_sse_chunk_unknown_type_returns_unknown_variant` —— forward compat
- `parse_sse_chunk_ignores_comment_lines` —— `: ...` 注释
- `sse_event_extract_text_returns_agent_message_text`
- `sse_event_extract_text_returns_status_with_message`
- `sse_event_extract_text_none_for_tool_call`
- `in_memory_sink_collects_events`

**dashboard** (6):
- `run_status_is_terminal_for_finished_error_cancelled`
- `parse_hermes_run_extracts_required_fields`
- `parse_hermes_run_accepts_id_alias`
- `parse_hermes_run_extracts_error`
- `parse_hermes_run_fails_without_run_id`
- `parse_hermes_run_defaults_unknown_status_to_running`

**retry_policy** (4):
- `backoff_grows_with_attempt`
- `backoff_respects_max_ms`
- `backoff_zero_base_returns_zero`
- `ms_converts_correctly`

### 集成 e2e (7 个) — **真 axum mock server**

| Test | 验证 |
|---|---|
| `dashboard_create_run_returns_run_id` | POST /v1/runs → r-{uuid} 返回 |
| `dashboard_get_run_returns_existing` | GET /v1/runs/{id} → 找到 |
| `dashboard_get_run_returns_error_for_missing` | 404 → Err |
| `sse_consume_collects_events_until_terminal` | SSE 流 → 4 events + terminal |
| `sse_extract_text_from_agent_messages` | AgentMessage 拼接文本 |
| `sse_terminal_event_marked_correctly` | task_complete is_terminal |
| `sse_consume_to_closed_port_returns_error` | ws://127.0.0.1:1 → Err |

## 测试结果

```
test result: ok. 44 passed; 0 failed   (lib)  — R608-R612 was 25, +19 new
test result: ok. 1 passed; 0 failed     (adapter_real pre-existing)
test result: ok. 7 passed; 0 failed     (sse_e2e new)
```

| Crate | R608 | R622 | 增加 |
|---|---:|---:|---:|
| `pc-adapter-hermes-gateway` lib | 25 | **44** | **+19** |
| `pc-adapter-hermes-gateway` e2e | 0 | **7** | **+7** |

## 已知限制

1. **真实 Hermes gateway spec 未公开**：endpoint URL 是 placeholder（`/v1/runs`、`/v1/events`），需 SDK docs 释出后调整
2. **`consume_until_terminal` 使用一次性 `resp.bytes()`**：而非 chunked `bytes_stream()` —— 真实流需切换到 chunked 模式
3. **`Adapter::execute` 未集成 SSE/dashboard**：execute path 仍 spawn CLI 子进程；后续 R623 整合

## 后续路线

- **R623**：把 Hermes gateway `Adapter::execute` 切到 DashboardClient + HermesSseClient 完整路径（POST /v1/runs + 启动 SSE consumer + poll 等待 terminal）
- **R618**：把 CursorCloudAdapter / OpenclawGatewayAdapterV2 生产路径切换到真实 client
- **R624+**：plugin-host / quota / MCP servers
