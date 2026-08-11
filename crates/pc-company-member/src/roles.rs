//! Human company membership role 规范化 + 权限映射。
//!
//! 对应 Node `server/src/services/company-member-roles.ts`（65 行）1:1 复刻。
//! （原 `pc-company-member-roles` crate 已下沉到 `pc-company-member`）。
//!
//! - `normalize_human_role`：把 `"member"` 兼容为 `"operator"`，未知值 fallback
//! - `grants_for_human_role`：每个 role 对应的 permission grants
//! - `resolve_human_invite_role`：从 invite defaults payload 解析 default role

/// Human company membership role 枚举 —— 与 Node `HumanCompanyMembershipRole` 1:1。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HumanCompanyMembershipRole {
    Owner,
    Admin,
    Operator,
    Viewer,
}

impl HumanCompanyMembershipRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Admin => "admin",
            Self::Operator => "operator",
            Self::Viewer => "viewer",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "owner" => Some(Self::Owner),
            "admin" => Some(Self::Admin),
            "operator" => Some(Self::Operator),
            "viewer" => Some(Self::Viewer),
            _ => None,
        }
    }
}

/// 所有合法 role（按权限从高到低）。
pub const HUMAN_COMPANY_MEMBERSHIP_ROLES: &[HumanCompanyMembershipRole] = &[
    HumanCompanyMembershipRole::Owner,
    HumanCompanyMembershipRole::Admin,
    HumanCompanyMembershipRole::Operator,
    HumanCompanyMembershipRole::Viewer,
];

/// Permission key 列表（与 Node `PERMISSION_KEYS` 同名常量 1:1）。
///
/// 这里只列出本 crate 关心的 8 个 key（owner/admin/operator/viewer 各自用到的子集）。
/// 完整列表在 `@paperclipai/shared`，但本 crate 是纯枚举，不依赖 shared 包。
pub mod permission_keys {
    pub const AGENTS_CREATE: &str = "agents:create";
    pub const AGENTS_CONFIGURE: &str = "agents:configure";
    pub const SKILLS_CREATE: &str = "skills:create";
    pub const ENVIRONMENTS_MANAGE: &str = "environments:manage";
    pub const USERS_INVITE: &str = "users:invite";
    pub const USERS_MANAGE_PERMISSIONS: &str = "users:manage_permissions";
    pub const TASKS_ASSIGN: &str = "tasks:assign";
    pub const JOINS_APPROVE: &str = "joins:approve";
}

/// 一条 grant。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Grant {
    pub permission_key: &'static str,
    pub scope: Option<serde_json::Value>,
}

/// 把任意值规范化为合法 role；兼容 `"member"` → `"operator"`。
///
/// 与 Node `normalizeHumanRole` 1:1 对齐：
/// - `"member"` → `Operator`
/// - 合法值 → 原样
/// - 其它 / 非 string → fallback
pub fn normalize_human_role(
    value: &serde_json::Value,
    fallback: HumanCompanyMembershipRole,
) -> HumanCompanyMembershipRole {
    if let Some(s) = value.as_str() {
        if s == "member" {
            return HumanCompanyMembershipRole::Operator;
        }
        if let Some(r) = HumanCompanyMembershipRole::from_str(s) {
            return r;
        }
    }
    fallback
}

/// 取得某个 role 的 permission grants。
pub fn grants_for_human_role(role: HumanCompanyMembershipRole) -> Vec<Grant> {
    use permission_keys::*;
    match role {
        HumanCompanyMembershipRole::Owner => vec![
            Grant { permission_key: AGENTS_CREATE, scope: None },
            Grant { permission_key: AGENTS_CONFIGURE, scope: None },
            Grant { permission_key: SKILLS_CREATE, scope: None },
            Grant { permission_key: ENVIRONMENTS_MANAGE, scope: None },
            Grant { permission_key: USERS_INVITE, scope: None },
            Grant { permission_key: USERS_MANAGE_PERMISSIONS, scope: None },
            Grant { permission_key: TASKS_ASSIGN, scope: None },
            Grant { permission_key: JOINS_APPROVE, scope: None },
        ],
        HumanCompanyMembershipRole::Admin => vec![
            Grant { permission_key: AGENTS_CREATE, scope: None },
            Grant { permission_key: AGENTS_CONFIGURE, scope: None },
            Grant { permission_key: SKILLS_CREATE, scope: None },
            Grant { permission_key: ENVIRONMENTS_MANAGE, scope: None },
            Grant { permission_key: USERS_INVITE, scope: None },
            Grant { permission_key: TASKS_ASSIGN, scope: None },
            Grant { permission_key: JOINS_APPROVE, scope: None },
        ],
        HumanCompanyMembershipRole::Operator => vec![
            Grant { permission_key: TASKS_ASSIGN, scope: None },
        ],
        HumanCompanyMembershipRole::Viewer => vec![],
    }
}

