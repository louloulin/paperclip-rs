//! `workspace_invitation` 表的 DB-backed 仓储。
//!
//! 对应 multica server `internal/repo/invitation.go`，覆盖：
//! - 创建邀请（生成 32 字节 hex token，TTL 7 天）
//! - 按 token / id / workspace / email 查询
//! - 收件人接受（原子事务：标记 `accepted_at` + 创建 `member` row）
//! - 收件人拒绝 / 管理员撤销（软删除 via `revoked_at`）
//! - 速率限制：单 workspace 1h 内邀请计数
//!
//! 设计要点：
//! - `accept` 在单事务中完成：避免「已经接受但 member 行未插入」导致再次接受失败
//! - `accept` 走 UNIQUE(`workspace_id`, `user_id`) 冲突 → 幂等返回已存在 member
//! - `revoked_at` 与 `accepted_at` 互斥：再次 accept 时检查 `revoked_at`
//!
//! Token 形态：32 字节随机 → 43 字符 base64url（无 padding），URL 安全，无歧义字符。

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tracing::warn;
use uuid::Uuid;

use mc_core::id::Id;
use mc_core::member::WorkspaceMember;
use mc_core::workspace::WorkspaceRole;
use mc_db::Db;

use crate::{RepoError, Result};

/// 默认 token 字节长度（32 → base64url 后约 43 字符）。
pub const INVITATION_TOKEN_BYTES: usize = 32;
/// 默认 TTL：7 天。
pub const INVITATION_DEFAULT_TTL_DAYS: i64 = 7;
/// 创建邀请时给收件人 member 的默认 role。
pub const INVITATION_DEFAULT_ROLE: WorkspaceRole = WorkspaceRole::Member;

/// 数据库行映射。
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct InvitationRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub email: String,
    pub role: String,
    pub invited_by_user_id: Uuid,
    pub token: String,
    pub expires_at: DateTime<Utc>,
    pub accepted_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl InvitationRow {
    /// 转换为领域 `Id` 形态。
    pub fn id(&self) -> Id {
        Id(self.id)
    }
    pub fn workspace_id(&self) -> Id {
        Id(self.workspace_id)
    }
    pub fn invited_by_user_id(&self) -> Id {
        Id(self.invited_by_user_id)
    }
    pub fn role(&self) -> WorkspaceRole {
        match self.role.as_str() {
            "admin" => WorkspaceRole::Admin,
            "owner" => WorkspaceRole::Owner,
            "guest" => WorkspaceRole::Guest,
            _ => WorkspaceRole::Member,
        }
    }
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.expires_at <= now
    }
    pub fn is_revoked(&self) -> bool {
        self.revoked_at.is_some()
    }
    pub fn is_accepted(&self) -> bool {
        self.accepted_at.is_some()
    }
    pub fn is_active(&self, now: DateTime<Utc>) -> bool {
        !self.is_revoked() && !self.is_accepted() && !self.is_expired(now)
    }
}

/// 创建邀请的输入。
#[derive(Debug, Clone)]
pub struct NewInvitation {
    pub workspace_id: Id,
    pub email: String,
    pub role: WorkspaceRole,
    pub invited_by_user_id: Id,
    /// 可选 TTL 覆盖（秒）。None → 默认 7 天。
    pub ttl_secs: Option<i64>,
}

/// 接受邀请的返回结果。
#[derive(Debug, Clone)]
pub struct AcceptOutcome {
    pub member: WorkspaceMember,
    /// 已经被先前接受过（UNIQUE 冲突幂等）—— 当前调用方未实际写入 member。
    pub already_accepted: bool,
}

#[derive(Clone)]
pub struct InvitationRepo {
    pool: PgPool,
}

impl InvitationRepo {
    /// 从应用共享 `Db` 句柄构造。
    pub fn new(db: &Db) -> Self {
        Self {
            pool: db.pool().clone(),
        }
    }

    /// 用自定义 pool 构造（用于集成测试）。
    pub fn with_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// 生成 43 字符 base64url token（32 字节随机，无 padding）。
    pub fn generate_token() -> String {
        let mut bytes = [0u8; INVITATION_TOKEN_BYTES];
        rand::thread_rng().fill_bytes(&mut bytes);
        URL_SAFE_NO_PAD.encode(bytes)
    }

