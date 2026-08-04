//! MCP Streamable HTTP 传输层辅助
//!
//! 对齐 Node `services/mcp-http.ts`（84 行）：
//! - 常量 `MCP_HTTP_ACCEPT = "application/json, text/event-stream"` —— 强制发送的双 Accept 头
//! - 函数 `mcp_http_request_headers(extra)` —— 构建 JSON-RPC POST headers
//! - 函数 `parse_mcp_http_response_body(body_text, content_type)` —— 解析 JSON 或 SSE 响应
//! - 内部 `looks_like_json_rpc_message(value)` —— 判定 JSON-RPC 消息
//!
//! 设计：
//! - 纯函数无副作用，方便单测
//! - SSE 解析兼容多行 `data:`、空行分隔事件、`\r\n` / `\n` 混用
//! - 容错：content_type 未知时回退到纯 JSON 解析（兼容不合规服务端）
//! - JSON-RPC 消息识别：必须包含 `result` / `error` / `method` / `id` 任一字段

use std::collections::BTreeMap;

// ============================================================================
// Constants
// ============================================================================

/// MCP Streamable HTTP 传输层要求的 `Accept` header。
///
/// 对齐 Node `MCP_HTTP_ACCEPT`。
pub const MCP_HTTP_ACCEPT: &str = "application/json, text/event-stream";

// ============================================================================
// Public API
// ============================================================================

/// 构建 MCP Streamable HTTP JSON-RPC POST 请求的默认 headers。
///
/// - 始终设置 `content-type: application/json`
/// - 始终设置 `accept: MCP_HTTP_ACCEPT`（覆盖调用方传入的 accept，保证 spec 合规）
/// - 调用方传入的 `extra` 中其它字段原样保留
///
/// 对齐 Node `mcpHttpRequestHeaders`。
pub fn mcp_http_request_headers(
    extra: Option<&BTreeMap<String, String>>,
) -> BTreeMap<String, String> {
    let mut headers = BTreeMap::new();
    headers.insert("content-type".to_string(), "application/json".to_string());
    if let Some(extra) = extra {
        for (k, v) in extra {
            headers.insert(k.clone(), v.clone());
        }
    }
    // Authoritative override of accept (last-write-wins via BTreeMap semantics).
    headers.insert("accept".to_string(), MCP_HTTP_ACCEPT.to_string());
    headers
}

/// 解析 MCP Streamable HTTP 响应 body 为 JSON-RPC 消息。
///
/// - `application/json` → 直接 `JSON.parse(bodyText)`
/// - `text/event-stream` → 解析 SSE，按 `data:` 行提取第一个像 JSON-RPC 的消息
/// - 未知 content_type → 回退到纯 JSON 解析（兼容不合规服务端）
///
/// SSE 处理：
/// - 多行 `data:` 字段用 `\n` 拼接
/// - 事件以空行分隔
/// - `\r\n` 与 `\n` 都被接受
///
/// 对齐 Node `parseMcpHttpResponseBody`。
pub fn parse_mcp_http_response_body(
    body_text: &str,
    content_type: Option<&str>,
) -> Result<serde_json::Value, McpHttpParseError> {
    let ct_lower = content_type
        .map(|c| c.to_ascii_lowercase())
        .unwrap_or_default();
    let is_event_stream = ct_lower.contains("text/event-stream");

    if !is_event_stream {
        // Fallback to plain JSON
        return serde_json::from_str(body_text).map_err(McpHttpParseError::Json);
    }

    // Normalize CRLF to LF, then split on blank lines
    let normalized = body_text.replace("\r\n", "\n");
    let events: Vec<&str> = normalized.split("\n\n").collect();

    let mut last_error: Option<McpHttpParseError> = None;
    let mut first_parsed: Option<serde_json::Value> = None;

    for event in events {
        let mut data_lines: Vec<&str> = Vec::new();
        for line in event.split('\n') {
            if let Some(rest) = line.strip_prefix("data:") {
                // Strip optional leading space per SSE spec
                let trimmed = rest.strip_prefix(' ').unwrap_or(rest);
                data_lines.push(trimmed);
            }
        }
        if data_lines.is_empty() {
            continue;
        }
        let data = data_lines.join("\n");
        match serde_json::from_str::<serde_json::Value>(&data) {
            Ok(parsed) => {
                if first_parsed.is_none() {
                    first_parsed = Some(parsed.clone());
                }
                if looks_like_json_rpc_message(&parsed) {
                    return Ok(parsed);
                }
            }
            Err(e) => {
                last_error = Some(McpHttpParseError::Json(e));
            }
        }
    }

    if let Some(p) = first_parsed {
        return Ok(p);
    }
    if let Some(e) = last_error {
        return Err(e);
    }
    Err(McpHttpParseError::NoDataEvents)
}

