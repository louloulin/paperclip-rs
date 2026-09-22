//! `UserRepo` — `"user"` 表 CRUD + 关联查询。
//!
//! 字段集比 `mc_core::user::User` 多（`starter_content_state` / `cloud_waitlist_at` /
//! `onboarding_runtime_choice` 等），那些字段在 M2 sub-issue 通过专门 service 暴露。
//! 本 repo 仅读 / 写 `User` 已声明的列。
//!
//! 错误映射：`sqlx::Error::RowNotFound` → `RepoError::NotFound`，
//! `unique` 约束 → `RepoError::Conflict`。

use chrono::{DateTime, Utc};
use mc_core::user::User;
use mc_core::{Id, Timestamp};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use crate::workspace::map_sqlx_err;
use crate::{RepoError, RepoWithDb, Result};

use mc_db::Db;

/// 新增 user 请求（认证流 / admin 创建场景）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewUser {
    pub name: String,
    pub email: String,
    pub avatar_url: Option<String>,
}

/// user patch。所有字段 `None` 表示不动；`timezone: Some("")` 触发清空（哨兵）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserUpdate {
    pub name: Option<String>,
    pub language: Option<String>,
    pub timezone: Option<String>,
    pub profile_description: Option<String>,
}

#[derive(Clone)]
pub struct UserRepo {
    db: Db,
}

