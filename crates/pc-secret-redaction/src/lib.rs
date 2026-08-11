#![forbid(unsafe_code)]

//! Secret field name detection + JWT-shape detection + recursive JSON redaction.
//!
//! R527: Direct port of `paperclip/server/src/redaction.ts` (pure parts).
//!
//! 设计原则:
//! - 所有 pub fn 都是纯函数 (无 IO, 无副作用, 无环境依赖)
//! - regex 编译成 `Lazy<Regex>` 一次, 后续零成本
//! - 错误用 [`RedactionError`] enum (目前只有 InvalidPattern, 但预留扩展)
//! - 全部 case-insensitive (Node upstream 用 `/i` flag)
//!
//! 范围 (本 crate):
//! - [`REDACTED_EVENT_VALUE`] 常量 (`"***REDACTED***"`)
//! - [`SECRET_TEXT_HINTS`] 常量列表
//! - [`is_secret_field_name`] 谓词 (基于 `SECRET_FIELD_NAME_PATTERN`)
//! - [`is_jwt_like`] 谓词 (检测 JWT 字符串 shape)
//! - [`maybe_contains_secret_text`] 启发式 (决定是否值得跑 redact)
//! - [`redact_sensitive_text`] 文本 redact (替换 inline JSON + escaped JSON +
//!   命令行风格 secrets: `Authorization: Bearer`, `sk-...`, `ghp_...` 等)
//! - [`redact_record`] 递归 JSON object redact (基于 field name pattern + JWT 值)
//! - [`is_cli_secret_flag`] 谓词 (检测 `--api-key` 等 CLI flag)
//!
//! **不** 范围 (留给集成层):
//! - `secret_ref` / `user_secret_ref` binding 类型检测 (需要 DTO 类型)
//! - `commandArgs` argv 处理 (需要 command execution context)
//! - `redactCommandTextForLogs` (来自 server-utils, 涉及完整 command resolution)
//!
//! Node 上游同时引用 `@paperclipai/adapter-utils` 的 `redactCommandText` 函数;
//! 本 crate 把其纯 inline pattern (Authorization Bearer / sk-* / ghp_* /
//! in-text JWT shape) 直接 inline, 这样 `redact_sensitive_text` 自包含,
//! 不需要把 adapter-utils 也搬过来.

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;
use thiserror::Error;

/// Replacement token used for all redacted values.
///
/// Mirrors Node upstream `REDACTED_EVENT_VALUE = "***REDACTED***"`.
pub const REDACTED_EVENT_VALUE: &str = "***REDACTED***";

/// Heuristic word list — used by [`maybe_contains_secret_text`] to decide
/// whether running the regex over `input` is likely to find anything.
pub const SECRET_TEXT_HINTS: &[&str] = &[
    "api",
    "key",
    "token",
    "auth",
    "bearer",
    "secret",
    "pass",
    "credential",
    "jwt",
    "private",
    "cookie",
    "connectionstring",
    "sk-",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
];

/// Errors that can arise in this crate.
#[derive(Debug, Error)]
pub enum RedactionError {
    #[error("invalid regex pattern: {0}")]
    InvalidPattern(String),
}

// ---------------------------------------------------------------------------
// Regex patterns — mirror Node upstream `redaction.ts` and (inline) the
// `command-redaction.ts` patterns used by `redactCommandText`.
// ---------------------------------------------------------------------------

/// Base field-name pattern: matches strings like `apiKey`, `access_token`,
/// `auth-token`, `password`, `bearerToken`, `privateKey`, `jwtSecret`, etc.
///
/// Mirrors Node upstream `SECRET_FIELD_NAME_PATTERN` literal. Case-insensitive.
pub static SECRET_FIELD_NAME_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)[A-Za-z0-9_-]*(?:api[-_]?key|access[-_]?token|auth(?:_?token)?|token|authorization|bearer|secret|passwd|password|credential|jwt|private[-_]?key|cookie|connectionstring)[A-Za-z0-9_-]*"
    ).expect("valid regex pattern")
});

/// JWT-shape regex: three base64url segments separated by `.`, optional 4th.
///
/// Mirrors Node upstream `JWT_VALUE_RE` (no `i` flag — case matters).
pub static JWT_VALUE_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+(?:\.[A-Za-z0-9_-]+)?$")
        .expect("valid regex pattern")
});

