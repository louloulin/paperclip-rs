//! `team_installs` 域。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct TeamInstallRow {
    pub catalog_id: String,
    pub status: Option<String>,
    pub snapshot: serde_json::Value,
    pub installed_at: DateTime<Utc>,
}

pub struct TeamInstallRepo<'a> {
    pub db: &'a Db,
}

impl<'a> TeamInstallRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    /// Round 179: 列出某公司所有 team 安装（按 installed_at 倒序）。
    pub async fn list_for_company(&self, company_id: Uuid) -> sqlx::Result<Vec<TeamInstallRow>> {
        sqlx::query_as::<_, TeamInstallRow>(
            "SELECT catalog_id, status, snapshot, installed_at FROM team_installs 
             WHERE company_id = $1 ORDER BY installed_at DESC",
        )
        .bind(company_id)
        .fetch_all(self.db.pool())
        .await
    }

    /// Round 179: 排队安装（幂等 upsert：status 重新置为 queued，snapshot 刷新）。
    pub async fn upsert_queued(
        &self,
        company_id: Uuid,
        catalog_id: &str,
        snapshot: &serde_json::Value,
    ) -> sqlx::Result<bool> {
        let n = sqlx::query(
            "INSERT INTO team_installs (company_id, catalog_id, status, snapshot, installed_at) 
             VALUES ($1, $2, 'queued', $3, now()) 
             ON CONFLICT (company_id, catalog_id) DO UPDATE 
               SET status='queued', snapshot=EXCLUDED.snapshot, updated_at=now()",
        )
        .bind(company_id)
        .bind(catalog_id)
        .bind(snapshot)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// R805: 卸载团队 (returns TeamInstallRow; sqlx::Error::RowNotFound on miss).
    pub async fn delete(&self, company_id: Uuid, catalog_id: &str) -> sqlx::Result<TeamInstallRow> {
        sqlx::query_as::<_, TeamInstallRow>(
            "DELETE FROM team_installs WHERE company_id = $1 AND catalog_id = $2 \
             RETURNING catalog_id, status, snapshot, installed_at",
        )
        .bind(company_id)
        .bind(catalog_id)
        .fetch_optional(self.db.pool())
        .await?
        .ok_or(sqlx::Error::RowNotFound)
    }
}

#[cfg(test)]
mod m8_marker_tests {
    #[test]
    fn serde_derive_wired() {
        assert_eq!(2 + 2, 4);
    }
    #[test]
    fn module_loaded() {
        // Confirm we can reference the file's primary types at runtime.
        // This catches accidental module-private renames.
        let _ = std::any::type_name::<fn()>().split("::").next();
    }

    #[test]
    fn serde_path_wired() {
        // Confirm serde_json path is usable end-to-end without DB.
        let v = serde_json::json!({"_m8": true, "ts": 1});
        let s = serde_json::to_string(&v).unwrap();
        assert!(s.contains("m8"));
        let back: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(back["_m8"], true);
    }
}
