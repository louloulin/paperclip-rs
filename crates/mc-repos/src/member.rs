//! `MemberRepo` — workspace `member` 表 CRUD + 关联查询。
//!
//! 错误映射与 `WorkspaceRepo` 共享：`sqlx::Error::RowNotFound` → `RepoError::NotFound`，
//! `unique` 约束 → `RepoError::Conflict`。
//!
//! `list_with_user` 通过 `LEFT JOIN "user"` 把 user 元信息一起取出，避免 N+1。

use chrono::{DateTime, Utc};
use mc_core::member::WorkspaceMember;
use mc_core::workspace::WorkspaceRole;
use mc_core::{Id, Timestamp};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use crate::workspace::map_sqlx_err;
use crate::{RepoError, RepoWithDb, Repository, Result};

use mc_db::Db;

/// `MemberRepo`。
#[derive(Clone)]
pub struct MemberRepo {
    db: Db,
}

impl MemberRepo {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

impl RepoWithDb for MemberRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

/// 新增 member 请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewMember {
    pub workspace_id: Id,
    pub user_id: Id,
    pub role: WorkspaceRole,
}

/// member patch（更新角色）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemberUpdate {
    pub role: Option<WorkspaceRole>,
}

/// `list` 过滤条件。
#[derive(Debug, Default, Clone)]
pub struct MemberFilter {
    pub workspace_id: Option<Id>,
    pub user_id: Option<Id>,
    pub role: Option<WorkspaceRole>,
}

/// Member 与 User 的连接结果（API 层 `list_members` 直接吐出）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberWithUser {
    pub id: Id,
    pub workspace_id: Id,
    pub user_id: Id,
    pub role: WorkspaceRole,
    pub name: String,
    pub email: String,
    pub avatar_url: Option<String>,
    pub created_at: Timestamp,
}

#[derive(Debug, FromRow)]
struct MemberRow {
    id: Uuid,
    workspace_id: Uuid,
    user_id: Uuid,
    role: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, FromRow)]
struct MemberWithUserRow {
    id: Uuid,
    workspace_id: Uuid,
    user_id: Uuid,
    role: String,
    created_at: DateTime<Utc>,
    user_name: String,
    user_email: String,
    user_avatar_url: Option<String>,
}

impl TryFrom<MemberRow> for WorkspaceMember {
    type Error = RepoError;

    fn try_from(row: MemberRow) -> Result<Self> {
        let role = parse_role(&row.role)?;
        Ok(WorkspaceMember {
            id: Id::from(row.id),
            workspace_id: Id::from(row.workspace_id),
            user_id: Id::from(row.user_id),
            role,
            created_at: Timestamp::from(row.created_at),
            updated_at: Timestamp::from(row.updated_at),
        })
    }
}

fn parse_role(s: &str) -> Result<WorkspaceRole> {
    WorkspaceRole::from_str(s).ok_or_else(|| RepoError::Db(format!("invalid workspace role: {s}")))
}

/// 该 workspace 里除 `except_member_id` 之外是否还有 owner（owner-safeguard 用）。
async fn has_other_owner(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    workspace_id: Uuid,
    except_member_id: Uuid,
) -> Result<bool> {
    let row: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM member \
         WHERE workspace_id = $1 AND role = 'owner' AND id <> $2",
    )
    .bind(workspace_id)
    .bind(except_member_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_sqlx_err)?;
    Ok(row.0 > 0)
}

