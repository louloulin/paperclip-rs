//! 敏感字段脱敏。

use serde_json::Value;

#[derive(Debug, Clone)]
pub struct RedactionConfig {
    pub fields: Vec<String>,
    pub placeholder: String,
}

impl Default for RedactionConfig {
    fn default() -> Self {
        Self {
            fields: vec![
                "password".into(), "passwd".into(), "secret".into(), "token".into(),
                "apiKey".into(), "apikey".into(), "api_key".into(), "authorization".into(),
                "cookie".into(), "set-cookie".into(), "masterKey".into(), "master_key".into(),
                "privateKey".into(), "private_key".into(), "accessKey".into(), "access_key".into(),
                "credential".into(),
            ],
            placeholder: "[REDACTED]".into(),
        }
    }
}

pub fn redact_json(value: &Value, cfg: &RedactionConfig) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                if cfg.fields.iter().any(|f| f.eq_ignore_ascii_case(k)) {
                    out.insert(k.clone(), Value::String(cfg.placeholder.clone()));
                } else {
                    out.insert(k.clone(), redact_json(v, cfg));
                }
            }
            Value::Object(out)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(|v| redact_json(v, cfg)).collect()),
        other => other.clone(),
    }
}

pub fn redact_text(input: &str, cfg: &RedactionConfig) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    let mut pending_key: Option<String> = None;
    let mut expecting_value = false;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'"' {
            let start = i;
            i += 1;
            while i < bytes.len() && bytes[i] != b'"' { i += 1; }
            let token = if i < bytes.len() { let s = &input[start + 1..i]; i += 1; s } else { &input[start + 1..] };
            if expecting_value {
                if pending_key.is_some() {
                    out.push('"'); out.push_str(&cfg.placeholder); out.push('"');
                } else {
                    out.push('"'); out.push_str(token); out.push('"');
                }
                pending_key = None;
                expecting_value = false;
            } else {
                let sensitive = cfg.fields.iter().any(|f| f.eq_ignore_ascii_case(token));
                if sensitive { pending_key = Some(token.to_owned()); }
                else { out.push('"'); out.push_str(token); out.push('"'); }
            }
        } else if b == b':' && pending_key.is_some() && !expecting_value {
            expecting_value = true;
            out.push(':');
            i += 1;
        } else {
            let ch = input[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
            if !ch.is_whitespace() && !expecting_value { pending_key = None; }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_nested_password() {
        let v = serde_json::json!({
            "user": "alice", "password": "hunter2",
            "nested": { "token": "abc" },
            "list": [{ "apiKey": "k1" }, { "ok": true }],
        });
        let cfg = RedactionConfig::default();
        let r = redact_json(&v, &cfg);
        assert_eq!(r["user"], "alice");
        assert_eq!(r["password"], "[REDACTED]");
        assert_eq!(r["nested"]["token"], "[REDACTED]");
        assert_eq!(r["list"][0]["apiKey"], "[REDACTED]");
        assert_eq!(r["list"][1]["ok"], true);
    }

    #[test]
    fn case_insensitive_field_match() {
        let v = serde_json::json!({"Password": "x", "API_KEY": "y"});
        let cfg = RedactionConfig::default();
        let r = redact_json(&v, &cfg);
        assert_eq!(r["Password"], "[REDACTED]");
        assert_eq!(r["API_KEY"], "[REDACTED]");
    }

    #[test]
    fn redact_text_replaces_string_values() {
        let cfg = RedactionConfig::default();
        let s = r#"{"password": "hunter2", "user": "alice"}"#;
        let r = redact_text(s, &cfg);
        assert!(r.contains("[REDACTED]"), "expected redaction in: {r}");
        assert!(r.contains("alice"), "expected non-sensitive in: {r}");
    }

    #[test]
    fn redact_text_handles_nested() {
        let cfg = RedactionConfig::default();
        let s = r#"{"outer": {"token": "xyz", "ok": 1}}"#;
        let r = redact_text(s, &cfg);
        assert!(r.contains("[REDACTED]"));
        assert!(r.contains("\"ok\""));
    }

    #[test]
    fn redact_text_passthrough_when_no_match() {
        let cfg = RedactionConfig::default();
        let s = r#"{"hello": "world"}"#;
        let r = redact_text(s, &cfg);
        assert_eq!(r, s);
    }
}
