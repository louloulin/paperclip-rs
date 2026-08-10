#![forbid(unsafe_code)]
//! Project domain service layer.
//!
//! See `lib.rs` for module-level docs.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;


use uuid::Uuid;

pub use pc_repos::project::{
    MembershipState, NewProject, ProjectMembershipRow, ProjectPatch, ProjectRow,
    ProjectStatus, ProjectWorkspaceRow,
};
use pc_repos::project::ProjectRepo;
use pc_repos::Db;

use pc_errors::{internal, validation, Error as PcError, Result};

// =============================================================================
// R613: lifecycle events surfaced to hooks
// =============================================================================

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ProjectHookEvent {
    Created {
        company_id: Uuid,
        project_id: Uuid,
        name: String,
    },
    Patched {
        company_id: Uuid,
        project_id: Uuid,
    },
    StatusChanged {
        company_id: Uuid,
        project_id: Uuid,
        old_status: Option<ProjectStatus>,
        new_status: ProjectStatus,
    },
    Paused {
        company_id: Uuid,
        project_id: Uuid,
        reason: Option<String>,
    },
    Resumed {
        company_id: Uuid,
        project_id: Uuid,
    },
    Archived {
        company_id: Uuid,
        project_id: Uuid,
    },
    Deleted {
        company_id: Uuid,
        project_id: Uuid,
    },
    MembershipUpserted {
        company_id: Uuid,
        project_id: Uuid,
        user_id: String,
        state: MembershipState,
    },
    WorkspaceCreated {
        company_id: Uuid,
        project_id: Uuid,
        workspace_id: Uuid,
    },
    WorkspaceSetPrimary {
        company_id: Uuid,
        project_id: Uuid,
        workspace_id: Uuid,
    },
    GoalAttached {
        company_id: Uuid,
        project_id: Uuid,
        goal_id: Uuid,
    },
    GoalDetached {
        company_id: Uuid,
        project_id: Uuid,
        goal_id: Uuid,
    },
}

// =============================================================================
// R613: hook trait
// =============================================================================

#[async_trait]
pub trait ProjectHook: Send + Sync {
    async fn on_project_event(&self, _event: ProjectHookEvent) -> Result<()> {
        Ok(())
    }
}

pub struct NoopProjectHook;
#[async_trait]
impl ProjectHook for NoopProjectHook {}

#[derive(Default)]
pub struct RecordingProjectHook {
    pub events: std::sync::Mutex<Vec<ProjectHookEvent>>,
}

#[async_trait]
impl ProjectHook for RecordingProjectHook {
    async fn on_project_event(&self, event: ProjectHookEvent) -> Result<()> {
        self.events.lock().expect("lock").push(event);
        Ok(())
    }
}

impl RecordingProjectHook {
    #[must_use]
    pub fn events_snapshot(&self) -> Vec<ProjectHookEvent> {
        self.events.lock().expect("lock").clone()
    }

    pub fn clear(&self) {
        self.events.lock().expect("lock").clear();
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.events.lock().expect("lock").len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.lock().expect("lock").is_empty()
    }
}

// =============================================================================
// R613: error type
// =============================================================================

#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("validation: {0}")]
    Validation(String),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Pc(#[from] PcError),
}

impl From<pc_repos::RepoError> for ProjectError {
    fn from(e: pc_repos::RepoError) -> Self {
        Self::Pc(internal(e.to_string()))
    }
}

pub type ProjectResult<T> = std::result::Result<T, ProjectError>;

// =============================================================================
// R613: input validation helpers
// =============================================================================

fn normalize_new(input: &NewProject) -> Result<()> {
    if input.company_id.is_nil() {
        return Err(validation("companyId is required"));
    }
    if input.name.trim().is_empty() {
        return Err(validation("project name must not be empty"));
    }
    Ok(())
}

fn normalize_patch(patch: &ProjectPatch) -> Result<()> {
    if let Some(name) = &patch.name {
        if name.trim().is_empty() {
            return Err(validation("project name must not be empty"));
        }
    }
    Ok(())
}

// =============================================================================
// R613: ProjectService
// =============================================================================

#[derive(Clone)]
pub struct ProjectService {
    db: Db,
    hooks: Vec<Arc<dyn ProjectHook>>,
}

impl ProjectService {
    pub fn new(db: Db) -> Self {
        Self { db, hooks: Vec::new() }
    }

    pub fn with_hooks(db: Db, hooks: Vec<Arc<dyn ProjectHook>>) -> Self {
        Self { db, hooks }
    }

    pub fn add_hook(mut self, h: Arc<dyn ProjectHook>) -> Self {
        self.hooks.push(h);
        self
    }

