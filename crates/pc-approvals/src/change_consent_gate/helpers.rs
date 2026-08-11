//! Pure helpers —— target key 构建、payload 校验、legacy key 展开。
//!
//! 与 Node `change-consent-gate.ts` 顶部纯函数部分 1:1 对齐。

use serde_json::Value;

use super::types::{AGENT_PROFILE_CHANGE_CONSENT_FIELDS, mark_result_consumed};


// ============================================================================
// Target key builders
// ============================================================================

/// `agent:<id>:instructions`。
pub fn agent_instructions_change_target_key(agent_id: &str) -> String {
    format!("agent:{agent_id}:instructions")
}

/// `agent:<id>:profile`。
pub fn agent_profile_change_target_key(agent_id: &str) -> String {
    format!("agent:{agent_id}:profile")
}

/// `skill:<id>`。
pub fn skill_change_target_key(skill_id: &str) -> String {
    format!("skill:{skill_id}")
}

/// `skill-slug:<slug>`。
pub fn skill_slug_change_target_key(slug: &str) -> String {
    format!("skill-slug:{slug}")
}

/// `skill-import:<source>`。
pub fn skill_import_change_target_key(source: &str) -> String {
    format!("skill-import:{source}")
}

/// `skills:scan-projects`（固定值）。
pub fn skills_scan_projects_change_target_key() -> &'static str {
    "skills:scan-projects"
}

/// 是否 patch 触及 agent profile 字段中任一需要 consent 的字段。
pub fn touches_agent_profile_change_consent_fields(patch: &Value) -> bool {
    let Some(obj) = patch.as_object() else {
        return false;
    };
    AGENT_PROFILE_CHANGE_CONSENT_FIELDS
        .iter()
        .any(|k| obj.contains_key(*k))
}

// ============================================================================
// Payload / result helpers
// ============================================================================

/// Trim 后非空字符串。
pub(crate) fn read_non_empty_string(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// `RequestConfirmationPayload.detailsMarkdown` 是否显示了一个 diff？
///
/// 判定规则（与 Node `payloadHasDisplayedDiff` 1:1 对齐）：
/// 1. detailsMarkdown 非空；AND
/// 2. 含 ```diff 代码块 OR 含至少一行 `+...` / `-...` 行首字符。
pub fn payload_has_displayed_diff(payload: &Value) -> bool {
    let Some(details) = payload
        .as_object()
        .and_then(|o| o.get("detailsMarkdown"))
        .and_then(read_non_empty_string)
    else {
        return false;
    };

    if contains_diff_codeblock(&details) {
        return true;
    }
    has_diff_lines(&details)
}

fn contains_diff_codeblock(details: &str) -> bool {
    let lower = details.to_ascii_lowercase();
    // ```diff\b...
    if let Some(idx) = lower.find("```diff") {
        // 紧跟 `\b` 即可（行尾或非标识符字符）。
        let after = idx + "```diff".len();
        if after >= lower.len() {
            return true;
        }
        let next_char = lower[after..].chars().next().unwrap_or(' ');
        !next_char.is_ascii_alphanumeric() && next_char != '_'
    } else {
        false
    }
}

fn has_diff_lines(details: &str) -> bool {
    // 行首是 + 或 -（且不是 +++ / --- 文件头）。
    details.lines().any(|line| {
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            return false;
        }
        let first = trimmed.chars().next().unwrap();
        if first != '+' && first != '-' {
            return false;
        }
        // 排除 +++ / ---（diff 文件头）
        let second = trimmed.chars().nth(1).unwrap_or(' ');
        if first == '+' && second == '+' {
            return false;
        }
        if first == '-' && second == '-' {
            return false;
        }
        true
    })
}

/// `RequestConfirmationResult` 是否已被消费（`consumedByRunId` 或 `consumedAt` 非空）。
pub fn request_confirmation_result_consumed(result: Option<&Value>) -> bool {
    let Some(result) = result else {
        return false;
    };
    let Some(obj) = result.as_object() else {
        return false;
    };
    read_non_empty_string(obj.get("consumedByRunId").unwrap_or(&Value::Null)).is_some()
        || read_non_empty_string(obj.get("consumedAt").unwrap_or(&Value::Null)).is_some()
}

