#![forbid(unsafe_code)]

//! Redaction of environment custom-image values + templates + setup sessions.
//!
//! R535: Direct port of `paperclip/packages/shared/src/environment-custom-images.ts`.
//!
//! 设计原则:
//! - 所有 `pub fn` 都是纯函数 (无 IO, 无副作用, 无环境依赖)
//! - regex 编译成 `Lazy<Regex>` 一次, 后续零成本
//! - 输入/输出用 `serde_json::Value` (镜像上游 `unknown` / `Record<string, unknown>`)
//! - 不引入业务 crate 依赖 (零耦合)
//!
//! 范围 (本 crate):
//! - [`REDACTED_ENVIRONMENT_CUSTOM_IMAGE_VALUE`] 常量
//! - [`redact_environment_custom_image_value`] — 递归 redact JSON Value
//!   (含敏感 key、IPv4、ssh 命令)
//! - [`redact_environment_custom_image_template`] — redact template (templateRef /
//!   sourceTemplateRef / metadata)
//! - [`redact_environment_custom_image_setup_session`] — redact setup session
//!   (providerLeaseId / baseTemplateRef / connectionSecretRef /
//!   connectionSummary 含 username / metadata)
//! - [`EnvironmentCustomImageTemplateRedactionInput`] /
//!   [`EnvironmentCustomImageSetupSessionRedactionInput`] 数据结构
//!
//! **不** 范围 (留给集成层):
//! - DB 持久化 (`server/src/services/environments.ts`)
//! - UI 渲染 (`ui/src/lib/environment-custom-image.ts`)
//!
//! 设计 vs Node 上游:
//! - 接受 `&serde_json::Value` 而非 `unknown` — 类型安全, 强制 JSON-shaped input
//! - 12 个敏感 key regex 用 `Lazy<Regex>` — 一次性编译
//! - `Redacted` 后缀 key 永远不被 redact (上游特例, 保留)
//! - IPv4 + ssh 命令两种 primitive-level 触发, 与上游一致

use regex::Regex;
use serde_json::{Map, Value};
use std::sync::LazyLock as Lazy;

// ============================================================================
// Constants
// ============================================================================

/// Replacement string used for all redacted values.
///
/// Mirrors Node `REDACTED_ENVIRONMENT_CUSTOM_IMAGE_VALUE`.
pub const REDACTED_ENVIRONMENT_CUSTOM_IMAGE_VALUE: &str = "[redacted]";

// ============================================================================
// Sensitive key patterns (mirror Node SENSITIVE_KEY_PATTERNS)
// ============================================================================

/// Individual sensitive-key patterns (case-insensitive).
///
/// Each pattern mirrors one entry of Node `SENSITIVE_KEY_PATTERNS`. They are
/// declared as separate statics (not an array of `Lazy<Regex>`) because
/// `Lazy` is not `const`-constructible and a `static` of `Lazy<Regex>` items
/// cannot reference temporary values.
static SENSITIVE_KEY_AUTH: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)auth").expect("valid regex pattern"));
static SENSITIVE_KEY_CREDENTIAL: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)credential").expect("valid regex pattern"));
static SENSITIVE_KEY_HOST: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)host").expect("valid regex pattern"));
static SENSITIVE_KEY_IP: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)ip").expect("valid regex pattern"));
static SENSITIVE_KEY_KEY_EXACT: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)^key$").expect("valid regex pattern"));
static SENSITIVE_KEY_LEASE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)lease").expect("valid regex pattern"));
static SENSITIVE_KEY_PASSWORD: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)password").expect("valid regex pattern"));
static SENSITIVE_KEY_PRIVATE_KEY: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)private.?key").expect("valid regex pattern"));
static SENSITIVE_KEY_SANDBOX_ID: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)sandbox.?id").expect("valid regex pattern"));
static SENSITIVE_KEY_SECRET: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)secret").expect("valid regex pattern"));
static SENSITIVE_KEY_TEMPLATE_REF: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)template.?ref").expect("valid regex pattern"));
static SENSITIVE_KEY_TOKEN: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)token").expect("valid regex pattern"));
static SENSITIVE_KEY_URL: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)url").expect("valid regex pattern"));

