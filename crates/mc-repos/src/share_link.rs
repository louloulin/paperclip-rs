//! Workspace share link 仓储层。

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use mc_core::id::Id;
use mc_core::workspace::WorkspaceRole;

use super::memory::MemoryStore;
use super::{RepoError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareLinkRow {
    pub id: Id,
    pub workspace_id: Id,
    pub code: String,
    pub created_by: Id,
    pub role: WorkspaceRole,
    pub expires_at: Option<DateTime<Utc>>,
    pub max_uses: Option<u32>,
    pub use_count: u32,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
}

impl ShareLinkRow {
    /// Link is currently joinable (active, not over-use, not past expiry).
    pub fn is_usable(&self, now: DateTime<Utc>) -> bool {
        self.is_active
            && self.expires_at.map_or(true, |e| e > now)
            && self.max_uses.map_or(true, |m| self.use_count < m)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewShareLink {
    pub id: Option<Id>,
    pub workspace_id: Id,
    pub code: String,
    pub created_by: Id,
    pub role: WorkspaceRole,
    pub expires_at: Option<DateTime<Utc>>,
    pub max_uses: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct ShareLinkFilter {
    pub workspace_id: Option<Id>,
    pub code: Option<String>,
    pub active_only: bool,
    pub limit: Option<u32>,
}

#[async_trait]
pub trait ShareLinkRepo: Send + Sync {
    async fn create(&self, item: NewShareLink) -> Result<ShareLinkRow>;
    async fn get(&self, id: Id) -> Result<ShareLinkRow>;
    async fn find_by_code(&self, code: &str) -> Result<Option<ShareLinkRow>>;
    async fn list(&self, filter: ShareLinkFilter) -> Result<Vec<ShareLinkRow>>;
    async fn revoke(&self, id: Id) -> Result<()>;
    async fn increment_use(&self, id: Id) -> Result<ShareLinkRow>;
}

// =========================================================================
// Memory
// =========================================================================

#[derive(Clone)]
pub struct MemoryShareLinkRepo {
    store: MemoryStore,
}

impl MemoryShareLinkRepo {
    pub fn new(store: MemoryStore) -> Self {
        Self { store }
    }

    pub fn store(&self) -> &MemoryStore {
        &self.store
    }
}

#[async_trait]
impl ShareLinkRepo for MemoryShareLinkRepo {
    async fn create(&self, item: NewShareLink) -> Result<ShareLinkRow> {
        {
            let links = self.store.share_links.read().await;
            if links
                .values()
                .any(|l| l.workspace_id == item.workspace_id && l.is_active)
            {
                return Err(RepoError::Conflict(
                    "workspace already has an active share link".into(),
                ));
            }
            if links.values().any(|l| l.code == item.code) {
                return Err(RepoError::Conflict(format!(
                    "share link code {} already exists",
                    item.code
                )));
            }
        }
        let row = ShareLinkRow {
            id: item.id.unwrap_or_else(Id::new),
            workspace_id: item.workspace_id,
            code: item.code,
            created_by: item.created_by,
            role: item.role,
            expires_at: item.expires_at,
            max_uses: item.max_uses,
            use_count: 0,
            is_active: true,
            created_at: Utc::now(),
        };
        let mut links = self.store.share_links.write().await;
        links.insert(row.id, row.clone());
        Ok(row)
    }

    async fn get(&self, id: Id) -> Result<ShareLinkRow> {
        let links = self.store.share_links.read().await;
        links.get(&id).cloned().ok_or(RepoError::NotFound)
    }

    async fn find_by_code(&self, code: &str) -> Result<Option<ShareLinkRow>> {
        let links = self.store.share_links.read().await;
        Ok(links.values().find(|l| l.code == code).cloned())
    }

    async fn list(&self, filter: ShareLinkFilter) -> Result<Vec<ShareLinkRow>> {
        let links = self.store.share_links.read().await;
        let mut out: Vec<ShareLinkRow> = links
            .values()
            .filter(|l| {
                filter.workspace_id.map_or(true, |w| l.workspace_id == w)
                    && filter.code.as_deref().map_or(true, |c| l.code == c)
                    && (!filter.active_only || l.is_active)
            })
            .cloned()
            .collect();
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if let Some(limit) = filter.limit {
            out.truncate(limit as usize);
        }
        Ok(out)
    }

    async fn revoke(&self, id: Id) -> Result<()> {
        let mut links = self.store.share_links.write().await;
        let link = links.get_mut(&id).ok_or(RepoError::NotFound)?;
        link.is_active = false;
        Ok(())
    }

    async fn increment_use(&self, id: Id) -> Result<ShareLinkRow> {
        let mut links = self.store.share_links.write().await;
        let link = links.get_mut(&id).ok_or(RepoError::NotFound)?;
        if !link.is_usable(Utc::now()) {
            return Err(RepoError::Invalid(
                "share link is no longer usable".into(),
            ));
        }
        link.use_count = link.use_count.saturating_add(1);
        Ok(link.clone())
    }
}

// =========================================================================
// Postgres
// =========================================================================

#[derive(Clone)]
pub struct PgShareLinkRepo {
    pool: sqlx::PgPool,
}

impl PgShareLinkRepo {
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl ShareLinkRepo for PgShareLinkRepo {
    async fn create(&self, item: NewShareLink) -> Result<ShareLinkRow> {
        let mut tx = self.pool.begin().await.map_err(RepoError::from)?;
        // Deactivate any prior active link for this workspace first so the
        // partial unique index `idx_share_link_workspace_active` accepts ours.
        sqlx::query(
            "UPDATE workspace_share_link SET is_active = FALSE WHERE workspace_id = $1 AND is_active = TRUE",
        )
        .bind(item.workspace_id)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        let row: ShareLinkRow = sqlx::query_as(
            r#"
            INSERT INTO workspace_share_link
                (id, workspace_id, code, created_by, role, expires_at, max_uses)
            VALUES (
                COALESCE($1, gen_random_uuid()),
                $2, $3, $4, $5, $6, $7
            )
            RETURNING id, workspace_id, code, created_by, role, expires_at,
                      max_uses, use_count, is_active, created_at
            "#,
        )
        .bind(item.id)
        .bind(item.workspace_id)
        .bind(&item.code)
        .bind(item.created_by)
        .bind(item.role.as_str())
        .bind(item.expires_at)
        .bind(item.max_uses.map(|n| n as i32))
        .fetch_one(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        tx.commit().await.map_err(RepoError::from)?;
        Ok(row)
    }

    async fn get(&self, id: Id) -> Result<ShareLinkRow> {
        let row: ShareLinkRow = sqlx::query_as(
            r#"
            SELECT id, workspace_id, code, created_by, role, expires_at,
                   max_uses, use_count, is_active, created_at
              FROM workspace_share_link WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(row)
    }

    async fn find_by_code(&self, code: &str) -> Result<Option<ShareLinkRow>> {
        let row: Option<ShareLinkRow> = sqlx::query_as(
            r#"
            SELECT id, workspace_id, code, created_by, role, expires_at,
                   max_uses, use_count, is_active, created_at
              FROM workspace_share_link WHERE code = $1
            "#,
        )
        .bind(code)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(row)
    }

    async fn list(&self, filter: ShareLinkFilter) -> Result<Vec<ShareLinkRow>> {
        let limit = filter.limit.unwrap_or(100).min(500) as i64;
        let rows: Vec<ShareLinkRow> = sqlx::query_as(
            r#"
            SELECT id, workspace_id, code, created_by, role, expires_at,
                   max_uses, use_count, is_active, created_at
              FROM workspace_share_link
             WHERE ($1::uuid IS NULL OR workspace_id = $1)
               AND ($2::text IS NULL OR code = $2)
               AND ($3::boolean = FALSE OR is_active = TRUE)
             ORDER BY created_at DESC
             LIMIT $4
            "#,
        )
        .bind(filter.workspace_id)
        .bind(filter.code)
        .bind(filter.active_only)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(rows)
    }

    async fn revoke(&self, id: Id) -> Result<()> {
        sqlx::query("UPDATE workspace_share_link SET is_active = FALSE WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(map_sqlx_error)?;
        Ok(())
    }

    async fn increment_use(&self, id: Id) -> Result<ShareLinkRow> {
        let mut tx = self.pool.begin().await.map_err(RepoError::from)?;
        let current: ShareLinkRow = sqlx::query_as(
            r#"
            SELECT id, workspace_id, code, created_by, role, expires_at,
                   max_uses, use_count, is_active, created_at
              FROM workspace_share_link WHERE id = $1 FOR UPDATE
            "#,
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        if !current.is_usable(Utc::now()) {
            return Err(RepoError::Invalid(
                "share link is no longer usable".into(),
            ));
        }
        let updated: ShareLinkRow = sqlx::query_as(
            r#"
            UPDATE workspace_share_link
               SET use_count = use_count + 1
             WHERE id = $1
            RETURNING id, workspace_id, code, created_by, role, expires_at,
                      max_uses, use_count, is_active, created_at
            "#,
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        tx.commit().await.map_err(RepoError::from)?;
        Ok(updated)
    }
}

fn map_sqlx_error(err: sqlx::Error) -> RepoError {
    if let sqlx::Error::Database(db) = &err {
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
    use uuid::Uuid;

    #[tokio::test]
    async fn memory_create_revokes_existing_active_link() {
        let store = MemoryStore::default();
        let repo = MemoryShareLinkRepo::new(store);
        let ws = Id::from(Uuid::new_v4());
        let first = repo
            .create(NewShareLink {
                id: None,
                workspace_id: ws,
                code: "abc".into(),
                created_by: Id::from(Uuid::nil()),
                role: WorkspaceRole::Member,
                expires_at: None,
                max_uses: None,
            })
            .await
            .unwrap();
        // Second active link for the same workspace auto-deactivates the
        // first, so the unique-active partial index never rejects us.
        let second = repo
            .create(NewShareLink {
                id: None,
                workspace_id: ws,
                code: "def".into(),
                created_by: Id::from(Uuid::nil()),
                role: WorkspaceRole::Member,
                expires_at: None,
                max_uses: None,
            })
            .await
            .unwrap();
        assert!(!first.is_active, "first link is now inactive");
        assert!(second.is_active);
    }

    #[tokio::test]
    async fn memory_increment_use_blocks_when_inactive() {
        let store = MemoryStore::default();
        let repo = MemoryShareLinkRepo::new(store);
        let ws = Id::from(Uuid::new_v4());
        let link = repo
            .create(NewShareLink {
                id: None,
                workspace_id: ws,
                code: "share".into(),
                created_by: Id::from(Uuid::nil()),
                role: WorkspaceRole::Member,
                expires_at: None,
                max_uses: Some(2),
            })
            .await
            .unwrap();
        repo.revoke(link.id).await.unwrap();
        let err = repo.increment_use(link.id).await.unwrap_err();
        assert!(matches!(err, RepoError::Invalid(_)));
    }
}
