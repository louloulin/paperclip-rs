//! `projects` + `project_memberships` + `project_workspaces` + `project_goals` 域。
//!
//! 设计：
//! - `Project` 是公司工作流的顶层容器，关联 goal / lead agent / 多个 workspace
//! - `ProjectMembership` 记录 user ↔ project 多对多（state: joined / invited / left）
//! - `ProjectWorkspace` 是项目的代码仓库绑定（一个 project 可有多个 workspace：primary/local/remote）
//! - `ProjectGoal` 多对多桥接 `projects ↔ goals`
//! - 状态机：`backlog → planned → active ⇄ paused → completed / archived`

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::{Db, RepoError, RepoResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectStatus {
    Backlog,
    Planned,
    Active,
    Paused,
    Completed,
    Archived,
}
impl ProjectStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Backlog => "backlog",
            Self::Planned => "planned",
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Archived => "archived",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "backlog" => Some(Self::Backlog),
            "planned" => Some(Self::Planned),
            "active" => Some(Self::Active),
            "paused" => Some(Self::Paused),
            "completed" => Some(Self::Completed),
            "archived" => Some(Self::Archived),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MembershipState {
    Joined,
    Invited,
    Left,
}
impl MembershipState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Joined => "joined",
            Self::Invited => "invited",
            Self::Left => "left",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "joined" => Some(Self::Joined),
            "invited" => Some(Self::Invited),
            "left" => Some(Self::Left),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceSourceType {
    LocalPath,
    GitRepo,
    RemoteWorkspace,
    SharedWorkspace,
}
impl WorkspaceSourceType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LocalPath => "local_path",
            Self::GitRepo => "git_repo",
            Self::RemoteWorkspace => "remote_workspace",
            Self::SharedWorkspace => "shared_workspace",
        }
    }
}

const PROJ_COLS: &str = "id, company_id, goal_id, name, description, status, lead_agent_id,     target_date, color, icon, env, pause_reason, paused_at, execution_workspace_policy,     archived_at, created_at, updated_at";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub goal_id: Option<Uuid>,
    pub name: String,
    pub description: Option<String>,
    pub status: String,
    pub lead_agent_id: Option<Uuid>,
    pub target_date: Option<NaiveDate>,
    pub color: Option<String>,
    pub icon: Option<String>,
    pub env: Option<Value>,
    pub pause_reason: Option<String>,
    pub paused_at: Option<Timestamp>,
    pub execution_workspace_policy: Option<Value>,
    pub archived_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectMembershipRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub project_id: Uuid,
    pub user_id: String,
    pub state: String,
    pub starred_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectWorkspaceRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub source_type: String,
    pub cwd: Option<String>,
    pub repo_url: Option<String>,
    pub repo_ref: Option<String>,
    pub default_ref: Option<String>,
    pub visibility: String,
    pub setup_command: Option<String>,
    pub cleanup_command: Option<String>,
    pub remote_provider: Option<String>,
    pub remote_workspace_ref: Option<String>,
    pub shared_workspace_key: Option<String>,
    pub metadata: Option<Value>,
    pub is_primary: bool,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectGoalRow {
    pub project_id: Uuid,
    pub goal_id: Uuid,
    pub company_id: Uuid,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewProject {
    pub company_id: Uuid,
    pub goal_id: Option<Uuid>,
    pub name: String,
    pub description: Option<String>,
    pub status: ProjectStatus,
    pub lead_agent_id: Option<Uuid>,
    pub target_date: Option<NaiveDate>,
    pub color: Option<String>,
    pub icon: Option<String>,
    pub env: Option<Value>,
}

#[derive(Debug, Clone, Default)]
pub struct ProjectPatch {
    pub name: Option<String>,
    pub description: Option<String>,
    pub status: Option<ProjectStatus>,
    pub lead_agent_id: Option<Option<Uuid>>,
    pub target_date: Option<Option<NaiveDate>>,
    pub color: Option<String>,
    pub icon: Option<String>,
    pub env: Option<Value>,
}

pub struct ProjectRepo<'a> {
    pub db: &'a Db,
}