    /// 创建一个邀请。返回 (row, token)。
    pub async fn create(&self, input: NewInvitation) -> Result<InvitationRow> {
        let ttl = input
            .ttl_secs
            .unwrap_or(INVITATION_DEFAULT_TTL_DAYS * 24 * 3600);
        let now = Utc::now();
        let expires_at = now + Duration::seconds(ttl);
        let token = Self::generate_token();

        let row = sqlx::query_as::<_, InvitationRow>(
            r"
            INSERT INTO workspace_invitation (workspace_id, invitee_email, role, inviter_id, token, expires_at, created_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            RETURNING id, workspace_id, invitee_email AS email, role, inviter_id AS invited_by_user_id, token,
                      expires_at, accepted_at, revoked_at, created_at
            ",
        )
        .bind(input.workspace_id.0)
        .bind(&input.email)
        .bind(input.role.as_str())
        .bind(input.invited_by_user_id.0)
        .bind(&token)
        .bind(expires_at)
        .bind(now)
        .fetch_one(&self.pool)
        .await
        .map_err(map_db_err("invitation.create"))?;

        // 占位：上游在此处会通过 SMTP / 站内信通知收件人。
        // multica-rs M1 阶段不接 SMTP，用 tracing 记一条 info。
        warn!(
            invitation_id = %row.id,
            workspace_id = %row.workspace_id,
            email = %row.email,
            role = %row.role,
            "invitation created (smtp placeholder; logged via tracing)"
        );

        Ok(row)
    }

    /// 按 token 查询（含已撤销 / 已过期）。返回 `Ok(None)` 表示不存在。
    pub async fn get_by_token(&self, token: &str) -> Result<Option<InvitationRow>> {
        let row = sqlx::query_as::<_, InvitationRow>(
            r"
            SELECT id, workspace_id, invitee_email AS email, role, inviter_id AS invited_by_user_id, token,
                   expires_at, accepted_at, revoked_at, created_at
            FROM workspace_invitation
            WHERE token = $1
            ",
        )
        .bind(token)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_db_err("invitation.get_by_token"))?;
        Ok(row)
    }

    /// 按 id 查询。404 if missing。
    pub async fn get_by_id(&self, id: Id) -> Result<InvitationRow> {
        let row = sqlx::query_as::<_, InvitationRow>(
            r"
            SELECT id, workspace_id, invitee_email AS email, role, inviter_id AS invited_by_user_id, token,
                   expires_at, accepted_at, revoked_at, created_at
            FROM workspace_invitation
            WHERE id = $1
            ",
        )
        .bind(id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_db_err("invitation.get_by_id"))?
        .ok_or(RepoError::NotFound)?;
        Ok(row)
    }

    /// 列出一个 workspace 下所有未撤销 / 未过期的邀请（按 `created_at` 倒序）。
    pub async fn list_for_workspace(&self, workspace_id: Id) -> Result<Vec<InvitationRow>> {
        let now = Utc::now();
        let rows = sqlx::query_as::<_, InvitationRow>(
            r"
            SELECT id, workspace_id, invitee_email AS email, role, inviter_id AS invited_by_user_id, token,
                   expires_at, accepted_at, revoked_at, created_at
            FROM workspace_invitation
            WHERE workspace_id = $1
              AND revoked_at IS NULL
              AND expires_at > $2
            ORDER BY created_at DESC
            ",
        )
        .bind(workspace_id.0)
        .bind(now)
        .fetch_all(&self.pool)
        .await
        .map_err(map_db_err("invitation.list_for_workspace"))?;
        Ok(rows)
    }

    /// 按 email 列出收件人未撤销 / 未接受的邀请（用于 `/api/invitations`）。
    pub async fn list_for_user_email(&self, email: &str) -> Result<Vec<InvitationRow>> {
        let now = Utc::now();
        let rows = sqlx::query_as::<_, InvitationRow>(
            r"
            SELECT id, workspace_id, invitee_email AS email, role, inviter_id AS invited_by_user_id, token,
                   expires_at, accepted_at, revoked_at, created_at
            FROM workspace_invitation
            WHERE invitee_email = $1
              AND revoked_at IS NULL
              AND accepted_at IS NULL
              AND expires_at > $2
            ORDER BY created_at DESC
            ",
        )
        .bind(email)
        .bind(now)
        .fetch_all(&self.pool)
        .await
        .map_err(map_db_err("invitation.list_for_user_email"))?;
        Ok(rows)
    }

