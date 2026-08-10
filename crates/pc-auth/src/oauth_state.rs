//! OAuth 2.0 state / PKCE 校验骨架。
//!
//! 对齐 Node `auth/better-auth.ts` + `routes/tool-access.ts` 中 OAuth 流程：
//! 1. start：生成 `state` + PKCE `code_verifier`，计算 `code_challenge` (S256)。
//! 2. 把 (state, code_verifier) 持久化到会话 / cookie。
//! 3. provider 重定向到 callback 带 `state` + `code`。
//! 4. callback：从持久化层取出 code_verifier，用相同 state 比对，
//!    再用 code + code_verifier 兑换 access_token。
//!
//! 当前模块只覆盖纯算法部分（生成、签名、校验）；HTTP 兑换留给调用方。

use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// OAuth state 记录（持久化层视角）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthStateRecord {
    /// 随机 state 值（base64url，32 字节）。
    pub state: String,
    /// PKCE code_verifier（43-128 字符，base64url）。
    pub code_verifier: String,
    /// 关联 redirect_uri（用于回调校验）。
    pub redirect_uri: String,
    /// 关联 provider 标识。
    pub provider: String,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OAuthStateOutcome {
    Ok { code_verifier: String, redirect_uri: String, provider: String },
    Expired,
    StateMismatch,
    MissingState,
}

impl OAuthStateOutcome {
    #[must_use]
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok { .. })
    }
}

/// 生成新的 OAuth state + PKCE verifier。
#[must_use]
pub fn new_oauth_state(
    provider: impl Into<String>,
    redirect_uri: impl Into<String>,
    ttl: Duration,
) -> (String, OAuthStateRecord) {
    // 32 bytes base64url
    let mut state_bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut state_bytes);
    let state = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(state_bytes);

    // 64 bytes base64url for code_verifier (RFC 7636: 43..=128 chars)
    let mut verifier_bytes = [0u8; 64];
    rand::thread_rng().fill_bytes(&mut verifier_bytes);
    let code_verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(verifier_bytes);

    let now = Utc::now();
    let record = OAuthStateRecord {
        state: state.clone(),
        code_verifier: code_verifier.clone(),
        redirect_uri: redirect_uri.into(),
        provider: provider.into(),
        issued_at: now,
        expires_at: now + ttl,
    };
    (state, record)
}

/// 计算 PKCE S256 code_challenge（OAuth provider 用）。
#[must_use]
pub fn code_challenge_s256(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

/// 校验 callback 带回的 state。
/// 不修改 record；调用方负责消费后删除。
#[must_use]
pub fn verify_oauth_state(
    provided_state: &str,
    record: &OAuthStateRecord,
    now: DateTime<Utc>,
) -> OAuthStateOutcome {
    if provided_state.is_empty() {
        return OAuthStateOutcome::MissingState;
    }
    if now >= record.expires_at {
        return OAuthStateOutcome::Expired;
    }
    // constant-time 比对（state 长度固定，但保持一致）
    if !constant_time_eq(provided_state.as_bytes(), record.state.as_bytes()) {
        return OAuthStateOutcome::StateMismatch;
    }
    OAuthStateOutcome::Ok {
        code_verifier: record.code_verifier.clone(),
        redirect_uri: record.redirect_uri.clone(),
        provider: record.provider.clone(),
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r565_new_oauth_state_has_distinct_verifier_and_state() {
        let (s1, r1) = new_oauth_state("github", "https://app/cb", Duration::minutes(10));
        let (s2, r2) = new_oauth_state("github", "https://app/cb", Duration::minutes(10));
        assert_ne!(s1, s2);
        assert_ne!(r1.code_verifier, r2.code_verifier);
        assert_eq!(s1.len(), 43); // 32 bytes base64url no pad
        assert!(r1.code_verifier.len() >= 43 && r1.code_verifier.len() <= 128);
    }

    #[test]
    fn r565_code_challenge_is_sha256_of_verifier() {
        let (_, r) = new_oauth_state("p", "cb", Duration::minutes(1));
        let challenge = code_challenge_s256(&r.code_verifier);
        // base64url no-pad, 32 bytes input -> 43 chars
        assert_eq!(challenge.len(), 43);
        // 重新计算应一致
        let again = code_challenge_s256(&r.code_verifier);
        assert_eq!(challenge, again);
    }

    #[test]
    fn r565_verify_oauth_state_succeeds_for_fresh_match() {
        let (s, r) = new_oauth_state("p", "https://app/cb", Duration::minutes(10));
        let outcome = verify_oauth_state(&s, &r, Utc::now());
        match outcome {
            OAuthStateOutcome::Ok { provider, redirect_uri, .. } => {
                assert_eq!(provider, "p");
                assert_eq!(redirect_uri, "https://app/cb");
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn r565_verify_oauth_state_mismatch_on_wrong_state() {
        let (_, r) = new_oauth_state("p", "cb", Duration::minutes(10));
        assert_eq!(
            verify_oauth_state("evil", &r, Utc::now()),
            OAuthStateOutcome::StateMismatch
        );
    }

    #[test]
    fn r565_verify_oauth_state_rejects_empty() {
        let (_, r) = new_oauth_state("p", "cb", Duration::minutes(10));
        assert_eq!(
            verify_oauth_state("", &r, Utc::now()),
            OAuthStateOutcome::MissingState
        );
    }

    #[test]
    fn r565_verify_oauth_state_expires() {
        let (s, mut r) = new_oauth_state("p", "cb", Duration::seconds(0));
        r.expires_at = Utc::now() - Duration::seconds(1);
        assert_eq!(verify_oauth_state(&s, &r, Utc::now()), OAuthStateOutcome::Expired);
    }
}
