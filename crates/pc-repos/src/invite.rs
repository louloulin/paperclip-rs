//! `invites` 域 — 公司邀请令牌（member / agent）。
//!
//! Schema (paperclip `packages/db/src/schema/invites.ts`)：
//! - `invites(id, company_id, invite_type, token_hash, allowed_join_types,
//!   defaults_payload jsonb, expires_at, invited_by_user_id, revoked_at,
//!   accepted_at, created_at, updated_at)`
//! - 唯一索引 `invites_token_hash_unique_idx(token_hash)`
//! - 普通索引 `invites_company_invite_state_idx(company_id, invite_type, revoked_at, expires_at)`
//!
//! 与 Node 等价的行为：
//! - 创建时生成 256-bit 随机令牌，仅保存 SHA-256 哈希到 `token_hash`；明文一次性回返。
//! - 状态：pending / accepted / revoked / expired 由 "validity()" 计算。
//! - `defaults_payload.role` 不属于真实列 — 仅作为 JSON 透传（Node 历史约定）。

use chrono::{DateTime, Utc};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::{Db, RepoError, RepoResult};

const COLS: &str = "id, company_id, invite_type, allowed_join_types, defaults_payload, \
    token_hash, expires_at, invited_by_user_id, revoked_at, accepted_at, \
    created_at, updated_at";

/// 一条邀请行的可观察状态（不存于数据库，每次查询派生）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InviteStatus {
    Pending,
    Accepted,
    Revoked,
    Expired,
}

impl InviteStatus {
    fn from_row(revoked_at: Option<Timestamp>, accepted_at: Option<Timestamp>, expires_at: Timestamp, now: DateTime<Utc>) -> Self {
        if revoked_at.is_some() {
            InviteStatus::Revoked
        } else if accepted_at.is_some() {
            InviteStatus::Accepted
        } else if expires_at.as_datetime() <= now {
            InviteStatus::Expired
        } else {
            InviteStatus::Pending
        }
    }
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InviteRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub invite_type: String,
    pub allowed_join_types: String,
    #[serde(default)]
    pub defaults_payload: Option<JsonValue>,
    pub token_hash: String,
    pub expires_at: Timestamp,
    #[serde(default)]
    pub invited_by_user_id: Option<String>,
    #[serde(default)]
    pub revoked_at: Option<Timestamp>,
    #[serde(default)]
    pub accepted_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// 邀请 + 派生状态（不在数据库里）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InviteWithStatus {
    #[serde(flatten)]
    pub row: InviteRow,
    pub status: InviteStatus,
    /// 从 `defaults_payload` 提取的 role，默认 "member"。
    pub role: String,
}

/// 邀请令牌对：明文只在创建瞬间返回，之后只剩哈希。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedInvite {
    #[serde(flatten)]
    pub row: InviteRow,
    pub status: InviteStatus,
    pub role: String,
    /// 32+ 字节随机字符串的 URL-safe 形式，仅此一次。
    pub token: String,
}

#[derive(Debug, Clone)]
pub struct NewInvite {
    pub company_id: Uuid,
    pub invite_type: String,
    pub allowed_join_types: String,
    pub defaults_payload: Option<JsonValue>,
    pub expires_at: Timestamp,
    pub invited_by_user_id: Option<String>,
}

pub struct InviteRepo<'a> {
    pub db: &'a Db,
}

impl<'a> InviteRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    /// 列出公司最近 100 条邀请（与原 Node `companies.ts` 行为一致）。
    pub async fn list_by_company(&self, company_id: Uuid) -> RepoResult<Vec<InviteWithStatus>> {
        let sql = format!(
            "SELECT {COLS} FROM invites \
             WHERE company_id = $1 \
             ORDER BY created_at DESC LIMIT 100"
        );
        let rows: Vec<InviteRow> = sqlx::query_as(&sql)
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| decorate_with_status(r))
            .collect())
    }

    /// 通过 token 哈希查找未撤销的邀请（公开端口使用）。
    pub async fn find_active_by_token_hash(&self, token_hash: &str) -> RepoResult<Option<InviteRow>> {
        let row: Option<InviteRow> = sqlx::query_as(&format!(
            "SELECT {COLS} FROM invites \
             WHERE token_hash = $1 AND revoked_at IS NULL AND expires_at > now() LIMIT 1"
        ))
        .bind(token_hash)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row)
    }

    /// 通过 token 明文查找未撤销的邀请（公开端口使用）。
    pub async fn find_active_by_token(&self, raw_token: &str) -> RepoResult<Option<InviteRow>> {
        self.find_active_by_token_hash(&hash_token_hex(raw_token)).await
    }

    /// 通过 token 哈希查找邀请（不限定 active）。
    /// 用于像 `revoke_invite_by_token` 这样的场景，需要先检查 inviter 后再 revoke，
    /// 不论当前是否已过期。
    pub async fn find_by_token_hash(&self, token_hash: &str) -> RepoResult<Option<InviteRow>> {
        let row: Option<InviteRow> = sqlx::query_as(&format!(
            "SELECT {COLS} FROM invites WHERE token_hash = $1 LIMIT 1"
        ))
        .bind(token_hash)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row)
    }

    /// 创建一条邀请：生成 32 字节随机 token，返回明文 + 持久化的 row。
    pub async fn create(&self, input: NewInvite) -> RepoResult<CreatedInvite> {
        let id = Uuid::new_v4();
        let token = generate_url_safe_token(32);
        let token_hash = hash_token_hex(&token);
        let sql = format!(
            "INSERT INTO invites \
             (id, company_id, invite_type, allowed_join_types, defaults_payload, \
              token_hash, expires_at, invited_by_user_id, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8, now(), now())"
        );
        sqlx::query(&sql)
            .bind(id)
            .bind(input.company_id)
            .bind(&input.invite_type)
            .bind(&input.allowed_join_types)
            .bind(&input.defaults_payload)
            .bind(&token_hash)
            .bind(input.expires_at)
            .bind(&input.invited_by_user_id)
            .execute(self.db.pool())
            .await?;
        // 回读以确保 created_at / updated_at 与数据库同步返回。
        let row: InviteRow = sqlx::query_as(&format!(
            "SELECT {COLS} FROM invites WHERE id = $1"
        ))
        .bind(id)
        .fetch_one(self.db.pool())
        .await?;
        let now = Utc::now();
        Ok(CreatedInvite {
            row,
            status: InviteStatus::from_row(None, None, input.expires_at, now),
            role: extract_role(input.defaults_payload.as_ref()),
            token,
        })
    }

    /// 撤销邀请（原子：要求 company_id 匹配 + revoked_at IS NULL）。
    pub async fn revoke(&self, company_id: Uuid, invite_id: Uuid) -> RepoResult<bool> {
        let r = sqlx::query(
            "UPDATE invites SET revoked_at = now(), updated_at = now() \
             WHERE company_id = $1 AND id = $2 AND revoked_at IS NULL",
        )
        .bind(company_id)
        .bind(invite_id)
        .execute(self.db.pool())
        .await?;
        Ok(r.rows_affected() > 0)
    }

    /// 标记邀请已接受（accept 路径使用）。
    pub async fn mark_accepted(&self, invite_id: Uuid) -> RepoResult<()> {
        sqlx::query(
            "UPDATE invites SET accepted_at = now(), updated_at = now() \
             WHERE id = $1 AND accepted_at IS NULL",
        )
        .bind(invite_id)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }
}

