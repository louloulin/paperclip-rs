//! Round 405 - integration tests for `pc_acpx::server_utils` (Part 1).
//!
//! Validates the cross-module composition of the helpers ported in R405:
//!   - env-key classifiers + paperclip runtime namespace interplay
//!   - parseObject / asString / etc. round-trip through serde_json::Value
//!   - appendWithCap + appendWithByteCap semantics on real UTF-8 inputs
//!   - renderTemplate + resolvePathValue compose for nested configs
//!   - joinPromptSections tolerates Option<&str> + non-string items
//!   - signalDecision drives the canonical (pgid / direct / none) matrix
//!   - TerminalResultCleanupEvidence carries the canonical wire shape

use serde_json::json;

use pc_acpx::server_utils::{
    append_with_byte_cap, append_with_cap, as_boolean, as_number, as_string, as_string_array,
    is_forbidden_config_env_key, is_paperclip_runtime_env_key, is_sensitive_env_key,
    is_valid_path_segment, join_prompt_sections, parse_json, parse_object, render_template,
    resolve_path_value, signal_decision, TerminalResultCleanupEvidence, REDACTED_LOG_VALUE,
    MAX_CAPTURE_BYTES, MAX_EXCERPT_BYTES, PATH_SEGMENT_RE_SRC, SENSITIVE_ENV_KEY_RE_SRC,
    UNMANAGED_BACKGROUND_TASK_LIVENESS_REASON, UNMANAGED_BACKGROUND_TASK_STOP_REASON,
};
use pc_acpx::server_utils::{RunningProcessSignalInfo, SignalTarget};

// ===========================================================================
// Env key classification interplay (paperclip runtime vs forbidden config).
// ===========================================================================

#[test]
fn env_key_classifiers_partition_paperclip_namespace() {
    // PAPERCLIP_API_KEY is both a runtime key AND forbidden from config.
    assert!(is_paperclip_runtime_env_key("PAPERCLIP_API_KEY"));
    assert!(is_forbidden_config_env_key("PAPERCLIP_API_KEY"));
    // Other PAPERCLIP_* keys are runtime-only (forbidden list is narrow).
    assert!(is_paperclip_runtime_env_key("PAPERCLIP_AGENT_ID"));
    assert!(!is_forbidden_config_env_key("PAPERCLIP_AGENT_ID"));
    // Non-paperclip keys fall outside both classifiers.
    assert!(!is_paperclip_runtime_env_key("PATH"));
    assert!(!is_forbidden_config_env_key("PATH"));
}

#[test]
fn sensitive_env_key_detects_full_keyword_set() {
    for k in [
        "API_KEY",
        "GH_TOKEN",
        "DB_PASSWORD",
        "DB_PASSWD",
        "AUTHORIZATION",
        "USER_COOKIE",
    ] {
        assert!(
            is_sensitive_env_key(k),
            "expected sensitive match for {k}"
        );
    }
    assert!(!is_sensitive_env_key("PAPERCLIP_AGENT_ID"));
    assert!(!is_sensitive_env_key("HOME"));
}

#[test]
fn path_segment_validator_rejects_special_chars() {
    assert!(is_valid_path_segment("default"));
    assert!(is_valid_path_segment("acpx-prod"));
    assert!(is_valid_path_segment("v1_2_3"));
    // Must NOT contain dots, slashes, or whitespace.
    assert!(!is_valid_path_segment("default.local"));
    assert!(!is_valid_path_segment("../etc"));
    assert!(!is_valid_path_segment("with space"));
    // Empty is invalid.
    assert!(!is_valid_path_segment(""));
}

// ===========================================================================
// JSON value coercion round-trips.
// ===========================================================================

