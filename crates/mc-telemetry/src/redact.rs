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
    /// 解析策略：扫描 word run 作为候选 key，向后窥视（允许空白 + 至多一个闭引号）
    /// 找 `=` / `:` 分隔符；命中才按 key-value 处理，否则原文透传。
    pub fn redact_str(&self, input: &str) -> String {
        let chars: Vec<char> = input.chars().collect();
        let n = chars.len();
        let mut out = String::with_capacity(input.len());
        let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
        let mut i = 0;
        while i < n {
            if !is_word(chars[i]) {
                out.push(chars[i]);
                i += 1;
                continue;
            }
            // 收集候选 key
            let start = i;
            while i < n && is_word(chars[i]) {
                i += 1;
            }
            let key: String = chars[start..i].iter().collect();

            // 窥视分隔符：ws [""] ws [=|:]
            let mut j = i;
            while j < n && chars[j].is_whitespace() {
                j += 1;
            }
            let had_close_quote = j < n && chars[j] == '"';
            if had_close_quote {
                j += 1;
                while j < n && chars[j].is_whitespace() {
                    j += 1;
                }
            }
            let is_kv = j < n && (chars[j] == '=' || chars[j] == ':');
            if !is_kv {
                out.push_str(&key);
                continue;
            }
            let sep_idx = j;

            if !self.is_sensitive(&key) {
                // 非敏感 key：只透传 key 本身，后续字符走常规扫描。
                out.push_str(&key);
                continue;
            }

            // 解析 value：sep 后空白，再引号或裸词。
            let mut v = sep_idx + 1;
            while v < n && chars[v].is_whitespace() {
                v += 1;
            }
            let quoted = v < n && chars[v] == '"';
            let value_start;
            let value_end; // exclusive
            if quoted {
                v += 1;
                value_start = v;
                while v < n {
                    if chars[v] == '\\' && v + 1 < n {
                        v += 2;
                        continue;
                    }
                    if chars[v] == '"' {
                        break;
                    }
                    v += 1;
                }
                value_end = v; // 停在闭引号或末尾
            } else {
                value_start = v;
                while v < n
                    && !chars[v].is_whitespace()
                    && chars[v] != ','
                    && chars[v] != ';'
                    && chars[v] != '}'
                    && chars[v] != ']'
                {
                    v += 1;
                }
                value_end = v;
            }

            // 输出：key + key/sep 之间的原文（含引号/空白） + sep + sep 到 value 的空白 + 替换值。
            out.push_str(&key);
            out.extend(chars[i..sep_idx].iter());
            out.push(chars[sep_idx]);
            // sep 后到 value_start 之间的空白保持原样（仅 quoted 时可能非空）。
            let ws_start = sep_idx + 1;
            out.extend(chars[ws_start..value_start].iter());
            if quoted {
                out.push('"');
                out.push_str(&self.replacement);
                out.push('"');
                // 消费闭引号
                if value_end < n && chars[value_end] == '"' {
                    i = value_end + 1;
                } else {
                    i = value_end;
                }
            } else {
                out.push_str(&self.replacement);
                i = value_end;
            }
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
