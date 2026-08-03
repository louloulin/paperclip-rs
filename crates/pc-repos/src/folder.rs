//! `folders` 域 — 嵌套文件夹容器，支持 routine / skill 两种分类。
//!
//! 设计：
//! - 三层唯一约束：(company, kind, NULL parent, slug) / (company, kind, parent, slug) /
//!   (company, kind, system_key)
//! - 重组树：移动到新父（update parent_id）、批量 re-position（drag/drop）
//! - 软删除通过 `archived_at`（外键 ON DELETE RESTRICT，需要先 archive）

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::{Db, RepoError, RepoResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FolderKind {
    Routine,
    Skill,
}
impl FolderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Routine => "routine",
            Self::Skill => "skill",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "routine" => Some(Self::Routine),
            "skill" => Some(Self::Skill),
            _ => None,
        }
    }
}

const COLS: &str = "id, company_id, kind, parent_id, name, slug, system_key, color,      position, created_at, updated_at";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub kind: String,
    pub parent_id: Option<Uuid>,
    pub name: String,
    pub slug: String,
    pub system_key: Option<String>,
    pub color: Option<String>,
    pub position: i32,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewFolder {
    pub company_id: Uuid,
    pub kind: FolderKind,
    pub parent_id: Option<Uuid>,
    pub name: String,
    pub slug: String,
    pub system_key: Option<String>,
    pub color: Option<String>,
    pub position: i32,
}

#[derive(Debug, Clone, Default)]
pub struct FolderPatch {
    pub name: Option<String>,
    pub slug: Option<String>,
    pub color: Option<String>,
    pub position: Option<i32>,
    pub parent_id: Option<Option<Uuid>>, // double Option: None = 不改, Some(None) = 设顶级, Some(Some(x)) = 设 x
}

pub struct FolderRepo<'a> {
    pub db: &'a Db,
}

