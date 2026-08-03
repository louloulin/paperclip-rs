//! `pipeline` 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;


#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct PipelineStageRow {
    pub id: Uuid,
    pub pipeline_id: Uuid,
    pub key: String,
    pub name: String,
    pub kind: String,
    pub position: i32,
    pub config: serde_json::Value,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct PipelineTransitionRow {
    pub id: Uuid,
    pub pipeline_id: Uuid,
    pub from_stage_id: Uuid,
    pub to_stage_id: Uuid,
    pub label: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct PipelineCaseRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub pipeline_id: Uuid,
    pub stage_id: Uuid,
    pub case_key: String,
    pub title: String,
    pub summary: Option<String>,
    pub fields: serde_json::Value,
    pub workspace_ref: Option<serde_json::Value>,
    pub parent_case_id: Option<Uuid>,
    pub version: i32,
    pub pending_suggestion: Option<serde_json::Value>,
    pub lease_owner_type: Option<String>,
    pub lease_agent_id: Option<Uuid>,
    pub lease_user_id: Option<String>,
    pub lease_token: Option<Uuid>,
    pub lease_expires_at: Option<Timestamp>,
    pub terminal_kind: Option<String>,
    pub terminal_at: Option<Timestamp>,
    pub child_count: i32,
    pub terminal_child_count: i32,
    pub created_by_user_id: Option<String>,
    pub created_by_agent_id: Option<Uuid>,
    pub origin_run_id: Option<Uuid>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct PipelineCaseEventRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub case_id: Uuid,
    pub actor_agent_id: Option<Uuid>,
    pub actor_user_id: Option<String>,
    pub event_type: String,
    pub from_stage_id: Option<Uuid>,
    pub to_stage_id: Option<Uuid>,
    pub note: Option<String>,
    pub payload: Option<serde_json::Value>,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct PipelineCaseIssueLinkRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub case_id: Uuid,
    pub issue_id: Uuid,
    pub role: String,
    pub created_at: Timestamp,
}
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct PipelineRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub project_id: Option<Uuid>,
    pub key: String,
    pub name: String,
    pub description: Option<String>,
    pub enforce_transitions: bool,
    pub created_by_user_id: Option<String>,
    pub created_by_agent_id: Option<Uuid>,
    pub archived_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

const COLS: &str = "id, company_id, project_id, key, name, description, enforce_transitions, \
    created_by_user_id, created_by_agent_id, archived_at, created_at, updated_at";

pub struct PipelineRepo<'a> {
    pub db: &'a Db,
}