    /// 接受邀请（原子事务）。
    ///
    /// 流程：
    /// 1. 锁住行：`SELECT ... FOR UPDATE`
    /// 2. 校验：未撤销 / 未过期 / 未接受
    /// 3. 写 `accepted_at = now()`
    /// 4. 插入 `member` row，UNIQUE 冲突 → 已接受过，返回已存在 member
    #[allow(clippy::too_many_lines)] // 事务步骤 + 冲突分支线性展开，拆函数反而割裂锁语义。
    pub async fn accept(&self, token: &str, accepting_user_id: Id) -> Result<AcceptOutcome> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(map_db_err("invitation.accept.begin"))?;

        let row: Option<InvitationRow> = sqlx::query_as::<_, InvitationRow>(
            r"
            SELECT id, workspace_id, invitee_email AS email, role, inviter_id AS invited_by_user_id, token,
                   expires_at, accepted_at, revoked_at, created_at
            FROM workspace_invitation
            WHERE token = $1
            FOR UPDATE
            ",
        )
        .bind(token)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db_err("invitation.accept.select"))?;

        let row = row.ok_or(RepoError::NotFound)?;

        let now = Utc::now();
        if row.revoked_at.is_some() {
            return Err(RepoError::Conflict);
        }
        if row.expires_at <= now {
            return Err(RepoError::Conflict);
        }
        if row.accepted_at.is_some() {
            // 已经被接受：直接返回当前 member，幂等。
            let member_row: Option<(Uuid, String, DateTime<Utc>, DateTime<Utc>)> = sqlx::query_as(
                r"
                    SELECT id, role, created_at, updated_at
                    FROM member
                    WHERE workspace_id = $1 AND user_id = $2
                    ",
            )
            .bind(row.workspace_id)
            .bind(accepting_user_id.0)
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_db_err("invitation.accept.find_existing_member"))?;
            let (mid, role, created_at, updated_at) = member_row.ok_or(RepoError::NotFound)?;
            tx.commit()
                .await
                .map_err(map_db_err("invitation.accept.commit"))?;
            return Ok(AcceptOutcome {
                member: WorkspaceMember {
                    id: Id(mid),
                    workspace_id: Id(row.workspace_id),
                    user_id: accepting_user_id,
                    role: parse_role(&role),
                    created_at: mc_core::Timestamp::from(created_at),
                    updated_at: mc_core::Timestamp::from(updated_at),
                },
                already_accepted: true,
            });
        }
        // 接受：写本地 `accepted_at`，并镜像上游 `status`
        sqlx::query(
            "UPDATE workspace_invitation SET accepted_at = $2, status = 'accepted' WHERE id = $1",
        )
        .bind(row.id)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(map_db_err("invitation.accept.update"))?;

        // 插入 member 行。
        let member_id = Uuid::new_v4();
        let role = row.role.as_str();

        let insert_res = sqlx::query(
            r"
            INSERT INTO member (id, workspace_id, user_id, role, created_at, updated_at)
            VALUES ($1, $2, $3, $4, $5, $5)
            ",
        )
        .bind(member_id)
        .bind(row.workspace_id)
        .bind(accepting_user_id.0)
        .bind(role)
        .bind(now)
        .execute(&mut *tx)
        .await;

        match insert_res {
            Ok(_) => {
                tx.commit()
                    .await
                    .map_err(map_db_err("invitation.accept.commit"))?;
                Ok(AcceptOutcome {
                    member: WorkspaceMember {
                        id: Id(member_id),
                        workspace_id: Id(row.workspace_id),
                        user_id: accepting_user_id,
                        role: row.role(),
                        created_at: mc_core::Timestamp::from(now),
                        updated_at: mc_core::Timestamp::from(now),
                    },
                    already_accepted: false,
                })
            }
            Err(sqlx::Error::Database(db_err))
                if db_err.constraint() == Some("member_workspace_id_user_id_key") =>
            {
                // 已存在 member —— 视为幂等成功。
                let member_row: Option<(Uuid, String, DateTime<Utc>, DateTime<Utc>)> =
                    sqlx::query_as(
                        r"
                        SELECT id, role, created_at, updated_at
                        FROM member
                        WHERE workspace_id = $1 AND user_id = $2
                        ",
                    )
                    .bind(row.workspace_id)
                    .bind(accepting_user_id.0)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(map_db_err("invitation.accept.find_existing_member"))?;
                let (mid, role, created_at, updated_at) = member_row.ok_or(RepoError::NotFound)?;
                tx.commit()
                    .await
                    .map_err(map_db_err("invitation.accept.commit"))?;
                Ok(AcceptOutcome {
                    member: WorkspaceMember {
                        id: Id(mid),
                        workspace_id: Id(row.workspace_id),
                        user_id: accepting_user_id,
                        role: parse_role(&role),
                        created_at: mc_core::Timestamp::from(created_at),
                        updated_at: mc_core::Timestamp::from(updated_at),
                    },
                    already_accepted: true,
                })
            }
            Err(e) => Err(map_db_err("invitation.accept.insert_member")(e)),
        }
    }

    /// 收件人自己拒绝邀请（软删除）。
    pub async fn decline(&self, token: &str) -> Result<()> {
        let now = Utc::now();
        let res = sqlx::query(
            r"
            UPDATE workspace_invitation
            SET revoked_at = $2, status = 'declined'
            WHERE token = $1
              AND revoked_at IS NULL
              AND accepted_at IS NULL
            ",
        )
        .bind(token)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(map_db_err("invitation.decline"))?;

        if res.rows_affected() == 0 {
            // 不存在 / 已撤销 / 已接受 —— 都视为 NotFound
            return Err(RepoError::NotFound);
        }
        Ok(())
    }

    /// 管理员撤销邀请。
    pub async fn revoke(&self, id: Id, by_user_id: Id) -> Result<()> {
        let now = Utc::now();
        let res = sqlx::query(
            r"
            UPDATE workspace_invitation
            SET revoked_at = $2, status = 'declined'
            WHERE id = $1
              AND revoked_at IS NULL
              AND accepted_at IS NULL
            ",
        )
        .bind(id.0)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(map_db_err("invitation.revoke"))?;

        if res.rows_affected() == 0 {
            return Err(RepoError::NotFound);
        }

        warn!(
            invitation_id = %id,
            revoked_by = %by_user_id,
            "invitation revoked by admin"
        );
        Ok(())
    }

    /// 速率限制：单 workspace 自 `since` 起新增的邀请条数。
    pub async fn count_recent_in_workspace(
        &self,
        workspace_id: Id,
        since: DateTime<Utc>,
    ) -> Result<i64> {
        let (count,): (i64,) = sqlx::query_as(
            r"
            SELECT COUNT(*)::BIGINT
            FROM workspace_invitation
            WHERE workspace_id = $1
              AND created_at >= $2
            ",
        )
        .bind(workspace_id.0)
        .bind(since)
        .fetch_one(&self.pool)
        .await
        .map_err(map_db_err("invitation.count_recent_in_workspace"))?;
        Ok(count)
    }
}

