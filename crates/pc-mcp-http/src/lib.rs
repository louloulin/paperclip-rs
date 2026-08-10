#![forbid(unsafe_code)]
//! `pc-mcp-http` —— MCP Streamable HTTP transport helpers.
//!
//! 对应 Node `server/src/services/mcp-http.ts`（84 行）。
//!
//! ## 设计目标
//!
//! - **Streamable HTTP 协议**：客户端必须发 `Accept: application/json, text/event-stream`，
//!   服务端可用任一格式响应（106 不接受其它 Accept）。
//! - **SSE 解析**：当响应是 `text/event-stream` 时，需要解析出真正的 JSON-RPC 消息。
//! - **零运行时**：所有函数纯函数 + 不依赖网络栈（caller 负责 `reqwest` / `hyper` 等）。
//!
//! ## 公共 API
//!
//! - [`MCP_HTTP_ACCEPT`] —— `"application/json, text/event-stream"` 常量
//! - [`mcp_http_request_headers`] —— 构造 POST headers（保持 caller 自定义 headers，
//!   但 Authoritative Accept）
//! - [`parse_mcp_http_response_body`] —— 解析 JSON 或 SSE-framed 响应为 JSON-RPC 消息
//! - [`looks_like_json_rpc_message`] —— 判断是否是 JSON-RPC 消息
//!
//! ## 设计原则
//!
//! - **高内聚**：transport helpers 集中在本 crate。
//! - **低耦合**：不依赖网络 / DB；caller 可独立测试。
//! - **可测**：纯函数 + 大覆盖单测。

use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

// ============================================================================
// Constants
// ============================================================================

/// MCP Streamable HTTP 要求的 Accept header 值。
///
/// 与 Node `MCP_HTTP_ACCEPT = "application/json, text/event-stream"` 1:1 对齐。
pub const MCP_HTTP_ACCEPT: &str = "application/json, text/event-stream";

/// SSE `event:` / `data:` 字段名（Spec 固定字符串）。
pub const SSE_FIELD_DATA: &str = "data";

// ============================================================================
// Errors
// ============================================================================

/// MCP HTTP 响应解析错误。
#[derive(Debug, Error)]
pub enum McpHttpError {
    /// SSE 响应无 `data:` 事件。
    #[error("MCP SSE response contained no data events")]
    NoDataEvents,

    /// 响应 body 既不是合法 JSON 也不是合法 SSE 帧。
    #[error("failed to parse MCP response body: {0}")]
    ParseError(String),
}

pub type McpHttpResult<T> = Result<T, McpHttpError>;

// ============================================================================
// Request headers
// ============================================================================

/// 构造 MCP Streamable HTTP POST headers（与 Node `mcpHttpRequestHeaders(extra?)` 1:1 对齐）。
///
/// 行为细节：
/// - 总是设置 `content-type: application/json` + `accept: MCP_HTTP_ACCEPT`
/// - caller 自定义 headers (`extra`) 中的 `accept` / `content-type` 被覆盖（保护规范要求）
/// - 其它 caller 自定义 header 保留
pub fn mcp_http_request_headers(
    extra: Option<&std::collections::HashMap<String, String>>,
) -> std::collections::HashMap<String, String> {
    let mut headers = std::collections::HashMap::new();
    headers.insert("content-type".to_string(), "application/json".to_string());
    if let Some(extra) = extra {
        for (k, v) in extra {
            headers.insert(k.clone(), v.clone());
        }
    }
    // Authoritative Accept — 即使 caller 传了 `accept`，我们强制覆盖
    headers.insert("accept".to_string(), MCP_HTTP_ACCEPT.to_string());
    headers
}

// ============================================================================
// JSON-RPC detection
// ============================================================================