impl<'a> PipelineRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    pub async fn list_by_company(&self, company_id: Uuid) -> sqlx::Result<Vec<PipelineRow>> {
        let sql =
            format!("SELECT {COLS} FROM pipelines WHERE company_id = $1 ORDER BY created_at DESC");
        sqlx::query_as::<_, PipelineRow>(&sql)
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await
    }

    /// 列出全部（跨公司）；limit 默认 200。
    pub async fn list_all(
        &self,
        limit: i64,
    ) -> sqlx::Result<Vec<PipelineRow>> {
        let sql = format!(
            "SELECT {COLS} FROM pipelines ORDER BY created_at DESC LIMIT $1"
        );
        sqlx::query_as::<_, PipelineRow>(&sql)
            .bind(limit)
            .fetch_all(self.db.pool())
            .await
    }

    pub async fn get(&self, id: Uuid) -> sqlx::Result<Option<PipelineRow>> {
        let sql = format!("SELECT {COLS} FROM pipelines WHERE id = $1");
        sqlx::query_as::<_, PipelineRow>(&sql)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn create(
        &self,
        company_id: Uuid,
        key: &str,
        name: &str,
        description: Option<&str>,
    ) -> sqlx::Result<PipelineRow> {
        let sql = format!(
            "INSERT INTO pipelines (company_id, key, name, description) VALUES ($1,$2,$3,$4) RETURNING {COLS}"
        );
        sqlx::query_as::<_, PipelineRow>(&sql)
            .bind(company_id)
            .bind(key)
            .bind(name)
            .bind(description)
            .fetch_one(self.db.pool())
            .await
    }

    pub async fn update(
        &self,
        id: Uuid,
        name: Option<&str>,
        description: Option<&str>,
    ) -> sqlx::Result<Option<PipelineRow>> {
        let sql = format!(
            "UPDATE pipelines SET name=COALESCE($2,name), description=COALESCE($3,description), updated_at=now() WHERE id=$1 RETURNING {COLS}"
        );
        sqlx::query_as::<_, PipelineRow>(&sql)
            .bind(id)
            .bind(name)
            .bind(description)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn delete(&self, id: Uuid) -> sqlx::Result<bool> {
        let r = sqlx::query("DELETE FROM pipelines WHERE id = $1")
            .bind(id)
            .execute(self.db.pool())
            .await?;
        Ok(r.rows_affected() > 0)
    }

    // =========================================================================
    // Pipeline stages
    // =========================================================================

    pub async fn list_stages(&self, pipeline_id: Uuid) -> sqlx::Result<Vec<PipelineStageRow>> {
        sqlx::query_as::<_, PipelineStageRow>(
            "SELECT id, pipeline_id, key, name, kind, position, config, created_at, updated_at \
             FROM pipeline_stages WHERE pipeline_id = $1 ORDER BY position ASC, created_at ASC",
        )
        .bind(pipeline_id)
        .fetch_all(self.db.pool())
        .await
    }

    pub async fn get_stage(&self, stage_id: Uuid) -> sqlx::Result<Option<PipelineStageRow>> {
        sqlx::query_as::<_, PipelineStageRow>(
            "SELECT id, pipeline_id, key, name, kind, position, config, created_at, updated_at \
             FROM pipeline_stages WHERE id = $1",
        )
        .bind(stage_id)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn create_stage(
        &self,
        pipeline_id: Uuid,
        key: &str,
        name: &str,
        kind: &str,
        position: i32,
        config: &serde_json::Value,
    ) -> sqlx::Result<PipelineStageRow> {
        sqlx::query_as::<_, PipelineStageRow>(
            "INSERT INTO pipeline_stages (pipeline_id, key, name, kind, position, config) \
             VALUES ($1,$2,$3,$4,$5,$6) \
             RETURNING id, pipeline_id, key, name, kind, position, config, created_at, updated_at",
        )
        .bind(pipeline_id)
        .bind(key)
        .bind(name)
        .bind(kind)
        .bind(position)
        .bind(config)
        .fetch_one(self.db.pool())
        .await
    }

    pub async fn update_stage(
        &self,
        stage_id: Uuid,
        name: Option<&str>,
        kind: Option<&str>,
        position: Option<i32>,
        config: Option<&serde_json::Value>,
    ) -> sqlx::Result<Option<PipelineStageRow>> {
        sqlx::query_as::<_, PipelineStageRow>(
            "UPDATE pipeline_stages SET \
                name = COALESCE($2, name), kind = COALESCE($3, kind), \
                position = COALESCE($4, position), config = COALESCE($5, config), \
                updated_at = now() \
             WHERE id = $1 \
             RETURNING id, pipeline_id, key, name, kind, position, config, created_at, updated_at",
        )
        .bind(stage_id)
        .bind(name)
        .bind(kind)
        .bind(position)
        .bind(config)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn delete_stage(&self, stage_id: Uuid) -> sqlx::Result<bool> {
        let r = sqlx::query("DELETE FROM pipeline_stages WHERE id = $1")
            .bind(stage_id)
            .execute(self.db.pool())
            .await?;
        Ok(r.rows_affected() > 0)
    }

    // =========================================================================
    // Pipeline transitions
    // =========================================================================

    pub async fn list_transitions(
        &self,
        pipeline_id: Uuid,
    ) -> sqlx::Result<Vec<PipelineTransitionRow>> {
        sqlx::query_as::<_, PipelineTransitionRow>(
            "SELECT id, pipeline_id, from_stage_id, to_stage_id, label, created_at, updated_at \
             FROM pipeline_transitions WHERE pipeline_id = $1 ORDER BY created_at ASC",
        )
        .bind(pipeline_id)
        .fetch_all(self.db.pool())
        .await
    }

    pub async fn create_transition(
        &self,
        pipeline_id: Uuid,
        from_stage_id: Uuid,
        to_stage_id: Uuid,
        label: Option<&str>,
    ) -> sqlx::Result<PipelineTransitionRow> {
        sqlx::query_as::<_, PipelineTransitionRow>(
            "INSERT INTO pipeline_transitions (pipeline_id, from_stage_id, to_stage_id, label) \
             VALUES ($1,$2,$3,$4) \
             RETURNING id, pipeline_id, from_stage_id, to_stage_id, label, created_at, updated_at",
        )
        .bind(pipeline_id)
        .bind(from_stage_id)
        .bind(to_stage_id)
        .bind(label)
        .fetch_one(self.db.pool())
        .await
    }

    pub async fn delete_transition(&self, transition_id: Uuid) -> sqlx::Result<bool> {
        let r = sqlx::query("DELETE FROM pipeline_transitions WHERE id = $1")
            .bind(transition_id)
            .execute(self.db.pool())
            .await?;
        Ok(r.rows_affected() > 0)
    }

    pub async fn is_valid_transition(
        &self,
        pipeline_id: Uuid,
        from_stage_id: Uuid,
        to_stage_id: Uuid,
    ) -> sqlx::Result<bool> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM pipeline_transitions \
             WHERE pipeline_id = $1 AND from_stage_id = $2 AND to_stage_id = $3",
        )
        .bind(pipeline_id)
        .bind(from_stage_id)
        .bind(to_stage_id)
        .fetch_one(self.db.pool())
        .await?;
        Ok(count > 0)
    }

    // =========================================================================
    // Pipeline cases
    // =========================================================================

    pub async fn list_cases(
        &self,
        pipeline_id: Uuid,
        stage_id: Option<Uuid>,
    ) -> sqlx::Result<Vec<PipelineCaseRow>> {
        sqlx::query_as::<_, PipelineCaseRow>(
            "SELECT id, company_id, pipeline_id, stage_id, case_key, title, summary, fields, \
                    workspace_ref, parent_case_id, version, pending_suggestion, \
                    lease_owner_type, lease_agent_id, lease_user_id, lease_token, lease_expires_at, \
                    terminal_kind, terminal_at, child_count, terminal_child_count, \
                    created_by_user_id, created_by_agent_id, origin_run_id, \
                    created_at, updated_at \
             FROM pipeline_cases WHERE pipeline_id = $1 \
             AND ($2::uuid IS NULL OR stage_id = $2) \
             ORDER BY created_at DESC",
        )
        .bind(pipeline_id)
        .bind(stage_id)
        .fetch_all(self.db.pool())
        .await
    }

    pub async fn get_case(&self, case_id: Uuid) -> sqlx::Result<Option<PipelineCaseRow>> {
        sqlx::query_as::<_, PipelineCaseRow>(
            "SELECT id, company_id, pipeline_id, stage_id, case_key, title, summary, fields, \
                    workspace_ref, parent_case_id, version, pending_suggestion, \
                    lease_owner_type, lease_agent_id, lease_user_id, lease_token, lease_expires_at, \
                    terminal_kind, terminal_at, child_count, terminal_child_count, \
                    created_by_user_id, created_by_agent_id, origin_run_id, \
                    created_at, updated_at \
             FROM pipeline_cases WHERE id = $1",
        )
        .bind(case_id)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn create_case(
        &self,
        company_id: Uuid,
        pipeline_id: Uuid,
        stage_id: Uuid,
        case_key: &str,
        title: &str,
        summary: Option<&str>,
        fields: &serde_json::Value,
        parent_case_id: Option<Uuid>,
        created_by_user_id: Option<&str>,
        created_by_agent_id: Option<Uuid>,
        origin_run_id: Option<Uuid>,
    ) -> sqlx::Result<PipelineCaseRow> {
        sqlx::query_as::<_, PipelineCaseRow>(
            "INSERT INTO pipeline_cases \
                (company_id, pipeline_id, stage_id, case_key, title, summary, fields, \
                 parent_case_id, created_by_user_id, created_by_agent_id, origin_run_id) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) \
             RETURNING id, company_id, pipeline_id, stage_id, case_key, title, summary, fields, \
                    workspace_ref, parent_case_id, version, pending_suggestion, \
                    lease_owner_type, lease_agent_id, lease_user_id, lease_token, lease_expires_at, \
                    terminal_kind, terminal_at, child_count, terminal_child_count, \
                    created_by_user_id, created_by_agent_id, origin_run_id, \
                    created_at, updated_at",
        )
        .bind(company_id)
        .bind(pipeline_id)
        .bind(stage_id)
        .bind(case_key)
        .bind(title)
        .bind(summary)
        .bind(fields)
        .bind(parent_case_id)
        .bind(created_by_user_id)
        .bind(created_by_agent_id)
        .bind(origin_run_id)
        .fetch_one(self.db.pool())
        .await
    }

    pub async fn update_case_stage(
        &self,
        case_id: Uuid,
        new_stage_id: Uuid,
        from_stage_id: Uuid,
    ) -> sqlx::Result<Option<PipelineCaseRow>> {
        sqlx::query_as::<_, PipelineCaseRow>(
            "UPDATE pipeline_cases SET \
                stage_id = $2, version = version + 1, \
                terminal_kind = CASE WHEN (SELECT kind FROM pipeline_stages WHERE id = $2) IN ('done', 'cancelled') \
                                    THEN (SELECT kind FROM pipeline_stages WHERE id = $2) ELSE terminal_kind END, \
                terminal_at = CASE WHEN (SELECT kind FROM pipeline_stages WHERE id = $2) IN ('done', 'cancelled') \
                                   THEN now() ELSE terminal_at END, \
                updated_at = now() \
             WHERE id = $1 AND stage_id = $3 \
             RETURNING id, company_id, pipeline_id, stage_id, case_key, title, summary, fields, \
                    workspace_ref, parent_case_id, version, pending_suggestion, \
                    lease_owner_type, lease_agent_id, lease_user_id, lease_token, lease_expires_at, \
                    terminal_kind, terminal_at, child_count, terminal_child_count, \
                    created_by_user_id, created_by_agent_id, origin_run_id, \
                    created_at, updated_at",
        )
        .bind(case_id)
        .bind(new_stage_id)
        .bind(from_stage_id)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn claim_case(
        &self,
        case_id: Uuid,
        owner_type: &str,
        owner_agent_id: Option<Uuid>,
        owner_user_id: Option<&str>,
        lease_token: Uuid,
    ) -> sqlx::Result<Option<PipelineCaseRow>> {
        sqlx::query_as::<_, PipelineCaseRow>(
            "UPDATE pipeline_cases SET \
                lease_owner_type = $2, lease_agent_id = $3, lease_user_id = $4, \
                lease_token = $5, lease_expires_at = now() + interval '1 hour', \
                updated_at = now() \
             WHERE id = $1 \
             RETURNING id, company_id, pipeline_id, stage_id, case_key, title, summary, fields, \
                    workspace_ref, parent_case_id, version, pending_suggestion, \
                    lease_owner_type, lease_agent_id, lease_user_id, lease_token, lease_expires_at, \
                    terminal_kind, terminal_at, child_count, terminal_child_count, \
                    created_by_user_id, created_by_agent_id, origin_run_id, \
                    created_at, updated_at",
        )
        .bind(case_id)
        .bind(owner_type)
        .bind(owner_agent_id)
        .bind(owner_user_id)
        .bind(lease_token)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn release_case(&self, case_id: Uuid) -> sqlx::Result<Option<PipelineCaseRow>> {
        sqlx::query_as::<_, PipelineCaseRow>(
            "UPDATE pipeline_cases SET \
                lease_owner_type = NULL, lease_agent_id = NULL, lease_user_id = NULL, \
                lease_token = NULL, lease_expires_at = NULL, updated_at = now() \
             WHERE id = $1 \
             RETURNING id, company_id, pipeline_id, stage_id, case_key, title, summary, fields, \
                    workspace_ref, parent_case_id, version, pending_suggestion, \
                    lease_owner_type, lease_agent_id, lease_user_id, lease_token, lease_expires_at, \
                    terminal_kind, terminal_at, child_count, terminal_child_count, \
                    created_by_user_id, created_by_agent_id, origin_run_id, \
                    created_at, updated_at",
        )
        .bind(case_id)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn delete_case(&self, case_id: Uuid) -> sqlx::Result<bool> {
        let r = sqlx::query("DELETE FROM pipeline_cases WHERE id = $1")
            .bind(case_id)
            .execute(self.db.pool())
            .await?;
        Ok(r.rows_affected() > 0)
    }

    // =========================================================================
    // Pipeline case events (history)
    // =========================================================================

    pub async fn list_case_events(
        &self,
        case_id: Uuid,
    ) -> sqlx::Result<Vec<PipelineCaseEventRow>> {
        sqlx::query_as::<_, PipelineCaseEventRow>(
            "SELECT id, company_id, case_id, actor_agent_id, actor_user_id, event_type, \
                    from_stage_id, to_stage_id, note, payload, created_at \
             FROM pipeline_case_events WHERE case_id = $1 ORDER BY created_at ASC",
        )
        .bind(case_id)
        .fetch_all(self.db.pool())
        .await
    }

    pub async fn create_case_event(
        &self,
        company_id: Uuid,
        case_id: Uuid,
        event_type: &str,
        from_stage_id: Option<Uuid>,
        to_stage_id: Option<Uuid>,
        payload: Option<&serde_json::Value>,
        actor_type: &str,
        actor_agent_id: Option<Uuid>,
        actor_user_id: Option<&str>,
        run_id: Option<Uuid>,
    ) -> sqlx::Result<PipelineCaseEventRow> {
        let payload_value = payload.cloned().unwrap_or_else(|| serde_json::json!({}));
        sqlx::query_as::<_, PipelineCaseEventRow>(
            "INSERT INTO pipeline_case_events \
                (company_id, case_id, type, from_stage_id, to_stage_id, payload, \
                 actor_type, actor_agent_id, actor_user_id, run_id) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) \
             RETURNING id, company_id, case_id, type, actor_type, actor_user_id, actor_agent_id, \
                    run_id, from_stage_id, to_stage_id, payload, created_at, updated_at",
        )
        .bind(company_id)
        .bind(case_id)
        .bind(event_type)
        .bind(from_stage_id)
        .bind(to_stage_id)
        .bind(&payload_value)
        .bind(actor_type)
        .bind(actor_agent_id)
        .bind(actor_user_id)
        .bind(run_id)
        .fetch_one(self.db.pool())
        .await
    }

    // =========================================================================
    // Pipeline case issue links
    // =========================================================================

    pub async fn list_case_issue_links(
        &self,
        case_id: Uuid,
    ) -> sqlx::Result<Vec<PipelineCaseIssueLinkRow>> {
        sqlx::query_as::<_, PipelineCaseIssueLinkRow>(
            "SELECT id, company_id, case_id, issue_id, role, created_at \
             FROM pipeline_case_issue_links WHERE case_id = $1 ORDER BY created_at DESC",
        )
        .bind(case_id)
        .fetch_all(self.db.pool())
        .await
    }

    pub async fn link_case_issue(
        &self,
        company_id: Uuid,
        case_id: Uuid,
        issue_id: Uuid,
        role: &str,
    ) -> sqlx::Result<PipelineCaseIssueLinkRow> {
        sqlx::query_as::<_, PipelineCaseIssueLinkRow>(
            "INSERT INTO pipeline_case_issue_links (company_id, case_id, issue_id, role) \
             VALUES ($1,$2,$3,$4) \
             ON CONFLICT (case_id, issue_id) DO NOTHING \
             RETURNING id, company_id, case_id, issue_id, role, created_at",
        )
        .bind(company_id)
        .bind(case_id)
        .bind(issue_id)
        .bind(role)
        .fetch_one(self.db.pool())
        .await
    }

    pub async fn unlink_case_issue(
        &self,
        case_id: Uuid,
        link_id: Uuid,
    ) -> sqlx::Result<bool> {
        let r = sqlx::query(
            "DELETE FROM pipeline_case_issue_links WHERE id = $1 AND case_id = $2",
        )
        .bind(link_id)
        .bind(case_id)
        .execute(self.db.pool())
        .await?;
        Ok(r.rows_affected() > 0)
    }

    // =========================================================================
    // Pipeline archive
    // =========================================================================

    pub async fn archive_pipeline(&self, pipeline_id: Uuid) -> sqlx::Result<Option<PipelineRow>> {
        sqlx::query_as::<_, PipelineRow>(
            "UPDATE pipelines SET archived_at = now(), updated_at = now() \
             WHERE id = $1 AND archived_at IS NULL \
             RETURNING id, company_id, project_id, key, name, description, enforce_transitions, \
                created_by_user_id, created_by_agent_id, archived_at, created_at, updated_at",
        )
        .bind(pipeline_id)
        .fetch_optional(self.db.pool())
        .await
    }

}