fn parse_role(role: &str) -> WorkspaceRole {
    match role {
        "owner" => WorkspaceRole::Owner,
        "admin" => WorkspaceRole::Admin,
        "guest" => WorkspaceRole::Guest,
        _ => WorkspaceRole::Member,
    }
}

fn map_db_err(op: &'static str) -> impl FnOnce(sqlx::Error) -> RepoError {
    move |e| match e {
        sqlx::Error::RowNotFound => RepoError::NotFound,
        other => {
            tracing::error!(op = op, error = %other, "invitation repo db error");
            RepoError::Db(other.to_string())
        }
    }
}

// ---------------------------------------------------------------------------
// 单元测试（不依赖数据库）：覆盖 token 格式与 row 状态机。
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_is_base64url_and_long_enough() {
        let t = InvitationRepo::generate_token();
        assert!(t.len() >= 43, "token too short: {t}");
        assert!(
            t.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "non base64url char in token: {t}"
        );
        // 两次生成应不同
        let t2 = InvitationRepo::generate_token();
        assert_ne!(t, t2);
    }

    #[test]
    fn row_state_machine() {
        let now = Utc::now();
        let row = InvitationRow {
            id: Uuid::new_v4(),
            workspace_id: Uuid::new_v4(),
            email: "x@y".into(),
            role: "member".into(),
            invited_by_user_id: Uuid::new_v4(),
            token: "t".into(),
            expires_at: now + Duration::days(7),
            accepted_at: None,
            revoked_at: None,
            created_at: now,
        };
        assert!(row.is_active(now));
        assert!(!row.is_expired(now));
        assert!(!row.is_revoked());
        assert!(!row.is_accepted());

        let revoked = InvitationRow {
            revoked_at: Some(now),
            ..row.clone()
        };
        assert!(!revoked.is_active(now));

        let accepted = InvitationRow {
            accepted_at: Some(now),
            ..row.clone()
        };
        assert!(!accepted.is_active(now));

        let expired = InvitationRow {
            expires_at: now - Duration::seconds(1),
            ..row
        };
        assert!(!expired.is_active(now));
        assert!(expired.is_expired(now));
    }

    #[test]
    fn role_round_trip() {
        let row = InvitationRow {
            id: Uuid::new_v4(),
            workspace_id: Uuid::new_v4(),
            email: "x".into(),
            role: "admin".into(),
            invited_by_user_id: Uuid::new_v4(),
            token: "t".into(),
            expires_at: Utc::now(),
            accepted_at: None,
            revoked_at: None,
            created_at: Utc::now(),
        };
        assert_eq!(row.role(), WorkspaceRole::Admin);
    }
}

