use crate::Db;
use pc_core::Timestamp;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct ActivityRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub actor_type: String,
    pub actor_id: String,
    pub action: String,
    pub entity_type: String,
    pub entity_id: String,
    pub agent_id: Option<Uuid>,
    pub run_id: Option<Uuid>,
    pub responsible_user_id: Option<String>,
    pub details: Option<serde_json::Value>,
    pub created_at: Timestamp,
}
pub struct ActivityRepo<'a> {
    pub db: &'a Db,
}
impl<'a> ActivityRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }
    pub async fn list_by_company(&self, c: Uuid, limit: i64) -> sqlx::Result<Vec<ActivityRow>> {
        sqlx::query_as::<_, ActivityRow>(
            "SELECT id, company_id, actor_type, actor_id, action, entity_type, entity_id, \
                    agent_id, run_id, responsible_user_id, details, created_at \
             FROM activity_log WHERE company_id = $1 ORDER BY created_at DESC LIMIT $2",
        )
        .bind(c)
        .bind(limit)
        .fetch_all(self.db.pool())
        .await
    }
    pub async fn log(
        &self,
        c: Uuid,
        actor_type: &str,
        actor_id: &str,
        action: &str,
        entity_type: &str,
        entity_id: &str,
    ) -> sqlx::Result<ActivityRow> {
        sqlx::query_as::<_, ActivityRow>(
            "INSERT INTO activity_log (company_id, actor_type, actor_id, action, entity_type, entity_id) \
             VALUES ($1,$2,$3,$4,$5,$6) \
             RETURNING id, company_id, actor_type, actor_id, action, entity_type, entity_id, \
                       agent_id, run_id, responsible_user_id, details, created_at"
        ).bind(c).bind(actor_type).bind(actor_id).bind(action).bind(entity_type).bind(entity_id)
         .fetch_one(self.db.pool()).await
    }
}
