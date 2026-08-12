//! Runtime service authorization for project and execution workspaces.
//!
//! Pure-function helpers mirroring Node server/src/routes/workspace-runtime-service-authz.ts.
//! Callers inject a RuntimeServiceContext with pre-fetched DB rows.

use pc_auth::{Actor, AuthContext};
use crate::trust::{
    resolve_core_trust_preset, DenyReason,
    LowTrustBoundary, ResolveInput as TrustResolveInput, TrustError, TrustPreset,
    TrustPresetResolution, TrustPresetSource,
};
use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

pub const WORKSPACE_RUNTIME_ELIGIBLE_ISSUE_STATUSES: &[&str] = &[
    "backlog",
    "todo",
    "in_progress",
    "in_review",
    "blocked",
];

#[derive(Debug, Error, Serialize)]
pub enum RuntimeServiceAuthzError {
    #[error("agent authentication required")]
    AgentRequired,
    #[error("agent key cannot access another company")]
    CrossCompany,
    #[error("missing permission to manage workspace runtime services")]
    MissingPermission,
    #[error("low-trust runtime service access denied: {0}")]
    LowTrustDenied(String),
    #[error("workspace not found")]
    WorkspaceNotFound,
    #[error("company access denied")]
    CompanyAccessDenied,
}

impl RuntimeServiceAuthzError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::AgentRequired => "agent_required",
            Self::CrossCompany => "cross_company",
            Self::MissingPermission => "missing_permission",
            Self::LowTrustDenied(_) => "low_trust_denied",
            Self::WorkspaceNotFound => "workspace_not_found",
            Self::CompanyAccessDenied => "company_access_denied",
        }
    }
}

#[derive(Debug, Clone)]
pub enum RuntimeServiceActor {
    BoardUser {
        is_instance_admin: bool,
        company_ids: Vec<Uuid>,
    },
    Agent {
        agent_id: Uuid,
        company_id: Uuid,
        run_id: Option<Uuid>,
    },
    User {
        user_id: String,
        company_ids: Vec<Uuid>,
    },
}

impl RuntimeServiceActor {
    pub fn from_auth(ctx: &AuthContext) -> Self {
        match &ctx.actor {
            Actor::System => Self::BoardUser {
                is_instance_admin: true,
                company_ids: Vec::new(),
            },
            Actor::User {
                id,
                company_ids,
                is_instance_admin,
                ..
            } => {
                if *is_instance_admin {
                    Self::BoardUser {
                        is_instance_admin: true,
                        company_ids: company_ids.clone(),
                    }
                } else {
                    Self::User {
                        user_id: id.clone(),
                        company_ids: company_ids.clone(),
                    }
                }
            }
            Actor::Agent {
                id,
                company_id,
                run_id,
                ..
            } => Self::Agent {
                agent_id: *id,
                company_id: *company_id,
                run_id: *run_id,
            },
            Actor::Anonymous => Self::User {
                user_id: String::new(),
                company_ids: Vec::new(),
            },
        }
    }

    pub fn is_instance_admin(&self) -> bool {
        matches!(self, Self::BoardUser { is_instance_admin: true, .. })
    }
}

#[derive(Debug, Clone, Default)]
pub struct AgentContextRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub role: String,
    pub permissions: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Default)]
pub struct RunContextRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub agent_id: Uuid,
    pub context_snapshot: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Default)]
pub struct IssueContextRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub project_id: Option<Uuid>,
    pub project_workspace_id: Option<Uuid>,
    pub execution_workspace_id: Option<Uuid>,
    pub assignee_agent_id: Option<Uuid>,
    pub status: String,
    pub hidden_at: bool,
    pub execution_policy: Option<serde_json::Value>,
    pub project_execution_workspace_policy: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Default)]
pub struct ProjectContextRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub execution_workspace_policy: Option<serde_json::Value>,
}

#[derive(Debug, Clone)]
pub struct RuntimeServiceContext {
    pub actor: RuntimeServiceActor,
    pub company_id: Uuid,
    pub project_workspace_id: Option<Uuid>,
    pub execution_workspace_id: Option<Uuid>,
    pub source_issue_id: Option<Uuid>,
    pub agent: AgentContextRow,
    pub run: Option<RunContextRow>,
    pub project: Option<ProjectContextRow>,
    pub run_issue: Option<IssueContextRow>,
    pub linked_scope_issues: Vec<IssueContextRow>,
    pub linked_assignee_issue: Option<IssueContextRow>,
    pub reporting_subtree_agent_ids: Vec<Uuid>,
}

