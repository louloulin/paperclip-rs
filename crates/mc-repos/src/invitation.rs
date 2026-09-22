//! Workspace invitation 仓储层。
//!
//! 邀请是发给邮箱（而不是 user_id）的可撤销记录；接受时通过邮箱或
//! 当前 session 的 user_id 找到 pending 行并标记为 accepted。

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use mc_core::id::Id;
use mc_core::workspace::WorkspaceRole;

use super::memory::MemoryStore;
use super::{RepoError, Result};

/// 邀请状态机。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InvitationStatus {
    Pending,
    Accepted,
    Declined,
    Revoked,
    /// 已经过了 expires_at 但还没有显式 declined/accepted 的情况；
    /// 在 list 时通过 expires_at < now() 推断；落库时常以 revoked 为底层值。
    Expired,
}

impl InvitationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Accepted => "accepted",
            Self::Declined => "declined",
            Self::Revoked => "revoked",
            Self::Expired => "expired",
        }
    }

    pub fn from_db(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "accepted" => Some(Self::Accepted),
            "declined" => Some(Self::Declined),
            "revoked" => Some(Self::Revoked),
            "expired" => Some(Self::Expired),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvitationRow {
    pub id: Id,
    pub workspace_id: Id,
    pub email: String,
    pub invitee_user_id: Option<Id>,
    pub invited_by_user_id: Id,
    pub role: WorkspaceRole,
    pub token: String,
    pub status: InvitationStatus,
    pub expires_at: DateTime<Utc>,
    pub accepted_at: Option<DateTime<Utc>>,
    pub declined_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl InvitationRow {
    pub fn is_pending(&self) -> bool {
        self.status == InvitationStatus::Pending && self.expires_at > Utc::now()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewInvitation {
    pub id: Option<Id>,
    pub workspace_id: Id,
    pub email: String,
    pub invitee_user_id: Option<Id>,
    pub invited_by_user_id: Id,
    pub role: WorkspaceRole,
    pub token: String,
    /// 默认邀请有效期 7 天（与 multica upstream 对齐）。
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateInvitationStatus {
    pub status: InvitationStatus,
    pub accepted_user_id: Option<Id>,
}

#[derive(Debug, Clone, Default)]
pub struct InvitationFilter {
    pub workspace_id: Option<Id>,
    pub email: Option<String>,
    pub invitee_user_id: Option<Id>,
    pub status: Option<InvitationStatus>,
    pub limit: Option<u32>,
}

#[async_trait]
pub trait InvitationRepo: Send + Sync {
    async fn create(&self, item: NewInvitation) -> Result<InvitationRow>;
    async fn get(&self, id: Id) -> Result<InvitationRow>;
    async fn find_by_token(&self, token: &str) -> Result<Option<InvitationRow>>;
    async fn list(&self, filter: InvitationFilter) -> Result<Vec<InvitationRow>>;
    async fn update_status(&self, id: Id, patch: UpdateInvitationStatus) -> Result<InvitationRow>;
    async fn revoke(&self, id: Id) -> Result<InvitationRow>;
}

// =========================================================================
// Memory
// =========================================================================

#[derive(Clone)]
pub struct MemoryInvitationRepo {
    store: MemoryStore,
}

impl MemoryInvitationRepo {
    pub fn new(store: MemoryStore) -> Self {
        Self { store }
    }

    pub fn store(&self) -> &MemoryStore {
        &self.store
    }
}

#[async_trait]
impl InvitationRepo for MemoryInvitationRepo {
    async fn create(&self, item: NewInvitation) -> Result<InvitationRow> {
        let now = Utc::now();
        let row = InvitationRow {
            id: item.id.unwrap_or_else(Id::new),
            workspace_id: item.workspace_id,
            email: item.email.clone(),
            invitee_user_id: item.invitee_user_id,
            invited_by_user_id: item.invited_by_user_id,
            role: item.role,
            token: item.token,
            status: InvitationStatus::Pending,
            expires_at: item
                .expires_at
                .unwrap_or_else(|| now + chrono::Duration::days(7)),
            accepted_at: None,
            declined_at: None,
            created_at: now,
            updated_at: now,
        };
        let invitations = self.store.invitations.read().await;
        if invitations.values().any(|i| {
            i.workspace_id == row.workspace_id
                && i.email == row.email
                && i.is_pending()
        }) {
            return Err(RepoError::Conflict(format!(
                "pending invitation already exists for {}",
                row.email
            )));
        }
        drop(invitations);
        let mut invitations = self.store.invitations.write().await;
        invitations.insert(row.id, row.clone());
        Ok(row)
    }

    async fn get(&self, id: Id) -> Result<InvitationRow> {
        let invitations = self.store.invitations.read().await;
        invitations.get(&id).cloned().ok_or(RepoError::NotFound)
    }

    async fn find_by_token(&self, token: &str) -> Result<Option<InvitationRow>> {
        let invitations = self.store.invitations.read().await;
        Ok(invitations.values().find(|i| i.token == token).cloned())
    }

    async fn list(&self, filter: InvitationFilter) -> Result<Vec<InvitationRow>> {
        let invitations = self.store.invitations.read().await;
        let now = Utc::now();
        let mut out: Vec<InvitationRow> = invitations
            .values()
            .map(|i| {
                let mut i = i.clone();
                // Auto-expire in-memory for callers.
                if i.status == InvitationStatus::Pending && i.expires_at <= now {
                    i.status = InvitationStatus::Expired;
                }
                i
            })
            .filter(|i| {
                filter.workspace_id.map_or(true, |w| i.workspace_id == w)
                    && filter.email.as_deref().map_or(true, |e| i.email == e)
                    && filter
                        .invitee_user_id
                        .map_or(true, |u| i.invitee_user_id == Some(u))
                    && filter.status.map_or(true, |s| i.status == s)
            })
            .collect();
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if let Some(limit) = filter.limit {
            out.truncate(limit as usize);
        }
        Ok(out)
    }

    async fn update_status(&self, id: Id, patch: UpdateInvitationStatus) -> Result<InvitationRow> {
        let mut invitations = self.store.invitations.write().await;
        let row = invitations.get_mut(&id).ok_or(RepoError::NotFound)?;
        let now = Utc::now();
        let mut updated = row.clone();
        updated.status = patch.status;
        updated.updated_at = now;
        match patch.status {
            InvitationStatus::Accepted => {
                updated.accepted_at = Some(now);
                if let Some(uid) = patch.accepted_user_id {
                    updated.invitee_user_id = Some(uid);
                }
            }
            InvitationStatus::Declined => {
                updated.declined_at = Some(now);
            }
            _ => {}
        }
        *row = updated.clone();
        Ok(updated)
    }

    async fn revoke(&self, id: Id) -> Result<InvitationRow> {
        self.update_status(
            id,
            UpdateInvitationStatus {
                status: InvitationStatus::Revoked,
                accepted_user_id: None,
            },
        )
        .await
    }
}

// =========================================================================
// Postgres
// =========================================================================

#[derive(Clone)]
pub struct PgInvitationRepo {
    pool: sqlx::PgPool,
}

impl PgInvitationRepo {
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl InvitationRepo for PgInvitationRepo {
    async fn create(&self, item: NewInvitation) -> Result<InvitationRow> {
        let now = Utc::now();
        let expires = item.expires_at.unwrap_or_else(|| now + chrono::Duration::days(7));
        let row: InvitationRow = sqlx::query_as(
            r#"
            INSERT INTO workspace_invitation
                (id, workspace_id, email, invitee_user_id, invited_by_user_id,
                 role, token, status, expires_at, accepted_at, declined_at, updated_at)
            VALUES (
                COALESCE($1, gen_random_uuid()),
                $2, $3, $4, $5,
                $6, $7, 'pending', $8, NULL, NULL, now()
            )
            RETURNING id, workspace_id, email, invitee_user_id, invited_by_user_id,
                      role, token, status, expires_at, accepted_at, declined_at,
                      created_at, updated_at
            "#,
        )
        .bind(item.id)
        .bind(item.workspace_id)
        .bind(&item.email)
        .bind(item.invitee_user_id)
        .bind(item.invited_by_user_id)
        .bind(item.role.as_str())
        .bind(&item.token)
        .bind(expires)
        .fetch_one(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(row)
    }

    async fn get(&self, id: Id) -> Result<InvitationRow> {
        let row: InvitationRow = sqlx::query_as(
            r#"
            SELECT id, workspace_id, email, invitee_user_id, invited_by_user_id,
                   role, token, status, expires_at, accepted_at, declined_at,
                   created_at, updated_at
              FROM workspace_invitation WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(normalize(row))
    }

    async fn find_by_token(&self, token: &str) -> Result<Option<InvitationRow>> {
        let row: Option<InvitationRow> = sqlx::query_as(
            r#"
            SELECT id, workspace_id, email, invitee_user_id, invited_by_user_id,
                   role, token, status, expires_at, accepted_at, declined_at,
                   created_at, updated_at
              FROM workspace_invitation WHERE token = $1
            "#,
        )
        .bind(token)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(row.map(normalize))
    }

    async fn list(&self, filter: InvitationFilter) -> Result<Vec<InvitationRow>> {
        let limit = filter.limit.unwrap_or(200).min(1000) as i64;
        let status_str = filter.status.map(|s| s.as_str().to_string());
        let rows: Vec<InvitationRow> = sqlx::query_as(
            r#"
            SELECT id, workspace_id, email, invitee_user_id, invited_by_user_id,
                   role, token, status, expires_at, accepted_at, declined_at,
                   created_at, updated_at
              FROM workspace_invitation
             WHERE ($1::uuid IS NULL OR workspace_id = $1)
               AND ($2::text IS NULL OR email = $2)
               AND ($3::uuid IS NULL OR invitee_user_id = $3)
               AND ($4::text IS NULL OR status = $4)
             ORDER BY created_at DESC
             LIMIT $5
            "#,
        )
        .bind(filter.workspace_id)
        .bind(filter.email)
        .bind(filter.invitee_user_id)
        .bind(status_str)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(rows.into_iter().map(normalize).collect())
    }

    async fn update_status(&self, id: Id, patch: UpdateInvitationStatus) -> Result<InvitationRow> {
        let row: InvitationRow = sqlx::query_as(
            r#"
            UPDATE workspace_invitation SET
                status = $2,
                accepted_at = CASE WHEN $2 = 'accepted' THEN now() ELSE accepted_at END,
                declined_at = CASE WHEN $2 = 'declined' THEN now() ELSE declined_at END,
                invitee_user_id = COALESCE($3, invitee_user_id),
                updated_at = now()
            WHERE id = $1
            RETURNING id, workspace_id, email, invitee_user_id, invited_by_user_id,
                      role, token, status, expires_at, accepted_at, declined_at,
                      created_at, updated_at
            "#,
        )
        .bind(id)
        .bind(patch.status.as_str())
        .bind(patch.accepted_user_id)
        .fetch_one(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(normalize(row))
    }

    async fn revoke(&self, id: Id) -> Result<InvitationRow> {
        self.update_status(
            id,
            UpdateInvitationStatus {
                status: InvitationStatus::Revoked,
                accepted_user_id: None,
            },
        )
        .await
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

/// Normalise DB-side `revoked` → `revoked` (no-op); `pending` rows past their
/// expiry are flagged `expired` for the caller.
fn normalize(mut row: InvitationRow) -> InvitationRow {
    if row.status == InvitationStatus::Pending && row.expires_at <= Utc::now() {
        row.status = InvitationStatus::Expired;
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn new_inv(workspace_id: Id, email: &str) -> NewInvitation {
        NewInvitation {
            id: None,
            workspace_id,
            email: email.into(),
            invitee_user_id: None,
            invited_by_user_id: Id::from(Uuid::nil()),
            role: WorkspaceRole::Member,
            token: format!("tok-{}", Uuid::new_v4()),
            expires_at: None,
        }
    }

    #[tokio::test]
    async fn memory_create_then_find_by_token() {
        let store = MemoryStore::default();
        let repo = MemoryInvitationRepo::new(store);
        let ws = Id::from(Uuid::new_v4());
        let created = repo
            .create(new_inv(ws, "alice@example.com"))
            .await
            .unwrap();
        assert_eq!(created.status, InvitationStatus::Pending);

        let found = repo.find_by_token(&created.token).await.unwrap().unwrap();
        assert_eq!(found.email, "alice@example.com");
    }

    #[tokio::test]
    async fn memory_duplicate_pending_for_same_workspace_email_conflicts() {
        let store = MemoryStore::default();
        let repo = MemoryInvitationRepo::new(store);
        let ws = Id::from(Uuid::new_v4());
        repo.create(new_inv(ws, "alice@example.com")).await.unwrap();
        let err = repo
            .create(new_inv(ws, "alice@example.com"))
            .await
            .unwrap_err();
        assert!(matches!(err, RepoError::Conflict(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn memory_revoked_invitation_does_not_block_reissue() {
        let store = MemoryStore::default();
        let repo = MemoryInvitationRepo::new(store);
        let ws = Id::from(Uuid::new_v4());
        let first = repo.create(new_inv(ws, "alice@example.com")).await.unwrap();
        repo.revoke(first.id).await.unwrap();
        let second = repo.create(new_inv(ws, "alice@example.com")).await.unwrap();
        assert_eq!(second.id, first.id); // memory store reuses slots? no, IDs differ
        assert!(second.id != first.id);
    }
}
