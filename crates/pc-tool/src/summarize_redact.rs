#![forbid(unsafe_code)]

//! Tool argument redaction + summarization.
//! R709: Direct port of tool-access-policy.ts::summarizeAndRedact + SENSITIVE_KEY_RE + SECRET_VALUE_RE.

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::OnceLock;

const REDACTED_PLACEHOLDER: &'static str = "[REDACTED]";
const STRING_TRUNCATE_LIMIT: usize = 500;
const SUMMARY_TRUNCATE_LIMIT: usize = 4000;
const ARRAY_LIMIT: usize = 50;

fn sensitive_key_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new("(?i)(^|[_-])(api[_-]?key|authorization|bearer|client[_-]?secret|cookie|credential|jwt|password|private[_-]?key|refresh[_-]?token|secret|session[_-]?token|token)($|[_-])").unwrap()
    })
}

fn secret_value_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)\b(sk-[a-z0-9_-]{12,}|ghp_[a-z0-9_]{12,}|xox[baprs]-[a-z0-9-]{12,}|bearer\s+[a-z0-9._-]{12,})\b").unwrap()
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RedactionResult {
    pub summary: RedactionSummary,
    pub redaction_plan: RedactionPlan,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RedactionSummary {
    pub summary: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub redacted_fields: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RedactionPlan {
    pub redacted_field_count: usize,
    pub redacted_fields: Vec<String>,
}

fn is_sensitive_key(key: &str) -> bool {
    sensitive_key_re().is_match(key)
}

fn is_secret_value(s: &str) -> bool {
    secret_value_re().is_match(s)
}

fn visit(current: &Value, path: &str, redacted_fields: &mut Vec<String>) -> Value {
    match current {
        Value::String(s) => {
            if is_secret_value(s) {
                if !path.is_empty() {
                    redacted_fields.push(path.to_string());
                } else {
                    redacted_fields.push("$".to_string());
                }
                Value::String(REDACTED_PLACEHOLDER.to_string())
            } else if s.len() > STRING_TRUNCATE_LIMIT {
                let mut truncated = String::with_capacity(STRING_TRUNCATE_LIMIT + 20);
                truncated.push_str(&s[..STRING_TRUNCATE_LIMIT]);
                truncated.push_str("...[truncated]");
                Value::String(truncated)
            } else {
                current.clone()
            }
        }
        Value::Array(arr) => {
            let limit = arr.len().min(ARRAY_LIMIT);
            let items: Vec<Value> = arr.iter().take(limit).enumerate().map(|(i, v)| {
                let sub_path = format!("{}[{}]", path, i);
                visit(v, &sub_path, redacted_fields)
            }).collect();
            Value::Array(items)
        }
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                let nested_path = if path.is_empty() { k.clone() } else { format!("{}.{}", path, k) };
                if is_sensitive_key(k) {
                    redacted_fields.push(nested_path);
                    out.insert(k.clone(), Value::String(REDACTED_PLACEHOLDER.to_string()));
                } else {
                    out.insert(k.clone(), visit(v, &nested_path, redacted_fields));
                }
            }
            Value::Object(out)
        }
        _ => current.clone(),
    }
}

fn stable_stringify(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "".to_string())
}

/// Summarize + redact sensitive fields. Node summarizeAndRedact 1:1 parity.
pub fn summarize_and_redact(value: &Value) -> RedactionResult {
    let mut redacted_fields: Vec<String> = Vec::new();
    let redacted = visit(value, "", &mut redacted_fields);
    let text = stable_stringify(&redacted);
    let summary_text = if text.len() > SUMMARY_TRUNCATE_LIMIT {
        format!("{}...[truncated]", &text[..SUMMARY_TRUNCATE_LIMIT])
    } else {
        text.clone()
    };
    let size_bytes = text.as_bytes().len() as u64;
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    let sha = format!("{:x}", hasher.finalize());
    RedactionResult {
        summary: RedactionSummary {
            summary: summary_text,
            size_bytes,
            sha256: sha,
            redacted_fields: redacted_fields.clone(),
        },
        redaction_plan: RedactionPlan {
            redacted_field_count: redacted_fields.len(),
            redacted_fields,
        },
    }
}

