//! R416 — Integration tests for `pc-adapter-pi-local::pi_stream_json`.
//!
//! Mirrors Node `packages/adapters/pi-local/src/server/parse.ts`:
//! - `parsePiJsonl` full event coverage.
//! - `isPiUnknownSessionError` recognition.
//! - tool_call_start / tool_execution_end / turn_end linkage.
//! - usage compatibility (Pi format + generic format).
//! - RPC internal events skipped.

use pc_adapter_pi_local::{
    is_pi_unknown_session_error, parse_pi_jsonl, ParsedPiOutput, PiToolCall,
};

// ---------------------------------------------------------------------------
// turn_end / agent_end / auto_retry_end
// ---------------------------------------------------------------------------

#[test]
fn turn_end_提取文本_usage与cost() {
    let parsed = parse_pi_jsonl(
        r#"{"type":"turn_end","message":{"role":"assistant","content":"final answer","usage":{"input":120,"output":40,"cacheRead":20,"cost":{"total":0.0025}}}}"#,
    );
    assert_eq!(parsed.final_message.as_deref(), Some("final answer"));
    assert_eq!(parsed.messages, vec!["final answer".to_string()]);
    assert_eq!(parsed.usage.input_tokens, 120);
    assert_eq!(parsed.usage.output_tokens, 40);
    assert_eq!(parsed.usage.cached_input_tokens, Some(20));
    assert_eq!(parsed.usage.cost_usd, Some(0.0025));
}

#[test]
fn agent_end取最后_assistant_content() {
    let parsed = parse_pi_jsonl(
        r#"{"type":"agent_end","sessionId":"s-final","messages":[{"role":"user","content":"hi"},{"role":"assistant","content":"reply from agent_end"}]}"#,
    );
    assert_eq!(parsed.final_message.as_deref(), Some("reply from agent_end"));
    assert_eq!(parsed.session_id.as_deref(), Some("s-final"));
}

#[test]
fn auto_retry_end失败记录final_error() {
    let parsed = parse_pi_jsonl(
        r#"{"type":"auto_retry_end","success":false,"finalError":"429 rate limit"}"#,
    );
    assert_eq!(parsed.errors, vec!["429 rate limit".to_string()]);
}

#[test]
fn auto_retry_end成功无错误() {
    let parsed = parse_pi_jsonl(r#"{"type":"auto_retry_end","success":true}"#);
    assert!(parsed.errors.is_empty());
}

#[test]
fn auto_retry_end_空final_error_有默认提示() {
    let parsed = parse_pi_jsonl(r#"{"type":"auto_retry_end","success":false}"#);
    assert_eq!(
        parsed.errors,
        vec!["Pi exhausted automatic retries without producing a response.".to_string()]
    );
}

// ---------------------------------------------------------------------------
// tool_calls state machine
// ---------------------------------------------------------------------------

#[test]
fn tool_execution_start_end_写回result() {
    let parsed = parse_pi_jsonl(
        r#"{"type":"tool_execution_start","toolCallId":"call_1","toolName":"read","args":{"path":"a.txt"}}
{"type":"tool_execution_end","toolCallId":"call_1","toolName":"read","result":"contents","isError":false}"#,
    );
    assert_eq!(parsed.tool_calls.len(), 1);
    let tc = &parsed.tool_calls[0];
    assert_eq!(tc.tool_call_id, "call_1");
    assert_eq!(tc.tool_name, "read");
    assert_eq!(tc.result.as_deref(), Some("contents"));
    assert!(!tc.is_error);
}

#[test]
fn tool_execution_end_无start_兜底创建() {
    let parsed = parse_pi_jsonl(
        r#"{"type":"tool_execution_end","toolCallId":"orphan","toolName":"bash","result":"oops","isError":true}"#,
    );
    assert_eq!(parsed.tool_calls.len(), 1);
    assert!(parsed.tool_calls[0].is_error);
}

#[test]
fn turn_end_toolResults_匹配已有toolCall() {
    let parsed = parse_pi_jsonl(
        r#"{"type":"tool_execution_start","toolCallId":"c1","toolName":"write","args":{"path":"x"}}
{"type":"turn_end","message":{"role":"assistant","content":"done"},"toolResults":[{"toolCallId":"c1","content":"written","isError":false}]}"#,
    );
    assert_eq!(parsed.tool_calls.len(), 1);
    assert_eq!(parsed.tool_calls[0].result.as_deref(), Some("written"));
    assert!(!parsed.tool_calls[0].is_error);
}

#[test]
fn 多次tool_calls_各自独立() {
    let parsed = parse_pi_jsonl(
        r#"{"type":"tool_execution_start","toolCallId":"a","toolName":"read"}
{"type":"tool_execution_end","toolCallId":"a","toolName":"read","result":"ok-a","isError":false}
{"type":"tool_execution_start","toolCallId":"b","toolName":"bash"}
{"type":"tool_execution_end","toolCallId":"b","toolName":"bash","result":"ok-b","isError":false}"#,
    );
    assert_eq!(parsed.tool_calls.len(), 2);
    let by_id: std::collections::HashMap<&str, &PiToolCall> = parsed
        .tool_calls
        .iter()
        .map(|tc| (tc.tool_call_id.as_str(), tc))
        .collect();
    assert_eq!(by_id["a"].result.as_deref(), Some("ok-a"));
    assert_eq!(by_id["b"].result.as_deref(), Some("ok-b"));
}

// ---------------------------------------------------------------------------
// Streaming text delta
// ---------------------------------------------------------------------------

#[test]
fn message_update_text_delta拼接() {
    let parsed = parse_pi_jsonl(
        r#"{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"Hel"}}
{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"lo "}}
{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"world"}}"#,
    );
    assert_eq!(parsed.messages, vec!["Hello world".to_string()]);
}