/// 从 invite defaults payload 解析 default role。
///
/// 与 Node `resolveHumanInviteRole` 1:1 对齐：
/// - payload 不是 object → `"operator"`
/// - `payload.human` 不是 object → `"operator"`
/// - `payload.human.role` 任意值 → `normalizeHumanRole(..., "operator")`
pub fn resolve_human_invite_role(
    defaults_payload: Option<&serde_json::Value>,
) -> HumanCompanyMembershipRole {
    let Some(payload) = defaults_payload else {
        return HumanCompanyMembershipRole::Operator;
    };
    let Some(payload_obj) = payload.as_object() else {
        return HumanCompanyMembershipRole::Operator;
    };
    let Some(scoped) = payload_obj.get("human") else {
        return HumanCompanyMembershipRole::Operator;
    };
    let Some(scoped_obj) = scoped.as_object() else {
        return HumanCompanyMembershipRole::Operator;
    };
    let role_value = scoped_obj.get("role").cloned().unwrap_or(serde_json::Value::Null);
    normalize_human_role(&role_value, HumanCompanyMembershipRole::Operator)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn r699_role_as_str_round_trip() {
        for r in HUMAN_COMPANY_MEMBERSHIP_ROLES {
            assert_eq!(HumanCompanyMembershipRole::from_str(r.as_str()), Some(*r));
        }
        assert_eq!(HumanCompanyMembershipRole::from_str("unknown"), None);
        assert_eq!(HumanCompanyMembershipRole::from_str("member"), None);
    }

    #[test]
    fn r699_normalize_member_to_operator() {
        let v = json!("member");
        let r = normalize_human_role(&v, HumanCompanyMembershipRole::Viewer);
        assert_eq!(r, HumanCompanyMembershipRole::Operator);
    }

    #[test]
    fn r699_normalize_known_roles() {
        for r in HUMAN_COMPANY_MEMBERSHIP_ROLES {
            let v = json!(r.as_str());
            assert_eq!(normalize_human_role(&v, HumanCompanyMembershipRole::Viewer), *r);
        }
    }

    #[test]
    fn r699_normalize_unknown_falls_back() {
        let v = json!("unknown");
        assert_eq!(
            normalize_human_role(&v, HumanCompanyMembershipRole::Viewer),
            HumanCompanyMembershipRole::Viewer
        );
    }

    #[test]
    fn r699_normalize_non_string_falls_back() {
        let v = json!(42);
        assert_eq!(
            normalize_human_role(&v, HumanCompanyMembershipRole::Admin),
            HumanCompanyMembershipRole::Admin
        );
        let v = json!(null);
        assert_eq!(
            normalize_human_role(&v, HumanCompanyMembershipRole::Admin),
            HumanCompanyMembershipRole::Admin
        );
    }

    #[test]
    fn r699_owner_has_all_grants() {
        let grants = grants_for_human_role(HumanCompanyMembershipRole::Owner);
        assert_eq!(grants.len(), 8);
        assert!(grants.iter().any(|g| g.permission_key == "agents:create"));
        assert!(grants.iter().any(|g| g.permission_key == "users:manage_permissions"));
        assert!(grants.iter().any(|g| g.permission_key == "tasks:assign"));
        assert!(grants.iter().any(|g| g.permission_key == "joins:approve"));
    }

    #[test]
    fn r699_admin_lacks_users_manage_permissions() {
        let grants = grants_for_human_role(HumanCompanyMembershipRole::Admin);
        assert_eq!(grants.len(), 7);
        assert!(!grants.iter().any(|g| g.permission_key == "users:manage_permissions"));
    }

    #[test]
    fn r699_operator_only_tasks_assign() {
        let grants = grants_for_human_role(HumanCompanyMembershipRole::Operator);
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0].permission_key, "tasks:assign");
    }

    #[test]
    fn r699_viewer_no_grants() {
        let grants = grants_for_human_role(HumanCompanyMembershipRole::Viewer);
        assert!(grants.is_empty());
    }

    #[test]
    fn r699_resolve_invite_role_from_none() {
        let r = resolve_human_invite_role(None);
        assert_eq!(r, HumanCompanyMembershipRole::Operator);
    }

    #[test]
    fn r699_resolve_invite_role_from_non_object() {
        let r = resolve_human_invite_role(Some(&json!("not-object")));
        assert_eq!(r, HumanCompanyMembershipRole::Operator);
        let r = resolve_human_invite_role(Some(&json!(42)));
        assert_eq!(r, HumanCompanyMembershipRole::Operator);
    }

    #[test]
    fn r699_resolve_invite_role_from_object_no_human_key() {
        let r = resolve_human_invite_role(Some(&json!({"foo": "bar"})));
        assert_eq!(r, HumanCompanyMembershipRole::Operator);
    }

    #[test]
    fn r699_resolve_invite_role_human_not_object() {
        let r = resolve_human_invite_role(Some(&json!({"human": "not-object"})));
        assert_eq!(r, HumanCompanyMembershipRole::Operator);
        let r = resolve_human_invite_role(Some(&json!({"human": 42})));
        assert_eq!(r, HumanCompanyMembershipRole::Operator);
    }

    #[test]
    fn r699_resolve_invite_role_human_object_with_role() {
        let r = resolve_human_invite_role(Some(&json!({"human": {"role": "admin"}})));
        assert_eq!(r, HumanCompanyMembershipRole::Admin);
    }

    #[test]
    fn r699_resolve_invite_role_human_member_compat() {
        let r = resolve_human_invite_role(Some(&json!({"human": {"role": "member"}})));
        assert_eq!(r, HumanCompanyMembershipRole::Operator);
    }

    #[test]
    fn r699_resolve_invite_role_human_unknown_falls_back_to_operator() {
        let r = resolve_human_invite_role(Some(&json!({"human": {"role": "unknown"}})));
        assert_eq!(r, HumanCompanyMembershipRole::Operator);
    }

    #[test]
    fn r699_resolve_invite_role_human_no_role_key() {
        let r = resolve_human_invite_role(Some(&json!({"human": {}})));
        assert_eq!(r, HumanCompanyMembershipRole::Operator);
    }
}
