//! pc-authz：决策引擎。
//!
//! 与原 `paperclip/server/src/services/authorization.ts` 中 `evaluateAuthorization`
//! 的核心分支对齐：instance_admin 短路、company membership 角色门、agent key scope、
//! issue assignee / responsible user / mention grant。
//!
//! 该模块是**纯函数**：不带 IO、不访问数据库；调用方负责把 memberships、grants、issue
//! 上下文注入到 [`Context`] 中。

use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

use pc_auth::{Actor, CompanyMembership};

use crate::types::{
    Action, CompanyRole, Decision, PermissionKey, PrincipalType, Reason, Resource,
};

/// 决策时所需的 actor 上下文（与 Node `AuthorizationActor` 的核心字段对齐）。
#[derive(Debug, Clone, Default, Serialize)]
pub struct Context {
    /// 公司成员资格（user / agent）。
    pub memberships: Vec<CompanyMembership>,
    /// 该 principal 在该公司被授予的额外 permission key。
    pub grants: Vec<PermissionKey>,
    /// 当前 principal 在该公司内的角色（user）。
    pub role: Option<CompanyRole>,
    /// 实例管理员标志（user only）。
    pub is_instance_admin: bool,
    /// 是否本地隐式 board（开发模式，绕过认证）。
    pub is_local_board: bool,
    /// Issue 维度扩展（assignee / responsible / mentions）— 调用方负责从 issue row 注入。
    pub issue_assignee_user_id: Option<String>,
    pub issue_responsible_user_id: Option<String>,
    pub issue_mentioned_user_ids: Vec<String>,
    pub issue_assignee_agent_id: Option<Uuid>,
    /// Issue 中提及的 agent ids（用于 mention-based grant）。
    pub issue_mentioned_agent_ids: Vec<Uuid>,
    /// Run 是否由 agent 自己触发（self）。
    pub is_self_run: bool,
    /// Run 的 issue 是否已有 explicit grant（mention / assignee）。
    pub has_explicit_grant: bool,
    /// Issue 的 parent issue id（用于 parent-report grant）。
    pub issue_parent_id: Option<Uuid>,
    /// 当前 run 的 issue id（用于 parent-report：判断 actor 是否在 parent 上是 assignee）。
    pub actor_is_assignee_on_parent: bool,
    /// Grant scope 中的 `consentedChange` 标志（grant 是否包含 consented change scope）。
    pub has_consented_change_grant: bool,
    /// 当前请求是否为 issue create 或 issue comment（low-trust 内允许的操作）。
    pub is_low_trust_create_or_comment: bool,
}

impl Context {
    pub fn anonymous() -> Self {
        Self::default()
    }

    /// 构造 user 上下文。
    pub fn for_user(
        memberships: Vec<CompanyMembership>,
        grants: Vec<PermissionKey>,
        role: Option<CompanyRole>,
        is_instance_admin: bool,
    ) -> Self {
        Self {
            memberships,
            grants,
            role,
            is_instance_admin,
            ..Self::default()
        }
    }

    /// 构造 agent 上下文。
    pub fn for_agent(memberships: Vec<CompanyMembership>, grants: Vec<PermissionKey>) -> Self {
        Self {
            memberships,
            grants,
            ..Self::default()
        }
    }

    pub fn with_local_board(mut self) -> Self {
        self.is_local_board = true;
        self
    }

    pub fn with_issue(
        mut self,
        assignee_user_id: Option<String>,
        responsible_user_id: Option<String>,
        mentioned_user_ids: Vec<String>,
        assignee_agent_id: Option<Uuid>,
    ) -> Self {
        self.issue_assignee_user_id = assignee_user_id;
        self.issue_responsible_user_id = responsible_user_id;
        self.issue_mentioned_user_ids = mentioned_user_ids;
        self.issue_assignee_agent_id = assignee_agent_id;
        self
    }

    /// M43：注入 mention agent / parent / consent 信息。
    pub fn with_extended_issue(
        mut self,
        mentioned_agent_ids: Vec<Uuid>,
        parent_id: Option<Uuid>,
        actor_is_assignee_on_parent: bool,
        has_consented_change_grant: bool,
        is_low_trust_create_or_comment: bool,
    ) -> Self {
        self.issue_mentioned_agent_ids = mentioned_agent_ids;
        self.issue_parent_id = parent_id;
        self.actor_is_assignee_on_parent = actor_is_assignee_on_parent;
        self.has_consented_change_grant = has_consented_change_grant;
        self.is_low_trust_create_or_comment = is_low_trust_create_or_comment;
        self
    }

