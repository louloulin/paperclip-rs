//! `InvitationRepo` 的全部 13 个仓储方法 + 两个私有错误映射 helper。
//!
//! 从 `src/invitation.rs` 拆出（门 ⑩ 第 8 批）。**0 行为变更**：
//! 父模块用 `pub use` 把符号原样重导出，`mc_repos::invitation::*` 路径逐字不变。

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use rand::RngCore;
use sqlx::PgPool;
use tracing::warn;
use uuid::Uuid;

use mc_core::id::Id;
use mc_core::member::WorkspaceMember;
use mc_core::workspace::WorkspaceRole;
use mc_db::Db;

use crate::invitation::{
    AcceptOutcome, InvitationRepo, InvitationRow, NewInvitation, INVITATION_DEFAULT_TTL_DAYS,
    INVITATION_TOKEN_BYTES,
};
use crate::{RepoError, Result};

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
