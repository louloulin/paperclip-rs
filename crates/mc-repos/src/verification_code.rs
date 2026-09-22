//! `VerificationCodeRepo` — DB-backed verification code 仓储。
//!
//! 对应 upstream `multica/server/pkg/db/queries/verification_code.sql` 的核心子集：
//! - `create`        — 写入 `verification_code` 表
//! - `consume`       — 原子化校验 + 标记 consumed_at（避免重放）
//! - `prune_expired` — 清掉过期记录
//! - `recent_for`    — 速率限制窗口计数
//!
//! 字段差异：上游用 `used BOOLEAN`，本 schema 用 `consumed_at TIMESTAMPTZ`，
//! 语义等价，但需要在已消费时返回 `Ok(None)` 而非抛错。
//!
//! sqlx 查询采用 **运行时版本**（`sqlx::query` / `sqlx::query_as`），
//! 不依赖编译期 `SQL_FILE` 数据库连接 —— 测试可在不带 `DATABASE_URL` 的
//! CI 环境中编译，但运行需要 PostgreSQL。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

use mc_auth::VerificationCodePurpose;
use mc_core::Id;
use mc_db::Db;

use crate::{RepoError, Result};

/// 新增验证码的入参（handler 侧负责 hash + TTL 计算）。
#[derive(Debug, Clone)]
pub struct NewVerificationCode {
    pub email: Option<String>,
    pub user_id: Option<Id>,
    pub purpose: VerificationCodePurpose,
    /// hex(sha256(code))
    pub code_hash: String,
    pub expires_at: DateTime<Utc>,
}

/// 单行 verification_code 视图。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationRow {
    pub id: Id,
    pub user_id: Option<Id>,
    pub email: Option<String>,
    pub purpose: String,
    pub code_hash: String,
    pub attempts: i32,
    pub expires_at: DateTime<Utc>,
    pub consumed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl VerificationRow {
    fn purpose_str(p: VerificationCodePurpose) -> &'static str {
        match p {
            VerificationCodePurpose::EmailVerification => "email_verification",
            VerificationCodePurpose::PasswordReset => "password_reset",
            VerificationCodePurpose::TwoFactor => "two_factor",
            VerificationCodePurpose::WorkspaceInvite => "workspace_invite",
        }
    }
}

impl From<&VerificationRow> for VerificationCodePurpose {
    fn from(row: &VerificationRow) -> Self {
        match row.purpose.as_str() {
            "password_reset" => VerificationCodePurpose::PasswordReset,
            "two_factor" => VerificationCodePurpose::TwoFactor,
            "workspace_invite" => VerificationCodePurpose::WorkspaceInvite,
            _ => VerificationCodePurpose::EmailVerification,
        }
    }
}

/// DB-backed verification code repository。
#[derive(Clone)]
pub struct VerificationCodeRepo {
    pool: Arc<PgPool>,
}

impl VerificationCodeRepo {
    pub fn new(db: Db) -> Self {
        Self { pool: Arc::new(db.pool().clone()) }
    }

