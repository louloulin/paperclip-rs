use pc_adapter_opencode_local::{
    is_opencode_unknown_session_error, parse_opencode_stream_json,
};

#[test]
fn 解析_text_step_finish和error() {
    let parsed = parse_opencode_stream_json(
        r#"{"type":"text","sessionID":"session_123","part":{"text":"Hello from OpenCode"}}
{"type":"step_finish","sessionID":"session_123","part":{"reason":"done","cost":0.0025,"tokens":{"input":120,"output":40,"reasoning":10,"cache":{"read":20,"write":0}}}}
{"type":"error","sessionID":"session_123","error":{"message":"model unavailable"}}"#,
    );
    assert_eq!(parsed.session_id.as_deref(), Some("session_123"));
    assert_eq!(parsed.summary, "Hello from OpenCode");
    assert_eq!(parsed.usage.input_tokens, 120);
    assert_eq!(parsed.usage.output_tokens, 50);
    assert_eq!(parsed.usage.cached_input_tokens, Some(20));
    assert_eq!(parsed.cost_usd, Some(0.0025));
    assert_eq!(parsed.error_message.as_deref(), Some("model unavailable"));
}

#[test]
fn 工具错误与主错误分离() {
    let parsed = parse_opencode_stream_json(
        r#"{"type":"tool_use","sessionID":"s","part":{"state":{"status":"error","error":"File not found: e2b-adapter-result.txt"}}}
{"type":"text","sessionID":"s","part":{"text":"Recovered and completed the task"}}"#,
    );
    assert_eq!(parsed.summary, "Recovered and completed the task");
    assert!(parsed.error_message.is_none());
    assert_eq!(parsed.tool_errors, vec!["File not found: e2b-adapter-result.txt"]);
}

#[test]
fn 嵌套data_message可作为错误文本() {
    let parsed = parse_opencode_stream_json(r#"{"type":"error","error":{"data":{"message":"nested failure"}}}"#);
    assert_eq!(parsed.error_message.as_deref(), Some("nested failure"));
}

#[test]
fn 未知session识别() {
    assert!(is_opencode_unknown_session_error("Session not found: s_123", ""));
    assert!(is_opencode_unknown_session_error("", "unknown session id"));
    assert!(!is_opencode_unknown_session_error("all good", ""));
}

#[test]
fn 非json行被安全忽略() {
    let parsed = parse_opencode_stream_json("not-json\n{\"type\":\"text\",\"sessionID\":\"s\",\"part\":{\"text\":\"ok\"}}");
    assert_eq!(parsed.summary, "ok");
}
