//! `pc-acpx` JSON-RPC 2.0 wire framing — pure helpers used by the
//! `SubprocessAcpRuntime` to talk to the `acpx` binary over its stdin/stdout
//! JSON-RPC channel. Mirrors the framing used by Node `acpx-runtime`'s
//! `AcpClient` (patches/acpx@0.12.0.patch shows the same `jsonrpc:"2.0"`
//! request/response/notification shape).
//!
//! The framing is deliberately permissive:
//! - Responses carry an `id` plus exactly one of `result` or `error`.
//! - Requests carry `method` and an optional `params` object.
//! - Notifications carry `method` only (no `id`).
//!
//! Encoding always emits a single line of valid JSON — no embedded newlines,
//! so the caller can safely split the stdout stream on `\n`.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::error::AcpxError;

/// JSON-RPC 2.0 protocol version literal.
pub const JSONRPC_VERSION: &str = "2.0";

/// Atomic monotonic id allocator. Wraps `AtomicU64` so `SubprocessAcpRuntime`
/// can hand out unique request ids from multiple tasks without a Mutex.
#[derive(Debug, Default)]
pub struct JsonRpcIdAllocator {
    next: AtomicU64,
}

impl JsonRpcIdAllocator {
    pub fn new() -> Self {
        Self {
            next: AtomicU64::new(1),
        }
    }

    /// Allocate the next id and return it.
    pub fn next_id(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }
}

/// Convenience wrapper around [`JsonRpcIdAllocator::next_id`] so callers can
/// write `next_jsonrpc_id(&alloc)` without a method chain.
pub fn next_jsonrpc_id(alloc: &JsonRpcIdAllocator) -> u64 {
    alloc.next_id()
}

/// JSON-RPC 2.0 request frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

/// JSON-RPC 2.0 response frame (carries either `result` or `error`, never
/// both — see `decode_jsonrpc_frame` for the discriminator).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
}

/// JSON-RPC 2.0 server-pushed notification (no `id`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
    /// Always `None` for notifications — kept in the struct so the frame enum
    /// has a single shape. The deserializer drops the field via
    /// `skip_serializing_if` and the field is reconstructed from the frame
    /// discriminator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
}

/// Standard JSON-RPC error body (`code`, `message`, optional `data`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonRpcErrorBody {
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// Discriminated union over the four shapes a JSON-RPC line can take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonRpcFrame {
    Request(JsonRpcRequest),
    Response(JsonRpcResponse),
    Error { id: u64, error: JsonRpcErrorBody },
    Notification(JsonRpcNotification),
}

/// Encode a request frame as a single line of JSON (no trailing newline).
pub fn encode_jsonrpc_request(
    id: u64,
    method: impl Into<String>,
    params: Option<serde_json::Value>,
) -> String {
    serde_json::to_string(&JsonRpcRequest {
        jsonrpc: JSONRPC_VERSION.to_string(),
        id,
        method: method.into(),
        params,
    })
    .expect("request serialization is infallible")
}

/// Encode a response frame as a single line of JSON.
pub fn encode_jsonrpc_response(id: u64, result: &serde_json::Value) -> String {
    serde_json::to_string(&JsonRpcResponse {
        jsonrpc: JSONRPC_VERSION.to_string(),
        id,
        result: Some(result.clone()),
    })
    .expect("response serialization is infallible")
}

/// Encode an error frame as a single line of JSON.
pub fn encode_jsonrpc_error(id: u64, error: JsonRpcErrorBody) -> String {
    serde_json::json!({
        "jsonrpc": JSONRPC_VERSION,
        "id": id,
        "error": error,
    })
    .to_string()
}

/// Encode a notification frame (no `id`) as a single line of JSON.
pub fn encode_jsonrpc_notification(
    method: impl Into<String>,
    params: Option<serde_json::Value>,
) -> String {
    serde_json::to_string(&JsonRpcNotification {
        jsonrpc: JSONRPC_VERSION.to_string(),
        method: method.into(),
        params,
        id: None,
    })
    .expect("notification serialization is infallible")
}