impl UserRepo {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

impl RepoWithDb for UserRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

#[derive(Debug, FromRow)]
struct UserRow {
    id: Uuid,
    name: String,
    email: String,
    avatar_url: Option<String>,
    email_verified_at: Option<DateTime<Utc>>,
    language: Option<String>,
    timezone: Option<String>,
    profile_description: Option<String>,
    onboarded_at: Option<DateTime<Utc>>,
    onboarding_state: Option<serde_json::Value>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl TryFrom<UserRow> for User {
    type Error = RepoError;

    fn try_from(row: UserRow) -> Result<Self> {
        Ok(User {
            id: Id::from(row.id),
            name: row.name,
            email: row.email,
            avatar_url: row.avatar_url,
            email_verified_at: row.email_verified_at.map(Timestamp::from),
            language: row.language,
            timezone: row.timezone,
            profile_description: row.profile_description,
            onboarded_at: row.onboarded_at.map(Timestamp::from),
            onboarding_state: row.onboarding_state,
            created_at: Timestamp::from(row.created_at),
            updated_at: Timestamp::from(row.updated_at),
        })
    }
}

#[async_trait::async_trait]
impl crate::Repository<User, NewUser, UserUpdate, UserFilter> for UserRepo
where
    NewUser: Send + Sync,
    UserUpdate: Send + Sync,
{
    async fn create(&self, item: NewUser) -> Result<User> {
        let row = sqlx::query_as::<_, UserRow>(
            "INSERT INTO \"user\" (name, email, avatar_url) \
             VALUES ($1, $2, $3) \
             RETURNING id, name, email, avatar_url, email_verified_at, language, timezone, \
                       profile_description, onboarded_at, onboarding_state, created_at, updated_at",
        )
        .bind(&item.name)
        .bind(&item.email)
        .bind(&item.avatar_url)
        .fetch_one(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        row.try_into()
    }

    async fn get(&self, id: &Id) -> Result<User> {
        let row = sqlx::query_as::<_, UserRow>(
            "SELECT id, name, email, avatar_url, email_verified_at, language, timezone, \
                    profile_description, onboarded_at, onboarding_state, created_at, updated_at \
             FROM \"user\" WHERE id = $1",
        )
        .bind(id.as_uuid())
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?
        .ok_or(RepoError::NotFound)?;
        row.try_into()
    }

    async fn update(&self, id: &Id, patch: UserUpdate) -> Result<User> {
        // timezone 哨兵：NULL → 不改；"" → 清空；其它 → 设置。
        let tz_clear = matches!(patch.timezone.as_deref(), Some(""));
        let tz_set = patch
            .timezone
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let row = sqlx::query_as::<_, UserRow>(
            "UPDATE \"user\" SET \
                name = COALESCE($2, name), \
                language = COALESCE($3, language), \
                profile_description = COALESCE($4, profile_description), \
                timezone = CASE \
                    WHEN $5::boolean THEN NULL \
                    WHEN $6::text IS NOT NULL THEN $6 \
                    ELSE timezone \
                END, \
                updated_at = now() \
             WHERE id = $1 \
             RETURNING id, name, email, avatar_url, email_verified_at, language, timezone, \
                       profile_description, onboarded_at, onboarding_state, created_at, updated_at",
        )
        .bind(id.as_uuid())
        .bind(patch.name.as_deref())
        .bind(patch.language.as_deref())
        .bind(patch.profile_description.as_deref())
        .bind(tz_clear)
        .bind(tz_set)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?
        .ok_or(RepoError::NotFound)?;
        row.try_into()
    }

    async fn delete(&self, id: &Id) -> Result<()> {
        let res = sqlx::query("DELETE FROM \"user\" WHERE id = $1")
            .bind(id.as_uuid())
            .execute(self.db.pool())
            .await
            .map_err(map_sqlx_err)?;
        if res.rows_affected() == 0 {
            Err(RepoError::NotFound)
        } else {
            Ok(())
        }
    }

    async fn list(&self, filter: UserFilter) -> Result<Vec<User>> {
        let limit: i64 = i64::from(filter.limit.unwrap_or(100).min(500));
        let after_id = filter.after_id;
        let rows = sqlx::query_as::<_, UserRow>(
            "SELECT id, name, email, avatar_url, email_verified_at, language, timezone, \
                    profile_description, onboarded_at, onboarding_state, created_at, updated_at \
             FROM \"user\" \
             WHERE ($1::uuid IS NULL OR id > $1) \
             ORDER BY id ASC LIMIT $2",
        )
        .bind(after_id.map(mc_core::Id::as_uuid))
        .bind(limit)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        rows.into_iter().map(TryInto::try_into).collect()
    }
}

/// `list` 过滤条件。
#[derive(Debug, Default, Clone)]
pub struct UserFilter {
    pub after_id: Option<Id>,
    pub limit: Option<u32>,
}

impl UserRepo {
    /// 按 email 查询；找不到返回 `Ok(None)`（不是 `Err(NotFound)`）。
    pub async fn get_by_email(&self, email: &str) -> Result<Option<User>> {
        let row = sqlx::query_as::<_, UserRow>(
            "SELECT id, name, email, avatar_url, email_verified_at, language, timezone, \
                    profile_description, onboarded_at, onboarding_state, created_at, updated_at \
             FROM \"user\" WHERE email = $1",
        )
        .bind(email)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        row.map(TryInto::try_into).transpose()
    }

    /// 认证流需要的幂等 upsert：email 存在则更新 name/avatar；不存在则插入。
    /// 触发 `ON CONFLICT (email)` —— 唯一键。
    pub async fn upsert_by_email(&self, input: NewUser) -> Result<User> {
        let row = sqlx::query_as::<_, UserRow>(
            "INSERT INTO \"user\" (name, email, avatar_url) \
             VALUES ($1, $2, $3) \
             ON CONFLICT (email) DO UPDATE \
             SET name = EXCLUDED.name, \
                 avatar_url = EXCLUDED.avatar_url, \
                 updated_at = now() \
             RETURNING id, name, email, avatar_url, email_verified_at, language, timezone, \
                       profile_description, onboarded_at, onboarding_state, created_at, updated_at",
        )
        .bind(&input.name)
        .bind(&input.email)
        .bind(&input.avatar_url)
        .fetch_one(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        row.try_into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Repository;

    #[test]
    fn filter_default_shape() {
        let f = UserFilter::default();
        assert!(f.after_id.is_none());
        assert!(f.limit.is_none());
    }

    #[test]
    fn new_user_construction() {
        let nu = NewUser {
            name: "alice".into(),
            email: "alice@example.com".into(),
            avatar_url: None,
        };
        assert_eq!(nu.email, "alice@example.com");
    }

    #[test]
    fn user_update_timezone_clear_marker() {
        // "" 字符串应当把 timezone 清除。
        let u = UserUpdate {
            name: None,
            language: None,
            timezone: Some(String::new()),
            profile_description: None,
        };
        assert_eq!(u.timezone.as_deref(), Some(""));
    }

    // ---- DB 集成测试 ----

    #[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
    #[tokio::test]
    async fn db_upsert_by_email_is_idempotent() {
        let url =
            std::env::var("MULTICA_TEST_DATABASE_URL").expect("set MULTICA_TEST_DATABASE_URL");
        let pool = mc_db::pool::Db::connect(&url, 4, 1).await.unwrap();
        let repo = UserRepo::new(pool);

        let email = "test-upsert@example.com";
        let u1 = repo
            .upsert_by_email(NewUser {
                name: "first".into(),
                email: email.into(),
                avatar_url: None,
            })
            .await
            .unwrap();
        let u2 = repo
            .upsert_by_email(NewUser {
                name: "second".into(),
                email: email.into(),
                avatar_url: Some("https://example.com/a.png".into()),
            })
            .await
            .unwrap();
        assert_eq!(u1.id, u2.id, "upsert must return same id for same email");
        assert_eq!(u2.name, "second");
        assert_eq!(u2.avatar_url.as_deref(), Some("https://example.com/a.png"));
        repo.delete(&u1.id).await.ok();
    }

    #[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
    #[tokio::test]
    async fn db_get_by_email_returns_none_on_missing() {
        let url =
            std::env::var("MULTICA_TEST_DATABASE_URL").expect("set MULTICA_TEST_DATABASE_URL");
        let pool = mc_db::pool::Db::connect(&url, 4, 1).await.unwrap();
        let repo = UserRepo::new(pool);
        let none = repo
            .get_by_email("definitely-does-not-exist@example.com")
            .await
            .unwrap();
        assert!(none.is_none());
    }

    #[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
    #[tokio::test]
    async fn db_update_me_partial_fields() {
        let url =
            std::env::var("MULTICA_TEST_DATABASE_URL").expect("set MULTICA_TEST_DATABASE_URL");
        let pool = mc_db::pool::Db::connect(&url, 4, 1).await.unwrap();
        let repo = UserRepo::new(pool);
        let u = repo
            .create(NewUser {
                name: "before".into(),
                email: "test-update-me@example.com".into(),
                avatar_url: None,
            })
            .await
            .unwrap();
        let updated = repo
            .update(
                &u.id,
                UserUpdate {
                    name: Some("after".into()),
                    language: Some("zh-CN".into()),
                    timezone: None,
                    profile_description: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(updated.name, "after");
        assert_eq!(updated.language.as_deref(), Some("zh-CN"));
        repo.delete(&u.id).await.ok();
    }
}