/// IPv4 address literal pattern.
static IPV4_PATTERN: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\b(?:\d{1,3}\.){3}\d{1,3}\b").expect("valid regex pattern"));

/// ssh command invocation pattern.
static SSH_COMMAND_PATTERN: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\bssh\s+[-\w@.:/]+\b").expect("valid regex pattern"));

// ============================================================================
// Helpers
// ============================================================================

/// Returns `true` if the given object key should trigger a redaction.
///
/// - Keys ending in `Redacted` are NEVER redacted (mirror upstream behavior —
///   these are already-redacted markers and re-redacting them would be a no-op
///   but breaks symmetry with upstream)
/// - Otherwise: `true` if any pattern in [`SENSITIVE_KEY_PATTERNS`] matches
#[inline]
#[must_use]
pub fn is_sensitive_key(key: &str) -> bool {
    if key.ends_with("Redacted") {
        return false;
    }
    SENSITIVE_KEY_AUTH.is_match(key)
        || SENSITIVE_KEY_CREDENTIAL.is_match(key)
        || SENSITIVE_KEY_HOST.is_match(key)
        || SENSITIVE_KEY_IP.is_match(key)
        || SENSITIVE_KEY_KEY_EXACT.is_match(key)
        || SENSITIVE_KEY_LEASE.is_match(key)
        || SENSITIVE_KEY_PASSWORD.is_match(key)
        || SENSITIVE_KEY_PRIVATE_KEY.is_match(key)
        || SENSITIVE_KEY_SANDBOX_ID.is_match(key)
        || SENSITIVE_KEY_SECRET.is_match(key)
        || SENSITIVE_KEY_TEMPLATE_REF.is_match(key)
        || SENSITIVE_KEY_TOKEN.is_match(key)
        || SENSITIVE_KEY_URL.is_match(key)
}

/// Returns `true` if the primitive value contains an IPv4 literal or an
/// `ssh ...` command invocation.
#[inline]
#[must_use]
pub fn is_sensitive_primitive_string(value: &str) -> bool {
    IPV4_PATTERN.is_match(value) || SSH_COMMAND_PATTERN.is_match(value)
}

fn redact_sensitive_primitive(value: &Value) -> Value {
    match value {
        Value::String(s) => {
            if is_sensitive_primitive_string(s) {
                Value::String(REDACTED_ENVIRONMENT_CUSTOM_IMAGE_VALUE.to_owned())
            } else {
                value.clone()
            }
        }
        // Booleans, numbers, and null pass through unchanged.
        _ => value.clone(),
    }
}

// ============================================================================
// Core recursive redaction
// ============================================================================

/// Recursively redact environment custom-image values.
///
/// Rules (mirror Node `redactEnvironmentCustomImageValue`):
/// - `Array` → recurse into each element
/// - `Object` (non-array) → for each entry:
///   - if key matches sensitive pattern → replace value with `REDACTED_…`
///   - else → recurse into value
/// - `String` → if contains IPv4 or ssh command, replace with `REDACTED_…`
/// - Other primitives (`Null`, `Bool`, `Number`) → pass through
#[must_use]
pub fn redact_environment_custom_image_value(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(redact_environment_custom_image_value)
                .collect(),
        ),
        Value::Object(map) => {
            let mut out = Map::with_capacity(map.len());
            for (key, entry) in map {
                if is_sensitive_key(key) {
                    out.insert(
                        key.clone(),
                        Value::String(REDACTED_ENVIRONMENT_CUSTOM_IMAGE_VALUE.to_owned()),
                    );
                } else {
                    out.insert(key.clone(), redact_environment_custom_image_value(entry));
                }
            }
            Value::Object(out)
        }
        Value::String(_) => redact_sensitive_primitive(value),
        Value::Null | Value::Bool(_) | Value::Number(_) => value.clone(),
    }
}