#[test]
fn message_update_非text_delta_忽略() {
    let parsed = parse_pi_jsonl(
        r#"{"type":"message_update","assistantMessageEvent":{"type":"tool_use","id":"x"}}
{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"only-this"}}"#,
    );
    assert_eq!(parsed.messages, vec!["only-this".to_string()]);
}

// ---------------------------------------------------------------------------
// Standalone usage event (both formats)
// ---------------------------------------------------------------------------

#[test]
fn standalone_usage_Pi格式累加() {
    let parsed = parse_pi_jsonl(
        r#"{"type":"usage","usage":{"input":100,"output":30,"cacheRead":10,"cost":{"total":0.001}}}
{"type":"usage","usage":{"input":50,"output":15,"cacheRead":5,"cost":{"total":0.0005}}}"#,
    );
    assert_eq!(parsed.usage.input_tokens, 150);
    assert_eq!(parsed.usage.output_tokens, 45);
    assert_eq!(parsed.usage.cached_input_tokens, Some(15));
    assert_eq!(parsed.usage.cost_usd, Some(0.0015));
}

#[test]
fn standalone_usage_generic格式兼容() {
    let parsed = parse_pi_jsonl(
        r#"{"type":"usage","usage":{"inputTokens":100,"outputTokens":30,"cachedInputTokens":10,"costUsd":0.002}}"#,
    );
    assert_eq!(parsed.usage.input_tokens, 100);
    assert_eq!(parsed.usage.output_tokens, 30);
    assert_eq!(parsed.usage.cached_input_tokens, Some(10));
    assert_eq!(parsed.usage.cost_usd, Some(0.002));
}

#[test]
fn turn_end_usage与_standalone_usage叠加() {
    let parsed = parse_pi_jsonl(
        r#"{"type":"turn_end","message":{"role":"assistant","content":"hi","usage":{"input":10,"output":5,"cacheRead":2,"cost":{"total":0.5}}}}
{"type":"usage","usage":{"input":20,"output":10,"cacheRead":4,"cost":{"total":0.25}}}"#,
    );
    assert_eq!(parsed.usage.input_tokens, 30);
    assert_eq!(parsed.usage.output_tokens, 15);
    assert_eq!(parsed.usage.cached_input_tokens, Some(6));
    assert_eq!(parsed.usage.cost_usd, Some(0.75));
}

// ---------------------------------------------------------------------------
// error events
// ---------------------------------------------------------------------------

