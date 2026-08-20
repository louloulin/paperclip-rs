//! Plugin JSON-RPC integration test — 1:1 mirror of paperclip Node
//! plugin-host-service `initialize` / `shutdown` handshake contract.
//!
//! Validates the JSON-RPC envelope contract:
//! 1. Request serialization round-trips through `serde_json`
//! 2. Response id matches request id
//! 3. Error responses use correct error codes (MethodNotFound, InvalidParams)
//! 4. Each message is single-line JSON (JSON-RPC over stdio requirement)
//!
//! Uses in-process mock dispatch (no real stdio subprocess). Real subprocess
//! tests live in `host_dispatcher_e2e.rs`.

use pc_plugin_protocol::envelope::{
    JsonRpcError, JsonRpcErrorCode, JsonRpcErrorResponse, JsonRpcRequest, JsonRpcResponse,
    JsonRpcSuccessResponse,
};
use serde_json::{json, Value};

/// Mock dispatch — handles `initialize` and `shutdown` like Node plugin-host-service.
async fn mock_dispatch(method: &str, params: Option<Value>) -> Result<Value, JsonRpcError> {
    match method {
        "initialize" => {
            let params = params.unwrap_or_else(|| json!({}));
            let plugin_id = params
                .get("pluginId")
                .and_then(|v| v.as_str())
                .ok_or_else(|| JsonRpcError {
                    code: JsonRpcErrorCode::InvalidParams as i32,
                    message: "missing pluginId".into(),
                    data: None,
                })?;
            Ok(json!({
                "pluginId": plugin_id,
                "status": "ready",
            }))
        }
        "shutdown" => Ok(json!({ "status": "stopped" })),
        other => Err(JsonRpcError {
            code: JsonRpcErrorCode::MethodNotFound as i32,
            message: format!("method not found: {other}"),
            data: None,
        }),
    }
}

/// Wrap a (id, result) into a JSON-RPC success response envelope.
fn success_envelope(id: String, result: Value) -> JsonRpcResponse {
    JsonRpcResponse::Success(JsonRpcSuccessResponse {
        jsonrpc: "2.0".into(),
        id,
        result,
    })
}

/// Wrap an (id, error) into a JSON-RPC error response envelope.
fn error_envelope(id: String, err: JsonRpcError) -> JsonRpcResponse {
    JsonRpcResponse::Error(JsonRpcErrorResponse {
        jsonrpc: "2.0".into(),
        id,
        error: err,
    })
}

fn build_request<P: serde::Serialize>(id: u64, method: &str, params: P) -> JsonRpcRequest<Value> {
    let params_value = serde_json::to_value(params).unwrap();
    JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: id.to_string(),
        method: method.to_string(),
        params: Some(params_value),
    }
}

/// End-to-end: build request → serialize → parse → dispatch → envelope.
async fn round_trip(id: u64, method: &str, params: Value) -> JsonRpcResponse {
    let req = build_request(id, method, params);
    let line = serde_json::to_string(&req).unwrap();
    let parsed: JsonRpcRequest<Value> = serde_json::from_str(&line).unwrap();
    let dispatch_id = parsed.id.clone();
    match mock_dispatch(&parsed.method, parsed.params).await {
        Ok(v) => success_envelope(dispatch_id, v),
        Err(e) => error_envelope(dispatch_id, e),
    }
}

#[tokio::test]
async fn round_trip_initialize_success() {
    let resp = round_trip(1, "initialize", json!({ "pluginId": "test-plugin" })).await;
    match resp {
        JsonRpcResponse::Success(s) => {
            assert_eq!(s.id, "1");
            assert_eq!(s.result["pluginId"], "test-plugin");
            assert_eq!(s.result["status"], "ready");
        }
        _ => panic!("expected success response"),
    }
}

#[tokio::test]
async fn round_trip_shutdown_success() {
    let resp = round_trip(42, "shutdown", json!({})).await;
    match resp {
        JsonRpcResponse::Success(s) => {
            assert_eq!(s.id, "42");
            assert_eq!(s.result["status"], "stopped");
        }
        _ => panic!("expected success"),
    }
}

#[tokio::test]
async fn unknown_method_returns_method_not_found() {
    let resp = round_trip(7, "unknown.method", json!({})).await;
    match resp {
        JsonRpcResponse::Error(e) => {
            assert_eq!(e.id, "7");
            assert_eq!(e.error.code, JsonRpcErrorCode::MethodNotFound as i32);
            assert!(e.error.message.contains("unknown.method"));
        }
        _ => panic!("expected error response"),
    }
}

#[tokio::test]
async fn initialize_with_missing_plugin_id_returns_invalid_params() {
    let resp = round_trip(8, "initialize", json!({})).await;
    match resp {
        JsonRpcResponse::Error(e) => {
            assert_eq!(e.error.code, JsonRpcErrorCode::InvalidParams as i32);
            assert!(e.error.message.contains("pluginId"));
        }
        _ => panic!("expected error"),
    }
}

#[tokio::test]
async fn response_id_matches_request_id() {
    for test_id in [1u64, 100, 9_999, u64::MAX] {
        let resp = round_trip(test_id, "shutdown", json!({})).await;
        match resp {
            JsonRpcResponse::Success(s) => assert_eq!(s.id, test_id.to_string()),
            JsonRpcResponse::Error(e) => assert_eq!(e.id, test_id.to_string()),
        }
    }
}

#[tokio::test]
async fn serialized_response_is_single_line_json() {
    let resp = round_trip(1, "initialize", json!({ "pluginId": "x" })).await;
    let line = serde_json::to_string(&resp).unwrap();
    assert!(!line.contains('\n'), "response must be single line");
    assert!(line.starts_with("{"));
    assert!(line.ends_with("}"));
}

#[tokio::test]
async fn request_envelope_includes_jsonrpc_version() {
    let req = build_request(1, "shutdown", json!({}));
    assert_eq!(req.jsonrpc, "2.0");
    assert_eq!(req.id, "1");
    assert_eq!(req.method, "shutdown");
}

#[tokio::test]
async fn error_envelope_preserves_request_id() {
    let success = round_trip(99, "shutdown", json!({})).await;
    if let JsonRpcResponse::Success(s) = success {
        assert_eq!(s.id, "99");
    } else {
        panic!("expected success");
    }

    let error = round_trip(99, "unknown.thing", json!({})).await;
    if let JsonRpcResponse::Error(e) = error {
        assert_eq!(e.id, "99");
    } else {
        panic!("expected error");
    }
}