    /// 在共享 `sqlx::PgPool` 上持有引用（与 `new(db)` 等价，但避免所有权转移）。
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool: Arc::new(pool) }
    }

    fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// 写入新验证码。`reason_hash` 应由调用方预先计算。
    pub async fn create(&self, input: NewVerificationCode) -> Result<VerificationRow> {
        let id = Uuid::new_v4();
        let purpose = VerificationRow::purpose_str(input.purpose);
        let row = sqlx::query_as::<_, VerificationRow>(
            r#"
            INSERT INTO verification_code
                (id, user_id, email, purpose, code_hash, attempts, expires_at, created_at)
            VALUES ($1, $2, $3, $4, $5, 0, $6, now())
            RETURNING id, user_id, email, purpose, code_hash, attempts,
                      expires_at, consumed_at, created_at
            "#,
        )
        .bind(id)
        .bind(input.user_id.map(|u| u.as_uuid()))
        .bind(input.email.as_deref())
        .bind(purpose)
        .bind(&input.code_hash)
        .bind(input.expires_at)
        .fetch_one(self.pool())
        .await
        .map_err(map_sqlx)?;

        Ok(row)
    }

    /// 校验并原子消费：找到同 email + purpose 最新一条未消费、未过期、attempts < 5
    /// 且 `code_hash` 匹配的行，标记 `consumed_at = now()` 并返回。
    ///
    /// 已消费 → `Ok(None)`；过期 / 不存在 / 不匹配 → `Ok(None)`。
    ///
    /// 标记 `consumed_at` 在单条 SQL 内完成，避免 verify 后的 TOCTOU 重放。
    pub async fn consume(&self, code_hash: &str, purpose: VerificationCodePurpose) -> Result<Option<VerificationRow>> {
        let purpose_str = VerificationRow::purpose_str(purpose);

        // 1) 找出最近一条匹配 hash + purpose、未消费、未过期的行
        let row = sqlx::query_as::<_, VerificationRow>(
            r#"
            SELECT id, user_id, email, purpose, code_hash, attempts,
                   expires_at, consumed_at, created_at
            FROM verification_code
            WHERE code_hash = $1
              AND purpose = $2
              AND consumed_at IS NULL
              AND expires_at > now()
              AND attempts < 5
            ORDER BY created_at DESC
            LIMIT 1
            "#,
        )
        .bind(code_hash)
        .bind(purpose_str)
        .fetch_optional(self.pool())
        .await
        .map_err(map_sqlx)?;

        let Some(row) = row else {
            return Ok(None);
        };

        // 2) 原子标记 consumed_at；若已被另一进程抢先消费则返回 None
        let res = sqlx::query(
            r#"
            UPDATE verification_code
            SET consumed_at = now()
            WHERE id = $1 AND consumed_at IS NULL
            "#,
        )
        .bind(row.id.as_uuid())
        .execute(self.pool())
        .await
        .map_err(map_sqlx)?;

        if res.rows_affected() == 0 {
            return Ok(None);
        }

        // 3) 返回最新视图（含 consumed_at）
        let consumed = sqlx::query_as::<_, VerificationRow>(
            r#"
            SELECT id, user_id, email, purpose, code_hash, attempts,
                   expires_at, consumed_at, created_at
            FROM verification_code
            WHERE id = $1
            "#,
        )
        .bind(row.id.as_uuid())
        .fetch_one(self.pool())
        .await
        .map_err(map_sqlx)?;

        Ok(Some(consumed))
    }

    /// 增加 `attempts` 计数（在 verify 失败时 handler 调用，避免暴力枚举）。
    pub async fn increment_attempts(&self, id: Id) -> Result<i32> {
        let row = sqlx::query_as::<_, (i32,)>(
            r#"
            UPDATE verification_code
            SET attempts = attempts + 1
            WHERE id = $1 AND consumed_at IS NULL
            RETURNING attempts
            "#,
        )
        .bind(id.as_uuid())
        .fetch_optional(self.pool())
        .await
        .map_err(map_sqlx)?;

        Ok(row.map(|(n,)| n).unwrap_or(0))
    }

    /// 清理过期验证码（保守：仅清理 1 小时前已过期的，留出时钟偏移）。
    pub async fn prune_expired(&self) -> Result<u64> {
        let res = sqlx::query(
            r#"
            DELETE FROM verification_code
            WHERE expires_at < now() - interval '1 hour'
            "#,
        )
        .execute(self.pool())
        .await
        .map_err(map_sqlx)?;

        Ok(res.rows_affected())
    }

    /// 速率限制：返回 `(now - window_secs)` 之内该 email 申请过的验证码数量。
    pub async fn recent_for(&self, email: &str, window_secs: i64) -> Result<i64> {
        let row = sqlx::query_as::<_, (Option<i64>,)>(
            r#"
            SELECT COUNT(*)::BIGINT
            FROM verification_code
            WHERE email = $1
              AND created_at > now() - make_interval(secs => $2)
            "#,
        )
        .bind(email)
        .bind(window_secs as f64)
        .fetch_one(self.pool())
        .await
        .map_err(map_sqlx)?;

        Ok(row.0.unwrap_or(0))
    }

    /// 取某 email 最新一条未消费的验证码（仅调试 / 测试用；生产路径走 `consume`）。
    #[allow(dead_code)]
    pub async fn latest_active_for(
        &self,
        email: &str,
        purpose: VerificationCodePurpose,
    ) -> Result<Option<VerificationRow>> {
        let purpose_str = VerificationRow::purpose_str(purpose);
        let row = sqlx::query_as::<_, VerificationRow>(
            r#"
            SELECT id, user_id, email, purpose, code_hash, attempts,
                   expires_at, consumed_at, created_at
            FROM verification_code
            WHERE email = $1
              AND purpose = $2
              AND consumed_at IS NULL
              AND expires_at > now()
            ORDER BY created_at DESC
            LIMIT 1
            "#,
        )
        .bind(email)
        .bind(purpose_str)
        .fetch_optional(self.pool())
        .await
        .map_err(map_sqlx)?;

        Ok(row)
    }
}

fn map_sqlx(e: sqlx::Error) -> RepoError {
    RepoError::Db(e.to_string())
}

// =================== 测试 ===================
//
// 仓库测试需要 PostgreSQL（DATABASE_URL 环境变量）以及已执行的迁移。
// 没有数据库时优雅跳过，避免在无 DB 的 CI runner 上失败。