/// 判断 value 是否像 JSON-RPC 消息（包含 `result` / `error` / `method` / `id` 任一字段）。
///
/// 对齐 Node `looksLikeJsonRpcMessage`。
pub fn looks_like_json_rpc_message(value: &serde_json::Value) -> bool {
    if !value.is_object() {
        return false;
    }
    let obj = value.as_object().unwrap();
    obj.contains_key("result")
        || obj.contains_key("error")
        || obj.contains_key("method")
        || obj.contains_key("id")
}

// ============================================================================
// Errors
// ============================================================================

/// MCP HTTP 响应解析错误。
#[derive(Debug, thiserror::Error)]
pub enum McpHttpParseError {
    #[error("JSON parse error: {0}")]
    Json(#[source] serde_json::Error),
    #[error("MCP SSE response contained no data events")]
    NoDataEvents,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // -----------------------------------------------------------------------
    // MCP_HTTP_ACCEPT
    // -----------------------------------------------------------------------

    #[test]
    fn accept_header_value_matches_node() {
        assert_eq!(MCP_HTTP_ACCEPT, "application/json, text/event-stream");
    }

    // -----------------------------------------------------------------------
    // mcp_http_request_headers
    // -----------------------------------------------------------------------

    #[test]
    fn request_headers_basic() {
        let h = mcp_http_request_headers(None);
        assert_eq!(h.get("content-type"), Some(&"application/json".to_string()));
        assert_eq!(h.get("accept"), Some(&MCP_HTTP_ACCEPT.to_string()));
    }

    #[test]
    fn request_headers_preserves_extra() {
        let mut extra = BTreeMap::new();
        extra.insert("authorization".to_string(), "Bearer xyz".to_string());
        extra.insert("x-custom".to_string(), "value".to_string());
        let h = mcp_http_request_headers(Some(&extra));
        assert_eq!(h.get("authorization"), Some(&"Bearer xyz".to_string()));
        assert_eq!(h.get("x-custom"), Some(&"value".to_string()));
        assert_eq!(h.get("content-type"), Some(&"application/json".to_string()));
        assert_eq!(h.get("accept"), Some(&MCP_HTTP_ACCEPT.to_string()));
    }

    #[test]
    fn request_headers_accept_is_authoritative() {
        // Per Node: caller's accept is overwritten by the MCP required value
        let mut extra = BTreeMap::new();
        extra.insert("accept".to_string(), "application/json".to_string());
        let h = mcp_http_request_headers(Some(&extra));
        assert_eq!(h.get("accept"), Some(&MCP_HTTP_ACCEPT.to_string()));
    }

    // -----------------------------------------------------------------------
    // parse_mcp_http_response_body: JSON branch
    // -----------------------------------------------------------------------

    #[test]
    fn parse_json_body() {
        let body = r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#;
        let parsed = parse_mcp_http_response_body(body, Some("application/json")).unwrap();
        assert_eq!(parsed, json!({"jsonrpc":"2.0","id":1,"result":{"ok":true}}));
    }

    #[test]
    fn parse_unknown_content_type_falls_back_to_json() {
        let body = r#"{"result":42}"#;
        let parsed = parse_mcp_http_response_body(body, None).unwrap();
        assert_eq!(parsed, json!({"result":42}));
    }

    #[test]
    fn parse_json_invalid_returns_error() {
        let body = "not json";
        let err = parse_mcp_http_response_body(body, Some("application/json")).unwrap_err();
        assert!(matches!(err, McpHttpParseError::Json(_)));
    }

    // -----------------------------------------------------------------------
    // parse_mcp_http_response_body: SSE branch
    // -----------------------------------------------------------------------

    #[test]
    fn parse_sse_with_data_event() {
        let body =
            "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}\n\n";
        let parsed = parse_mcp_http_response_body(body, Some("text/event-stream")).unwrap();
        assert_eq!(parsed, json!({"jsonrpc":"2.0","id":1,"result":{"ok":true}}));
    }

    #[test]
    fn parse_sse_multiline_data() {
        // Per SSE spec, multiple data: lines are joined with \n
        let body = "data: {\"a\":\ndata: 1,\"b\":2}\n\n";
        let parsed = parse_mcp_http_response_body(body, Some("text/event-stream")).unwrap();
        assert_eq!(parsed, json!({"a":1,"b":2}));
    }

    #[test]
    fn parse_sse_crlf_normalized() {
        let body = "event: message\r\ndata: {\"id\":1,\"result\":{}}\r\n\r\n";
        let parsed = parse_mcp_http_response_body(body, Some("text/event-stream")).unwrap();
        assert_eq!(parsed, json!({"id":1,"result":{}}));
    }

    #[test]
    fn parse_sse_skips_non_jsonrpc_data_event() {
        // First data event is a comment (not JSON-RPC), second is the real message
        let body = "data: {\"comment\":\"hello\"}\n\ndata: {\"id\":1,\"result\":{\"ok\":true}}\n\n";
        let parsed = parse_mcp_http_response_body(body, Some("text/event-stream")).unwrap();
        assert_eq!(parsed, json!({"id":1,"result":{"ok":true}}));
    }

    #[test]
    fn parse_sse_falls_back_to_first_parsed_when_no_jsonrpc() {
        // No data event has JSON-RPC shape — return first parsed
        let body = "data: {\"foo\":1}\n\ndata: {\"bar\":2}\n\n";
        let parsed = parse_mcp_http_response_body(body, Some("text/event-stream")).unwrap();
        assert_eq!(parsed, json!({"foo":1}));
    }

    #[test]
    fn parse_sse_no_data_events_returns_error() {
        let body = "event: ping\n\n";
        let err = parse_mcp_http_response_body(body, Some("text/event-stream")).unwrap_err();
        assert!(matches!(err, McpHttpParseError::NoDataEvents));
    }

    #[test]
    fn parse_sse_data_line_with_leading_space() {
        // Per SSE spec, leading space after "data:" is ignored
        let body = "data: {\"id\":1,\"result\":{\"v\":42}}\n\n";
        let parsed = parse_mcp_http_response_body(body, Some("text/event-stream")).unwrap();
        assert_eq!(parsed, json!({"id":1,"result":{"v":42}}));
    }

    #[test]
    fn parse_sse_invalid_json_returns_last_error() {
        let body = "data: not json\n\n";
        let err = parse_mcp_http_response_body(body, Some("text/event-stream")).unwrap_err();
        assert!(matches!(err, McpHttpParseError::Json(_)));
    }

    #[test]
    fn parse_sse_multiple_events_with_bad_then_good() {
        // First event has invalid JSON, second has valid JSON-RPC
        let body = "data: invalid json\n\ndata: {\"id\":1,\"result\":{}}\n\n";
        let parsed = parse_mcp_http_response_body(body, Some("text/event-stream")).unwrap();
        assert_eq!(parsed, json!({"id":1,"result":{}}));
    }

    // -----------------------------------------------------------------------
    // looks_like_json_rpc_message
    // -----------------------------------------------------------------------

    #[test]
    fn looks_like_with_result() {
        assert!(looks_like_json_rpc_message(&json!({"result": {}})));
    }

    #[test]
    fn looks_like_with_error() {
        assert!(looks_like_json_rpc_message(&json!({"error": {}})));
    }

    #[test]
    fn looks_like_with_method() {
        assert!(looks_like_json_rpc_message(&json!({"method": "x"})));
    }

    #[test]
    fn looks_like_with_id() {
        assert!(looks_like_json_rpc_message(&json!({"id": 1})));
    }

    #[test]
    fn not_looks_like_for_non_object() {
        assert!(!looks_like_json_rpc_message(&json!(null)));
        assert!(!looks_like_json_rpc_message(&json!(42)));
        assert!(!looks_like_json_rpc_message(&json!("string")));
        assert!(!looks_like_json_rpc_message(&json!([])));
    }

    #[test]
    fn not_looks_like_for_object_without_keys() {
        assert!(!looks_like_json_rpc_message(&json!({"foo": "bar"})));
        assert!(!looks_like_json_rpc_message(&json!({})));
    }
}
