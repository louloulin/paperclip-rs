//! Email verification token 生命周期。
//!
//! 对齐原 paperclip `auth/better-auth.ts` 中 email verification 流程：
//! 1. 用户注册 / 修改邮箱 → 调用 [`issue_email_verification`] 生成 token。
//! 2. 邮件中携带 raw token，用户点击链接提交。
//! 3. 服务端调用 [`verify_email_token`] 比对 hash + 过期时间 + 已用标记。
//!
//! 设计原则：
//! - raw token 仅在邮件中明文出现；服务端只保存 SHA-256 hash。
//! - token 默认 24h 过期，单次使用。
//! - 验证成功后由调用方把 `account.emailVerifiedAt` 写入持久层。

use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// 邮件验证 token 记录（持久化层视角）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailVerificationRecord {
    /// 用户 ID。
    pub user_id: String,
    /// 新邮箱地址。
    pub email: String,
    /// token 的 SHA-256 哈希。
    pub token_hash: String,
    /// 过期时间（UTC）。
    pub expires_at: DateTime<Utc>,
    /// 验证完成时间；`None` 表示未验证。
    pub consumed_at: Option<DateTime<Utc>>,
}

/// 验证结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmailVerificationOutcome {
    /// token 匹配且未过期未使用。
    Ok,
    /// token 不存在 / hash 不匹配。
    NotFound,
    /// token 过期。
    Expired,
    /// token 已被使用（replay 防御）。
    AlreadyConsumed,
}

impl EmailVerificationOutcome {
    #[must_use]
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok)
    }
}

fn hash_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    hex::encode(digest)
}

/// 生成邮件验证 token，返回 `(raw_token, record)`。
/// `raw_token` 应当通过邮件发送给用户；服务端仅持久化 `record`。
#[must_use]
pub fn issue_email_verification(
    user_id: impl Into<String>,
    email: impl Into<String>,
    ttl: Duration,
) -> (String, EmailVerificationRecord) {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let now = Utc::now();
    let record = EmailVerificationRecord {
        user_id: user_id.into(),
        email: email.into(),
        token_hash: hash_token(&raw),
        expires_at: now + ttl,
        consumed_at: None,
    };
    (raw, record)
}

/// 给定 raw token + 已持久化的 record，判定验证结果。
/// 不会改 record；调用方负责把 `consumed_at = now()` 写回。
#[must_use]
pub fn verify_email_token(raw: &str, record: &EmailVerificationRecord) -> EmailVerificationOutcome {
    if record.consumed_at.is_some() {
        return EmailVerificationOutcome::AlreadyConsumed;
    }
    if Utc::now() >= record.expires_at {
        return EmailVerificationOutcome::Expired;
    }
    if hash_token(raw) != record.token_hash {
        return EmailVerificationOutcome::NotFound;
    }
    EmailVerificationOutcome::Ok
}

/// 标记 record 为已使用（返回新的 record，调用方负责写回持久层）。
#[must_use]
pub fn consume_email_verification(mut record: EmailVerificationRecord) -> EmailVerificationRecord {
    if record.consumed_at.is_none() {
        record.consumed_at = Some(Utc::now());
    }
    record
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r565_issue_token_creates_unique_token() {
        let (a, _) = issue_email_verification("u1", "a@example.com", Duration::hours(24));
        let (b, _) = issue_email_verification("u1", "a@example.com", Duration::hours(24));
        assert_ne!(a, b);
        assert!(a.len() >= 32);
    }

    #[test]
    fn r565_verify_token_succeeds_when_fresh() {
        let (raw, rec) = issue_email_verification("u1", "a@example.com", Duration::hours(24));
        assert_eq!(verify_email_token(&raw, &rec), EmailVerificationOutcome::Ok);
    }

    #[test]
    fn r565_verify_token_fails_on_wrong_raw() {
        let (_raw, rec) = issue_email_verification("u1", "a@example.com", Duration::hours(24));
        assert_eq!(
            verify_email_token("nope", &rec),
            EmailVerificationOutcome::NotFound
        );
    }

    #[test]
    fn r565_verify_token_fails_when_expired() {
        let (raw, rec) = issue_email_verification("u1", "a@example.com", Duration::seconds(0));
        // 即使 TTL=0，issue 时 expires_at 仍是 now+0，立即过期
        let rec2 = EmailVerificationRecord {
            expires_at: Utc::now() - Duration::seconds(1),
            ..rec
        };
        assert_eq!(
            verify_email_token(&raw, &rec2),
            EmailVerificationOutcome::Expired
        );
    }

    #[test]
    fn r565_verify_token_replay_protection() {
        let (raw, rec) = issue_email_verification("u1", "a@example.com", Duration::hours(24));
        let rec = consume_email_verification(rec);
        assert_eq!(
            verify_email_token(&raw, &rec),
            EmailVerificationOutcome::AlreadyConsumed
        );
    }

    #[test]
    fn r565_consume_is_idempotent() {
        let (_raw, rec) = issue_email_verification("u1", "a@example.com", Duration::hours(24));
        let rec = consume_email_verification(rec);
        let first = rec.consumed_at.unwrap();
        let rec = consume_email_verification(rec);
        let second = rec.consumed_at.unwrap();
        assert_eq!(first, second, "consume must not advance consumed_at twice");
    }
}
