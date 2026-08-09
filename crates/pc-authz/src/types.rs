//! pc-authz：核心类型（PermissionKey / Resource / Decision / Reason）。
//!
//! 与原 `paperclip/server/src/services/authorization.ts` 中的
//! `AuthorizationAction` / `AuthorizationResource` / `AuthorizationDecision` 对齐。

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 主体类型（与原 `paperclip/packages/shared/src/constants.ts` 中 `PRINCIPAL_TYPES` 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PrincipalType {
    User,
    Agent,
}

impl PrincipalType {
    pub fn as_str(self) -> &'static str {
        match self {
            PrincipalType::User => "user",
            PrincipalType::Agent => "agent",
        }
    }
}

/// 公司成员角色（与原 `COMPANY_MEMBERSHIP_ROLES` 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompanyRole {
    Owner,
    Admin,
    Operator,
    Viewer,
    Member,
}

impl CompanyRole {
    pub fn as_str(self) -> &'static str {
        match self {
            CompanyRole::Owner => "owner",
            CompanyRole::Admin => "admin",
            CompanyRole::Operator => "operator",
            CompanyRole::Viewer => "viewer",
            CompanyRole::Member => "member",
        }
    }
    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "owner" => Some(Self::Owner),
            "admin" => Some(Self::Admin),
            "operator" => Some(Self::Operator),
            "viewer" => Some(Self::Viewer),
            "member" => Some(Self::Member),
            _ => None,
        }
    }
    pub fn is_admin_or_above(self) -> bool {
        matches!(self, Self::Owner | Self::Admin)
    }
    pub fn is_write(self) -> bool {
        matches!(self, Self::Owner | Self::Admin | Self::Operator)
    }
}

/// 授权作用域：被授权的细粒度 permission key。
///
/// 与原 `PERMISSION_KEYS` 数组（21 个常量）一一对应。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum PermissionKey {
    #[serde(rename = "agents:create")]
    AgentsCreate,
    #[serde(rename = "agents:configure")]
    AgentsConfigure,
    #[serde(rename = "agents:suggest-changes")]
    AgentsSuggestChanges,
    #[serde(rename = "skills:create")]
    SkillsCreate,
    #[serde(rename = "skills:suggest-changes")]
    SkillsSuggestChanges,
    #[serde(rename = "environments:manage")]
    EnvironmentsManage,
    #[serde(rename = "tools:admin")]
    ToolsAdmin,
    #[serde(rename = "tools:manage_connections")]
    ToolsManageConnections,
    #[serde(rename = "tools:manage_profiles")]
    ToolsManageProfiles,
    #[serde(rename = "tools:view_audit")]
    ToolsViewAudit,
    #[serde(rename = "audit:view_agent_actions")]
    AuditViewAgentActions,
    #[serde(rename = "tools:use")]
    ToolsUse,
    #[serde(rename = "tools:manage_runtime")]
    ToolsManageRuntime,
    #[serde(rename = "inbox:manage")]
    InboxManage,
    #[serde(rename = "users:invite")]
    UsersInvite,
    #[serde(rename = "users:manage_permissions")]
    UsersManagePermissions,
    #[serde(rename = "tasks:assign")]
    TasksAssign,
    #[serde(rename = "tasks:assign_scope")]
    TasksAssignScope,
    #[serde(rename = "tasks:manage_active_checkouts")]
    TasksManageActiveCheckouts,
    #[serde(rename = "pipelines:write")]
    PipelinesWrite,
    #[serde(rename = "joins:approve")]
    JoinsApprove,
}