#[cfg(test)]
mod tests {
    use super::*;
    use mc_auth::VerificationCodePurpose;
    use sha2::{Digest, Sha256};

    async fn try_db() -> Option<(Db, PgPool)> {
        let url = std::env::var("DATABASE_URL").ok()?;
        let db = Db::connect(&url, 4, 1).await.ok()?;
        let pool = db.pool().clone();
        Some((db, pool))
    }

    fn hash_code(code: &str) -> String {
        let mut h = Sha256::new();
        h.update(code.as_bytes());
        hex::encode(h.finalize())
    }

    fn unique_email(prefix: &str) -> String {
        format!("{}-{}@example.test", prefix, Uuid::new_v4())
    }

    #[tokio::test]
    async fn create_and_get_round_trip() {
        let Some((db, _pool)) = try_db().await else {
            eprintln!("DATABASE_URL not set; skipping");
            return;
        };
        let repo = VerificationCodeRepo::new(db);
        let email = unique_email("round-trip");
        let hash = hash_code("123456");
        let row = repo
            .create(NewVerificationCode {
                email: Some(email.clone()),
                user_id: None,
                purpose: VerificationCodePurpose::EmailVerification,
                code_hash: hash.clone(),
                expires_at: Utc::now() + chrono::Duration::seconds(60),
            })
            .await
            .expect("create");
        assert_eq!(row.email.as_deref(), Some(email.as_str()));
        assert_eq!(row.code_hash, hash);
        assert!(row.consumed_at.is_none());
    }

    #[tokio::test]
    async fn consume_is_idempotent() {
        let Some((db, _pool)) = try_db().await else {
            return;
        };
        let repo = VerificationCodeRepo::new(db);
        let hash = hash_code(&format!("c-{}", Uuid::new_v4()));
        repo.create(NewVerificationCode {
            email: None,
            user_id: None,
            purpose: VerificationCodePurpose::EmailVerification,
            code_hash: hash.clone(),
            expires_at: Utc::now() + chrono::Duration::seconds(60),
        })
        .await
        .expect("create");

        let first = repo
            .consume(&hash, VerificationCodePurpose::EmailVerification)
            .await
            .expect("consume");
        assert!(first.is_some(), "first consume should succeed");
        assert!(first.unwrap().consumed_at.is_some());

        let second = repo
            .consume(&hash, VerificationCodePurpose::EmailVerification)
            .await
            .expect("second consume");
        assert!(second.is_none(), "second consume must be None");
    }

    #[tokio::test]
    async fn expired_code_cannot_be_consumed() {
        let Some((db, _pool)) = try_db().await else {
            return;
        };
        let repo = VerificationCodeRepo::new(db);
        let hash = hash_code(&format!("c-{}", Uuid::new_v4()));
        // 直接插一条已过期的行（绕过 create 的 TTL 检查）
        sqlx::query(
            r#"
            INSERT INTO verification_code (id, email, purpose, code_hash, expires_at, created_at)
            VALUES ($1, $2, 'email_verification', $3, now() - interval '1 hour', now() - interval '2 hours')
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(unique_email("expired"))
        .bind(&hash)
        .execute(repo.pool())
        .await
        .expect("insert expired");

        let res = repo
            .consume(&hash, VerificationCodePurpose::EmailVerification)
            .await
            .expect("consume");
        assert!(res.is_none(), "expired code must not be consumable");
    }

    #[tokio::test]
    async fn recent_for_counts_window() {
        let Some((db, _pool)) = try_db().await else {
            return;
        };
        let repo = VerificationCodeRepo::new(db);
        let email = unique_email("rate");
        for n in 0..3 {
            let _ = repo
                .create(NewVerificationCode {
                    email: Some(email.clone()),
                    user_id: None,
                    purpose: VerificationCodePurpose::EmailVerification,
                    code_hash: hash_code(&format!("{email}-{n}")),
                    expires_at: Utc::now() + chrono::Duration::seconds(60),
                })
                .await;
        }
        let count = repo.recent_for(&email, 60).await.expect("count");
        assert_eq!(count, 3, "should count 3 codes in 60s window");
    }

    #[tokio::test]
    async fn prune_expired_removes_old_rows() {
        let Some((db, _pool)) = try_db().await else {
            return;
        };
        let repo = VerificationCodeRepo::new(db);
        // 插一条 2 小时前过期的
        sqlx::query(
            r#"
            INSERT INTO verification_code (id, email, purpose, code_hash, expires_at, created_at)
            VALUES ($1, 'prune@example.test', 'email_verification', $2,
                    now() - interval '2 hours', now() - interval '3 hours')
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(hash_code("prune-1"))
        .execute(repo.pool())
        .await
        .expect("insert");
        let removed = repo.prune_expired().await.expect("prune");
        assert!(removed >= 1, "should remove at least one expired row");
    }
}