#[test]
fn json_coercers_handle_all_value_kinds() {
    let obj = json!({
        "name": "alice",
        "score": 42,
        "active": true,
        "tags": ["a", "b", 1, "c"],
        "missing": null,
        "nested": { "deep": { "value": "ok" } },
    });
    // parseObject → only top-level object survives.
    assert_eq!(parse_object(&obj).len(), 6);
    assert!(parse_object(&json!("x")).is_empty());
    assert!(parse_object(&json!(null)).is_empty());
    assert!(parse_object(&json!([])).is_empty());

    // asString: non-empty string only.
    assert_eq!(as_string(&obj["name"], "fallback"), "alice");
    assert_eq!(as_string(&obj["missing"], "fallback"), "fallback");
    assert_eq!(as_string(&obj["score"], "fallback"), "fallback");

    // asNumber: finite number only.
    assert_eq!(as_number(&obj["score"], -1.0), 42.0);
    assert_eq!(as_number(&obj["missing"], -1.0), -1.0);
    assert_eq!(as_number(&obj["name"], -1.0), -1.0);

    // asBoolean: bool only.
    assert!(as_boolean(&obj["active"], false));
    // null → fallback (Node: typeof !== "boolean").
    assert!(as_boolean(&obj["missing"], true)); // null → fallback true
    // string → fallback (false here, since fallback arg is false).
    assert!(!as_boolean(&obj["name"], false)); // string → fallback false
    // number → fallback.
    assert!(!as_boolean(&obj["score"], false)); // number → fallback false

    // asStringArray: only string elements survive.
    assert_eq!(
        as_string_array(&obj["tags"]),
        vec!["a", "b", "c"],
        "non-string elements must be filtered"
    );
    assert_eq!(as_string_array(&obj["missing"]), Vec::<String>::new());
}

