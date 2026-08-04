//! heartbeat run summary：纯逻辑模块，对齐 Node `heartbeat-run-summary.ts`。
//!
//! 包含：
//! - 常量：`HEARTBEAT_RUN_RESULT_SUMMARY_MAX_CHARS` / `HEARTBEAT_RUN_RESULT_OUTPUT_MAX_CHARS` /
//!   `HEARTBEAT_RUN_SAFE_RESULT_JSON_MAX_BYTES`
//! - 纯函数：`merge_heartbeat_run_result_json` /
//!   `summarize_heartbeat_run_result_json` / `build_heartbeat_run_issue_comment`
//!
//! 设计：
//! - 与 `stop_metadata` 一样不依赖 DB / actor，方便单测
//! - 复用 serde_json::Value / Map 作为输入输出，调用方负责序列化

/// 心跳 run result JSON 中 `summary` / `result` / `message` / `error` 文本字段最大长度。
pub const HEARTBEAT_RUN_RESULT_SUMMARY_MAX_CHARS: usize = 500;

/// 心跳 run result JSON 中 `output` 文本字段最大长度。
pub const HEARTBEAT_RUN_RESULT_OUTPUT_MAX_CHARS: usize = 4_096;

/// 心跳 run result JSON 安全持久化上限（字节）。
pub const HEARTBEAT_RUN_SAFE_RESULT_JSON_MAX_BYTES: usize = 64 * 1024;

// ============================================================================
// Helpers
// ============================================================================

fn truncate_summary_text(value: Option<&serde_json::Value>, max_length: usize) -> Option<String> {
    let s = value?.as_str()?;
    if s.len() > max_length {
        Some(s[..max_length].to_string())
    } else {
        Some(s.to_string())
    }
}

