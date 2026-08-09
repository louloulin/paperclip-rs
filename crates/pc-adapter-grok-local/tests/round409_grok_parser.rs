use pc_adapter_grok_local::{is_grok_unknown_session_error, parse_grok_jsonl, parse_grok_output};

#[test]
fn 流式文本和推理按事件顺序聚合() {
    let output = [
        r#"{"type":"thought","data":"Plan"}"#,
        r#"{"type":"thought","data":" first."}"#,
        r#"{"type":"text","data":"hel"}"#,
        r#"{"type":"text","data":"lo"}"#,
        r#"{"type":"end","sessionId":"sess-1","stopReason":"EndTurn","requestId":"req-1"}"#,
    ]
    .join("\n");
    let parsed = parse_grok_jsonl(&output);
    assert_eq!(parsed.thought, "Plan first.");
    assert_eq!(parsed.summary, "hello");
    assert_eq!(parsed.session_id.as_deref(), Some("sess-1"));
    assert_eq!(parsed.stop_reason.as_deref(), Some("EndTurn"));
    assert_eq!(parsed.request_id.as_deref(), Some("req-1"));
}

#[test]
fn 多个结束事件使用最后一个非空元数据() {
    let parsed = parse_grok_jsonl(
        r#"{"type":"end","sessionId":"s1","stopReason":"partial"}
{"type":"end","sessionId":"s2","stopReason":"done","requestId":"r2"}"#,
    );
    assert_eq!(parsed.session_id.as_deref(), Some("s2"));
    assert_eq!(parsed.stop_reason.as_deref(), Some("done"));
    assert_eq!(parsed.request_id.as_deref(), Some("r2"));
}

#[test]
fn 错误事件支持字符串和对象() {
    let parsed = parse_grok_jsonl(
        r#"{"type":"error","error":"first"}
{"type":"error","error":{"detail":"second"}}"#,
    );
    assert_eq!(parsed.error_message.as_deref(), Some("second"));
}

#[test]
fn 无关事件和空行不影响协议结果() {
    let parsed = parse_grok_jsonl(
        "\n{\"type\":\"metadata\",\"data\":123}\nnot-json\n{\"type\":\"text\",\"data\":\"ok\"}",
    );
    assert_eq!(parsed.summary, "ok");
    assert_eq!(parsed.session_id, None);
}

#[test]
fn 回合边界只作用于thought不改变用户可见文本() {
    let parsed = parse_grok_jsonl(
        r#"{"type":"thought","data":"Done."}
{"type":"thought","data":"Next"}
{"type":"text","data":"Done."}
{"type":"text","data":"Next"}"#,
    );
    assert_eq!(parsed.thought, "Done.\nNext");
    assert_eq!(parsed.summary, "Done.Next");
}

#[test]
fn 未知会话检测只匹配明确的恢复错误() {
    assert!(is_grok_unknown_session_error(
        "",
        "resume session not found"
    ));
    assert!(is_grok_unknown_session_error("invalid session", ""));
    assert!(!is_grok_unknown_session_error(
        "session started",
        "not found in output file"
    ));
}

#[test]
fn legacy输出仍保持兼容而新协议走结构化解析() {
    assert_eq!(
        parse_grok_output("plain final\n").as_deref(),
        Some("plain final")
    );
    assert_eq!(
        parse_grok_output(r#"{"type":"text","data":"structured"}"#).as_deref(),
        Some("structured")
    );
}
