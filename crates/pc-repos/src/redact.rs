//! 通用字段 redact：对 `activity_log.details` 等自由 JSON 中的
//! 敏感键做遮罩。对齐 `paperclip/server/src/redaction.ts::sanitizeRecord`
//! 的核心语义，但只做最小可用集合：
//!
//! - secret 模式键（api[_]?key / access[_]?token / auth / token /
//!   authorization / bearer / secret / passwd / password /
//!   credential / jwt / private[_]?key / cookie / connectionstring）
//!   → 替换为 `***REDACTED***`
//! - `commandArgs` / `command_args` / `argv` 数组 → 保留
//!   `--secret`/`-secret` flag，把后一个元素 redact
//! - `command` / `cmd` / `command-line` 字符串 → 文本层 redact
//! - 保留 `secret_ref` / `user_secret_ref` 绑定（不展开）
//! - 保留 `plain` 绑定结构，遮罩 value

use serde_json::{Map, Value};

pub const REDACTED_VALUE: &str = "***REDACTED***";

const SECRET_FIELD_RE: &str = r"(?i)[A-Za-z0-9_-]*(?:api[-_]?key|access[-_]?token|auth(?:_?token)?|token|authorization|bearer|secret|passwd|password|credential|jwt|private[-_]?key|cookie|connectionstring)[A-Za-z0-9_-]*";
const COMMAND_PAYLOAD_KEY_RE: &str = r"(?i)(^command$|^cmd$|command[-_]?line)";
const COMMAND_ARGS_PAYLOAD_KEY_RE: &str = r"(?i)^(commandArgs|command_?args|argv)$";
const CLI_SECRET_FLAG_RE: &str = r"(?i)^-{1,2}[A-Za-z0-9_-]*(?:api[-_]?key|access[-_]?token|auth(?:_?token)?|token|authorization|bearer|secret|passwd|password|credential|jwt|private[-_]?key|cookie|connectionstring)[A-Za-z0-9_-]*$";

fn is_plain_object(value: &Value) -> bool {
    value.as_object().is_some()
}

fn is_secret_ref(value: &Value) -> bool {
    value
        .as_object()
        .map(|o| {
            o.get("type").and_then(|t| t.as_str()) == Some("secret_ref")
                && o.get("secretId").and_then(|t| t.as_str()).is_some()
        })
        .unwrap_or(false)
}

fn is_user_secret_ref(value: &Value) -> bool {
    value
        .as_object()
        .map(|o| {
            o.get("type").and_then(|t| t.as_str()) == Some("user_secret_ref")
                && o.get("key").and_then(|t| t.as_str()).is_some()
        })
        .unwrap_or(false)
}

fn is_plain_binding(value: &Value) -> bool {
    value
        .as_object()
        .map(|o| o.get("type").and_then(|t| t.as_str()) == Some("plain") && o.contains_key("value"))
        .unwrap_or(false)
}

fn sanitize_value(value: &Value) -> Value {
    if value.is_null() {
        return Value::Null;
    }
    if let Some(arr) = value.as_array() {
        return Value::Array(arr.iter().map(sanitize_value).collect());
    }
    if is_secret_ref(value) || is_user_secret_ref(value) {
        return value.clone();
    }
    if is_plain_binding(value) {
        let mut out = Map::new();
        if let Some(obj) = value.as_object() {
            for (k, v) in obj {
                out.insert(
                    k.clone(),
                    if k == "value" {
                        Value::String(REDACTED_VALUE.into())
                    } else {
                        v.clone()
                    },
                );
            }
        }
        return Value::Object(out);
    }
    if !is_plain_object(value) {
        return value.clone();
    }
    sanitize_record(value)
}

