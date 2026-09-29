//! [`CursorStreamDecoder`] 的单元用例（从 `stream.rs` 原样搬出，**0 断言改动**）。
//!
//! 拆出来是门 ⑩（单文件 800 行上限，`scripts/file_size_check.py`）的要求；先例 = `docs/32`
//! §30 的 **D10**。

use super::*;

fn decoder() -> CursorStreamDecoder {
    CursorStreamDecoder::new("cursor-model")
}

fn texts(events: &[RuntimeEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            RuntimeEvent::Text { delta } => Some(delta.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn prefixes_are_stripped_only_in_front_of_json() {
    assert_eq!(
        normalize_stream_line("stdout:{\"type\":\"system\"}"),
        "{\"type\":\"system\"}"
    );
    assert_eq!(
        normalize_stream_line("  stderr = {\"type\":\"error\"}  "),
        "{\"type\":\"error\"}"
    );
    // 以 "stdout" 开头但不是前缀的正文不能被切掉。
    assert_eq!(
        normalize_stream_line("stdout is closed"),
        "stdout is closed"
    );
}

#[test]
fn assistant_text_and_thinking_blocks_are_forwarded() {
    let mut decoder = decoder();
    let events = decoder.push_line(
        r#"{"type":"assistant","message":{"model":"cursor-model","usage":{"input_tokens":9,"output_tokens":9},"content":[{"type":"output_text","text":"ok"},{"type":"thinking","text":"想"},{"type":"tool_use","id":"c1","name":"read","input":{"path":"/a"}}]}}"#,
    );
    assert_eq!(texts(&events), "ok");
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, RuntimeEvent::Thinking { .. }))
            .count(),
        1
    );
    assert!(events
        .iter()
        .any(|event| matches!(event, RuntimeEvent::ToolUse { .. })));
    // assistant 里的 usage 不算数（只有 result / step_finish 算）。
    assert_eq!(decoder.summary().usage, Vec::new());
}