impl PermissionKey {
    pub fn as_str(self) -> &'static str {
        match self {
            PermissionKey::AgentsCreate => "agents:create",
            PermissionKey::AgentsConfigure => "agents:configure",
            PermissionKey::AgentsSuggestChanges => "agents:suggest-changes",
            PermissionKey::SkillsCreate => "skills:create",
            PermissionKey::SkillsSuggestChanges => "skills:suggest-changes",
            PermissionKey::EnvironmentsManage => "environments:manage",
            PermissionKey::ToolsAdmin => "tools:admin",
            PermissionKey::ToolsManageConnections => "tools:manage_connections",
            PermissionKey::ToolsManageProfiles => "tools:manage_profiles",
            PermissionKey::ToolsViewAudit => "tools:view_audit",
            PermissionKey::AuditViewAgentActions => "audit:view_agent_actions",
            PermissionKey::ToolsUse => "tools:use",
            PermissionKey::ToolsManageRuntime => "tools:manage_runtime",
            PermissionKey::InboxManage => "inbox:manage",
            PermissionKey::UsersInvite => "users:invite",
            PermissionKey::UsersManagePermissions => "users:manage_permissions",
            PermissionKey::TasksAssign => "tasks:assign",
            PermissionKey::TasksAssignScope => "tasks:assign_scope",
            PermissionKey::TasksManageActiveCheckouts => "tasks:manage_active_checkouts",
            PermissionKey::PipelinesWrite => "pipelines:write",
            PermissionKey::JoinsApprove => "joins:approve",
        }
    }

    /// 默认所需公司角色（admin 才能授予的为 `Admin`，普通写为 `Operator`，只读为 `Viewer`）。
    pub fn default_required_role(self) -> CompanyRole {
        match self {
            PermissionKey::UsersManagePermissions
            | PermissionKey::JoinsApprove
            | PermissionKey::ToolsAdmin
            | PermissionKey::EnvironmentsManage => CompanyRole::Admin,
            PermissionKey::AgentsCreate
            | PermissionKey::AgentsConfigure
            | PermissionKey::AgentsSuggestChanges
            | PermissionKey::SkillsCreate
            | PermissionKey::SkillsSuggestChanges
            | PermissionKey::ToolsManageConnections
            | PermissionKey::ToolsManageProfiles
            | PermissionKey::ToolsManageRuntime
            | PermissionKey::ToolsViewAudit
            | PermissionKey::AuditViewAgentActions
            | PermissionKey::ToolsUse
            | PermissionKey::InboxManage
            | PermissionKey::UsersInvite
            | PermissionKey::TasksAssign
            | PermissionKey::TasksAssignScope
            | PermissionKey::TasksManageActiveCheckouts
            | PermissionKey::PipelinesWrite => CompanyRole::Operator,
        }
    }
}

/// 简化的 authorization action：直接以 permission key 表达。
///
/// 对应原 `AuthorizationAction = PermissionKey | "agent_config:read" | ...` 中
/// 不属于 PermissionKey 的额外场景 action，单独建模为 [`Action`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Action {
    /// 任意 PermissionKey（公司范围）。
    Permission(PermissionKey),
    /// 一些与 PermissionKey 不重叠的隐式 action。
    AgentConfigRead,
    AgentConfigUpdate,
    SkillConfigUpdate,
    AgentRead,
    AgentWake,
    CompanyScopeRead,
    IssueComment,
    IssueMutate,
    IssueRead,
    ProjectRead,
    RuntimeManage,
    SecretsRead,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Permission(p) => p.as_str(),
            Action::AgentConfigRead => "agent_config:read",
            Action::AgentConfigUpdate => "agent_config:update",
            Action::SkillConfigUpdate => "skill_config:update",
            Action::AgentRead => "agent:read",
            Action::AgentWake => "agent:wake",
            Action::CompanyScopeRead => "company_scope:read",
            Action::IssueComment => "issue:comment",
            Action::IssueMutate => "issue:mutate",
            Action::IssueRead => "issue:read",
            Action::ProjectRead => "project:read",
            Action::RuntimeManage => "runtime:manage",
            Action::SecretsRead => "secrets:read",
        }
    }
}

/// 待授权的资源。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Resource {
    Company {
        company_id: Uuid,
    },
    Agent {
        company_id: Uuid,
        #[serde(default)]
        agent_id: Option<Uuid>,
    },
    Project {
        company_id: Uuid,
        #[serde(default)]
        project_id: Option<Uuid>,
    },
    Issue {
        company_id: Uuid,
        #[serde(default)]
        issue_id: Option<Uuid>,
        #[serde(default)]
        project_id: Option<Uuid>,
        #[serde(default)]
        parent_issue_id: Option<Uuid>,
        #[serde(default)]
        assignee_agent_id: Option<Uuid>,
        #[serde(default)]
        assignee_user_id: Option<String>,
        #[serde(default)]
        origin_kind: Option<String>,
        #[serde(default)]
        origin_id: Option<String>,
        #[serde(default)]
        status: Option<String>,
    },
}

impl Resource {
    pub fn company_id(&self) -> Uuid {
        match self {
            Resource::Company { company_id }
            | Resource::Agent { company_id, .. }
            | Resource::Project { company_id, .. }
            | Resource::Issue { company_id, .. } => *company_id,
        }
    }
}

