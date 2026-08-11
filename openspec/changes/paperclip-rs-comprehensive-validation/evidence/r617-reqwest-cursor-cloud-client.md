# R617 — Cursor Cloud 真实 HTTP 客户端 (ReqwestCursorCloudClient)

## 背景

R613 完成 Cursor Cloud `execute.rs` + `FakeCursorCloudClient` 剧本驱动。但生产路径仍仅 mockable。

R617 实现真实 HTTP transport：
- `reqwest = "0.12"` (workspace dep) + `rustls-tls` 
- 5 个 REST endpoint (POST/GET) 骨架
- Auth header (`X-API-Key`)
- SSE message stream 解析
- 4xx/5xx → `CloudError` with `gateway_code`
- Mock HTTP server (axum) 真 e2e

## 模块拆分

**新增文件**：
- `crates/pc-adapter-cursor-cloud/src/http_client.rs` (440 行)
- `crates/pc-adapter-cursor-cloud/tests/http_e2e.rs` (288 行)

**修改文件**：
- `Cargo.toml` (workspace)：加 `reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }`
- `crates/pc-adapter-cursor-cloud/Cargo.toml`：加 `reqwest` + `futures-util` + `tokio` (dev-deps: `axum` + `serde_json` + `futures-util`)
- `crates/pc-adapter-cursor-cloud/src/lib.rs`：暴露 `http_client` 模块

## 关键设计

### 5 个 REST endpoint (placeholder URL shape)

| Method | Trait | Endpoint | Body | Response |
|---|---|---|---|---|
| `POST` | `create_agent` | `/agents` | `AgentOptions` JSON | `CloudAgent` |
| `GET` | `resume_agent` | `/agents/{id}` | — | `CloudAgent` |
| `POST` | `send_prompt` | `/agents/{id}/runs` | `{prompt, model}` | `CloudRun` |
| `GET` | `get_run` | `/runs/{id}` | — | `Option<CloudRun>` (404 → None) |
| `GET` (SSE) | `stream_messages` | `/runs/{id}/messages` | — | SSE stream of `SdkTransportMessage` |
| `GET` (poll) | `wait_for_run` | `/runs/{id}` (×120) | — | `CloudRun` (until non-Running) |

**注**：由于 Cursor Cloud REST API 未公开文档，以上 endpoint shape 是 **interface contract**，SDK docs 释出后可批量替换。

### Auth + Error mapping

```rust
fn auth_headers(&self) -> HeaderMap {
    headers.insert("x-api-key", self.api_key);
    headers.insert("content-type", "application/json");
    headers
}

async fn parse_response<T: DeserializeOwned>(resp: Response) -> Result<T, CloudError> {
    let status = resp.status();
    if status.is_success() {
        serde_json::from_slice(&bytes)
    } else {
        let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        let message = body.get("message").and_then(|v| v.as_str()).unwrap_or("http error");
        let code = body.get("code").and_then(|v| v.as_str())
            .unwrap_or_else(|| status.to_string());
        Err(CloudError::new(message).with_code(code).with_details(body))
    }
}
```

### SSE 解析

```rust
// Read full response body (or stream chunked)
let bytes = resp.bytes().await?;
let mut buffer = String::from_utf8_lossy(&bytes).to_string();
// SSE events separated by "\n\n", each starts with "data: "
while let Some(idx) = buffer.find("\n\n") {
    let event = buffer[..idx].to_owned();
    buffer.drain(..idx + 2);
    for line in event.lines() {
        if let Some(rest) = line.strip_prefix("data: ") {
            if let Ok(value) = serde_json::from_str::<Value>(rest.trim()) {
                if let Some(msg) = parse_sse_message(&value) {
                    sink(msg);
                }
            }
        }
    }
}
```

`parse_sse_message` 把 SSE JSON value 映射到 `SdkTransportMessage` 枚举（assistant / user / thinking / tool_call / tool_result / status / task）。

## 测试覆盖