    pub fn hook_count(&self) -> usize {
        self.hooks.len()
    }

    async fn dispatch(&self, event: ProjectHookEvent) {
        for h in &self.hooks {
            if let Err(e) = h.on_project_event(event.clone()).await {
                tracing::warn!(?e, "project hook failed");
            }
        }
    }

    fn repo(&self) -> ProjectRepo<'_> {
        ProjectRepo::new(&self.db)
    }

    // -------------------------------------------------------------------------
    // Read paths
    // -------------------------------------------------------------------------

    pub async fn list_by_company(
        &self,
        company_id: Uuid,
        only_active: bool,
    ) -> ProjectResult<Vec<ProjectRow>> {
        if company_id.is_nil() {
            return Err(ProjectError::Validation("companyId is required".into()));
        }
        Ok(self.repo().list_by_company(company_id, only_active).await?)
    }

    pub async fn list_by_company_no_filter(
        &self,
        company_id: Uuid,
    ) -> ProjectResult<Vec<ProjectRow>> {
        if company_id.is_nil() {
            return Err(ProjectError::Validation("companyId is required".into()));
        }
        Ok(self.repo().list_by_company_no_filter(company_id).await?)
    }

    pub async fn list_all(&self, limit: i64) -> ProjectResult<Vec<ProjectRow>> {
        Ok(self.repo().list_all(limit).await?)
    }

    pub async fn get(
        &self,
        company_id: Uuid,
        id: Uuid,
    ) -> ProjectResult<Option<ProjectRow>> {
        if company_id.is_nil() {
            return Err(ProjectError::Validation("companyId is required".into()));
        }
        Ok(self.repo().get(company_id, id).await?)
    }

    pub async fn get_id_only(&self, id: Uuid) -> ProjectResult<Option<ProjectRow>> {
        Ok(self.repo().get_id_only(id).await?)
    }

    // -------------------------------------------------------------------------
    // Write paths (validation + hooks)
    // -------------------------------------------------------------------------

    pub async fn create(&self, input: NewProject) -> ProjectResult<ProjectRow> {
        normalize_new(&input)?;
        let row = self.repo().create(&input).await?;
        self.dispatch(ProjectHookEvent::Created {
            company_id: input.company_id,
            project_id: row.id,
            name: row.name.clone(),
        })
        .await;
        Ok(row)
    }

    pub async fn patch(
        &self,
        company_id: Uuid,
        id: Uuid,
        patch: ProjectPatch,
    ) -> ProjectResult<Option<ProjectRow>> {
        if company_id.is_nil() {
            return Err(ProjectError::Validation("companyId is required".into()));
        }
        normalize_patch(&patch)?;

        // Capture old status before mutation so we can emit StatusChanged.
        let old_status = self
            .repo()
            .get(company_id, id)
            .await?
            .and_then(|r| ProjectStatus::parse(&r.status));

        let new_row = self.repo().patch(company_id, id, &patch).await?;
        if let Some(r) = &new_row {
            self.dispatch(ProjectHookEvent::Patched {
                company_id,
                project_id: r.id,
            })
            .await;
            let new_status = ProjectStatus::parse(&r.status).unwrap_or(ProjectStatus::Backlog);
            if Some(new_status) != old_status {
                self.dispatch(ProjectHookEvent::StatusChanged {
                    company_id,
                    project_id: r.id,
                    old_status,
                    new_status,
                })
                .await;
            }
        }
        Ok(new_row)
    }

    pub async fn pause(
        &self,
        company_id: Uuid,
        id: Uuid,
        reason: Option<&str>,
    ) -> ProjectResult<Option<ProjectRow>> {
        if company_id.is_nil() {
            return Err(ProjectError::Validation("companyId is required".into()));
        }
        let row = self.repo().pause(company_id, id, reason).await?;
        if let Some(r) = &row {
            self.dispatch(ProjectHookEvent::Paused {
                company_id,
                project_id: r.id,
                reason: reason.map(str::to_string),
            })
            .await;
        }
        Ok(row)
    }

    pub async fn resume(
        &self,
        company_id: Uuid,
        id: Uuid,
    ) -> ProjectResult<Option<ProjectRow>> {
        if company_id.is_nil() {
            return Err(ProjectError::Validation("companyId is required".into()));
        }
        let row = self.repo().resume(company_id, id).await?;
        if let Some(r) = &row {
            self.dispatch(ProjectHookEvent::Resumed {
                company_id,
                project_id: r.id,
            })
            .await;
        }
        Ok(row)
    }

    pub async fn archive(
        &self,
        company_id: Uuid,
        id: Uuid,
    ) -> ProjectResult<Option<ProjectRow>> {
        if company_id.is_nil() {
            return Err(ProjectError::Validation("companyId is required".into()));
        }
        let row = self.repo().archive(company_id, id).await?;
        if let Some(r) = &row {
            self.dispatch(ProjectHookEvent::Archived {
                company_id,
                project_id: r.id,
            })
            .await;
        }
        Ok(row)
    }

    pub async fn delete(&self, company_id: Uuid, id: Uuid) -> ProjectResult<bool> {
        if company_id.is_nil() {
            return Err(ProjectError::Validation("companyId is required".into()));
        }
        let deleted = self.repo().delete(company_id, id).await?;
        if deleted {
            self.dispatch(ProjectHookEvent::Deleted {
                company_id,
                project_id: id,
            })
            .await;
        }
        Ok(deleted)
    }

    // -------------------------------------------------------------------------
    // Memberships
    // -------------------------------------------------------------------------

    pub async fn list_memberships(
        &self,
        project_id: Uuid,
    ) -> ProjectResult<Vec<ProjectMembershipRow>> {
        Ok(self.repo().list_memberships(project_id).await?)
    }

    pub async fn list_user_memberships(
        &self,
        company_id: Uuid,
        user_id: &str,
    ) -> ProjectResult<Vec<ProjectMembershipRow>> {
        if company_id.is_nil() {
            return Err(ProjectError::Validation("companyId is required".into()));
        }
        Ok(self.repo().list_user_memberships(company_id, user_id).await?)
    }

    pub async fn upsert_membership(
        &self,
        company_id: Uuid,
        project_id: Uuid,
        user_id: &str,
        state: MembershipState,
    ) -> ProjectResult<ProjectMembershipRow> {
        if company_id.is_nil() {
            return Err(ProjectError::Validation("companyId is required".into()));
        }
        if project_id.is_nil() {
            return Err(ProjectError::Validation("projectId is required".into()));
        }
        if user_id.trim().is_empty() {
            return Err(ProjectError::Validation("userId must not be empty".into()));
        }
        let row = self.repo().upsert_membership(company_id, project_id, user_id, state).await?;
        self.dispatch(ProjectHookEvent::MembershipUpserted {
            company_id: row.company_id,
            project_id: row.project_id,
            user_id: user_id.to_string(),
            state,
        })
        .await;
        Ok(row)
    }

    // -------------------------------------------------------------------------
    // Workspaces
    // -------------------------------------------------------------------------

    pub async fn list_workspaces(
        &self,
        project_id: Uuid,
    ) -> ProjectResult<Vec<ProjectWorkspaceRow>> {
        Ok(self.repo().list_workspaces(project_id).await?)
    }

    pub async fn get_primary_workspace(
        &self,
        project_id: Uuid,
    ) -> ProjectResult<Option<ProjectWorkspaceRow>> {
        Ok(self.repo().get_primary_workspace(project_id).await?)
    }

    pub async fn create_workspace(
        &self,
        company_id: Uuid,
        project_id: Uuid,
        source_type: &str,
        source_url: Option<&str>,
        source_ref: Option<&str>,
        local_path: Option<&str>,
        is_primary: bool,
    ) -> ProjectResult<ProjectWorkspaceRow> {
        if company_id.is_nil() {
            return Err(ProjectError::Validation("companyId is required".into()));
        }
        if project_id.is_nil() {
            return Err(ProjectError::Validation("projectId is required".into()));
        }
        if source_type.trim().is_empty() {
            return Err(ProjectError::Validation("sourceType must not be empty".into()));
        }
        // Build a ProjectWorkspaceRow with sensible defaults and let the
        // repo do the INSERT.
        let row = ProjectWorkspaceRow {
            id: Uuid::new_v4(),
            company_id,
            project_id,
            name: source_ref.unwrap_or("workspace").to_string(),
            source_type: source_type.to_string(),
            cwd: local_path.map(str::to_string),
            repo_url: source_url.map(str::to_string),
            repo_ref: source_ref.map(str::to_string),
            default_ref: None,
            visibility: "private".into(),
            setup_command: None,
            cleanup_command: None,
            remote_provider: None,
            remote_workspace_ref: None,
            shared_workspace_key: None,
            metadata: None,
            is_primary,
            created_at: pc_core::Timestamp::now(),
            updated_at: pc_core::Timestamp::now(),
        };
        let row = self.repo().create_workspace(&row).await?;
        self.dispatch(ProjectHookEvent::WorkspaceCreated {
            company_id,
            project_id,
            workspace_id: row.id,
        })
        .await;
        Ok(row)
    }

    pub async fn set_primary_workspace(
        &self,
        company_id: Uuid,
        project_id: Uuid,
        workspace_id: Uuid,
    ) -> ProjectResult<bool> {
        if company_id.is_nil() {
            return Err(ProjectError::Validation("companyId is required".into()));
        }
        let ok = self.repo().set_primary_workspace(project_id, workspace_id).await?;
        if ok {
            self.dispatch(ProjectHookEvent::WorkspaceSetPrimary {
                company_id,
                project_id,
                workspace_id,
            })
            .await;
        }
        Ok(ok)
    }

    // -------------------------------------------------------------------------
    // Goal bindings
    // -------------------------------------------------------------------------

    pub async fn attach_goal(
        &self,
        company_id: Uuid,
        project_id: Uuid,
        goal_id: Uuid,
    ) -> ProjectResult<bool> {
        if company_id.is_nil() {
            return Err(ProjectError::Validation("companyId is required".into()));
        }
        self.repo().attach_goal(company_id, project_id, goal_id).await?;
        self.dispatch(ProjectHookEvent::GoalAttached {
            company_id,
            project_id,
            goal_id,
        })
        .await;
        Ok(true)
    }

    pub async fn detach_goal(
        &self,
        company_id: Uuid,
        project_id: Uuid,
        goal_id: Uuid,
    ) -> ProjectResult<bool> {
        if company_id.is_nil() {
            return Err(ProjectError::Validation("companyId is required".into()));
        }
        let ok = self.repo().detach_goal(project_id, goal_id).await?;
        if ok {
            self.dispatch(ProjectHookEvent::GoalDetached {
                company_id,
                project_id,
                goal_id,
            })
            .await;
        }
        Ok(ok)
    }

    pub async fn goals_for_project(
        &self,
        project_id: Uuid,
    ) -> ProjectResult<Vec<pc_repos::project::ProjectGoalRow>> {
        Ok(self.repo().goals_for_project(project_id).await?)
    }

    // -------------------------------------------------------------------------
    // Misc
    // -------------------------------------------------------------------------

    /// Light-weight `create` shim for callers that don't have a full
    /// `NewProject` payload. Mirrors Node `createSimple(...)`.
    pub async fn create_simple(
        &self,
        company_id: Uuid,
        name: &str,
        goal_id: Option<Uuid>,
    ) -> ProjectResult<ProjectRow> {
        let input = NewProject {
            company_id,
            goal_id,
            name: name.to_string(),
            description: None,
            status: ProjectStatus::Backlog,
            lead_agent_id: None,
            target_date: None,
            color: None,
            icon: None,
            env: None,
        };
        self.create(input).await
    }
}