// ============================================================================
// Template redaction
// ============================================================================

/// Input shape for [`redact_environment_custom_image_template`].
///
/// Mirrors Node `EnvironmentCustomImageTemplateRedactionInput`.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentCustomImageTemplateRedactionInput {
    /// Free-form metadata object; recursively redacted.
    #[serde(default)]
    pub template_ref: Option<String>,
    #[serde(default)]
    pub source_template_ref: Option<String>,
    #[serde(default)]
    pub metadata: Option<Map<String, Value>>,
}

impl EnvironmentCustomImageTemplateRedactionInput {
    /// Construct from individual optional fields.
    #[must_use]
    pub fn new(
        template_ref: Option<String>,
        source_template_ref: Option<String>,
        metadata: Option<Map<String, Value>>,
    ) -> Self {
        Self {
            template_ref,
            source_template_ref,
            metadata,
        }
    }
}

/// Redact an environment custom-image template.
///
/// - `templateRef` / `sourceTemplateRef`: non-null → replace with
///   `REDACTED_…`; null / missing → preserve as-is (including `None`)
/// - `metadata`: non-null → recursively redact; null / missing → preserve
///
/// Mirrors Node `redactEnvironmentCustomImageTemplate`.
#[must_use]
pub fn redact_environment_custom_image_template<T>(template: &T) -> serde_json::Value
where
    T: serde::Serialize,
{
    // We need to inspect each field individually for null-handling, so we
    // serialize first then walk the resulting JSON object.
    let value = serde_json::to_value(template).unwrap_or(Value::Null);
    let Value::Object(mut map) = value else {
        return value;
    };

    for field in ["templateRef", "sourceTemplateRef"] {
        if let Some(entry) = map.get_mut(field) {
            if !entry.is_null() {
                *entry = Value::String(REDACTED_ENVIRONMENT_CUSTOM_IMAGE_VALUE.to_owned());
            }
        }
    }

    if let Some(meta_entry) = map.get_mut("metadata") {
        if !meta_entry.is_null() {
            let redacted = redact_environment_custom_image_value(meta_entry);
            *meta_entry = redacted;
        }
    }

    Value::Object(map)
}

// ============================================================================
// Setup session redaction
// ============================================================================

/// Input shape for [`redact_environment_custom_image_setup_session`].
///
/// Mirrors Node `EnvironmentCustomImageSetupSessionRedactionInput`.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentCustomImageSetupSessionRedactionInput {
    #[serde(default)]
    pub provider_lease_id: Option<String>,
    #[serde(default)]
    pub base_template_ref: Option<String>,
    #[serde(default)]
    pub connection_secret_ref: Option<String>,
    /// Per-protocol connection summary object; recursively redacted +
    /// `username` always replaced with `REDACTED_…`.
    #[serde(default)]
    pub connection_summary: Option<Map<String, Value>>,
    /// Free-form metadata object; recursively redacted.
    #[serde(default)]
    pub metadata: Option<Map<String, Value>>,
}

impl EnvironmentCustomImageSetupSessionRedactionInput {
    /// Construct from individual optional fields.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider_lease_id: Option<String>,
        base_template_ref: Option<String>,
        connection_secret_ref: Option<String>,
        connection_summary: Option<Map<String, Value>>,
        metadata: Option<Map<String, Value>>,
    ) -> Self {
        Self {
            provider_lease_id,
            base_template_ref,
            connection_secret_ref,
            connection_summary,
            metadata,
        }
    }
}

