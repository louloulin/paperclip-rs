//! `routine` 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct RoutineRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub project_id: Option<Uuid>,
    pub folder_id: Option<Uuid>,
    pub goal_id: Option<Uuid>,
    pub parent_issue_id: Option<Uuid>,
    pub title: String,
    pub description: Option<String>,
    pub assignee_agent_id: Option<Uuid>,
    pub priority: String,
    pub status: String,
    pub concurrency_policy: String,
    pub catch_up_policy: String,
    pub activity_gate_policy: String,
    pub activity_gate_scope: String,
    pub origin_kind: String,
    pub origin_id: Option<String>,
    pub variables: serde_json::Value,
    pub env: Option<serde_json::Value>,
    pub latest_revision_id: Option<Uuid>,
    pub latest_revision_number: i32,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub responsible_user_id: Option<String>,
    pub updated_by_agent_id: Option<Uuid>,
    pub updated_by_user_id: Option<String>,
    pub last_triggered_at: Option<Timestamp>,
    pub last_enqueued_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

const COLS: &str = "id, company_id, project_id, folder_id, goal_id, parent_issue_id, \
    title, description, assignee_agent_id, priority, status, \
    concurrency_policy, catch_up_policy, activity_gate_policy, activity_gate_scope, \
    origin_kind, origin_id, variables, env, latest_revision_id, latest_revision_number, \
    created_by_agent_id, created_by_user_id, responsible_user_id, updated_by_agent_id, \
    updated_by_user_id, last_triggered_at, last_enqueued_at, created_at, updated_at";

pub struct RoutineRepo<'a> {
    pub db: &'a Db,
}

impl<'a> RoutineRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    pub async fn list_by_company(&self, company_id: Uuid) -> sqlx::Result<Vec<RoutineRow>> {
        let sql = format!(
            "SELECT {COLS} FROM routines WHERE company_id = $1 ORDER BY created_at DESC LIMIT 200"
        );
        sqlx::query_as::<_, RoutineRow>(&sql)
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await
    }

    pub async fn get(&self, id: Uuid) -> sqlx::Result<Option<RoutineRow>> {
        let sql = format!("SELECT {COLS} FROM routines WHERE id = $1");
        sqlx::query_as::<_, RoutineRow>(&sql)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn create(
        &self,
        company_id: Uuid,
        title: &str,
        description: Option<&str>,
        assignee_agent_id: Option<Uuid>,
    ) -> sqlx::Result<RoutineRow> {
        let sql = format!(
            "INSERT INTO routines (company_id, title, description, assignee_agent_id) \
             VALUES ($1,$2,$3,$4) RETURNING {COLS}"
        );
        sqlx::query_as::<_, RoutineRow>(&sql)
            .bind(company_id)
            .bind(title)
            .bind(description)
            .bind(assignee_agent_id)
            .fetch_one(self.db.pool())
            .await
    }

    pub async fn update(
        &self,
        id: Uuid,
        title: Option<&str>,
        description: Option<&str>,
        status: Option<&str>,
    ) -> sqlx::Result<Option<RoutineRow>> {
        let sql = format!(
            "UPDATE routines SET title=COALESCE($2,title), description=COALESCE($3,description), \
             status=COALESCE($4,status), updated_at=now() WHERE id=$1 RETURNING {COLS}"
        );
        sqlx::query_as::<_, RoutineRow>(&sql)
            .bind(id)
            .bind(title)
            .bind(description)
            .bind(status)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn trigger(&self, id: Uuid) -> sqlx::Result<Option<RoutineRow>> {
        let sql = format!(
            "UPDATE routines SET last_triggered_at=now(), last_enqueued_at=now(), updated_at=now() WHERE id=$1 RETURNING {COLS}"
        );
        sqlx::query_as::<_, RoutineRow>(&sql)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn delete(&self, id: Uuid) -> sqlx::Result<bool> {
        let r = sqlx::query("DELETE FROM routines WHERE id = $1")
            .bind(id)
            .execute(self.db.pool())
            .await?;
        Ok(r.rows_affected() > 0)
    }
}
