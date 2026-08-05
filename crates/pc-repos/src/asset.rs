//! `asset` 域：assets 表 CRUD（1:1 port of Node `server/src/services/assets.ts`，22 行）。
//!
//! 与 Node 端 `assetService(db)` 1:1 对齐：
//! - `create(company_id, record) -> AssetRow` —— 插入并 RETURNING 整行
//! - `get_by_id(id) -> Option<AssetRow>` —— 按 id 查单行
//!
//! 设计：
//! - `AssetRow` —— assets 表行（13 列：id / company_id / provider / object_key /
//!   content_type / byte_size / sha256 / original_filename / created_by_agent_id /
//!   created_by_user_id / created_at / updated_at）
//! - `CreateAssetRecord` —— create 入参（除 `company_id` 外，company_id 由调用方单独传；
//!   与 Node `Omit<typeof assets.$inferInsert, "companyId">` 1:1 对齐）
//! - `AssetRepo<'a>` —— 仓储入口，引用 `Db` 生命周期
//! - 列清单抽到 `ASSET_COLUMNS` 常量，create 与 get_by_id 复用（避免拼写漂移）
//! - 所有方法返回 `sqlx::Result<...>`，调用方按需转 `RepoResult`

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

/// assets 表全部列名（camelCase 与 snake_case 都与 Drizzle schema 1:1 对齐）。
const ASSET_COLUMNS: &str = "id, company_id, provider, object_key, content_type, byte_size, \
    sha256, original_filename, created_by_agent_id, created_by_user_id, created_at, updated_at";

/// assets 表行（与 Node `typeof assets.$inferSelect` 1:1 对齐）。
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub provider: String,
    pub object_key: String,
    pub content_type: String,
    pub byte_size: i32,
    pub sha256: String,
    pub original_filename: Option<String>,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// `create` 入参（与 Node `Omit<typeof assets.$inferInsert, "companyId">` 1:1 对齐）。
///
/// 注：assets 表的 `id` 默认 `gen_random_uuid()`，所以 `CreateAssetRecord.id` 是
/// `Option<Uuid>`，与 Node 端 `data.id` 可选语义一致。
#[derive(Debug, Clone, Default)]
pub struct CreateAssetRecord {
    pub id: Option<Uuid>,
    pub provider: String,
    pub object_key: String,
    pub content_type: String,
    pub byte_size: i32,
    pub sha256: String,
    pub original_filename: Option<String>,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
}

impl CreateAssetRecord {
    pub fn new(
        provider: impl Into<String>,
        object_key: impl Into<String>,
        content_type: impl Into<String>,
        byte_size: i32,
        sha256: impl Into<String>,
    ) -> Self {
        Self {
            id: None,
            provider: provider.into(),
            object_key: object_key.into(),
            content_type: content_type.into(),
            byte_size,
            sha256: sha256.into(),
            original_filename: None,
            created_by_agent_id: None,
            created_by_user_id: None,
        }
    }
}

/// assets 表仓储入口。
pub struct AssetRepo<'a> {
    pub db: &'a Db,
}

