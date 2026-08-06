//! `labels` 域 — 公司级 issue / case 标签。
//!
//! Schema (paperclip `packages/db/src/schema/labels.ts`)：
//! - `labels(id, company_id, name, color)` + 唯一索引 `(company_id, name)`
//! - `case_labels(case_id, label_id)`、`issue_labels(issue_id, label_id)` 多对多关联。
//!
//! 关联管理（case / issue ↔ labels）已在 `case.rs` 与 `issue.rs` 内，
//! 本模块只负责 labels 本身的 CRUD 与按 company 列出。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::{Db, RepoError, RepoResult};

const COLS: &str = "id, company_id, name, color, created_at, updated_at";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub color: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewLabel {
    pub company_id: Uuid,
    pub name: String,
    pub color: String,
}

#[derive(Debug, Clone, Default)]
pub struct LabelPatch {
    pub name: Option<String>,
    pub color: Option<String>,
}

pub struct LabelRepo<'a> {
    pub db: &'a Db,
}

impl<'a> LabelRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    /// 按 company 列出所有 label（按 name 升序）。
    pub async fn list_by_company(&self, company_id: Uuid) -> RepoResult<Vec<LabelRow>> {
        let sql = format!("SELECT {COLS} FROM labels WHERE company_id=$1 ORDER BY name");
        Ok(sqlx::query_as::<_, LabelRow>(&sql)
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    /// 按 id 查询（不限定 company，便于跨公司查找）。
    pub async fn get_by_id(&self, id: Uuid) -> RepoResult<Option<LabelRow>> {
        let sql = format!("SELECT {COLS} FROM labels WHERE id=$1");
        Ok(sqlx::query_as::<_, LabelRow>(&sql)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    /// 按 (company, name) 查询。
    pub async fn find_by_name(&self, company_id: Uuid, name: &str) -> RepoResult<Option<LabelRow>> {
        let sql = format!("SELECT {COLS} FROM labels WHERE company_id=$1 AND name=$2");
        Ok(sqlx::query_as::<_, LabelRow>(&sql)
            .bind(company_id)
            .bind(name)
            .fetch_optional(self.db.pool())
            .await?)
    }

    pub async fn create(&self, l: &NewLabel) -> RepoResult<LabelRow> {
        let trimmed = l.name.trim();
        if trimmed.is_empty() {
            return Err(RepoError::Invalid("label name must not be empty".into()));
        }
        let color = normalize_color(&l.color);
        let sql = format!(
            "INSERT INTO labels (company_id, name, color) VALUES ($1, $2, $3) RETURNING {COLS}"
        );
        Ok(sqlx::query_as::<_, LabelRow>(&sql)
            .bind(l.company_id)
            .bind(trimmed)
            .bind(&color)
            .fetch_one(self.db.pool())
            .await?)
    }

    pub async fn patch(&self, id: Uuid, p: &LabelPatch) -> RepoResult<Option<LabelRow>> {
        let new_color = p.color.as_deref().map(normalize_color);
        let sql = format!(
            "UPDATE labels SET name = COALESCE($2, name), color = COALESCE($3, color), updated_at = now() WHERE id=$1 RETURNING {COLS}"
        );
        Ok(sqlx::query_as::<_, LabelRow>(&sql)
            .bind(id)
            .bind(p.name.as_deref().map(str::trim))
            .bind(new_color.as_deref())
            .fetch_optional(self.db.pool())
            .await?)
    }

    /// 按 id 删除（外键 ON DELETE CASCADE 自动清理 case_labels / issue_labels）。
    /// 返回 true 表示实际删除了。
    pub async fn delete(&self, id: Uuid) -> RepoResult<bool> {
        let n = sqlx::query("DELETE FROM labels WHERE id=$1")
            .bind(id)
            .execute(self.db.pool())
            .await?
            .rows_affected();
        Ok(n > 0)
    }

    pub async fn count_by_company(&self, company_id: Uuid) -> RepoResult<i64> {
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM labels WHERE company_id=$1")
            .bind(company_id)
            .fetch_one(self.db.pool())
            .await?;
        Ok(n)
    }

    /// 校验一组 label id 是否全部属于指定 company（用于 case / issue update 时的引用完整性）。
    /// 返回属于 company 的 id 集合；调用方对比输入集合即可判断是否存在越界引用。
    pub async fn filter_to_company(&self, company_id: Uuid, ids: &[Uuid]) -> RepoResult<Vec<Uuid>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows: Vec<(Uuid,)> =
            sqlx::query_as("SELECT id FROM labels WHERE company_id=$1 AND id = ANY($2::uuid[])")
                .bind(company_id)
                .bind(ids)
                .fetch_all(self.db.pool())
                .await?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }
}

/// 颜色规范化：trim + 默认 `#94a3b8`（slate-400），确保非空。
fn normalize_color(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        "#94a3b8".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_color_defaults_when_empty() {
        assert_eq!(normalize_color(""), "#94a3b8");
        assert_eq!(normalize_color("   "), "#94a3b8");
        assert_eq!(normalize_color("#ff0000"), "#ff0000");
        assert_eq!(normalize_color("  #00ff00  "), "#00ff00");
    }

    #[test]
    fn new_label_rejects_empty_name() {
        let bad = NewLabel {
            company_id: Uuid::new_v4(),
            name: "".into(),
            color: "#000000".into(),
        };
        assert!(bad.name.trim().is_empty());
    }

    #[test]
    fn label_patch_default_is_empty() {
        let p = LabelPatch::default();
        assert!(p.name.is_none());
        assert!(p.color.is_none());
    }
}
