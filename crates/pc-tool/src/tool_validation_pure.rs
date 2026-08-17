#![forbid(unsafe_code)]

//! Tool application validation pure helpers — extracted from
//! `pc-tool/src/service.rs` create / patch / set_status validation to make
//! the policy rules independently testable.
//!
//! R747: 与 R744/R745/R746 同模式——核心校验拆为纯函数（不返回 pc_errors，
//! 调用方负责把 `Err(&'static str)` 升级为 `ToolError::Validation`）。
//!
//! 对齐 `paperclip/server/src/services/tool-access.ts` 中的工具创建/更新校验：
//! - name 非空
//! - kind 非空且属于允许的 kind 集合（mcp / api / cli / webhook）
//! - metadata 必须是 object
//! - status 非空

use serde_json::Value;

/// 工具 application 允许的 kind 集合。
///
/// 与 `pc_repos::tool::ToolApplicationType` 对齐：mcp / api / cli / webhook。
pub const ALLOWED_TOOL_KINDS: &[&str] = &["mcp", "api", "cli", "webhook"];

/// 工具 application 允许的 status 集合。
///
/// 与 `pc_repos::tool::ToolApplicationStatus` 对齐：active / disabled / draft。
pub const ALLOWED_TOOL_STATUSES: &[&str] = &["active", "disabled", "draft"];

pub fn is_tool_kind_allowed(value: &str) -> bool {
    ALLOWED_TOOL_KINDS.contains(&value)
}

pub fn is_tool_status_allowed(value: &str) -> bool {
    ALLOWED_TOOL_STATUSES.contains(&value)
}

/// 校验 name 非空（trim 后）。
pub fn validate_tool_name_non_empty(name: &str) -> Result<(), &'static str> {
    if name.trim().is_empty() {
        Err("name must not be empty")
    } else {
        Ok(())
    }
}

/// 校验 kind 非空且属于 ALLOWED_TOOL_KINDS。
pub fn validate_tool_kind(kind: &str) -> Result<(), &'static str> {
    let trimmed = kind.trim();
    if trimmed.is_empty() {
        return Err("kind must not be empty");
    }
    if !is_tool_kind_allowed(trimmed) {
        return Err("kind must be one of mcp/api/cli/webhook");
    }
    Ok(())
}

/// 校验 status 非空且属于 ALLOWED_TOOL_STATUSES。
pub fn validate_tool_status(status: &str) -> Result<(), &'static str> {
    let trimmed = status.trim();
    if trimmed.is_empty() {
        return Err("status must not be empty");
    }
    if !is_tool_status_allowed(trimmed) {
        return Err("status must be one of active/disabled/draft");
    }
    Ok(())
}

/// 校验 metadata 是 object 或为空（service 层允许传入空 object）。
pub fn validate_tool_metadata(metadata: &Value) -> Result<(), &'static str> {
    if metadata.is_null() {
        return Ok(()); // null 视作"未设置"
    }
    if !metadata.is_object() {
        return Err("metadata must be an object");
    }
    Ok(())
}

/// 校验 patch 中的 name（Some("")/Some("   ") 拒绝；None 不动）。
pub fn validate_tool_patch_name(
    name: Option<&str>,
) -> Result<(), &'static str> {
    match name {
        Some(s) => validate_tool_name_non_empty(s),
        None => Ok(()),
    }
}

/// 校验 patch 中的 description（Some("") 拒绝；None 不动）。
pub fn validate_tool_patch_description(
    description: Option<&str>,
) -> Result<(), &'static str> {
    match description {
        Some(s) if s.trim().is_empty() => Err("description must not be empty"),
        _ => Ok(()),
    }
}

/// 校验 patch 中的 status（Some("") 拒绝；None 不动）。
pub fn validate_tool_patch_status(
    status: Option<&str>,
) -> Result<(), &'static str> {
    match status {
        Some(s) => validate_tool_status(s),
        None => Ok(()),
    }
}

/// 校验 patch 中的 metadata_merge 必须是 object。
pub fn validate_tool_patch_metadata_merge(
    metadata: Option<&Value>,
) -> Result<(), &'static str> {
    match metadata {
        Some(v) if !v.is_object() => Err("metadata_merge must be an object"),
        _ => Ok(()),
    }
}

/// 检测一组应用名是否包含重复（按 trim 后精确比较）。
pub fn has_duplicate_name(names: &[&str]) -> bool {
    let mut seen = std::collections::HashSet::new();
    for n in names {
        let trimmed = n.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !seen.insert(trimmed.to_string()) {
            return true;
        }
    }
    false
}