fn redact_sensitive_text(input: &str) -> String {
    // Minimal: replace any substring containing sk-/ghp_/gho_ tokens + jwt-like dot-separated values
    let mut out = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    let jwt_re = regex_lite_jwt();
    let secret_prefixes = ["sk-", "ghp_", "gho_", "ghu_", "ghs_", "ghr_"];
    while i < chars.len() {
        let slice: String = chars[i..].iter().collect();
        let mut matched = false;
        for prefix in &secret_prefixes {
            if slice.to_ascii_lowercase().starts_with(prefix) {
                // redact until next whitespace
                let end = slice
                    .find(|c: char| c.is_whitespace())
                    .unwrap_or(slice.len());
                out.push_str(REDACTED_VALUE);
                i += end;
                matched = true;
                break;
            }
        }
        if matched {
            continue;
        }
        if jwt_re.is_match(&slice) {
            // Redact the whole token
            let m = jwt_re.find(&slice).unwrap();
            out.push_str(&slice[..m.start()]);
            out.push_str(REDACTED_VALUE);
            i += m.end();
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn regex_lite_jwt() -> regex::Regex {
    regex::Regex::new(r"[A-Za-z0-9_-]{6,}\.[A-Za-z0-9_-]{6,}\.[A-Za-z0-9_-]{6,}").unwrap()
}

fn sanitize_command_args(args: &[Value]) -> Vec<Value> {
    let flag_re = regex::Regex::new(CLI_SECRET_FLAG_RE).unwrap();
    let mut redact_next = false;
    args.iter()
        .map(|arg| {
            if redact_next {
                redact_next = false;
                if arg.is_string() {
                    return Value::String(REDACTED_VALUE.into());
                }
                return sanitize_value(arg);
            }
            if let Some(text) = arg.as_str() {
                if flag_re.is_match(text.trim()) {
                    redact_next = true;
                    return arg.clone();
                }
                return Value::String(redact_sensitive_text(text));
            }
            sanitize_value(arg)
        })
        .collect()
}

pub fn sanitize_record(value: &Value) -> Value {
    let Some(obj) = value.as_object() else {
        return value.clone();
    };
    let secret_re = regex::Regex::new(SECRET_FIELD_RE).unwrap();
    let command_re = regex::Regex::new(COMMAND_PAYLOAD_KEY_RE).unwrap();
    let args_re = regex::Regex::new(COMMAND_ARGS_PAYLOAD_KEY_RE).unwrap();
    let mut out = Map::new();
    for (key, val) in obj {
        if args_re.is_match(key) && val.is_array() {
            if let Some(arr) = val.as_array() {
                out.insert(key.clone(), Value::Array(sanitize_command_args(arr)));
                continue;
            }
        }
        if command_re.is_match(key) {
            if let Some(text) = val.as_str() {
                out.insert(key.clone(), Value::String(redact_sensitive_text(text)));
                continue;
            }
        }
        if secret_re.is_match(key) {
            if is_secret_ref(val) || is_user_secret_ref(val) {
                out.insert(key.clone(), val.clone());
                continue;
            }
            if is_plain_binding(val) {
                let mut inner = Map::new();
                if let Some(o) = val.as_object() {
                    for (k, v) in o {
                        inner.insert(
                            k.clone(),
                            if k == "value" {
                                Value::String(REDACTED_VALUE.into())
                            } else {
                                v.clone()
                            },
                        );
                    }
                }
                out.insert(key.clone(), Value::Object(inner));
                continue;
            }
            out.insert(key.clone(), Value::String(REDACTED_VALUE.into()));
            continue;
        }
        out.insert(key.clone(), sanitize_value(val));
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn secret_keys_are_redacted() {
        let input = json!({
            "username": "alice",
            "apiKey": "sk-abcdefghij",
            "password": "p4ssw0rd",
            "auth_token": "abc.def.ghi",
        });
        let out = sanitize_record(&input);
        assert_eq!(out["username"], "alice");
        assert_eq!(out["apiKey"], REDACTED_VALUE);
        assert_eq!(out["password"], REDACTED_VALUE);
        assert_eq!(out["auth_token"], REDACTED_VALUE);
    }

    #[test]
    fn secret_ref_binding_passes_through() {
        let input = json!({
            "token": { "type": "secret_ref", "secretId": "sec-1" }
        });
        let out = sanitize_record(&input);
        assert_eq!(out["token"]["type"], "secret_ref");
        assert_eq!(out["token"]["secretId"], "sec-1");
    }

    #[test]
    fn plain_binding_redacts_value_only() {
        let input = json!({
            "credentials": { "type": "plain", "value": "real" }
        });
        let out = sanitize_record(&input);
        assert_eq!(out["credentials"]["type"], "plain");
        assert_eq!(out["credentials"]["value"], REDACTED_VALUE);
    }

    #[test]
    fn command_args_redacts_flag_followed_by_value() {
        let input = json!({
            "commandArgs": ["git", "clone", "--api-key", "sk-secret", "ok"]
        });
        let out = sanitize_record(&input);
        let arr = out["commandArgs"].as_array().unwrap();
        assert_eq!(arr[2], "--api-key");
        assert_eq!(arr[3], REDACTED_VALUE);
        assert_eq!(arr[4], "ok");
    }

    #[test]
    fn command_string_redacts_inline() {
        let input = json!({
            "command": "git push https://x-access-token:ghp_xxx@github.com"
        });
        let out = sanitize_record(&input);
        assert!(out["command"].as_str().unwrap().contains(REDACTED_VALUE));
        assert!(!out["command"].as_str().unwrap().contains("ghp_xxx"));
    }

    #[test]
    fn nested_object_walks_recursively() {
        let input = json!({
            "outer": {
                "token": "abc",
                "inner": { "password": "p1" }
            }
        });
        let out = sanitize_record(&input);
        assert_eq!(out["outer"]["token"], REDACTED_VALUE);
        assert_eq!(out["outer"]["inner"]["password"], REDACTED_VALUE);
    }

    #[test]
    fn activity_log_payload_redacts_secrets() {
        let input = json!({
            "action": "tool.called",
            "details": {
                "toolName": "github",
                "commandArgs": ["gh", "auth", "login", "--with-token"],
                "token": "ghp_xxxxxxxx",
                "metadata": { "bearer": "Bearer abc" }
            }
        });
        let out = sanitize_record(&input["details"]);
        // secret fields redacted
        assert_eq!(out["token"], REDACTED_VALUE);
        // nested object walked
        assert_eq!(out["metadata"]["bearer"], REDACTED_VALUE);
        // safe fields preserved
        assert_eq!(out["toolName"], "github");
    }

    #[test]
    fn non_redacted_field_keeps_value() {
        let input = json!({
            "name": "ok",
            "kind": "issue"
        });
        let out = sanitize_record(&input);
        assert_eq!(out["name"], "ok");
        assert_eq!(out["kind"], "issue");
    }

    #[test]
    fn command_field_with_jwt_value_is_redacted() {
        // `redact_sensitive_text` runs only on command-style keys
        let input = json!({
            "command": "echo Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ1c2VyIn0.signature123"
        });
        let out = sanitize_record(&input);
        let s = out["command"].as_str().unwrap();
        assert!(s.contains(REDACTED_VALUE), "expected redact, got: {s}");
        assert!(!s.contains("eyJ"), "jwt prefix leaked: {s}");
    }

    #[test]
    fn non_command_key_preserves_arbitrary_string() {
        // Non-secret, non-command keys pass through unchanged
        let input = json!({ "summary": "Bearer eyJ.abc.signature" });
        let out = sanitize_record(&input);
        assert_eq!(out["summary"], input["summary"]);
    }

    #[test]
    fn non_object_passthrough() {
        assert_eq!(sanitize_record(&json!(42)), json!(42));
        assert_eq!(sanitize_record(&json!("x")), json!("x"));
        assert_eq!(sanitize_record(&json!(null)), json!(null));
    }
}
