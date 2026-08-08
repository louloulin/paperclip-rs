use pc_adapter_gemini_local::{
    detect_gemini_auth_required, detect_gemini_quota_exhausted,
    describe_gemini_failure, is_gemini_session_unrecoverable_error,
    is_gemini_transient_network_error, is_gemini_turn_limit_result,
    parse_gemini_stream_json,
};

#[test]
fn message和legacy_assistant事件聚合并保留session() {
    let stdout = [
        r#"{"type":"message","role":"user","content":"ignore"}"#,
        r#"{"type":"message","role":"assistant","content":"first"}"#,
        r#"{"type":"assistant","session_id":"legacy","message":{"content":[{"type":"output_text","text":"second"}]}}"#,
        r#"{"type":"result","sessionId":"final","result":"ignored because messages exist"}"#,
    ].join("\n");
    let parsed = parse_gemini_stream_json(&stdout);
    assert_eq!(parsed.summary, "first\n\nsecond");
    assert_eq!(parsed.session_id.as_deref(), Some("final"));
}

#[test]
fn 多种usage字段累加并保留cost() {
    let stdout = [
        r#"{"type":"step_finish","usage":{"promptTokenCount":10,"cachedContentTokenCount":2,"candidatesTokenCount":4}}"#,
        r#"{"type":"result","usageMetadata":{"input_tokens":5,"cached_input_tokens":1,"output_tokens":3},"cost_usd":1.25,"result":"done"}"#,
    ].join("\n");
    let parsed = parse_gemini_stream_json(&stdout);
    assert_eq!(parsed.usage.input_tokens, 15);
    assert_eq!(parsed.usage.cached_input_tokens, Some(3));
    assert_eq!(parsed.usage.output_tokens, 7);
    assert_eq!(parsed.cost_usd, Some(1.25));
}

#[test]
fn question结构保留选项描述() {
    let parsed = parse_gemini_stream_json(
        r#"{"type":"assistant","message":{"content":[{"type":"question","prompt":"选择","choices":[{"key":"a","label":"A","description":"方案A"},{"key":"b","label":"B"}]}]}}"#,
    );
    let question = parsed.question.unwrap();
    assert_eq!(question.prompt, "选择");
    assert_eq!(question.choices[0].description.as_deref(), Some("方案A"));
    assert_eq!(question.choices[1].description, None);
}

#[test]
fn 错误事件和失败描述() {
    let parsed = parse_gemini_stream_json(
        r#"{"type":"result","status":"error","error":{"message":"quota exceeded"}}"#,
    );
    assert_eq!(parsed.error_message.as_deref(), Some("quota exceeded"));
    assert_eq!(describe_gemini_failure(&serde_json::json!({"status":"error","error":"boom"})).as_deref(), Some("Gemini run failed: status=error: boom"));
}

#[test]
fn auth和quota分类互不混淆() {
    assert!(detect_gemini_auth_required(None, "", "Manual authorization is required but session is non-interactive"));
    assert!(!detect_gemini_auth_required(None, "authenticated", "normal output"));
    assert!(detect_gemini_quota_exhausted(None, "", "RESOURCE_EXHAUSTED: quota"));
}

#[test]
fn session不可恢复和网络瞬态分类() {
    assert!(is_gemini_session_unrecoverable_error("", "input token count exceeds maximum"));
    assert!(!is_gemini_session_unrecoverable_error("", "Some other error"));
    assert!(is_gemini_transient_network_error("", "getaddrinfo ENOTFOUND oauth2.googleapis.com"));
    assert!(!is_gemini_transient_network_error("", "unknown session abc"));
}

#[test]
fn turn_limit支持退出码和结构化原因() {
    assert!(is_gemini_turn_limit_result(None, Some(53)));
    assert!(is_gemini_turn_limit_result(Some(&serde_json::json!({"stopReason":"max_turns_exhausted"})), Some(0)));
    assert!(!is_gemini_turn_limit_result(Some(&serde_json::json!({"status":"success"})), Some(0)));
}