fn thinking_of(events: &[RuntimeEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            RuntimeEvent::Thinking { delta } => Some(delta.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn thinking_deltas_get_a_blank_line_between_blocks() {
    let mut decoder = decoder();
    let first = decoder.push_line(r#"{"type":"thinking","subtype":"delta","text":"a"}"#);
    assert_eq!(thinking_of(&first), "a");
    // `completed` 只关块，不产出事件；未知 subtype 也不许并进推理。
    assert!(decoder
        .push_line(r#"{"type":"thinking","subtype":"completed"}"#)
        .is_empty());
    assert!(decoder
        .push_line(r#"{"type":"thinking","subtype":"未来新增","text":"x"}"#)
        .is_empty());
    let second = decoder.push_line(r#"{"type":"thinking","subtype":"delta","text":"b"}"#);
    assert_eq!(thinking_of(&second), "\n\nb");
}

#[test]
fn tool_call_envelope_yields_name_args_and_result() {
    let mut decoder = decoder();
    let started = decoder.push_line(
        r#"{"type":"tool_call","subtype":"started","call_id":"call-1\nfc_1","tool_call":{"readToolCall":{"args":{"path":"/a.txt"}},"toolCallId":"call-1\nfc_1"}}"#,
    );
    let Some(RuntimeEvent::ToolUse {
        call_id,
        tool,
        input,
    }) = started.first()
    else {
        panic!("应出 ToolUse，实际 {started:?}");
    };
    assert_eq!(call_id, "call-1");
    assert_eq!(tool, "read");
    assert_eq!(input, &serde_json::json!({"path": "/a.txt"}));

    let completed = decoder.push_line(
        r#"{"type":"tool_call","subtype":"completed","call_id":"call-1","tool_call":{"shellToolCall":{"args":{"command":"ls"},"result":{"isBackground":false}}}}"#,
    );
    let Some(RuntimeEvent::ToolResult {
        call_id, output, ..
    }) = completed.first()
    else {
        panic!("应出 ToolResult，实际 {completed:?}");
    };
    assert_eq!(call_id, "call-1");
    assert!(output.contains("isBackground"));
}

#[test]
fn a_progress_subtype_never_closes_a_tool_call() {
    let mut decoder = decoder();
    let events = decoder.push_line(
        r#"{"type":"tool_call","subtype":"progress","call_id":"c1","tool_call":{"shellToolCall":{"args":{}}}}"#,
    );
    assert!(events.is_empty(), "{events:?}");
}

#[test]
fn result_usage_wins_over_step_finish_usage() {
    let mut decoder = decoder();
    decoder.push_line(
        r#"{"type":"step_finish","model":"cursor-model","part":{"tokens":{"input":1,"output":1,"cache":{"read":0}}}}"#,
    );
    decoder.push_line(
        r#"{"type":"result","subtype":"success","session_id":"s1","result":"ok","is_error":false,"inputTokens":10,"outputTokens":5,"cacheReadTokens":0,"cacheWriteTokens":0}"#,
    );
    let summary = decoder.summary();
    assert_eq!(summary.session_id.as_deref(), Some("s1"));
    assert_eq!(summary.output, "ok");
    assert_eq!(summary.usage.len(), 1);
    assert_eq!(summary.usage[0].usage.total_tokens, 15);
    assert!(decoder.finish().is_empty(), "见过 result 就不该再判失败");
}

#[test]
fn step_finish_usage_is_the_fallback_when_result_reports_none() {
    let mut decoder = decoder();
    decoder.push_line(
        r#"{"type":"step_finish","part":{"tokens":{"input":3,"output":4,"cache":{"read":5}}}}"#,
    );
    decoder.push_line(r#"{"type":"result","subtype":"success","result":"ok"}"#);
    // 用量兜底在 `finish()` 里合（`result` 那一行把 usage 视为“未提供”）。
    let events = decoder.finish();
    assert!(matches!(events.first(), Some(RuntimeEvent::Usage { .. })));
    assert_eq!(
        decoder.summary().terminal_error,
        None,
        "见过 result 就不该判失败"
    );
    let summary = decoder.summary();
    assert_eq!(summary.usage.len(), 1);
    assert_eq!(summary.usage[0].usage.total_tokens, 12);
}

#[test]
fn nested_legacy_usage_is_understood() {
    let value = serde_json::json!({
        "input_tokens": 1,
        "outputTokens": 2,
        "cache_read_input_tokens": 3,
        "cacheCreationInputTokens": 4,
    });
    assert_eq!(nested_usage(&value).total_tokens, 10);
}

#[test]
fn a_stream_without_result_is_a_failure() {
    let mut decoder = decoder();
    decoder.push_line(
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"半句话"}]}}"#,
    );
    let events = decoder.finish();
    let Some(RuntimeEvent::Error { message }) = events.first() else {
        panic!("应出 Error，实际 {events:?}");
    };
    assert!(message.contains("stream ended without terminal result"));
    assert_eq!(
        decoder.summary().terminal_error.as_deref(),
        Some(message.as_str())
    );
}

#[test]
fn a_protocol_error_upgrades_only_when_no_result_arrives() {
    let mut decoder = decoder();
    decoder.push_line(r#"{"type":"system","subtype":"error","error":"枚举会话失败"}"#);
    decoder.push_line(r#"{"type":"result","subtype":"success","result":"ok"}"#);
    assert_eq!(decoder.summary().terminal_error, None);
    assert!(decoder.finish().is_empty());
}

#[test]
fn an_error_result_is_terminal() {
    let mut decoder = decoder();
    decoder
        .push_line(r#"{"type":"result","subtype":"error","is_error":true,"error":"额度用完了"}"#);
    assert_eq!(
        decoder.summary().terminal_error.as_deref(),
        Some("额度用完了")
    );
}

#[test]
fn unknown_events_and_junk_lines_are_ignored() {
    let mut decoder = decoder();
    assert!(decoder.push_line("not json at all").is_empty());
    assert!(decoder
        .push_line(r#"{"type":"future_event","payload":{}}"#)
        .is_empty());
    assert!(decoder.push_line("").is_empty());
}

#[test]
fn result_text_is_only_a_fallback_for_the_body() {
    let mut fallback = decoder();
    fallback.push_line(r#"{"type":"result","subtype":"success","result":"兜底正文"}"#);
    assert_eq!(fallback.summary().output, "兜底正文");

    let mut body_wins = decoder();
    body_wins
        .push_line(r#"{"type":"assistant","message":{"content":[{"type":"text","text":"正文"}]}}"#);
    body_wins.push_line(r#"{"type":"result","subtype":"success","result":"另一份"}"#);
    assert_eq!(body_wins.summary().output, "正文");
}
