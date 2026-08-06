//! personal folder：bundled / my / projects 容器 + ensureMyFolder。
//!
//! 对齐 Node `ensureContainer` + `ensureMyFolder`：系统根目录 + per-user `my:{userId}` 子目录。
//! 采用 advisory transaction lock 防止并发竞态创建同名 slug。

use std::collections::HashSet;

use uuid::Uuid;

use crate::folder::slug::{normalize_folder_slug, RESERVED_ROOT_SLUGS};
use crate::folder::{FolderKind, FolderRepo, FolderRow};
use crate::{Db, RepoError, RepoResult};

/// 系统根文件夹保留名。
pub const SYSTEM_KEYS: &[&str] = &["bundled", "my", "projects"];

impl<'a> FolderRepo<'a> {
    /// 锁定公司级 advisory lock，防止并发 mutation。
    pub async fn with_company_lock<F, Fut, T>(&self, company_id: Uuid, f: F) -> RepoResult<T>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = RepoResult<T>>,
    {
        let key = format!("paperclip:folders:{company_id}");
        let mut tx = self.db.pool().begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(&key)
            .execute(&mut *tx)
            .await?;
        let result = f().await?;
        tx.commit().await?;
        Ok(result)
    }

    /// 确保 "bundled" / "my" / "projects" 容器存在并返回其视图。
    /// 若同名 slug 被用户占用，自动 rename 占位者再创建系统根。
    pub async fn ensure_container(
        &self,
        company_id: Uuid,
        slug: &str,
        name: &str,
    ) -> RepoResult<FolderRow> {
        for _attempt in 0..3 {
            if let Some(existing) = self
                .get_by_system_key(company_id, FolderKind::Skill, slug)
                .await?
            {
                return Ok(existing);
            }
            // 若同名根 slug 被用户占用，先 rename 占位者
            let squatted: Option<(Uuid,)> = sqlx::query_as(
                "SELECT id FROM folders WHERE company_id=$1 AND kind='skill' AND parent_id IS NULL AND slug=$2 LIMIT 1",
            )
            .bind(company_id)
            .bind(slug)
            .fetch_optional(self.db.pool())
            .await?;
            if let Some((id,)) = squatted {
                let new_slug = self
                    .unique_sibling_slug(company_id, None, slug, &id.to_string()[..8])
                    .await?;
                sqlx::query("UPDATE folders SET slug=$1, updated_at=now() WHERE id=$2")
                    .bind(&new_slug)
                    .bind(id)
                    .execute(self.db.pool())
                    .await?;
            }
            let position = self
                .next_position(company_id, FolderKind::Skill, None)
                .await?;
            let inserted: Option<(Uuid,)> = sqlx::query_as(
                "INSERT INTO folders (company_id, kind, parent_id, name, slug, system_key, color, position) VALUES ($1,'skill',NULL,$2,$3,$4,NULL,$5) ON CONFLICT DO NOTHING RETURNING id",
            )
            .bind(company_id)
            .bind(name)
            .bind(slug)
            .bind(slug)
            .bind(position)
            .fetch_optional(self.db.pool())
            .await?;
            if let Some((id,)) = inserted {
                return self.get(company_id, id).await?.ok_or_else(|| {
                    RepoError::Invalid("ensure_container: inserted row vanished".into())
                });
            }
        }
        Err(RepoError::Invalid(format!(
            "could not create system folder '{slug}' after retries"
        )))
    }

    /// 在 "my" 容器下确保当前 user 的专属 skill 文件夹存在。
    pub async fn ensure_personal_folder(
        &self,
        company_id: Uuid,
        user_id: &str,
        user_name: Option<&str>,
        requested_slug: Option<&str>,
    ) -> RepoResult<FolderRow> {
        let parent = self.ensure_container(company_id, "my", "My Skills").await?;
        let system_key = format!("my:{user_id}");
        for _attempt in 0..3 {
            // 已存在？
            if let Some(existing) = self
                .get_by_system_key(company_id, FolderKind::Skill, &system_key)
                .await?
            {
                return Ok(existing);
            }
            let base_slug = requested_slug
                .map(str::to_string)
                .unwrap_or_else(|| normalize_folder_slug(user_name.unwrap_or(user_id)));
            let slug = self
                .unique_sibling_slug(company_id, Some(parent.id), &base_slug, user_id)
                .await?;
            let position = self
                .next_position(company_id, FolderKind::Skill, Some(parent.id))
                .await?;
            let inserted: Option<(Uuid,)> = sqlx::query_as(
                "INSERT INTO folders (company_id, kind, parent_id, name, slug, system_key, color, position) VALUES ($1,'skill',$2,$3,$4,$5,NULL,$6) ON CONFLICT DO NOTHING RETURNING id",
            )
            .bind(company_id)
            .bind(parent.id)
            .bind(user_name.unwrap_or("My Skills").trim())
            .bind(&slug)
            .bind(&system_key)
            .bind(position)
            .fetch_optional(self.db.pool())
            .await?;
            if let Some((id,)) = inserted {
                return self.get(company_id, id).await?.ok_or_else(|| {
                    RepoError::Invalid("ensure_personal_folder: inserted row vanished".into())
                });
            }
        }
        Err(RepoError::Invalid(
            "could not create personal folder after retries".into(),
        ))
    }

    /// 在 (company, kind, parent) 下生成一个不冲突的 slug。
    /// base_slug 若已被占，按 `base-{suffix}` / `base-{suffix}-2` 递增。
    pub async fn unique_sibling_slug(
        &self,
        company_id: Uuid,
        parent_id: Option<Uuid>,
        base_slug: &str,
        stable_suffix: &str,
    ) -> RepoResult<String> {
        let sibling_rows: Vec<(String,)> = sqlx::query_as(
            "SELECT slug FROM folders WHERE company_id=$1 AND kind='skill' AND ($2::uuid IS NULL AND parent_id IS NULL OR parent_id = $2)",
        )
        .bind(company_id)
        .bind(parent_id)
        .fetch_all(self.db.pool())
        .await?;
        let siblings: HashSet<String> = sibling_rows.into_iter().map(|(s,)| s).collect();
        if !siblings.contains(base_slug) {
            return Ok(base_slug.to_string());
        }
        let suffix = normalize_folder_slug(stable_suffix);
        let suffix = &suffix[..suffix.len().min(24)];
        let mut candidate = format!("{base_slug}-{suffix}");
        if !siblings.contains(&candidate) {
            return Ok(candidate);
        }
        let mut n = 2;
        loop {
            let c = format!("{base_slug}-{suffix}-{n}");
            if !siblings.contains(&c) {
                return Ok(c);
            }
            n += 1;
            if n > 9999 {
                return Err(RepoError::Invalid(
                    "too many slug collisions while creating folder".into(),
                ));
            }
        }
    }

    /// 列出顶级 skill 文件夹保留名。
    #[allow(dead_code)]
    pub fn reserved_root_slugs() -> &'static [&'static str] {
        RESERVED_ROOT_SLUGS
    }

    /// 列出系统根 system key。
    #[allow(dead_code)]
    pub fn system_keys() -> &'static [&'static str] {
        SYSTEM_KEYS
    }

    #[allow(dead_code)]
    pub fn db_handle(&self) -> &Db {
        self.db
    }
}
