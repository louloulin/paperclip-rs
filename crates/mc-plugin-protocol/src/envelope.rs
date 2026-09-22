//! JSON-RPC envelope。

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: String,
    pub method: String,
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    pub fn success(id: impl Into<String>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: id.into(),
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: impl Into<String>, code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: id.into(),
            result: None,
            error: Some(JsonRpcError {
                code: code as i32,
                message: message.into(),
                data: None,
            }),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Multica plugin IPC 错误码（与 multica `packages/plugin-sdk/src/errors.ts` 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorCode {
    ParseError = -32700,
    InvalidRequest = -32600,
    MethodNotFound = -32601,
    InvalidParams = -32602,
    InternalError = -32603,
    Unauthorized = -32001,
    Forbidden = -32002,
    NotFound = -32003,
    Conflict = -32004,
    RateLimited = -32005,
    SignatureInvalid = -32010,
    VersionMismatch = -32011,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_response_serializes() {
        let r = JsonRpcResponse::success("1", serde_json::json!({"ok": true}));
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"result\":{\"ok\":true}"));
        assert!(!json.contains("error"));
    }

    #[test]
    fn error_response_serializes() {
        let r = JsonRpcResponse::error("1", ErrorCode::NotFound, "missing");
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"error\""));
    }
}