/// Redact an environment custom-image setup session.
///
/// - `providerLeaseId` / `baseTemplateRef` / `connectionSecretRef`:
///   non-null → replace with `REDACTED_…`; null / missing → preserve
/// - `connectionSummary`: non-null → recursively redact + always replace
///   `username` with `REDACTED_…` (regardless of whether username was sensitive)
/// - `metadata`: non-null → recursively redact; null / missing → preserve
///
/// Mirrors Node `redactEnvironmentCustomImageSetupSession`.
#[must_use]
pub fn redact_environment_custom_image_setup_session<T>(session: &T) -> serde_json::Value
where
    T: serde::Serialize,
{
    let value = serde_json::to_value(session).unwrap_or(Value::Null);
    let Value::Object(mut map) = value else {
        return value;
    };

    for field in ["providerLeaseId", "baseTemplateRef", "connectionSecretRef"] {
        if let Some(entry) = map.get_mut(field) {
            if !entry.is_null() {
                *entry = Value::String(REDACTED_ENVIRONMENT_CUSTOM_IMAGE_VALUE.to_owned());
            }
        }
    }

    if let Some(summary) = map.get_mut("connectionSummary") {
        if !summary.is_null() {
            let mut redacted_summary = redact_environment_custom_image_value(summary);
            // Per upstream: username is ALWAYS replaced with REDACTED, even if
            // it didn't match a sensitive pattern (the user is not the
            // sensitive element; the connection itself is).
            if let Value::Object(ref mut obj) = redacted_summary {
                if obj.contains_key("username") {
                    obj.insert(
                        "username".to_owned(),
                        Value::String(REDACTED_ENVIRONMENT_CUSTOM_IMAGE_VALUE.to_owned()),
                    );
                }
            }
            *summary = redacted_summary;
        }
    }

    if let Some(meta_entry) = map.get_mut("metadata") {
        if !meta_entry.is_null() {
            let redacted = redact_environment_custom_image_value(meta_entry);
            *meta_entry = redacted;
        }
    }

    Value::Object(map)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn r535_sensitive_key_basic() {
        // Note: `apiKey` does NOT match `/^key$/` (only literal "key" does)
        // and `description` DOES match `/ip/i` — both confirmed by upstream.
        assert!(is_sensitive_key("auth"));
        assert!(is_sensitive_key("Authorization"));
        assert!(is_sensitive_key("apiToken"));
        assert!(is_sensitive_key("api_token"));
        assert!(is_sensitive_key("host"));
        assert!(is_sensitive_key("password"));
        assert!(is_sensitive_key("token"));
        assert!(is_sensitive_key("url"));
        assert!(is_sensitive_key("templateRef"));
        assert!(is_sensitive_key("template_ref"));
        assert!(is_sensitive_key("privateKey"));
        assert!(is_sensitive_key("connectionSecretRef"));
    }

    #[test]
    fn r535_sensitive_key_exact_key_pattern() {
        // `/^key$/i` only matches the literal key, not `keyX` or `myKey`.
        assert!(is_sensitive_key("key"));
        assert!(is_sensitive_key("Key"));
        assert!(is_sensitive_key("KEY"));
        assert!(!is_sensitive_key("keyword"));
        assert!(!is_sensitive_key("myKey"));
    }

    #[test]
    fn r535_sensitive_key_redacted_suffix_excluded() {
        // Keys ending in `Redacted` are NEVER redacted — they are already
        // redacted markers (mirror upstream).
        assert!(!is_sensitive_key("tokenRedacted"));
        assert!(!is_sensitive_key("secretRedacted"));
        assert!(!is_sensitive_key("hostRedacted"));
        assert!(!is_sensitive_key("authRedacted"));
    }

    #[test]
    fn r535_sensitive_key_non_sensitive() {
        // Note: `description` DOES match `/ip/i` ("descript**ip**tion").
        // Verified upstream.
        assert!(!is_sensitive_key("name"));
        assert!(!is_sensitive_key("id"));
        assert!(!is_sensitive_key("kind"));
        assert!(!is_sensitive_key(""));
        assert!(!is_sensitive_key("color"));
        assert!(!is_sensitive_key("label"));
        assert!(!is_sensitive_key("type"));
    }

    #[test]
    fn r535_sensitive_primitive_ipv4() {
        assert!(is_sensitive_primitive_string("host 10.0.0.1"));
        assert!(is_sensitive_primitive_string("192.168.1.255"));
        assert!(is_sensitive_primitive_string("server at 172.16.0.42"));
    }

    #[test]
    fn r535_sensitive_primitive_ssh_command() {
        assert!(is_sensitive_primitive_string("ssh user@host"));
        assert!(is_sensitive_primitive_string("do: ssh -p 22 root@1.2.3.4"));
        assert!(is_sensitive_primitive_string("ssh deploy@server.local"));
    }

    #[test]
    fn r535_sensitive_primitive_clean_strings() {
        assert!(!is_sensitive_primitive_string("hello world"));
        assert!(!is_sensitive_primitive_string("name: foo"));
        assert!(!is_sensitive_primitive_string(""));
        // Numbers without dots aren't IPs
        assert!(!is_sensitive_primitive_string("count: 42"));
    }

    #[test]
    fn r535_redact_value_passthrough_primitives() {
        assert_eq!(
            redact_environment_custom_image_value(&json!(null)),
            json!(null)
        );
        assert_eq!(
            redact_environment_custom_image_value(&json!(true)),
            json!(true)
        );
        assert_eq!(redact_environment_custom_image_value(&json!(42)), json!(42));
        assert_eq!(
            redact_environment_custom_image_value(&json!("hello")),
            json!("hello")
        );
    }

    #[test]
    fn r535_redact_value_string_with_ipv4_redacted() {
        let input = json!("host 10.0.0.1");
        let expected = json!(REDACTED_ENVIRONMENT_CUSTOM_IMAGE_VALUE);
        assert_eq!(redact_environment_custom_image_value(&input), expected);
    }

    #[test]
    fn r535_redact_value_string_with_ssh_redacted() {
        let input = json!("run: ssh user@host");
        let expected = json!(REDACTED_ENVIRONMENT_CUSTOM_IMAGE_VALUE);
        assert_eq!(redact_environment_custom_image_value(&input), expected);
    }

    #[test]
    fn r535_redact_value_object_sensitive_key_replaced() {
        let input = json!({"auth": "basic", "name": "alice"});
        let expected = json!({
            "auth": REDACTED_ENVIRONMENT_CUSTOM_IMAGE_VALUE,
            "name": "alice",
        });
        assert_eq!(redact_environment_custom_image_value(&input), expected);
    }

    #[test]
    fn r535_redact_value_nested_object_recursive() {
        let input = json!({
            "config": {
                "host": "10.0.0.1",
                "port": 5432,
                "password": "secret"
            },
            "name": "db",
        });
        let expected = json!({
            "config": {
                "host": REDACTED_ENVIRONMENT_CUSTOM_IMAGE_VALUE,
                "port": 5432,
                "password": REDACTED_ENVIRONMENT_CUSTOM_IMAGE_VALUE,
            },
            "name": "db",
        });
        assert_eq!(redact_environment_custom_image_value(&input), expected);
    }

    #[test]
    fn r535_redact_value_array_recursive() {
        let input = json!([
            {"host": "10.0.0.1"},
            "ssh user@host",
            "plain string",
        ]);
        let expected = json!([
            {"host": REDACTED_ENVIRONMENT_CUSTOM_IMAGE_VALUE},
            REDACTED_ENVIRONMENT_CUSTOM_IMAGE_VALUE,
            "plain string",
        ]);
        assert_eq!(redact_environment_custom_image_value(&input), expected);
    }

    #[test]
    fn r535_redact_value_redacted_suffix_key_not_redacted() {
        // `hostRedacted` is the marker; key-based redaction skips it.
        // But the VALUE is still subject to primitive-level redaction if
        // it contains IPv4 / ssh.
        let input = json!({"hostRedacted": "marker-value"});
        let expected = json!({"hostRedacted": "marker-value"});
        assert_eq!(redact_environment_custom_image_value(&input), expected);
    }

    #[test]
    fn r535_redact_value_redacted_suffix_key_but_value_redacted() {
        // Key is NOT sensitive (Redacted suffix), but the VALUE contains
        // an IPv4 — primitive-level redaction triggers.
        let input = json!({"hostRedacted": "10.0.0.1"});
        let expected = json!({"hostRedacted": REDACTED_ENVIRONMENT_CUSTOM_IMAGE_VALUE});
        assert_eq!(redact_environment_custom_image_value(&input), expected);
    }

    #[test]
    fn r535_redact_template_all_fields_redacted() {
        let template = EnvironmentCustomImageTemplateRedactionInput::new(
            Some("template-abc".to_owned()),
            Some("source-xyz".to_owned()),
            Some(Map::from_iter([(
                "host".to_owned(),
                Value::String("10.0.0.1".to_owned()),
            )])),
        );
        let result = redact_environment_custom_image_template(&template);
        let map = result.as_object().unwrap();
        assert_eq!(
            map.get("templateRef").unwrap(),
            &json!(REDACTED_ENVIRONMENT_CUSTOM_IMAGE_VALUE)
        );
        assert_eq!(
            map.get("sourceTemplateRef").unwrap(),
            &json!(REDACTED_ENVIRONMENT_CUSTOM_IMAGE_VALUE)
        );
        assert_eq!(
            map.get("metadata").unwrap().get("host").unwrap(),
            &json!(REDACTED_ENVIRONMENT_CUSTOM_IMAGE_VALUE)
        );
    }

    #[test]
    fn r535_redact_template_nulls_preserved() {
        let template = EnvironmentCustomImageTemplateRedactionInput::new(None, None, None);
        let result = redact_environment_custom_image_template(&template);
        let map = result.as_object().unwrap();
        assert!(map.get("templateRef").is_some());
        assert!(map["templateRef"].is_null());
        assert!(map["sourceTemplateRef"].is_null());
        assert!(map["metadata"].is_null());
    }

    #[test]
    fn r535_redact_template_partial_nulls() {
        let template = EnvironmentCustomImageTemplateRedactionInput::new(
            Some("template-abc".to_owned()),
            None,
            None,
        );
        let result = redact_environment_custom_image_template(&template);
        let map = result.as_object().unwrap();
        assert_eq!(
            map.get("templateRef").unwrap(),
            &json!(REDACTED_ENVIRONMENT_CUSTOM_IMAGE_VALUE)
        );
        assert!(map["sourceTemplateRef"].is_null());
        assert!(map["metadata"].is_null());
    }

    #[test]
    fn r535_redact_template_metadata_nested() {
        let template = EnvironmentCustomImageTemplateRedactionInput::new(
            None,
            None,
            Some(Map::from_iter([(
                "config".to_owned(),
                json!({"ssh": "ssh user@1.2.3.4", "plain": "ok"}),
            )])),
        );
        let result = redact_environment_custom_image_template(&template);
        let map = result.as_object().unwrap();
        let metadata = map.get("metadata").unwrap();
        let config = metadata.get("config").unwrap();
        assert_eq!(
            config.get("ssh").unwrap(),
            &json!(REDACTED_ENVIRONMENT_CUSTOM_IMAGE_VALUE)
        );
        assert_eq!(config.get("plain").unwrap(), &json!("ok"));
    }

    #[test]
    fn r535_redact_setup_session_all_fields_redacted() {
        let session = EnvironmentCustomImageSetupSessionRedactionInput::new(
            Some("lease-123".to_owned()),
            Some("base-abc".to_owned()),
            Some("secret-ref".to_owned()),
            Some(Map::from_iter([(
                "host".to_owned(),
                Value::String("10.0.0.1".to_owned()),
            )])),
            Some(Map::from_iter([(
                "password".to_owned(),
                Value::String("p".to_owned()),
            )])),
        );
        let result = redact_environment_custom_image_setup_session(&session);
        let map = result.as_object().unwrap();
        assert_eq!(
            map.get("providerLeaseId").unwrap(),
            &json!(REDACTED_ENVIRONMENT_CUSTOM_IMAGE_VALUE)
        );
        assert_eq!(
            map.get("baseTemplateRef").unwrap(),
            &json!(REDACTED_ENVIRONMENT_CUSTOM_IMAGE_VALUE)
        );
        assert_eq!(
            map.get("connectionSecretRef").unwrap(),
            &json!(REDACTED_ENVIRONMENT_CUSTOM_IMAGE_VALUE)
        );
        assert_eq!(
            map.get("connectionSummary").unwrap().get("host").unwrap(),
            &json!(REDACTED_ENVIRONMENT_CUSTOM_IMAGE_VALUE)
        );
        assert_eq!(
            map.get("metadata").unwrap().get("password").unwrap(),
            &json!(REDACTED_ENVIRONMENT_CUSTOM_IMAGE_VALUE)
        );
    }

    #[test]
    fn r535_redact_setup_session_username_always_redacted() {
        // Even though `username` doesn't match any sensitive pattern, the
        // setup-session redaction ALWAYS replaces it.
        // Note: `host` IS sensitive — so use `kind` for the preserved key.
        let session = EnvironmentCustomImageSetupSessionRedactionInput::new(
            None,
            None,
            None,
            Some(Map::from_iter([
                ("kind".to_owned(), Value::String("ssh".to_owned())),
                ("username".to_owned(), Value::String("alice".to_owned())),
            ])),
            None,
        );
        let result = redact_environment_custom_image_setup_session(&session);
        let summary = result
            .as_object()
            .unwrap()
            .get("connectionSummary")
            .unwrap()
            .as_object()
            .unwrap();
        assert_eq!(
            summary.get("username").unwrap(),
            &json!(REDACTED_ENVIRONMENT_CUSTOM_IMAGE_VALUE)
        );
        // Non-sensitive, non-username key preserved.
        assert_eq!(summary.get("kind").unwrap(), &json!("ssh"));
    }

    #[test]
    fn r535_redact_setup_session_no_username_in_summary_passthrough() {
        let session = EnvironmentCustomImageSetupSessionRedactionInput::new(
            None,
            None,
            None,
            Some(Map::from_iter([(
                "kind".to_owned(),
                Value::String("ssh".to_owned()),
            )])),
            None,
        );
        let result = redact_environment_custom_image_setup_session(&session);
        let summary = result
            .as_object()
            .unwrap()
            .get("connectionSummary")
            .unwrap()
            .as_object()
            .unwrap();
        // No `username` key was present, so no fake username injected.
        assert!(summary.get("username").is_none());
        assert_eq!(summary.get("kind").unwrap(), &json!("ssh"));
    }

    #[test]
    fn r535_redact_setup_session_nulls_preserved() {
        let session =
            EnvironmentCustomImageSetupSessionRedactionInput::new(None, None, None, None, None);
        let result = redact_environment_custom_image_setup_session(&session);
        let map = result.as_object().unwrap();
        assert!(map["providerLeaseId"].is_null());
        assert!(map["baseTemplateRef"].is_null());
        assert!(map["connectionSecretRef"].is_null());
        assert!(map["connectionSummary"].is_null());
        assert!(map["metadata"].is_null());
    }

    #[test]
    fn r535_redact_setup_session_metadata_recursive() {
        let session = EnvironmentCustomImageSetupSessionRedactionInput::new(
            None,
            None,
            None,
            None,
            Some(Map::from_iter([(
                "deeply".to_owned(),
                json!({
                    "nested": {
                        "token": "abc123",
                        "name": "ok"
                    }
                }),
            )])),
        );
        let result = redact_environment_custom_image_setup_session(&session);
        let metadata = result
            .as_object()
            .unwrap()
            .get("metadata")
            .unwrap()
            .as_object()
            .unwrap();
        let nested = metadata
            .get("deeply")
            .unwrap()
            .as_object()
            .unwrap()
            .get("nested")
            .unwrap()
            .as_object()
            .unwrap();
        assert_eq!(
            nested.get("token").unwrap(),
            &json!(REDACTED_ENVIRONMENT_CUSTOM_IMAGE_VALUE)
        );
        assert_eq!(nested.get("name").unwrap(), &json!("ok"));
    }

    #[test]
    fn r535_template_struct_serializes_camel_case() {
        let template = EnvironmentCustomImageTemplateRedactionInput::new(
            Some("t".to_owned()),
            Some("s".to_owned()),
            Some(Map::new()),
        );
        let json_str = serde_json::to_string(&template).unwrap();
        assert!(json_str.contains("\"templateRef\""));
        assert!(json_str.contains("\"sourceTemplateRef\""));
        assert!(json_str.contains("\"metadata\""));
        // No snake_case leakage.
        assert!(!json_str.contains("template_ref"));
    }

    #[test]
    fn r535_setup_session_struct_serializes_camel_case() {
        let session = EnvironmentCustomImageSetupSessionRedactionInput::new(
            Some("l".to_owned()),
            Some("b".to_owned()),
            Some("c".to_owned()),
            Some(Map::new()),
            Some(Map::new()),
        );
        let json_str = serde_json::to_string(&session).unwrap();
        assert!(json_str.contains("\"providerLeaseId\""));
        assert!(json_str.contains("\"baseTemplateRef\""));
        assert!(json_str.contains("\"connectionSecretRef\""));
        assert!(json_str.contains("\"connectionSummary\""));
        assert!(!json_str.contains("provider_lease_id"));
    }

    #[test]
    fn r535_redacted_constant_value() {
        assert_eq!(REDACTED_ENVIRONMENT_CUSTOM_IMAGE_VALUE, "[redacted]");
    }

    #[test]
    fn r535_redact_value_mixed_structure() {
        // Realistic fixture: full env-custom-image value with nested arrays
        // and multiple sensitive keys at various depths.
        let input = json!({
            "name": "ubuntu-template",
            "auth": {"token": "abc"},
            "config": {
                "sshCommand": "ssh root@10.0.0.1",
                "port": 22,
                "leaseId": "lease-xyz",
            },
            "entrypoint": [
                "echo hello",
                "ssh user@server",
            ],
            "hostRedacted": "marker-only",
        });
        let result = redact_environment_custom_image_value(&input);
        let map = result.as_object().unwrap();
        assert_eq!(map.get("name").unwrap(), &json!("ubuntu-template"));
        // `auth` is sensitive — entire value replaced (the inner object
        // structure is gone after redaction).
        assert_eq!(
            map.get("auth").unwrap(),
            &json!(REDACTED_ENVIRONMENT_CUSTOM_IMAGE_VALUE)
        );
        // `config` is NOT sensitive, recurse:
        let config = map.get("config").unwrap().as_object().unwrap();
        // `sshCommand` value contains ssh command → redacted
        assert_eq!(
            config.get("sshCommand").unwrap(),
            &json!(REDACTED_ENVIRONMENT_CUSTOM_IMAGE_VALUE)
        );
        // `leaseId` matches `lease` pattern → redacted
        assert_eq!(
            config.get("leaseId").unwrap(),
            &json!(REDACTED_ENVIRONMENT_CUSTOM_IMAGE_VALUE)
        );
        // `port` is not sensitive key, primitive number passes through
        assert_eq!(config.get("port").unwrap(), &json!(22));
        let entrypoint = map.get("entrypoint").unwrap().as_array().unwrap();
        assert_eq!(&entrypoint[0], &json!("echo hello"));
        assert_eq!(
            &entrypoint[1],
            &json!(REDACTED_ENVIRONMENT_CUSTOM_IMAGE_VALUE)
        );
        // hostRedacted not redacted as key (Redacted suffix), and value
        // contains no IPv4/ssh — preserved.
        assert_eq!(map.get("hostRedacted").unwrap(), &json!("marker-only"));
    }
}
