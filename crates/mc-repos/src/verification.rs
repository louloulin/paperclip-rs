//! Verification code 仓储层。

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use mc_core::id::Id;

use super::memory::MemoryStore;
use super::{RepoError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationPurpose {
    EmailVerification,
    PasswordReset,
    TwoFactor,
    WorkspaceInvite,
}

impl VerificationPurpose {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EmailVerification => "email_verification",
            Self::PasswordReset => "password_reset",
            Self::TwoFactor => "two_factor",
            Self::WorkspaceInvite => "workspace_invite",
        }
    }

    pub fn from_db(s: &str) -> Option<Self> {
        match s {
            "email_verification" => Some(Self::EmailVerification),
            "password_reset" => Some(Self::PasswordReset),
            "two_factor" => Some(Self::TwoFactor),
            "workspace_invite" => Some(Self::WorkspaceInvite),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationRow {
    pub id: Id,
    pub user_id: Option<Id>,
    pub email: Option<String>,
    pub purpose: VerificationPurpose,
    pub code_hash: String,
    pub attempts: u32,
    pub expires_at: DateTime<Utc>,
    pub consumed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl VerificationRow {
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.expires_at <= now
    }
    pub fn is_consumed(&self) -> bool {
        self.consumed_at.is_some()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewVerificationCode {
    pub id: Option<Id>,
    pub user_id: Option<Id>,
    pub email: Option<String>,
    pub purpose: VerificationPurpose,
    pub code_hash: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default)]
pub struct VerificationCodeFilter {
    pub email: Option<String>,
    pub user_id: Option<Id>,
    pub purpose: Option<VerificationPurpose>,
    pub active_only: bool,
    pub limit: Option<u32>,
}

#[async_trait]
pub trait VerificationCodeRepo: Send + Sync {
    async fn create(&self, item: NewVerificationCode) -> Result<VerificationRow>;
    async fn get(&self, id: Id) -> Result<VerificationRow>;
    async fn find_active(&self, email: &str, purpose: VerificationPurpose) -> Result<Option<VerificationRow>>;
    async fn consume(&self, id: Id) -> Result<VerificationRow>;
    async fn increment_attempts(&self, id: Id) -> Result<VerificationRow>;
    async fn list(&self, filter: VerificationCodeFilter) -> Result<Vec<VerificationRow>>;
}

// =========================================================================
// Memory
// =========================================================================

#[derive(Clone)]
pub struct MemoryVerificationCodeRepo {
    store: MemoryStore,
}

impl MemoryVerificationCodeRepo {
    pub fn new(store: MemoryStore) -> Self {
        Self { store }
    }

    pub fn store(&self) -> &MemoryStore {
        &self.store
    }
}

#[async_trait]
impl VerificationCodeRepo for MemoryVerificationCodeRepo {
    async fn create(&self, item: NewVerificationCode) -> Result<VerificationRow> {
        let row = VerificationRow {
            id: item.id.unwrap_or_else(Id::new),
            user_id: item.user_id,
            email: item.email,
            purpose: item.purpose,
            code_hash: item.code_hash,
            attempts: 0,
            expires_at: item.expires_at,
            consumed_at: None,
            created_at: Utc::now(),
        };
        let mut codes = self.store.verification_codes.write().await;
        codes.insert(row.id, row.clone());
        Ok(row)
    }

    async fn get(&self, id: Id) -> Result<VerificationRow> {
        let codes = self.store.verification_codes.read().await;
        codes.get(&id).cloned().ok_or(RepoError::NotFound)
    }

    async fn find_active(
        &self,
        email: &str,
        purpose: VerificationPurpose,
    ) -> Result<Option<VerificationRow>> {
        let codes = self.store.verification_codes.read().await;
        let now = Utc::now();
        let mut out: Vec<VerificationRow> = codes
            .values()
            .filter(|c| {
                c.purpose == purpose
                    && c.email.as_deref() == Some(email)
                    && c.consumed_at.is_none()
                    && c.expires_at > now
            })
            .cloned()
            .collect();
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(out.into_iter().next())
    }

    async fn consume(&self, id: Id) -> Result<VerificationRow> {
        let mut codes = self.store.verification_codes.write().await;
        let row = codes.get_mut(&id).ok_or(RepoError::NotFound)?;
        if row.consumed_at.is_some() {
            return Err(RepoError::Invalid("code already consumed".into()));
        }
        if row.expires_at <= Utc::now() {
            return Err(RepoError::Invalid("code expired".into()));
        }
        row.consumed_at = Some(Utc::now());
        Ok(row.clone())
    }

    async fn increment_attempts(&self, id: Id) -> Result<VerificationRow> {
        let mut codes = self.store.verification_codes.write().await;
        let row = codes.get_mut(&id).ok_or(RepoError::NotFound)?;
        row.attempts = row.attempts.saturating_add(1);
        Ok(row.clone())
    }

    async fn list(&self, filter: VerificationCodeFilter) -> Result<Vec<VerificationRow>> {
        let codes = self.store.verification_codes.read().await;
        let now = Utc::now();
        let mut out: Vec<VerificationRow> = codes
            .values()
            .filter(|c| {
                filter.email.as_deref().map_or(true, |e| c.email.as_deref() == Some(e))
                    && filter.user_id.map_or(true, |u| c.user_id == Some(u))
                    && filter.purpose.map_or(true, |p| c.purpose == p)
                    && (!filter.active_only || (c.consumed_at.is_none() && c.expires_at > now))
            })
            .cloned()
            .collect();
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if let Some(limit) = filter.limit {
            out.truncate(limit as usize);
        }
        Ok(out)
    }
}

// =========================================================================
// Postgres
// =========================================================================

#[derive(Clone)]
pub struct PgVerificationCodeRepo {
    pool: sqlx::PgPool,
}

impl PgVerificationCodeRepo {
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl VerificationCodeRepo for PgVerificationCodeRepo {
    async fn create(&self, item: NewVerificationCode) -> Result<VerificationRow> {
        let row: VerificationRow = sqlx::query_as(
            r#"
            INSERT INTO verification_code
                (id, user_id, email, purpose, code_hash, expires_at)
            VALUES (
                COALESCE($1, gen_random_uuid()),
                $2, $3, $4, $5, $6
            )
            RETURNING id, user_id, email, purpose, code_hash, attempts,
                      expires_at, consumed_at, created_at
            "#,
        )
        .bind(item.id)
        .bind(item.user_id)
        .bind(item.email.as_deref())
        .bind(item.purpose.as_str())
        .bind(&item.code_hash)
        .bind(item.expires_at)
        .fetch_one(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(row)
    }

    async fn get(&self, id: Id) -> Result<VerificationRow> {
        let row: VerificationRow = sqlx::query_as(
            r#"
            SELECT id, user_id, email, purpose, code_hash, attempts,
                   expires_at, consumed_at, created_at
              FROM verification_code WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(row)
    }

    async fn find_active(
        &self,
        email: &str,
        purpose: VerificationPurpose,
    ) -> Result<Option<VerificationRow>> {
        let row: Option<VerificationRow> = sqlx::query_as(
            r#"
            SELECT id, user_id, email, purpose, code_hash, attempts,
                   expires_at, consumed_at, created_at
              FROM verification_code
             WHERE email = $1
               AND purpose = $2
               AND consumed_at IS NULL
               AND expires_at > now()
             ORDER BY created_at DESC
             LIMIT 1
            "#,
        )
        .bind(email)
        .bind(purpose.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(row)
    }

    async fn consume(&self, id: Id) -> Result<VerificationRow> {
        let mut tx = self.pool.begin().await.map_err(RepoError::from)?;
        let current: VerificationRow = sqlx::query_as(
            r#"
            SELECT id, user_id, email, purpose, code_hash, attempts,
                   expires_at, consumed_at, created_at
              FROM verification_code WHERE id = $1 FOR UPDATE
            "#,
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        if current.consumed_at.is_some() {
            return Err(RepoError::Invalid("code already consumed".into()));
        }
        if current.expires_at <= Utc::now() {
            return Err(RepoError::Invalid("code expired".into()));
        }
        let updated: VerificationRow = sqlx::query_as(
            r#"
            UPDATE verification_code SET consumed_at = now()
             WHERE id = $1
            RETURNING id, user_id, email, purpose, code_hash, attempts,
                      expires_at, consumed_at, created_at
            "#,
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        tx.commit().await.map_err(RepoError::from)?;
        Ok(updated)
    }

    async fn increment_attempts(&self, id: Id) -> Result<VerificationRow> {
        let updated: VerificationRow = sqlx::query_as(
            r#"
            UPDATE verification_code SET attempts = attempts + 1
             WHERE id = $1
            RETURNING id, user_id, email, purpose, code_hash, attempts,
                      expires_at, consumed_at, created_at
            "#,
        )
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(updated)
    }

    async fn list(&self, filter: VerificationCodeFilter) -> Result<Vec<VerificationRow>> {
        let limit = filter.limit.unwrap_or(50).min(500) as i64;
        let purpose_str = filter.purpose.map(|p| p.as_str().to_string());
        let rows: Vec<VerificationRow> = sqlx::query_as(
            r#"
            SELECT id, user_id, email, purpose, code_hash, attempts,
                   expires_at, consumed_at, created_at
              FROM verification_code
             WHERE ($1::text IS NULL OR email = $1)
               AND ($2::uuid IS NULL OR user_id = $2)
               AND ($3::text IS NULL OR purpose = $3)
               AND ($4::boolean = FALSE OR (consumed_at IS NULL AND expires_at > now()))
             ORDER BY created_at DESC
             LIMIT $5
            "#,
        )
        .bind(filter.email)
        .bind(filter.user_id)
        .bind(purpose_str)
        .bind(filter.active_only)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(rows)
    }
}

fn map_sqlx_error(err: sqlx::Error) -> RepoError {
    RepoError::from(err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use uuid::Uuid;

    fn new_code(email: &str) -> NewVerificationCode {
        NewVerificationCode {
            id: None,
            user_id: Some(Id::from(Uuid::nil())),
            email: Some(email.into()),
            purpose: VerificationPurpose::EmailVerification,
            code_hash: format!("hash-{}", Uuid::new_v4()),
            expires_at: Utc::now() + Duration::minutes(10),
        }
    }

    #[tokio::test]
    async fn memory_lifecycle() {
        let store = MemoryStore::default();
        let repo = MemoryVerificationCodeRepo::new(store);
        let row = repo.create(new_code("alice@example.com")).await.unwrap();
        let found = repo
            .find_active("alice@example.com", VerificationPurpose::EmailVerification)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.id, row.id);
        let bumped = repo.increment_attempts(row.id).await.unwrap();
        assert_eq!(bumped.attempts, 1);
        let consumed = repo.consume(row.id).await.unwrap();
        assert!(consumed.consumed_at.is_some());
        // Re-consuming returns Err.
        let err = repo.consume(row.id).await.unwrap_err();
        assert!(matches!(err, RepoError::Invalid(_)));
    }
}
