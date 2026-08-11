# R616 — OpenClaw Gateway 真实 WebSocket 客户端 (TungsteniteWireClient)

## 背景

R615 完成 OpenClaw Gateway 的 `execute.rs`，但生产路径仍用 `FakeWireClient` —— 仅 mockable，未对接真实网络。

R616 实现真实 WebSocket transport：
- `tokio-tungstenite` 集成（`connect_async` / `Message::Text` / `Message::Binary` / `Message::Close`）
- `TungsteniteWireClient` 实现 `GatewayWireClient` trait
- 真 e2e：spawn local WS server → 端到端验证 connect / send_request / stream_events
- 解决关键并发 bug：pending HashMap 必须 Arc 共享

## 模块拆分

**新增文件**：
- `crates/pc-adapter-openclaw-gateway/src/ws_client.rs` (458 行)
- `crates/pc-adapter-openclaw-gateway/tests/ws_e2e.rs` (324 行)

**修改文件**：
- `Cargo.toml` (workspace)：加 `tokio-tungstenite = "0.24"` + `futures-util = "0.3"`
- `crates/pc-adapter-openclaw-gateway/Cargo.toml`：加 `tokio-tungstenite` + `futures-util` + `tracing` + `uuid` + `tokio`
- `crates/pc-adapter-openclaw-gateway/src/lib.rs`：暴露 `ws_client` 模块

## 关键架构

### Pump task 模式

```rust
loop {
    tokio::select! {
        biased;
        msg = stream.next() => { /* route to pending or events */ }
        Some(text) = outbound_rx.recv() => { stream.send(text) }
        else => break,
    }
}
```

- **Outbound channel**：`mpsc::UnboundedSender<String>` —— `send_request` 推文本，pump task drain + send
- **Pending map**：`Arc<Mutex<HashMap<id, oneshot::Sender<Result<Value, GatewayError>>>>>` —— pump 收到 response 后取出并 send（**关键**：必须 Arc 共享，详见下面 bug 复盘）
- **Event queue**：`mpsc::UnboundedSender<GatewayEventFrame>` —— pump 推、`next_event` 拉

### 关键 bug 复盘：pending HashMap 共享

最初设计：Inner 持有 `pending: HashMap<...>`，pump task 持有独立的 `pending_for_task: Arc<Mutex<HashMap<...>>>`。
**Bug**：`send_request` 把 entry 插入 Inner.pending，但 pump task 看的是 pending_for_task —— **永远是空的**。
**症状**：所有 `send_request` 都 timeout。
**修复**：把 `Inner.pending` 改为 `Arc<Mutex<HashMap<...>>>`，与 pump task 共享同一个 Arc。

debug log 揭示：
```
[DBG send_request] pushed to outbound, awaiting response id=req-...
[DBG route_text] pending keys: []                    ← BUG：空！
[DBG route_text] no sender for id=req-...           ← sender not found
send_request: GatewayError { message: "request timeout" }
```

修复后：
```
[DBG route_text] pending keys: ["req-..."]          ← 正确
[DBG route_text] found sender for id=req-...        ← routed
```

### `biased;` select 顺序

`biased;` 让 select 优先检查第一个分支（`stream.next()`），避免 outbound 抢断 inbound —— 保证低延迟响应路由。

## 公共 API

```rust
pub struct TungsteniteWireClient { /* Arc<Mutex<Inner>> */ }

impl TungsteniteWireClient {
    pub async fn connect(
        url: &str,
        opts: &ConnectOptions,
        signed_connect_params: Value,
        connect_timeout: Duration,
    ) -> Result<(Self, GatewayHello), GatewayError>;

    pub async fn send_request(
        &self,
        method: &str,
        params: Option<Value>,
        timeout: Duration,
    ) -> Result<Value, GatewayError>;

    pub async fn next_event(&self) -> Option<GatewayEventFrame>;
}

#[async_trait]
impl GatewayWireClient for TungsteniteWireClient { ... }  // 与 FakeWireClient 同 trait
```

## 测试覆盖

### Lib 单测 (5 个)

| Test | 验证 |
|---|---|
| `route_text_dispatches_response_to_pending` | res 帧 → pending[id] → oneshot send |
| `route_text_dispatches_event_to_queue` | event 帧 → events channel |
| `route_text_response_with_error_maps_to_gateway_error` | ok=false → GatewayError with code |
| `route_text_response_with_unknown_id_silently_dropped` | 未注册的 id → 静默 drop |
| `route_text_rejects_unknown_frame_type` | type=bogus → error |

### 集成 e2e (6 个) — **真 WS server**

| Test | 验证 |
|---|---|
| `tungstenite_client_connects_to_real_ws_server` | 启动 TcpListener → accept_async → 完整 connect 握手 |
| `tungstenite_client_send_request_roundtrip` | device.run.send req → server response → 完整 Value 接收 |
| `tungstenite_client_streams_events` | run.send → 3 stream.chunk + 1 run.complete 顺序接收 |
| `tungstenite_client_unknown_method_returns_error` | server 返 error → GatewayError 包含 gateway_code |
| `tungstenite_client_connect_to_closed_port_fails` | ws://127.0.0.1:1 → GatewayError |
| `tungstenite_client_implements_gateway_wire_client_trait` | 通过 `dyn GatewayWireClient` trait 调用 |

### Test server fixture

`spawn_echo_server()` 在测试中启动真实 `tokio::net::TcpListener` + `tokio_tungstenite::accept_async`，处理：
- `device.connect` → 返 hello
- `device.run.send` → 返 runId + 推 3 stream.chunk + 1 run.complete
- 未知 method → 返 UNKNOWN_METHOD error

## 测试结果

```
test result: ok. 173 passed; 0 failed   (lib)
test result: ok. 6 passed; 0 failed     (e2e)
```

| Crate | R615 | R616 | 增加 |
|---|---:|---:|---:|
| `pc-adapter-openclaw-gateway` lib | 168 | **173** | **+5** |
| `pc-adapter-openclaw-gateway` e2e | 0 | **6** | **+6** |

总 workspace lib tests：~7,485 → **~7,496**

## Adapter 生产路径升级指引

`OpenclawGatewayAdapterV2::execute` 当前用 `FakeWireClient::new()` —— 这只是占位。

生产路径升级（下一步 R617 范围）：
```rust
async fn execute(...) -> Result<...> {
    let cfg = parse_execute_config(&context.adapter_config)?;
    // ... validate gateway URL ...
    let identity = cfg.identity.ok_or(...)?;
    let opts = make_connect_options(cfg.gateway_url.clone(), identity);
    let connect_params = sign_connect_params(&identity, &opts); // Ed25519
    let (client, _hello) = TungsteniteWireClient::connect(
        &cfg.gateway_url,
        &opts,
        connect_params,
        Duration::from_millis(cfg.connect_timeout_ms),
    ).await?;
    execute_with_client(Arc::new(client), context, events).await
}
```

`sign_connect_params` 需要 Ed25519 SPKI 签名（Node `execute.ts::signConnectParams` 对齐）—— 后续 R617-R618 单独实现。

## 后续路线

- **R617**：Cursor Cloud 真实 `ReqwestCursorCloudClient` (5 REST endpoints)
- **R617.5**：OpenClaw `sign_connect_params` (Ed25519) + 把生产路径切到 `TungsteniteWireClient`
- **R618-R620**：5 个 stub adapter 补 execute path
- **R621**：Hermes-gateway SSE + dashboard
- **R622**：架构 dedup