// =====================================================================
// 辅助：从 defaults_payload.role 派生角色；为空时回退 "member"。
// =====================================================================

fn extract_role(defaults: Option<&JsonValue>) -> String {
    defaults
        .and_then(|v| v.get("role"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "member".to_string())
}

fn decorate_with_status(row: InviteRow) -> InviteWithStatus {
    let now = Utc::now();
    let status = InviteStatus::from_row(row.revoked_at, row.accepted_at, row.expires_at, now);
    let role = extract_role(row.defaults_payload.as_ref());
    InviteWithStatus { row, status, role }
}

// =====================================================================
// 公开辅助：对 raw token 计算 hex(SHA-256) 与生成 32 字节 URL-safe token。
// 故意暴露，便于上层 (pc-http) 在路由 handler 里复用同一种哈希。
// =====================================================================

/// SHA-256 hex digest of a token string. Mirrors `pc_auth::hash_token`.
pub fn hash_token_hex(raw: &str) -> String {
    let digest = Sha256::digest(raw.as_bytes());
    hex::encode(digest)
}

/// 32 字节熵 -> URL-safe base64 (no padding) — 与 Node `crypto.randomBytes(32).toString('base64url')` 等价。
pub fn generate_url_safe_token(byte_len: usize) -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    let mut buf = vec![0u8; byte_len];
    OsRng.fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(&buf)
}

// =====================================================================
// 单元测试：纯函数 — token 长度 / 哈希稳定性 / role 提取
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_token_hex_is_stable_and_64_chars() {
        let h1 = hash_token_hex("hello");
        let h2 = hash_token_hex("hello");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);
        assert!(h1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn hash_token_hex_differs_for_different_input() {
        assert_ne!(hash_token_hex("a"), hash_token_hex("b"));
    }

    #[test]
    fn generate_url_safe_token_has_expected_min_length_and_unique() {
        let t1 = generate_url_safe_token(32);
        let t2 = generate_url_safe_token(32);
        assert!(t1.len() >= 43, "32-byte base64url >= 43 chars; got {}", t1.len());
        assert!(t2.len() >= 43);
        assert_ne!(t1, t2, "two consecutive tokens must differ");
        assert!(t1.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn extract_role_defaults_to_member() {
        assert_eq!(extract_role(None), "member");
        assert_eq!(extract_role(Some(&JsonValue::Null)), "member");
        assert_eq!(
            extract_role(Some(&serde_json::json!({"role": "admin"}))),
            "admin"
        );
        assert_eq!(
            extract_role(Some(&serde_json::json!({"other": "x"}))),
            "member"
        );
    }

    #[test]
    fn invite_status_pending_vs_expired() -> anyhow::Result<()> {
        use pc_core::Timestamp as TS;
        let now = Utc::now();
        let future_ts = TS::from_dt(now + chrono::Duration::days(1));
        let past_ts = TS::from_dt(now - chrono::Duration::days(1));
        let future: TS = future_ts;
        let past: TS = past_ts;
        assert_eq!(InviteStatus::from_row(None, None, future, now), InviteStatus::Pending);
        assert_eq!(InviteStatus::from_row(None, None, past, now), InviteStatus::Expired);
        assert_eq!(
            InviteStatus::from_row(Some(TS::from_dt(now)), None, future, now),
            InviteStatus::Revoked
        );
        assert_eq!(
            InviteStatus::from_row(None, Some(TS::from_dt(now)), future, now),
            InviteStatus::Accepted
        );
        Ok(())
    }
}
