use pc_adapter_claude_local::{
    claude_model_usage_totals, detect_claude_login_required, extract_claude_login_url,
    is_claude_image_processing_error, is_claude_unknown_session_error, parse_claude_stream_json,
};

#[test]
fn system_assistant_result完整映射_node核心字段() {
    let stdout = [
        r#"{"type":"system","subtype":"init","session_id":"s-init","model":"claude-opus"}"#,
        r#"{"type":"assistant","session_id":"s-init","message":{"content":[{"type":"text","text":"阶段一"},{"type":"tool_use","name":"bash"},{"type":"text","text":"阶段二"}]}}"#,
        r#"{"type":"result","session_id":"s-final","model":"claude-opus","result":"最终答案","stop_reason":"end_turn","total_cost_usd":2.5,"usage":{"input_tokens":10,"output_tokens":20,"cache_read_input_tokens":3}}"#,
    ].join("\n");
    let parsed = parse_claude_stream_json(&stdout);
    assert_eq!(parsed.session_id.as_deref(), Some("s-final"));
    assert_eq!(parsed.model.as_deref(), Some("claude-opus"));
    assert_eq!(parsed.summary, "最终答案");
    assert_eq!(parsed.stop_reason.as_deref(), Some("end_turn"));
    assert_eq!(parsed.cost_usd, Some(2.5));
    assert_eq!(parsed.usage.unwrap().input_tokens, 10);
}

#[test]
fn 缺少result时使用assistant文本并保持无usage() {
    let parsed = parse_claude_stream_json(
        r#"{"type":"system","subtype":"init","session_id":"s1","model":"haiku"}
{"type":"assistant","message":{"content":[{"type":"text","text":"hello"}]}}
{"type":"assistant","message":{"content":[{"type":"text","text":"world"}]}}"#,
    );
    assert_eq!(parsed.summary, "hello\n\nworld");
    assert!(parsed.usage.is_none());
    assert!(parsed.result_json.is_none());
}

#[test]
fn model_usage跨模型汇总并把cache_creation计入输入() {
    let value = serde_json::json!({
        "opus": {"inputTokens": 100, "cacheCreationInputTokens": 20, "cacheReadInputTokens": 30, "outputTokens": 40},
        "haiku": {"inputTokens": 5, "cacheCreationInputTokens": 2, "cacheReadInputTokens": 3, "outputTokens": 7}
    });
    let usage = claude_model_usage_totals(Some(&value)).unwrap();
    assert_eq!(usage.input_tokens, 127);
    assert_eq!(usage.output_tokens, 47);
    assert_eq!(usage.cached_input_tokens, Some(33));
    assert!(claude_model_usage_totals(Some(&serde_json::json!({}))).is_none());
}

#[test]
fn 登录检测和登录链接提取() {
    assert!(detect_claude_login_required(
        None,
        "",
        "Please run claude login"
    ));
    assert!(!detect_claude_login_required(
        None,
        "authenticated",
        "network timeout"
    ));
    assert_eq!(
        extract_claude_login_url("Open https://console.anthropic.com/login for auth."),
        Some("https://console.anthropic.com/login".into())
    );
}

#[test]
fn 未知session与图片处理错误识别() {
    assert!(is_claude_unknown_session_error(&serde_json::json!({
        "errors": [{"message": "--resume requires a valid session ID"}]
    })));
    assert!(!is_claude_unknown_session_error(
        &serde_json::json!({"result": "Network timeout"})
    ));
    assert!(is_claude_image_processing_error(&serde_json::json!({
        "errors": [{"message": "400 Could not process image"}]
    })));
}

#[test]
fn 非法和未知事件被安全忽略() {
    let parsed = parse_claude_stream_json("not-json\n{\"type\":\"metadata\"}\n");
    assert_eq!(parsed.summary, "");
    assert!(parsed.session_id.is_none());
}