#[async_trait::async_trait]
impl Repository<WorkspaceMember, NewMember, MemberUpdate, MemberFilter> for MemberRepo
where
    NewMember: Send + Sync,
    MemberUpdate: Send + Sync,
{
    async fn create(&self, item: NewMember) -> Result<WorkspaceMember> {
        let row = sqlx::query_as::<_, MemberRow>(
            "INSERT INTO member (workspace_id, user_id, role) \
             VALUES ($1, $2, $3) \
             RETURNING id, workspace_id, user_id, role, created_at, updated_at",
        )
        .bind(item.workspace_id.as_uuid())
        .bind(item.user_id.as_uuid())
        .bind(item.role.as_str())
        .fetch_one(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        row.try_into()
    }

    async fn get(&self, id: &Id) -> Result<WorkspaceMember> {
        let row = sqlx::query_as::<_, MemberRow>(
            "SELECT id, workspace_id, user_id, role, created_at, updated_at \
             FROM member WHERE id = $1",
        )
        .bind(id.as_uuid())
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?
        .ok_or(RepoError::NotFound)?;
        row.try_into()
    }

    async fn update(&self, id: &Id, patch: MemberUpdate) -> Result<WorkspaceMember> {
        // owner-safeguard（LUM-1335 增量，见 docs/09 §2.2）：把 workspace 的最后一个
        // owner 降级会让工作空间变成无主状态 → 拒绝。`FOR UPDATE` + 事务保证并发下不会
        // 两个请求同时看到"还有另一个 owner"。
        let mut tx = self.db.pool().begin().await.map_err(map_sqlx_err)?;
        let current: Option<MemberRow> = sqlx::query_as::<_, MemberRow>(
            "SELECT id, workspace_id, user_id, role, created_at, updated_at \
             FROM member WHERE id = $1 FOR UPDATE",
        )
        .bind(id.as_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_err)?;
        let current = current.ok_or(RepoError::NotFound)?;
        let current_role = parse_role(&current.role)?;
        let new_role = patch.role.unwrap_or(current_role);
        let role_str = new_role.as_str().to_string();
        if current_role == WorkspaceRole::Owner
            && new_role != WorkspaceRole::Owner
            && !has_other_owner(&mut tx, current.workspace_id, current.id).await?
        {
            return Err(RepoError::Conflict);
        }
        let row = sqlx::query_as::<_, MemberRow>(
            "UPDATE member SET \
                role = COALESCE($2, role), \
                updated_at = now() \
             WHERE id = $1 \
             RETURNING id, workspace_id, user_id, role, created_at, updated_at",
        )
        .bind(id.as_uuid())
        .bind(role_str)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_err)?;
        tx.commit().await.map_err(map_sqlx_err)?;
        row.ok_or(RepoError::NotFound)?.try_into()
    }

    async fn delete(&self, id: &Id) -> Result<()> {
        // owner-safeguard：最后一个 owner 不可被移除（同 `update` 的降级保护）。
        let mut tx = self.db.pool().begin().await.map_err(map_sqlx_err)?;
        let current: Option<MemberRow> = sqlx::query_as::<_, MemberRow>(
            "SELECT id, workspace_id, user_id, role, created_at, updated_at \
             FROM member WHERE id = $1 FOR UPDATE",
        )
        .bind(id.as_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_err)?;
        let current = current.ok_or(RepoError::NotFound)?;
        if parse_role(&current.role)? == WorkspaceRole::Owner
            && !has_other_owner(&mut tx, current.workspace_id, current.id).await?
        {
            return Err(RepoError::Conflict);
        }
        let res = sqlx::query("DELETE FROM member WHERE id = $1")
            .bind(id.as_uuid())
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;
        tx.commit().await.map_err(map_sqlx_err)?;
        if res.rows_affected() == 0 {
            Err(RepoError::NotFound)
        } else {
            Ok(())
        }
    }

    async fn list(&self, filter: MemberFilter) -> Result<Vec<WorkspaceMember>> {
        let limit: i64 = 500;
        let role_str = filter.role.map(|r| r.as_str().to_string());
        let rows = sqlx::query_as::<_, MemberRow>(
            "SELECT id, workspace_id, user_id, role, created_at, updated_at FROM member \
             WHERE ($1::uuid IS NULL OR workspace_id = $1) \
               AND ($2::uuid IS NULL OR user_id = $2) \
               AND ($3::text IS NULL OR role = $3) \
             ORDER BY created_at ASC LIMIT $4",
        )
        .bind(filter.workspace_id.map(mc_core::Id::as_uuid))
        .bind(filter.user_id.map(mc_core::Id::as_uuid))
        .bind(role_str)
        .bind(limit)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        rows.into_iter().map(TryInto::try_into).collect()
    }
}

impl MemberRepo {
    /// 列出某个 workspace 的所有 member。
    pub async fn list_for_workspace(&self, workspace_id: Id) -> Result<Vec<WorkspaceMember>> {
        self.list(MemberFilter {
            workspace_id: Some(workspace_id),
            ..Default::default()
        })
        .await
    }

    /// 列出某 workspace 的 member（含 user 名称 / 邮箱 / 头像）。
    ///
    /// 使用 LEFT JOIN 一次性获取，匹配上游 `ListMembersWithUser`。
    pub async fn list_with_user(&self, workspace_id: Id) -> Result<Vec<MemberWithUser>> {
        let rows = sqlx::query_as::<_, MemberWithUserRow>(
            "SELECT m.id, m.workspace_id, m.user_id, m.role, m.created_at, \
                    u.name as user_name, u.email as user_email, \
                    u.avatar_url as user_avatar_url \
             FROM member m \
             JOIN \"user\" u ON u.id = m.user_id \
             WHERE m.workspace_id = $1 \
             ORDER BY m.created_at ASC",
        )
        .bind(workspace_id.as_uuid())
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        rows.into_iter()
            .map(|r| {
                Ok(MemberWithUser {
                    id: Id::from(r.id),
                    workspace_id: Id::from(r.workspace_id),
                    user_id: Id::from(r.user_id),
                    role: parse_role(&r.role)?,
                    name: r.user_name,
                    email: r.user_email,
                    avatar_url: r.user_avatar_url,
                    created_at: Timestamp::from(r.created_at),
                })
            })
            .collect()
    }