#[test]
fn parse_json_handles_object_and_rejects_invalid() {
    let v = parse_json(r#"{"agent":"alpha","n":3}"#).expect("valid");
    assert_eq!(v["agent"], json!("alpha"));
    assert_eq!(v["n"], json!(3));
    // Whitespace-only is invalid.
    assert!(parse_json("   ").is_none());
    // Truncated object is invalid.
    assert!(parse_json("{").is_none());
}

// ===========================================================================
// Bounded string accumulators.
// ===========================================================================

#[test]
fn append_with_cap_counts_chars_not_bytes() {
    // 'h' 'é' 'l' 'l' 'o' = 5 chars but 6 bytes (é = 2 bytes).
    let combined = append_with_cap("héllo", "", 5);
    assert_eq!(combined, "héllo");
    let truncated = append_with_cap("héllo", "", 3);
    assert_eq!(truncated, "llo");
}

#[test]
fn append_with_byte_cap_never_splits_a_utf8_codepoint() {
    // The trailing window must always start at a char boundary, even if
    // the requested byte cap falls mid-codepoint.
    let s = "héllo"; // 6 bytes total
    assert_eq!(append_with_byte_cap("", s, 6), "héllo");
    // cap=5 → trailing window is bytes 1..6 = "éllo". 0xC3 (start of é)
    // is NOT a continuation byte (0x80), so the skip loop does not
    // advance past it.
    assert_eq!(append_with_byte_cap("", s, 5), "éllo");
    // cap=4 → trailing window starts at byte 2. 0xA9 IS a continuation
    // byte, so we skip to byte 3 → "llo".
    assert_eq!(append_with_byte_cap("", s, 4), "llo");
    // cap=3 → window starts at byte 3 → "llo".
    assert_eq!(append_with_byte_cap("", s, 3), "llo");
    // cap=1 → trailing 1 byte is 0x6F ('o'), which is a leading
    // byte (ASCII), not a continuation byte, so no skipping → "o".
    assert_eq!(append_with_byte_cap("", s, 1), "o");
    // cap=2 → trailing 2 bytes are 0x6C ('l') + 0x6F ('o'), both
    // ASCII leading bytes → "lo".
    assert_eq!(append_with_byte_cap("", s, 2), "lo");
}

#[test]
fn append_with_cap_and_byte_cap_default_to_max_capture() {
    // No explicit cap → uses the MAX_CAPTURE_BYTES default.
    let big = "x".repeat(MAX_CAPTURE_BYTES + 100);
    let out = append_with_cap(&big, "", MAX_CAPTURE_BYTES);
    assert_eq!(out.len(), MAX_CAPTURE_BYTES);
    let out = append_with_byte_cap(&big, "", MAX_CAPTURE_BYTES);
    assert_eq!(out.len(), MAX_CAPTURE_BYTES);
}

// ===========================================================================
// Template / path resolution.
// ===========================================================================

#[test]
fn render_template_walks_nested_dotted_paths() {
    let cfg = json!({
        "agent": { "id": "abc", "version": 2 },
        "host": "node-01",
        "flags": { "verbose": true },
    });
    let tpl = "agent={{agent.id}} v{{agent.version}} on {{host}} verbose={{flags.verbose}}";
    assert_eq!(
        render_template(tpl, &cfg),
        "agent=abc v2 on node-01 verbose=true"
    );
}

#[test]
fn resolve_path_value_stringifies_complex_leaves() {
    let cfg = json!({ "obj": { "x": 1, "y": [1, 2] } });
    // Object leaf → JSON.stringify.
    assert_eq!(resolve_path_value(&cfg, "obj"), r#"{"x":1,"y":[1,2]}"#);
    // Number leaf → number string.
    assert_eq!(resolve_path_value(&cfg, "obj.x"), "1");
    // Bool leaf → "true" / "false".
    let cfg2 = json!({ "b": false });
    assert_eq!(resolve_path_value(&cfg2, "b"), "false");
    // Missing path → "".
    assert_eq!(resolve_path_value(&cfg, "missing"), "");
}

// ===========================================================================
// joinPromptSections.
// ===========================================================================

#[test]
fn join_prompt_sections_handles_optionals_and_separator() {
    let sections: Vec<Option<&str>> = vec![
        Some("  first  "),
        None,
        Some(""),
        Some("second"),
        Some("   "),
        Some("third"),
    ];
    assert_eq!(
        join_prompt_sections(sections, " | "),
        "first | second | third"
    );
}

#[test]
fn join_prompt_sections_default_separator_is_blank_line() {
    // Default = "\n\n" (matches Node `joinPromptSections(sections)`).
    let sections: Vec<Option<&str>> = vec![Some("a"), Some("b")];
    assert_eq!(join_prompt_sections(sections, "\n\n"), "a\n\nb");
}

// ===========================================================================
// signalDecision matrix.
// ===========================================================================

#[test]
fn signal_decision_canonical_matrix() {
    // Exited + POSIX → None.
    let exited = RunningProcessSignalInfo {
        process_group_id: Some(123),
        already_exited: true,
    };
    assert_eq!(signal_decision(exited, false), SignalTarget::None);
    // POSIX + valid pgid + alive → ProcessGroup.
    let alive_posix = RunningProcessSignalInfo {
        process_group_id: Some(999),
        already_exited: false,
    };
    assert_eq!(
        signal_decision(alive_posix, false),
        SignalTarget::ProcessGroup { pgid: 999 }
    );
    // Windows + alive → DirectChild (no group signaling on Windows).
    let alive_win = RunningProcessSignalInfo {
        process_group_id: Some(999),
        already_exited: false,
    };
    assert_eq!(signal_decision(alive_win, true), SignalTarget::DirectChild);
    // POSIX + missing pgid → DirectChild.
    let no_pgid = RunningProcessSignalInfo {
        process_group_id: None,
        already_exited: false,
    };
    assert_eq!(signal_decision(no_pgid, false), SignalTarget::DirectChild);
    // POSIX + pgid=0 → DirectChild.
    let zero_pgid = RunningProcessSignalInfo {
        process_group_id: Some(0),
        already_exited: false,
    };
    assert_eq!(
        signal_decision(zero_pgid, false),
        SignalTarget::DirectChild
    );
    // POSIX + negative pgid → DirectChild (pgid must be > 0).
    let neg_pgid = RunningProcessSignalInfo {
        process_group_id: Some(-1),
        already_exited: false,
    };
    assert_eq!(signal_decision(neg_pgid, false), SignalTarget::DirectChild);
}

// ===========================================================================
// TerminalResultCleanupEvidence wire shape.
// ===========================================================================

#[test]
fn cleanup_evidence_wire_shape_matches_node() {
    let ev = TerminalResultCleanupEvidence::new(true, Some("SIGTERM".to_string()), true);
    // Serialize as JSON to confirm wire shape (camelCase, stop_reason
    // uses the canonical strings, signal is preserved).
    let j = serde_json::to_value(&ev).expect("serialize");
    assert_eq!(j["kind"], "terminal_result_cleanup");
    assert_eq!(j["stopped"], true);
    assert_eq!(j["stopReason"], UNMANAGED_BACKGROUND_TASK_STOP_REASON);
    assert_eq!(j["reason"], UNMANAGED_BACKGROUND_TASK_LIVENESS_REASON);
    assert_eq!(j["terminalResultSeen"], true);
    assert_eq!(j["signal"], "SIGTERM");
    assert_eq!(j["forceKilled"], true);
}

// ===========================================================================
// Constants public surface.
// ===========================================================================

#[test]
fn public_constants_match_node_literals() {
    assert_eq!(REDACTED_LOG_VALUE, "***REDACTED***");
    assert_eq!(MAX_EXCERPT_BYTES, 32 * 1024);
    assert_eq!(MAX_CAPTURE_BYTES, 4 * 1024 * 1024);
    assert_eq!(PATH_SEGMENT_RE_SRC, r"^[a-zA-Z0-9_-]+$");
    assert_eq!(
        SENSITIVE_ENV_KEY_RE_SRC,
        r"(?i)(key|token|secret|password|passwd|authorization|cookie)"
    );
}