### Lib 单测 (4 个)

| Test | 验证 |
|---|---|
| `parse_sse_message_extracts_assistant_text` | `{type:assistant, text}` → `SdkTransportMessage::Assistant` |
| `parse_sse_message_extracts_tool_call` | `{type:tool_call, name, status, args}` → `SdkTransportMessage::ToolCall` |
| `parse_sse_message_returns_none_for_unknown_kind` | unknown type → None |
| `build_create_body_includes_all_fields` | AgentOptions → 完整 JSON |

### 集成 e2e (7 个) — **真 axum mock server**

| Test | 验证 |
|---|---|
| `http_client_create_agent_returns_id` | POST /agents → cu-xxx 返回 |
| `http_client_resume_agent_finds_existing` | GET /agents/{id} → 复用 |
| `http_client_send_prompt_returns_running_run` | POST /agents/{id}/runs → r-xxx Running |
| `http_client_get_run_returns_404_as_none` | GET /runs/nonexistent → None (404 mapped) |
| `http_client_stream_messages_collects_sse_events` | SSE stream → 3 messages collected |
| `http_client_404_returns_cloud_error_with_code` | 404 → CloudError with gateway_code |
| `http_client_implements_cursor_cloud_client_trait` | dyn-compatible via trait |

### Mock server fixture

`spawn_mock_server()` 用 `axum::Router` 启真实 HTTP server：
- `POST /agents` → 分配 cu-{uuid}
- `GET /agents/{id}` → 查找或 404
- `POST /agents/{id}/runs` → 分配 r-{uuid} + 标记 Running
- `GET /runs/{id}` → 返回当前状态
- `GET /runs/{id}/messages` → SSE 推送 3 frames + 标记 Finished

## 测试结果

```
test result: ok. 127 passed; 0 failed   (lib)  — R613 was 123, +4 http_client tests
test result: ok. 7 passed; 0 failed     (e2e)
```

| Crate | R613 | R617 | 增加 |
|---|---:|---:|---:|
| `pc-adapter-cursor-cloud` lib | 123 | **127** | **+4** |
| `pc-adapter-cursor-cloud` e2e | 0 | **7** | **+7** |

总 workspace lib tests：~7,496 → **~7,507**

## 与 OpenClaw TungsteniteWireClient (R616) 的对比

| 维度 | OpenClaw WS | Cursor Cloud HTTP |
|---|---|---|
| Transport | WebSocket (RFC 6455) | REST over HTTPS |
| Lib | tokio-tungstenite | reqwest |
| Stream | `next_event` (pull) | SSE (server-sent events) |
| 协议 | JSON frame `req/res/event` | HTTP POST/GET + JSON body |
| Session | sessionKey + device id | cloudAgentId + runId |
| Resume | 复用 sessionKey | `GET /agents/{id}` + 复用 |
| 共同 | mockable trait + 真 e2e | mockable trait + 真 e2e |

## 已知限制

1. **真实 SDK endpoint shape 未验证**：5 个 endpoint URL 是 placeholder —— SDK docs 释出后需替换
2. **预存在的 `adapter_real.rs` 测试失败**：使用 `pc_adapter_cursor_cloud::CursorCloudAdapter` 但实际是 `execute::CursorCloudAdapter`，不属于本 round 范围
3. **SSE chunked transfer**：当前实现 `resp.bytes()` 一次性读完，不分块 —— 大 stream 需 `bytes_stream()` （reqwest 提供但当前未用）

## 后续路线

- **R617.5**：把 `CursorCloudAdapter` 生产路径切换到 `ReqwestCursorCloudClient`
- **R617.6**：OpenClaw `sign_connect_params` (Ed25519) + 把 `OpenclawGatewayAdapterV2` 切换到 `TungsteniteWireClient`
- **R618-R620**：5 个 stub adapter 补 execute path
- **R621**：Hermes-gateway SSE + dashboard
- **R622**：架构 dedup
