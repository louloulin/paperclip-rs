# R614-R615 — OpenClaw Gateway execute path 整合

## 背景

R608-R612 已完成 OpenClaw Gateway 适配器的 10 个核心模块（constants / session_key / credentials / host_security / frame_codec / config_schema / wake_env / parse_stdout / retry_policy）。但缺少 `execute.rs` —— 完整的 Adapter execute path 整合。

R614 完成 `wire_client.rs`（mockable transport trait + FakeWireClient 剧本驱动）。
R615 完成 `execute.rs`（完整 execute path：parse config → validate → wake env → session key → connect → run → stream events → result）。

## R614 — wire_client.rs

**文件**：`crates/pc-adapter-openclaw-gateway/src/wire_client.rs` (457 行)
**新增测试**：13 个

| 抽象 | 描述 |
|---|---|
| `GatewayWireClient` trait | connect / disconnect / send_request / next_event / is_connected |
| `FakeWireClient` | 剧本驱动（`ScriptedStep` 枚举），支持 Connect/Disconnect/Request/Event/Error |
| `GatewayHello` | device.connect 响应 payload |
| `GatewayError` | 含 `gateway_code` 字段 |
| `make_connect_options` / `build_request` / `build_ok_response` / `build_event` | 帧构造辅助 |

**修复的预存 bug**：
- `url::Url` 解析需要 `url` crate 在 `[dependencies]`（之前误放 `[dev-dependencies]`）
- `serde_json::json!` 宏未导入
- `unused_imports`: `HashMap` / `GatewayFrame` / `GatewayResponseErrorBody` / `json` (frame_codec.rs)

## R615 — execute.rs

**文件**：`crates/pc-adapter-openclaw-gateway/src/execute.rs` (973 行)
**新增测试**：18 个（含 6 个 e2e）

### 公共 API

```rust
// 错误
pub enum ExecuteError {
    InvalidConfig(String),
    InvalidGatewayUrl(String),
    Gateway(String, Option<String>),
    RunError(String),
}

// 解析配置
pub struct ExecuteConfig { gateway_url, scopes, timeouts, strategy, identity, ... }
pub fn parse_execute_config(config: &Value) -> Result<ExecuteConfig, ExecuteError>;

// Session key + Wake env 桥接
pub fn build_session_key(strategy, configured, agent_id, run_id, issue_id) -> String;
pub fn extract_issue_id(wake: Option<&Value>) -> Option<String>;

// Event 处理
pub fn extract_event_text(frame: &GatewayEventFrame) -> Option<String>;
pub fn is_terminal_event(event_name: &str) -> bool;

// Prompt 拼接
pub fn assemble_prompt(instructions, wake, env_note, user_prompt, handoff) -> String;

// Result 构造
pub fn build_result(run_info, terminal_event, stdout_text, session_id, model)
    -> AdapterExecutionResult;

// 完整 execute path
pub async fn execute_with_client(client: DynWireClient, context, events)
    -> Result<AdapterExecutionResult, AdapterError>;

// Adapter 实现（生产路径使用 FakeWireClient —— 真实 WS client 需另写）
pub struct OpenclawGatewayAdapterV2;
impl Adapter for OpenclawGatewayAdapterV2 { ... }
```

### 关键设计决策

1. **mockable transport**：`GatewayWireClient` trait + `DynWireClient = Arc<dyn GatewayWireClient>` 让 e2e 测试用 `FakeWireClient` 剧本驱动
2. **`next_event` callback 已 fallback 到 script**：当 `events_received` 队列为空时，pop script 的 `ScriptedStep::Event` —— 这样 e2e 测试可以直接在 script 里编排 stream events
3. **`#![forbid(unsafe_code)]` 保持**：所有代码纯 safe Rust
4. **`Send` bound 跨 async 边界**：`next_event(&self, timeout_ms: u64) -> Option<GatewayEventFrame>` 返回 owned frame，与 cursor-cloud 的 `stream_messages` 模式保持一致

### 集成测试覆盖

| Test | 验证 |
|---|---|
| `full_execute_happy_path_emits_session_event` | 完整 path：connect → run → 3 events → session event → result |
| `full_execute_error_branch_returns_exit_one` | run.error event → exit_code=1 + error_message |
| `full_execute_connect_error_propagates` | ScriptedStep::Error on connect → AdapterError::Process |
| `full_execute_invalid_gateway_url_returns_config_error` | ftp:// scheme → InvalidConfiguration |
| `full_execute_missing_identity_returns_config_error` | identity 缺失 → InvalidConfiguration |
| `full_execute_no_terminal_event_still_returns_ok` | 跑完所有 events 无 terminal → exit_code=0 |
| `full_execute_session_key_strategy_run_uses_run_id` | strategy=run → sessionKey=agent:<id>:run-<id> |

### 单测覆盖

- `parse_execute_config` × 7：gatewayUrl 必需 / scopes 兜底 / identity / timeouts / sessionKeyStrategy 对象+字符串两种形式
- `build_session_key` × 4：issue / fixed / run / 无 agent_id 不加前缀
- `extract_issue_id` × 4：issue 优先于 task / 缺省 / trim 空
- `extract_event_text` × 3：delta > text / 仅 text / 无 payload
- `is_terminal_event` × 1
- `assemble_prompt` × 2：拼接 / 跳过空段
- `build_result` × 3：complete / error / session_id 透传

## 测试结果

```
test result: ok. 168 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

| Crate | 之前 | 现在 | 增加 |
|---|---:|---:|---:|
| `pc-adapter-openclaw-gateway` | 137 | **168** | **+31** |
| `pc-adapter-cursor-cloud` | 123 | 123 | 0 |

## 与 Cursor Cloud 的设计对比

| 维度 | Cursor Cloud (R613) | OpenClaw Gateway (R615) |
|---|---|---|
| Transport | HTTP REST (`reqwest`) | WebSocket RPC (`tokio-tungstenite` 未来) |
| Stream 模式 | `stream_messages` + `+Send` callback | `next_event` 拉一个 |
| 协议 | JSON via HTTPS | JSON via WS frames |
| Session 持久化 | `session_codec::CursorCloudSession` | sessionKey 字符串 |
| Resume 模式 | `resume_agent(agent_id)` | sessionKey 重用 |
| Event shape | `SdkTransportMessage` (typed) | `GatewayEventFrame` (frame) |
| 共同 | 都用 mockable trait + FakeClient 剧本驱动；都用 collect-then-emit 桥接 async |

## 后续路线

- R616：实现真实 `TungsteniteWireClient`（用 `tokio-tungstenite`）—— 把 `OpenclawGatewayAdapterV2` 的生产路径换掉 FakeClient
- R617：写 OpenClaw 的 integration e2e（用 real-shell 启动 fake `openclaw-gw` 命令，验证 wire_client 的 Unix socket / TCP 路径）
- 后续 P2：quota / plugin-host / V13 heartbeat / G11 路由字节级差异