fn read_numeric_field(obj: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<serde_json::Value> {
    obj.get(key).cloned()
}

fn read_comment_text(value: Option<&serde_json::Value>) -> Option<String> {
    let s = value?.as_str()?;
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn is_valid_base_result(value: Option<&serde_json::Value>) -> bool {
    match value {
        Some(serde_json::Value::Object(_)) => true,
        _ => false,
    }
}

// ============================================================================
// Public API（对齐 Node）
// ============================================================================

/// 把 `summary` 文本合并到已有的 `resultJson`：
/// - resultJson 为 null / 非对象 → 返回 `{ summary }` 或 `null`
/// - resultJson 已有 `summary`（trim 非空）→ 保留原值
/// - resultJson 已有 `summary` 但为空 → 用新 summary 覆盖
/// - resultJson 无 `summary` → spread + summary
pub fn merge_heartbeat_run_result_json(
    result_json: Option<&serde_json::Value>,
    summary: Option<&str>,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    // normalize summary：trim 后为空 → None，否则保留 trimmed String
    let normalized_summary: Option<String> = summary.and_then(|s| {
        let t = s.trim();
        if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        }
    });

    let base_result = if is_valid_base_result(result_json) {
        result_json.and_then(|v| v.as_object()).cloned()
    } else {
        None
    };

    if base_result.is_none() {
        return normalized_summary.map(|s| {
            let mut m = serde_json::Map::new();
            m.insert("summary".into(), serde_json::Value::String(s));
            m
        });
    }
    let base = base_result.expect("checked above");

    if normalized_summary.is_none() {
        return Some(base);
    }
    let new_summary = normalized_summary.expect("checked above");

    if read_comment_text(base.get("summary")).is_some() {
        return Some(base);
    }
    let mut out = base;
    out.insert("summary".into(), serde_json::Value::String(new_summary));
    Some(out)
}

/// 从 resultJson 抽取 heartbeat run summary 字段。
///
/// 提取规则：
/// - 文本字段：`summary` / `result` / `message` / `error`，截断到 `SUMMARY_MAX_CHARS`
/// - 数值别名：`total_cost_usd` / `cost_usd` / `costUsd`
/// - 文本字段：`stopReason` / `timeoutSource`
/// - 数值字段：`effectiveTimeoutSec` / `effectiveTimeoutMs`
/// - 布尔字段：`timeoutConfigured` / `timeoutFired`
///
/// 返回的 Map 仅包含实际存在的字段；空 Map → None。
pub fn summarize_heartbeat_run_result_json(
    result_json: Option<&serde_json::Value>,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    let obj = match result_json.and_then(|v| v.as_object()) {
        Some(o) => o,
        None => return None,
    };

    let mut summary: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();

    for key in ["summary", "result", "message", "error"] {
        if let Some(value) = truncate_summary_text(obj.get(key), HEARTBEAT_RUN_RESULT_SUMMARY_MAX_CHARS) {
            summary.insert(key.into(), serde_json::Value::String(value));
        }
    }

    for key in ["total_cost_usd", "cost_usd", "costUsd"] {
        if let Some(value) = read_numeric_field(obj, key) {
            if !value.is_null() {
                summary.insert(key.into(), value);
            }
        }
    }

    for key in ["stopReason", "timeoutSource"] {
        if let Some(value) = read_comment_text(obj.get(key)) {
            summary.insert(key.into(), serde_json::Value::String(value));
        }
    }

    for key in ["effectiveTimeoutSec", "effectiveTimeoutMs"] {
        if let Some(value) = read_numeric_field(obj, key) {
            if !value.is_null() {
                summary.insert(key.into(), value);
            }
        }
    }

    for key in ["timeoutConfigured", "timeoutFired"] {
        if let Some(serde_json::Value::Bool(b)) = obj.get(key) {
            summary.insert(key.into(), serde_json::Value::Bool(*b));
        }
    }

    if summary.is_empty() {
        None
    } else {
        Some(summary)
    }
}

/// 从 resultJson 抽取可作为 issue comment 的文本。
/// 优先级：`summary` > `result` > `message`，都为空 → None。
pub fn build_heartbeat_run_issue_comment(
    result_json: Option<&serde_json::Value>,
) -> Option<String> {
    let obj = result_json.and_then(|v| v.as_object())?;
    read_comment_text(obj.get("summary"))
        .or_else(|| read_comment_text(obj.get("result")))
        .or_else(|| read_comment_text(obj.get("message")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn constants_match_node() {
        assert_eq!(HEARTBEAT_RUN_RESULT_SUMMARY_MAX_CHARS, 500);
        assert_eq!(HEARTBEAT_RUN_RESULT_OUTPUT_MAX_CHARS, 4_096);
        assert_eq!(HEARTBEAT_RUN_SAFE_RESULT_JSON_MAX_BYTES, 64 * 1024);
    }

    #[test]
    fn merge_null_result_with_text_summary_returns_object() {
        let merged = merge_heartbeat_run_result_json(None, Some("hello"));
        assert_eq!(merged, Some({
            let mut m = serde_json::Map::new();
            m.insert("summary".into(), json!("hello"));
            m
        }));
    }

    #[test]
    fn merge_null_result_with_no_summary_returns_none() {
        assert_eq!(merge_heartbeat_run_result_json(None, None), None);
        assert_eq!(merge_heartbeat_run_result_json(None, Some("")), None);
        assert_eq!(merge_heartbeat_run_result_json(None, Some("   ")), None);
    }

    #[test]
    fn merge_existing_object_preserves_fields_and_adds_summary() {
        let result = json!({ "stopReason": "completed", "durationMs": 120 });
        let merged = merge_heartbeat_run_result_json(Some(&result), Some("ran ok"));
        assert!(merged.is_some());
        let m = merged.unwrap();
        assert_eq!(m.get("stopReason").and_then(|v| v.as_str()), Some("completed"));
        assert_eq!(m.get("summary").and_then(|v| v.as_str()), Some("ran ok"));
        assert_eq!(m.get("durationMs").and_then(|v| v.as_i64()), Some(120));
    }

    #[test]
    fn merge_does_not_overwrite_existing_non_empty_summary() {
        let result = json!({ "summary": "keep me" });
        let merged = merge_heartbeat_run_result_json(Some(&result), Some("new"));
        assert_eq!(
            merged.as_ref().and_then(|m| m.get("summary")).and_then(|v| v.as_str()),
            Some("keep me")
        );
    }

    #[test]
    fn merge_overwrites_empty_summary() {
        let result = json!({ "summary": "   " });
        let merged = merge_heartbeat_run_result_json(Some(&result), Some("fresh"));
        assert_eq!(
            merged.as_ref().and_then(|m| m.get("summary")).and_then(|v| v.as_str()),
            Some("fresh")
        );
    }

    #[test]
    fn merge_rejects_non_object_base() {
        let arr = json!([1, 2, 3]);
        let merged = merge_heartbeat_run_result_json(Some(&arr), Some("x"));
        assert_eq!(
            merged.as_ref().and_then(|m| m.get("summary")).and_then(|v| v.as_str()),
            Some("x")
        );
    }

    #[test]
    fn merge_skips_summary_when_empty_even_with_base() {
        let result = json!({ "k": "v" });
        let merged = merge_heartbeat_run_result_json(Some(&result), Some(""));
        assert_eq!(
            merged.as_ref().and_then(|m| m.get("k")).and_then(|v| v.as_str()),
            Some("v")
        );
        assert!(merged.as_ref().and_then(|m| m.get("summary")).is_none());
    }

    #[test]
    fn summarize_extracts_text_fields_truncated() {
        let long = "x".repeat(800);
        let result = json!({
            "summary": long.clone(),
            "result": "ok",
            "message": "msg",
            "error": null,
        });
        let summary = summarize_heartbeat_run_result_json(Some(&result)).unwrap();
        let s = summary.get("summary").and_then(|v| v.as_str()).unwrap();
        assert_eq!(s.len(), HEARTBEAT_RUN_RESULT_SUMMARY_MAX_CHARS);
        assert_eq!(summary.get("result").and_then(|v| v.as_str()), Some("ok"));
        assert_eq!(summary.get("message").and_then(|v| v.as_str()), Some("msg"));
        assert!(!summary.contains_key("error"));
    }

    #[test]
    fn summarize_extracts_numeric_cost_aliases() {
        let result = json!({
            "total_cost_usd": 1.23,
            "cost_usd": 0.5,
            "costUsd": 2.0,
        });
        let summary = summarize_heartbeat_run_result_json(Some(&result)).unwrap();
        assert!(summary.contains_key("total_cost_usd"));
        assert!(summary.contains_key("cost_usd"));
        assert!(summary.contains_key("costUsd"));
    }

    #[test]
    fn summarize_extracts_stop_metadata_fields() {
        let result = json!({
            "stopReason": "completed",
            "timeoutSource": "config",
            "effectiveTimeoutSec": 30,
            "effectiveTimeoutMs": 30000,
            "timeoutConfigured": true,
            "timeoutFired": false,
        });
        let summary = summarize_heartbeat_run_result_json(Some(&result)).unwrap();
        assert_eq!(summary.get("stopReason").and_then(|v| v.as_str()), Some("completed"));
        assert_eq!(summary.get("timeoutSource").and_then(|v| v.as_str()), Some("config"));
        assert_eq!(summary.get("effectiveTimeoutSec").and_then(|v| v.as_f64()), Some(30.0));
        assert_eq!(summary.get("effectiveTimeoutMs").and_then(|v| v.as_i64()), Some(30000));
        assert_eq!(summary.get("timeoutConfigured").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(summary.get("timeoutFired").and_then(|v| v.as_bool()), Some(false));
    }

    #[test]
    fn summarize_returns_none_for_empty() {
        let result = json!({ "irrelevant": "value" });
        assert!(summarize_heartbeat_run_result_json(Some(&result)).is_none());
    }

    #[test]
    fn summarize_returns_none_for_null_or_non_object() {
        assert!(summarize_heartbeat_run_result_json(None).is_none());
        assert!(summarize_heartbeat_run_result_json(Some(&json!("x"))).is_none());
        assert!(summarize_heartbeat_run_result_json(Some(&json!([1, 2]))).is_none());
    }

    #[test]
    fn build_issue_comment_prefers_summary() {
        let result = json!({
            "summary": "from summary",
            "result": "from result",
            "message": "from message",
        });
        assert_eq!(
            build_heartbeat_run_issue_comment(Some(&result)),
            Some("from summary".to_string())
        );
    }

    #[test]
    fn build_issue_comment_falls_back_to_result_then_message() {
        let result = json!({
            "result": "  spaced  ",
            "message": "from message",
        });
        assert_eq!(
            build_heartbeat_run_issue_comment(Some(&result)),
            Some("spaced".to_string())
        );

        let result2 = json!({ "message": "only msg" });
        assert_eq!(
            build_heartbeat_run_issue_comment(Some(&result2)),
            Some("only msg".to_string())
        );
    }

    #[test]
    fn build_issue_comment_returns_none_when_empty() {
        assert_eq!(build_heartbeat_run_issue_comment(None), None);
        assert_eq!(build_heartbeat_run_issue_comment(Some(&json!({}))), None);
        let result = json!({ "summary": "   " });
        assert_eq!(build_heartbeat_run_issue_comment(Some(&result)), None);
    }
}