/// 判断 `value` 是否看起来像一个 JSON-RPC 消息。
///
/// 与 Node `looksLikeJsonRpcMessage(value)` 1:1 对齐：
/// - `result` / `error` / `method` / `id` 任意 key 存在即可。
pub fn looks_like_json_rpc_message(value: &Value) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    obj.contains_key("result")
        || obj.contains_key("error")
        || obj.contains_key("method")
        || obj.contains_key("id")
}

// ============================================================================
// SSE event parsing
// ============================================================================

/// 解析一行 SSE 行，得到 `(field, value)`。
///
/// 例如 `"data: hello"` → `("data", " hello")`（含前导空格）。
/// `"data:hello"` → `("data", "hello")`。
/// `"event: message"` → `("event", " message")`。
///
/// 不匹配 SSE 行（如 `:comment` 注释 / 其它 field）→ 返回 `None`。
fn parse_sse_line(line: &str) -> Option<(&str, &str)> {
    if line.is_empty() || line.starts_with(':') {
        return None;
    }
    if let Some(idx) = line.find(':') {
        let field = &line[..idx];
        let value = &line[idx + 1..];
        // remove single leading space (SSE 规范)
        let value = value.strip_prefix(' ').unwrap_or(value);
        Some((field, value))
    } else {
        Some((line, ""))
    }
}

/// 把一段 SSE body 字符串解析为 [(field → data_lines)] events。
pub fn parse_sse_events(body_text: &str) -> Vec<std::collections::HashMap<String, String>> {
    // 1) 标准化 CRLF → LF
    let normalized = body_text.replace("\r\n", "\n");
    // 2) 按空行切分多个 events
    let mut out = Vec::new();
    for raw in normalized.split("\n\n") {
        let mut event: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        for line in raw.split('\n') {
            if let Some((field, value)) = parse_sse_line(line) {
                // multi-line data: spec says concatenate with \n
                event
                    .entry(field.to_string())
                    .and_modify(|existing| {
                        existing.push('\n');
                        existing.push_str(value);
                    })
                    .or_insert_with(|| value.to_string());
            }
        }
        if !event.is_empty() {
            out.push(event);
        }
    }
    out
}

/// 从一段 SSE 事件列表里抽取第一个 JSON-RPC 消息。
///
/// 返回：找到的 JSON 解析值；如果 `data:` 都不可解析为 JSON-RPC，则回退到第一个解析成功的值；
/// 如果 `data:` 全部解析失败，抛 [`McpHttpError::NoDataEvents`]。
pub fn first_json_rpc_from_events(
    body_text: &str,
) -> McpHttpResult<Value> {
    let events = parse_sse_events(body_text);
    let mut last_error: Option<String> = None;
    let mut first_parsed: Option<Value> = None;
    for event in &events {
        let Some(data) = event.get(SSE_FIELD_DATA) else { continue };
        let parsed: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(e) => {
                last_error = Some(e.to_string());
                continue;
            }
        };
        if first_parsed.is_none() {
            first_parsed = Some(parsed.clone());
        }
        if looks_like_json_rpc_message(&parsed) {
            return Ok(parsed);
        }
    }
    if let Some(first) = first_parsed {
        return Ok(first);
    }
    if let Some(err) = last_error {
        return Err(McpHttpError::ParseError(err));
    }
    Err(McpHttpError::NoDataEvents)
}

// ============================================================================
// Response body parsing
// ============================================================================

/// 解析 MCP HTTP 响应 body 为 JSON-RPC 消息。
///
/// 与 Node `parseMcpHttpResponseBody(bodyText, contentType)` 1:1 对齐：
/// - `contentType` 含 `text/event-stream` → SSE 解析
/// - 否则 → 直接 `JSON.parse(bodyText)`
/// - 异常包装到 [`McpHttpError`]
pub fn parse_mcp_http_response_body(body_text: &str, content_type: Option<&str>) -> McpHttpResult<Value> {
    let is_event_stream = content_type
        .map(|ct| ct.to_lowercase().contains("text/event-stream"))
        .unwrap_or(false);
    if !is_event_stream {
        return serde_json::from_str(body_text).map_err(|e| McpHttpError::ParseError(e.to_string()));
    }
    first_json_rpc_from_events(body_text)
}

