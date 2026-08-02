//! `inbox_dismissals` 域。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq, Eq)]
pub struct InboxDismissalRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub user_id: String,
    pub item_key: String,
    pub kind: String,
    pub dismissed_at: Timestamp,
    pub snoozed_until: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

pub struct InboxRepo<'a> {
    pub db: &'a Db,
}

impl<'a> InboxRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    pub async fn list_for_user(
        &self,
        company_id: Uuid,
        user_id: &str,
    ) -> sqlx::Result<Vec<InboxDismissalRow>> {
        sqlx::query_as(
            "SELECT id, company_id, user_id, item_key, kind, dismissed_at, snoozed_until, created_at, updated_at \
             FROM inbox_dismissals WHERE company_id=$1 AND user_id=$2 ORDER BY updated_at DESC",
        )
        .bind(company_id)
        .bind(user_id)
        .fetch_all(self.db.pool())
        .await
    }

    pub async fn upsert(
        &self,
        company_id: Uuid,
        user_id: &str,
        item_key: &str,
        kind: &str,
        snoozed_until: Option<Timestamp>,
    ) -> sqlx::Result<InboxDismissalRow> {
        sqlx::query_as(
            "INSERT INTO inbox_dismissals (company_id,user_id,item_key,kind,snoozed_until) \
             VALUES ($1,$2,$3,$4,$5) ON CONFLICT (company_id,user_id,item_key) DO UPDATE SET \
             kind=EXCLUDED.kind, dismissed_at=now(), snoozed_until=EXCLUDED.snoozed_until, updated_at=now() \
             RETURNING id, company_id, user_id, item_key, kind, dismissed_at, snoozed_until, created_at, updated_at",
        )
        .bind(company_id)
        .bind(user_id)
        .bind(item_key)
        .bind(kind)
        .bind(snoozed_until)
        .fetch_one(self.db.pool())
        .await
    }

    pub async fn restore(
        &self,
        company_id: Uuid,
        user_id: &str,
        item_key: &str,
    ) -> sqlx::Result<bool> {
        Ok(sqlx::query(
            "DELETE FROM inbox_dismissals WHERE company_id=$1 AND user_id=$2 AND item_key=$3",
        )
        .bind(company_id)
        .bind(user_id)
        .bind(item_key)
        .execute(self.db.pool())
        .await?
        .rows_affected()
            > 0)
    }
}
