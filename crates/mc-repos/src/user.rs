//! User 仓储层：增删改查 + 邮箱查找。

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use mc_core::id::Id;
use mc_core::timestamp::Timestamp;

use super::memory::MemoryStore;
use super::{RepoError, Result};

/// 与数据库行 1:1 对应的领域行。`created_at` 用 `DateTime<Utc>` 是因为
/// sqlx 的 PG 实现返回原生 TIMESTAMPTZ；内存版则可与 `Timestamp` 互转。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserRow {
    pub id: Id,
    pub name: String,
    pub email: String,
    pub avatar_url: Option<String>,
    pub email_verified_at: Option<DateTime<Utc>>,
    pub language: Option<String>,
    pub timezone: Option<String>,
    pub profile_description: Option<String>,
    pub onboarded_at: Option<DateTime<Utc>>,
    pub onboarding_state: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl UserRow {
    pub fn into_domain(self) -> mc_core::user::User {
        mc_core::user::User {
            id: self.id,
            name: self.name,
            email: self.email,
            avatar_url: self.avatar_url,
            email_verified_at: self.email_verified_at.map(Timestamp::from),
            created_at: Timestamp::from(self.created_at),
            updated_at: Timestamp::from(self.updated_at),
            onboarded_at: self.onboarded_at.map(Timestamp::from),
            onboarding_state: self.onboarding_state,
            language: self.language,
            timezone: self.timezone,
            profile_description: self.profile_description,
        }
    }
}

/// 创建 User 的入参。`id` 不传时由 DB 默认生成。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewUser {
    pub id: Option<Id>,
    pub name: String,
    pub email: String,
    pub avatar_url: Option<String>,
    pub language: Option<String>,
    pub timezone: Option<String>,
    pub profile_description: Option<String>,
}

