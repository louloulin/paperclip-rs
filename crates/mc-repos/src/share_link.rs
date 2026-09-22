//! `workspace_share_link` 表的 DB-backed 仓储。
//!
//! 来源：LUM-1335（`feat/multica-rs-m1` @ `3ad402e`）的 share-link 增量，
//! 由 M1-D（LUM-1347）按本仓既有约定移植（见 `docs/09-M1-INTEGRATION.md` §2.2）：
//! - 单结构体 + `PgPool`（本仓 M1 各 Repo 无 `MemoryStore` / trait 变体）
//! - Row 用裸 `Uuid` / `String` 字段 + `Id` / `WorkspaceRole` 访问器
//! - 错误统一 `RepoError`；"最后一个 owner"/唯一约束一类的语义冲突用 `Conflict`
//!
//! 语义：
//! - 一个 workspace 同时只有 **一条** active share link（`create` 在事务里先把旧的
//!   `is_active = FALSE`，与迁移里的 partial unique index 对齐）
//! - 公开面用 `code`（不是 id）加入 workspace
//! - 可用性 = active ∧ 未过期 ∧ 未超使用次数（`is_usable`）

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use mc_core::id::Id;
use mc_core::workspace::WorkspaceRole;
use mc_db::Db;

use crate::workspace::map_sqlx_err;
use crate::{RepoError, Result};

/// 默认 share link 有效期（天）；`None` → 不过期。
pub const SHARE_LINK_DEFAULT_TTL_DAYS: i64 = 7;

/// 数据库行映射（`mc_core::Id` 没有 sqlx `Decode`，故保留裸 `Uuid`）。
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ShareLinkRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub code: String,
    pub created_by: Uuid,
    pub role: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub max_uses: Option<i32>,
    pub use_count: i32,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
}

impl ShareLinkRow {
    pub fn id(&self) -> Id {
        Id(self.id)
    }
    pub fn workspace_id(&self) -> Id {
        Id(self.workspace_id)
    }
    pub fn created_by(&self) -> Id {
        Id(self.created_by)
    }
    pub fn role(&self) -> WorkspaceRole {
        WorkspaceRole::from_str(&self.role).unwrap_or(WorkspaceRole::Member)
    }
    /// Link 当前可加入：active ∧ 未过期 ∧ 未超使用次数。
    ///
    /// 用 `is_some_and` + 取反（而不是 `map_or` / `is_none_or`）：后者在 clippy 1.98
    /// 下触发 `unnecessary_map_or`，而它建议的 `Option::is_none_or` 需要 Rust 1.82，
    /// 超出本仓 MSRV 1.80。
    pub fn is_usable(&self, now: DateTime<Utc>) -> bool {
        let expired = self.expires_at.is_some_and(|e| e <= now);
        let exhausted = self.max_uses.is_some_and(|m| self.use_count >= m);
        self.is_active && !expired && !exhausted
    }
}

/// 创建 share link 的输入。
#[derive(Debug, Clone)]
pub struct NewShareLink {
    pub workspace_id: Id,
    pub code: String,
    pub created_by: Id,
    pub role: WorkspaceRole,
    /// `None` → `now + SHARE_LINK_DEFAULT_TTL_DAYS`。
    pub expires_at: Option<DateTime<Utc>>,
    pub max_uses: Option<u32>,
}

/// 列出 share link 的过滤条件。
#[derive(Debug, Clone, Default)]
pub struct ShareLinkFilter {
    pub workspace_id: Option<Id>,
    pub code: Option<String>,
    pub active_only: bool,
    pub limit: Option<u32>,
}

/// `workspace_share_link` 仓储。
#[derive(Clone)]
pub struct ShareLinkRepo {
    pool: PgPool,
}

const COLUMNS: &str = "id, workspace_id, code, created_by, role, expires_at, \
                       max_uses, use_count, is_active, created_at";

impl ShareLinkRepo {
    /// 从应用共享 `Db` 句柄构造。
    pub fn new(db: &Db) -> Self {
        Self {
            pool: db.pool().clone(),
        }
    }

    /// 用自定义 pool 构造（集成测试用）。
    pub fn with_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// 生成 10 字符 URL-safe code（去掉易混淆字符 0/O/1/l/I）。
    pub fn generate_code() -> String {
        use rand::Rng;
        const ALPHABET: &[u8] = b"abcdefghijkmnopqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
        let mut rng = rand::thread_rng();
        (0..10)
            .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
            .collect()
    }