/// Inline JSON secret-field pattern, e.g. `"apiKey": "abc123"`.
///
/// Mirrors Node upstream `JSON_SECRET_FIELD_TEXT_RE` (with `g` + `i` flags).
/// The two capture groups are the opening `"<field>": "` and closing `"`
/// so that `replace` can splice [`REDACTED_EVENT_VALUE`] between them.
pub static JSON_SECRET_FIELD_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"(?i)((?:"|')?[A-Za-z0-9_-]*(?:api[-_]?key|access[-_]?token|auth(?:_?token)?|token|authorization|bearer|secret|passwd|password|credential|jwt|private[-_]?key|cookie|connectionstring)[A-Za-z0-9_-]*(?:"|')?\s*:\s*(?:"|'))[^"'\r\n]+((?:"|'))"#,
    )
    .expect("valid regex pattern")
});

/// Escaped-JSON variant for strings like `\"apiKey\": \"abc123\"`.
///
/// Mirrors Node upstream `ESCAPED_JSON_SECRET_FIELD_TEXT_RE` (with `g` + `i`).
pub static ESCAPED_JSON_SECRET_FIELD_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"(?i)((?:\\")?[A-Za-z0-9_-]*(?:api[-_]?key|access[-_]?token|auth(?:_?token)?|token|authorization|bearer|secret|passwd|password|credential|jwt|private[-_]?key|cookie|connectionstring)[A-Za-z0-9_-]*(?:\\")?\s*:\s*(?:\\"))[^\\\r\n]+((?:\\"))"#,
    )
    .expect("valid regex pattern")
});

/// CLI `--secret X` / `--api-key X` style flag detection (case-insensitive).
///
/// Mirrors Node upstream `CLI_SECRET_FLAG_RE`.
pub static CLI_SECRET_FLAG_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)^-{1,2}(api[-_]?key|access[-_]?token|auth(?:_?token)?|token|authorization|bearer|secret|passwd|password|credential|jwt|private[-_]?key|cookie|connectionstring)[A-Za-z0-9_-]*$"
    ).expect("valid regex pattern")
});

/// Inline `Authorization: Bearer <token>` form (used by command-line text).
///
/// Mirrors Node upstream `COMMAND_AUTHORIZATION_BEARER_RE` (`g` + `i`).
pub static AUTHORIZATION_BEARER_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?i)\bAuthorization\s*:\s*Bearer\s+[^\s"'`]+"#).expect("valid regex pattern")
});

/// Inline OpenAI-style `sk-...` keys (12+ alphanum chars).
///
/// Mirrors Node upstream `COMMAND_OPENAI_KEY_RE`.
pub static OPENAI_KEY_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\bsk-[A-Za-z0-9_-]{12,}\b").expect("valid regex pattern")
});

/// Inline GitHub tokens `gh[pousr]_<20+ alphanum>`.
///
/// Mirrors Node upstream `COMMAND_GITHUB_TOKEN_RE`.
pub static GITHUB_TOKEN_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\bgh[pousr]_[A-Za-z0-9_]{20,}\b").expect("valid regex pattern")
});

/// Inline JWT shape in free-form text (8+ chars per segment, optional 4th).
///
/// Mirrors Node upstream `COMMAND_JWT_RE`.
pub static INLINE_JWT_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\b[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}(?:\.[A-Za-z0-9_-]{8,})?\b")
        .expect("valid regex pattern")
});

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// True if `name` (a JSON object key) matches the secret-field pattern.
#[must_use]
pub fn is_secret_field_name(name: &str) -> bool {
    SECRET_FIELD_NAME_PATTERN.is_match(name)
}

/// True if `value` looks like a JWT (three base64url segments separated by `.`).
#[must_use]
pub fn is_jwt_like(value: &str) -> bool {
    JWT_VALUE_PATTERN.is_match(value)
}

/// Heuristic — returns true if `input` is worth running the expensive
/// redact regex over. Mirrors Node upstream `maybeContainsSecretText`:
///
/// - True if any [`SECRET_TEXT_HINTS`] substring appears (case-insensitive)
/// - OR `input` contains a `.` (catches JWTs, hostnames, env-style refs)
#[must_use]
pub fn maybe_contains_secret_text(input: &str) -> bool {
    let lower = input.to_lowercase();
    SECRET_TEXT_HINTS
        .iter()
        .any(|hint| lower.contains(hint))
        || input.contains('.')
}