// ============================================================================
// Legacy target key expansion
// ============================================================================

/// 把单个 target key 展开为"legacy 兼容集合"（与 Node `legacyTargetKeysFor` 1:1 对齐）。
///
/// 例如 `agent:abc:profile` → `["reflection-coach:agent-description:abc"]`。
fn legacy_target_keys_for(target_key: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();

    if let Some(agent_id) = target_key
        .strip_prefix("agent:")
        .and_then(|s| s.strip_suffix(":instructions"))
    {
        if !agent_id.is_empty() {
            out.push(format!("reflection-coach:agent-instructions:{agent_id}"));
        }
        return out;
    }

    if let Some(agent_id) = target_key
        .strip_prefix("agent:")
        .and_then(|s| s.strip_suffix(":profile"))
    {
        if !agent_id.is_empty() {
            out.push(format!("reflection-coach:agent-description:{agent_id}"));
        }
        return out;
    }

    if let Some(skill_id) = target_key.strip_prefix("skill:") {
        if !skill_id.is_empty() {
            out.push(format!("reflection-coach:company-skill:{skill_id}"));
        }
        return out;
    }

    if let Some(slug) = target_key.strip_prefix("skill-slug:") {
        if !slug.is_empty() {
            out.push(format!("reflection-coach:company-skill-slug:{slug}"));
        }
        return out;
    }

    if let Some(source) = target_key.strip_prefix("skill-import:") {
        if !source.is_empty() {
            out.push(format!(
                "reflection-coach:company-skill-import:{source}"
            ));
            out.push(format!(
                "reflection-coach:company-skill-catalog:{source}"
            ));
        }
        return out;
    }

    if target_key == skills_scan_projects_change_target_key() {
        out.push("reflection-coach:company-skills:scan-projects".to_string());
        return out;
    }

    out
}

