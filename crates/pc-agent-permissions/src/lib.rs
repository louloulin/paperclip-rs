#![forbid(unsafe_code)]
//! `pc-agent-permissions` —— 规范化 agent permissions 对象。
//!
//! 对应 Node `server/src/services/agent-permissions.ts`（35 行）。
//!
//! 设计目标：1:1 复刻
//! - `NormalizedAgentPermissions` —— 任意键值对 + 两个强制 boolean 字段
//! - `defaultPermissionsForRole(role)` —— `"ceo"` → `canCreateAgents=true`，
//!   其它 role → `canCreateAgents=false`；`canCreateSkills` 始终为 `true`
//! - `normalizeAgentPermissions(perms, role)` —— 在 defaults 基础上覆盖：
//!   - 输入非 object / null / array → 直接返回 defaults
//!   - 否则保留原键值，仅当 `canCreateAgents` / `canCreateSkills` 是 boolean 时
//!     用其覆盖 defaults

use std::collections::BTreeMap;

/// 规范化后的 agent permissions。
///
/// 与 Node `NormalizedAgentPermissions` 1:1 对齐：
/// - `canCreateAgents: bool`
/// - `canCreateSkills: bool`
/// - 其它任意键（保留原值）
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NormalizedAgentPermissions {
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
    pub can_create_agents: bool,
    pub can_create_skills: bool,
}

impl NormalizedAgentPermissions {
    pub fn new(can_create_agents: bool, can_create_skills: bool) -> Self {
        Self {
            extra: BTreeMap::new(),
            can_create_agents,
            can_create_skills,
        }
    }
}

/// 计算某 role 的默认 permissions。
///
/// 与 Node `defaultPermissionsForRole` 1:1 对齐：
/// - `role.trim().toLowerCase() === "ceo"` → `canCreateAgents=true`
/// - 其它 → `canCreateAgents=false`
/// - `canCreateSkills` 恒为 `true`
pub fn default_permissions_for_role(role: &str) -> NormalizedAgentPermissions {
    NormalizedAgentPermissions::new(role.trim().to_lowercase() == "ceo", true)
}

/// 规范化输入 permissions：
/// - 输入非 object / null / array → 返回 `defaultPermissionsForRole(role)`
/// - 保留所有输入键值，再覆盖 `canCreateAgents` / `canCreateSkills`
///
/// 与 Node `normalizeAgentPermissions` 1:1 对齐。
pub fn normalize_agent_permissions(
    permissions: Option<&serde_json::Value>,
    role: &str,
) -> NormalizedAgentPermissions {
    let defaults = default_permissions_for_role(role);
    let Some(p) = permissions else {
        return defaults;
    };
    if !p.is_object() {
        return defaults;
    }
    let obj = p.as_object().unwrap();

    let mut extra = BTreeMap::new();
    for (k, v) in obj {
        // 两个特殊字段单独处理
        if k == "canCreateAgents" || k == "canCreateSkills" {
            continue;
        }
        extra.insert(k.clone(), v.clone());
    }

    let can_create_agents = obj
        .get("canCreateAgents")
        .and_then(|v| v.as_bool())
        .unwrap_or(defaults.can_create_agents);
    let can_create_skills = obj
        .get("canCreateSkills")
        .and_then(|v| v.as_bool())
        .unwrap_or(defaults.can_create_skills);

    NormalizedAgentPermissions {
        extra,
        can_create_agents,
        can_create_skills,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn r703_default_ceo_has_can_create_agents_true() {
        let p = default_permissions_for_role("ceo");
        assert!(p.can_create_agents);
        assert!(p.can_create_skills);
    }

    #[test]
    fn r703_default_ceo_uppercase_also_true() {
        // trim + to_lowercase 在比较之前
        let p = default_permissions_for_role("CEO");
        assert!(p.can_create_agents);
        let p = default_permissions_for_role("  ceo  ");
        assert!(p.can_create_agents);
    }

    #[test]
    fn r703_default_other_roles_lack_can_create_agents() {
        for role in ["operator", "viewer", "admin", "member", ""] {
            let p = default_permissions_for_role(role);
            assert!(
                !p.can_create_agents,
                "role {role:?} should default can_create_agents=false"
            );
            assert!(p.can_create_skills);
        }
    }

    #[test]
    fn r703_normalize_null_returns_defaults() {
        let p = normalize_agent_permissions(None, "ceo");
        assert!(p.can_create_agents);
        assert!(p.can_create_skills);
    }

    #[test]
    fn r703_normalize_non_object_returns_defaults() {
        for v in [json!("string"), json!(42), json!(true), json!([1, 2, 3])] {
            let p = normalize_agent_permissions(Some(&v), "ceo");
            assert!(p.can_create_agents);
        }
    }

    #[test]
    fn r703_normalize_overrides_boolean_fields() {
        let v = json!({
            "canCreateAgents": false,
            "canCreateSkills": false
        });
        // 即使 role=ceo，inputs 显式 false 也会覆盖 defaults
        let p = normalize_agent_permissions(Some(&v), "ceo");
        assert!(!p.can_create_agents);
        assert!(!p.can_create_skills);
    }

    #[test]
    fn r703_normalize_preserves_extra_fields() {
        let v = json!({
            "canCreateAgents": true,
            "canCreateSkills": true,
            "customKey": "custom-value",
            "anotherKey": 42
        });
        let p = normalize_agent_permissions(Some(&v), "operator");
        assert!(p.can_create_agents); // input true
        assert!(p.can_create_skills);
        assert_eq!(p.extra.get("customKey").unwrap(), "custom-value");
        assert_eq!(p.extra.get("anotherKey").unwrap(), 42);
    }

    #[test]
    fn r703_normalize_non_boolean_uses_default() {
        let v = json!({
            "canCreateAgents": "yes",
            "canCreateSkills": 1
        });
        // 非 boolean 类型 → 用 defaults
        let p = normalize_agent_permissions(Some(&v), "ceo");
        assert!(p.can_create_agents); // default for ceo = true
        assert!(p.can_create_skills); // default = true
    }

    #[test]
    fn r703_normalize_partial_override() {
        let v = json!({"canCreateAgents": false});
        let p = normalize_agent_permissions(Some(&v), "ceo");
        assert!(!p.can_create_agents); // overridden to false
        assert!(p.can_create_skills); // default true (no input)
    }

    #[test]
    fn r703_normalize_object_without_special_fields() {
        let v = json!({"customKey": "value"});
        let p = normalize_agent_permissions(Some(&v), "ceo");
        // 保留 extra，用 defaults 填两个 boolean
        assert!(p.can_create_agents);
        assert!(p.can_create_skills);
        assert_eq!(p.extra.get("customKey").unwrap(), "value");
    }

    #[test]
    fn r703_serialization_uses_camel_case() {
        let mut p = NormalizedAgentPermissions::new(false, true);
        p.extra.insert("customKey".to_string(), json!("v"));
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["canCreateAgents"], false);
        assert_eq!(v["canCreateSkills"], true);
        assert_eq!(v["customKey"], "v");
    }
}