    /// 创建一条 share link。同 workspace 的旧 active link 会被自动置为 inactive。
    pub async fn create(&self, input: NewShareLink) -> Result<ShareLinkRow> {
        let expires_at = input
            .expires_at
            .unwrap_or_else(|| Utc::now() + Duration::days(SHARE_LINK_DEFAULT_TTL_DAYS));
        let mut tx = self.pool.begin().await.map_err(map_sqlx_err)?;
        // 与 `idx_share_link_workspace_active`（partial unique）对齐：先撤下旧的。
        sqlx::query("UPDATE workspace_share_link SET is_active = FALSE WHERE workspace_id = $1 AND is_active = TRUE")
            .bind(input.workspace_id.as_uuid())
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;
        let row = sqlx::query_as::<_, ShareLinkRow>(
            "INSERT INTO workspace_share_link \
                (workspace_id, code, created_by, role, expires_at, max_uses) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             RETURNING id, workspace_id, code, created_by, role, expires_at, \
                       max_uses, use_count, is_active, created_at",
        )
        .bind(input.workspace_id.as_uuid())
        .bind(&input.code)
        .bind(input.created_by.as_uuid())
        .bind(input.role.as_str())
        .bind(expires_at)
        // DB 列是 INT（i32）；超出 i32 的 max_uses 视为"几乎无限"（饱和），
        // 而不是回绕成负数。
        .bind(input.max_uses.map(|n| i32::try_from(n).unwrap_or(i32::MAX)))
        .fetch_one(&mut *tx)
        .await
        .map_err(map_sqlx_err)?;
        tx.commit().await.map_err(map_sqlx_err)?;
        Ok(row)
    }

    /// 按 id 查询。
    pub async fn get(&self, id: Id) -> Result<ShareLinkRow> {
        let row = sqlx::query_as::<_, ShareLinkRow>(&format!(
            "SELECT {COLUMNS} FROM workspace_share_link WHERE id = $1"
        ))
        .bind(id.as_uuid())
        .fetch_optional(self.pool())
        .await
        .map_err(map_sqlx_err)?
        .ok_or(RepoError::NotFound)?;
        Ok(row)
    }

