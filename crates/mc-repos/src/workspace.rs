//! Workspace 仓储层：CRUD + 列出 user 视角下的 workspace。
//!
//! schema：见 migrations/0001_init.up.sql 与 0002_auth_and_invitations.up.sql。

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use mc_core::id::Id;
use mc_core::slug::Slug;

use super::memory::MemoryStore;
use super::{RepoError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceRow {
    pub id: Id,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub avatar_url: Option<String>,
    pub settings: serde_json::Value,
    pub attribution_fail_closed: bool,
    pub private_plugin_identity: Option<serde_json::Value>,
    pub archived_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl WorkspaceRow {
    pub fn is_archived(&self) -> bool {
        self.archived_at.is_some()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewWorkspace {
    pub id: Option<Id>,
    pub name: String,
    pub slug: Slug,
    pub description: Option<String>,
    pub avatar_url: Option<String>,
    pub settings: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkspaceUpdate {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub avatar_url: Option<Option<String>>,
    pub settings: Option<serde_json::Value>,
    pub archived: Option<bool>,
}

#[derive(Debug, Clone, Default)]
pub struct WorkspaceFilter {
    /// 仅返回该 user 是 member 的 workspace。
    pub member_user_id: Option<Id>,
    pub slug: Option<String>,
    pub archived: Option<bool>,
    pub limit: Option<u32>,
}

#[async_trait]
pub trait WorkspaceRepo: Send + Sync {
    async fn create(&self, owner_id: Id, item: NewWorkspace) -> Result<WorkspaceRow>;
    async fn get(&self, id: Id) -> Result<WorkspaceRow>;
    async fn get_by_slug(&self, slug: &str) -> Result<Option<WorkspaceRow>>;
    async fn update(&self, id: Id, patch: WorkspaceUpdate) -> Result<WorkspaceRow>;
    async fn delete(&self, id: Id) -> Result<()>;
    async fn list(&self, filter: WorkspaceFilter) -> Result<Vec<WorkspaceRow>>;
}

// =========================================================================
// Memory
// =========================================================================

#[derive(Clone)]
pub struct MemoryWorkspaceRepo {
    store: MemoryStore,
}

impl MemoryWorkspaceRepo {
    pub fn new(store: MemoryStore) -> Self {
        Self { store }
    }

    pub fn store(&self) -> &MemoryStore {
        &self.store
    }
}

#[async_trait]
impl WorkspaceRepo for MemoryWorkspaceRepo {
    async fn create(&self, owner_id: Id, item: NewWorkspace) -> Result<WorkspaceRow> {
        let now = Utc::now();
        let id = item.id.unwrap_or_else(Id::new);
        let row = WorkspaceRow {
            id,
            name: item.name,
            slug: item.slug.as_str().to_string(),
            description: item.description,
            avatar_url: item.avatar_url,
            settings: item.settings.unwrap_or_else(|| serde_json::json!({})),
            attribution_fail_closed: false,
            private_plugin_identity: None,
            archived_at: None,
            created_at: now,
            updated_at: now,
        };
        {
            let workspaces = self.store.workspaces.read().await;
            if workspaces.values().any(|w| w.slug == row.slug) {
                return Err(RepoError::Conflict(format!(
                    "workspace slug {} already exists",
                    row.slug
                )));
            }
        }
        let mut workspaces = self.store.workspaces.write().await;
        workspaces.insert(row.id, row.clone());
        // Drop workspaces on a populated memory store — owner attachment is
        // handled by the member repo in real code paths; here we just persist
        // the workspace and trust the service layer to add the owner row.
        drop(workspaces);

        // Auto-create owner membership in memory store so listing by user works.
        let owner_row = super::member::MemberRow {
            id: Id::new(),
            workspace_id: row.id,
            user_id: owner_id,
            role: mc_core::workspace::WorkspaceRole::Owner,
            created_at: now,
            updated_at: now,
        };
        let mut members = self.store.members.write().await;
        members.insert(owner_row.id, owner_row);

        Ok(row)
    }

    async fn get(&self, id: Id) -> Result<WorkspaceRow> {
        let workspaces = self.store.workspaces.read().await;
        workspaces.get(&id).cloned().ok_or(RepoError::NotFound)
    }

    async fn get_by_slug(&self, slug: &str) -> Result<Option<WorkspaceRow>> {
        let workspaces = self.store.workspaces.read().await;
        Ok(workspaces.values().find(|w| w.slug == slug).cloned())
    }

    async fn update(&self, id: Id, patch: WorkspaceUpdate) -> Result<WorkspaceRow> {
        let mut workspaces = self.store.workspaces.write().await;
        let row = workspaces.get_mut(&id).ok_or(RepoError::NotFound)?;
        if let Some(name) = patch.name {
            row.name = name;
        }
        if let Some(desc) = patch.description {
            row.description = desc;
        }
        if let Some(avatar) = patch.avatar_url {
            row.avatar_url = avatar;
        }
        if let Some(settings) = patch.settings {
            row.settings = settings;
        }
        if let Some(archived) = patch.archived {
            row.archived_at = if archived { Some(Utc::now()) } else { None };
        }
        row.updated_at = Utc::now();
        Ok(row.clone())
    }

    async fn delete(&self, id: Id) -> Result<()> {
        let mut workspaces = self.store.workspaces.write().await;
        workspaces.remove(&id).ok_or(RepoError::NotFound)?;
        // Cascade in memory: wipe related members / invitations / share_links.
        let mut members = self.store.members.write().await;
        members.retain(|_, m| m.workspace_id != id);
        let mut invitations = self.store.invitations.write().await;
        invitations.retain(|_, i| i.workspace_id != id);
        let mut links = self.store.share_links.write().await;
        links.retain(|_, l| l.workspace_id != id);
        Ok(())
    }

    async fn list(&self, filter: WorkspaceFilter) -> Result<Vec<WorkspaceRow>> {
        let workspaces = self.store.workspaces.read().await;
        let members = self.store.members.read().await;
        let mut out: Vec<WorkspaceRow> = workspaces
            .values()
            .filter(|w| {
                filter.slug.as_deref().map_or(true, |s| w.slug == s)
                    && filter.archived.map_or(true, |a| w.is_archived() == a)
            })
            .filter(|w| {
                filter.member_user_id.map_or(true, |uid| {
                    members
                        .values()
                        .any(|m| m.workspace_id == w.id && m.user_id == uid)
                })
            })
            .cloned()
            .collect();
        out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
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
pub struct PgWorkspaceRepo {
    pool: sqlx::PgPool,
}

impl PgWorkspaceRepo {
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl WorkspaceRepo for PgWorkspaceRepo {
    async fn create(&self, owner_id: Id, item: NewWorkspace) -> Result<WorkspaceRow> {
        let mut tx = self.pool.begin().await.map_err(RepoError::from)?;
        let id = item.id.unwrap_or_else(Id::new);
        let row: WorkspaceRow = sqlx::query_as(
            r#"
            INSERT INTO workspace
                (id, name, slug, description, avatar_url, settings, attribution_fail_closed)
            VALUES ($1, $2, $3, $4, $5,
                    COALESCE($6, '{}'::jsonb),
                    FALSE)
            RETURNING id, name, slug, description, avatar_url, settings,
                      attribution_fail_closed, private_plugin_identity,
                      archived_at, created_at, updated_at
            "#,
        )
        .bind(id)
        .bind(&item.name)
        .bind(item.slug.as_str())
        .bind(&item.description)
        .bind(&item.avatar_url)
        .bind(item.settings.clone())
        .fetch_one(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;

        // Owner membership — ensures `list{ member_user_id = owner }` returns
        // the brand-new workspace.
        sqlx::query(
            r#"
            INSERT INTO member (id, workspace_id, user_id, role)
            VALUES (gen_random_uuid(), $1, $2, 'owner')
            ON CONFLICT (workspace_id, user_id) DO UPDATE SET role = 'owner'
            "#,
        )
        .bind(row.id)
        .bind(owner_id)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;

        tx.commit().await.map_err(RepoError::from)?;
        Ok(row)
    }

    async fn get(&self, id: Id) -> Result<WorkspaceRow> {
        let row: WorkspaceRow = sqlx::query_as(
            r#"
            SELECT id, name, slug, description, avatar_url, settings,
                   attribution_fail_closed, private_plugin_identity,
                   archived_at, created_at, updated_at
              FROM workspace WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(row)
    }

    async fn get_by_slug(&self, slug: &str) -> Result<Option<WorkspaceRow>> {
        let row: Option<WorkspaceRow> = sqlx::query_as(
            r#"
            SELECT id, name, slug, description, avatar_url, settings,
                   attribution_fail_closed, private_plugin_identity,
                   archived_at, created_at, updated_at
              FROM workspace WHERE slug = $1
            "#,
        )
        .bind(slug)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(row)
    }

    async fn update(&self, id: Id, patch: WorkspaceUpdate) -> Result<WorkspaceRow> {
        let mut current = self.get(id).await?;
        if let Some(name) = patch.name {
            current.name = name;
        }
        if let Some(desc) = patch.description {
            current.description = desc;
        }
        if let Some(avatar) = patch.avatar_url {
            current.avatar_url = avatar;
        }
        if let Some(settings) = patch.settings {
            current.settings = settings;
        }
        if let Some(archived) = patch.archived {
            current.archived_at = if archived { Some(Utc::now()) } else { None };
        }
        current.updated_at = Utc::now();

        let updated: WorkspaceRow = sqlx::query_as(
            r#"
            UPDATE workspace SET
                name = $2,
                description = $3,
                avatar_url = $4,
                settings = $5,
                archived_at = $6,
                updated_at = now()
            WHERE id = $1
            RETURNING id, name, slug, description, avatar_url, settings,
                      attribution_fail_closed, private_plugin_identity,
                      archived_at, created_at, updated_at
            "#,
        )
        .bind(current.id)
        .bind(&current.name)
        .bind(&current.description)
        .bind(&current.avatar_url)
        .bind(&current.settings)
        .bind(current.archived_at)
        .fetch_one(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(updated)
    }

    async fn delete(&self, id: Id) -> Result<()> {
        // FK ON DELETE CASCADE handles member / invitation / share_link;
        // see migrations for the CASCADE clauses.
        sqlx::query("DELETE FROM workspace WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(map_sqlx_error)?;
        Ok(())
    }

    async fn list(&self, filter: WorkspaceFilter) -> Result<Vec<WorkspaceRow>> {
        let limit = filter.limit.unwrap_or(50).min(500) as i64;
        let rows: Vec<WorkspaceRow> = sqlx::query_as(
            r#"
            SELECT w.id, w.name, w.slug, w.description, w.avatar_url, w.settings,
                   w.attribution_fail_closed, w.private_plugin_identity,
                   w.archived_at, w.created_at, w.updated_at
              FROM workspace w
             WHERE ($1::text IS NULL OR w.slug = $1)
               AND ($2::boolean IS NULL OR (w.archived_at IS NOT NULL) = $2)
               AND ($3::uuid IS NULL OR EXISTS (
                   SELECT 1 FROM member m
                    WHERE m.workspace_id = w.id AND m.user_id = $3
               ))
             ORDER BY w.updated_at DESC
             LIMIT $4
            "#,
        )
        .bind(filter.slug)
        .bind(filter.archived)
        .bind(filter.member_user_id)
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

    async fn seed() -> (MemoryWorkspaceRepo, MemoryStore) {
        let store = MemoryStore::default();
        let repo = MemoryWorkspaceRepo::new(store.clone());
        repo.create(
            Id::from(Uuid::nil()),
            NewWorkspace {
                id: None,
                name: "Acme".into(),
                slug: Slug::parse("acme").unwrap(),
                description: None,
                avatar_url: None,
                settings: None,
            },
        )
        .await
        .unwrap();
        (repo, store)
    }

    #[tokio::test]
    async fn memory_create_rejects_duplicate_slug() {
        let (repo, _) = seed().await;
        let err = repo
            .create(
                Id::from(Uuid::new_v4()),
                NewWorkspace {
                    id: None,
                    name: "Acme 2".into(),
                    slug: Slug::parse("acme").unwrap(),
                    description: None,
                    avatar_url: None,
                    settings: None,
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, RepoError::Conflict(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn memory_list_for_user_returns_only_member_workspaces() {
        let (repo, store) = seed().await;
        // owner of seeded workspace is Uuid::nil
        let member_of = repo
            .list(WorkspaceFilter {
                member_user_id: Some(Id::from(Uuid::nil())),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(member_of.len(), 1);

        let not_member = repo
            .list(WorkspaceFilter {
                member_user_id: Some(Id::from(Uuid::new_v4())),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(not_member.is_empty());

        let _ = store;
    }

    #[tokio::test]
    async fn memory_delete_cascades_member_rows() {
        let (repo, store) = seed().await;
        let ws = repo
            .get_by_slug("acme")
            .await
            .unwrap()
            .expect("acme seeded");
        repo.delete(ws.id).await.unwrap();

        let members = store.members.read().await;
        assert!(members
            .values()
            .all(|m| m.workspace_id != ws.id));
    }
}