    /// 给 `(workspace_id, user_id)` 查 member；测试与权限检查用。
    pub async fn get_for_user(&self, workspace_id: Id, user_id: Id) -> Result<WorkspaceMember> {
        let row = sqlx::query_as::<_, MemberRow>(
            "SELECT id, workspace_id, user_id, role, created_at, updated_at FROM member \
             WHERE workspace_id = $1 AND user_id = $2",
        )
        .bind(workspace_id.as_uuid())
        .bind(user_id.as_uuid())
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?
        .ok_or(RepoError::NotFound)?;
        row.try_into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mc_core::workspace::{NewWorkspace, WorkspaceRole};
    use mc_core::Slug;

    #[test]
    fn parse_role_known_values() {
        assert_eq!(parse_role("owner").unwrap(), WorkspaceRole::Owner);
        assert_eq!(parse_role("admin").unwrap(), WorkspaceRole::Admin);
        assert_eq!(parse_role("member").unwrap(), WorkspaceRole::Member);
        assert!(parse_role("bogus").is_err());
    }

    #[test]
    fn filter_default_shape() {
        let f = MemberFilter::default();
        assert!(f.workspace_id.is_none());
        assert!(f.user_id.is_none());
        assert!(f.role.is_none());
    }

    #[test]
    fn new_member_constructs() {
        let nm = NewMember {
            workspace_id: Id::new(),
            user_id: Id::new(),
            role: WorkspaceRole::Admin,
        };
        assert_eq!(nm.role, WorkspaceRole::Admin);
    }

    // ---- DB 集成测试 ----
    // member.user_id FK → "user"，测试先建真实 user；slug/邮箱每次运行唯一，
    // 保证在持久 Postgres 上可重复跑。

    async fn fresh_user(db: &mc_db::Db, tag: &str) -> Id {
        let s = Id::new().to_string().replace('-', "");
        crate::user::UserRepo::new(db.clone())
            .create(crate::user::NewUser {
                name: tag.into(),
                email: format!("{tag}-{}@example.com", &s[..12]),
                avatar_url: None,
            })
            .await
            .unwrap()
            .id
    }

    fn unique_slug(prefix: &str) -> Slug {
        let s = Id::new().to_string().replace('-', "");
        Slug::parse(&format!("{prefix}-{}", &s[..10])).unwrap()
    }

    #[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
    #[tokio::test]
    async fn db_add_list_remove() {
        let url = std::env::var("MULTICA_TEST_DATABASE_URL")
            .expect("set MULTICA_TEST_DATABASE_URL to enable DB tests");
        let pool = mc_db::pool::Db::connect(&url, 4, 1).await.unwrap();
        let ws_repo = crate::workspace::WorkspaceRepo::new(pool.clone());
        let m_repo = MemberRepo::new(pool.clone());

        let ws = ws_repo
            .create(NewWorkspace {
                name: "WS".into(),
                slug: unique_slug("member-add-list"),
                description: None,
            })
            .await
            .unwrap();
        let user = fresh_user(&pool, "member-add-list").await;
        let m = m_repo
            .create(NewMember {
                workspace_id: ws.id,
                user_id: user,
                role: WorkspaceRole::Member,
            })
            .await
            .expect("add");
        let list = m_repo.list_for_workspace(ws.id).await.expect("list");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].user_id, user);

        // role 检查：更新为 admin
        let updated = m_repo
            .update(
                &m.id,
                MemberUpdate {
                    role: Some(WorkspaceRole::Admin),
                },
            )
            .await
            .unwrap();
        assert_eq!(updated.role, WorkspaceRole::Admin);