/// Redact inline secret patterns in a free-form string.
///
/// Pipeline (mirrors Node upstream `redactSensitiveText`):
/// 1. Skip if [`maybe_contains_secret_text`] returns false
/// 2. Replace inline JSON (`"apiKey": "abc"`)
/// 3. Replace escaped JSON (`\"apiKey\": \"abc\"`)
/// 4. Replace `Authorization: Bearer <token>`
/// 5. Replace OpenAI-style `sk-...` keys
/// 6. Replace GitHub-style `ghp_...` / `gho_...` etc tokens
/// 7. Replace inline JWT shape (`xxx.yyy.zzz`)
#[must_use]
pub fn redact_sensitive_text(input: &str) -> String {
    if !maybe_contains_secret_text(input) {
        return input.to_string();
    }
    let s = JSON_SECRET_FIELD_PATTERN.replace_all(input, |caps: &regex::Captures| {
        format!(
            "{}{}{}",
            caps.get(1).map(|m| m.as_str()).unwrap_or(""),
            REDACTED_EVENT_VALUE,
            caps.get(2).map(|m| m.as_str()).unwrap_or("")
        )
    });
    let s = ESCAPED_JSON_SECRET_FIELD_PATTERN.replace_all(&s, |caps: &regex::Captures| {
        format!(
            "{}{}{}",
            caps.get(1).map(|m| m.as_str()).unwrap_or(""),
            REDACTED_EVENT_VALUE,
            caps.get(2).map(|m| m.as_str()).unwrap_or("")
        )
    });
    let s = AUTHORIZATION_BEARER_PATTERN.replace_all(&s, |caps: &regex::Captures| {
        let head = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        format!("{head}{REDACTED_EVENT_VALUE}")
    });
    let s = OPENAI_KEY_PATTERN.replace_all(&s, REDACTED_EVENT_VALUE);
    let s = GITHUB_TOKEN_PATTERN.replace_all(&s, REDACTED_EVENT_VALUE);
    INLINE_JWT_PATTERN.replace_all(&s, REDACTED_EVENT_VALUE).into_owned()
}

/// Recursively redact a JSON object, replacing values whose key matches
/// [`is_secret_field_name`] or whose value matches [`is_jwt_like`].
///
/// - Object key matches secret pattern → value replaced with `REDACTED_EVENT_VALUE`
/// - String value matches JWT shape → replaced with `REDACTED_EVENT_VALUE`
/// - Otherwise, recurse into nested objects / arrays
/// - Primitive values (number, bool, null) pass through unchanged
#[must_use]
pub fn redact_record(record: &Value) -> Value {
    match record {
        Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                if is_secret_field_name(k) {
                    out.insert(k.clone(), Value::String(REDACTED_EVENT_VALUE.to_string()));
                    continue;
                }
                if let Value::String(s) = v {
                    if is_jwt_like(s) {
                        out.insert(k.clone(), Value::String(REDACTED_EVENT_VALUE.to_string()));
                        continue;
                    }
                }
                out.insert(k.clone(), redact_record(v));
            }
            Value::Object(out)
        }
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(redact_record(item));
            }
            Value::Array(out)
        }
        other => other.clone(),
    }
}

