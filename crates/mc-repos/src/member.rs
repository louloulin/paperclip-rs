//! Workspace member 仓储层。

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use mc_core::id::Id;
use mc_core::workspace::WorkspaceRole;

use super::memory::MemoryStore;
use super::{RepoError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberRow {
    pub id: Id,
    pub workspace_id: Id,
    pub user_id: Id,
    pub role: WorkspaceRole,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewMember {
    pub workspace_id: Id,
    pub user_id: Id,
    pub role: WorkspaceRole,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemberUpdate {
    pub role: Option<WorkspaceRole>,
}

#[derive(Debug, Clone, Default)]
pub struct WorkspaceId(pub Id);

#[derive(Debug, Clone, Default)]
pub struct MemberFilter {
    pub workspace_id: Option<Id>,
    pub user_id: Option<Id>,
    pub role: Option<WorkspaceRole>,
    pub limit: Option<u32>,
}

#[async_trait]
pub trait MemberRepo: Send + Sync {
    async fn create(&self, item: NewMember) -> Result<MemberRow>;
    async fn get(&self, id: Id) -> Result<MemberRow>;
    async fn find(&self, workspace_id: Id, user_id: Id) -> Result<Option<MemberRow>>;
    async fn update(&self, id: Id, patch: MemberUpdate) -> Result<MemberRow>;
    async fn delete(&self, id: Id) -> Result<()>;
    async fn list_for_workspace(&self, workspace_id: Id) -> Result<Vec<MemberRow>>;
    async fn list_for_user(&self, user_id: Id) -> Result<Vec<MemberRow>>;
    async fn list(&self, filter: MemberFilter) -> Result<Vec<MemberRow>>;
}

// =========================================================================
// Memory
// =========================================================================

#[derive(Clone)]
pub struct MemoryMemberRepo {
    store: MemoryStore,
}

impl MemoryMemberRepo {
    pub fn new(store: MemoryStore) -> Self {
        Self { store }
    }

    pub fn store(&self) -> &MemoryStore {
        &self.store
    }
}

#[async_trait]
impl MemberRepo for MemoryMemberRepo {
    async fn create(&self, item: NewMember) -> Result<MemberRow> {
        let now = Utc::now();
        let row = MemberRow {
            id: Id::new(),
            workspace_id: item.workspace_id,
            user_id: item.user_id,
            role: item.role,
            created_at: now,
            updated_at: now,
        };
        let mut members = self.store.members.write().await;
        if members
            .values()
            .any(|m| m.workspace_id == row.workspace_id && m.user_id == row.user_id)
        {
            return Err(RepoError::Conflict("user already a member".into()));
        }
        members.insert(row.id, row.clone());
        Ok(row)
    }

    async fn get(&self, id: Id) -> Result<MemberRow> {
        let members = self.store.members.read().await;
        members.get(&id).cloned().ok_or(RepoError::NotFound)
    }

    async fn find(&self, workspace_id: Id, user_id: Id) -> Result<Option<MemberRow>> {
        let members = self.store.members.read().await;
        Ok(members
            .values()
            .find(|m| m.workspace_id == workspace_id && m.user_id == user_id)
            .cloned())
    }

    async fn update(&self, id: Id, patch: MemberUpdate) -> Result<MemberRow> {
        let mut members = self.store.members.write().await;
        let row = members.get_mut(&id).ok_or(RepoError::NotFound)?;
        if let Some(role) = patch.role {
            row.role = role;
        }
        row.updated_at = Utc::now();
        Ok(row.clone())
    }

    async fn delete(&self, id: Id) -> Result<()> {
        let mut members = self.store.members.write().await;
        let removed = members.remove(&id).ok_or(RepoError::NotFound)?;
        // Owner safeguard — call sites must move the owner off a workspace
        // before the last owner can be removed, but defensive in-repo check:
        if removed.role == WorkspaceRole::Owner {
            let remaining_owners = members
                .values()
                .filter(|m| m.workspace_id == removed.workspace_id && m.role == WorkspaceRole::Owner)
                .count();
            if remaining_owners == 0 {
                // Re-insert; the caller should have caught this, but refuse
                // to leave a workspace ownerless even from the repo layer.
                members.insert(removed.id, removed);
                return Err(RepoError::Invalid(
                    "cannot remove the last owner of a workspace".into(),
                ));
            }
        }
        Ok(())
    }

    async fn list_for_workspace(&self, workspace_id: Id) -> Result<Vec<MemberRow>> {
        let members = self.store.members.read().await;
        let mut out: Vec<MemberRow> = members
            .values()
            .filter(|m| m.workspace_id == workspace_id)
            .cloned()
            .collect();
        out.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(out)
    }

    async fn list_for_user(&self, user_id: Id) -> Result<Vec<MemberRow>> {
        let members = self.store.members.read().await;
        let mut out: Vec<MemberRow> = members.values().filter(|m| m.user_id == user_id).cloned().collect();
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(out)
    }

    async fn list(&self, filter: MemberFilter) -> Result<Vec<MemberRow>> {
        let members = self.store.members.read().await;
        let mut out: Vec<MemberRow> = members
            .values()
            .filter(|m| {
                filter.workspace_id.map_or(true, |w| m.workspace_id == w)
                    && filter.user_id.map_or(true, |u| m.user_id == u)
                    && filter.role.map_or(true, |r| m.role == r)
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
// Postgres
// =========================================================================

#[derive(Clone)]
pub struct PgMemberRepo {
    pool: sqlx::PgPool,
}

impl PgMemberRepo {
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl MemberRepo for PgMemberRepo {
    async fn create(&self, item: NewMember) -> Result<MemberRow> {
        let row: MemberRow = sqlx::query_as(
            r#"
            INSERT INTO member (id, workspace_id, user_id, role)
            VALUES (gen_random_uuid(), $1, $2, $3)
            RETURNING id, workspace_id, user_id, role, created_at, updated_at
            "#,
        )
        .bind(item.workspace_id)
        .bind(item.user_id)
        .bind(item.role.as_str())
        .fetch_one(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(row)
    }

    async fn get(&self, id: Id) -> Result<MemberRow> {
        let row: MemberRow = sqlx::query_as(
            r#"
            SELECT id, workspace_id, user_id, role, created_at, updated_at
              FROM member WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(row)
    }

    async fn find(&self, workspace_id: Id, user_id: Id) -> Result<Option<MemberRow>> {
        let row: Option<MemberRow> = sqlx::query_as(
            r#"
            SELECT id, workspace_id, user_id, role, created_at, updated_at
              FROM member
             WHERE workspace_id = $1 AND user_id = $2
            "#,
        )
        .bind(workspace_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(row)
    }

    async fn update(&self, id: Id, patch: MemberUpdate) -> Result<MemberRow> {
        let mut tx = self.pool.begin().await.map_err(RepoError::from)?;
        let current: MemberRow = sqlx::query_as(
            r#"
            SELECT id, workspace_id, user_id, role, created_at, updated_at
              FROM member WHERE id = $1 FOR UPDATE
            "#,
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        let new_role = patch.role.unwrap_or(current.role);
        if current.role == WorkspaceRole::Owner && new_role != WorkspaceRole::Owner {
            // Demoting the last owner is forbidden.
            let row: (i64,) = sqlx::query_as(
                r#"
                SELECT COUNT(*) FROM member
                 WHERE workspace_id = $1 AND role = 'owner' AND id <> $2
                "#,
            )
            .bind(current.workspace_id)
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
            if row.0 == 0 {
                return Err(RepoError::Invalid(
                    "cannot demote the last owner of a workspace".into(),
                ));
            }
        }
        let updated: MemberRow = sqlx::query_as(
            r#"
            UPDATE member SET role = $2, updated_at = now()
             WHERE id = $1
            RETURNING id, workspace_id, user_id, role, created_at, updated_at
            "#,
        )
        .bind(id)
        .bind(new_role.as_str())
        .fetch_one(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        tx.commit().await.map_err(RepoError::from)?;
        Ok(updated)
    }

    async fn delete(&self, id: Id) -> Result<()> {
        let mut tx = self.pool.begin().await.map_err(RepoError::from)?;
        let current: MemberRow = sqlx::query_as(
            r#"
            SELECT id, workspace_id, user_id, role, created_at, updated_at
              FROM member WHERE id = $1 FOR UPDATE
            "#,
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        if current.role == WorkspaceRole::Owner {
            let row: (i64,) = sqlx::query_as(
                r#"SELECT COUNT(*) FROM member WHERE workspace_id = $1 AND role = 'owner' AND id <> $2"#,
            )
            .bind(current.workspace_id)
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
            if row.0 == 0 {
                return Err(RepoError::Invalid(
                    "cannot remove the last owner of a workspace".into(),
                ));
            }
        }
        sqlx::query("DELETE FROM member WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        tx.commit().await.map_err(RepoError::from)?;
        Ok(())
    }

    async fn list_for_workspace(&self, workspace_id: Id) -> Result<Vec<MemberRow>> {
        let rows: Vec<MemberRow> = sqlx::query_as(
            r#"
            SELECT id, workspace_id, user_id, role, created_at, updated_at
              FROM member
             WHERE workspace_id = $1
             ORDER BY created_at ASC
            "#,
        )
        .bind(workspace_id)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(rows)
    }

    async fn list_for_user(&self, user_id: Id) -> Result<Vec<MemberRow>> {
        let rows: Vec<MemberRow> = sqlx::query_as(
            r#"
            SELECT id, workspace_id, user_id, role, created_at, updated_at
              FROM member
             WHERE user_id = $1
             ORDER BY created_at DESC
            "#,
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(rows)
    }

    async fn list(&self, filter: MemberFilter) -> Result<Vec<MemberRow>> {
        let limit = filter.limit.unwrap_or(100).min(1000) as i64;
        let role_str = filter.role.map(|r| r.as_str().to_string());
        let rows: Vec<MemberRow> = sqlx::query_as(
            r#"
            SELECT id, workspace_id, user_id, role, created_at, updated_at
              FROM member
             WHERE ($1::uuid IS NULL OR workspace_id = $1)
               AND ($2::uuid IS NULL OR user_id = $2)
               AND ($3::text IS NULL OR role = $3)
             ORDER BY created_at ASC
             LIMIT $4
            "#,
        )
        .bind(filter.workspace_id)
        .bind(filter.user_id)
        .bind(role_str)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(rows)
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

    async fn seed() -> (MemoryMemberRepo, MemoryStore) {
        let store = MemoryStore::default();
        let repo = MemoryMemberRepo::new(store.clone());
        repo.create(NewMember {
            workspace_id: Id::from(Uuid::new_v4()),
            user_id: Id::from(Uuid::nil()),
            role: WorkspaceRole::Member,
        })
        .await
        .unwrap();
        (repo, store)
    }

    #[tokio::test]
    async fn memory_create_then_find() {
        let (repo, _) = seed().await;
        let found = repo
            .find(Id::from(Uuid::new_v4()), Id::from(Uuid::nil()))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.role, WorkspaceRole::Member);
    }

    #[tokio::test]
    async fn memory_create_rejects_duplicates() {
        let (repo, _) = seed().await;
        let err = repo
            .create(NewMember {
                workspace_id: Id::from(Uuid::new_v4()),
                user_id: Id::from(Uuid::nil()),
                role: WorkspaceRole::Member,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, RepoError::Conflict(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn memory_list_for_user_returns_inserted() {
        let (repo, _) = seed().await;
        let listed = repo.list_for_user(Id::from(Uuid::nil())).await.unwrap();
        assert_eq!(listed.len(), 1);
    }
}