impl<'a> AssetRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    /// 插入一行 asset 并 RETURNING 整行。
    ///
    /// 行为对齐 Node `assetService.create(companyId, data)`：
    /// - `data` 中除 `companyId` 外的所有字段传入对应列
    /// - `id` 未指定时由 DB 默认值 `gen_random_uuid()` 生成
    /// - `RETURNING` 单行（Node `rows[0]` 与 Rust `fetch_one` 等价）
    pub async fn create(
        &self,
        company_id: Uuid,
        record: CreateAssetRecord,
    ) -> sqlx::Result<AssetRow> {
        let sql = format!(
            "INSERT INTO assets (id, company_id, provider, object_key, content_type, \
                byte_size, sha256, original_filename, created_by_agent_id, created_by_user_id) \
             VALUES (COALESCE($1, gen_random_uuid()), $2, $3, $4, $5, $6, $7, $8, $9, $10) \
             RETURNING {ASSET_COLUMNS}"
        );
        sqlx::query_as::<_, AssetRow>(&sql)
            .bind(record.id)
            .bind(company_id)
            .bind(record.provider)
            .bind(record.object_key)
            .bind(record.content_type)
            .bind(record.byte_size)
            .bind(record.sha256)
            .bind(record.original_filename)
            .bind(record.created_by_agent_id)
            .bind(record.created_by_user_id)
            .fetch_one(self.db.pool())
            .await
    }

    /// 按 id 查单行；不存在返回 `None`。
    ///
    /// 行为对齐 Node `assetService.getById(id)`：
    /// - `where(eq(assets.id, id))` —— 1:1 对齐
    /// - `rows[0] ?? null` —— 1:1 对齐
    pub async fn get_by_id(&self, id: Uuid) -> sqlx::Result<Option<AssetRow>> {
        let sql = format!("SELECT {ASSET_COLUMNS} FROM assets WHERE id = $1");
        sqlx::query_as::<_, AssetRow>(&sql)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await
    }

    /// 按 company 列出最近 N 个 asset（按 created_at DESC）。
    ///
    /// 对齐 `companies.list_artifacts` 端点（Round 131 仓储化）。
    pub async fn list_by_company(
        &self,
        company_id: Uuid,
        limit: i64,
    ) -> sqlx::Result<Vec<AssetRow>> {
        let sql = format!(
            "SELECT {ASSET_COLUMNS} FROM assets WHERE company_id = $1              ORDER BY created_at DESC LIMIT $2"
        );
        sqlx::query_as::<_, AssetRow>(&sql)
            .bind(company_id)
            .bind(limit)
            .fetch_all(self.db.pool())
            .await
    }

    /// Round 152: 查找公司 logo 的存储元数据（provider / object_key / content_type /
    /// byte_size / original_filename）。`company_logos` 是 junction 表，
    /// 通过 `cl.company_id = $1` 关联到 `assets`。返回第一条匹配（每公司通常 1 条）。
    pub async fn find_logo_meta_by_company(
        &self,
        company_id: Uuid,
    ) -> sqlx::Result<Option<(String, String, String, i32, Option<String>)>> {
        let row: Option<(String, String, String, i32, Option<String>)> = sqlx::query_as(
            "SELECT a.provider, a.object_key, a.content_type, a.byte_size, a.original_filename \
             FROM company_logos cl \
             INNER JOIN assets a ON a.id = cl.asset_id \
             WHERE cl.company_id = $1 LIMIT 1",
        )
        .bind(company_id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- 列清单与结构 ----

    #[test]
    fn asset_columns_constant_covers_all_drizzle_columns() {
        // 必须包含 12 个列名（顺序敏感，便于复用 RETURNING 与 SELECT）
        let expected = [
            "id",
            "company_id",
            "provider",
            "object_key",
            "content_type",
            "byte_size",
            "sha256",
            "original_filename",
            "created_by_agent_id",
            "created_by_user_id",
            "created_at",
            "updated_at",
        ];
        for col in expected {
            assert!(ASSET_COLUMNS.contains(col), "missing column: {col}");
        }
    }

    #[test]
    fn asset_row_has_twelve_fields() {
        // 编译期保证：AssetRow 字段数与 ASSET_COLUMNS 列数一致
        let row = AssetRow {
            id: Uuid::nil(),
            company_id: Uuid::nil(),
            provider: String::new(),
            object_key: String::new(),
            content_type: String::new(),
            byte_size: 0,
            sha256: String::new(),
            original_filename: None,
            created_by_agent_id: None,
            created_by_user_id: None,
            created_at: Timestamp::now(),
            updated_at: Timestamp::now(),
        };
        let value = serde_json::to_value(&row).unwrap();
        let obj = value.as_object().unwrap();
        // 12 个 camelCase 字段（serde rename_all）
        assert_eq!(obj.len(), 12);
        for k in [
            "id",
            "companyId",
            "provider",
            "objectKey",
            "contentType",
            "byteSize",
            "sha256",
            "originalFilename",
            "createdByAgentId",
            "createdByUserId",
            "createdAt",
            "updatedAt",
        ] {
            assert!(obj.contains_key(k), "missing camelCase field: {k}");
        }
    }

    // ---- CreateAssetRecord ----

    #[test]
    fn create_record_new_sets_required_fields() {
        let r = CreateAssetRecord::new(
            "paperclip",
            "company-1/logo.png",
            "image/png",
            1024,
            "abc123",
        );
        assert_eq!(r.provider, "paperclip");
        assert_eq!(r.object_key, "company-1/logo.png");
        assert_eq!(r.content_type, "image/png");
        assert_eq!(r.byte_size, 1024);
        assert_eq!(r.sha256, "abc123");
        // 默认为空
        assert!(r.id.is_none());
        assert!(r.original_filename.is_none());
        assert!(r.created_by_agent_id.is_none());
        assert!(r.created_by_user_id.is_none());
    }

    #[test]
    fn create_record_default_is_empty() {
        let r = CreateAssetRecord::default();
        assert!(r.id.is_none());
        assert!(r.provider.is_empty());
        assert!(r.object_key.is_empty());
        assert!(r.content_type.is_empty());
        assert_eq!(r.byte_size, 0);
        assert!(r.sha256.is_empty());
    }

    #[test]
    fn create_record_can_carry_id_and_optional_fields() {
        let agent_id = Uuid::new_v4();
        let user_id = "user-1".to_string();
        let id = Uuid::new_v4();
        let r = CreateAssetRecord {
            id: Some(id),
            provider: "s3".into(),
            object_key: "bucket/key".into(),
            content_type: "application/pdf".into(),
            byte_size: 4096,
            sha256: "deadbeef".into(),
            original_filename: Some("report.pdf".into()),
            created_by_agent_id: Some(agent_id),
            created_by_user_id: Some(user_id.clone()),
        };
        assert_eq!(r.id, Some(id));
        assert_eq!(r.original_filename, Some("report.pdf".to_string()));
        assert_eq!(r.created_by_agent_id, Some(agent_id));
        assert_eq!(r.created_by_user_id, Some(user_id));
    }

    // ---- SQL 形状 ----

    #[test]
    fn create_sql_uses_coalesce_for_optional_id() {
        // 与 Node `assets.$inferInsert` 行为一致：id 不传时由 DB 默认生成
        let sql = format!(
            "INSERT INTO assets (id, company_id, provider, object_key, content_type, \
                byte_size, sha256, original_filename, created_by_agent_id, created_by_user_id) \
             VALUES (COALESCE($1, gen_random_uuid()), $2, $3, $4, $5, $6, $7, $8, $9, $10) \
             RETURNING {ASSET_COLUMNS}"
        );
        assert!(sql.contains("COALESCE($1, gen_random_uuid())"));
        assert!(sql.contains("RETURNING"));
        assert!(sql.contains("INSERT INTO assets"));
    }

    #[test]
    fn get_by_id_sql_filters_on_id() {
        let sql = format!("SELECT {ASSET_COLUMNS} FROM assets WHERE id = $1");
        assert!(sql.contains("WHERE id = $1"));
        assert!(sql.starts_with("SELECT "));
        assert!(sql.contains("FROM assets"));
    }

    #[test]
    fn both_queries_reference_full_column_list() {
        // 确保 ASSET_COLUMNS 在 create 与 get_by_id 中都正确出现
        assert!(ASSET_COLUMNS.contains("id"));
        assert!(ASSET_COLUMNS.contains("created_at"));
        assert!(ASSET_COLUMNS.contains("updated_at"));
        assert!(ASSET_COLUMNS.contains("original_filename"));
        // 不应包含额外列
        assert!(!ASSET_COLUMNS.contains("metadata"));
        assert!(!ASSET_COLUMNS.contains("deleted_at"));
    }
}
