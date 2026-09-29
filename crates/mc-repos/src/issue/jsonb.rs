use mc_core::Id;
use serde_json::Value as JsonValue;

use super::{map_sqlx_err, IssueRepo, RepoError, Result};

impl IssueRepo {
    // ---- metadata / properties（JSONB） -----------------------------------

    /// 读取 `metadata`。
    pub async fn get_metadata(&self, workspace_id: Id, id: Id) -> Result<JsonValue> {
        let row = self.get(workspace_id, id).await?;
        Ok(row.metadata)
    }

    /// 写单个 metadata key，返回写入后的完整 metadata。
    pub async fn set_metadata_key(
        &self,
        workspace_id: Id,
        id: Id,
        key: &str,
        value: &JsonValue,
    ) -> Result<JsonValue> {
        let meta: Option<JsonValue> = sqlx::query_scalar(
            "UPDATE issue SET metadata = jsonb_set(metadata, ARRAY[$3::text], $4::jsonb, true), \
                    revision = revision + 1, updated_at = now(), last_activity_at = now() \
             WHERE workspace_id = $1 AND id = $2 RETURNING metadata",
        )
        .bind(workspace_id.0)
        .bind(id.0)
        .bind(key)
        .bind(value)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        meta.ok_or(RepoError::NotFound)
    }

    /// 删除单个 metadata key，返回删除后的完整 metadata。
    pub async fn delete_metadata_key(
        &self,
        workspace_id: Id,
        id: Id,
        key: &str,
    ) -> Result<JsonValue> {
        let meta: Option<JsonValue> = sqlx::query_scalar(
            "UPDATE issue SET metadata = metadata - $3::text, \
                    revision = revision + 1, updated_at = now(), last_activity_at = now() \
             WHERE workspace_id = $1 AND id = $2 RETURNING metadata",
        )
        .bind(workspace_id.0)
        .bind(id.0)
        .bind(key)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        meta.ok_or(RepoError::NotFound)
    }

    /// 写单个 property，返回写入后的完整 properties。
    pub async fn set_property(
        &self,
        workspace_id: Id,
        id: Id,
        property_id: &str,
        value: &JsonValue,
    ) -> Result<JsonValue> {
        let props: Option<JsonValue> = sqlx::query_scalar(
            "UPDATE issue SET properties = jsonb_set(properties, ARRAY[$3::text], $4::jsonb, true), \
                    revision = revision + 1, updated_at = now(), last_activity_at = now() \
             WHERE workspace_id = $1 AND id = $2 RETURNING properties",
        )
        .bind(workspace_id.0)
        .bind(id.0)
        .bind(property_id)
        .bind(value)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        props.ok_or(RepoError::NotFound)
    }

    /// 删除单个 property，返回删除后的完整 properties。
    pub async fn delete_property(
        &self,
        workspace_id: Id,
        id: Id,
        property_id: &str,
    ) -> Result<JsonValue> {
        let props: Option<JsonValue> = sqlx::query_scalar(
            "UPDATE issue SET properties = properties - $3::text, \
                    revision = revision + 1, updated_at = now(), last_activity_at = now() \
             WHERE workspace_id = $1 AND id = $2 RETURNING properties",
        )
        .bind(workspace_id.0)
        .bind(id.0)
        .bind(property_id)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        props.ok_or(RepoError::NotFound)
    }
}
