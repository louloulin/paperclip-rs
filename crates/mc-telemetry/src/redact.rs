//! 日志 redaction：在写入日志前去掉 secret / token / cookie。

use serde_json::Value;

/// 已知敏感字段名（key 子串匹配，case-insensitive）。
pub const SENSITIVE_KEYS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "token",
    "apikey",
    "api_key",
    "api-key",
    "authorization",
    "cookie",
    "set-cookie",
    "session",
    "private_key",
    "privatekey",
    "private-key",
    "pat",
    "access_token",
    "refresh_token",
    "client_secret",
    "csrf",
    "x-csrf",
];

pub struct Redactor {
    pub replacement: String,
}

impl Default for Redactor {
    fn default() -> Self {
        Self {
            replacement: "[REDACTED]".into(),
        }
    }
}

impl Redactor {
    pub fn new(replacement: impl Into<String>) -> Self {
        Self {
            replacement: replacement.into(),
        }
    }

    pub fn is_sensitive(&self, key: &str) -> bool {
        let lower = key.to_ascii_lowercase();
        SENSITIVE_KEYS.iter().any(|k| lower.contains(k))
    }

    /// 递归 redact 一棵 JSON。
    pub fn redact_json(&self, value: &mut Value) {
        match value {
            Value::Object(map) => {
                for (k, v) in map.iter_mut() {
                    if self.is_sensitive(k) {
                        *v = Value::String(self.replacement.clone());
                    } else {
                        self.redact_json(v);
                    }
                }
            }
            Value::Array(items) => {
                for item in items.iter_mut() {
                    self.redact_json(item);
                }
            }
            _ => {}
        }
    }

    /// redact 一行文本：把 `key=value` / `"key":"value"` / `key: value` 的模式替换。
    pub fn redact_str<'a>(&self, input: &'a str) -> String {
        let mut output = String::with_capacity(input.len());
        let mut chars = input.chars().peekable();
        let mut key_buf = String::new();
        while let Some(c) = chars.next() {
            if c.is_ascii_alphabetic() || c == '-' || c == '_' {
                key_buf.push(c);
                continue;
            }
            // key finished
            if !key_buf.is_empty() {
                if self.is_sensitive(&key_buf) {
                    // skip until separator
                    output.push_str(&key_buf);
                    output.push(c);
                    // consume value: `=value` or `: value` or `: "value"`
                    let mut consumed = false;
                    if c == '=' {
                        // read until whitespace or comma or quote-end
                        while let Some(&next) = chars.peek() {
                            if next.is_whitespace() || next == ',' || next == ';' {
                                break;
                            }
                            chars.next();
                            consumed = true;
                        }
                        output.push_str(&self.replacement);
                    } else if c == ':' {
                        // skip whitespace
                        while let Some(&next) = chars.peek() {
                            if next.is_whitespace() {
                                output.push(next);
                                chars.next();
                                continue;
                            }
                            if next == '"' {
                                // quoted value
                                output.push(next);
                                chars.next();
                                while let Some(&next) = chars.peek() {
                                    output.push(next);
                                    chars.next();
                                    if next == '"' {
                                        break;
                                    }
                                }
                                // overwrite last quoted block with replacement
                                // find the actual quoted region
                                if let Some(start) = output.rfind('"') {
                                    let after = output[start..].to_string();
                                    let prefix_len = output.len() - after.len();
                                    output.truncate(prefix_len);
                                    output.push('"');
                                    output.push_str(&self.replacement);
                                    output.push('"');
                                }
                                consumed = true;
                                break;
                            }
                            // unquoted value
                            while let Some(&next) = chars.peek() {
                                if next.is_whitespace() || next == ',' || next == '}' {
                                    break;
                                }
                                chars.next();
                                consumed = true;
                            }
                            output.push_str(&self.replacement);
                            break;
                        }
                        if !consumed {
                            // nothing after colon
                        }
                    } else {
                        output.push(c);
                    }
                } else {
                    output.push_str(&key_buf);
                    output.push(c);
                }
                key_buf.clear();
            } else {
                output.push(c);
            }
        }
        if !key_buf.is_empty() {
            output.push_str(&key_buf);
        }
        output
    }
}

/// 默认 redactor redact 一行。
pub fn redact_log(input: &str) -> String {
    Redactor::default().redact_str(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_redaction_replaces_secrets() {
        let mut v = serde_json::json!({
            "name": "alice",
            "password": "hunter2",
            "nested": {
                "api_key": "sk-abc",
                "ok": "value"
            }
        });
        Redactor::default().redact_json(&mut v);
        assert_eq!(v["password"], "[REDACTED]");
        assert_eq!(v["nested"]["api_key"], "[REDACTED]");
        assert_eq!(v["nested"]["ok"], "value");
    }

    #[test]
    fn str_redaction_handles_key_equals_value() {
        let s = "user=alice password=hunter2 ok=1";
        let redacted = Redactor::default().redact_str(s);
        assert!(redacted.contains("password=[REDACTED]"));
        assert!(redacted.contains("user=alice"));
        assert!(redacted.contains("ok=1"));
    }

    #[test]
    fn str_redaction_handles_json_style() {
        let s = r#"{"password": "hunter2", "name": "alice"}"#;
        let redacted = Redactor::default().redact_str(s);
        assert!(redacted.contains("[REDACTED]"));
        assert!(redacted.contains("alice"));
    }

    #[test]
    fn is_sensitive_is_case_insensitive() {
        let r = Redactor::default();
        assert!(r.is_sensitive("Password"));
        assert!(r.is_sensitive("API_KEY"));
        assert!(r.is_sensitive("Authorization"));
        assert!(!r.is_sensitive("name"));
    }
}