/// 把一组 tool kind 规范化：trim + 校验。
pub fn normalize_tool_kinds(kinds: &[&str]) -> Result<Vec<String>, &'static str> {
    let mut out = Vec::with_capacity(kinds.len());
    for k in kinds {
        let trimmed = k.trim();
        if trimmed.is_empty() {
            return Err("kind must not be empty");
        }
        out.push(trimmed.to_string());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r747_kind_predicate() {
        assert!(is_tool_kind_allowed("mcp"));
        assert!(is_tool_kind_allowed("api"));
        assert!(is_tool_kind_allowed("cli"));
        assert!(is_tool_kind_allowed("webhook"));
        assert!(!is_tool_kind_allowed(""));
        assert!(!is_tool_kind_allowed("MCP")); // case-sensitive
        assert!(!is_tool_kind_allowed("grpc"));
    }

    #[test]
    fn r747_status_predicate() {
        assert!(is_tool_status_allowed("active"));
        assert!(is_tool_status_allowed("disabled"));
        assert!(is_tool_status_allowed("draft"));
        assert!(!is_tool_status_allowed("deleted"));
        assert!(!is_tool_status_allowed(""));
        assert!(!is_tool_status_allowed("Active")); // case-sensitive
    }

    #[test]
    fn r747_validate_name_non_empty_ok() {
        assert!(validate_tool_name_non_empty("hello").is_ok());
        assert!(validate_tool_name_non_empty("  hello  ").is_ok());
    }

    #[test]
    fn r747_validate_name_non_empty_empty() {
        assert!(validate_tool_name_non_empty("").is_err());
        assert!(validate_tool_name_non_empty("   ").is_err());
    }

    #[test]
    fn r747_validate_kind_ok() {
        assert!(validate_tool_kind("mcp").is_ok());
        assert!(validate_tool_kind("api").is_ok());
        assert!(validate_tool_kind(" cli ").is_ok()); // trim 后合法
    }

    #[test]
    fn r747_validate_kind_empty() {
        let err = validate_tool_kind("").unwrap_err();
        assert!(err.contains("kind"));
    }

    #[test]
    fn r747_validate_kind_unknown() {
        let err = validate_tool_kind("grpc").unwrap_err();
        assert!(err.contains("kind must be"));
    }

    #[test]
    fn r747_validate_status_ok() {
        assert!(validate_tool_status("active").is_ok());
        assert!(validate_tool_status("disabled").is_ok());
    }

    #[test]
    fn r747_validate_status_empty() {
        let err = validate_tool_status("").unwrap_err();
        assert!(err.contains("status"));
    }

    #[test]
    fn r747_validate_status_unknown() {
        let err = validate_tool_status("archived").unwrap_err();
        assert!(err.contains("status must be"));
    }

    #[test]
    fn r747_validate_metadata_null_passes() {
        assert!(validate_tool_metadata(&Value::Null).is_ok());
    }

    #[test]
    fn r747_validate_metadata_object_passes() {
        assert!(validate_tool_metadata(&serde_json::json!({})).is_ok());
        assert!(validate_tool_metadata(&serde_json::json!({"x": 1})).is_ok());
    }

    #[test]
    fn r747_validate_metadata_non_object_blocked() {
        assert!(validate_tool_metadata(&serde_json::json!([])).is_err());
        assert!(validate_tool_metadata(&serde_json::json!("string")).is_err());
        assert!(validate_tool_metadata(&serde_json::json!(42)).is_err());
    }

    #[test]
    fn r747_validate_patch_name_none_passes() {
        assert!(validate_tool_patch_name(None).is_ok());
    }

    #[test]
    fn r747_validate_patch_name_nonempty_passes() {
        assert!(validate_tool_patch_name(Some("hello")).is_ok());
    }

    #[test]
    fn r747_validate_patch_name_empty_blocked() {
        assert!(validate_tool_patch_name(Some("")).is_err());
        assert!(validate_tool_patch_name(Some("   ")).is_err());
    }

    #[test]
    fn r747_validate_patch_description_none_passes() {
        assert!(validate_tool_patch_description(None).is_ok());
    }

    #[test]
    fn r747_validate_patch_description_nonempty_passes() {
        assert!(validate_tool_patch_description(Some("hello")).is_ok());
    }

    #[test]
    fn r747_validate_patch_description_empty_blocked() {
        assert!(validate_tool_patch_description(Some("")).is_err());
        assert!(validate_tool_patch_description(Some("   ")).is_err());
    }

    #[test]
    fn r747_validate_patch_status_none_passes() {
        assert!(validate_tool_patch_status(None).is_ok());
    }

    #[test]
    fn r747_validate_patch_status_known_passes() {
        assert!(validate_tool_patch_status(Some("active")).is_ok());
    }

    #[test]
    fn r747_validate_patch_status_unknown_blocked() {
        assert!(validate_tool_patch_status(Some("garbage")).is_err());
    }

    #[test]
    fn r747_validate_patch_metadata_merge_none_passes() {
        assert!(validate_tool_patch_metadata_merge(None).is_ok());
    }

    #[test]
    fn r747_validate_patch_metadata_merge_object_passes() {
        assert!(validate_tool_patch_metadata_merge(Some(&serde_json::json!({}))).is_ok());
    }

    #[test]
    fn r747_validate_patch_metadata_merge_non_object_blocked() {
        assert!(
            validate_tool_patch_metadata_merge(Some(&serde_json::json!([]))).is_err()
        );
    }

    #[test]
    fn r747_has_duplicate_name_no_dup() {
        assert!(!has_duplicate_name(&["a", "b", "c"]));
        assert!(!has_duplicate_name(&[]));
    }

    #[test]
    fn r747_has_duplicate_name_dup() {
        assert!(has_duplicate_name(&["a", "b", "a"]));
    }

    #[test]
    fn r747_has_duplicate_name_trim_dup() {
        // " a " 和 "a" 被视为重复
        assert!(has_duplicate_name(&["a", " a "]));
    }

    #[test]
    fn r747_has_duplicate_name_skips_empty() {
        // 空字符串被视为"未设置"，不算重复
        assert!(!has_duplicate_name(&["", ""]));
        assert!(!has_duplicate_name(&["a", "  "]));
    }

    #[test]
    fn r747_normalize_tool_kinds_ok() {
        let r = normalize_tool_kinds(&["mcp", "api", "  cli  "]).unwrap();
        assert_eq!(r, vec!["mcp", "api", "cli"]);
    }

    #[test]
    fn r747_normalize_tool_kinds_empty_blocked() {
        let err = normalize_tool_kinds(&["mcp", ""]).unwrap_err();
        assert!(err.contains("kind"));
    }

    #[test]
    fn r747_constants_match_repo() {
        assert_eq!(ALLOWED_TOOL_KINDS.len(), 4);
        assert_eq!(ALLOWED_TOOL_STATUSES.len(), 3);
    }
}