/// 决策结果 reason 标签（与 Node `AuthorizationDecision["reason"]` 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Reason {
    AllowLowTrustBoundary,
    AllowLocalBoard,
    AllowInstanceAdmin,
    AllowExplicitGrant,
    AllowDirectChange,
    AllowConsentedChange,
    AllowLegacyAgentCreator,
    AllowIssueMentionGrant,
    AllowDirectParentReport,
    AllowSelf,
    AllowCompanyAgent,
    AllowCompanyMember,
    AllowSimpleCompanyMember,
    AllowManagerChain,
    InboxTargetUserUnresolved,
    InboxManagementDisabled,
    InboxAgentNotAllowed,
    DenyUnauthenticated,
    DenyCompanyBoundary,
    DenyMissingMembership,
    DenyMissingGrant,
    DenyMissingConsent,
    DenyNoGrant,
    DenyPolicyRestricted,
    DenyForbidden,
    DenyUnknownAction,
}

impl Reason {
    pub fn is_allow(self) -> bool {
        matches!(
            self,
            Reason::AllowLowTrustBoundary
                | Reason::AllowLocalBoard
                | Reason::AllowInstanceAdmin
                | Reason::AllowExplicitGrant
                | Reason::AllowDirectChange
                | Reason::AllowConsentedChange
                | Reason::AllowLegacyAgentCreator
                | Reason::AllowIssueMentionGrant
                | Reason::AllowDirectParentReport
                | Reason::AllowSelf
                | Reason::AllowCompanyAgent
                | Reason::AllowCompanyMember
                | Reason::AllowSimpleCompanyMember
                | Reason::AllowManagerChain
        )
    }
}

/// 授权决策（与 Node `AuthorizationDecision` 等价）。
#[derive(Debug, Clone, Serialize)]
pub struct Decision {
    pub allowed: bool,
    pub action: Action,
    pub reason: Reason,
    pub explanation: String,
}

impl Decision {
    pub fn allow(action: Action, reason: Reason, explanation: impl Into<String>) -> Self {
        Self {
            allowed: true,
            action,
            reason,
            explanation: explanation.into(),
        }
    }
    pub fn deny(action: Action, reason: Reason, explanation: impl Into<String>) -> Self {
        Self {
            allowed: false,
            action,
            reason,
            explanation: explanation.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn company_role_round_trip() {
        for role in [
            CompanyRole::Owner,
            CompanyRole::Admin,
            CompanyRole::Operator,
            CompanyRole::Viewer,
            CompanyRole::Member,
        ] {
            assert_eq!(CompanyRole::from_str_opt(role.as_str()), Some(role));
        }
    }

    #[test]
    fn company_role_admin_or_above() {
        assert!(CompanyRole::Owner.is_admin_or_above());
        assert!(CompanyRole::Admin.is_admin_or_above());
        assert!(!CompanyRole::Operator.is_admin_or_above());
        assert!(!CompanyRole::Viewer.is_admin_or_above());
        assert!(!CompanyRole::Member.is_admin_or_above());
    }

    #[test]
    fn permission_key_as_str_matches_node() {
        assert_eq!(PermissionKey::AgentsCreate.as_str(), "agents:create");
        assert_eq!(PermissionKey::PipelinesWrite.as_str(), "pipelines:write");
        assert_eq!(PermissionKey::JoinsApprove.as_str(), "joins:approve");
    }

    #[test]
    fn action_as_str_for_typed_variants() {
        assert_eq!(Action::AgentRead.as_str(), "agent:read");
        assert_eq!(Action::IssueMutate.as_str(), "issue:mutate");
        assert_eq!(
            Action::Permission(PermissionKey::ToolsUse).as_str(),
            "tools:use"
        );
    }

    #[test]
    fn resource_company_id_extracts() {
        let c = Uuid::new_v4();
        let r = Resource::Company { company_id: c };
        assert_eq!(r.company_id(), c);
    }

    #[test]
    fn reason_is_allow_classification() {
        assert!(Reason::AllowInstanceAdmin.is_allow());
        assert!(Reason::AllowCompanyMember.is_allow());
        assert!(!Reason::DenyUnauthenticated.is_allow());
        assert!(!Reason::DenyCompanyBoundary.is_allow());
    }

    #[test]
    fn decision_allow_and_deny_helpers() {
        let d_ok = Decision::allow(
            Action::IssueRead,
            Reason::AllowCompanyMember,
            "actor is company member",
        );
        assert!(d_ok.allowed);
        assert_eq!(d_ok.reason, Reason::AllowCompanyMember);

        let d_no = Decision::deny(
            Action::IssueRead,
            Reason::DenyCompanyBoundary,
            "different company",
        );
        assert!(!d_no.allowed);
    }
}
