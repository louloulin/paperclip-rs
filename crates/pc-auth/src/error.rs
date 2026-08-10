//! Typed 认证错误分类。
//!
//! 与 `pc-secrets::error` 思路一致：把现有 `AuthError` 之上再分一层
//! category，方便上层做重试 / 提示文案分流 / 监控埋点。
//!
//! 现状：原 `AuthError` 是 `thiserror` enum，覆盖面有限（missing / invalid
//! / expired / db / hash）。新增 `AuthErrorCategory` 不破坏 `AuthError`，
//! 只在需要时由调用方基于字符串 / 错误体走 `classify()`。

use crate::AuthError;

/// 错误分类。供上层（路由、middleware、日志聚合）分流。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuthErrorCategory {
    /// 缺少凭证（未带 Authorization / cookie）—— 让用户登录。
    MissingCredentials,
    /// 凭证格式错误（malformed token / 字段缺失）—— 不可重试。
    InvalidFormat,
    /// 凭证找不到对应会话 / user —— 不可重试，让用户重新登录。
    NotFound,
    /// 凭证过期（session / token / email verification）—— 客户端可重试。
    Expired,
    /// 凭证被撤销 / 主动登出 —— 不可重试。
    Revoked,
    /// 凭证被回放（已使用过）—— 不可重试。
    Replayed,
    /// 邮箱尚未验证 —— 不可直接重试，提示用户先验证。
    EmailNotVerified,
    /// 邮箱验证 token 错误 / 过期 / 重复使用。
    EmailVerificationFailed,
    /// OAuth state / PKCE 不匹配。
    OAuthStateMismatch,
    /// OAuth provider 错误（4xx / 5xx）—— 多数不可重试。
    OAuthProviderError,
    /// argon2 / sha256 等底层 hash 失败 —— 系统问题，不可重试。
    Hash,
    /// 数据库错误 —— 通常不可重试（除非 transient）。
    Database,
    /// 其它未知 —— 保守不可重试。
    Other,
}

impl AuthErrorCategory {
    /// 是否可短暂重试（refresh / 重新登录）。当前仅 `Expired` 属于可重试。
    /// `Database` 在 SQL transient 错误下也可重试，调用方在分类时可上调。
    #[must_use]
    pub fn is_transient(self) -> bool {
        matches!(self, Self::Expired)
    }
}

/// Auth error 分类辅助。
pub fn classify(err: &AuthError) -> AuthErrorCategory {
    match err {
        AuthError::MissingCredentials => AuthErrorCategory::MissingCredentials,
        AuthError::InvalidToken => AuthErrorCategory::NotFound,
        AuthError::Expired => AuthErrorCategory::Expired,
        AuthError::Db(_) => AuthErrorCategory::Database,
        AuthError::Hash(_) => AuthErrorCategory::Hash,
    }
}

/// 从错误字符串启发式分类（用于把上游字符串错误归类）。
/// 与 `pc-secrets::SecretProviderError::classify` 思路一致。
#[must_use]
pub fn classify_str(message: &str) -> AuthErrorCategory {
    let lower = message.to_lowercase();
    if lower.contains("missing credentials") || lower.contains("missing token") {
        return AuthErrorCategory::MissingCredentials;
    }
    if lower.contains("expired") || lower.contains("expire") {
        return AuthErrorCategory::Expired;
    }
    if lower.contains("replayed") || lower.contains("already used") {
        return AuthErrorCategory::Replayed;
    }
    if lower.contains("revoked") || lower.contains("revoke") {
        return AuthErrorCategory::Revoked;
    }
    if lower.contains("email not verified") {
        return AuthErrorCategory::EmailNotVerified;
    }
    if lower.contains("verification") {
        return AuthErrorCategory::EmailVerificationFailed;
    }
    if lower.contains("oauth") {
        if lower.contains("state") || lower.contains("pkce") {
            return AuthErrorCategory::OAuthStateMismatch;
        }
        return AuthErrorCategory::OAuthProviderError;
    }
    if lower.contains("invalid") || lower.contains("malformed") {
        return AuthErrorCategory::InvalidFormat;
    }
    if lower.contains("not found") || lower.contains("no such") {
        return AuthErrorCategory::NotFound;
    }
    if lower.contains("hash") {
        return AuthErrorCategory::Hash;
    }
    if lower.contains("sql") || lower.contains("database") || lower.contains("db error") {
        return AuthErrorCategory::Database;
    }
    AuthErrorCategory::Other
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r565_classify_auth_error_variants() {
        assert_eq!(classify(&AuthError::MissingCredentials), AuthErrorCategory::MissingCredentials);
        assert_eq!(classify(&AuthError::InvalidToken), AuthErrorCategory::NotFound);
        assert_eq!(classify(&AuthError::Expired), AuthErrorCategory::Expired);
    }

    #[test]
    fn r565_classify_string_basic_keywords() {
        assert_eq!(classify_str("missing credentials"), AuthErrorCategory::MissingCredentials);
        assert_eq!(classify_str("session expired"), AuthErrorCategory::Expired);
        assert_eq!(classify_str("token already used"), AuthErrorCategory::Replayed);
        assert_eq!(classify_str("session revoked"), AuthErrorCategory::Revoked);
        assert_eq!(classify_str("oauth state mismatch"), AuthErrorCategory::OAuthStateMismatch);
        assert_eq!(classify_str("email not verified"), AuthErrorCategory::EmailNotVerified);
    }

    #[test]
    fn r565_only_expired_is_transient() {
        assert!(AuthErrorCategory::Expired.is_transient());
        assert!(!AuthErrorCategory::Revoked.is_transient());
        assert!(!AuthErrorCategory::NotFound.is_transient());
        assert!(!AuthErrorCategory::EmailVerificationFailed.is_transient());
        assert!(!AuthErrorCategory::OAuthStateMismatch.is_transient());
    }
}