    /// 按 code 查询（公开加入面）。
    pub async fn find_by_code(&self, code: &str) -> Result<Option<ShareLinkRow>> {
        let row = sqlx::query_as::<_, ShareLinkRow>(&format!(
            "SELECT {COLUMNS} FROM workspace_share_link WHERE code = $1"
        ))
        .bind(code)
        .fetch_optional(self.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(row)
    }

    /// 列出（按创建时间倒序）。
    pub async fn list(&self, filter: ShareLinkFilter) -> Result<Vec<ShareLinkRow>> {
        let limit = i64::from(filter.limit.unwrap_or(100).min(500));
        let rows = sqlx::query_as::<_, ShareLinkRow>(&format!(
            "SELECT {COLUMNS} FROM workspace_share_link \
             WHERE ($1::uuid IS NULL OR workspace_id = $1) \
               AND ($2::text IS NULL OR code = $2) \
               AND ($3::boolean = FALSE OR is_active = TRUE) \
             ORDER BY created_at DESC LIMIT $4"
        ))
        .bind(filter.workspace_id.map(mc_core::Id::as_uuid))
        .bind(filter.code)
        .bind(filter.active_only)
        .bind(limit)
        .fetch_all(self.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(rows)
    }

    /// 撤销（幂等：不存在返回 `NotFound`）。
    pub async fn revoke(&self, id: Id) -> Result<()> {
        let res = sqlx::query("UPDATE workspace_share_link SET is_active = FALSE WHERE id = $1")
            .bind(id.as_uuid())
            .execute(self.pool())
            .await
            .map_err(map_sqlx_err)?;
        if res.rows_affected() == 0 {
            Err(RepoError::NotFound)
        } else {
            Ok(())
        }
    }

    /// 原子 `use_count += 1`；不可用时返回 `Conflict`（与"最后一个 owner"同语义：
    /// 状态机拒绝本次变更）。
    pub async fn increment_use(&self, id: Id) -> Result<ShareLinkRow> {
        let row = sqlx::query_as::<_, ShareLinkRow>(
            "UPDATE workspace_share_link SET use_count = use_count + 1 \
             WHERE id = $1 \
               AND is_active = TRUE \
               AND (expires_at IS NULL OR expires_at > now()) \
               AND (max_uses IS NULL OR use_count < max_uses) \
             RETURNING id, workspace_id, code, created_by, role, expires_at, \
                       max_uses, use_count, is_active, created_at",
        )
        .bind(id.as_uuid())
        .fetch_optional(self.pool())
        .await
        .map_err(map_sqlx_err)?;
        row.ok_or(RepoError::Conflict)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        expires_at: Option<DateTime<Utc>>,
        max_uses: Option<i32>,
        use_count: i32,
    ) -> ShareLinkRow {
        ShareLinkRow {
            id: Uuid::new_v4(),
            workspace_id: Uuid::new_v4(),
            code: "abc".into(),
            created_by: Uuid::new_v4(),
            role: "member".into(),
            expires_at,
            max_uses,
            use_count,
            is_active: true,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn usable_when_active_without_limits() {
        assert!(row(None, None, 0).is_usable(Utc::now()));
    }

    #[test]
    fn unusable_when_expired_or_exhausted_or_inactive() {
        let now = Utc::now();
        assert!(!row(Some(now - Duration::hours(1)), None, 0).is_usable(now));
        assert!(!row(None, Some(2), 2).is_usable(now));
        let mut inactive = row(None, None, 0);
        inactive.is_active = false;
        assert!(!inactive.is_usable(now));
    }

    #[test]
    fn generate_code_is_unambiguous_and_distinct() {
        let a = ShareLinkRepo::generate_code();
        let b = ShareLinkRepo::generate_code();
        assert_eq!(a.len(), 10);
        assert!(!a.chars().any(|c| "0O1lI".contains(c)));
        assert_ne!(a, b);
    }

    // ---- DB 集成测试（`cargo test -- --ignored` + MULTICA_TEST_DATABASE_URL）----

    use crate::Repository;

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

    fn unique_slug(prefix: &str) -> mc_core::Slug {
        let s = Id::new().to_string().replace('-', "");
        mc_core::Slug::parse(&format!("{prefix}-{}", &s[..10])).unwrap()
    }

    #[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
    #[tokio::test]
    async fn db_create_find_and_consume() {
        let url = std::env::var("MULTICA_TEST_DATABASE_URL")
            .expect("set MULTICA_TEST_DATABASE_URL to enable DB tests");
        let pool = mc_db::pool::Db::connect(&url, 4, 1).await.unwrap();
        let ws = crate::workspace::WorkspaceRepo::new(pool.clone())
            .create(mc_core::workspace::NewWorkspace {
                name: "WS".into(),
                slug: unique_slug("share-link"),
                description: None,
            })
            .await
            .unwrap();
        let user = fresh_user(&pool, "share-link").await;
        let repo = ShareLinkRepo::new(&pool);

        let link = repo
            .create(NewShareLink {
                workspace_id: ws.id,
                code: ShareLinkRepo::generate_code(),
                created_by: user,
                role: WorkspaceRole::Member,
                expires_at: None,
                max_uses: Some(2),
            })
            .await
            .expect("create");
        assert_eq!(link.role(), WorkspaceRole::Member);

        let found = repo
            .find_by_code(&link.code)
            .await
            .unwrap()
            .expect("by code");
        assert_eq!(found.id(), link.id());

        let bumped = repo.increment_use(link.id()).await.expect("use 1");
        assert_eq!(bumped.use_count, 1);

        // 第二次创建会把旧的置为 inactive → 旧 link 不可再消费。
        let second = repo
            .create(NewShareLink {
                workspace_id: ws.id,
                code: ShareLinkRepo::generate_code(),
                created_by: user,
                role: WorkspaceRole::Admin,
                expires_at: None,
                max_uses: None,
            })
            .await
            .expect("create second");
        let err = repo.increment_use(link.id()).await.unwrap_err();
        assert!(matches!(err, RepoError::Conflict));

        repo.revoke(second.id()).await.expect("revoke");
        let listed = repo
            .list(ShareLinkFilter {
                workspace_id: Some(ws.id),
                active_only: true,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(listed.is_empty(), "all links inactive: {listed:?}");

        crate::workspace::WorkspaceRepo::new(pool.clone())
            .delete(&ws.id)
            .await
            .ok();
        crate::user::UserRepo::new(pool.clone())
            .delete(&user)
            .await
            .ok();
    }
}