impl NewUser {
    pub fn new(name: impl Into<String>, email: impl Into<String>) -> Self {
        Self {
            id: None,
            name: name.into(),
            email: email.into(),
            avatar_url: None,
            language: None,
            timezone: None,
            profile_description: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserUpdate {
    pub name: Option<String>,
    pub avatar_url: Option<Option<String>>,
    pub language: Option<Option<String>>,
    pub timezone: Option<Option<String>>,
    pub profile_description: Option<Option<String>>,
    pub email_verified_at: Option<DateTime<Utc>>,
    pub onboarded_at: Option<DateTime<Utc>>,
    pub onboarding_state: Option<Option<serde_json::Value>>,
}

#[derive(Debug, Clone, Default)]
pub struct UserFilter {
    pub email: Option<String>,
    pub email_prefix: Option<String>,
    pub limit: Option<u32>,
}

#[async_trait]
pub trait UserRepo: Send + Sync {
    async fn create(&self, item: NewUser) -> Result<UserRow>;
    async fn get(&self, id: Id) -> Result<UserRow>;
    async fn find_by_email(&self, email: &str) -> Result<Option<UserRow>>;
    async fn update(&self, id: Id, patch: UserUpdate) -> Result<UserRow>;
    async fn delete(&self, id: Id) -> Result<()>;
    async fn list(&self, filter: UserFilter) -> Result<Vec<UserRow>>;
}

// =========================================================================
// In-memory implementation
// =========================================================================

#[derive(Clone)]
pub struct MemoryUserRepo {
    store: MemoryStore,
}

impl MemoryUserRepo {
    pub fn new(store: MemoryStore) -> Self {
        Self { store }
    }

    pub fn store(&self) -> &MemoryStore {
        &self.store
    }
}

#[async_trait]
impl UserRepo for MemoryUserRepo {
    async fn create(&self, item: NewUser) -> Result<UserRow> {
        if item.email.is_empty() {
            return Err(RepoError::Invalid("email is required".into()));
        }
        let now = Utc::now();
        let id = item.id.unwrap_or_else(Id::new);
        let row = UserRow {
            id,
            name: item.name,
            email: item.email,
            avatar_url: item.avatar_url,
            email_verified_at: None,
            language: item.language,
            timezone: item.timezone,
            profile_description: item.profile_description,
            onboarded_at: None,
            onboarding_state: None,
            created_at: now,
            updated_at: now,
        };
        {
            let users = self.store.users.read().await;
            if users.values().any(|u| u.email == row.email) {
                return Err(RepoError::Conflict(format!(
                    "user with email {} already exists",
                    row.email
                )));
            }
        }
        let mut users = self.store.users.write().await;
        users.insert(row.id, row.clone());
        Ok(row)
    }

    async fn get(&self, id: Id) -> Result<UserRow> {
        let users = self.store.users.read().await;
        users.get(&id).cloned().ok_or(RepoError::NotFound)
    }

    async fn find_by_email(&self, email: &str) -> Result<Option<UserRow>> {
        let users = self.store.users.read().await;
        Ok(users.values().find(|u| u.email == email).cloned())
    }

    async fn update(&self, id: Id, patch: UserUpdate) -> Result<UserRow> {
        let mut users = self.store.users.write().await;
        let row = users.get_mut(&id).ok_or(RepoError::NotFound)?;
        if let Some(name) = patch.name {
            row.name = name;
        }
        if let Some(avatar) = patch.avatar_url {
            row.avatar_url = avatar;
        }
        if let Some(lang) = patch.language {
            row.language = lang;
        }
        if let Some(tz) = patch.timezone {
            row.timezone = tz;
        }
        if let Some(desc) = patch.profile_description {
            row.profile_description = desc;
        }
        if let Some(verified_at) = patch.email_verified_at {
            row.email_verified_at = Some(verified_at);
        }
        if let Some(onboarded) = patch.onboarded_at {
            row.onboarded_at = Some(onboarded);
        }
        if let Some(state) = patch.onboarding_state {
            row.onboarding_state = state;
        }
        row.updated_at = Utc::now();
        Ok(row.clone())
    }

    async fn delete(&self, id: Id) -> Result<()> {
        let mut users = self.store.users.write().await;
        users.remove(&id).ok_or(RepoError::NotFound)?;
        Ok(())
    }

    async fn list(&self, filter: UserFilter) -> Result<Vec<UserRow>> {
        let users = self.store.users.read().await;
        let mut out: Vec<UserRow> = users
            .values()
            .filter(|u| {
                filter.email.as_deref().map_or(true, |e| u.email == e)
                    && filter
                        .email_prefix
                        .as_deref()
                        .map_or(true, |p| u.email.starts_with(p))
            })
            .cloned()
            .collect();
        out.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if let Some(limit) = filter.limit {
            out.truncate(limit as usize);
        }
        Ok(out)
    }
}

// =========================================================================
// Postgres implementation (runtime queries — no compile-time DB connection)
// =========================================================================

/// Postgres 实现的 UserRepo。
#[derive(Clone)]
pub struct PgUserRepo {
    pool: sqlx::PgPool,
}

impl PgUserRepo {
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl UserRepo for PgUserRepo {
    async fn create(&self, item: NewUser) -> Result<UserRow> {
        let id = item.id.unwrap_or_else(Id::new);
        let row: UserRow = sqlx::query_as(
            r#"
            INSERT INTO "user" (id, name, email, avatar_url, language, timezone, profile_description)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            RETURNING id,
                      name,
                      email,
                      avatar_url,
                      email_verified_at,
                      language,
                      timezone,
                      profile_description,
                      onboarded_at,
                      onboarding_state,
                      created_at,
                      updated_at
            "#,
        )
        .bind(id)
        .bind(&item.name)
        .bind(&item.email)
        .bind(&item.avatar_url)
        .bind(&item.language)
        .bind(&item.timezone)
        .bind(&item.profile_description)
        .fetch_one(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(row)
    }

    async fn get(&self, id: Id) -> Result<UserRow> {
        let row: UserRow = sqlx::query_as(
            r#"
            SELECT id,
                   name,
                   email,
                   avatar_url,
                   email_verified_at,
                   language,
                   timezone,
                   profile_description,
                   onboarded_at,
                   onboarding_state,
                   created_at,
                   updated_at
              FROM "user"
             WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(row)
    }

    async fn find_by_email(&self, email: &str) -> Result<Option<UserRow>> {
        let row: Option<UserRow> = sqlx::query_as(
            r#"
            SELECT id,
                   name,
                   email,
                   avatar_url,
                   email_verified_at,
                   language,
                   timezone,
                   profile_description,
                   onboarded_at,
                   onboarding_state,
                   created_at,
                   updated_at
              FROM "user"
             WHERE email = $1
            "#,
        )
        .bind(email)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(row)
    }

    async fn update(&self, id: Id, patch: UserUpdate) -> Result<UserRow> {
        // Two-step to keep the SQL small: resolve the current row, apply
        // changes in memory, persist. Cheaper than maintaining a single
        // huge CAS query, and matches the memory implementation's semantics.
        let mut current = self.get(id).await?;
        if let Some(name) = patch.name {
            current.name = name;
        }
        if let Some(avatar) = patch.avatar_url {
            current.avatar_url = avatar;
        }
        if let Some(lang) = patch.language {
            current.language = lang;
        }
        if let Some(tz) = patch.timezone {
            current.timezone = tz;
        }
        if let Some(desc) = patch.profile_description {
            current.profile_description = desc;
        }
        if let Some(verified_at) = patch.email_verified_at {
            current.email_verified_at = Some(verified_at);
        }
        if let Some(onboarded) = patch.onboarded_at {
            current.onboarded_at = Some(onboarded);
        }
        if let Some(state) = patch.onboarding_state {
            current.onboarding_state = state;
        }
        current.updated_at = Utc::now();

        let updated: UserRow = sqlx::query_as(
            r#"
            UPDATE "user" SET
                name = $2,
                avatar_url = $3,
                language = $4,
                timezone = $5,
                profile_description = $6,
                email_verified_at = $7,
                onboarded_at = $8,
                onboarding_state = $9,
                updated_at = now()
            WHERE id = $1
            RETURNING id,
                      name,
                      email,
                      avatar_url,
                      email_verified_at,
                      language,
                      timezone,
                      profile_description,
                      onboarded_at,
                      onboarding_state,
                      created_at,
                      updated_at
            "#,
        )
        .bind(current.id)
        .bind(&current.name)
        .bind(&current.avatar_url)
        .bind(&current.language)
        .bind(&current.timezone)
        .bind(&current.profile_description)
        .bind(current.email_verified_at)
        .bind(current.onboarded_at)
        .bind(&current.onboarding_state)
        .fetch_one(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(updated)
    }

    async fn delete(&self, id: Id) -> Result<()> {
        sqlx::query("DELETE FROM \"user\" WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(map_sqlx_error)?;
        Ok(())
    }

    async fn list(&self, filter: UserFilter) -> Result<Vec<UserRow>> {
        let limit = filter.limit.unwrap_or(50).min(500) as i64;
        let rows: Vec<UserRow> = sqlx::query_as(
            r#"
            SELECT id,
                   name,
                   email,
                   avatar_url,
                   email_verified_at,
                   language,
                   timezone,
                   profile_description,
                   onboarded_at,
                   onboarding_state,
                   created_at,
                   updated_at
              FROM "user"
             WHERE ($1::text IS NULL OR email = $1)
               AND ($2::text IS NULL OR email LIKE $2 || '%')
             ORDER BY created_at ASC
             LIMIT $3
            "#,
        )
        .bind(filter.email)
        .bind(filter.email_prefix)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(rows)
    }
}

fn map_sqlx_error(err: sqlx::Error) -> RepoError {
    if let sqlx::Error::Database(db) = &err {
        // Postgres unique_violation = SQLSTATE 23505
        if db.code().as_deref() == Some("23505") {
            return RepoError::Conflict(format!(
                "unique constraint violated: {}",
                db.constraint().unwrap_or("?")
            ));
        }
    }
    RepoError::from(err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    async fn seed() -> MemoryUserRepo {
        let store = MemoryStore::default();
        let repo = MemoryUserRepo::new(store);
        repo.create(NewUser {
            id: Some(Id::from(Uuid::nil())),
            name: "alice".into(),
            email: "alice@example.com".into(),
            avatar_url: None,
            language: Some("en".into()),
            timezone: Some("UTC".into()),
            profile_description: None,
        })
        .await
        .unwrap();
        repo
    }

    #[tokio::test]
    async fn memory_get_returns_inserted() {
        let repo = seed().await;
        let row = repo.get(Id::from(Uuid::nil())).await.expect("user exists");
        assert_eq!(row.name, "alice");
    }

    #[tokio::test]
    async fn memory_create_rejects_duplicate_email() {
        let repo = seed().await;
        let err = repo
            .create(NewUser::new("Alice 2", "alice@example.com"))
            .await
            .unwrap_err();
        assert!(matches!(err, RepoError::Conflict(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn memory_find_by_email_is_case_sensitive() {
        let repo = seed().await;
        assert!(repo.find_by_email("alice@example.com").await.unwrap().is_some());
        assert!(repo
            .find_by_email("ALICE@example.com")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn memory_update_propagates_optional_fields() {
        let repo = seed().await;
        let id = Id::from(Uuid::nil());
        repo.update(
            id,
            UserUpdate {
                avatar_url: Some(Some("https://avatar".into())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let row = repo.get(id).await.unwrap();
        assert_eq!(row.avatar_url.as_deref(), Some("https://avatar"));
        assert!(row.updated_at.timestamp() >= row.created_at.timestamp());
        let _ = Utc::now();
    }
}
