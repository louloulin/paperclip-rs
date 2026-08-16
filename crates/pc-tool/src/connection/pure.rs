#![forbid(unsafe_code)]

//! Tool connection 业务层的 zero-DB pure helpers。
//!
//! 对应 Node paperclip/server/src/services/tool-access.ts 与
//! tool-gateway.ts 中跟 DB 解耦的纯函数部分（validation / normalization）。

use serde_json::Value;

/// Tool connection 名称最大长度（对齐 Node shared types）。
pub const TOOL_CONNECTION_NAME_MAX_LEN: usize = 128;

/// Tool connection 合法 status（与 Node tool-status union 对齐）。
pub const ALLOWED_CONNECTION_STATUSES: &[&str] = &[
    "active",
    "paused",
    "error",
    "reconnecting",
    "disabled",
];

/// 校验 connection name：trim + 非空 + 长度上限。
pub fn validate_connection_name(name: &str) -> Result<(), String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("name must not be empty".into());
    }
    if trimmed.chars().count() > TOOL_CONNECTION_NAME_MAX_LEN {
        return Err(format!(
            "name must be at most {} characters",
            TOOL_CONNECTION_NAME_MAX_LEN
        ));
    }
    Ok(())
}

/// 校验 status：trim + 非空 + 在 ALLOWED_CONNECTION_STATUSES 中。
pub fn validate_connection_status(status: &str) -> Result<(), String> {
    let trimmed = status.trim();
    if trimmed.is_empty() {
        return Err("status must not be empty".into());
    }
    if !ALLOWED_CONNECTION_STATUSES.contains(&trimmed) {
        return Err(format!(
            "status must be one of {:?}, got {:?}",
            ALLOWED_CONNECTION_STATUSES, trimmed
        ));
    }
    Ok(())
}

/// 校验 config 是 JSON object。
pub fn validate_config_object(config: &Value) -> Result<(), String> {
    if !config.is_object() {
        return Err("config must be an object".into());
    }
    Ok(())
}

/// 校验 credential refs 是 JSON array（可为空数组）。
pub fn validate_credential_refs(refs: &Value) -> Result<(), String> {
    if !refs.is_array() {
        return Err("credential refs must be an array".into());
    }
    Ok(())
}

/// 规范化 status：小写 + trim。
pub fn normalize_status(status: &str) -> String {
    status.trim().to_lowercase()
}

/// 判断两个 connection name 是否等价（trim + case-insensitive）。
pub fn connection_name_eq(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

#[cfg(test)]
mod internal_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validate_connection_name_accepts_normal() {
        assert!(validate_connection_name("GitHub").is_ok());
    }

    #[test]
    fn validate_connection_name_rejects_empty() {
        assert!(validate_connection_name("").is_err());
        assert!(validate_connection_name("   ").is_err());
    }

    #[test]
    fn validate_connection_name_rejects_too_long() {
        let long = "a".repeat(TOOL_CONNECTION_NAME_MAX_LEN + 1);
        assert!(validate_connection_name(&long).is_err());
    }

    #[test]
    fn validate_connection_status_accepts_known() {
        for s in ALLOWED_CONNECTION_STATUSES {
            assert!(validate_connection_status(s).is_ok(), "{s}");
        }
    }

    #[test]
    fn validate_connection_status_rejects_unknown() {
        assert!(validate_connection_status("borked").is_err());
    }

    #[test]
    fn validate_connection_status_rejects_empty() {
        assert!(validate_connection_status("").is_err());
    }

    #[test]
    fn validate_config_object_accepts_object() {
        assert!(validate_config_object(&json!({"k": 1})).is_ok());
    }

    #[test]
    fn validate_config_object_rejects_array() {
        assert!(validate_config_object(&json!([1, 2])).is_err());
    }

    #[test]
    fn validate_config_object_rejects_string() {
        assert!(validate_config_object(&json!("hi")).is_err());
    }

    #[test]
    fn validate_credential_refs_accepts_empty_array() {
        assert!(validate_credential_refs(&json!([])).is_ok());
    }

    #[test]
    fn validate_credential_refs_accepts_non_empty_array() {
        assert!(validate_credential_refs(&json!(["ref1"])).is_ok());
    }

    #[test]
    fn validate_credential_refs_rejects_object() {
        assert!(validate_credential_refs(&json!({})).is_err());
    }

    #[test]
    fn normalize_status_lowercases_and_trims() {
        assert_eq!(normalize_status("  ACTIVE "), "active");
    }

    #[test]
    fn normalize_status_preserves_unknown() {
        // normalize 不校验，仅大小写规范化
        assert_eq!(normalize_status("CUSTOM"), "custom");
    }

    #[test]
    fn connection_name_eq_case_insensitive() {
        assert!(connection_name_eq("GitHub", "github"));
        assert!(connection_name_eq("  GitHub  ", "GITHUB"));
    }

    #[test]
    fn connection_name_eq_different_values() {
        assert!(!connection_name_eq("GitHub", "GitLab"));
    }
}