/// 同 [`parse_mcp_http_response_body`] 但返回 `serde_json::Value` (与签名一致)。
pub fn parse_mcp_http_response_value(body_text: &str, content_type: Option<&str>) -> McpHttpResult<Value> {
    parse_mcp_http_response_body(body_text, content_type)
}

/// 包装结果为 serde-serializable（HTTP handler 直接 JSON 返回）。
#[derive(Debug, Serialize)]
pub struct ParseOutcome<'a> {
    pub value: &'a Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn r681_accept_constant_matches_node() {
        assert_eq!(MCP_HTTP_ACCEPT, "application/json, text/event-stream");
    }

    #[test]
    fn r681_request_headers_default() {
        let h = mcp_http_request_headers(None);
        assert_eq!(h.get("content-type"), Some(&"application/json".to_string()));
        assert_eq!(h.get("accept"), Some(&MCP_HTTP_ACCEPT.to_string()));
    }

    #[test]
    fn r681_request_headers_preserves_caller_extras() {
        let mut extra = std::collections::HashMap::new();
        extra.insert("authorization".to_string(), "Bearer xyz".to_string());
        extra.insert("user-agent".to_string(), "paperclip-rs".to_string());
        let h = mcp_http_request_headers(Some(&extra));
        assert_eq!(h.get("authorization"), Some(&"Bearer xyz".to_string()));
        assert_eq!(h.get("user-agent"), Some(&"paperclip-rs".to_string()));
        assert_eq!(h.get("accept"), Some(&MCP_HTTP_ACCEPT.to_string()));
    }

    #[test]
    fn r681_request_headers_caller_accept_is_overridden() {
        let mut extra = std::collections::HashMap::new();
        extra.insert("accept".to_string(), "text/plain".to_string());
        let h = mcp_http_request_headers(Some(&extra));
        assert_eq!(h.get("accept"), Some(&MCP_HTTP_ACCEPT.to_string()));
    }

    #[test]
    fn r681_looks_like_json_rpc_message_matches_node() {
        assert!(looks_like_json_rpc_message(&json!({"result": 1})));
        assert!(looks_like_json_rpc_message(&json!({"error": {"code": 1}})));
        assert!(looks_like_json_rpc_message(&json!({"method": "tools/list"})));
        assert!(looks_like_json_rpc_message(&json!({"id": 1})));
        assert!(!looks_like_json_rpc_message(&json!({"foo": "bar"})));
        assert!(!looks_like_json_rpc_message(&json!(null)));
        assert!(!looks_like_json_rpc_message(&json!("string")));
        assert!(!looks_like_json_rpc_message(&json!(123)));
    }

    #[test]
    fn r681_parse_sse_basic_event() {
        let s = "data: hello\n\n";
        let events = parse_sse_events(s);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].get("data"), Some(&"hello".to_string()));
    }

    #[test]
    fn r681_parse_sse_skips_comments() {
        let s = ": this is a comment\ndata: hello\n\n";
        let events = parse_sse_events(s);
        assert_eq!(events.len(), 1);
        assert!(events[0].contains_key("data"));
        assert!(!events[0].contains_key(": this is a comment"));
    }

    #[test]
    fn r681_parse_sse_multiline_data() {
        let s = "data: line1\ndata: line2\n\n";
        let events = parse_sse_events(s);
        assert_eq!(events.len(), 1);
        let data = events[0].get("data").unwrap();
        assert_eq!(data, "line1\nline2");
    }

    #[test]
    fn r681_parse_sse_multiple_events() {
        let s = "data: {\"a\":1}\n\ndata: {\"a\":2}\n\n";
        let events = parse_sse_events(s);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].get("data"), Some(&"{\"a\":1}".to_string()));
        assert_eq!(events[1].get("data"), Some(&"{\"a\":2}".to_string()));
    }

    #[test]
    fn r681_parse_json_response_body() {
        let body = r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[]}}"#;
        let v = parse_mcp_http_response_body(body, Some("application/json")).expect("parse json");
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["id"], 1);
        assert!(v["result"].is_object());
    }

    #[test]
    fn r681_parse_sse_response_body_with_json_rpc_message() {
        let body = "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"tools\":[]}}\n\n";
        let v = parse_mcp_http_response_body(body, Some("text/event-stream"))
            .expect("parse sse");
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["id"], 1);
    }

    #[test]
    fn r681_parse_sse_response_body_picks_first_json_rpc() {
        // 两个 events，第一个不是 JSON-RPC (heartbeat)，第二个是
        let body = "data: heartbeat\n\ndata: {\"id\":2,\"result\":42}\n\n";
        let v = parse_mcp_http_response_body(body, Some("text/event-stream"))
            .expect("parse sse");
        assert_eq!(v["id"], 2);
        assert_eq!(v["result"], 42);
    }

    #[test]
    fn r681_empty_sse_body_returns_error()
    {
        let body = "";
        let err = parse_mcp_http_response_body(body, Some("text/event-stream")).unwrap_err();
        match err {
            McpHttpError::NoDataEvents => {}
            other => panic!("expected NoDataEvents, got {other:?}"),
        }
    }

    #[test]
    fn r681_no_data_events_returns_no_data_error() {
        let body = "event: ping\n\n";
        let err = parse_mcp_http_response_body(body, Some("text/event-stream")).unwrap_err();
        match err {
            McpHttpError::NoDataEvents => {}
            other => panic!("expected NoDataEvents, got {other:?}"),
        }
    }

    #[test]
    fn r681_invalid_json_returns_parse_error() {
        let body = "data: not_json_garbage\n\n";
        // 没有其它事件能解析 → 应该报错（NoDataEvents 之前的 fallback）
        let result = parse_mcp_http_response_body(body, Some("text/event-stream"));
        assert!(result.is_err(), "should fail");
    }

    #[test]
    fn r681_non_json_non_sse_content_type_falls_back_to_json_parse() {
        let body = r#"{"id":3,"result":"ok"}"#;
        let v = parse_mcp_http_response_body(body, Some("application/octet-stream"))
            .expect("parse json fallback");
        assert_eq!(v["id"], 3);
        assert_eq!(v["result"], "ok");
    }

    #[test]
    fn r681_content_type_case_insensitive_for_event_stream() {
        let body = "data: {\"result\":\"x\"}\n\n";
        let v = parse_mcp_http_response_body(body, Some("Text/Event-Stream"))
            .expect("case insensitive");
        assert_eq!(v["result"], "x");
    }

    #[test]
    fn r681_crlf_in_sse_body_normalized() {
        let body = "data: {\"id\":1}\r\n\r\n";
        let v = parse_mcp_http_response_body(body, Some("text/event-stream"))
            .expect("crlf parse");
        assert_eq!(v["id"], 1);
    }

    #[test]
    fn r681_content_type_none_falls_back_to_json() {
        let body = r#"{"id":4,"result":true}"#;
        let v = parse_mcp_http_response_body(body, None).expect("none ct");
        assert_eq!(v["id"], 4);
    }

    #[test]
    fn r681_first_json_rpc_from_events_returns_first_parsed_when_no_json_rpc() {
        // 所有 events 都是合法 JSON 但不是 JSON-RPC → 返回第一个 parsed
        let body = "data: {\"hello\":\"world\"}\n\ndata: {\"foo\":\"bar\"}\n\n";
        let v = first_json_rpc_from_events(body).expect("first parsed");
        assert_eq!(v["hello"], "world");
    }
}
