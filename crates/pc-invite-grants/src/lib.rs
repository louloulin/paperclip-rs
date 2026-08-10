#![forbid(unsafe_code)]
//! `pc-invite-grants` —— 从 invite defaults payload 提取 grant 数组。
//!
//! 对应 Node `server/src/services/invite-grants.ts`（68 行）。
//!
//! 设计目标：1:1 复刻
//! - `grantsFromDefaults`：从 payload 中按 key 提取合法 grants，过滤非法 permissionKey
//! - `agentJoinGrantsFromDefaults`：保证 agent grant 包含 `tasks:assign`
//! - `humanJoinGrantsFromDefaults`：human grant 为空时回退到 `grantsForHumanRole(role)`

use pc_company_member_roles::{
    grants_for_human_role, permission_keys, Grant, HumanCompanyMembershipRole,
};

/// Defaults payload 中 grants 数组的 key 上下文。
///
/// 与 Node `grantsFromDefaults(defaultsPayload, key)` 第二参数 1:1 对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DefaultsKey {
    Human,
    Agent,
}

impl DefaultsKey {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Agent => "agent",
        }
    }
}

/// 所有合法的 permission key —— 与 Node `PERMISSION_KEYS` 1:1。
///
/// 当前 8 个 key 来自 `pc-company-member-roles::permission_keys`；如果之后新增
/// permission 类别，需要同步扩展 `is_valid_permission_key` 的判断。
pub fn is_valid_permission_key(s: &str) -> bool {
    matches!(
        s,
        permission_keys::AGENTS_CREATE
            | permission_keys::AGENTS_CONFIGURE
            | permission_keys::SKILLS_CREATE
            | permission_keys::ENVIRONMENTS_MANAGE
            | permission_keys::USERS_INVITE
            | permission_keys::USERS_MANAGE_PERMISSIONS
            | permission_keys::TASKS_ASSIGN
            | permission_keys::JOINS_APPROVE
    )
}

/// 从 `defaultsPayload[key].grants` 中提取合法 grants。
///
/// 与 Node `grantsFromDefaults` 1:1 对齐：
/// - payload 不是 object → `[]`
/// - `payload[key]` 不是 object → `[]`
/// - `grants` 不是 array → `[]`
/// - 跳过非 object / 缺 `permissionKey` 字符串 / 非法 key 的项
/// - `scope` 是 object (非 array) 时保留，否则 `null`
pub fn grants_from_defaults(
    defaults_payload: Option<&serde_json::Value>,
    key: DefaultsKey,
) -> Vec<Grant> {
    let Some(payload) = defaults_payload else {
        return Vec::new();
    };
    let Some(payload_obj) = payload.as_object() else {
        return Vec::new();
    };
    let Some(scoped) = payload_obj.get(key.as_str()) else {
        return Vec::new();
    };
    let Some(scoped_obj) = scoped.as_object() else {
        return Vec::new();
    };
    let Some(grants_value) = scoped_obj.get("grants") else {
        return Vec::new();
    };
    let Some(grants_arr) = grants_value.as_array() else {
        return Vec::new();
    };

    let mut result = Vec::new();
    for item in grants_arr {
        let Some(item_obj) = item.as_object() else {
            continue;
        };
        let Some(pk) = item_obj.get("permissionKey").and_then(|v| v.as_str()) else {
            continue;
        };
        if !is_valid_permission_key(pk) {
            continue;
        }

        // Rust 端把 permissionKey 物化为 &'static str (来自常量)。
        let static_pk: &'static str = match pk {
            permission_keys::AGENTS_CREATE => permission_keys::AGENTS_CREATE,
            permission_keys::AGENTS_CONFIGURE => permission_keys::AGENTS_CONFIGURE,
            permission_keys::SKILLS_CREATE => permission_keys::SKILLS_CREATE,
            permission_keys::ENVIRONMENTS_MANAGE => permission_keys::ENVIRONMENTS_MANAGE,
            permission_keys::USERS_INVITE => permission_keys::USERS_INVITE,
            permission_keys::USERS_MANAGE_PERMISSIONS => permission_keys::USERS_MANAGE_PERMISSIONS,
            permission_keys::TASKS_ASSIGN => permission_keys::TASKS_ASSIGN,
            permission_keys::JOINS_APPROVE => permission_keys::JOINS_APPROVE,
            _ => continue, // 已经过滤过，这里是 unreachable
        };

        let scope = item_obj.get("scope").and_then(|v| {
            if v.is_object() {
                Some(v.clone())
            } else {
                None
            }
        });

        result.push(Grant {
            permission_key: static_pk,
            scope,
        });
    }
    result
}