// =============================================================================
// Unit tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_status_terminal_states() {
        assert_eq!(ProjectStatus::Archived.as_str(), "archived");
        assert_eq!(ProjectStatus::Completed.as_str(), "completed");
        assert_eq!(ProjectStatus::Active.as_str(), "active");
    }

    #[test]
    fn normalize_new_rejects_empty_name() {
        let mut input = NewProject {
            company_id: Uuid::new_v4(),
            goal_id: None,
            name: "  ".into(),
            description: None,
            status: ProjectStatus::Backlog,
            lead_agent_id: None,
            target_date: None,
            color: None,
            icon: None,
            env: None,
        };
        assert!(normalize_new(&input).is_err());
        input.name = "valid".into();
        assert!(normalize_new(&input).is_ok());
    }

    #[test]
    fn normalize_new_rejects_nil_company() {
        let input = NewProject {
            company_id: Uuid::nil(),
            goal_id: None,
            name: "x".into(),
            description: None,
            status: ProjectStatus::Backlog,
            lead_agent_id: None,
            target_date: None,
            color: None,
            icon: None,
            env: None,
        };
        assert!(normalize_new(&input).is_err());
    }

    #[test]
    fn normalize_patch_rejects_empty_name() {
        let patch = ProjectPatch {
            name: Some("".into()),
            ..Default::default()
        };
        assert!(normalize_patch(&patch).is_err());
    }

    #[test]
    fn project_status_roundtrip() {
        for s in [
            ProjectStatus::Backlog,
            ProjectStatus::Planned,
            ProjectStatus::Active,
            ProjectStatus::Paused,
            ProjectStatus::Completed,
            ProjectStatus::Archived,
        ] {
            assert_eq!(ProjectStatus::parse(s.as_str()), Some(s));
        }
        assert_eq!(ProjectStatus::parse("nope"), None);
    }
}
