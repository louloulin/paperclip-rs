use pc_adapter_cursor_local::{
    is_cursor_unknown_session_error, normalize_cursor_stream_line, parse_cursor_stream_json,
};

#[test]
fn stream_prefix会去掉stdout标签并解析json() {
    assert_eq!(
        normalize_cursor_stream_line("stdout: {\"x\":1}"),
        "{\"x\":1}"
    );
    assert_eq!(
        normalize_cursor_stream_line("stderr {\"y\":2}"),
        "{\"y\":2}"
    );
    assert_eq!(normalize_cursor_stream_line("noise"), "noise");
}

#[test]
fn result会覆盖最后的assistant摘要文本() {
    let parsed = parse_cursor_stream_json(
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi"}]}}
{"type":"result","is_error":false,"result":"final","session_id":"sess-1","usage":{"input_tokens":3,"output_tokens":2,"cache_read_input_tokens":1}}"#,
    );
    assert_eq!(parsed.summary, "final");
    assert_eq!(parsed.session_id.as_deref(), Some("sess-1"));
    assert_eq!(parsed.usage.input_tokens, 3);
    assert_eq!(parsed.usage.cached_input_tokens, Some(1));
}

#[test]
fn step_finish累加usage和cost() {
    let parsed = parse_cursor_stream_json(
        r#"{"type":"step_finish","part":{"tokens":{"input":1,"output":2,"cache":{"read":3}},"cost":0.5}}"#,
    );
    assert_eq!(parsed.usage.input_tokens, 1);
    assert_eq!(parsed.usage.output_tokens, 2);
    assert_eq!(parsed.usage.cached_input_tokens, Some(3));
    assert_eq!(parsed.cost_usd, Some(0.5));
}

#[test]
fn 错误事件支持message和嵌套error() {
    let parsed = parse_cursor_stream_json(
        r#"{"type":"result","is_error":true,"error":{"message":"rate limit"}}"#,
    );
    assert_eq!(parsed.error_message.as_deref(), Some("rate limit"));
}

#[test]
fn session不可恢复错误识别() {
    assert!(is_cursor_unknown_session_error(
        "",
        "Error: unknown session abc"
    ));
    assert!(is_cursor_unknown_session_error(
        "",
        "Resume chat abc-123 not found on disk"
    ));
    assert!(!is_cursor_unknown_session_error("", "Network timeout"));
}

#[test]
fn 非法行不会中断解析() {
    let parsed = parse_cursor_stream_json("not-json\n{\"type\":\"system\",\"session_id\":\"x\"}\n");
    assert_eq!(parsed.session_id.as_deref(), Some("x"));
}
