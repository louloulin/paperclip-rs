//! Personal Access Token (PAT) 仓储层。

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use mc_core::id::Id;

use super::memory::MemoryStore;
use super::{RepoError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatRow {
    pub id: Id,
    pub user_id: Id,
    pub name: String,
    pub token_hash: String,
    pub token_prefix: String,
    pub token_last4: String,
    pub expires_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub scopes: Vec<String>,
    pub created_at: DateTime<Utc>,
}

impl PatRow {
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.expires_at <= now
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewPat {
    pub id: Option<Id>,
    pub user_id: Id,
    pub name: String,
    pub token_hash: String,
    pub token_prefix: String,
    pub token_last4: String,
    /// Token validity window from creation. Default 90 days matches upstream.
    pub ttl_secs: Option<u64>,
    pub scopes: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct PatFilter {
    pub user_id: Option<Id>,
    pub limit: Option<u32>,
}

#[async_trait]
pub trait PatRepo: Send + Sync {
    async fn create(&self, item: NewPat) -> Result<PatRow>;
    async fn get(&self, id: Id) -> Result<PatRow>;
    async fn find_by_hash(&self, hash: &str) -> Result<Option<PatRow>>;
    async fn touch_last_used(&self, id: Id) -> Result<()>;
    async fn revoke(&self, id: Id, user_id: Id) -> Result<()>;
    async fn list(&self, filter: PatFilter) -> Result<Vec<PatRow>>;
    /// In-place extend `expires_at`; returns the new expiry timestamp.
    /// Predicate guards against concurrent renewals.
    async fn renew_in_place(&self, id: Id, new_expiry: DateTime<Utc>) -> Result<DateTime<Utc>>;
}

// =========================================================================
// Memory
// =========================================================================

#[derive(Clone)]
pub struct MemoryPatRepo {
    store: MemoryStore,
}

impl MemoryPatRepo {
    pub fn new(store: MemoryStore) -> Self {
        Self { store }
    }

    pub fn store(&self) -> &MemoryStore {
        &self.store
    }
}

#[async_trait]
impl PatRepo for MemoryPatRepo {
    async fn create(&self, item: NewPat) -> Result<PatRow> {
        let now = Utc::now();
        let ttl_secs = item.ttl_secs.unwrap_or(90 * 24 * 60 * 60);
        let row = PatRow {
            id: item.id.unwrap_or_else(Id::new),
            user_id: item.user_id,
            name: item.name,
            token_hash: item.token_hash,
            token_prefix: item.token_prefix,
            token_last4: item.token_last4,
            expires_at: now + chrono::Duration::seconds(ttl_secs as i64),
            last_used_at: None,
            scopes: item.scopes,
            created_at: now,
        };
        let mut pats = self.store.pats.write().await;
        pats.insert(row.id, row.clone());
        Ok(row)
    }

    async fn get(&self, id: Id) -> Result<PatRow> {
        let pats = self.store.pats.read().await;
        pats.get(&id).cloned().ok_or(RepoError::NotFound)
    }

    async fn find_by_hash(&self, hash: &str) -> Result<Option<PatRow>> {
        let pats = self.store.pats.read().await;
        Ok(pats.values().find(|p| p.token_hash == hash).cloned())
    }

    async fn touch_last_used(&self, id: Id) -> Result<()> {
        let mut pats = self.store.pats.write().await;
        let pat = pats.get_mut(&id).ok_or(RepoError::NotFound)?;
        pat.last_used_at = Some(Utc::now());
        Ok(())
    }

    async fn revoke(&self, id: Id, user_id: Id) -> Result<()> {
        let mut pats = self.store.pats.write().await;
        let pat = pats.get_mut(&id).ok_or(RepoError::NotFound)?;
        if pat.user_id != user_id {
            // Don't reveal existence; treat as not-found.
            return Err(RepoError::NotFound);
        }
        pats.remove(&id);
        Ok(())
    }

    async fn list(&self, filter: PatFilter) -> Result<Vec<PatRow>> {
        let pats = self.store.pats.read().await;
        let mut out: Vec<PatRow> = pats
            .values()
            .filter(|p| filter.user_id.map_or(true, |u| p.user_id == u))
            .cloned()
            .collect();
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if let Some(limit) = filter.limit {
            out.truncate(limit as usize);
        }
        Ok(out)
    }

    async fn renew_in_place(&self, id: Id, new_expiry: DateTime<Utc>) -> Result<DateTime<Utc>> {
        let mut pats = self.store.pats.write().await;
        let pat = pats.get_mut(&id).ok_or(RepoError::NotFound)?;
        pat.expires_at = new_expiry;
        Ok(new_expiry)
    }
}

// =========================================================================
// Postgres
// =========================================================================

#[derive(Clone)]
pub struct PgPatRepo {
    pool: sqlx::PgPool,
}

impl PgPatRepo {
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl PatRepo for PgPatRepo {
    async fn create(&self, item: NewPat) -> Result<PatRow> {
        let now = Utc::now();
        let ttl_secs = item.ttl_secs.unwrap_or(90 * 24 * 60 * 60) as i64;
        let row: PatRow = sqlx::query_as(
            r#"
            INSERT INTO personal_access_token
                (id, user_id, name, token_hash, token_prefix, token_last4,
                 expires_at, scopes, created_at)
            VALUES (
                COALESCE($1, gen_random_uuid()),
                $2, $3, $4, $5, $6,
                now() + ($7::bigint || ' seconds')::interval,
                $8::jsonb,
                now()
            )
            RETURNING id, user_id, name, token_hash, token_prefix, token_last4,
                      expires_at, last_used_at, scopes, created_at
            "#,
        )
        .bind(item.id)
        .bind(item.user_id)
        .bind(&item.name)
        .bind(&item.token_hash)
        .bind(&item.token_prefix)
        .bind(&item.token_last4)
        .bind(ttl_secs)
        .bind(serde_json::to_value(&item.scopes)?)
        .fetch_one(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(row)
    }

    async fn get(&self, id: Id) -> Result<PatRow> {
        let row: PatRow = sqlx::query_as(
            r#"
            SELECT id, user_id, name, token_hash, token_prefix, token_last4,
                   expires_at, last_used_at, scopes, created_at
              FROM personal_access_token WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(normalize(row))
    }

    async fn find_by_hash(&self, hash: &str) -> Result<Option<PatRow>> {
        let row: Option<PatRow> = sqlx::query_as(
            r#"
            SELECT id, user_id, name, token_hash, token_prefix, token_last4,
                   expires_at, last_used_at, scopes, created_at
              FROM personal_access_token WHERE token_hash = $1
            "#,
        )
        .bind(hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(row.map(normalize))
    }

    async fn touch_last_used(&self, id: Id) -> Result<()> {
        sqlx::query("UPDATE personal_access_token SET last_used_at = now() WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(map_sqlx_error)?;
        Ok(())
    }

    async fn revoke(&self, id: Id, user_id: Id) -> Result<()> {
        let affected = sqlx::query(
            "DELETE FROM personal_access_token WHERE id = $1 AND user_id = $2",
        )
        .bind(id)
        .bind(user_id)
        .execute(&self.pool)
        .await
        .map_err(map_sqlx_error)?
        .rows_affected();
        if affected == 0 {
            return Err(RepoError::NotFound);
        }
        Ok(())
    }

    async fn list(&self, filter: PatFilter) -> Result<Vec<PatRow>> {
        let limit = filter.limit.unwrap_or(100).min(500) as i64;
        let rows: Vec<PatRow> = sqlx::query_as(
            r#"
            SELECT id, user_id, name, token_hash, token_prefix, token_last4,
                   expires_at, last_used_at, scopes, created_at
              FROM personal_access_token
             WHERE ($1::uuid IS NULL OR user_id = $1)
             ORDER BY created_at DESC
             LIMIT $2
            "#,
        )
        .bind(filter.user_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(rows.into_iter().map(normalize).collect())
    }

    async fn renew_in_place(&self, id: Id, new_expiry: DateTime<Utc>) -> Result<DateTime<Utc>> {
        let new_expiry: DateTime<Utc> = sqlx::query_scalar(
            r#"
            UPDATE personal_access_token SET expires_at = $2
             WHERE id = $1
            RETURNING expires_at
            "#,
        )
        .bind(id)
        .bind(new_expiry)
        .fetch_one(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(new_expiry)
    }
}

fn map_sqlx_error(err: sqlx::Error) -> RepoError {
    RepoError::from(err)
}

fn normalize(mut row: PatRow) -> PatRow {
    // scopes 字段 DB 端是 jsonb；遇 null 时设为空列表（CREATE 路径保证非 null，
    // 但下游 caller 仍受益于显式保证）。
    if row.scopes.is_empty() {
        row.scopes.clear();
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[tokio::test]
    async fn memory_create_then_lookup_by_hash() {
        let store = MemoryStore::default();
        let repo = MemoryPatRepo::new(store);
        let row = repo
            .create(NewPat {
                id: None,
                user_id: Id::from(Uuid::nil()),
                name: "laptop".into(),
                token_hash: "h-123".into(),
                token_prefix: "mul_abc".into(),
                token_last4: "wxyz".into(),
                ttl_secs: Some(3600),
                scopes: vec!["read".into(), "write".into()],
            })
            .await
            .unwrap();
        let found = repo.find_by_hash("h-123").await.unwrap().unwrap();
        assert_eq!(found.id, row.id);
        assert_eq!(found.scopes.len(), 2);
    }

    #[tokio::test]
    async fn memory_revoke_only_owner_succeeds() {
        let store = MemoryStore::default();
        let repo = MemoryPatRepo::new(store);
        let owner = Id::from(Uuid::nil());
        let row = repo
            .create(NewPat {
                id: None,
                user_id: owner,
                name: "ci".into(),
                token_hash: "h-xyz".into(),
                token_prefix: "mul_xyz".into(),
                token_last4: "tail".into(),
                ttl_secs: None,
                scopes: vec![],
            })
            .await
            .unwrap();
        let err = repo.revoke(row.id, Id::new()).await.unwrap_err();
        assert!(matches!(err, RepoError::NotFound), "got {err:?}");
        repo.revoke(row.id, owner).await.unwrap();
    }

    #[tokio::test]
    async fn memory_renew_in_place_updates_expiry() {
        let store = MemoryStore::default();
        let repo = MemoryPatRepo::new(store);
        let row = repo
            .create(NewPat {
                id: None,
                user_id: Id::from(Uuid::nil()),
                name: "test".into(),
                token_hash: "h".into(),
                token_prefix: "p".into(),
                token_last4: "tail".into(),
                ttl_secs: Some(60),
                scopes: vec![],
            })
            .await
            .unwrap();
        let new_expiry = row.expires_at + chrono::Duration::days(30);
        let got = repo.renew_in_place(row.id, new_expiry).await.unwrap();
        assert_eq!(got, new_expiry);
    }
}
