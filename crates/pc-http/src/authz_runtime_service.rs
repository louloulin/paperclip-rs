//! Compose pc_http::authz_loaders into a single helper that hydrates a
//! RuntimeServiceContext and runs the pure-function auth check.
//!
//! Mirrors Node server/src/routes/workspace-runtime-service-authz.ts:
//! 1. resolve companyId from the workspace (project or execution)
//! 2. load the actor agent row (if actor is an Agent)
//! 3. load the actor run row + contextSnapshot
//! 4. load linked scope issues for the workspace
//! 5. load the linked assignee issue (within reporting subtree)
//! 6. delegate to pc_authz::runtime_service::assert_*

use uuid::Uuid;

use pc_auth::{Actor, AuthContext};
use pc_authz::{
    assert_can_manage_execution_workspace_runtime_services,
    assert_can_manage_project_workspace_runtime_services, RuntimeServiceActor,
    RuntimeServiceAuthzError, RuntimeServiceContext,
};
use pc_db::Db;

use crate::authz_loaders::RuntimeServiceAuthzLoader;

#[derive(Debug, Clone)]
pub enum WorkspaceKind {
    Project {
        workspace_id: Uuid,
    },
    Execution {
        workspace_id: Uuid,
        source_issue_id: Option<Uuid>,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum LoadRuntimeServiceError {
    #[error(transparent)]
    Sql(#[from] sqlx::Error),
    #[error("workspace not found")]
    WorkspaceNotFound,
    #[error("agent not found")]
    AgentNotFound,
}

impl From<LoadRuntimeServiceError> for RuntimeServiceAuthzError {
    fn from(err: LoadRuntimeServiceError) -> Self {
        match err {
            LoadRuntimeServiceError::Sql(_) => RuntimeServiceAuthzError::MissingPermission,
            LoadRuntimeServiceError::WorkspaceNotFound => {
                RuntimeServiceAuthzError::WorkspaceNotFound
            }
            LoadRuntimeServiceError::AgentNotFound => RuntimeServiceAuthzError::MissingPermission,
        }
    }
}

pub async fn load_and_assert_runtime_service_manage(
    db: &Db,
    auth: &AuthContext,
    company_id: Uuid,
    workspace: WorkspaceKind,
) -> Result<RuntimeServiceContext, RuntimeServiceAuthzError> {
    let actor = RuntimeServiceActor::from_auth(auth);
    let mut ctx = RuntimeServiceContext {
        actor,
        company_id,
        ..Default::default()
    };
    match &workspace {
        WorkspaceKind::Project { workspace_id } => {
            ctx.project_workspace_id = Some(*workspace_id);
        }
        WorkspaceKind::Execution {
            workspace_id,
            source_issue_id,
        } => {
            ctx.execution_workspace_id = Some(*workspace_id);
            ctx.source_issue_id = *source_issue_id;
        }
    }

    let loader = RuntimeServiceAuthzLoader::new(db);

    if let Actor::Agent {
        id,
        company_id: actor_company,
        run_id,
        ..
    } = &auth.actor
    {
        if actor_company != &company_id {
            return Err(RuntimeServiceAuthzError::CrossCompany);
        }
        let agent = loader
            .load_actor_agent(*id, company_id)
            .await
            .map_err(LoadRuntimeServiceError::from)?
            .ok_or(LoadRuntimeServiceError::AgentNotFound)?;
        ctx.agent = agent;
        if let Some(run_id) = run_id {
            if let Some(run) = loader
                .load_actor_run(*run_id, company_id, *id)
                .await
                .map_err(LoadRuntimeServiceError::from)?
            {
                ctx.run = Some(run);
            }
        }
        let run_issue_id =
            pc_authz::read_run_issue_id(ctx.run.as_ref().and_then(|r| r.context_snapshot.as_ref()))
                .and_then(|s| Uuid::parse_str(&s).ok());
        if let Some(rid) = run_issue_id {
            if let Some(run_issue) = loader
                .load_run_issue(rid, company_id)
                .await
                .map_err(LoadRuntimeServiceError::from)?
            {
                ctx.run_issue = Some(run_issue);
            }
        }
        ctx.reporting_subtree_agent_ids = loader
            .list_reporting_subtree_agent_ids(company_id, *id)
            .await
            .map_err(LoadRuntimeServiceError::from)?;
    }

    let (project_workspace_id, execution_workspace_id) = match workspace {
        WorkspaceKind::Project { workspace_id } => (Some(workspace_id), None),
        WorkspaceKind::Execution { workspace_id, .. } => (None, Some(workspace_id)),
    };
    ctx.linked_scope_issues = if let Some(pwid) = project_workspace_id {
        loader
            .list_linked_scope_issues_for_project_workspace(pwid, company_id)
            .await
            .map_err(LoadRuntimeServiceError::from)?
    } else if let Some(ewid) = execution_workspace_id {
        loader
            .list_linked_scope_issues_for_execution_workspace(ewid, company_id)
            .await
            .map_err(LoadRuntimeServiceError::from)?
    } else {
        Vec::new()
    };
    if !ctx.reporting_subtree_agent_ids.is_empty() {
        ctx.linked_assignee_issue = loader
            .load_linked_assignee_issue_in_workspace(
                project_workspace_id,
                execution_workspace_id,
                &ctx.reporting_subtree_agent_ids,
                company_id,
            )
            .await
            .map_err(LoadRuntimeServiceError::from)?;
    }
    Ok(ctx)
}

pub async fn assert_project_workspace_runtime_manage(
    db: &Db,
    auth: &AuthContext,
    company_id: Uuid,
    project_workspace_id: Uuid,
) -> Result<(), RuntimeServiceAuthzError> {
    let ctx = load_and_assert_runtime_service_manage(
        db,
        auth,
        company_id,
        WorkspaceKind::Project {
            workspace_id: project_workspace_id,
        },
    )
    .await?;
    assert_can_manage_project_workspace_runtime_services(&ctx)
}

pub async fn assert_execution_workspace_runtime_manage(
    db: &Db,
    auth: &AuthContext,
    company_id: Uuid,
    execution_workspace_id: Uuid,
    source_issue_id: Option<Uuid>,
) -> Result<(), RuntimeServiceAuthzError> {
    let ctx = load_and_assert_runtime_service_manage(
        db,
        auth,
        company_id,
        WorkspaceKind::Execution {
            workspace_id: execution_workspace_id,
            source_issue_id,
        },
    )
    .await?;
    assert_can_manage_execution_workspace_runtime_services(&ctx)
}

pub fn map_authz_error_to_api(err: RuntimeServiceAuthzError) -> crate::ApiError {
    use crate::ApiError;
    match err {
        RuntimeServiceAuthzError::AgentRequired => {
            ApiError::Forbidden("agent authentication required".into())
        }
        RuntimeServiceAuthzError::CrossCompany => {
            ApiError::Forbidden("agent key cannot access another company".into())
        }
        RuntimeServiceAuthzError::MissingPermission => {
            ApiError::Forbidden("missing permission to manage workspace runtime services".into())
        }
        RuntimeServiceAuthzError::LowTrustDenied(detail) => {
            ApiError::Forbidden(format!("low-trust runtime service access denied: {detail}"))
        }
        RuntimeServiceAuthzError::WorkspaceNotFound => {
            ApiError::NotFound("workspace not found".into())
        }
        RuntimeServiceAuthzError::CompanyAccessDenied => {
            ApiError::Forbidden("company access denied".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pc_auth::AuthContext;
    use pc_authz::RuntimeServiceAuthzError;

    #[test]
    fn map_authz_error_to_api_returns_expected_403() {
        let err = map_authz_error_to_api(RuntimeServiceAuthzError::MissingPermission);
        match err {
            crate::ApiError::Forbidden(msg) => {
                assert!(msg.contains("missing permission"));
            }
            other => panic!("expected Forbidden, got {:?}", other),
        }
    }

    #[test]
    fn map_authz_error_to_api_returns_404_for_workspace_not_found() {
        let err = map_authz_error_to_api(RuntimeServiceAuthzError::WorkspaceNotFound);
        match err {
            crate::ApiError::NotFound(msg) => {
                assert!(msg.contains("workspace"));
            }
            other => panic!("expected NotFound, got {:?}", other),
        }
    }

    #[test]
    fn map_authz_error_low_trust_keeps_detail() {
        let err = map_authz_error_to_api(RuntimeServiceAuthzError::LowTrustDenied(
            "missing boundary".into(),
        ));
        match err {
            crate::ApiError::Forbidden(msg) => {
                assert!(msg.contains("missing boundary"));
            }
            other => panic!("expected Forbidden, got {:?}", other),
        }
    }

    #[test]
    fn workspace_kind_holds_workspace_id() {
        let kind = WorkspaceKind::Project {
            workspace_id: Uuid::nil(),
        };
        match kind {
            WorkspaceKind::Project { workspace_id } => {
                assert_eq!(workspace_id, Uuid::nil());
            }
            _ => panic!("expected Project variant"),
        }
    }

    #[test]
    fn execution_workspace_kind_carries_optional_source_issue() {
        let kind = WorkspaceKind::Execution {
            workspace_id: Uuid::nil(),
            source_issue_id: Some(Uuid::new_v4()),
        };
        match kind {
            WorkspaceKind::Execution {
                workspace_id,
                source_issue_id,
            } => {
                assert_eq!(workspace_id, Uuid::nil());
                assert!(source_issue_id.is_some());
            }
            _ => panic!("expected Execution variant"),
        }
    }

    #[test]
    fn load_runtime_service_error_maps_to_api_error() {
        let lerr = LoadRuntimeServiceError::WorkspaceNotFound;
        let authz_err: RuntimeServiceAuthzError = lerr.into();
        assert_eq!(authz_err.code(), "workspace_not_found");
    }
}