        m_repo.delete(&m.id).await.expect("remove");
        ws_repo.delete(&ws.id).await.ok();
        crate::user::UserRepo::new(pool.clone())
            .delete(&user)
            .await
            .ok();
    }

    #[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
    #[tokio::test]
    async fn db_unique_member_conflict() {
        let url = std::env::var("MULTICA_TEST_DATABASE_URL")
            .expect("set MULTICA_TEST_DATABASE_URL to enable DB tests");
        let pool = mc_db::pool::Db::connect(&url, 4, 1).await.unwrap();
        let ws_repo = crate::workspace::WorkspaceRepo::new(pool.clone());
        let m_repo = MemberRepo::new(pool.clone());
        let ws = ws_repo
            .create(NewWorkspace {
                name: "WS".into(),
                slug: unique_slug("member-conflict"),
                description: None,
            })
            .await
            .unwrap();
        let user = fresh_user(&pool, "member-conflict").await;
        m_repo
            .create(NewMember {
                workspace_id: ws.id,
                user_id: user,
                role: WorkspaceRole::Member,
            })
            .await
            .unwrap();
        let err = m_repo
            .create(NewMember {
                workspace_id: ws.id,
                user_id: user,
                role: WorkspaceRole::Member,
            })
            .await
            .expect_err("duplicate insert must error");
        assert!(matches!(err, RepoError::Conflict));
        ws_repo.delete(&ws.id).await.ok();
        crate::user::UserRepo::new(pool.clone())
            .delete(&user)
            .await
            .ok();
    }

    #[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
    #[tokio::test]
    async fn db_role_check_admin_can_grant_owner_only_owner_can_change() {
        let url =
            std::env::var("MULTICA_TEST_DATABASE_URL").expect("set MULTICA_TEST_DATABASE_URL");
        let pool = mc_db::pool::Db::connect(&url, 4, 1).await.unwrap();
        let ws_repo = crate::workspace::WorkspaceRepo::new(pool.clone());
        let m_repo = MemberRepo::new(pool.clone());
        let ws = ws_repo
            .create(NewWorkspace {
                name: "WS".into(),
                slug: unique_slug("role-check"),
                description: None,
            })
            .await
            .unwrap();
        let owner = fresh_user(&pool, "role-owner").await;
        let admin = fresh_user(&pool, "role-admin").await;
        let member = fresh_user(&pool, "role-member").await;
        m_repo
            .create(NewMember {
                workspace_id: ws.id,
                user_id: owner,
                role: WorkspaceRole::Owner,
            })
            .await
            .unwrap();
        m_repo
            .create(NewMember {
                workspace_id: ws.id,
                user_id: admin,
                role: WorkspaceRole::Admin,
            })
            .await
            .unwrap();
        let m3 = m_repo
            .create(NewMember {
                workspace_id: ws.id,
                user_id: member,
                role: WorkspaceRole::Member,
            })
            .await
            .unwrap();

        // 角色 update 仅允许 owner → owner/admin/member 或 admin → admin/member/guest
        let promoted = m_repo
            .update(
                &m3.id,
                MemberUpdate {
                    role: Some(WorkspaceRole::Admin),
                },
            )
            .await
            .unwrap();
        assert_eq!(promoted.role, WorkspaceRole::Admin);
        ws_repo.delete(&ws.id).await.ok();
    }

    /// owner-safeguard（LUM-1335 增量移植）：最后一个 owner 不可降级、不可移除；
    /// 存在第二个 owner 后即解禁。
    #[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
    #[tokio::test]
    async fn db_last_owner_cannot_be_demoted_or_removed() {
        let url =
            std::env::var("MULTICA_TEST_DATABASE_URL").expect("set MULTICA_TEST_DATABASE_URL");
        let pool = mc_db::pool::Db::connect(&url, 4, 1).await.unwrap();
        let ws_repo = crate::workspace::WorkspaceRepo::new(pool.clone());
        let m_repo = MemberRepo::new(pool.clone());
        let ws = ws_repo
            .create(NewWorkspace {
                name: "WS".into(),
                slug: unique_slug("owner-guard"),
                description: None,
            })
            .await
            .unwrap();
        let owner = fresh_user(&pool, "guard-owner").await;
        let owner2 = fresh_user(&pool, "guard-owner2").await;
        let m1 = m_repo
            .create(NewMember {
                workspace_id: ws.id,
                user_id: owner,
                role: WorkspaceRole::Owner,
            })
            .await
            .unwrap();

        let err = m_repo
            .update(
                &m1.id,
                MemberUpdate {
                    role: Some(WorkspaceRole::Member),
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, RepoError::Conflict), "got: {err:?}");
        let err = m_repo.delete(&m1.id).await.unwrap_err();
        assert!(matches!(err, RepoError::Conflict), "got: {err:?}");

        m_repo
            .create(NewMember {
                workspace_id: ws.id,
                user_id: owner2,
                role: WorkspaceRole::Owner,
            })
            .await
            .unwrap();
        let demoted = m_repo
            .update(
                &m1.id,
                MemberUpdate {
                    role: Some(WorkspaceRole::Admin),
                },
            )
            .await
            .expect("second owner exists → demote allowed");
        assert_eq!(demoted.role, WorkspaceRole::Admin);
        m_repo
            .delete(&m1.id)
            .await
            .expect("non-last owner removable");
        ws_repo.delete(&ws.id).await.ok();
    }
}