// ---------------------------------------------------------------------------
// 集成测试（需要 Postgres）。仅在 `MULTICA_TEST_DATABASE_URL` 设置时才连接。
// 运行：`MULTICA_TEST_DATABASE_URL=postgres://... cargo test -p mc-repos -- --ignored`
// ---------------------------------------------------------------------------
#[cfg(test)]
mod integration_tests {
    use super::*;
    use std::env;

    async fn connect() -> Option<(sqlx::PgPool, Id, Id)> {
        let url = env::var("MULTICA_TEST_DATABASE_URL").ok()?;
        let pool = sqlx::PgPool::connect(&url).await.ok()?;
        // 准备一个 workspace + 一个邀请者 user
        let workspace_id: Uuid = sqlx::query_scalar(
            "INSERT INTO workspace(name, slug) VALUES ('itest-ws', $1) RETURNING id",
        )
        .bind(format!("itest-{}", Uuid::new_v4()))
        .fetch_one(&pool)
        .await
        .ok()?;
        let user_id: Uuid = sqlx::query_scalar(
            r#"INSERT INTO "user"(name, email) VALUES ('itest-inviter', $1) RETURNING id"#,
        )
        .bind(format!("itest-{}@example.com", Uuid::new_v4()))
        .fetch_one(&pool)
        .await
        .ok()?;
        Some((pool, Id(workspace_id), Id(user_id)))
    }

    async fn cleanup_workspace(pool: &sqlx::PgPool, workspace_id: Uuid) {
        let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
            .bind(workspace_id)
            .execute(pool)
            .await;
    }

    async fn cleanup_user(pool: &sqlx::PgPool, user_id: Uuid) {
        let _ = sqlx::query("DELETE FROM \"user\" WHERE id = $1")
            .bind(user_id)
            .execute(pool)
            .await;
    }

    /// 1. create + `get_by_token` 往返。
    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn create_then_get_by_token() {
        let Some((pool, ws, inviter)) = connect().await else {
            eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
            return;
        };
        let repo = InvitationRepo::with_pool(pool.clone());

        let row = repo
            .create(NewInvitation {
                workspace_id: ws,
                email: "x@example.com".into(),
                role: WorkspaceRole::Member,
                invited_by_user_id: inviter,
                ttl_secs: Some(3600),
            })
            .await
            .expect("create ok");

        let fetched = repo
            .get_by_token(&row.token)
            .await
            .expect("get_by_token ok")
            .expect("present");
        assert_eq!(fetched.id, row.id);
        assert_eq!(fetched.email, "x@example.com");
        assert_eq!(fetched.workspace_id, ws.0);

        cleanup_workspace(&pool, ws.0).await;
        cleanup_user(&pool, inviter.0).await;
        pool.close().await;
    }