    pub fn with_self_run(mut self) -> Self {
        self.is_self_run = true;
        self
    }

    pub fn with_explicit_grant(mut self) -> Self {
        self.has_explicit_grant = true;
        self
    }

    pub fn has_membership(&self, company_id: Uuid) -> bool {
        self.memberships
            .iter()
            .any(|m| m.company_id == company_id && m.status.as_deref() == Some("active"))
    }

    pub fn has_grant(&self, key: PermissionKey) -> bool {
        self.grants.contains(&key)
    }
}

#[derive(Debug, Error, Serialize)]
pub enum AuthzError {
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("not authenticated")]
    Unauthenticated,
}

impl AuthzError {
    pub fn from_decision(d: &Decision) -> Self {
        if d.allowed {
            Self::Forbidden("decision was allowed".into())
        } else {
            Self::Forbidden(d.explanation.clone())
        }
    }
}

/// 核心决策入口。
///
/// 复刻 Node `evaluateAuthorization` 的核心短路顺序：
/// 1. System → allow_instance_admin 等价路径（system 全局 allow）
/// 2. Anonymous → deny_unauthenticated
/// 3. instance_admin (User) → allow_instance_admin
/// 4. is_local_board (开发模式) → allow_local_board
/// 5. 公司边界检查
/// 6. Agent: key scope + company agent 规则
/// 7. User: company membership + role + grants
/// 8. Issue 维度（assignee / responsible / mention）
pub fn evaluate(actor: &Actor, ctx: &Context, resource: &Resource, action: Action) -> Decision {
    let company_id = resource.company_id();

    // 1. System 全局允许
    if matches!(actor, Actor::System) {
        return Decision::allow(
            action,
            Reason::AllowInstanceAdmin,
            "system actor has full access",
        );
    }

    // 2. Anonymous 直接拒绝（除了开发模式 local_board）
    if matches!(actor, Actor::Anonymous) {
        if ctx.is_local_board {
            return Decision::allow(
                action,
                Reason::AllowLocalBoard,
                "anonymous local board in dev mode",
            );
        }
        return Decision::deny(
            action,
            Reason::DenyUnauthenticated,
            "actor is anonymous",
        );
    }

    match actor {
        Actor::User {
            id,
            is_instance_admin,
            ..
        } => evaluate_user(*is_instance_admin, id, ctx, resource, action, company_id),
        Actor::Agent { id, .. } => evaluate_agent(*id, ctx, resource, action, company_id),
        Actor::System => unreachable!("system handled above"),
        Actor::Anonymous => unreachable!("anonymous handled above"),
    }
}

