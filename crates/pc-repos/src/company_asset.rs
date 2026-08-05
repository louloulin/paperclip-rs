//! `company_assets` 域。

use uuid::Uuid;

use crate::Db;

pub struct CompanyAssetRepo<'a> {
    pub db: &'a Db,
}

impl<'a> CompanyAssetRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    /// Round 182: 插入一条 company_asset 记录（kind='image'）。
    pub async fn insert_image(
        &self,
        asset_id: Uuid,
        company_id: Uuid,
        key: &str,
        content_type: &str,
        size_bytes: i64,
        sha256: &str,
    ) -> sqlx::Result<()> {
        sqlx::query(
            "INSERT INTO company_assets \\
             (id, company_id, kind, key, content_type, size_bytes, sha256, created_at) \\
             VALUES ($1, $2, 'image', $3, $4, $5, $6, now())",
        )
        .bind(asset_id)
        .bind(company_id)
        .bind(key)
        .bind(content_type)
        .bind(size_bytes)
        .bind(sha256)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// Round 182: 按 id 取 (key, content_type)。
    pub async fn get_content_meta(
        &self,
        asset_id: Uuid,
    ) -> sqlx::Result<Option<(String, String)>> {
        let row: Option<(String, Option<String>)> = sqlx::query_as(
            "SELECT key, content_type FROM company_assets WHERE id = $1",
        )
        .bind(asset_id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row.map(|(k, ct)| (k, ct.unwrap_or_else(|| "application/octet-stream".to_owned()))))
    }
}