#[test]
fn error事件记录消息() {
    let parsed = parse_pi_jsonl(r#"{"type":"error","message":"upstream timeout"}"#);
    assert_eq!(parsed.errors, vec!["upstream timeout".to_string()]);
}

#[test]
fn error空消息跳过() {
    let parsed = parse_pi_jsonl(r#"{"type":"error","message":""}"#);
    assert!(parsed.errors.is_empty());
}

#[test]
fn 多error事件累积() {
    let parsed = parse_pi_jsonl(
        r#"{"type":"error","message":"first"}
{"type":"error","message":"second"}"#,
    );
    assert_eq!(
        parsed.errors,
        vec!["first".to_string(), "second".to_string()]
    );
}

// ---------------------------------------------------------------------------
// RPC / lifecycle events skipped
// ---------------------------------------------------------------------------

#[test]
fn rpc_内部事件全部跳过() {
    let parsed = parse_pi_jsonl(
        r#"{"type":"response","id":"1","result":{}}
{"type":"extension_ui_request","id":"2"}
{"type":"extension_ui_response","id":"3"}
{"type":"extension_error","id":"4"}
{"type":"agent_start","id":"5"}
{"type":"turn_start","id":"6"}"#,
    );
    assert!(parsed.messages.is_empty());
    assert!(parsed.errors.is_empty());
    assert!(parsed.tool_calls.is_empty());
    assert!(parsed.final_message.is_none());
}

// ---------------------------------------------------------------------------
// Unknown session detection
// ---------------------------------------------------------------------------

#[test]
fn unknown_session_识别_session_not_found() {
    assert!(is_pi_unknown_session_error("Session not found: abc123", ""));
    assert!(is_pi_unknown_session_error("", "Session not found"));
}

#[test]
fn unknown_session_识别_unknown_session() {
    assert!(is_pi_unknown_session_error("", "unknown session id: s1"));
}

#[test]
fn unknown_session_识别_no_session() {
    assert!(is_pi_unknown_session_error("", "no session available"));
}

#[test]
fn unknown_session_识别_session_x_not_found() {
    assert!(is_pi_unknown_session_error("error: session abcdef not found", ""));
}

#[test]
fn unknown_session_正常文本不触发() {
    assert!(!is_pi_unknown_session_error("all good", ""));
    assert!(!is_pi_unknown_session_error("", ""));
    assert!(!is_pi_unknown_session_error("session ready", ""));
}

// ---------------------------------------------------------------------------
// Robustness
// ---------------------------------------------------------------------------

#[test]
fn 非json行安全忽略() {
    let parsed = parse_pi_jsonl(
        "not-json-line\n[broken\n{\"type\":\"turn_end\",\"message\":{\"role\":\"assistant\",\"content\":\"ok\"}}",
    );
    assert_eq!(parsed.final_message.as_deref(), Some("ok"));
}

#[test]
fn 空输入返回默认值() {
    let parsed = parse_pi_jsonl("");
    assert_eq!(parsed, ParsedPiOutput::default());
}

#[test]
fn content数组提取_text_合并() {
    let parsed = parse_pi_jsonl(
        r#"{"type":"agent_end","messages":[{"role":"assistant","content":[{"type":"text","text":"hello "},{"type":"text","text":"world"}]}]}"#,
    );
    // Node 行为：按 text 段拼接（空 join）。
    assert_eq!(parsed.final_message.as_deref(), Some("hello world"));
}

// ---------------------------------------------------------------------------
// sessionId 兼容多种命名
// ---------------------------------------------------------------------------

#[test]
fn sessionId_顶层兼容_sessionId() {
    let parsed = parse_pi_jsonl(r#"{"type":"turn_end","sessionId":"abc","message":{"role":"assistant","content":"x"}}"#);
    assert_eq!(parsed.session_id.as_deref(), Some("abc"));
}

#[test]
fn sessionId_顶层兼容_sessionID() {
    let parsed = parse_pi_jsonl(r#"{"type":"turn_end","sessionID":"xyz","message":{"role":"assistant","content":"x"}}"#);
    assert_eq!(parsed.session_id.as_deref(), Some("xyz"));
}
