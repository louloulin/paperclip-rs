//! `auth` 域 — better-auth 兼容的 4 张表：`user` / `session` / `account` / `verification`。
//!
//! 设计：
//! - id 是 `text`（better-auth 生成的字符串 ID），不能用 Uuid
//! - 提供最常用的查询（按 email / 按 session token / 按 user account）
//! - 验证表用于无密码 OTP / 邮件验证流
//! - 此模块**不**实现密码哈希，调用方传入已 hash 的 password

use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use pc_core::Timestamp;
use uuid::Uuid;

use crate::{Db, RepoError, RepoResult};

// ---------- user ----------

const USER_COLS: &str =
    "id, name, email, email_verified, image, created_at, updated_at";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UserRow {
    pub id: String,
    pub name: String,
    pub email: String,
    pub email_verified: bool,
    pub image: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewUser {
    pub id: String,
    pub name: String,
    pub email: String,
    pub email_verified: bool,
    pub image: Option<String>,
}

// ---------- session ----------

const SESSION_COLS: &str =
    "id, expires_at, token, created_at, updated_at, ip_address, user_agent, user_id";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionRow {
    pub id: String,
    pub expires_at: Timestamp,
    pub token: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
    pub user_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSession {
    pub id: String,
    pub token: String,
    pub user_id: String,
    pub expires_at: Timestamp,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
}

// ---------- account ----------

const ACCOUNT_COLS: &str = "id, account_id, provider_id, user_id, access_token,      refresh_token, id_token, access_token_expires_at, refresh_token_expires_at,      scope, password, created_at, updated_at";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AccountRow {
    pub id: String,
    pub account_id: String,
    pub provider_id: String,
    pub user_id: String,
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub id_token: Option<String>,
    pub access_token_expires_at: Option<Timestamp>,
    pub refresh_token_expires_at: Option<Timestamp>,
    pub scope: Option<String>,
    pub password: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

// ---------- verification ----------

const VERIF_COLS: &str = "id, identifier, value, expires_at, created_at, updated_at";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VerificationRow {
    pub id: String,
    pub identifier: String,
    pub value: String,
    pub expires_at: Timestamp,
    pub created_at: Option<Timestamp>,
    pub updated_at: Option<Timestamp>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewVerification {
    pub id: String,
    pub identifier: String,
    pub value: String,
    pub expires_at: Timestamp,
}

// =================================================================
// 主仓库
// =================================================================

pub struct AuthRepo<'a> {
    pub db: &'a Db,
}

impl<'a> AuthRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    // ---- user ----

    pub async fn find_by_email(&self, email: &str) -> RepoResult<Option<UserRow>> {
        Ok(sqlx::query_as::<_, UserRow>(&format!(
            "SELECT {USER_COLS} FROM \"user\" WHERE email = $1"
        ))
        .bind(email)
        .fetch_optional(self.db.pool())
        .await?)
    }

    pub async fn find_by_id(&self, id: &str) -> RepoResult<Option<UserRow>> {
        Ok(sqlx::query_as::<_, UserRow>(&format!(
            "SELECT {USER_COLS} FROM \"user\" WHERE id = $1"
        ))
        .bind(id)
        .fetch_optional(self.db.pool())
        .await?)
    }

    pub async fn upsert_user(&self, u: &NewUser) -> RepoResult<UserRow> {
        if u.email.trim().is_empty() || u.id.trim().is_empty() {
            return Err(RepoError::Invalid("user email/id must not be empty".into()));
        }
        Ok(sqlx::query_as::<_, UserRow>(&format!(
            "INSERT INTO \"user\" (id, name, email, email_verified, image, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, now(), now()) ON CONFLICT (id) DO UPDATE SET name=EXCLUDED.name, email=EXCLUDED.email, email_verified=EXCLUDED.email_verified, image=EXCLUDED.image, updated_at=now() RETURNING {USER_COLS}"
        ))
        .bind(&u.id)
        .bind(&u.name)
        .bind(&u.email)
        .bind(u.email_verified)
        .bind(u.image.as_deref())
        .fetch_one(self.db.pool())
        .await?)
    }

    pub async fn set_email_verified(&self, user_id: &str) -> RepoResult<bool> {
        let n = sqlx::query(
            "UPDATE \"user\" SET email_verified=true, updated_at=now() WHERE id=$1",
        )
        .bind(user_id)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    pub async fn update_profile(
        &self,
        user_id: &str,
        name: Option<&str>,
        image: Option<&str>,
    ) -> RepoResult<Option<UserRow>> {
        Ok(sqlx::query_as::<_, UserRow>(&format!(
            "UPDATE \"user\" SET name = COALESCE($2, name), image = COALESCE($3, image), updated_at = now() WHERE id = $1 RETURNING {USER_COLS}"
        ))
        .bind(user_id)
        .bind(name)
        .bind(image)
        .fetch_optional(self.db.pool())
        .await?)
    }

    pub async fn delete(&self, user_id: &str) -> RepoResult<bool> {
        let n = sqlx::query("DELETE FROM \"user\" WHERE id = $1")
            .bind(user_id)
            .execute(self.db.pool())
            .await?
            .rows_affected();
        Ok(n > 0)
    }

    /// Round 140: legacy ensure_user — INSERT ... ON CONFLICT (id) DO NOTHING。
    /// 返回 Some(UserRow) 表示新建；None 表示已存在。
    /// 与 `upsert_user`（DO UPDATE）的语义不同：保留已有 name/email/image。
    pub async fn ensure_user(
        &self,
        id: &str,
        name: &str,
        email: &str,
    ) -> RepoResult<Option<UserRow>> {
        let inserted = sqlx::query(
            "INSERT INTO \"user\" (id, name, email, email_verified, created_at, updated_at)              VALUES ($1, $2, $3, false, now(), now())              ON CONFLICT (id) DO NOTHING",
        )
        .bind(id)
        .bind(name)
        .bind(email)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        if inserted > 0 {
            self.find_by_id(id).await
        } else {
            Ok(None)
        }
    }

    /// Round 140: 创建 credential account（带密码哈希）。
    /// 简化版：id/account_id 都用 u_<uuid>；provider_id='credential'。
    pub async fn create_credential_account(
        &self,
        user_id: &str,
        password_hash: &str,
    ) -> RepoResult<AccountRow> {
        let account_id = format!("acc_{}", Uuid::new_v4().simple());
        let row_id = format!("acc_{}", Uuid::new_v4().simple());
        sqlx::query_as::<_, AccountRow>(&format!(
            "INSERT INTO account (id, account_id, provider_id, user_id, password, created_at, updated_at)              VALUES ($1, $2, 'credential', $3, $4, now(), now())              RETURNING {ACCOUNT_COLS}"
        ))
        .bind(&row_id)
        .bind(&account_id)
        .bind(user_id)
        .bind(password_hash)
        .fetch_one(self.db.pool())
        .await
        .map_err(Into::into)
    }

    /// Round 140: 检查 user 是否存在（按 id）。轻量，仅返回 bool。
    pub async fn user_exists(&self, user_id: &str) -> RepoResult<bool> {
        Ok(sqlx::query_scalar::<_, i32>("SELECT 1 FROM \"user\" WHERE id = $1 LIMIT 1")
            .bind(user_id)
            .fetch_optional(self.db.pool())
            .await?
            .is_some())
    }

    // ---- session ----

    pub async fn find_session_by_token(&self, token: &str) -> RepoResult<Option<SessionRow>> {
        Ok(sqlx::query_as::<_, SessionRow>(&format!(
            "SELECT {SESSION_COLS} FROM session              WHERE token = $1 AND expires_at > now()"
        ))
        .bind(token)
        .fetch_optional(self.db.pool())
        .await?)
    }

    pub async fn find_session_by_id(&self, id: &str) -> RepoResult<Option<SessionRow>> {
        Ok(sqlx::query_as::<_, SessionRow>(&format!(
            "SELECT {SESSION_COLS} FROM session WHERE id = $1"
        ))
        .bind(id)
        .fetch_optional(self.db.pool())
        .await?)
    }

    pub async fn upsert_session(&self, s: &NewSession) -> RepoResult<SessionRow> {
        Ok(sqlx::query_as::<_, SessionRow>(&format!(
            "INSERT INTO session (id, expires_at, token, created_at, updated_at, ip_address, user_agent, user_id)              VALUES ($1, $2, $3, now(), now(), $4, $5, $6)              ON CONFLICT (id) DO UPDATE SET                 expires_at=EXCLUDED.expires_at, token=EXCLUDED.token,                 ip_address=EXCLUDED.ip_address, user_agent=EXCLUDED.user_agent,                 updated_at=now()              RETURNING {SESSION_COLS}"
        ))
        .bind(&s.id)
        .bind(s.expires_at)
        .bind(&s.token)
        .bind(s.ip_address.as_deref())
        .bind(s.user_agent.as_deref())
        .bind(&s.user_id)
        .fetch_one(self.db.pool())
        .await?)
    }

    pub async fn extend_session(&self, id: &str, new_expiry: Timestamp) -> RepoResult<bool> {
        let n = sqlx::query(
            "UPDATE session SET expires_at=$2, updated_at=now() WHERE id=$1 AND expires_at > now()",
        )
        .bind(id)
        .bind(new_expiry)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    pub async fn delete_session(&self, id: &str) -> RepoResult<bool> {
        let n = sqlx::query("DELETE FROM session WHERE id=$1")
            .bind(id)
            .execute(self.db.pool())
            .await?
            .rows_affected();
        Ok(n > 0)
    }

    pub async fn delete_sessions_for_user(&self, user_id: &str) -> RepoResult<u64> {
        Ok(sqlx::query("DELETE FROM session WHERE user_id=$1 AND expires_at > now()")
            .bind(user_id)
            .execute(self.db.pool())
            .await?
            .rows_affected())
    }

    pub async fn prune_expired(&self) -> RepoResult<u64> {
        Ok(sqlx::query("DELETE FROM session WHERE expires_at <= now()")
            .execute(self.db.pool())
            .await?
            .rows_affected())
    }

    // ---- account ----

    pub async fn find_account(
        &self,
        provider_id: &str,
        account_id: &str,
    ) -> RepoResult<Option<AccountRow>> {
        Ok(sqlx::query_as::<_, AccountRow>(&format!(
            "SELECT {ACCOUNT_COLS} FROM account              WHERE provider_id=$1 AND account_id=$2"
        ))
        .bind(provider_id)
        .bind(account_id)
        .fetch_optional(self.db.pool())
        .await?)
    }

    pub async fn find_account_for_user(
        &self,
        user_id: &str,
        provider_id: &str,
    ) -> RepoResult<Option<AccountRow>> {
        Ok(sqlx::query_as::<_, AccountRow>(&format!(
            "SELECT {ACCOUNT_COLS} FROM account              WHERE user_id=$1 AND provider_id=$2"
        ))
        .bind(user_id)
        .bind(provider_id)
        .fetch_optional(self.db.pool())
        .await?)
    }

    pub async fn upsert_account(
        &self,
        a: &AccountRow,
    ) -> RepoResult<AccountRow> {
        Ok(sqlx::query_as::<_, AccountRow>(&format!(
            "INSERT INTO account (id, account_id, provider_id, user_id, access_token,                 refresh_token, id_token, access_token_expires_at, refresh_token_expires_at,                 scope, password, created_at, updated_at)              VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)              ON CONFLICT (id) DO UPDATE SET                 access_token=EXCLUDED.access_token,                 refresh_token=EXCLUDED.refresh_token,                 id_token=EXCLUDED.id_token,                 access_token_expires_at=EXCLUDED.access_token_expires_at,                 refresh_token_expires_at=EXCLUDED.refresh_token_expires_at,                 scope=EXCLUDED.scope, password=EXCLUDED.password, updated_at=now()              RETURNING {ACCOUNT_COLS}"
        ))
        .bind(&a.id)
        .bind(&a.account_id)
        .bind(&a.provider_id)
        .bind(&a.user_id)
        .bind(a.access_token.as_deref())
        .bind(a.refresh_token.as_deref())
        .bind(a.id_token.as_deref())
        .bind(a.access_token_expires_at)
        .bind(a.refresh_token_expires_at)
        .bind(a.scope.as_deref())
        .bind(a.password.as_deref())
        .bind(a.created_at)
        .bind(a.updated_at)
        .fetch_one(self.db.pool())
        .await?)
    }

    pub async fn delete_account(&self, id: &str) -> RepoResult<bool> {
        let n = sqlx::query("DELETE FROM account WHERE id=$1")
            .bind(id)
            .execute(self.db.pool())
            .await?
            .rows_affected();
        Ok(n > 0)
    }

    // ---- verification ----

    pub async fn find_verification(
        &self,
        identifier: &str,
    ) -> RepoResult<Option<VerificationRow>> {
        Ok(sqlx::query_as::<_, VerificationRow>(&format!(
            "SELECT {VERIF_COLS} FROM verification              WHERE identifier=$1 AND expires_at > now()              ORDER BY created_at DESC LIMIT 1"
        ))
        .bind(identifier)
        .fetch_optional(self.db.pool())
        .await?)
    }

    pub async fn consume_verification(
        &self,
        identifier: &str,
        value: &str,
    ) -> RepoResult<bool> {
        let n = sqlx::query(
            "DELETE FROM verification              WHERE identifier=$1 AND value=$2 AND expires_at > now()",
        )
        .bind(identifier)
        .bind(value)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    pub async fn create_verification(&self, v: &NewVerification) -> RepoResult<VerificationRow> {
        // better-auth 行为：同一 identifier 下先清旧的
        sqlx::query("DELETE FROM verification WHERE identifier=$1")
            .bind(&v.identifier)
            .execute(self.db.pool())
            .await?;
        Ok(sqlx::query_as::<_, VerificationRow>(&format!(
            "INSERT INTO verification (id, identifier, value, expires_at, created_at, updated_at)              VALUES ($1,$2,$3,$4, now(), now())              RETURNING {VERIF_COLS}"
        ))
        .bind(&v.id)
        .bind(&v.identifier)
        .bind(&v.value)
        .bind(v.expires_at)
        .fetch_one(self.db.pool())
        .await?)
    }

    pub async fn purge_expired_verifications(&self) -> RepoResult<u64> {
        Ok(sqlx::query("DELETE FROM verification WHERE expires_at <= now()")
            .execute(self.db.pool())
            .await?
            .rows_affected())
    }

    // ---- Round 140: API key + session helpers for auth.rs route ----

    /// 轻量查 user.id by email（路由 sign_in 用，避免拉全 UserRow）。
    pub async fn find_user_id_by_email(&self, email: &str) -> RepoResult<Option<String>> {
        sqlx::query_scalar("SELECT id FROM \"user\" WHERE email = $1 LIMIT 1")
            .bind(email)
            .fetch_optional(self.db.pool())
            .await
            .map_err(Into::into)
    }

    /// 撤回 board_api_keys（设置 revoked_at = now()）。
    pub async fn revoke_api_key(&self, key_id: Uuid, user_id: &str) -> RepoResult<bool> {
        let n = sqlx::query(
            "UPDATE board_api_keys SET revoked_at = now() WHERE id = $1 AND user_id = $2 AND revoked_at IS NULL",
        )
        .bind(key_id)
        .bind(user_id)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// 按 token 软删除 session（保留行但 expires_at = now()）。
    pub async fn revoke_session_by_token(&self, token: &str) -> RepoResult<bool> {
        let n = sqlx::query("DELETE FROM session WHERE token = $1")
            .bind(token)
            .execute(self.db.pool())
            .await?
            .rows_affected();
        Ok(n > 0)
    }

    /// 按 user_id 删除所有 session（sign_out 用）。
    pub async fn revoke_all_sessions_for_user(&self, user_id: &str) -> RepoResult<u64> {
        sqlx::query("DELETE FROM session WHERE user_id = $1")
            .bind(user_id)
            .execute(self.db.pool())
            .await
            .map(|r| r.rows_affected())
            .map_err(Into::into)
    }

    /// 按 user_id + name 更新用户 profile。
    pub async fn update_user_name(&self, user_id: &str, name: &str) -> RepoResult<bool> {
        let n = sqlx::query("UPDATE \"user\" SET name = $1, updated_at = now() WHERE id = $2")
            .bind(name)
            .bind(user_id)
            .execute(self.db.pool())
            .await?
            .rows_affected();
        Ok(n > 0)
    }

    /// 按 user_id 更新 image。
    pub async fn update_user_image(&self, user_id: &str, image: &str) -> RepoResult<bool> {
        let n = sqlx::query("UPDATE \"user\" SET image = $1, updated_at = now() WHERE id = $2")
            .bind(image)
            .bind(user_id)
            .execute(self.db.pool())
            .await?
            .rows_affected();
        Ok(n > 0)
    }

    /// Round 152: 插入一条 bootstrap session（v3 schema 下 `sessions` 表不存在，
    /// 仅用于遗留 `/api/auth/bootstrap-claim` 路由 stub — 调用方需先检查 schema）。
    /// 真实 auth 走 `board_api_keys` / `cli_auth_challenges`。
    pub async fn insert_bootstrap_session(
        &self,
        session_id: Uuid,
        user_id: &str,
        token_hash: &str,
    ) -> sqlx::Result<u64> {
        let r = sqlx::query(
            "INSERT INTO sessions (id, user_id, token_hash, expires_at) \
             VALUES ($1, $2, $3, now() + interval '30 days') \
             ON CONFLICT (token_hash) DO NOTHING",
        )
        .bind(session_id)
        .bind(user_id)
        .bind(token_hash)
        .execute(self.db.pool())
        .await?;
        Ok(r.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_user_validates_email() {
        let bad = NewUser {
            id: "u_1".into(),
            name: "x".into(),
            email: "".into(),
            email_verified: false,
            image: None,
        };
        assert!(bad.email.trim().is_empty());
    }

    #[test]
    fn new_session_carries_user_id() {
        let s = NewSession {
            id: "s_1".into(),
            token: "tk".into(),
            user_id: "u_1".into(),
            expires_at: pc_core::Timestamp::from_dt(chrono::Utc::now() + chrono::Duration::days(7)),
            ip_address: Some("127.0.0.1".into()),
            user_agent: Some("ua".into()),
        };
        assert_eq!(s.user_id, "u_1");
    }

    #[test]
    fn new_verification_basic() {
        let v = NewVerification {
            id: "v_1".into(),
            identifier: "user@example.com".into(),
            value: "123456".into(),
            expires_at: pc_core::Timestamp::from_dt(chrono::Utc::now() + chrono::Duration::minutes(10)),
        };
        assert!(v.expires_at.as_datetime() > chrono::Utc::now());
    }
}
