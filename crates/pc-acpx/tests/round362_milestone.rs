//! R362 集成测试 — `pc-acpx` 纯函数模块组合端到端验证。
//!
//! 用例覆盖了跨模块协作：transcript line → usage 汇总 → session codec
//! 序列化 → 哈希稳定性，确保各 pure 函数彼此组合时输出仍稳定。

use pc_acpx::{
    acpx_agent_id_for_adapter_type, gemini_version_supports_native_acp_flag,
    parse_acpx_stdout_line, parse_gemini_version_parts, rewrite_gemini_acp_flag_for_version,
    session_codec_deserialize, session_codec_get_display_id, session_codec_serialize, short_hash,
    stable_json, summarize_from_value, summarize_tool_call, AcpxSessionParams, TranscriptEntry,
};
use serde_json::json;

#[test]
fn constants_acpx_adapter_mapping_resolves_each_known_type() {
    assert_eq!(
        acpx_agent_id_for_adapter_type(Some("claude_local")),
        Some("claude")
    );
    assert_eq!(
        acpx_agent_id_for_adapter_type(Some("codex_local")),
        Some("codex")
    );
    assert_eq!(
        acpx_agent_id_for_adapter_type(Some("gemini_local")),
        Some("gemini")
    );
    assert_eq!(
        acpx_agent_id_for_adapter_type(Some("custom_acp")),
        Some("custom")
    );
    assert_eq!(acpx_agent_id_for_adapter_type(None), None);
    assert_eq!(acpx_agent_id_for_adapter_type(Some("unknown")), None);
}

#[test]
fn gemini_version_pipeline_rewrites_flag_for_legacy_but_not_current() {
    let parsed = parse_gemini_version_parts(Some("gemini-cli v0.30.0\n")).unwrap();
    assert_eq!(parsed, [0, 30, 0]);
    assert!(!gemini_version_supports_native_acp_flag(Some(parsed)));
    let rewritten = rewrite_gemini_acp_flag_for_version("gemini --acp", Some(parsed));
    assert_eq!(rewritten, "gemini --experimental-acp");

    let parsed = parse_gemini_version_parts(Some("0.34.0")).unwrap();
    assert!(gemini_version_supports_native_acp_flag(Some(parsed)));
    let rewritten = rewrite_gemini_acp_flag_for_version("gemini --acp", Some(parsed));
    assert_eq!(rewritten, "gemini --acp");
}

#[test]
fn session_codec_normalizes_minimum_required_payload() {
    let raw = json!({
        "acpSessionId": "acp-1",
        "agent": "claude",
        "cwd": "/repo",
        "mode": "persistent"
    });
    let params = session_codec_deserialize(&raw).expect("params");
    assert_eq!(params.acp_session_id.as_deref(), Some("acp-1"));
    assert_eq!(params.runtime_session_name, None);
    assert_eq!(
        session_codec_get_display_id(Some(&params)).as_deref(),
        Some("acp-1")
    );

    let serialized = session_codec_serialize(Some(&params)).expect("serialized");
    assert_eq!(serialized, raw);
}

#[test]
fn session_codec_serialize_handles_typed_params() {
    let params = AcpxSessionParams {
        runtime_session_name: Some("runtime-1".into()),
        agent_session_id: Some("agent-1".into()),
        ..Default::default()
    };
    let value = session_codec_serialize(Some(&params)).expect("value");
    assert_eq!(
        value,
        json!({
            "runtimeSessionName": "runtime-1",
            "agentSessionId": "agent-1"
        })
    );
}

#[test]
fn stable_json_and_short_hash_round_trip_with_key_order_variations() {
    let a = json!({ "x": 1, "y": [2, 3, { "nested": true }], "z": "abc" });
    let b = json!({ "z": "abc", "y": [2, 3, { "nested": true }], "x": 1 });
    assert_eq!(stable_json(&a), stable_json(&b));
    assert_eq!(short_hash(&a), short_hash(&b));
}

#[test]
fn transcript_pipeline_emits_full_event_sequence() {
    let events = [
        json!({
            "type": "acpx.session",
            "agent": "claude",
            "mode": "persistent",
            "permissionMode": "approve-all",
            "acpSessionId": "acp-1"
        })
        .to_string(),
        json!({
            "type": "acpx.text_delta",
            "channel": "assistant",
            "text": "hello"
        })
        .to_string(),
        json!({
            "type": "acpx.tool_call",
            "name": "exec",
            "toolCallId": "tc-1",
            "status": "completed",
            "input": { "command": "ls" }
        })
        .to_string(),
        json!({
            "type": "acpx.result",
            "summary": "done",
            "inputTokens": 10,
            "outputTokens": 20,
            "costUsd": 0.25,
            "subtype": "success",
            "isError": false
        })
        .to_string(),
    ];

    let mut entries = Vec::new();
    for line in &events {
        entries.extend(parse_acpx_stdout_line(line, "ts"));
    }

    assert_eq!(entries.len(), 5);
    assert!(matches!(entries[0], TranscriptEntry::Init { .. }));
    assert!(matches!(entries[1], TranscriptEntry::Assistant { .. }));
    assert!(matches!(entries[2], TranscriptEntry::ToolCall { .. }));
    assert!(matches!(entries[3], TranscriptEntry::ToolResult { .. }));
    assert!(matches!(entries[4], TranscriptEntry::Result { .. }));

    // The tool_call summary should pick up the command as the title.
    let summary = summarize_tool_call(&entries[2]).expect("summary");
    assert_eq!(summary.name, "exec");
    assert_eq!(
        summary.detail.get("command").map(String::as_str),
        Some("ls")
    );
}

#[test]
fn summarize_from_value_pipeline_consumes_wire_payload() {
    let pre = json!({
        "usage": {
            "cumulative": { "inputTokens": 10, "outputTokens": 500, "cachedReadTokens": 30 },
            "cost": { "amount": 0.5, "currency": "USD" }
        }
    });
    let post = json!({
        "usage": {
            "cumulative": { "inputTokens": 10, "outputTokens": 500, "cachedReadTokens": 30 },
            "cost": { "amount": 0.5, "currency": "USD" }
        }
    });
    let event = json!({
        "inputTokens": 25,
        "outputTokens": 75,
        "cachedReadTokens": 5
    });

    let out = summarize_from_value(Some(&pre), Some(&post), Some(&event), None);
    let usage = out.usage.expect("usage");
    assert_eq!(usage.input_tokens, 25);
    assert_eq!(usage.output_tokens, 75);
    assert_eq!(usage.cached_input_tokens, 5);
}

#[test]
fn tokenizer_round_trip_for_legacy_gemini_invocation() {
    let parsed = parse_gemini_version_parts(Some("0.30.0")).unwrap();
    let rewritten =
        rewrite_gemini_acp_flag_for_version("/opt/bin/gemini --acp --model x", Some(parsed));
    assert_eq!(rewritten, "/opt/bin/gemini --experimental-acp --model x");
}
