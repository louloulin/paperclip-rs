//! Company 域常量（port 自 paperclip `constants.ts` 的 company 块）。
//!
//! 命名与值与 Node 上游 1:1 对齐，确保跨语言互操作。

/// Company 生命周期状态（与 Node `COMPANY_STATUSES = ["active", "paused", "archived"]` 对齐）。
pub const COMPANY_STATUSES: &[&str] = &["active", "paused", "archived"];

/// 默认公司附件最大字节数（10 MiB，对齐 `DEFAULT_COMPANY_ATTACHMENT_MAX_BYTES`）。
pub const DEFAULT_COMPANY_ATTACHMENT_MAX_BYTES: usize = 10 * 1024 * 1024;

/// 公司附件最大字节数上限（1 GiB，对齐 `MAX_COMPANY_ATTACHMENT_MAX_BYTES`）。
pub const MAX_COMPANY_ATTACHMENT_MAX_BYTES: usize = 1024 * 1024 * 1024;

/// Principal 类型（user / agent）。
pub const PRINCIPAL_TYPES: &[&str] = &["user", "agent"];

/// Membership 状态。
pub const MEMBERSHIP_STATUSES: &[&str] = &["pending", "active", "suspended", "archived"];

/// 公司成员角色（含子集 — Human / Agent）。
pub const COMPANY_MEMBERSHIP_ROLES: &[&str] = &["owner", "admin", "member", "agent"];

/// Human 专用成员角色。
pub const HUMAN_COMPANY_MEMBERSHIP_ROLES: &[&str] = &["owner", "admin", "member"];

/// Instance 用户角色（用于跨公司系统级 admin）。
pub const INSTANCE_USER_ROLES: &[&str] = &["instance_admin"];

/// Invite 类型。
pub const INVITE_TYPES: &[&str] = &["company_join", "bootstrap_ceo"];

/// Invite 加入类型（human / agent / both）。
pub const INVITE_JOIN_TYPES: &[&str] = &["human", "agent", "both"];

/// Join request 类型。
pub const JOIN_REQUEST_TYPES: &[&str] = &["human", "agent"];

/// Join request 状态。
pub const JOIN_REQUEST_STATUSES: &[&str] = &["pending_approval", "approved", "rejected"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn company_statuses_match_node() {
        assert_eq!(COMPANY_STATUSES, &["active", "paused", "archived"]);
    }

    #[test]
    fn attachment_limits_match_node() {
        assert_eq!(DEFAULT_COMPANY_ATTACHMENT_MAX_BYTES, 10 * 1024 * 1024);
        assert_eq!(MAX_COMPANY_ATTACHMENT_MAX_BYTES, 1024 * 1024 * 1024);
    }

    #[test]
    fn principal_types_match_node() {
        assert_eq!(PRINCIPAL_TYPES, &["user", "agent"]);
    }

    #[test]
    fn membership_roles_superset_human_subset() {
        // Human roles must be subset of company membership roles
        for role in HUMAN_COMPANY_MEMBERSHIP_ROLES {
            assert!(
                COMPANY_MEMBERSHIP_ROLES.contains(role),
                "human role {role} must be in company roles"
            );
        }
    }

    #[test]
    fn invite_join_types_match_node() {
        assert_eq!(INVITE_JOIN_TYPES, &["human", "agent", "both"]);
    }

    #[test]
    fn join_request_statuses_match_node() {
        assert_eq!(
            JOIN_REQUEST_STATUSES,
            &["pending_approval", "approved", "rejected"]
        );
    }
}