fn evaluate_user(
    is_instance_admin: bool,
    user_id: &str,
    ctx: &Context,
    resource: &Resource,
    action: Action,
    company_id: Uuid,
) -> Decision {
    // 3. Instance admin 短路
    if is_instance_admin {
        return Decision::allow(
            action,
            Reason::AllowInstanceAdmin,
            "user is instance admin",
        );
    }

    // 4. 本地 board（开发模式）
    if ctx.is_local_board {
        return Decision::allow(action, Reason::AllowLocalBoard, "user is local board");
    }

    // 5. 公司成员资格
    if !ctx.has_membership(company_id) {
        return Decision::deny(
            action,
            Reason::DenyCompanyBoundary,
            format!(
                "user {user_id} has no active membership in company {company_id}"
            ),
        );
    }

    let role = ctx.role.unwrap_or(CompanyRole::Member);

    // 6. Issue 维度特殊判断
    if let Resource::Issue {
        assignee_user_id,
        assignee_agent_id,
        ..
    } = resource
    {
        // assignee 直接改自己被分配的 issue
        if let Some(assignee) = assignee_user_id {
            if assignee == user_id && matches!(action, Action::IssueMutate | Action::IssueComment)
            {
                return Decision::allow(
                    action,
                    Reason::AllowDirectChange,
                    "user is the issue assignee",
                );
            }
        }
        // responsible user 可以 mutate/comment
        if let Some(resp) = &ctx.issue_responsible_user_id {
            if resp == user_id && matches!(action, Action::IssueMutate | Action::IssueComment) {
                return Decision::allow(
                    action,
                    Reason::AllowDirectChange,
                    "user is the issue responsible user",
                );
            }
        }
        // 提及 grant
        if ctx.issue_mentioned_user_ids.iter().any(|m| m == user_id)
            && matches!(action, Action::IssueComment | Action::IssueMutate)
        {
            return Decision::allow(
                action,
                Reason::AllowIssueMentionGrant,
                "user has explicit mention grant on issue",
            );
        }
        // Consent gate：grant scope 包含 consentedChange
        if ctx.has_consented_change_grant && matches!(action, Action::IssueMutate) {
            return Decision::allow(
                action,
                Reason::AllowConsentedChange,
                "user has consented change grant",
            );
        }
        let _ = assignee_agent_id; // 用于后续 agent 自指派场景
    }

    // 7. Permission key 匹配：先看 grants，再看 role
    if let Action::Permission(perm) = action {
        if ctx.has_grant(perm) {
            return Decision::allow(
                action,
                Reason::AllowExplicitGrant,
                format!("user has explicit grant for {}", perm.as_str()),
            );
        }
        if role_meets(perm.default_required_role(), role) {
            return Decision::allow(
                action,
                Reason::AllowSimpleCompanyMember,
                format!(
                    "role {} meets required {} for {}",
                    role.as_str(),
                    perm.default_required_role().as_str(),
                    perm.as_str()
                ),
            );
        }
        return Decision::deny(
            action,
            Reason::DenyMissingGrant,
            format!(
                "role {} insufficient for {} (requires {})",
                role.as_str(),
                perm.as_str(),
                perm.default_required_role().as_str()
            ),
        );
    }

    // 8. 特殊 Action 默认规则
    match action {
        Action::IssueRead | Action::ProjectRead | Action::AgentRead | Action::CompanyScopeRead => {
            Decision::allow(
                action,
                Reason::AllowCompanyMember,
                "company member can read these resources",
            )
        }
        Action::IssueComment => Decision::allow(
            action,
            Reason::AllowCompanyMember,
            "company member can comment on issues",
        ),
        Action::SecretsRead => Decision::allow(
            action,
            Reason::AllowCompanyMember,
            "company member can read non-secret secrets metadata",
        ),
        Action::RuntimeManage => {
            if role.is_admin_or_above() {
                Decision::allow(
                    action,
                    Reason::AllowCompanyMember,
                    "admin can manage runtime",
                )
            } else {
                Decision::deny(
                    action,
                    Reason::DenyPolicyRestricted,
                    "runtime:manage requires admin role",
                )
            }
        }
        Action::IssueMutate => {
            // 已由 issue 维度 above 处理过（assignee / mention），非 assignee 默认拒绝
            if ctx.has_explicit_grant {
                Decision::allow(
                    action,
                    Reason::AllowExplicitGrant,
                    "user has explicit issue grant",
                )
            } else if role.is_write() {
                Decision::allow(
                    action,
                    Reason::AllowCompanyMember,
                    "write-capable member can mutate issues",
                )
            } else {
                Decision::deny(
                    action,
                    Reason::DenyMissingGrant,
                    "viewer/member cannot mutate issues without explicit grant",
                )
            }
        }
        Action::AgentConfigRead | Action::AgentWake | Action::SkillConfigUpdate => {
            if role.is_write() {
                Decision::allow(
                    action,
                    Reason::AllowCompanyMember,
                    "write-capable member allowed",
                )
            } else {
                Decision::deny(
                    action,
                    Reason::DenyPolicyRestricted,
                    "viewer cannot perform this action",
                )
            }
        }
        Action::AgentConfigUpdate => {
            if role.is_admin_or_above() {
                Decision::allow(
                    action,
                    Reason::AllowCompanyMember,
                    "admin can update agent config",
                )
            } else {
                Decision::deny(
                    action,
                    Reason::DenyPolicyRestricted,
                    "agent_config:update requires admin",
                )
            }
        }
        // Permission(_) 已在上方 return，不应到这里。
        Action::Permission(_) => unreachable!("permission key handled above"),
    }
}