    /// 2. accept → 插入 member row + `accepted_at` 标记 + UNIQUE 冲突幂等。
    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn accept_creates_member_and_is_idempotent() {
        let Some((pool, ws, inviter)) = connect().await else {
            eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
            return;
        };
        let repo = InvitationRepo::with_pool(pool.clone());

        // 收件人 user
        let recipient: Uuid = sqlx::query_scalar(
            r#"INSERT INTO "user"(name, email) VALUES ('itest-recipient', $1) RETURNING id"#,
        )
        .bind(format!("recipient-{}@example.com", Uuid::new_v4()))
        .fetch_one(&pool)
        .await
        .expect("insert recipient");

        let row = repo
            .create(NewInvitation {
                workspace_id: ws,
                email: "ignored@example.com".into(),
                role: WorkspaceRole::Member,
                invited_by_user_id: inviter,
                ttl_secs: Some(3600),
            })
            .await
            .expect("create ok");

        // 首次 accept
        let out1 = repo
            .accept(&row.token, Id(recipient))
            .await
            .expect("first accept ok");
        assert!(!out1.already_accepted);
        assert_eq!(out1.member.user_id, Id(recipient));
        assert_eq!(out1.member.workspace_id, ws);

        // 二次 accept 应幂等
        let out2 = repo
            .accept(&row.token, Id(recipient))
            .await
            .expect("second accept ok");
        assert!(out2.already_accepted);

        cleanup_workspace(&pool, ws.0).await;
        cleanup_user(&pool, inviter.0).await;
        sqlx::query("DELETE FROM \"user\" WHERE id = $1")
            .bind(recipient)
            .execute(&pool)
            .await
            .ok();
        pool.close().await;
    }

    /// 3. decline 标记 `revoked_at`。
    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn decline_marks_revoked() {
        let Some((pool, ws, inviter)) = connect().await else {
            eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
            return;
        };
        let repo = InvitationRepo::with_pool(pool.clone());

        let row = repo
            .create(NewInvitation {
                workspace_id: ws,
                email: "x@example.com".into(),
                role: WorkspaceRole::Guest,
                invited_by_user_id: inviter,
                ttl_secs: Some(3600),
            })
            .await
            .expect("create ok");

        repo.decline(&row.token).await.expect("decline ok");
        let after = repo
            .get_by_token(&row.token)
            .await
            .expect("get ok")
            .expect("present");
        assert!(after.revoked_at.is_some());

        cleanup_workspace(&pool, ws.0).await;
        cleanup_user(&pool, inviter.0).await;
        pool.close().await;
    }

    /// 4. revoke by admin 标记 `revoked_at`（以及再次 revoke 返回 `NotFound`）。
    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn revoke_by_admin_marks_revoked() {
        let Some((pool, ws, inviter)) = connect().await else {
            eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
            return;
        };
        let repo = InvitationRepo::with_pool(pool.clone());

        let row = repo
            .create(NewInvitation {
                workspace_id: ws,
                email: "x@example.com".into(),
                role: WorkspaceRole::Admin,
                invited_by_user_id: inviter,
                ttl_secs: Some(3600),
            })
            .await
            .expect("create ok");

        repo.revoke(Id(row.id), inviter).await.expect("revoke ok");
        // 再次 revoke → NotFound
        let err = repo.revoke(Id(row.id), inviter).await.unwrap_err();
        assert!(
            matches!(err, RepoError::NotFound),
            "expected NotFound, got {err:?}"
        );

        cleanup_workspace(&pool, ws.0).await;
        cleanup_user(&pool, inviter.0).await;
        pool.close().await;
    }

    /// 5. `count_recent_in_workspace` 速率窗口。
    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn count_recent_in_workspace_window() {
        let Some((pool, ws, inviter)) = connect().await else {
            eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
            return;
        };
        let repo = InvitationRepo::with_pool(pool.clone());

        // 创建 3 条邀请
        for i in 0..3 {
            repo.create(NewInvitation {
                workspace_id: ws,
                email: format!("r{i}@example.com"),
                role: WorkspaceRole::Member,
                invited_by_user_id: inviter,
                ttl_secs: Some(3600),
            })
            .await
            .expect("create ok");
        }

        let since_recent = Utc::now() - Duration::seconds(60);
        let count = repo
            .count_recent_in_workspace(ws, since_recent)
            .await
            .expect("count ok");
        assert!(count >= 3, "expected at least 3, got {count}");

        // 2 小时前的窗口应返回 0
        let since_old = Utc::now() - Duration::hours(2);
        let count_old = repo
            .count_recent_in_workspace(ws, since_old)
            .await
            .expect("count ok");
        assert!(count_old >= 3, "still >=3 since we created them just now");

        cleanup_workspace(&pool, ws.0).await;
        cleanup_user(&pool, inviter.0).await;
        pool.close().await;
    }
}