pub fn run_execution_policy(run: Option<&RunContextRow>) -> Option<serde_json::Value> {
    run.and_then(|r| r.context_snapshot.as_ref())
        .and_then(|snap| snap.get("executionPolicy"))
        .cloned()
}

pub fn read_run_issue_id(context: Option<&serde_json::Value>) -> Option<String> {
    let direct = context
        .and_then(|c| c.get("issueId"))
        .and_then(|v| v.as_str());
    if let Some(id) = direct {
        if Uuid::parse_str(id).is_ok() {
            return Some(id.to_string());
        }
    }
    let nested_id = context
        .and_then(|c| c.get("paperclipIssue"))
        .and_then(|p| p.get("id"))
        .and_then(|v| v.as_str());
    if let Some(id) = nested_id {
        if Uuid::parse_str(id).is_ok() {
            return Some(id.to_string());
        }
    }
    None
}

fn is_issue_eligible(issue: &IssueContextRow) -> bool {
    if issue.hidden_at {
        return false;
    }
    WORKSPACE_RUNTIME_ELIGIBLE_ISSUE_STATUSES.contains(&issue.status.as_str())
}

fn boundary_allows_runtime_manage(boundary: &LowTrustBoundary) -> bool {
    !boundary.allowed_tool_classes.is_empty()
        || !boundary.allowed_agent_ids.is_empty()
        || boundary.root_issue_id.is_some()
        || !boundary.issue_ids.is_empty()
}

fn assert_company_access(
    actor: &RuntimeServiceActor,
    company_id: Uuid,
) -> Result<(), RuntimeServiceAuthzError> {
    match actor {
        RuntimeServiceActor::BoardUser { is_instance_admin, .. } if *is_instance_admin => Ok(()),
        RuntimeServiceActor::BoardUser { company_ids, .. } => {
            if company_ids.contains(&company_id) || company_ids.is_empty() {
                Ok(())
            } else {
                Err(RuntimeServiceAuthzError::CompanyAccessDenied)
            }
        }
        RuntimeServiceActor::User { company_ids, .. } => {
            if company_ids.contains(&company_id) {
                Ok(())
            } else {
                Err(RuntimeServiceAuthzError::CompanyAccessDenied)
            }
        }
        RuntimeServiceActor::Agent { company_id: agent_cid, .. } => {
            if *agent_cid == company_id {
                Ok(())
            } else {
                Err(RuntimeServiceAuthzError::CrossCompany)
            }
        }
    }
}

fn resolve_actor_trust(
    company_id: Uuid,
    agent: &AgentContextRow,
    run_exec_policy: Option<&serde_json::Value>,
) -> Result<TrustPresetResolution, RuntimeServiceAuthzError> {
    let input = TrustResolveInput {
        company_id,
        agent_permissions: agent.permissions.as_ref(),
        project_workspace_policy: None,
        issue_execution_policy: None,
        run_execution_policy: run_exec_policy,
    };
    let resolution = resolve_core_trust_preset(&input);
    match &resolution {
        TrustPresetResolution::Denied { detail, .. } => {
            Err(RuntimeServiceAuthzError::LowTrustDenied(detail.clone()))
        }
        TrustPresetResolution::LowTrustReview { boundary, .. } => {
            if boundary_allows_runtime_manage(boundary) {
                Ok(resolution)
            } else {
                Err(RuntimeServiceAuthzError::LowTrustDenied(
                    "low-trust runs cannot manage workspace runtime services unless the boundary grants runtime.manage".into(),
                ))
            }
        }
        TrustPresetResolution::Standard { .. } => Ok(resolution),
    }
}

