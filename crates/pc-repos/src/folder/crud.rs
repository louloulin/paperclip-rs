//! folder CRUD：create / patch / delete / get / list。
//!
//! 全部 SQL 收口在这里，复杂业务规则（环检测、descendant）放 hierarchy.rs。

use uuid::Uuid;

use crate::folder::slug::is_reserved_root_slug;
use crate::folder::{FolderKind, FolderPatch, FolderRepo, FolderRow, NewFolder, COLS};
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

    pub async fn list_by_kind(
        &self,
        company_id: Uuid,
        kind: FolderKind,
    ) -> RepoResult<Vec<FolderRow>> {
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
        let sql =
            format!("SELECT {COLS} FROM folders WHERE company_id=$1 AND kind=$2 AND system_key=$3");
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
            return Err(RepoError::Invalid(
                "folder name/slug must not be empty".into(),
            ));
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
    pub async fn patch(
        &self,
        company_id: Uuid,
        id: Uuid,
        p: &FolderPatch,
    ) -> RepoResult<Option<FolderRow>> {
        if let Some(Some(new_parent)) = p.parent_id {
            if new_parent == id {
                return Err(RepoError::Invalid("folder cannot be its own parent".into()));
            }
            if self.would_create_cycle(id, new_parent).await? {
                return Err(RepoError::Invalid(
                    "moving folder would create a cycle".into(),
                ));
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

    /// R800: 删除一个 folder (returns FolderRow; RepoError::NotFound on miss).
    pub async fn delete(&self, company_id: Uuid, id: Uuid) -> RepoResult<FolderRow> {
        let has_children: Option<i64> =
            sqlx::query_scalar("SELECT COUNT(*) FROM folders WHERE parent_id=$1")
                .bind(id)
                .fetch_one(self.db.pool())
                .await?;
        if has_children.unwrap_or(0) > 0 {
            return Err(RepoError::Invalid(
                "folder has children; archive or move first".into(),
            ));
        }
        sqlx::query_as::<_, FolderRow>(
            "DELETE FROM folders WHERE company_id=$1 AND id=$2 \
             RETURNING id, company_id, kind, parent_id, name, slug, system_key, color, position, \
                created_at, updated_at",
        )
        .bind(company_id)
        .bind(id)
        .fetch_optional(self.db.pool())
        .await?
        .ok_or_else(|| RepoError::NotFound { entity: "folder", id: id.to_string() })
    }

    pub async fn count_by_kind(&self, company_id: Uuid, kind: FolderKind) -> RepoResult<i64> {
        let n: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM folders WHERE company_id=$1 AND kind=$2")
                .bind(company_id)
                .bind(kind.as_str())
                .fetch_one(self.db.pool())
                .await?;
        Ok(n)
    }

    /// Legacy / 通用 create：kind 用 &str 传入，绕过 FolderKind 枚举限制。
    ///
    /// 对齐 Node `POST /companies/:id/folders` 当 kind="personal" 等非标准值时的行为：
    /// - 不做 reserved slug 校验
    /// - 不写 slug / system_key 字段（保留 schema 默认值）
    /// - 仍需外部 caller 算 next_position
    ///
    /// 替代 routes 中 `create_folder` legacy path 的兜底 SQL。
    pub async fn create_with_kind_str(
        &self,
        company_id: Uuid,
        kind: &str,
        name: &str,
        color: Option<&str>,
        position: i32,
    ) -> RepoResult<FolderRow> {
        if name.trim().is_empty() {
            return Err(RepoError::Invalid("folder name must not be empty".into()));
        }
        let sql = format!(
            "INSERT INTO folders (id, company_id, kind, name, color, position)              VALUES (gen_random_uuid(), $1, $2, $3, $4, $5) RETURNING {COLS}"
        );
        Ok(sqlx::query_as::<_, FolderRow>(&sql)
            .bind(company_id)
            .bind(kind)
            .bind(name.trim())
            .bind(color)
            .bind(position)
            .fetch_one(self.db.pool())
            .await?)
    }

    /// 计算下一个可用 position（兼容任意 kind 字符串）。
    /// 替代 routes 中 legacy kind 的 inline `COALESCE(MAX(position),0)+1`。
    pub async fn next_position_for_kind(&self, company_id: Uuid, kind: &str) -> RepoResult<i32> {
        let n: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(position),0)+1 FROM folders WHERE company_id=$1 AND kind=$2",
        )
        .bind(company_id)
        .bind(kind)
        .fetch_one(self.db.pool())
        .await?;
        Ok(n as i32)
    }

    /// Get-or-create "Personal" folder（kind='personal'）— 对齐 Node `ensureMyFolder`。
    ///
    /// 行为：
    /// 1. SELECT id FROM folders WHERE company_id=$1 AND kind='personal' LIMIT 1
    /// 2. 若存在返回 Some(existing)
    /// 3. 否则 INSERT kind='personal' name='Personal' position=0 并返回 Some(new)
    ///
    /// 注：与 `ensure_container` 区别在于 kind 字符串（'personal' vs 'skill'）以及
    /// 不通过 system_key 标识。
    pub async fn ensure_personal_root(&self, company_id: Uuid) -> RepoResult<(FolderRow, bool)> {
        let existing: Option<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM folders WHERE company_id=$1 AND kind='personal' LIMIT 1",
        )
        .bind(company_id)
        .fetch_optional(self.db.pool())
        .await?;
        if let Some((id,)) = existing {
            let row = self.get(company_id, id).await?.ok_or_else(|| {
                RepoError::Invalid("ensure_personal_root: existing row vanished".into())
            })?;
            return Ok((row, false));
        }
        let sql = format!(
            "INSERT INTO folders (id, company_id, kind, name, position)              VALUES (gen_random_uuid(), $1, 'personal', 'Personal', 0) RETURNING {COLS}"
        );
        let row: FolderRow = sqlx::query_as::<_, FolderRow>(&sql)
            .bind(company_id)
            .fetch_one(self.db.pool())
            .await?;
        Ok((row, true))
    }
}
