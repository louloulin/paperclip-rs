//! JSON-RPC 2.0 信封（envelope）类型。
//!
//! 与原 `packages/plugins/sdk/src/protocol.ts` 中 JSON-RPC 部分等价。

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const JSONRPC_VERSION: &str = "2.0";

/// JSON-RPC 请求 ID（字符串或数字）。
pub type JsonRpcId = String;

/// JSON-RPC 2.0 请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest<P = Value> {
    pub jsonrpc: String,
    pub id: JsonRpcId,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<P>,
}

impl<P> JsonRpcRequest<P> {
    pub fn new(id: impl Into<String>, method: impl Into<String>, params: P) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: id.into(),
            method: method.into(),
            params: Some(params),
        }
    }

    pub fn new_no_params(id: impl Into<String>, method: impl Into<String>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: id.into(),
            method: method.into(),
            params: None,
        }
    }
}

/// JSON-RPC 2.0 成功响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcSuccessResponse<R = Value> {
    pub jsonrpc: String,
    pub id: JsonRpcId,
    pub result: R,
}

/// JSON-RPC 2.0 错误响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcErrorResponse {
    pub jsonrpc: String,
    pub id: JsonRpcId,
    pub error: JsonRpcError,
}

/// JSON-RPC 2.0 错误。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JsonRpcError {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn with_data(code: i32, message: impl Into<String>, data: Value) -> Self {
        Self {
            code,
            message: message.into(),
            data: Some(data),
        }
    }

    pub fn into_response(self, id: impl Into<String>) -> JsonRpcErrorResponse {
        JsonRpcErrorResponse {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: id.into(),
            error: self,
        }
    }
}

/// JSON-RPC 统一响应（成功或失败）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JsonRpcResponse<R = Value> {
    Success(JsonRpcSuccessResponse<R>),
    Error(JsonRpcErrorResponse),
}

impl<R> JsonRpcResponse<R> {
    pub fn success(id: impl Into<String>, result: R) -> Self {
        Self::Success(JsonRpcSuccessResponse {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: id.into(),
            result,
        })
    }

    pub fn error(id: impl Into<String>, error: JsonRpcError) -> Self {
        Self::Error(error.into_response(id))
    }
}

/// 标准 JSON-RPC 2.0 错误码。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum JsonRpcErrorCode {
    ParseError = -32700,
    InvalidRequest = -32600,
    MethodNotFound = -32601,
    InvalidParams = -32602,
    InternalError = -32603,
    /// Marker for custom server error (range -32000 to -32099). Use [`Self::server`] to construct.
    ServerError = -32000,
}

impl JsonRpcErrorCode {
    pub fn server(code: i32) -> Self {
        debug_assert!(
            (-32099..=-32000).contains(&code),
            "server error code must be in -32000..-32099"
        );
        let _ = code;
        Self::ServerError
    }
}

impl JsonRpcErrorCode {
    pub fn as_i32(self) -> i32 {
        match self {
            Self::ParseError => -32700,
            Self::InvalidRequest => -32600,
            Self::MethodNotFound => -32601,
            Self::InvalidParams => -32602,
            Self::InternalError => -32603,
            Self::ServerError => -32000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_serializes_correctly() {
        let req: JsonRpcRequest<Value> =
            JsonRpcRequest::new("req-1", "initialize", serde_json::json!({"foo":"bar"}));
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"jsonrpc\":\"2.0\""));
        assert!(json.contains("\"method\":\"initialize\""));
    }

    #[test]
    fn response_success_serializes() {
        let resp: JsonRpcResponse<Value> =
            JsonRpcResponse::success("req-1", serde_json::json!({"ok": true}));
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"result\""));
        assert!(!json.contains("\"error\""));
    }

    #[test]
    fn response_error_includes_code() {
        let resp: JsonRpcResponse<Value> =
            JsonRpcResponse::error("req-1", JsonRpcError::new(-32601, "method not found"));
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"error\""));
        assert!(json.contains("\"code\":-32601"));
    }

    #[test]
    fn error_codes_have_correct_values() {
        assert_eq!(JsonRpcErrorCode::ParseError.as_i32(), -32700);
        assert_eq!(JsonRpcErrorCode::InvalidRequest.as_i32(), -32600);
        assert_eq!(JsonRpcErrorCode::MethodNotFound.as_i32(), -32601);
        assert_eq!(JsonRpcErrorCode::InvalidParams.as_i32(), -32602);
        assert_eq!(JsonRpcErrorCode::InternalError.as_i32(), -32603);
        assert_eq!(JsonRpcErrorCode::ServerError.as_i32(), -32000);
    }
}