/// 把 input targetKeys 展开为（含 legacy 兼容 + 去重）的全集（与 Node `expandTargetKeysForLegacyCompatibility` 1:1 对齐）。
pub fn expand_target_keys_for_legacy_compatibility(target_keys: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for key in target_keys {
        out.push(key.clone());
        for legacy in legacy_target_keys_for(key) {
            out.push(legacy);
        }
    }
    // 去重（保留首次出现顺序）
    let mut seen = std::collections::HashSet::new();
    out.retain(|k| seen.insert(k.clone()));
    out
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn target_keys_format_matches_node() {
        assert_eq!(
            agent_instructions_change_target_key("a1"),
            "agent:a1:instructions"
        );
        assert_eq!(
            agent_profile_change_target_key("a1"),
            "agent:a1:profile"
        );
        assert_eq!(skill_change_target_key("s1"), "skill:s1");
        assert_eq!(skill_slug_change_target_key("my-skill"), "skill-slug:my-skill");
        assert_eq!(
            skill_import_change_target_key("gh"),
            "skill-import:gh"
        );
        assert_eq!(
            skills_scan_projects_change_target_key(),
            "skills:scan-projects"
        );
    }

    #[test]
    fn touches_profile_fields() {
        assert!(touches_agent_profile_change_consent_fields(&json!({
            "name": "x",
        })));
        assert!(touches_agent_profile_change_consent_fields(&json!({
            "role": "x",
        })));
        assert!(touches_agent_profile_change_consent_fields(&json!({
            "title": "x",
        })));
        assert!(touches_agent_profile_change_consent_fields(&json!({
            "capabilities": [],
        })));
        assert!(touches_agent_profile_change_consent_fields(&json!({
            "name": "x",
            "description": "y",
        })));
        // 不相关字段
        assert!(!touches_agent_profile_change_consent_fields(&json!({
            "description": "x",
        })));
        assert!(!touches_agent_profile_change_consent_fields(&json!(null)));
        assert!(!touches_agent_profile_change_consent_fields(&json!("string")));
    }

    #[test]
    fn payload_has_displayed_diff_recognizes_diff_codeblock() {
        let p = json!({
            "detailsMarkdown": "Here is the change:\n```diff\n-old\n+new\n```\n",
        });
        assert!(payload_has_displayed_diff(&p));
    }

    #[test]
    fn payload_has_displayed_diff_recognizes_diff_lines() {
        let p = json!({
            "detailsMarkdown": "Summary:\n+ added line\n- removed line\nAnd more.",
        });
        assert!(payload_has_displayed_diff(&p));
    }

    #[test]
    fn payload_has_displayed_diff_ignores_file_headers() {
        // 纯 diff 文件头（+++/---）不算 diff
        let p = json!({
            "detailsMarkdown": "--- a/file.txt\n+++ b/file.txt",
        });
        assert!(!payload_has_displayed_diff(&p));
    }

    #[test]
    fn payload_has_displayed_diff_returns_false_for_no_details() {
        assert!(!payload_has_displayed_diff(&json!({})));
        assert!(!payload_has_displayed_diff(&json!({
            "detailsMarkdown": "",
        })));
        assert!(!payload_has_displayed_diff(&json!({
            "detailsMarkdown": "   ",
        })));
        assert!(!payload_has_displayed_diff(&json!({
            "detailsMarkdown": "No diff here.",
        })));
    }

    #[test]
    fn result_consumed_detects_markers() {
        assert!(!request_confirmation_result_consumed(None));
        assert!(!request_confirmation_result_consumed(Some(&json!({
            "outcome": "accepted",
        }))));
        assert!(request_confirmation_result_consumed(Some(&json!({
            "outcome": "accepted",
            "consumedByRunId": "run-1",
        }))));
        assert!(request_confirmation_result_consumed(Some(&json!({
            "outcome": "accepted",
            "consumedAt": "2026-01-01T00:00:00Z",
        }))));
        // 空字符串视为未消费
        assert!(!request_confirmation_result_consumed(Some(&json!({
            "outcome": "accepted",
            "consumedByRunId": "",
        }))));
    }

    #[test]
    fn legacy_target_keys_expansion() {
        let r = expand_target_keys_for_legacy_compatibility(&[
            "agent:a1:profile".to_string(),
            "agent:a1:profile".to_string(), // 重复
        ]);
        // 重复 input 也应去重
        let expected: Vec<String> = vec![
            "agent:a1:profile".to_string(),
            "reflection-coach:agent-description:a1".to_string(),
        ];
        assert_eq!(r, expected);

        let r = expand_target_keys_for_legacy_compatibility(&[
            "skill:s1".to_string(),
            "skill-slug:my-skill".to_string(),
            "skill-import:gh".to_string(),
            "skills:scan-projects".to_string(),
        ]);
        assert!(r.contains(&"reflection-coach:company-skill:s1".to_string()));
        assert!(r.contains(&"reflection-coach:company-skill-slug:my-skill".to_string()));
        assert!(r.contains(&"reflection-coach:company-skill-import:gh".to_string()));
        assert!(r.contains(&"reflection-coach:company-skill-catalog:gh".to_string()));
        assert!(r.contains(&"reflection-coach:company-skills:scan-projects".to_string()));
    }

    #[test]
    fn legacy_target_keys_for_preserves_unmatched() {
        // 未匹配的 key 仅返回自身（去重阶段会保留）
        let r = expand_target_keys_for_legacy_compatibility(&[
            "some:unknown:target".to_string(),
        ]);
        assert_eq!(r, vec!["some:unknown:target".to_string()]);
    }

    #[test]
    fn mark_result_consumed_adds_fields() {
        let original = json!({
            "version": 1,
            "outcome": "accepted",
        });
        let consumed = mark_result_consumed(original.clone(), "run-42", "2026-01-01T00:00:00Z");
        assert_eq!(consumed["consumedByRunId"], "run-42");
        assert_eq!(consumed["consumedAt"], "2026-01-01T00:00:00Z");
        assert_eq!(consumed["outcome"], "accepted");
        // 不可变 original
        assert!(original.get("consumedByRunId").is_none());
    }
}
