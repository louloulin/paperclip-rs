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

    /// redact 一行文本：把 `key=value` / `"key": "value"` / `key: value` 的模式替换。
    ///
    /// key 允许带引号（JSON 风格）；值为带引号时保留引号结构。
    pub fn redact_str<'a>(&self, input: &'a str) -> String {
        let chars: Vec<char> = input.chars().collect();
        let mut output = String::with_capacity(input.len());
        let mut i = 0usize;
        let is_key_char = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';

        while i < chars.len() {
            let c = chars[i];
            // key 只从字母 / `-` / `_` 开头（数字开头不视为 key，与旧行为一致）。
            if c.is_ascii_alphabetic() || c == '-' || c == '_' {
                let start = i;
                while i < chars.len() && is_key_char(chars[i]) {
                    i += 1;
                }
                let key: String = chars[start..i].iter().collect();

                // 前瞻分隔符：可跳过一个收尾引号 + 空白，再看是否为 `:` / `=`。
                let mut j = i;
                if j < chars.len() && chars[j] == '"' {
                    j += 1;
                }
                while j < chars.len() && chars[j].is_whitespace() {
                    j += 1;
                }
                let has_sep = j < chars.len() && (chars[j] == ':' || chars[j] == '=');

                if has_sep && self.is_sensitive(&key) {
                    output.push_str(&key);
                    // 消耗 key 与分隔符之间的字符（收尾引号 / 空白）
                    while i < j {
                        output.push(chars[i]);
                        i += 1;
                    }
                    output.push(chars[i]); // `:` 或 `=`
                    i += 1;
                    // 分隔符后的空白
                    while i < chars.len() && chars[i].is_whitespace() {
                        output.push(chars[i]);
                        i += 1;
                    }
                    if i < chars.len() && chars[i] == '"' {
                        // 带引号的值：保留引号结构
                        i += 1;
                        while i < chars.len() && chars[i] != '"' {
                            i += 1;
                        }
                        if i < chars.len() {
                            i += 1;
                        }
                        output.push('"');
                        output.push_str(&self.replacement);
                        output.push('"');
                    } else {
                        // 无引号的值：消费到空白 / `,` / `;` / `}`
                        let vstart = i;
                        while i < chars.len()
                            && !chars[i].is_whitespace()
                            && chars[i] != ','
                            && chars[i] != ';'
                            && chars[i] != '}'
                        {
                            i += 1;
                        }
                        if i > vstart {
                            output.push_str(&self.replacement);
                        }
                    }
                } else {
                    // 非敏感 key（或后面没有分隔符）：原样输出
                    output.push_str(&key);
                }
            } else {
                output.push(c);
                i += 1;
            }
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