fn resolve_issue_trust(
    actor_agent: &AgentContextRow,
    issue: &IssueContextRow,
    run_exec_policy: Option<&serde_json::Value>,
) -> Result<(), RuntimeServiceAuthzError> {
    let project_policy = issue
        .project_execution_workspace_policy
        .as_ref()
        .or(issue.execution_policy.as_ref());
    let input = TrustResolveInput {
        company_id: issue.company_id,
        agent_permissions: actor_agent.permissions.as_ref(),
        project_workspace_policy: project_policy,
        issue_execution_policy: issue.execution_policy.as_ref(),
        run_execution_policy: run_exec_policy,
    };
    let resolution = resolve_core_trust_preset(&input);
    match resolution {
        TrustPresetResolution::Denied { detail, .. } => {
            Err(RuntimeServiceAuthzError::LowTrustDenied(detail))
        }
        TrustPresetResolution::LowTrustReview { boundary, .. } => {
            if boundary_allows_runtime_manage(&boundary) {
                Ok(())
            } else {
                Err(RuntimeServiceAuthzError::LowTrustDenied(
                    "low-trust runs cannot manage workspace runtime services unless the boundary grants runtime.manage".into(),
                ))
            }
        }
        TrustPresetResolution::Standard { .. } => Ok(()),
    }
}

fn assert_agent_can_manage_runtime(
    ctx: &RuntimeServiceContext,
) -> Result<(), RuntimeServiceAuthzError> {
    let actor_resolution = resolve_actor_trust(
        ctx.company_id,
        &ctx.agent,
        run_execution_policy(ctx.run.as_ref()).as_ref(),
    )?;

    let actor_is_ceo = ctx.agent.role == "ceo";

    if actor_is_ceo && matches!(actor_resolution, TrustPresetResolution::Standard { .. }) {
        return Ok(());
    }

    if let Some(run_issue) = ctx.run_issue.as_ref() {
        resolve_issue_trust(
            &ctx.agent,
            run_issue,
            run_execution_policy(ctx.run.as_ref()).as_ref(),
        )?;
    }

    for linked in &ctx.linked_scope_issues {
        if !is_issue_eligible(linked) {
            continue;
        }
        if linked.company_id != ctx.company_id {
            continue;
        }
        resolve_issue_trust(
            &ctx.agent,
            linked,
            run_execution_policy(ctx.run.as_ref()).as_ref(),
        )?;
        if actor_is_ceo {
            return Ok(());
        }
        return Ok(());
    }

    if actor_is_ceo {
        return Ok(());
    }

    if let Some(assignee_issue) = ctx.linked_assignee_issue.as_ref() {
        if is_issue_eligible(assignee_issue)
            && assignee_issue.company_id == ctx.company_id
            && assignee_issue
                .assignee_agent_id
                .map(|a| ctx.reporting_subtree_agent_ids.contains(&a))
                .unwrap_or(false)
        {
            resolve_issue_trust(
                &ctx.agent,
                assignee_issue,
                run_execution_policy(ctx.run.as_ref()).as_ref(),
            )?;
            return Ok(());
        }
    }

    Err(RuntimeServiceAuthzError::MissingPermission)
}


pub fn assert_can_manage_project_workspace_runtime_services(
    ctx: &RuntimeServiceContext,
) -> Result<(), RuntimeServiceAuthzError> {
    assert_company_access(&ctx.actor, ctx.company_id)?;
    if !ctx.actor.is_instance_admin()
        && !matches!(
            ctx.actor,
            RuntimeServiceActor::BoardUser { .. } | RuntimeServiceActor::User { .. }
        )
    {
        return assert_agent_can_manage_runtime(ctx);
    }
    match &ctx.actor {
        RuntimeServiceActor::BoardUser { .. } | RuntimeServiceActor::User { .. } => Ok(()),
        RuntimeServiceActor::Agent { .. } => assert_agent_can_manage_runtime(ctx),
    }
}

pub fn assert_can_manage_execution_workspace_runtime_services(
    ctx: &RuntimeServiceContext,
) -> Result<(), RuntimeServiceAuthzError> {
    assert_company_access(&ctx.actor, ctx.company_id)?;
    match &ctx.actor {
        RuntimeServiceActor::BoardUser { .. } | RuntimeServiceActor::User { .. } => Ok(()),
        RuntimeServiceActor::Agent { .. } => assert_agent_can_manage_runtime(ctx),
    }
}

impl From<TrustError> for RuntimeServiceAuthzError {
    fn from(err: TrustError) -> Self {
        RuntimeServiceAuthzError::LowTrustDenied(err.to_string())
    }
}

#[allow(dead_code)]
fn _trust_re_exports_used(_: TrustPreset) {}
#[allow(dead_code)]
fn _deny_reason_re_exports_used(_: DenyReason) {}
#[allow(dead_code)]
fn _boundary_re_exports_used(_: &LowTrustBoundary) {}
#[allow(dead_code)]
fn _resolution_re_exports_used(_: &TrustPresetResolution) {}
#[allow(dead_code)]
fn _source_re_exports_used(_: TrustPresetSource) {}