/// Parse a single JSON line into the appropriate [`JsonRpcFrame`] variant.
/// Returns [`AcpxError::JsonRpcParse`] on malformed input or an
/// unsupported `jsonrpc` version.
pub fn parse_jsonrpc_line(line: &str) -> Result<JsonRpcFrame, AcpxError> {
    let value: serde_json::Value =
        serde_json::from_str(line).map_err(|error| AcpxError::JsonRpcParse {
            line: line.to_string(),
            reason: error.to_string(),
        })?;
    decode_jsonrpc_value(&value, line)
}

/// Parse a single JSON line and route through [`parse_jsonrpc_line`] —
/// convenience for callers that already hold a `String`.
pub fn decode_jsonrpc_frame(line: &str) -> Result<JsonRpcFrame, AcpxError> {
    parse_jsonrpc_line(line)
}

fn decode_jsonrpc_value(
    value: &serde_json::Value,
    original_line: &str,
) -> Result<JsonRpcFrame, AcpxError> {
    let object = value.as_object().ok_or_else(|| AcpxError::JsonRpcParse {
        line: original_line.to_string(),
        reason: "expected JSON object".to_string(),
    })?;
    let jsonrpc = object
        .get("jsonrpc")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpxError::JsonRpcParse {
            line: original_line.to_string(),
            reason: "missing `jsonrpc` field".to_string(),
        })?;
    if jsonrpc != JSONRPC_VERSION {
        return Err(AcpxError::JsonRpcParse {
            line: original_line.to_string(),
            reason: format!("unsupported jsonrpc version `{jsonrpc}`"),
        });
    }
    if object.contains_key("error") {
        let id =
            object
                .get("id")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| AcpxError::JsonRpcParse {
                    line: original_line.to_string(),
                    reason: "error frame missing numeric `id`".to_string(),
                })?;
        let error_value = object
            .get("error")
            .cloned()
            .ok_or_else(|| AcpxError::JsonRpcParse {
                line: original_line.to_string(),
                reason: "error frame missing `error`".to_string(),
            })?;
        let body: JsonRpcErrorBody =
            serde_json::from_value(error_value).map_err(|error| AcpxError::JsonRpcParse {
                line: original_line.to_string(),
                reason: format!("invalid error body: {error}"),
            })?;
        return Ok(JsonRpcFrame::Error { id, error: body });
    }
    if object.contains_key("result") {
        let id =
            object
                .get("id")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| AcpxError::JsonRpcParse {
                    line: original_line.to_string(),
                    reason: "response frame missing numeric `id`".to_string(),
                })?;
        let result = object.get("result").cloned();
        return Ok(JsonRpcFrame::Response(JsonRpcResponse {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            result,
        }));
    }
    if object.contains_key("method") {
        let method = object
            .get("method")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AcpxError::JsonRpcParse {
                line: original_line.to_string(),
                reason: "method frame missing `method`".to_string(),
            })?
            .to_string();
        let params = object.get("params").cloned();
        let id = object.get("id").and_then(|v| v.as_u64());
        if id.is_some() {
            let id = id.unwrap();
            return Ok(JsonRpcFrame::Request(JsonRpcRequest {
                jsonrpc: JSONRPC_VERSION.to_string(),
                id,
                method,
                params,
            }));
        }
        return Ok(JsonRpcFrame::Notification(JsonRpcNotification {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method,
            params,
            id: None,
        }));
    }
    Err(AcpxError::JsonRpcParse {
        line: original_line.to_string(),
        reason: "frame missing `method`, `result`, and `error`".to_string(),
    })
}

/// If the given JSON value is a JSON-RPC error object, return the parsed
/// `JsonRpcErrorBody`. Returns `None` for result-bearing frames.
pub fn jsonrpc_error_from_value(value: &serde_json::Value) -> Option<JsonRpcErrorBody> {
    let object = value.as_object()?;
    if !object.contains_key("error") {
        return None;
    }
    serde_json::from_value(object.get("error").cloned()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_request_skips_params_when_none() {
        let body = encode_jsonrpc_request(1, "session/status", None);
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(parsed.get("params").is_none());
    }

    #[test]
    fn decode_value_routes_request_with_id_as_request() {
        let v = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "session/prompt",
            "params": {"k": "v"}
        });
        let frame = decode_jsonrpc_value(&v, "{}").unwrap();
        match frame {
            JsonRpcFrame::Request(r) => {
                assert_eq!(r.id, 9);
                assert_eq!(r.method, "session/prompt");
            }
            other => panic!("expected request, got {other:?}"),
        }
    }
}
