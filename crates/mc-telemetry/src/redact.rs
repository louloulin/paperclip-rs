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
    /// 扫描 key token（`[A-Za-z_-]`），若 key 敏感且其后（可跨引号与空白）出现
    /// `=` / `:` 分隔符，则消费对应 value 并替换为 replacement；否则原样输出。
    pub fn redact_str<'a>(&self, input: &'a str) -> String {
        let chars: Vec<char> = input.chars().collect();
        let mut out = String::with_capacity(chars.len());
        let mut i = 0;
        let is_key_char = |c: char| c.is_ascii_alphabetic() || c == '-' || c == '_';
        while i < chars.len() {
            if !is_key_char(chars[i]) {
                out.push(chars[i]);
                i += 1;
                continue;
            }
            let start = i;
            while i < chars.len() && is_key_char(chars[i]) {
                i += 1;
            }
            let key: String = chars[start..i].iter().collect();
            if !self.is_sensitive(&key) {
                out.push_str(&key);
                continue;
            }
            // lookahead: key [" ]? [ws] (= | :) [ws] value
            let mut k = i;
            if k < chars.len() && chars[k] == '"' {
                k += 1;
            }
            while k < chars.len() && chars[k].is_whitespace() {
                k += 1;
            }
            if k >= chars.len() || (chars[k] != '=' && chars[k] != ':') {
                // 敏感词但不是键值对（如 prose 中出现）→ 原样输出
                out.push_str(&key);
                continue;
            }
            // 输出到分隔符为止（含 key、闭引号、空白与 `=`/`:`）
            out.extend(&chars[start..=k]);
            let mut v = k + 1;
            while v < chars.len() && chars[v].is_whitespace() {
                out.push(chars[v]);
                v += 1;
            }
            if v < chars.len() && chars[v] == '"' {
                // 引号值 → 保持引号风格
                out.push('"');
                out.push_str(&self.replacement);
                out.push('"');
                v += 1;
                while v < chars.len() && chars[v] != '"' {
                    v += 1;
                }
                if v < chars.len() {
                    v += 1; // 跳过闭引号
                }
            } else {
                out.push_str(&self.replacement);
                while v < chars.len()
                    && !chars[v].is_whitespace()
                    && chars[v] != ','
                    && chars[v] != ';'
                    && chars[v] != '}'
                    && chars[v] != '\n'
                {
                    v += 1;
                }
            }
            i = v;
        }
        out
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