fn evaluate_agent(
    agent_id: Uuid,
    ctx: &Context,
    resource: &Resource,
    action: Action,
    company_id: Uuid,
) -> Decision {
    // Agent 必须属于该公司
    if !ctx.has_membership(company_id) {
        return Decision::deny(
            action,
            Reason::DenyCompanyBoundary,
            format!("agent {agent_id} not in company {company_id}"),
        );
    }

    // Issue 维度：self / assignee / mention / parent-report / consent
    if let Resource::Issue {
        assignee_agent_id,
        ..
    } = resource
    {
        if let Some(assignee) = assignee_agent_id {
            if *assignee == agent_id && matches!(action, Action::IssueComment | Action::IssueMutate)
            {
                return Decision::allow(
                    action,
                    Reason::AllowSelf,
                    "agent is the issue assignee",
                );
            }
        }
        // Mention grant：当前 actor agent 在 issue 中被 @ 到
        if ctx.issue_mentioned_agent_ids.contains(&agent_id)
            && matches!(action, Action::IssueComment | Action::IssueMutate)
        {
            return Decision::allow(
                action,
                Reason::AllowIssueMentionGrant,
                "agent has mention grant on the issue",
            );
        }
        // Parent-report：actor 在 parent issue 上是 assignee，可以在子 issue 上 comment
        if ctx.actor_is_assignee_on_parent
            && ctx.issue_parent_id.is_some()
            && matches!(action, Action::IssueComment)
        {
            return Decision::allow(
                action,
                Reason::AllowDirectParentReport,
                "agent is assignee on parent issue; reporting back is allowed",
            );
        }
        // Consent gate：grant scope 包含 consentedChange
        if ctx.has_consented_change_grant && matches!(action, Action::IssueMutate) {
            return Decision::allow(
                action,
                Reason::AllowConsentedChange,
                "agent has consented change grant",
            );
        }
        // Self-run：agent 在自己的 run 上 mutate/comment
        if ctx.is_self_run && matches!(action, Action::IssueComment | Action::IssueMutate) {
            return Decision::allow(
                action,
                Reason::AllowSelf,
                "agent acting on its own run",
            );
        }
    }

    // Grant 优先
    if let Action::Permission(perm) = action {
        if ctx.has_grant(perm) {
            return Decision::allow(
                action,
                Reason::AllowExplicitGrant,
                "agent has explicit grant",
            );
        }
        // Agent 默认无写权限（除非 issue self）
        if matches!(action, Action::IssueRead | Action::ProjectRead | Action::AgentRead) {
            return Decision::allow(
                action,
                Reason::AllowCompanyAgent,
                "agent can read company-scoped resources",
            );
        }
        return Decision::deny(
            action,
            Reason::DenyNoGrant,
            format!("agent has no grant for {}", perm.as_str()),
        );
    }

    match action {
        Action::IssueRead
        | Action::ProjectRead
        | Action::AgentRead
        | Action::CompanyScopeRead => Decision::allow(
            action,
            Reason::AllowCompanyAgent,
            "agent in company can read",
        ),
        Action::IssueComment | Action::IssueMutate => {
            // 已由 issue 维度短路；非 self 拒绝
            Decision::deny(
                action,
                Reason::DenyPolicyRestricted,
                "agent cannot mutate issues outside its own assignment",
            )
        }
        _ => Decision::deny(
            action,
            Reason::DenyPolicyRestricted,
            "agent policy does not allow this action",
        ),
    }
}

fn role_meets(required: CompanyRole, actual: CompanyRole) -> bool {
    use CompanyRole::*;
    // role >= required（按权限大小排列）
    let rank = |r: CompanyRole| -> u8 {
        match r {
            Owner => 4,
            Admin => 3,
            Operator => 2,
            Member => 1,
            Viewer => 0,
        }
    };
    rank(actual) >= rank(required)
}

/// 便捷函数：把 Decision 转 Result，allow 则返回 Ok(()), deny 则返回 AuthzError。
pub fn check(actor: &Actor, ctx: &Context, resource: &Resource, action: Action) -> Result<(), AuthzError> {
    let d = evaluate(actor, ctx, resource, action);
    if d.allowed {
        Ok(())
    } else {
        Err(AuthzError::from_decision(&d))
    }
}