/// Agent 加入时使用的 grants —— 保证包含 `tasks:assign`。
///
/// 与 Node `agentJoinGrantsFromDefaults` 1:1 对齐：
/// - 若已有 `tasks:assign`，返回原 grants
/// - 否则在末尾追加 `{ permissionKey: "tasks:assign", scope: null }`
pub fn agent_join_grants_from_defaults(
    defaults_payload: Option<&serde_json::Value>,
) -> Vec<Grant> {
    let grants = grants_from_defaults(defaults_payload, DefaultsKey::Agent);
    if grants
        .iter()
        .any(|g| g.permission_key == permission_keys::TASKS_ASSIGN)
    {
        return grants;
    }
    let mut out = grants;
    out.push(Grant {
        permission_key: permission_keys::TASKS_ASSIGN,
        scope: None,
    });
    out
}

/// Human 加入时使用的 grants —— 空时回退到 role 默认 grants。
///
/// 与 Node `humanJoinGrantsFromDefaults` 1:1 对齐：
/// - 从 payload 中提取非空 human grants 直接返回
/// - 空时返回 `grantsForHumanRole(membershipRole)`
pub fn human_join_grants_from_defaults(
    defaults_payload: Option<&serde_json::Value>,
    membership_role: HumanCompanyMembershipRole,
) -> Vec<Grant> {
    let grants = grants_from_defaults(defaults_payload, DefaultsKey::Human);
    if !grants.is_empty() {
        return grants;
    }
    grants_for_human_role(membership_role)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn r700_is_valid_permission_key_all_keys() {
        assert!(is_valid_permission_key("agents:create"));
        assert!(is_valid_permission_key("agents:configure"));
        assert!(is_valid_permission_key("skills:create"));
        assert!(is_valid_permission_key("environments:manage"));
        assert!(is_valid_permission_key("users:invite"));
        assert!(is_valid_permission_key("users:manage_permissions"));
        assert!(is_valid_permission_key("tasks:assign"));
        assert!(is_valid_permission_key("joins:approve"));
        assert!(!is_valid_permission_key("unknown:key"));
        assert!(!is_valid_permission_key(""));
    }

    #[test]
    fn r700_grants_from_defaults_none_payload() {
        let r = grants_from_defaults(None, DefaultsKey::Human);
        assert!(r.is_empty());
    }

    #[test]
    fn r700_grants_from_defaults_non_object_payload() {
        let r = grants_from_defaults(Some(&json!("not-object")), DefaultsKey::Human);
        assert!(r.is_empty());
        let r = grants_from_defaults(Some(&json!(42)), DefaultsKey::Human);
        assert!(r.is_empty());
        let r = grants_from_defaults(Some(&json!(null)), DefaultsKey::Human);
        assert!(r.is_empty());
    }

    #[test]
    fn r700_grants_from_defaults_missing_key() {
        let r = grants_from_defaults(Some(&json!({"foo": "bar"})), DefaultsKey::Human);
        assert!(r.is_empty());
        let r = grants_from_defaults(Some(&json!({})), DefaultsKey::Agent);
        assert!(r.is_empty());
    }

    #[test]
    fn r700_grants_from_defaults_scoped_not_object() {
        let r = grants_from_defaults(
            Some(&json!({"human": "not-object"})),
            DefaultsKey::Human,
        );
        assert!(r.is_empty());
        let r = grants_from_defaults(Some(&json!({"human": 42})), DefaultsKey::Human);
        assert!(r.is_empty());
    }

    #[test]
    fn r700_grants_from_defaults_no_grants_key() {
        let r = grants_from_defaults(
            Some(&json!({"human": {}})),
            DefaultsKey::Human,
        );
        assert!(r.is_empty());
    }

    #[test]
    fn r700_grants_from_defaults_grants_not_array() {
        let r = grants_from_defaults(
            Some(&json!({"human": {"grants": "not-array"}})),
            DefaultsKey::Human,
        );
        assert!(r.is_empty());
    }

    #[test]
    fn r700_grants_from_defaults_filters_invalid_keys() {
        let payload = json!({
            "human": {
                "grants": [
                    {"permissionKey": "agents:create"},
                    {"permissionKey": "unknown:key"},
                    {"permissionKey": 42},
                    "not-object",
                    null,
                    {"permissionKey": "tasks:assign"}
                ]
            }
        });
        let r = grants_from_defaults(Some(&payload), DefaultsKey::Human);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].permission_key, "agents:create");
        assert_eq!(r[1].permission_key, "tasks:assign");
    }

    #[test]
    fn r700_grants_from_defaults_keeps_object_scope() {
        let payload = json!({
            "agent": {
                "grants": [
                    {"permissionKey": "tasks:assign", "scope": {"foo": "bar"}}
                ]
            }
        });
        let r = grants_from_defaults(Some(&payload), DefaultsKey::Agent);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].permission_key, "tasks:assign");
        assert_eq!(r[0].scope.as_ref().unwrap().get("foo").unwrap(), "bar");
    }

    #[test]
    fn r700_grants_from_defaults_nullifies_non_object_scope() {
        let payload = json!({
            "agent": {
                "grants": [
                    {"permissionKey": "tasks:assign", "scope": "string"},
                    {"permissionKey": "joins:approve", "scope": [1, 2, 3]},
                    {"permissionKey": "skills:create"}
                ]
            }
        });
        let r = grants_from_defaults(Some(&payload), DefaultsKey::Agent);
        assert_eq!(r.len(), 3);
        assert!(r[0].scope.is_none());
        assert!(r[1].scope.is_none());
        assert!(r[2].scope.is_none());
    }

    #[test]
    fn r700_grants_from_defaults_correct_key_extraction() {
        let payload = json!({
            "human": {"grants": [{"permissionKey": "tasks:assign"}]},
            "agent": {"grants": [{"permissionKey": "joins:approve"}]}
        });
        let human = grants_from_defaults(Some(&payload), DefaultsKey::Human);
        let agent = grants_from_defaults(Some(&payload), DefaultsKey::Agent);
        assert_eq!(human[0].permission_key, "tasks:assign");
        assert_eq!(agent[0].permission_key, "joins:approve");
    }

    #[test]
    fn r700_agent_join_keeps_existing_tasks_assign() {
        let payload = json!({
            "agent": {
                "grants": [
                    {"permissionKey": "tasks:assign"},
                    {"permissionKey": "joins:approve"}
                ]
            }
        });
        let r = agent_join_grants_from_defaults(Some(&payload));
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].permission_key, "tasks:assign");
        assert_eq!(r[1].permission_key, "joins:approve");
    }

    #[test]
    fn r700_agent_join_appends_tasks_assign_if_missing() {
        let payload = json!({
            "agent": {
                "grants": [
                    {"permissionKey": "joins:approve"}
                ]
            }
        });
        let r = agent_join_grants_from_defaults(Some(&payload));
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].permission_key, "joins:approve");
        assert_eq!(r[1].permission_key, "tasks:assign");
        assert!(r[1].scope.is_none());
    }

    #[test]
    fn r700_agent_join_from_empty_payload_returns_only_tasks_assign() {
        let r = agent_join_grants_from_defaults(None);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].permission_key, "tasks:assign");
        assert!(r[0].scope.is_none());
    }

    #[test]
    fn r700_agent_join_from_payload_with_no_agent_key() {
        let payload = json!({"human": {"grants": [{"permissionKey": "tasks:assign"}]}});
        let r = agent_join_grants_from_defaults(Some(&payload));
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].permission_key, "tasks:assign");
    }

    #[test]
    fn r700_human_join_returns_payload_grants_when_non_empty() {
        let payload = json!({
            "human": {
                "grants": [
                    {"permissionKey": "tasks:assign", "scope": {"x": 1}}
                ]
            }
        });
        let r = human_join_grants_from_defaults(
            Some(&payload),
            HumanCompanyMembershipRole::Viewer,
        );
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].permission_key, "tasks:assign");
        assert!(r[0].scope.is_some());
    }

    #[test]
    fn r700_human_join_falls_back_to_role_when_empty() {
        // payload 为 None
        let r = human_join_grants_from_defaults(None, HumanCompanyMembershipRole::Owner);
        assert_eq!(r.len(), 8);

        // payload.human.grants 是空数组
        let payload = json!({"human": {"grants": []}});
        let r = human_join_grants_from_defaults(
            Some(&payload),
            HumanCompanyMembershipRole::Operator,
        );
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].permission_key, "tasks:assign");

        // payload.human.grants 中所有项都被过滤掉（非法 key）
        let payload = json!({"human": {"grants": [{"permissionKey": "unknown"}]}});
        let r = human_join_grants_from_defaults(
            Some(&payload),
            HumanCompanyMembershipRole::Viewer,
        );
        assert!(r.is_empty());
    }

    #[test]
    fn r700_human_join_falls_back_to_admin_role() {
        // 没有 payload 时，admin role 应该返回 7 个 grants (无 users:manage_permissions)
        let r = human_join_grants_from_defaults(None, HumanCompanyMembershipRole::Admin);
        assert_eq!(r.len(), 7);
        assert!(!r.iter().any(|g| g.permission_key == "users:manage_permissions"));
        assert!(r.iter().any(|g| g.permission_key == "agents:create"));
    }
}