impl<'a> ProjectRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    // ---- projects ----

    pub async fn list_by_company(
        &self,
        company_id: Uuid,
        only_active: bool,
    ) -> RepoResult<Vec<ProjectRow>> {
        let mut sql = format!(
            "SELECT {PROJ_COLS} FROM projects WHERE company_id=$1"
        );
        if only_active {
            sql.push_str(" AND status NOT IN ('archived','completed')");
        }
        sql.push_str(" ORDER BY created_at DESC");
        Ok(sqlx::query_as::<_, ProjectRow>(&sql)
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn list_all(&self, limit: i64) -> RepoResult<Vec<ProjectRow>> {
        let sql = format!(
            "SELECT {PROJ_COLS} FROM projects ORDER BY created_at DESC LIMIT $1"
        );
        Ok(sqlx::query_as::<_, ProjectRow>(&sql)
            .bind(limit)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn get(
        &self,
        company_id: Uuid,
        id: Uuid,
    ) -> RepoResult<Option<ProjectRow>> {
        let sql = format!(
            "SELECT {PROJ_COLS} FROM projects WHERE company_id=$1 AND id=$2"
        );
        Ok(sqlx::query_as::<_, ProjectRow>(&sql)
            .bind(company_id)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    pub async fn create(&self, p: &NewProject) -> RepoResult<ProjectRow> {
        if p.name.trim().is_empty() {
            return Err(RepoError::Invalid("project name must not be empty".into()));
        }
        let sql = format!(
            "INSERT INTO projects (company_id, goal_id, name, description, status, lead_agent_id,                 target_date, color, icon, env)              VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)              RETURNING {PROJ_COLS}"
        );
        Ok(sqlx::query_as::<_, ProjectRow>(&sql)
            .bind(p.company_id)
            .bind(p.goal_id)
            .bind(&p.name)
            .bind(p.description.as_deref())
            .bind(p.status.as_str())
            .bind(p.lead_agent_id)
            .bind(p.target_date)
            .bind(p.color.as_deref())
            .bind(p.icon.as_deref())
            .bind(p.env.clone())
            .fetch_one(self.db.pool())
            .await?)
    }

    pub async fn patch(
        &self,
        company_id: Uuid,
        id: Uuid,
        patch: &ProjectPatch,
    ) -> RepoResult<Option<ProjectRow>> {
        let sql = format!(
            "UPDATE projects SET                 name = COALESCE($2, name),                 description = COALESCE($3, description),                 status = COALESCE($4, status),                 target_date = CASE WHEN $5::bool THEN $6 ELSE target_date END,                 color = COALESCE($7, color),                 icon = COALESCE($8, icon),                 env = CASE WHEN $9::bool THEN $10 ELSE env END,                 lead_agent_id = CASE WHEN $11::bool THEN $12 ELSE lead_agent_id END,                 updated_at = now()              WHERE company_id=$1 AND id=$13              RETURNING {PROJ_COLS}"
        );
        let has_target = patch.target_date.is_some();
        let target = patch.target_date.flatten();
        let has_env = patch.env.is_some();
        let has_lead = patch.lead_agent_id.is_some();
        let lead = patch.lead_agent_id.flatten();
        Ok(sqlx::query_as::<_, ProjectRow>(&sql)
            .bind(company_id)
            .bind(patch.name.as_deref())
            .bind(patch.description.as_deref())
            .bind(patch.status.map(|s| s.as_str()))
            .bind(has_target)
            .bind(target)
            .bind(patch.color.as_deref())
            .bind(patch.icon.as_deref())
            .bind(has_env)
            .bind(patch.env.clone())
            .bind(has_lead)
            .bind(lead)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    pub async fn pause(
        &self,
        company_id: Uuid,
        id: Uuid,
        reason: Option<&str>,
    ) -> RepoResult<Option<ProjectRow>> {
        let sql = format!(
            "UPDATE projects SET status='paused', pause_reason=$3, paused_at=now(), updated_at=now()              WHERE company_id=$1 AND id=$2 AND status NOT IN ('completed','archived')              RETURNING {PROJ_COLS}"
        );
        Ok(sqlx::query_as::<_, ProjectRow>(&sql)
            .bind(company_id)
            .bind(id)
            .bind(reason)
            .fetch_optional(self.db.pool())
            .await?)
    }

    pub async fn resume(
        &self,
        company_id: Uuid,
        id: Uuid,
    ) -> RepoResult<Option<ProjectRow>> {
        let sql = format!(
            "UPDATE projects SET status='active', pause_reason=NULL, paused_at=NULL, updated_at=now()              WHERE company_id=$1 AND id=$2 AND status='paused'              RETURNING {PROJ_COLS}"
        );
        Ok(sqlx::query_as::<_, ProjectRow>(&sql)
            .bind(company_id)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    /// Back-compat shim: legacy update with positional (id, name, description, status).
    #[allow(dead_code)]
    pub async fn update(
        &self,
        id: Uuid,
        name: Option<&str>,
        description: Option<&str>,
        status: Option<&str>,
    ) -> RepoResult<Option<ProjectRow>> {
        // find company_id
        let cid: Option<Uuid> = sqlx::query_scalar("SELECT company_id FROM projects WHERE id=$1")
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?;
        let cid = cid.ok_or_else(|| RepoError::NotFound {
            entity: "project",
            id: id.to_string(),
        })?;
        let p = ProjectPatch {
            name: name.map(String::from),
            description: description.map(String::from),
            status: status.and_then(|s| ProjectStatus::parse(s)),
            ..Default::default()
        };
        self.patch(cid, id, &p).await
    }

    pub async fn archive(&self, company_id: Uuid, id: Uuid) -> RepoResult<Option<ProjectRow>> {
        let sql = format!(
            "UPDATE projects SET status='archived', archived_at=now(), updated_at=now()              WHERE company_id=$1 AND id=$2 RETURNING {PROJ_COLS}"
        );
        Ok(sqlx::query_as::<_, ProjectRow>(&sql)
            .bind(company_id)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    pub async fn delete(&self, company_id: Uuid, id: Uuid) -> RepoResult<bool> {
        let n = sqlx::query("DELETE FROM projects WHERE company_id=$1 AND id=$2")
            .bind(company_id)
            .bind(id)
            .execute(self.db.pool())
            .await?
            .rows_affected();
        Ok(n > 0)
    }

    // ---- memberships ----

    pub async fn list_memberships(
        &self,
        project_id: Uuid,
    ) -> RepoResult<Vec<ProjectMembershipRow>> {
        Ok(sqlx::query_as::<_, ProjectMembershipRow>(
            "SELECT id, company_id, project_id, user_id, state, starred_at, created_at, updated_at              FROM project_memberships WHERE project_id=$1 ORDER BY created_at",
        )
        .bind(project_id)
        .fetch_all(self.db.pool())
        .await?)
    }

    pub async fn list_user_memberships(
        &self,
        company_id: Uuid,
        user_id: &str,
    ) -> RepoResult<Vec<ProjectMembershipRow>> {
        Ok(sqlx::query_as::<_, ProjectMembershipRow>(
            "SELECT id, company_id, project_id, user_id, state, starred_at, created_at, updated_at              FROM project_memberships WHERE company_id=$1 AND user_id=$2 AND state!='left'              ORDER BY starred_at DESC NULLS LAST, created_at",
        )
        .bind(company_id)
        .bind(user_id)
        .fetch_all(self.db.pool())
        .await?)
    }

    pub async fn upsert_membership(
        &self,
        company_id: Uuid,
        project_id: Uuid,
        user_id: &str,
        state: MembershipState,
    ) -> RepoResult<ProjectMembershipRow> {
        Ok(sqlx::query_as::<_, ProjectMembershipRow>(
            "INSERT INTO project_memberships (company_id, project_id, user_id, state)              VALUES ($1,$2,$3,$4)              ON CONFLICT (company_id, user_id, project_id) DO UPDATE SET                 state=EXCLUDED.state, updated_at=now()              RETURNING id, company_id, project_id, user_id, state, starred_at, created_at, updated_at",
        )
        .bind(company_id)
        .bind(project_id)
        .bind(user_id)
        .bind(state.as_str())
        .fetch_one(self.db.pool())
        .await?)
    }

    pub async fn toggle_star(
        &self,
        project_id: Uuid,
        user_id: &str,
        star: bool,
    ) -> RepoResult<Option<ProjectMembershipRow>> {
        Ok(sqlx::query_as::<_, ProjectMembershipRow>(
            "UPDATE project_memberships SET starred_at = CASE WHEN $2 THEN now() ELSE NULL END,              updated_at=now()              WHERE project_id=$1 AND user_id=$3              RETURNING id, company_id, project_id, user_id, state, starred_at, created_at, updated_at",
        )
        .bind(project_id)
        .bind(star)
        .bind(user_id)
        .fetch_optional(self.db.pool())
        .await?)
    }

    // ---- workspaces ----

    pub async fn list_workspaces(
        &self,
        project_id: Uuid,
    ) -> RepoResult<Vec<ProjectWorkspaceRow>> {
        Ok(sqlx::query_as::<_, ProjectWorkspaceRow>(
            "SELECT id, company_id, project_id, name, source_type, cwd, repo_url, repo_ref,              default_ref, visibility, setup_command, cleanup_command, remote_provider,              remote_workspace_ref, shared_workspace_key, metadata, is_primary,              created_at, updated_at              FROM project_workspaces WHERE project_id=$1 ORDER BY is_primary DESC, created_at",
        )
        .bind(project_id)
        .fetch_all(self.db.pool())
        .await?)
    }

    pub async fn get_primary_workspace(
        &self,
        project_id: Uuid,
    ) -> RepoResult<Option<ProjectWorkspaceRow>> {
        Ok(sqlx::query_as::<_, ProjectWorkspaceRow>(
            "SELECT id, company_id, project_id, name, source_type, cwd, repo_url, repo_ref,              default_ref, visibility, setup_command, cleanup_command, remote_provider,              remote_workspace_ref, shared_workspace_key, metadata, is_primary,              created_at, updated_at              FROM project_workspaces WHERE project_id=$1 AND is_primary=true LIMIT 1",
        )
        .bind(project_id)
        .fetch_optional(self.db.pool())
        .await?)
    }

    pub async fn create_workspace(
        &self,
        w: &ProjectWorkspaceRow,
    ) -> RepoResult<ProjectWorkspaceRow> {
        let mut tx = self.db.pool().begin().await?;
        // primary 唯一性
        if w.is_primary {
            sqlx::query(
                "UPDATE project_workspaces SET is_primary=false, updated_at=now()                  WHERE project_id=$1 AND is_primary=true",
            )
            .bind(w.project_id)
            .execute(&mut *tx)
            .await?;
        }
        let row = sqlx::query_as::<_, ProjectWorkspaceRow>(
            "INSERT INTO project_workspaces (company_id, project_id, name, source_type, cwd,                 repo_url, repo_ref, default_ref, visibility, setup_command, cleanup_command,                 remote_provider, remote_workspace_ref, shared_workspace_key, metadata, is_primary)              VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16)              RETURNING id, company_id, project_id, name, source_type, cwd, repo_url, repo_ref,              default_ref, visibility, setup_command, cleanup_command, remote_provider,              remote_workspace_ref, shared_workspace_key, metadata, is_primary,              created_at, updated_at",
        )
        .bind(w.company_id)
        .bind(w.project_id)
        .bind(&w.name)
        .bind(&w.source_type)
        .bind(w.cwd.as_deref())
        .bind(w.repo_url.as_deref())
        .bind(w.repo_ref.as_deref())
        .bind(w.default_ref.as_deref())
        .bind(&w.visibility)
        .bind(w.setup_command.as_deref())
        .bind(w.cleanup_command.as_deref())
        .bind(w.remote_provider.as_deref())
        .bind(w.remote_workspace_ref.as_deref())
        .bind(w.shared_workspace_key.as_deref())
        .bind(w.metadata.clone())
        .bind(w.is_primary)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(row)
    }

    pub async fn set_primary_workspace(
        &self,
        project_id: Uuid,
        workspace_id: Uuid,
    ) -> RepoResult<bool> {
        let mut tx = self.db.pool().begin().await?;
        sqlx::query(
            "UPDATE project_workspaces SET is_primary=false, updated_at=now()              WHERE project_id=$1 AND is_primary=true",
        )
        .bind(project_id)
        .execute(&mut *tx)
        .await?;
        let n = sqlx::query(
            "UPDATE project_workspaces SET is_primary=true, updated_at=now()              WHERE project_id=$1 AND id=$2",
        )
        .bind(project_id)
        .bind(workspace_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        tx.commit().await?;
        Ok(n > 0)
    }

    // ---- goals bridge ----

    pub async fn attach_goal(
        &self,
        company_id: Uuid,
        project_id: Uuid,
        goal_id: Uuid,
    ) -> RepoResult<()> {
        sqlx::query(
            "INSERT INTO project_goals (company_id, project_id, goal_id) VALUES ($1,$2,$3)              ON CONFLICT (project_id, goal_id) DO NOTHING",
        )
        .bind(company_id)
        .bind(project_id)
        .bind(goal_id)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    pub async fn detach_goal(
        &self,
        project_id: Uuid,
        goal_id: Uuid,
    ) -> RepoResult<bool> {
        let n = sqlx::query(
            "DELETE FROM project_goals WHERE project_id=$1 AND goal_id=$2",
        )
        .bind(project_id)
        .bind(goal_id)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    pub async fn goals_for_project(
        &self,
        project_id: Uuid,
    ) -> RepoResult<Vec<ProjectGoalRow>> {
        Ok(sqlx::query_as::<_, ProjectGoalRow>(
            "SELECT project_id, goal_id, company_id, created_at, updated_at              FROM project_goals WHERE project_id=$1 ORDER BY created_at",
        )
        .bind(project_id)
        .fetch_all(self.db.pool())
        .await?)
    }
    // --------- Backward-compat positional shims ---------

    /// Back-compat: list_by_company with default behaviour (true = skip archived).
    #[allow(dead_code)]
    pub async fn list_by_company_no_filter(&self, company_id: Uuid) -> RepoResult<Vec<ProjectRow>> {
        self.list_by_company(company_id, true).await
    }

    /// Back-compat: get by id only.
    #[allow(dead_code)]
    pub async fn get_id_only(&self, id: Uuid) -> RepoResult<Option<ProjectRow>> {
        let sql = format!("SELECT {PROJ_COLS} FROM projects WHERE id=$1");
        Ok(sqlx::query_as::<_, ProjectRow>(&sql)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    /// Back-compat: create(company_id, name, description).
    #[allow(dead_code)]
    pub async fn create_simple(
        &self,
        company_id: Uuid,
        name: &str,
        description: Option<&str>,
    ) -> RepoResult<ProjectRow> {
        let n = NewProject {
            company_id,
            goal_id: None,
            name: name.into(),
            description: description.map(String::from),
            status: ProjectStatus::Backlog,
            lead_agent_id: None,
            target_date: None,
            color: None,
            icon: None,
            env: None,
        };
        self.create(&n).await
    }

    /// Back-compat: delete by id only.
    #[allow(dead_code)]
    pub async fn delete_one(&self, id: Uuid) -> RepoResult<bool> {
        let n = sqlx::query("DELETE FROM projects WHERE id=$1")
            .bind(id)
            .execute(self.db.pool())
            .await?
            .rows_affected();
        Ok(n > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_status_round_trip() {
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

    #[test]
    fn membership_state_strings() {
        assert_eq!(MembershipState::Joined.as_str(), "joined");
        assert_eq!(MembershipState::Invited.as_str(), "invited");
        assert_eq!(MembershipState::Left.as_str(), "left");
    }

    #[test]
    fn new_project_requires_name() {
        let p = NewProject {
            company_id: Uuid::new_v4(),
            goal_id: None,
            name: "".into(),
            description: None,
            status: ProjectStatus::Backlog,
            lead_agent_id: None,
            target_date: None,
            color: None,
            icon: None,
            env: None,
        };
        assert!(p.name.trim().is_empty());
    }
}
