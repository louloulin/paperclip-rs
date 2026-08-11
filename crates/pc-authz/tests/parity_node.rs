//! pc-authz：与 Node `authorization-service.test.ts` 对齐的端到端测试。
//!
//! 覆盖核心决策分支（system / anonymous / instance_admin / company membership /
//! grants / role / issue self / mention / consent / parent-report），所有断言基于
//! pc-authz 的纯函数 `evaluate()`，无需 DB。

use pc_auth::Actor;
use pc_authz::{evaluate, Action, CompanyRole, Context, PermissionKey, Reason, Resource};
use uuid::Uuid;

fn system() -> Actor {
    Actor::System
}
fn anonymous() -> Actor {
    Actor::Anonymous
}
fn user(id: &str, admin: bool) -> Actor {
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
fn agent(id: Uuid, company: Uuid) -> Actor {
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
fn membership(company: Uuid, role: Option<CompanyRole>) -> pc_auth::CompanyMembership {
    pc_auth::CompanyMembership {
        company_id: company,
        role: role.map(|r| r.as_str().to_string()),
        status: Some("active".into()),
    }
}
fn company_resource(c: Uuid) -> Resource {
    Resource::Company { company_id: c }
}
fn issue_resource(c: Uuid) -> Resource {
    Resource::Issue {
        company_id: c,
        issue_id: None,
        project_id: None,
        parent_issue_id: None,
        assignee_agent_id: None,
        assignee_user_id: None,
        origin_kind: None,
        origin_id: None,
        status: None,
    }
}

#[test]
fn parity_user_role_grant_allows_tasks_assign() {
    // 对应 Node "allows active user role grants and explains the grant source"
    let c = Uuid::new_v4();
    let actor = user("u1", false);
    let ctx = Context::for_user(
        vec![membership(c, Some(CompanyRole::Operator))],
        vec![PermissionKey::TasksAssign],
        Some(CompanyRole::Operator),
        false,
    );
    let d = evaluate(
        &actor,
        &ctx,
        &company_resource(c),
        Action::Permission(PermissionKey::TasksAssign),
    );
    assert!(d.allowed);
    assert_eq!(d.reason, Reason::AllowExplicitGrant);
}

#[test]
fn parity_agent_suggest_grant_allows_agent_config_read() {
    // 对应 Node "allows suggest grants to read peer agent configuration"
    let c = Uuid::new_v4();
    let actor_id = Uuid::new_v4();
    let actor = agent(actor_id, c);
    let ctx = Context::for_agent(
        vec![membership(c, None)],
        vec![PermissionKey::AgentsSuggestChanges],
    );
    let d = evaluate(
        &actor,
        &ctx,
        &Resource::Agent {
            company_id: c,
            agent_id: Some(actor_id),
        },
        Action::Permission(PermissionKey::AgentsSuggestChanges),
    );
    assert!(d.allowed);
    assert_eq!(d.reason, Reason::AllowExplicitGrant);
}

#[test]
fn parity_instance_admin_short_circuits_all_actions() {
    let c = Uuid::new_v4();
    let actor = user("admin", true);
    let ctx = Context::anonymous();
    let actions = [
        Action::Permission(PermissionKey::JoinsApprove),
        Action::Permission(PermissionKey::ToolsAdmin),
        Action::IssueMutate,
        Action::SecretsRead,
    ];
    for action in actions {
        let d = evaluate(&actor, &ctx, &company_resource(c), action);
        assert!(d.allowed, "instance_admin should allow {action:?}: {d:?}");
        assert_eq!(d.reason, Reason::AllowInstanceAdmin);
    }
}

#[test]
fn parity_anonymous_is_always_denied() {
    let c = Uuid::new_v4();
    let actor = anonymous();
    let ctx = Context::anonymous();
    let d = evaluate(&actor, &ctx, &company_resource(c), Action::IssueRead);
    assert!(!d.allowed);
    assert_eq!(d.reason, Reason::DenyUnauthenticated);
}

#[test]
fn parity_system_is_universal_allow() {
    let c = Uuid::new_v4();
    let actor = system();
    let ctx = Context::anonymous();
    for action in [
        Action::IssueMutate,
        Action::SecretsRead,
        Action::Permission(PermissionKey::ToolsAdmin),
    ] {
        let d = evaluate(&actor, &ctx, &company_resource(c), action);
        assert!(d.allowed);
        assert_eq!(d.reason, Reason::AllowInstanceAdmin);
    }
}

#[test]
fn parity_cross_company_is_denied_for_user() {
    let c1 = Uuid::new_v4();
    let c2 = Uuid::new_v4();
    let actor = user("u1", false);
    let ctx = Context::for_user(
        vec![membership(c1, Some(CompanyRole::Admin))],
        vec![],
        Some(CompanyRole::Admin),
        false,
    );
    let d = evaluate(
        &actor,
        &ctx,
        &company_resource(c2),
        Action::Permission(PermissionKey::JoinsApprove),
    );
    assert!(!d.allowed);
    assert_eq!(d.reason, Reason::DenyCompanyBoundary);
}

#[test]
fn parity_admin_role_unlocks_admin_actions() {
    let c = Uuid::new_v4();
    let actor = user("u1", false);
    let ctx = Context::for_user(
        vec![membership(c, Some(CompanyRole::Admin))],
        vec![],
        Some(CompanyRole::Admin),
        false,
    );
    for key in [
        PermissionKey::JoinsApprove,
        PermissionKey::EnvironmentsManage,
        PermissionKey::ToolsAdmin,
        PermissionKey::UsersManagePermissions,
    ] {
        let d = evaluate(&actor, &ctx, &company_resource(c), Action::Permission(key));
        assert!(d.allowed, "{key:?} should be allowed for admin");
        assert_eq!(d.reason, Reason::AllowSimpleCompanyMember);
    }
}

#[test]
fn parity_operator_role_lacks_admin_only_keys() {
    let c = Uuid::new_v4();
    let actor = user("u1", false);
    let ctx = Context::for_user(
        vec![membership(c, Some(CompanyRole::Operator))],
        vec![],
        Some(CompanyRole::Operator),
        false,
    );
    for key in [
        PermissionKey::JoinsApprove,
        PermissionKey::EnvironmentsManage,
        PermissionKey::ToolsAdmin,
        PermissionKey::UsersManagePermissions,
    ] {
        let d = evaluate(&actor, &ctx, &company_resource(c), Action::Permission(key));
        assert!(!d.allowed, "{key:?} should be denied for operator");
        assert_eq!(d.reason, Reason::DenyMissingGrant);
    }
}

#[test]
fn parity_issue_assignee_can_mutate() {
    let c = Uuid::new_v4();
    let actor = user("u1", false);
    let ctx = Context::for_user(
        vec![membership(c, Some(CompanyRole::Viewer))],
        vec![],
        Some(CompanyRole::Viewer),
        false,
    );
    let mut resource = issue_resource(c);
    if let Resource::Issue {
        assignee_user_id, ..
    } = &mut resource
    {
        *assignee_user_id = Some("u1".into());
    }
    let d = evaluate(&actor, &ctx, &resource, Action::IssueMutate);
    assert!(d.allowed);
    assert_eq!(d.reason, Reason::AllowDirectChange);
}

#[test]
fn parity_issue_mention_grant_for_user() {
    let c = Uuid::new_v4();
    let actor = user("u1", false);
    let ctx = Context::for_user(
        vec![membership(c, Some(CompanyRole::Viewer))],
        vec![],
        Some(CompanyRole::Viewer),
        false,
    )
    .with_issue(None, None, vec!["u1".into()], None);
    let d = evaluate(&actor, &ctx, &issue_resource(c), Action::IssueComment);
    assert!(d.allowed);
    assert_eq!(d.reason, Reason::AllowIssueMentionGrant);
}

#[test]
fn parity_responsible_user_can_mutate_issue() {
    let c = Uuid::new_v4();
    let actor = user("u1", false);
    let ctx = Context::for_user(
        vec![membership(c, Some(CompanyRole::Viewer))],
        vec![],
        Some(CompanyRole::Viewer),
        false,
    )
    .with_issue(None, Some("u1".into()), vec![], None);
    let d = evaluate(&actor, &ctx, &issue_resource(c), Action::IssueMutate);
    assert!(d.allowed);
    assert_eq!(d.reason, Reason::AllowDirectChange);
}

#[test]
fn parity_agent_self_via_assignee_can_mutate() {
    let c = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let actor = agent(agent_id, c);
    let ctx = Context::for_agent(vec![membership(c, None)], vec![]);
    let mut resource = issue_resource(c);
    if let Resource::Issue {
        assignee_agent_id, ..
    } = &mut resource
    {
        *assignee_agent_id = Some(agent_id);
    }
    let d = evaluate(&actor, &ctx, &resource, Action::IssueMutate);
    assert!(d.allowed);
    assert_eq!(d.reason, Reason::AllowSelf);
}

#[test]
fn parity_agent_self_run_allows_comment() {
    let c = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let actor = agent(agent_id, c);
    let ctx = Context::for_agent(vec![membership(c, None)], vec![]).with_self_run();
    let d = evaluate(&actor, &ctx, &issue_resource(c), Action::IssueComment);
    assert!(d.allowed);
    assert_eq!(d.reason, Reason::AllowSelf);
}

#[test]
fn parity_agent_mention_grant_allows_comment() {
    let c = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let actor = agent(agent_id, c);
    let ctx = Context::for_agent(vec![membership(c, None)], vec![]).with_extended_issue(
        vec![agent_id],
        None,
        false,
        false,
        false,
    );
    let d = evaluate(&actor, &ctx, &issue_resource(c), Action::IssueComment);
    assert!(d.allowed);
    assert_eq!(d.reason, Reason::AllowIssueMentionGrant);
}

#[test]
fn parity_agent_parent_report_allows_comment() {
    let c = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let parent_id = Uuid::new_v4();
    let actor = agent(agent_id, c);
    let ctx = Context::for_agent(vec![membership(c, None)], vec![]).with_extended_issue(
        vec![],
        Some(parent_id),
        true,
        false,
        false,
    );
    let mut resource = issue_resource(c);
    if let Resource::Issue {
        parent_issue_id, ..
    } = &mut resource
    {
        *parent_issue_id = Some(parent_id);
    }
    let d = evaluate(&actor, &ctx, &resource, Action::IssueComment);
    assert!(d.allowed);
    assert_eq!(d.reason, Reason::AllowDirectParentReport);
}

#[test]
fn parity_agent_consent_grant_allows_mutate() {
    let c = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let actor = agent(agent_id, c);
    let ctx = Context::for_agent(vec![membership(c, None)], vec![]).with_extended_issue(
        vec![],
        None,
        false,
        true,
        false,
    );
    let d = evaluate(&actor, &ctx, &issue_resource(c), Action::IssueMutate);
    assert!(d.allowed);
    assert_eq!(d.reason, Reason::AllowConsentedChange);
}

#[test]
fn parity_agent_without_grant_cannot_write() {
    let c = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let actor = agent(agent_id, c);
    let ctx = Context::for_agent(vec![membership(c, None)], vec![]);
    for key in [
        PermissionKey::PipelinesWrite,
        PermissionKey::JoinsApprove,
        PermissionKey::UsersInvite,
        PermissionKey::ToolsAdmin,
    ] {
        let d = evaluate(&actor, &ctx, &company_resource(c), Action::Permission(key));
        assert!(!d.allowed, "agent without grant should not write {key:?}");
        assert_eq!(d.reason, Reason::DenyNoGrant);
    }
}

#[test]
fn parity_agent_can_read_company_resources() {
    let c = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let actor = agent(agent_id, c);
    let ctx = Context::for_agent(vec![membership(c, None)], vec![]);
    for action in [
        Action::IssueRead,
        Action::ProjectRead,
        Action::AgentRead,
        Action::CompanyScopeRead,
    ] {
        let d = evaluate(&actor, &ctx, &company_resource(c), action);
        assert!(d.allowed, "agent should be able to {action:?}");
        assert_eq!(d.reason, Reason::AllowCompanyAgent);
    }
}

#[test]
fn parity_company_member_can_read_by_default() {
    let c = Uuid::new_v4();
    let actor = user("u1", false);
    let ctx = Context::for_user(
        vec![membership(c, Some(CompanyRole::Member))],
        vec![],
        Some(CompanyRole::Member),
        false,
    );
    for action in [
        Action::IssueRead,
        Action::ProjectRead,
        Action::AgentRead,
        Action::CompanyScopeRead,
        Action::IssueComment,
    ] {
        let d = evaluate(&actor, &ctx, &company_resource(c), action);
        assert!(d.allowed, "member should be able to {action:?}");
        assert_eq!(d.reason, Reason::AllowCompanyMember);
    }
}

#[test]
fn parity_viewer_cannot_mutate_without_assignment() {
    let c = Uuid::new_v4();
    let actor = user("u1", false);
    let ctx = Context::for_user(
        vec![membership(c, Some(CompanyRole::Viewer))],
        vec![],
        Some(CompanyRole::Viewer),
        false,
    );
    let d = evaluate(&actor, &ctx, &issue_resource(c), Action::IssueMutate);
    assert!(!d.allowed);
    assert_eq!(d.reason, Reason::DenyMissingGrant);
}

#[test]
fn parity_pending_membership_is_denied() {
    let c = Uuid::new_v4();
    let actor = user("u1", false);
    let mut m = membership(c, Some(CompanyRole::Admin));
    m.status = Some("pending".into());
    let ctx = Context::for_user(vec![m], vec![], Some(CompanyRole::Admin), false);
    let d = evaluate(
        &actor,
        &ctx,
        &company_resource(c),
        Action::Permission(PermissionKey::JoinsApprove),
    );
    assert!(!d.allowed);
    assert_eq!(d.reason, Reason::DenyCompanyBoundary);
}

#[test]
fn parity_grant_overrides_insufficient_role() {
    let c = Uuid::new_v4();
    let actor = user("u1", false);
    let ctx = Context::for_user(
        vec![membership(c, Some(CompanyRole::Viewer))],
        vec![PermissionKey::JoinsApprove],
        Some(CompanyRole::Viewer),
        false,
    );
    let d = evaluate(
        &actor,
        &ctx,
        &company_resource(c),
        Action::Permission(PermissionKey::JoinsApprove),
    );
    assert!(d.allowed);
    assert_eq!(d.reason, Reason::AllowExplicitGrant);
}

#[test]
fn r555_mentions_helpers_in_public_api() {
    // 烟雾测试：确认 mention 提取 + helper 公开 API 可达。
    use pc_authz::{
        build_context_with_issue_body, extract_agent_mention_ids, extract_user_mention_ids,
    };
    use uuid::Uuid;
    let id = Uuid::new_v4();
    let body = format!("Hi [claude](agent://{id}?i=claude) and [alice](user://alice)");
    let agents = extract_agent_mention_ids(&body);
    assert_eq!(agents, vec![id]);
    let users = extract_user_mention_ids(&body);
    assert_eq!(users, vec!["alice".to_string()]);
    // 确认 high-level helper 也是可调用的(实际 DB 调用跳过,仅类型断言)
    let _ = build_context_with_issue_body;
}
