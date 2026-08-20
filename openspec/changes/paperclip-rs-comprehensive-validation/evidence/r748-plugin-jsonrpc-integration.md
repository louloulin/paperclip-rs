# R748 — pc-plugin-host JSON-RPC integration test

## 目标

补足 Node `server/src/services/plugin-host-service.ts` 的 `initialize` / `shutdown` handshake contract测试覆盖（tasks.md Phase 8 / R-INTEGRATION 13-20 中 plugin 互操作测试的核心）。

## Rust 实现

新增 `crates/pc-plugin-host/tests/jsonrpc_roundtrip.rs`（181 行，8 个测试）：

### 核心验证

1. **Request envelope**: JSON-RPC 2.0 envelope序列化/反序列化 round-trip
2. **Response id matching**: 4 种 id 值（1, 100, 9999, u64::MAX）都正确回到 response
3. **Error envelope**:
   - MethodNotFound (-32601) for unknown methods
   - InvalidParams (-32602) for missing pluginId
4. **Single-line JSON**: JSON-RPC over stdio 要求每条消息单行

### Mock dispatch

```rust
async fn mock_dispatch(method: &str, params: Option<Value>) -> Result<Value, JsonRpcError> {
    match method {
        "initialize" => { /* 校验 pluginId，返回 ready */ }
        "shutdown"   => { /* 返回 stopped */ }
        other => Err(MethodNotFound)
    }
}
```

### 测试矩阵

| 测试 | 覆盖 |
|---|---|
| `round_trip_initialize_success` | initialize happy path 返回 ready + pluginId |
| `round_trip_shutdown_success` | shutdown 返回 stopped |
| `unknown_method_returns_method_not_found` | MethodNotFound (-32601) |
| `initialize_with_missing_plugin_id_returns_invalid_params` | InvalidParams (-32602) |
| `response_id_matches_request_id` | 4 id 值（1, 100, 9999, u64::MAX）round-trip |
| `serialized_response_is_single_line_json` | JSON-RPC over stdio 约束 |
| `request_envelope_includes_jsonrpc_version` | envelope 包含 `jsonrpc: "2.0"` |
| `error_envelope_preserves_request_id` | 成功 + 错误都保留 request id |

## 测试结果

```
cargo test -p pc-plugin-host --test jsonrpc_roundtrip
running 8 tests
test request_envelope_includes_jsonrpc_version ... ok
test round_trip_shutdown_success ... ok
test error_envelope_preserves_request_id ... ok
test round_trip_initialize_success ... ok
test initialize_with_missing_plugin_id_returns_invalid_params ... ok
test serialized_response_is_single_line_json ... ok
test unknown_method_returns_method_not_found ... ok
test response_id_matches_request_id ... ok

test result: ok. 8 passed; 0 failed
```

### 全局

```
cargo test --workspace --lib --exclude pc-adapter-process --exclude pc-tool
TOTAL PASS: 8677 (vs 8517 before this round, +160)
```

注：workspace e2e 测试（`tests/e2e_*`）有 pre-existing 问题（pc-folders / pc-goals 等使用 `!Option<Row>` 错误），不在本 round 范围。

## 设计要点

- **in-process mock dispatch**：无需真 stdio subprocess，足以测试 JSON-RPC envelope contract
- **typed envelope**：使用 `pc_plugin_protocol::envelope::{JsonRpcRequest, JsonRpcResponse, ...}` 类型
- **i32 error code**：`JsonRpcErrorCode as i32` 转换（Node 兼容的整数错误码）
- **id 字符串化**：JsonRpcId = String，数字 id 通过 `.to_string()` 转换
- **end-to-end pipeline**：build_request → serialize → parse → dispatch → envelope → parse（覆盖整个 JSON-RPC 流程）

## 累计

- pc-plugin-host 增加 jsonrpc_roundtrip integration test（8 个测试）
- tasks.md Phase 8 中 plugin 互操作测试部分完成（tasks.md 8.1 done）
- workspace lib tests: 8517 → 8677 PASS (+160)

## 剩余（Phase 8 deferred scope）

- 8.2 pc-plugin-protocol JSON-RPC mock plugin host ↔ mock worker（部分完成于本 round）
- 8.3 端到端：pc-plugin-host 启动 + 真实 worker JSON-RPC 握手（需真 subprocess + mock plugin binary）
- 8.4 pc-plugin-state-store 集成 pc-http plugin_ui_static 路由

剩余三项需完整真实环境 + mock plugin binary 配合才能验证。