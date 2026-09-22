//! `WorkspaceRepo` — workspace 表 CRUD + 关联查询。
//!
//! 与上游 multica `workspace.sql` 对齐的子集（member-visible 字段）。
//! 所有 SQL 走 sqlx 的 typed-builder（`sqlx::query_as` / `sqlx::query`），参数通过
//! `.bind()` 注入；不使用 compile-time 宏，因此构建期不需要数据库。
//!
//! 错误映射：`sqlx::Error::RowNotFound` → `RepoError::NotFound`，
//! `unique` 约束 → `RepoError::Conflict`，其余 → `RepoError::Db`。

use chrono::{DateTime, Utc};
use mc_core::workspace::{NewWorkspace, Workspace, WorkspaceRole, WorkspaceUpdate};
use mc_core::{Id, Slug, Timestamp};
use serde_json::Value as JsonValue;
use sqlx::FromRow;
use uuid::Uuid;

use crate::{RepoError, RepoWithDb, Repository, Result};

use mc_db::Db;

/// WorkspaceRepo — `workspace` 表 + 必要的 JOIN。
#[derive(Clone)]
pub struct WorkspaceRepo {
    db: Db,
}

impl WorkspaceRepo {
    /// 构造。
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

impl RepoWithDb for WorkspaceRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

/// DB 行结构（镜像 `workspace` 表）。
#[derive(Debug, FromRow)]
struct WorkspaceRow {
    id: Uuid,
    name: String,
    slug: String,
    description: Option<String>,
    avatar_url: Option<String>,
    settings: JsonValue,
    archived_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl TryFrom<WorkspaceRow> for Workspace {
    type Error = RepoError;

    fn try_from(row: WorkspaceRow) -> Result<Self> {
        let slug = Slug::parse_unchecked(row.slug);
        Ok(Workspace {
            id: Id::from(row.id),
            name: row.name,
            slug,
            description: row.description,
            avatar_url: row.avatar_url,
            created_at: Timestamp::from(row.created_at),
            updated_at: Timestamp::from(row.updated_at),
            archived_at: row.archived_at.map(Timestamp::from),
            settings: row.settings,
        })
    }
}

const COLUMNS: &str = "id, name, slug, description, avatar_url, settings, \
                       archived_at, created_at, updated_at";

pub(crate) fn map_sqlx_err(err: sqlx::Error) -> RepoError {
    match &err {
        sqlx::Error::RowNotFound => RepoError::NotFound,
        sqlx::Error::Database(db) => {
            if let Some(code) = db.code() {
                if code == "23505" {
                    return RepoError::Conflict;
                }
            }
            RepoError::Db(err.to_string())
        }
        _ => RepoError::Db(err.to_string()),
    }
}

#[async_trait::async_trait]
impl Repository<Workspace, NewWorkspace, WorkspaceUpdate, WorkspaceFilter> for WorkspaceRepo
where
    NewWorkspace: Send + Sync,
    WorkspaceUpdate: Send + Sync,
{
    async fn create(&self, item: NewWorkspace) -> Result<Workspace> {
        let row = sqlx::query_as::<_, WorkspaceRow>(
            "INSERT INTO workspace (name, slug, description) \
             VALUES ($1, $2, $3) \
             RETURNING id, name, slug, description, avatar_url, settings, \
                       archived_at, created_at, updated_at",
        )
        .bind(item.name)
        .bind(item.slug.as_str())
        .bind(item.description)
        .fetch_one(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        row.try_into()
    }

    async fn get(&self, id: &Id) -> Result<Workspace> {
        let row = sqlx::query_as::<_, WorkspaceRow>(
            "SELECT id, name, slug, description, avatar_url, settings, \
                    archived_at, created_at, updated_at \
             FROM workspace WHERE id = $1",
        )
        .bind(id.as_uuid())
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?
        .ok_or(RepoError::NotFound)?;
        row.try_into()
    }

    async fn update(&self, id: &Id, patch: WorkspaceUpdate) -> Result<Workspace> {
        let row = sqlx::query_as::<_, WorkspaceRow>(
            "UPDATE workspace SET \
                name = COALESCE($2, name), \
                description = COALESCE($3, description), \
                avatar_url = COALESCE($4, avatar_url), \
                settings = COALESCE($5, settings), \
                updated_at = now() \
             WHERE id = $1 \
             RETURNING id, name, slug, description, avatar_url, settings, \
                       archived_at, created_at, updated_at",
        )
        .bind(id.as_uuid())
        .bind(patch.name)
        .bind(patch.description)
        .bind(patch.avatar_url)
        .bind(patch.settings)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?
        .ok_or(RepoError::NotFound)?;
        row.try_into()
    }

    async fn delete(&self, id: &Id) -> Result<()> {
        // 软删：archived_at = now()。
        // 上游 multica 的 hard-delete 流在 `workspace_delete_*` 系列（任务范围之外），
        // 本 sub-issue 仅做软删以兼容其它字段。
        let res = sqlx::query(
            "UPDATE workspace SET archived_at = now(), updated_at = now() \
             WHERE id = $1 AND archived_at IS NULL",
        )
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

    async fn list(&self, filter: WorkspaceFilter) -> Result<Vec<Workspace>> {
        let limit = filter.limit.unwrap_or(100).min(500);
        let after_id = filter.after_id;
        let include_archived = filter.include_archived;
        let rows = sqlx::query_as::<_, WorkspaceRow>(
            "SELECT id, name, slug, description, avatar_url, settings, \
                    archived_at, created_at, updated_at \
             FROM workspace \
             WHERE ($1::boolean OR archived_at IS NULL) \
               AND ($2::uuid IS NULL OR id > $2) \
             ORDER BY id ASC LIMIT $3",
        )
        .bind(include_archived)
        .bind(after_id.map(|i| i.as_uuid()))
        .bind(limit as i64)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        rows.into_iter().map(TryInto::try_into).collect()
    }
}

impl WorkspaceRepo {
    /// 按 slug 查询单个 workspace。
    pub async fn get_by_slug(&self, slug: &Slug) -> Result<Workspace> {
        let row = sqlx::query_as::<_, WorkspaceRow>(
            "SELECT id, name, slug, description, avatar_url, settings, \
                    archived_at, created_at, updated_at \
             FROM workspace WHERE slug = $1",
        )
        .bind(slug.as_str())
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?
        .ok_or(RepoError::NotFound)?;
        row.try_into()
    }

    /// 列出某个用户作为 member 的所有 workspace（profile / switcher）。
    pub async fn list_for_user(&self, user_id: Id) -> Result<Vec<Workspace>> {
        let rows = sqlx::query_as::<_, WorkspaceRow>(
            "SELECT w.id, w.name, w.slug, w.description, w.avatar_url, w.settings, \
                    w.archived_at, w.created_at, w.updated_at \
             FROM workspace w \
             JOIN member m ON m.workspace_id = w.id \
             WHERE m.user_id = $1 \
             ORDER BY w.created_at ASC",
        )
        .bind(user_id.as_uuid())
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        rows.into_iter().map(TryInto::try_into).collect()
    }
}

/// `list` 过滤条件。
#[derive(Debug, Default, Clone)]
pub struct WorkspaceFilter {
    pub include_archived: bool,
    pub after_id: Option<Id>,
    pub limit: Option<u32>,
}

/// `member` 行辅助构造器（re-export 处的 `member` repo 复用）。
#[allow(dead_code)]
pub(crate) fn role_to_str(role: WorkspaceRole) -> &'static str {
    role.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_sqlx_err_row_not_found() {
        let err = sqlx::Error::RowNotFound;
        assert!(matches!(map_sqlx_err(err), RepoError::NotFound));
    }

    #[test]
    fn filter_default_is_latest_only() {
        let f = WorkspaceFilter::default();
        assert!(!f.include_archived);
        assert_eq!(f.limit, None);
        assert_eq!(f.after_id, None);
    }

    #[test]
    fn role_to_str_canonical() {
        assert_eq!(role_to_str(WorkspaceRole::Owner), "owner");
        assert_eq!(role_to_str(WorkspaceRole::Admin), "admin");
        assert_eq!(role_to_str(WorkspaceRole::Member), "member");
        assert_eq!(role_to_str(WorkspaceRole::Guest), "guest");
    }

    // ---- DB 集成测试 ----
    // 需要可达的 Postgres：cargo test -p mc-repos --lib workspace::tests::db_*
    // 环境变量 `MULTICA_TEST_DATABASE_URL` 未设置时，`#[ignore]` 下的测试会被跳过。
    // slug 每次运行唯一（workspace 软删后 slug 仍占唯一约束），保证可重复跑。

    fn unique_slug(prefix: &str) -> Slug {
        let s = Id::new().to_string().replace('-', "");
        Slug::parse(&format!("{prefix}-{}", &s[..10])).unwrap()
    }

    #[ignore]
    #[tokio::test]
    async fn db_create_and_get_roundtrip() {
        let url = std::env::var("MULTICA_TEST_DATABASE_URL")
            .expect("set MULTICA_TEST_DATABASE_URL to enable DB tests");
        let pool = mc_db::pool::Db::connect(&url, 4, 1).await.unwrap();
        let repo = WorkspaceRepo::new(pool);
        let slug = unique_slug("test-roundtrip");
        let created = repo
            .create(NewWorkspace {
                name: "Test".into(),
                slug: slug.clone(),
                description: None,
            })
            .await
            .expect("create ok");
        assert_eq!(created.slug.as_str(), slug.as_str());
        let got = repo.get(&created.id).await.expect("get ok");
        assert_eq!(got.id, created.id);
        // 清理
        repo.delete(&created.id).await.expect("soft delete ok");
    }

    #[ignore]
    #[tokio::test]
    async fn db_unique_slug_conflict() {
        let url = std::env::var("MULTICA_TEST_DATABASE_URL")
            .expect("set MULTICA_TEST_DATABASE_URL to enable DB tests");
        let pool = mc_db::pool::Db::connect(&url, 4, 1).await.unwrap();
        let repo = WorkspaceRepo::new(pool);
        let slug = unique_slug("test-conflict");
        let a = repo
            .create(NewWorkspace {
                name: "A".into(),
                slug: slug.clone(),
                description: None,
            })
            .await
            .expect("first create ok");
        let err = repo
            .create(NewWorkspace {
                name: "B".into(),
                slug,
                description: None,
            })
            .await
            .expect_err("second create should conflict");
        assert!(matches!(err, RepoError::Conflict));
        repo.delete(&a.id).await.ok();
    }

    #[ignore]
    #[tokio::test]
    async fn db_list_for_user_filters_by_membership() {
        let url = std::env::var("MULTICA_TEST_DATABASE_URL")
            .expect("set MULTICA_TEST_DATABASE_URL to enable DB tests");
        let pool = mc_db::pool::Db::connect(&url, 4, 1).await.unwrap();
        let repo = WorkspaceRepo::new(pool);
        let ws_id = repo
            .create(NewWorkspace {
                name: "Member".into(),
                slug: unique_slug("list-for-user"),
                description: None,
            })
            .await
            .unwrap()
            .id;
        // member.user_id 有 FK 指向 "user"，先建真实 user。
        let s = Id::new().to_string().replace('-', "");
        let user = crate::user::UserRepo::new(repo.db().clone())
            .create(crate::user::NewUser {
                name: "list-for-user".into(),
                email: format!("list-for-user-{}@example.com", &s[..12]),
                avatar_url: None,
            })
            .await
            .unwrap();
        let user_id = user.id;
        let member_repo = crate::member::MemberRepo::new(repo.db().clone());
        member_repo
            .create(crate::member::NewMember {
                workspace_id: ws_id,
                user_id,
                role: WorkspaceRole::Member,
            })
            .await
            .unwrap();
        let list = repo.list_for_user(user_id).await.unwrap();
        assert!(list.iter().any(|w| w.id == ws_id));
        repo.delete(&ws_id).await.ok();
        crate::user::UserRepo::new(repo.db().clone())
            .delete(&user_id)
            .await
            .ok();
    }
}
