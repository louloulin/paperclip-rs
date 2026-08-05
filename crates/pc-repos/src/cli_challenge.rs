//! `cli_auth_challenges` 域 — CLI / Board 短时 challenge token 持久化。
//!
//! 设计：
//! - 用户在 CLI 发起登录请求时，board 端创建一条 challenge（含 secret + pending board api key）
//! - 用户在浏览器端 approve（写入 `approved_by_user_id` + `approved_at`）
//! - CLI 端轮询直到看见 approved_at，然后拿 `pending_key_hash` 对应明文换 board api key
//! - cancel 用于超时或主动撤销（写 `cancelled_at`）
//!
//! Round 149: 从 `routes/access.rs` 抽出 SQL，提供仓储方法。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::{Db, RepoError, RepoResult};

/// 单条 CLI 授权挑战的完整 DB 行投影（1:1 映射 `cli_auth_challenges`）。
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct ChallengeRow {
    pub id: Uuid,
    pub secret_hash: String,
    pub command: String,
    pub client_name: Option<String>,
    pub requested_access: String,
    pub requested_company_id: Option<Uuid>,
    pub pending_key_hash: String,
    pub pending_key_name: String,
    pub approved_by_user_id: Option<String>,
    pub approved_at: Option<Timestamp>,
    pub cancelled_at: Option<Timestamp>,
    pub expires_at: Timestamp,
    pub created_at: Timestamp,
}

/// Round 149: 创建 challenge（一次性写入并返回行）。
/// 字段语义参见 `ChallengeRow`。
#[allow(clippy::too_many_arguments)]
pub async fn create(
    db: &Db,
    secret_hash: &str,
    command: &str,
    client_name: Option<&str>,
    requested_access: &str,
    requested_company_id: Option<Uuid>,
    pending_key_hash: &str,
    pending_key_name: &str,
    expires_at: Timestamp,
) -> RepoResult<ChallengeRow> {
    let row: ChallengeRow = sqlx::query_as(
        "INSERT INTO cli_auth_challenges             (secret_hash, command, client_name, requested_access, requested_company_id,              pending_key_hash, pending_key_name, expires_at)          VALUES ($1, $2, $3, $4, $5, $6, $7, $8)          RETURNING id, secret_hash, command, client_name, requested_access, requested_company_id,                    pending_key_hash, pending_key_name, approved_by_user_id, approved_at,                    cancelled_at, expires_at, created_at",
    )
    .bind(secret_hash)
    .bind(command)
    .bind(client_name)
    .bind(requested_access)
    .bind(requested_company_id)
    .bind(pending_key_hash)
    .bind(pending_key_name)
    .bind(expires_at)
    .fetch_one(db.pool())
    .await?;
    Ok(row)
}

/// Round 149: 按 id 查找 challenge（CLI 轮询路径）。
pub async fn find_by_id(db: &Db, id: Uuid) -> RepoResult<Option<ChallengeRow>> {
    let row: Option<ChallengeRow> = sqlx::query_as(
        "SELECT id, secret_hash, command, client_name, requested_access, requested_company_id,                 pending_key_hash, pending_key_name, approved_by_user_id, approved_at,                 cancelled_at, expires_at, created_at          FROM cli_auth_challenges WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(db.pool())
    .await?;
    Ok(row)
}

/// Round 149: 标记 challenge 为 approved（设置 approved_by_user_id + approved_at）。
pub async fn approve(db: &Db, id: Uuid, user_id: &str) -> RepoResult<ChallengeRow> {
    let row: ChallengeRow = sqlx::query_as(
        "UPDATE cli_auth_challenges SET             approved_by_user_id = $2, approved_at = now(), updated_at = now()          WHERE id = $1          RETURNING id, secret_hash, command, client_name, requested_access, requested_company_id,                    pending_key_hash, pending_key_name, approved_by_user_id, approved_at,                    cancelled_at, expires_at, created_at",
    )
    .bind(id)
    .bind(user_id)
    .fetch_one(db.pool())
    .await?;
    Ok(row)
}

/// Round 149: 标记 challenge 为 cancelled（写 cancelled_at，不动 approved_* 字段）。
pub async fn cancel(db: &Db, id: Uuid) -> RepoResult<ChallengeRow> {
    let row: ChallengeRow = sqlx::query_as(
        "UPDATE cli_auth_challenges SET cancelled_at = now(), updated_at = now()          WHERE id = $1          RETURNING id, secret_hash, command, client_name, requested_access, requested_company_id,                    pending_key_hash, pending_key_name, approved_by_user_id, approved_at,                    cancelled_at, expires_at, created_at",
    )
    .bind(id)
    .fetch_one(db.pool())
    .await?;
    Ok(row)
}

/// Repository handle — wrap `Db` so callers can use the OOP-style API.
#[derive(Clone)]
pub struct ChallengeRepo<'a> {
    db: &'a Db,
}

impl<'a> ChallengeRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    pub async fn create(
        &self,
        secret_hash: &str,
        command: &str,
        client_name: Option<&str>,
        requested_access: &str,
        requested_company_id: Option<Uuid>,
        pending_key_hash: &str,
        pending_key_name: &str,
        expires_at: Timestamp,
    ) -> RepoResult<ChallengeRow> {
        create(
            self.db,
            secret_hash,
            command,
            client_name,
            requested_access,
            requested_company_id,
            pending_key_hash,
            pending_key_name,
            expires_at,
        )
        .await
    }

    pub async fn find_by_id(&self, id: Uuid) -> RepoResult<Option<ChallengeRow>> {
        find_by_id(self.db, id).await
    }

    pub async fn approve(&self, id: Uuid, user_id: &str) -> RepoResult<ChallengeRow> {
        approve(self.db, id, user_id).await
    }

    pub async fn cancel(&self, id: Uuid) -> RepoResult<ChallengeRow> {
        cancel(self.db, id).await
    }
}