impl Default for RuntimeServiceContext {
    fn default() -> Self {
        Self {
            actor: RuntimeServiceActor::BoardUser {
                is_instance_admin: true,
                company_ids: Vec::new(),
            },
            company_id: Uuid::nil(),
            project_workspace_id: None,
            execution_workspace_id: None,
            source_issue_id: None,
            agent: AgentContextRow::default(),
            run: None,
            project: None,
            run_issue: None,
            linked_scope_issues: Vec::new(),
            linked_assignee_issue: None,
            reporting_subtree_agent_ids: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn agent_row(role: &str) -> AgentContextRow {
        AgentContextRow {
            id: Uuid::new_v4(),
            company_id: Uuid::nil(),
            role: role.into(),
            permissions: None,
        }
    }

    fn eligible_issue_with(assignee: Option<Uuid>) -> IssueContextRow {
        IssueContextRow {
            id: Uuid::new_v4(),
            company_id: Uuid::nil(),
            project_id: None,
            project_workspace_id: Some(Uuid::new_v4()),
            execution_workspace_id: Some(Uuid::new_v4()),
            assignee_agent_id: assignee,
            status: "todo".into(),
            hidden_at: false,
            execution_policy: None,
            project_execution_workspace_policy: None,
        }
    }

    #[test]
    fn board_user_is_always_allowed() {
        let ctx = RuntimeServiceContext {
            actor: RuntimeServiceActor::BoardUser {
                is_instance_admin: true,
                company_ids: vec![],
            },
            company_id: Uuid::nil(),
            ..Default::default()
        };
        assert!(assert_can_manage_execution_workspace_runtime_services(&ctx).is_ok());
    }

    #[test]
    fn instance_admin_actor_with_no_company_ids_is_allowed() {
        let ctx = RuntimeServiceContext {
            actor: RuntimeServiceActor::BoardUser {
                is_instance_admin: true,
                company_ids: vec![],
            },
            company_id: Uuid::new_v4(),
            ..Default::default()
        };
        assert!(assert_can_manage_project_workspace_runtime_services(&ctx).is_ok());
    }

    #[test]
    fn ceo_agent_with_linked_scope_issue_is_allowed() {
        let a = agent_row("ceo");
        let linked = eligible_issue_with(Some(a.id));
        let ctx = RuntimeServiceContext {
            actor: RuntimeServiceActor::Agent {
                agent_id: a.id,
                company_id: Uuid::nil(),
                run_id: None,
            },
            agent: a.clone(),
            company_id: Uuid::nil(),
            linked_scope_issues: vec![linked],
            ..Default::default()
        };
        assert!(assert_can_manage_execution_workspace_runtime_services(&ctx).is_ok());
    }

    #[test]
    fn engineer_without_assignment_is_denied() {
        let a = agent_row("engineer");
        let ctx = RuntimeServiceContext {
            actor: RuntimeServiceActor::Agent {
                agent_id: a.id,
                company_id: Uuid::nil(),
                run_id: None,
            },
            agent: a,
            company_id: Uuid::nil(),
            ..Default::default()
        };
        let err = assert_can_manage_execution_workspace_runtime_services(&ctx).unwrap_err();
        assert_eq!(err.code(), "missing_permission");
    }

    #[test]
    fn engineer_with_active_assignment_is_allowed() {
        let a = agent_row("engineer");
        let ctx = RuntimeServiceContext {
            actor: RuntimeServiceActor::Agent {
                agent_id: a.id,
                company_id: Uuid::nil(),
                run_id: None,
            },
            agent: a.clone(),
            company_id: Uuid::nil(),
            reporting_subtree_agent_ids: vec![a.id],
            linked_assignee_issue: Some(eligible_issue_with(Some(a.id))),
            ..Default::default()
        };
        assert!(assert_can_manage_execution_workspace_runtime_services(&ctx).is_ok());
    }

    #[test]
    fn completed_issue_does_not_count_as_linked_scope() {
        let a = agent_row("engineer");
        let mut linked = eligible_issue_with(Some(a.id));
        linked.status = "done".into();
        let ctx = RuntimeServiceContext {
            actor: RuntimeServiceActor::Agent {
                agent_id: a.id,
                company_id: Uuid::nil(),
                run_id: None,
            },
            agent: a,
            company_id: Uuid::nil(),
            linked_scope_issues: vec![linked],
            ..Default::default()
        };
        let err = assert_can_manage_execution_workspace_runtime_services(&ctx).unwrap_err();
        assert_eq!(err.code(), "missing_permission");
    }

    #[test]
    fn cross_company_user_is_denied() {
        let a = agent_row("ceo");
        let ctx = RuntimeServiceContext {
            actor: RuntimeServiceActor::User {
                user_id: "u1".into(),
                company_ids: vec![Uuid::nil()],
            },
            agent: a,
            company_id: Uuid::new_v4(),
            ..Default::default()
        };
        let err = assert_can_manage_execution_workspace_runtime_services(&ctx).unwrap_err();
        assert_eq!(err.code(), "company_access_denied");
    }

    #[test]
    fn cross_company_agent_is_denied() {
        let a = agent_row("engineer");
        let ctx = RuntimeServiceContext {
            actor: RuntimeServiceActor::Agent {
                agent_id: a.id,
                company_id: Uuid::new_v4(),
                run_id: None,
            },
            agent: a,
            company_id: Uuid::nil(),
            ..Default::default()
        };
        let err = assert_can_manage_execution_workspace_runtime_services(&ctx).unwrap_err();
        assert_eq!(err.code(), "cross_company");
    }

    #[test]
    fn low_trust_ceo_without_boundary_is_denied() {
        let a = agent_row("ceo");
        let run = RunContextRow {
            id: Uuid::new_v4(),
            company_id: Uuid::nil(),
            agent_id: a.id,
            context_snapshot: Some(json!({
                "executionPolicy": {
                    "trustPreset": "low_trust_review"
                }
            })),
        };
        let ctx = RuntimeServiceContext {
            actor: RuntimeServiceActor::Agent {
                agent_id: a.id,
                company_id: Uuid::nil(),
                run_id: Some(run.id),
            },
            agent: a,
            run: Some(run),
            company_id: Uuid::nil(),
            ..Default::default()
        };
        let err = assert_can_manage_execution_workspace_runtime_services(&ctx).unwrap_err();
        assert_eq!(err.code(), "low_trust_denied");
    }

    #[test]
    fn low_trust_ceo_with_full_boundary_is_allowed() {
        let a = agent_row("ceo");
        let run = RunContextRow {
            id: Uuid::new_v4(),
            company_id: Uuid::nil(),
            agent_id: a.id,
            context_snapshot: Some(json!({
                "executionPolicy": {
                    "trustPreset": "low_trust_review",
                    "trustBoundary": {
                        "companyId": Uuid::nil().to_string(),
                        "allowedToolClasses": ["git.read"],
                        "issueIds": [Uuid::new_v4().to_string()]
                    }
                }
            })),
        };
        let ctx = RuntimeServiceContext {
            actor: RuntimeServiceActor::Agent {
                agent_id: a.id,
                company_id: Uuid::nil(),
                run_id: Some(run.id),
            },
            agent: a,
            run: Some(run),
            company_id: Uuid::nil(),
            linked_scope_issues: vec![eligible_issue_with(Some(Uuid::new_v4()))],
            ..Default::default()
        };
        assert!(assert_can_manage_execution_workspace_runtime_services(&ctx).is_ok());
    }

    #[test]
    fn read_run_issue_id_handles_both_paths() {
        let direct = json!({"issueId": Uuid::new_v4().to_string()});
        assert!(read_run_issue_id(Some(&direct)).is_some());
        let nested_id = Uuid::new_v4().to_string();
        let nested = json!({"paperclipIssue": {"id": nested_id.clone()}});
        assert_eq!(read_run_issue_id(Some(&nested)), Some(nested_id));
        assert_eq!(read_run_issue_id(None), None);
        let bad = json!({"issueId": "not-a-uuid"});
        assert_eq!(read_run_issue_id(Some(&bad)), None);
    }

    #[test]
    fn run_execution_policy_extracts_policy_field() {
        let r = RunContextRow {
            context_snapshot: Some(json!({"executionPolicy": {"trustPreset": "low_trust_review"}})),
            ..Default::default()
        };
        let p = run_execution_policy(Some(&r));
        assert!(p.is_some());
        assert_eq!(run_execution_policy(None), None);
    }
}
