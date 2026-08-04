//! folder CRUD：create / patch / delete / get / list。
//!
//! 全部 SQL 收口在这里，复杂业务规则（环检测、descendant）放 hierarchy.rs。

use uuid::Uuid;

use crate::folder::slug::is_reserved_root_slug;
use crate::folder::{COLS, FolderKind, FolderPatch, FolderRepo, FolderRow, NewFolder};
use crate::{Db, RepoError, RepoResult};

impl<'a> FolderRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    pub async fn list_by_company(&self, company_id: Uuid) -> RepoResult<Vec<FolderRow>> {
        let sql = format!(
            "SELECT {COLS} FROM folders WHERE company_id=$1 ORDER BY kind, COALESCE(parent_id, '00000000-0000-0000-0000-000000000000'), position, name"
        );
        Ok(sqlx::query_as::<_, FolderRow>(&sql)
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn list_by_kind(&self, company_id: Uuid, kind: FolderKind) -> RepoResult<Vec<FolderRow>> {
        let sql = format!(
            "SELECT {COLS} FROM folders WHERE company_id=$1 AND kind=$2 ORDER BY COALESCE(parent_id, '00000000-0000-0000-0000-000000000000'), position, name"
        );
        Ok(sqlx::query_as::<_, FolderRow>(&sql)
            .bind(company_id)
            .bind(kind.as_str())
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn get(&self, company_id: Uuid, id: Uuid) -> RepoResult<Option<FolderRow>> {
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
            "SELECT {COLS} FROM folders WHERE company_id=$1 AND kind=$2 AND slug=$3 AND parent_id IS NULL"
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
        if is_reserved_root_slug(f.kind, f.parent_id, &f.slug) {
            return Err(RepoError::Invalid(
                "reserved skill folders are system-managed".into(),
            ));
        }
        let sql = format!(
            "INSERT INTO folders (company_id, kind, parent_id, name, slug, system_key, color, position) VALUES ($1,$2,$3,$4,$5,$6,$7,$8) RETURNING {COLS}"
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

    /// 改字段，且如果请求改 parent_id，做循环检测。
    pub async fn patch(&self, company_id: Uuid, id: Uuid, p: &FolderPatch) -> RepoResult<Option<FolderRow>> {
        if let Some(Some(new_parent)) = p.parent_id {
            if new_parent == id {
                return Err(RepoError::Invalid("folder cannot be its own parent".into()));
            }
            if self.would_create_cycle(id, new_parent).await? {
                return Err(RepoError::Invalid("moving folder would create a cycle".into()));
            }
        }
        let sql = format!(
            "UPDATE folders SET name = COALESCE($2, name), slug = COALESCE($3, slug), color = COALESCE($4, color), position = COALESCE($5, position), parent_id = CASE WHEN $6::boolean THEN $7 ELSE parent_id END, updated_at = now() WHERE company_id=$1 AND id=$8 RETURNING {COLS}"
        );
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

    /// Back-compat shim.
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

    /// Back-compat shim.
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

    pub async fn count_by_kind(&self, company_id: Uuid, kind: FolderKind) -> RepoResult<i64> {
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