impl<'a> FolderRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    pub async fn list_by_company(&self, company_id: Uuid) -> RepoResult<Vec<FolderRow>> {
        let sql = format!(
            "SELECT {COLS} FROM folders              WHERE company_id=$1              ORDER BY kind, COALESCE(parent_id, '00000000-0000-0000-0000-000000000000'), position, name"
        );
        Ok(sqlx::query_as::<_, FolderRow>(&sql)
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn list_by_kind(
        &self,
        company_id: Uuid,
        kind: FolderKind,
    ) -> RepoResult<Vec<FolderRow>> {
        let sql = format!(
            "SELECT {COLS} FROM folders              WHERE company_id=$1 AND kind=$2              ORDER BY COALESCE(parent_id, '00000000-0000-0000-0000-000000000000'), position, name"
        );
        Ok(sqlx::query_as::<_, FolderRow>(&sql)
            .bind(company_id)
            .bind(kind.as_str())
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn get(
        &self,
        company_id: Uuid,
        id: Uuid,
    ) -> RepoResult<Option<FolderRow>> {
        let sql = format!("SELECT {COLS} FROM folders WHERE company_id=$1 AND id=$2");
        Ok(sqlx::query_as::<_, FolderRow>(&sql)
            .bind(company_id)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    pub async fn get_by_system_key(
        &self,
        company_id: Uuid,
        kind: FolderKind,
        system_key: &str,
    ) -> RepoResult<Option<FolderRow>> {
        let sql = format!(
            "SELECT {COLS} FROM folders WHERE company_id=$1 AND kind=$2 AND system_key=$3"
        );
        Ok(sqlx::query_as::<_, FolderRow>(&sql)
            .bind(company_id)
            .bind(kind.as_str())
            .bind(system_key)
            .fetch_optional(self.db.pool())
            .await?)
    }

    pub async fn find_by_slug(
        &self,
        company_id: Uuid,
        kind: FolderKind,
        slug: &str,
    ) -> RepoResult<Option<FolderRow>> {
        let sql = format!(
            "SELECT {COLS} FROM folders WHERE company_id=$1 AND kind=$2 AND slug=$3              AND parent_id IS NULL"
        );
        Ok(sqlx::query_as::<_, FolderRow>(&sql)
            .bind(company_id)
            .bind(kind.as_str())
            .bind(slug)
            .fetch_optional(self.db.pool())
            .await?)
    }

    pub async fn create(&self, f: &NewFolder) -> RepoResult<FolderRow> {
        if f.name.trim().is_empty() || f.slug.trim().is_empty() {
            return Err(RepoError::Invalid("folder name/slug must not be empty".into()));
        }
        let sql = format!(
            "INSERT INTO folders (company_id, kind, parent_id, name, slug, system_key, color, position)              VALUES ($1,$2,$3,$4,$5,$6,$7,$8)              RETURNING {COLS}"
        );
        Ok(sqlx::query_as::<_, FolderRow>(&sql)
            .bind(f.company_id)
            .bind(f.kind.as_str())
            .bind(f.parent_id)
            .bind(&f.name)
            .bind(&f.slug)
            .bind(f.system_key.as_deref())
            .bind(f.color.as_deref())
            .bind(f.position)
            .fetch_one(self.db.pool())
            .await?)
    }

    /// 改字段，且如果请求改 parent_id，做循环检测（不允许把自己或子孙设为父）。
    pub async fn patch(
        &self,
        company_id: Uuid,
        id: Uuid,
        p: &FolderPatch,
    ) -> RepoResult<Option<FolderRow>> {
        if let Some(Some(new_parent)) = p.parent_id {
            if new_parent == id {
                return Err(RepoError::Invalid(
                    "folder cannot be its own parent".into(),
                ));
            }
            if self.would_create_cycle(id, new_parent).await? {
                return Err(RepoError::Invalid(
                    "moving folder would create a cycle".into(),
                ));
            }
        }
        let sql = format!(
            "UPDATE folders SET                 name = COALESCE($2, name),                 slug = COALESCE($3, slug),                 color = COALESCE($4, color),                 position = COALESCE($5, position),                 parent_id = CASE WHEN $6::boolean THEN $7 ELSE parent_id END,                 updated_at = now()              WHERE company_id=$1 AND id=$8              RETURNING {COLS}"
        );
        // The CASE expression: bind $6 as flag, $7 as parent value
        let has_new_parent = p.parent_id.is_some();
        let new_parent_value = p.parent_id.flatten();
        Ok(sqlx::query_as::<_, FolderRow>(&sql)
            .bind(company_id)
            .bind(p.name.as_deref())
            .bind(p.slug.as_deref())
            .bind(p.color.as_deref())
            .bind(p.position)
            .bind(has_new_parent)
            .bind(new_parent_value)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    /// 递归检查 ancestor = 后代=id：把 id 临时移到自己下面是否成环。
    /// 实现：从 id 出发向上回溯若见到 new_parent 即成环。
    async fn would_create_cycle(
        &self,
        id: Uuid,
        new_parent: Uuid,
    ) -> RepoResult<bool> {
        let mut cur: Option<Uuid> = Some(new_parent);
        // 给个深度上限防止恶意数据
        for _ in 0..512 {
            match cur {
                None => return Ok(false),
                Some(p) if p == id => return Ok(true),
                Some(p) => {
                    let n: Option<Uuid> =
                        sqlx::query_scalar("SELECT parent_id FROM folders WHERE id=$1")
                            .bind(p)
                            .fetch_optional(self.db.pool())
                            .await?;
                    cur = n;
                }
            }
        }
        Ok(true) // 太深视为不安全
    }

    pub async fn reorder_siblings(
        &self,
        company_id: Uuid,
        kind: FolderKind,
        parent_id: Option<Uuid>,
        ordered_ids: &[Uuid],
    ) -> RepoResult<()> {
        let mut tx = self.db.pool().begin().await?;
        for (i, fid) in ordered_ids.iter().enumerate() {
            sqlx::query(
                "UPDATE folders SET position=$1, parent_id=$2, updated_at=now()                  WHERE company_id=$3 AND kind=$4 AND id=$5",
            )
            .bind(i as i32)
            .bind(parent_id)
            .bind(company_id)
            .bind(kind.as_str())
            .bind(fid)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Back-compat shim: simpler create signature.
    #[allow(dead_code)]
    pub async fn create_legacy(
        &self,
        company_id: Uuid,
        kind: &str,
        name: &str,
        slug: &str,
    ) -> RepoResult<FolderRow> {
        let parsed_kind = FolderKind::parse(kind).unwrap_or(FolderKind::Routine);
        let input = NewFolder {
            company_id,
            kind: parsed_kind,
            parent_id: None,
            name: name.into(),
            slug: slug.into(),
            system_key: None,
            color: None,
            position: 0,
        };
        self.create(&input).await
    }

    /// Back-compat shim: delete by id only.
    #[allow(dead_code)]
    pub async fn delete_legacy(&self, id: Uuid) -> RepoResult<bool> {
        let n = sqlx::query("DELETE FROM folders WHERE id=$1")
            .bind(id)
            .execute(self.db.pool())
            .await?
            .rows_affected();
        Ok(n > 0)
    }

    pub async fn delete(&self, company_id: Uuid, id: Uuid) -> RepoResult<bool> {
        // 受 FK ON DELETE RESTRICT 约束：先看是不是空文件夹
        let has_children: Option<i64> = sqlx::query_scalar(
            "SELECT COUNT(*) FROM folders WHERE parent_id=$1",
        )
        .bind(id)
        .fetch_one(self.db.pool())
        .await?;
        if has_children.unwrap_or(0) > 0 {
            return Err(RepoError::Invalid(
                "folder has children; archive or move first".into(),
            ));
        }
        let n = sqlx::query("DELETE FROM folders WHERE company_id=$1 AND id=$2")
            .bind(company_id)
            .bind(id)
            .execute(self.db.pool())
            .await?
            .rows_affected();
        Ok(n > 0)
    }

    pub async fn count_by_kind(
        &self,
        company_id: Uuid,
        kind: FolderKind,
    ) -> RepoResult<i64> {
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM folders WHERE company_id=$1 AND kind=$2",
        )
        .bind(company_id)
        .bind(kind.as_str())
        .fetch_one(self.db.pool())
        .await?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_kind_strings_round_trip() {
        for k in [FolderKind::Routine, FolderKind::Skill] {
            assert_eq!(FolderKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(FolderKind::parse("nope"), None);
    }

    #[test]
    fn folder_patch_double_option_logic() {
        let p = FolderPatch::default();
        let has_new = p.parent_id.is_some();
        assert!(!has_new);
        let p2 = FolderPatch {
            parent_id: Some(None),
            ..Default::default()
        };
        assert!(p2.parent_id.is_some());
        assert!(p2.parent_id.flatten().is_none());
    }

    #[test]
    fn new_folder_validation() {
        let bad = NewFolder {
            company_id: Uuid::new_v4(),
            kind: FolderKind::Routine,
            parent_id: None,
            name: "".into(),
            slug: "abc".into(),
            system_key: None,
            color: None,
            position: 0,
        };
        assert!(bad.name.trim().is_empty());
    }
}