/// True if `arg` is a CLI flag whose name looks like a secret (e.g. `--api-key`).
/// Used by command-argv sanitization.
#[must_use]
pub fn is_cli_secret_flag(arg: &str) -> bool {
    CLI_SECRET_FLAG_PATTERN.is_match(arg.trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn r527_redacted_event_value_matches_node() {
        assert_eq!(REDACTED_EVENT_VALUE, "***REDACTED***");
    }

    #[test]
    fn r527_secret_text_hints_has_18_entries() {
        // Mirrors Node upstream 18-item list — guard against silent drift.
        assert_eq!(SECRET_TEXT_HINTS.len(), 18);
    }

    #[test]
    fn r527_is_secret_field_name_recognises_common_names() {
        assert!(is_secret_field_name("apiKey"));
        assert!(is_secret_field_name("API_KEY"));
        assert!(is_secret_field_name("api-key"));
        assert!(is_secret_field_name("accessToken"));
        assert!(is_secret_field_name("authToken"));
        assert!(is_secret_field_name("authorization"));
        assert!(is_secret_field_name("bearer"));
        assert!(is_secret_field_name("password"));
        assert!(is_secret_field_name("passwd"));
        assert!(is_secret_field_name("jwtSecret"));
        assert!(is_secret_field_name("privateKey"));
        assert!(is_secret_field_name("connectionString"));
    }

    #[test]
    fn r527_is_secret_field_name_rejects_safe_names() {
        assert!(!is_secret_field_name("name"));
        assert!(!is_secret_field_name("id"));
        assert!(!is_secret_field_name("userId"));
        assert!(!is_secret_field_name("createdAt"));
        assert!(!is_secret_field_name(""));
    }

    #[test]
    fn r527_is_jwt_like_recognises_jwt_shape() {
        assert!(is_jwt_like("eyJhbGc.eyJzdWI.SflKxw"));
        assert!(is_jwt_like(
            "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c"
        ));
        // Optional 4th segment (nested JWS)
        assert!(is_jwt_like("a.b.c.d"));
    }

    #[test]
    fn r527_is_jwt_like_rejects_non_jwt_strings() {
        assert!(!is_jwt_like(""));
        assert!(!is_jwt_like("not-a-jwt"));
        assert!(!is_jwt_like("only.two"));
        // 5+ segments rejected
        assert!(!is_jwt_like("a.b.c.d.e"));
        // Spaces not allowed
        assert!(!is_jwt_like("eyJ a.b.c"));
    }

    #[test]
    fn r527_maybe_contains_secret_text_heuristic() {
        assert!(maybe_contains_secret_text("api_key=abc"));
        assert!(maybe_contains_secret_text("Authorization: Bearer xyz"));
        assert!(maybe_contains_secret_text("hello.world"));
        assert!(maybe_contains_secret_text("sk-1234567890"));
        assert!(maybe_contains_secret_text("ghp_abcdef"));
        assert!(!maybe_contains_secret_text("just a normal log line"));
        assert!(!maybe_contains_secret_text("user_id=42"));
    }

    #[test]
    fn r527_redact_sensitive_text_no_match_returns_input() {
        assert_eq!(redact_sensitive_text("hello world"), "hello world");
    }

    #[test]
    fn r527_redact_sensitive_text_replaces_inline_json_secret() {
        let input = r#"log: {"apiKey": "abc123", "name": "alice"}"#;
        let out = redact_sensitive_text(input);
        assert!(out.contains("***REDACTED***"), "got: {out}");
        assert!(out.contains(r#""name": "alice""#), "got: {out}");
        assert!(!out.contains("abc123"), "secret leaked: {out}");
    }

    #[test]
    fn r527_redact_sensitive_text_handles_multiple_fields() {
        let input = r#"{"token": "xxx", "password": "yyy", "safe": "zzz"}"#;
        let out = redact_sensitive_text(input);
        assert!(out.contains("***REDACTED***"), "got: {out}");
        assert!(out.contains(r#""safe": "zzz""#), "got: {out}");
        assert!(!out.contains(r#""xxx""#), "token leaked: {out}");
        assert!(!out.contains(r#""yyy""#), "password leaked: {out}");
    }

    #[test]
    fn r527_redact_sensitive_text_handles_uppercase_field_names() {
        // Case-insensitive: "API_KEY" should also be redacted.
        let input = r#"{"API_KEY": "abc"}"#;
        let out = redact_sensitive_text(input);
        assert!(out.contains("***REDACTED***"), "got: {out}");
        assert!(!out.contains("abc"), "got: {out}");
    }

    #[test]
    fn r527_redact_sensitive_text_handles_escaped_json() {
        // \"apiKey\": \"abc123\" form
        let input = r#"log: {\"apiKey\": \"abc123\", \"name\": \"alice\"}"#;
        let out = redact_sensitive_text(input);
        assert!(out.contains("***REDACTED***"), "got: {out}");
        assert!(!out.contains("abc123"), "got: {out}");
    }

    #[test]
    fn r527_redact_sensitive_text_redacts_authorization_bearer() {
        let input = "Authorization: Bearer abcdef123456789";
        let out = redact_sensitive_text(input);
        assert!(out.contains("***REDACTED***"), "got: {out}");
        assert!(!out.contains("abcdef123456789"), "got: {out}");
    }

    #[test]
    fn r527_redact_sensitive_text_redacts_openai_keys() {
        let input = "model=gpt-4 key=sk-abcdefghijklmnopqrstuvwxyz";
        let out = redact_sensitive_text(input);
        assert!(out.contains("***REDACTED***"), "got: {out}");
        assert!(!out.contains("sk-abcdefghijklmnopqrstuvwxyz"), "got: {out}");
    }

    #[test]
    fn r527_redact_sensitive_text_redacts_github_tokens() {
        let input = "GITHUB_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz";
        let out = redact_sensitive_text(input);
        assert!(out.contains("***REDACTED***"), "got: {out}");
        assert!(!out.contains("ghp_abcdefghijklmnopqrstuvwxyz"), "got: {out}");
    }

    #[test]
    fn r527_redact_sensitive_text_redacts_inline_jwt() {
        let input = "see token eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c for details";
        let out = redact_sensitive_text(input);
        assert!(out.contains("***REDACTED***"), "got: {out}");
    }

    #[test]
    fn r527_redact_record_replaces_secret_field_values() {
        let v = json!({
            "apiKey": "secret",
            "name": "alice",
            "nested": {
                "password": "secret",
                "id": 42
            }
        });
        let out = redact_record(&v);
        assert_eq!(out["apiKey"], "***REDACTED***");
        assert_eq!(out["name"], "alice");
        assert_eq!(out["nested"]["password"], "***REDACTED***");
        assert_eq!(out["nested"]["id"], 42);
    }

    #[test]
    fn r527_redact_record_replaces_jwt_string_values() {
        let v = json!({
            "session": "eyJhbGc.eyJzdWI.SflKxw",
            "user": "alice"
        });
        let out = redact_record(&v);
        assert_eq!(out["session"], "***REDACTED***");
        assert_eq!(out["user"], "alice");
    }

    #[test]
    fn r527_redact_record_handles_arrays() {
        let v = json!([
            {"apiKey": "secret"},
            {"token": "xxx", "name": "bob"}
        ]);
        let out = redact_record(&v);
        assert_eq!(out[0]["apiKey"], "***REDACTED***");
        assert_eq!(out[1]["token"], "***REDACTED***");
        assert_eq!(out[1]["name"], "bob");
    }

    #[test]
    fn r527_redact_record_preserves_primitives() {
        let v = json!({"n": 42, "b": true, "z": null, "arr": [1, 2, 3]});
        let out = redact_record(&v);
        assert_eq!(out, v);
    }

    #[test]
    fn r527_redact_record_handles_empty_inputs() {
        assert_eq!(redact_record(&json!({})), json!({}));
        assert_eq!(redact_record(&json!([])), json!([]));
        assert_eq!(redact_record(&Value::Null), Value::Null);
    }

    #[test]
    fn r527_is_cli_secret_flag_recognises_long_flags() {
        assert!(is_cli_secret_flag("--api-key"));
        assert!(is_cli_secret_flag("--api_key"));
        assert!(is_cli_secret_flag("--token"));
        assert!(is_cli_secret_flag("--password"));
        assert!(is_cli_secret_flag("--secret"));
        assert!(is_cli_secret_flag("--private-key"));
    }

    #[test]
    fn r527_is_cli_secret_flag_recognises_short_flags() {
        assert!(is_cli_secret_flag("-token"));
        assert!(is_cli_secret_flag("-password"));
    }

    #[test]
    fn r527_is_cli_secret_flag_rejects_safe_flags() {
        assert!(!is_cli_secret_flag("--help"));
        assert!(!is_cli_secret_flag("--verbose"));
        assert!(!is_cli_secret_flag("--output"));
        assert!(!is_cli_secret_flag("-h"));
    }

    #[test]
    fn r527_is_cli_secret_flag_case_insensitive() {
        assert!(is_cli_secret_flag("--API-KEY"));
        assert!(is_cli_secret_flag("--Token"));
        assert!(is_cli_secret_flag("--SECRET"));
    }
}