/// PrincipalType 辅助：把 Actor 转 PrincipalType（用于 grants 表的 principal_type 字段）。
pub fn principal_type_of(actor: &Actor) -> Option<PrincipalType> {
    match actor {
        Actor::User { .. } => Some(PrincipalType::User),
        Actor::Agent { .. } => Some(PrincipalType::Agent),
        Actor::System | Actor::Anonymous => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pc_auth::Actor;

    fn user_actor(id: &str, admin: bool) -> Actor {
        Actor::User {
            id: id.into(),
            name: None,
            email: None,
            is_instance_admin: admin,
            company_ids: vec![],
            memberships: vec![],
            run_id: None,
        }
    }

    fn agent_actor(id: Uuid, company: Uuid) -> Actor {
        Actor::Agent {
            id,
            company_id: company,
            key_id: None,
            key_scope: Default::default(),
            run_id: None,
            on_behalf_of_user_id: None,
            on_behalf_of_memberships: vec![],
        }
    }

    fn membership(company: Uuid) -> CompanyMembership {
        CompanyMembership {
            company_id: company,
            role: Some("admin".into()),
            status: Some("active".into()),
        }
    }

    fn operator_membership(company: Uuid) -> CompanyMembership {
        CompanyMembership {
            company_id: company,
            role: Some("operator".into()),
            status: Some("active".into()),
        }
    }

    #[test]
    fn system_is_universal_allow() {
        let d = evaluate(
            &Actor::System,
            &Context::anonymous(),
            &Resource::Company {
                company_id: Uuid::new_v4(),
            },
            Action::IssueMutate,
        );
        assert!(d.allowed);
        assert_eq!(d.reason, Reason::AllowInstanceAdmin);
    }

    #[test]
    fn anonymous_is_denied_by_default() {
        let d = evaluate(
            &Actor::Anonymous,
            &Context::anonymous(),
            &Resource::Company {
                company_id: Uuid::new_v4(),
            },
            Action::IssueRead,
        );
        assert!(!d.allowed);
        assert_eq!(d.reason, Reason::DenyUnauthenticated);
    }

    #[test]
    fn anonymous_local_board_allowed() {
        let d = evaluate(
            &Actor::Anonymous,
            &Context::anonymous().with_local_board(),
            &Resource::Company {
                company_id: Uuid::new_v4(),
            },
            Action::IssueRead,
        );
        assert!(d.allowed);
        assert_eq!(d.reason, Reason::AllowLocalBoard);
    }

    #[test]
    fn instance_admin_short_circuits() {
        let d = evaluate(
            &user_actor("u1", true),
            &Context::anonymous(),
            &Resource::Company {
                company_id: Uuid::new_v4(),
            },
            Action::Permission(PermissionKey::JoinsApprove),
        );
        assert!(d.allowed);
        assert_eq!(d.reason, Reason::AllowInstanceAdmin);
    }

    #[test]
    fn user_without_membership_denied_cross_company() {
        let c = Uuid::new_v4();
        let other = Uuid::new_v4();
        let ctx = Context::for_user(vec![membership(c)], vec![], Some(CompanyRole::Admin), false);
        let d = evaluate(
            &user_actor("u1", false),
            &ctx,
            &Resource::Company { company_id: other },
            Action::IssueRead,
        );
        assert!(!d.allowed);
        assert_eq!(d.reason, Reason::DenyCompanyBoundary);
    }

    #[test]
    fn admin_can_approve_joins() {
        let c = Uuid::new_v4();
        let ctx = Context::for_user(vec![membership(c)], vec![], Some(CompanyRole::Admin), false);
        let d = evaluate(
            &user_actor("u1", false),
            &ctx,
            &Resource::Company { company_id: c },
            Action::Permission(PermissionKey::JoinsApprove),
        );
        assert!(d.allowed);
        assert_eq!(d.reason, Reason::AllowSimpleCompanyMember);
    }

    #[test]
    fn operator_cannot_approve_joins() {
        let c = Uuid::new_v4();
        let ctx = Context::for_user(
            vec![operator_membership(c)],
            vec![],
            Some(CompanyRole::Operator),
            false,
        );
        let d = evaluate(
            &user_actor("u1", false),
            &ctx,
            &Resource::Company { company_id: c },
            Action::Permission(PermissionKey::JoinsApprove),
        );
        assert!(!d.allowed);
        assert_eq!(d.reason, Reason::DenyMissingGrant);
    }

    #[test]
    fn explicit_grant_overrides_role() {
        let c = Uuid::new_v4();
        let ctx = Context::for_user(
            vec![operator_membership(c)],
            vec![PermissionKey::JoinsApprove],
            Some(CompanyRole::Operator),
            false,
        );
        let d = evaluate(
            &user_actor("u1", false),
            &ctx,
            &Resource::Company { company_id: c },
            Action::Permission(PermissionKey::JoinsApprove),
        );
        assert!(d.allowed);
        assert_eq!(d.reason, Reason::AllowExplicitGrant);
    }

    #[test]
    fn issue_assignee_can_mutate() {
        let c = Uuid::new_v4();
        let ctx = Context::for_user(
            vec![membership(c)],
            vec![],
            Some(CompanyRole::Viewer),
            false,
        );
        let d = evaluate(
            &user_actor("u1", false),
            &ctx,
            &Resource::Issue {
                company_id: c,
                issue_id: None,
                project_id: None,
                parent_issue_id: None,
                assignee_agent_id: None,
                assignee_user_id: Some("u1".into()),
                origin_kind: None,
                origin_id: None,
                status: None,
            },
            Action::IssueMutate,
        );
        assert!(d.allowed);
        assert_eq!(d.reason, Reason::AllowDirectChange);
    }

    #[test]
    fn mentioned_user_can_comment() {
        let c = Uuid::new_v4();
        let ctx = Context::for_user(
            vec![membership(c)],
            vec![],
            Some(CompanyRole::Viewer),
            false,
        )
        .with_issue(None, None, vec!["u1".into()], None);
        let d = evaluate(
            &user_actor("u1", false),
            &ctx,
            &Resource::Issue {
                company_id: c,
                issue_id: None,
                project_id: None,
                parent_issue_id: None,
                assignee_agent_id: None,
                assignee_user_id: None,
                origin_kind: None,
                origin_id: None,
                status: None,
            },
            Action::IssueComment,
        );
        assert!(d.allowed);
        assert_eq!(d.reason, Reason::AllowIssueMentionGrant);
    }

    #[test]
    fn agent_assignee_can_mutate_self() {
        let c = Uuid::new_v4();
        let agent_id = Uuid::new_v4();
        let ctx = Context::for_agent(vec![membership(c)], vec![]);
        let d = evaluate(
            &agent_actor(agent_id, c),
            &ctx,
            &Resource::Issue {
                company_id: c,
                issue_id: None,
                project_id: None,
                parent_issue_id: None,
                assignee_agent_id: Some(agent_id),
                assignee_user_id: None,
                origin_kind: None,
                origin_id: None,
                status: None,
            },
            Action::IssueMutate,
        );
        assert!(d.allowed);
        assert_eq!(d.reason, Reason::AllowSelf);
    }

    #[test]
    fn agent_cross_company_denied() {
        let c1 = Uuid::new_v4();
        let c2 = Uuid::new_v4();
        let agent_id = Uuid::new_v4();
        let ctx = Context::for_agent(vec![membership(c1)], vec![]);
        let d = evaluate(
            &agent_actor(agent_id, c1),
            &ctx,
            &Resource::Company { company_id: c2 },
            Action::IssueRead,
        );
        assert!(!d.allowed);
        assert_eq!(d.reason, Reason::DenyCompanyBoundary);
    }

    #[test]
    fn agent_no_grant_cannot_write() {
        let c = Uuid::new_v4();
        let agent_id = Uuid::new_v4();
        let ctx = Context::for_agent(vec![membership(c)], vec![]);
        let d = evaluate(
            &agent_actor(agent_id, c),
            &ctx,
            &Resource::Company { company_id: c },
            Action::Permission(PermissionKey::PipelinesWrite),
        );
        assert!(!d.allowed);
        assert_eq!(d.reason, Reason::DenyNoGrant);
    }

    #[test]
    fn check_returns_ok_for_allow() {
        let c = Uuid::new_v4();
        let ctx = Context::for_user(vec![membership(c)], vec![], Some(CompanyRole::Admin), false);
        assert!(check(
            &user_actor("u1", false),
            &ctx,
            &Resource::Company { company_id: c },
            Action::Permission(PermissionKey::AgentsCreate),
        )
        .is_ok());
    }

    #[test]
    fn check_returns_err_for_deny() {
        let c = Uuid::new_v4();
        let ctx = Context::anonymous();
        assert!(check(
            &Actor::Anonymous,
            &ctx,
            &Resource::Company { company_id: c },
            Action::IssueRead,
        )
        .is_err());
    }

    #[test]
    fn user_viewer_cannot_mutate_without_assignment() {
        let c = Uuid::new_v4();
        let ctx = Context::for_user(
            vec![membership(c)],
            vec![],
            Some(CompanyRole::Viewer),
            false,
        );
        let d = evaluate(
            &user_actor("u1", false),
            &ctx,
            &Resource::Issue {
                company_id: c,
                issue_id: None,
                project_id: None,
                parent_issue_id: None,
                assignee_agent_id: None,
                assignee_user_id: None,
                origin_kind: None,
                origin_id: None,
                status: None,
            },
            Action::IssueMutate,
        );
        // viewer 角色无 assignment / responsible / grants 时不能 mutate
        assert!(!d.allowed, "viewer with no assignment cannot mutate");
    }

    #[test]
    fn user_responsible_user_can_mutate() {
        let c = Uuid::new_v4();
        let ctx = Context::for_user(
            vec![membership(c)],
            vec![],
            Some(CompanyRole::Viewer),
            false,
        );
        let d = evaluate(
            &user_actor("u1", false),
            &ctx.with_issue(
                None,
                Some("u1".into()),
                vec![],
                None,
            ),
            &Resource::Issue {
                company_id: c,
                issue_id: None,
                project_id: None,
                parent_issue_id: None,
                assignee_agent_id: None,
                assignee_user_id: None,
                origin_kind: None,
                origin_id: None,
                status: None,
            },
            Action::IssueMutate,
        );
        assert!(d.allowed);
        assert_eq!(d.reason, Reason::AllowDirectChange);
    }

    #[test]
    fn agent_mention_grant_allows_comment() {
        let c = Uuid::new_v4();
        let agent_id = Uuid::new_v4();
        let ctx = Context::for_agent(vec![membership(c)], vec![]);
        let d = evaluate(
            &agent_actor(agent_id, c),
            &ctx.with_extended_issue(
                vec![agent_id],
                None,
                false,
                false,
                false,
            ),
            &Resource::Issue {
                company_id: c,
                issue_id: None,
                project_id: None,
                parent_issue_id: None,
                assignee_agent_id: None,
                assignee_user_id: None,
                origin_kind: None,
                origin_id: None,
                status: None,
            },
            Action::IssueComment,
        );
        assert!(d.allowed);
        assert_eq!(d.reason, Reason::AllowIssueMentionGrant);
    }

    #[test]
    fn agent_parent_report_allows_comment() {
        let c = Uuid::new_v4();
        let agent_id = Uuid::new_v4();
        let parent_id = Uuid::new_v4();
        let ctx = Context::for_agent(vec![membership(c)], vec![]);
        let d = evaluate(
            &agent_actor(agent_id, c),
            &ctx.with_extended_issue(
                vec![],
                Some(parent_id),
                true,
                false,
                false,
            ),
            &Resource::Issue {
                company_id: c,
                issue_id: None,
                project_id: None,
                parent_issue_id: Some(parent_id),
                assignee_agent_id: None,
                assignee_user_id: None,
                origin_kind: None,
                origin_id: None,
                status: None,
            },
            Action::IssueComment,
        );
        assert!(d.allowed);
        assert_eq!(d.reason, Reason::AllowDirectParentReport);
    }

    #[test]
    fn agent_consent_grant_allows_mutate() {
        let c = Uuid::new_v4();
        let agent_id = Uuid::new_v4();
        let ctx = Context::for_agent(vec![membership(c)], vec![]);
        let d = evaluate(
            &agent_actor(agent_id, c),
            &ctx.with_extended_issue(
                vec![],
                None,
                false,
                true,
                false,
            ),
            &Resource::Issue {
                company_id: c,
                issue_id: None,
                project_id: None,
                parent_issue_id: None,
                assignee_agent_id: None,
                assignee_user_id: None,
                origin_kind: None,
                origin_id: None,
                status: None,
            },
            Action::IssueMutate,
        );
        assert!(d.allowed);
        assert_eq!(d.reason, Reason::AllowConsentedChange);
    }

    #[test]
    fn principal_type_of_returns_correct_variant() {
        assert_eq!(
            principal_type_of(&user_actor("u1", false)),
            Some(PrincipalType::User)
        );
        assert_eq!(
            principal_type_of(&agent_actor(Uuid::new_v4(), Uuid::new_v4())),
            Some(PrincipalType::Agent)
        );
        assert_eq!(principal_type_of(&Actor::System), None);
        assert_eq!(principal_type_of(&Actor::Anonymous), None);
    }
}