#[cfg(test)]
mod internal_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sensitive_key_redacted() {
        let v = json!({"api_key": "abc123", "name": "foo"});
        let r = summarize_and_redact(&v);
        assert_eq!(r.summary.summary, json!({"api_key": "[REDACTED]", "name": "foo"}).to_string());
        assert_eq!(r.redaction_plan.redacted_field_count, 1);
        assert_eq!(r.redaction_plan.redacted_fields, vec!["api_key".to_string()]);
    }

    #[test]
    fn multiple_sensitive_keys() {
        let v = json!({
            "Authorization": "Bearer x",
            "cookie": "sid=1",
            "session_token": "tok",
            "name": "foo",
        });
        let r = summarize_and_redact(&v);
        assert_eq!(r.redaction_plan.redacted_field_count, 3);
        let names: Vec<&String> = r.redaction_plan.redacted_fields.iter().collect();
        assert!(names.contains(&&"Authorization".to_string()));
        assert!(names.contains(&&"cookie".to_string()));
        assert!(names.contains(&&"session_token".to_string()));
    }

    #[test]
    fn secret_value_pattern_sk_prefix() {
        let v = json!({"content": "prefix sk-abc1234567890123456 end"});
        let r = summarize_and_redact(&v);
        assert_eq!(r.redaction_plan.redacted_field_count, 1);
        assert!(r.summary.summary.contains("[REDACTED]"));
    }

    #[test]
    fn secret_value_pattern_ghp_prefix() {
        let v = json!({"token": "ghp_abc123def456ghi789jkl012mno"});
        let r = summarize_and_redact(&v);
        assert_eq!(r.redaction_plan.redacted_field_count, 1);
    }

    #[test]
    fn secret_value_pattern_xoxb() {
        let v = json!({"slack": "xoxb-1234567890123-1234567890123"});
        let r = summarize_and_redact(&v);
        assert_eq!(r.redaction_plan.redacted_field_count, 1);
    }

    #[test]
    fn secret_value_pattern_bearer() {
        let v = json!({"h": "bearer abcdefghijklmnop1234"});
        let r = summarize_and_redact(&v);
        assert_eq!(r.redaction_plan.redacted_field_count, 1);
    }

    #[test]
    fn no_redaction_for_safe_content() {
        let v = json!({"name": "alice", "age": 30});
        let r = summarize_and_redact(&v);
        assert_eq!(r.redaction_plan.redacted_field_count, 0);
        assert_eq!(r.summary.summary, json!({"name": "alice", "age": 30}).to_string());
    }

    #[test]
    fn string_truncation_at_500() {
        let long = "x".repeat(600);
        let v = json!({"content": long});
        let r = summarize_and_redact(&v);
        assert!(r.summary.summary.contains("...[truncated]"));
        let val_str = r.summary.summary;
        let prefix_part: String = val_str.chars().take_while(|c| *c != '.').collect();
        assert!(prefix_part.chars().count() >= 500);
    }

    #[test]
    fn array_limited_to_50() {
        let arr: Vec<i32> = (0..100).collect();
        let v = json!({"items": arr});
        let r = summarize_and_redact(&v);
        let parsed: serde_json::Value = serde_json::from_str(&r.summary.summary).unwrap();
        let items = parsed.get("items").unwrap().as_array().unwrap();
        assert_eq!(items.len(), 50);
    }

    #[test]
    fn nested_redaction_with_path() {
        let v = json!({"user": {"password": "secret123", "name": "foo"}});
        let r = summarize_and_redact(&v);
        assert_eq!(r.redaction_plan.redacted_fields, vec!["user.password".to_string()]);
    }

    #[test]
    fn summary_sha256_is_64_hex() {
        let v = json!({"name": "alice"});
        let r = summarize_and_redact(&v);
        assert_eq!(r.summary.sha256.len(), 64);
        assert!(r.summary.sha256.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn summary_size_bytes_positive() {
        let v = json!({"name": "alice"});
        let r = summarize_and_redact(&v);
        assert!(r.summary.size_bytes > 0);
    }

    #[test]
    fn empty_value_handled() {
        let v = json!({});
        let r = summarize_and_redact(&v);
        assert_eq!(r.redaction_plan.redacted_field_count, 0);
        assert_eq!(r.summary.summary, "{}");
    }

    #[test]
    fn secret_in_array_indexed_path() {
        let v = json!({"list": [{"name": "alice"}, {"API_KEY": "k"}]});
        let r = summarize_and_redact(&v);
        assert!(r.redaction_plan.redacted_fields.contains(&"list[1].API_KEY".to_string()));
    }

    #[test]
    fn token_in_value_redacts_path() {
        let v = json!({
            "msg": "here is sk-1234567890abcdef1234 token",
        });
        let r = summarize_and_redact(&v);
        assert_eq!(r.redaction_plan.redacted_field_count, 1);
        assert_eq!(r.redaction_plan.redacted_fields[0], "msg");
    }

    #[test]
    fn top_level_string_secret() {
        let v = json!("sk-abcdefghijklmnop1234");
        let r = summarize_and_redact(&v);
        assert_eq!(r.redaction_plan.redacted_fields, vec!["$".to_string()]);
    }
}
