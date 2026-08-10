//! 提供方 typed error 与分类。
//!
//! 设计目标：
//! - 不破坏现有 `SecretProvider::create_secret → Result<T, String>` 接口；
//!   老 provider 可以继续返回 `String`，新代码在边界处用
//!   [`SecretProviderError::classify`] 把字符串映射为 typed 错误。
//! - 调用方可以基于 `is_transient()` 决定是否重试，
//!   基于 `category()` 决定是否降级/告警。
//! - 后续 provider（AWS IAM role、Vault AppRole、Kubernetes）都会返回
//!   `SecretProviderError`，避免 `String` 错误不可分桶的问题。

use std::time::Duration;

/// 提供方错误分类。用于上层路由（重试 / 降级 / 告警）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SecretErrorCategory {
    /// 配置错误（地址/凭证/字段缺失）—— 不可重试。
    Config,
    /// 鉴权失败（401 / 403 / InvalidToken / 凭证过期）—— 不可重试，除非凭证可刷新。
    Auth,
    /// 资源不存在（404）—— 不可重试。
    NotFound,
    /// 限流（429）—— 可重试，建议带 Retry-After。
    RateLimited,
    /// 上游 5xx / 服务端错误 —— 可重试（短暂）。
    Upstream,
    /// 网络 / DNS / TCP / TLS 失败 —— 可重试。
    Transport,
    /// 解码 / 序列化失败 —— 通常不可重试。
    Serialization,
    /// 整体超时 —— 可重试。
    Timeout,
    /// 其它（未知）—— 保守按不可重试处理。
    Other,
}

/// 提供方 typed error。`String` 错误信息保留以兼容现有 provider；
/// 真正分类由 `category()` 给出。
#[derive(Debug, Clone)]
pub struct SecretProviderError {
    pub category: SecretErrorCategory,
    pub message: String,
    /// 用于限流场景下回退的 Retry-After 时长。
    pub retry_after: Option<Duration>,
    /// 上游 HTTP 状态码（如果有）。
    pub status: Option<u16>,
}

impl std::fmt::Display for SecretProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.status {
            Some(s) => write!(f, "[{:?}/{}] {}", self.category, s, self.message),
            None => write!(f, "[{:?}] {}", self.category, self.message),
        }
    }
}

impl std::error::Error for SecretProviderError {}

impl SecretProviderError {
    /// 构造一个 typed error。
    #[must_use]
    pub fn new(category: SecretErrorCategory, message: impl Into<String>) -> Self {
        Self {
            category,
            message: message.into(),
            retry_after: None,
            status: None,
        }
    }

    /// 带 Retry-After 的限流错误。
    #[must_use]
    pub fn rate_limited(message: impl Into<String>, retry_after: Option<Duration>) -> Self {
        Self {
            category: SecretErrorCategory::RateLimited,
            message: message.into(),
            retry_after,
            status: Some(429),
        }
    }

    /// 带状态码的上游错误。
    #[must_use]
    pub fn upstream(status: u16, message: impl Into<String>) -> Self {
        let category = match status {
            401 | 403 => SecretErrorCategory::Auth,
            404 => SecretErrorCategory::NotFound,
            429 => SecretErrorCategory::RateLimited,
            500..=599 => SecretErrorCategory::Upstream,
            _ => SecretErrorCategory::Other,
        };
        let mut err = Self::new(category, message);
        err.status = Some(status);
        err
    }

    /// 类别访问。
    #[must_use]
    pub fn category(&self) -> SecretErrorCategory {
        self.category
    }

    /// 是否可短暂重试。Config / Auth / NotFound / Serialization / Other 一律 false。
    /// RateLimited / Upstream / Transport / Timeout 为 true。
    #[must_use]
    pub fn is_transient(&self) -> bool {
        matches!(
            self.category,
            SecretErrorCategory::RateLimited
                | SecretErrorCategory::Upstream
                | SecretErrorCategory::Transport
                | SecretErrorCategory::Timeout
        )
    }

    /// 把一个未知字符串分类为 typed error。使用关键词 + 启发式，与
    /// Node `classifyAwsError` / `classifyVaultError` 的语义一致。
    #[must_use]
    pub fn classify(message: &str) -> Self {
        let lower = message.to_lowercase();
        let status = extract_status(&lower);
        if let Some(s) = status {
            return Self::upstream(s, message);
        }
        if lower.contains("timeout") || lower.contains("timed out") {
            return Self::new(SecretErrorCategory::Timeout, message);
        }
        if lower.contains("rate") && lower.contains("limit") {
            return Self::rate_limited(message, None);
        }
        if lower.contains("invalid token")
            || lower.contains("unauthorized")
            || lower.contains("forbidden")
            || lower.contains("permission denied")
        {
            return Self::new(SecretErrorCategory::Auth, message);
        }
        if lower.contains("not found") || lower.contains("no such") {
            return Self::new(SecretErrorCategory::NotFound, message);
        }
        if lower.contains("connection refused")
            || lower.contains("dns")
            || lower.contains("tls")
            || lower.contains("network")
            || lower.contains("transport")
        {
            return Self::new(SecretErrorCategory::Transport, message);
        }
        if lower.contains("invalid ") || lower.contains("missing ") {
            return Self::new(SecretErrorCategory::Config, message);
        }
        if lower.contains("decode") || lower.contains("serialize") || lower.contains("json") {
            return Self::new(SecretErrorCategory::Serialization, message);
        }
        Self::new(SecretErrorCategory::Other, message)
    }
}

fn extract_status(lower: &str) -> Option<u16> {
    // 形如 "HTTP 503"、"status 401"、"returned 429"
    let bytes = lower.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if let Ok(n) = lower[start..i].parse::<u16>() {
                if (100..600).contains(&n) {
                    return Some(n);
                }
            }
        } else {
            i += 1;
        }
    }
    None
}

impl From<SecretProviderError> for String {
    fn from(err: SecretProviderError) -> Self {
        err.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_503_upstream() {
        let e = SecretProviderError::classify("aws get failed: HTTP 503");
        assert_eq!(e.category, SecretErrorCategory::Upstream);
        assert_eq!(e.status, Some(503));
        assert!(e.is_transient());
    }

    #[test]
    fn classify_401_auth() {
        let e = SecretProviderError::classify("vault read returned 401");
        assert_eq!(e.category, SecretErrorCategory::Auth);
        assert!(!e.is_transient());
    }

    #[test]
    fn classify_404_not_found() {
        let e = SecretProviderError::classify("aws get failed: HTTP 404");
        assert_eq!(e.category, SecretErrorCategory::NotFound);
        assert!(!e.is_transient());
    }

    #[test]
    fn classify_429_rate_limited() {
        let e = SecretProviderError::classify("vault write returned 429");
        assert_eq!(e.category, SecretErrorCategory::RateLimited);
        assert!(e.is_transient());
    }

    #[test]
    fn classify_timeout_is_transient() {
        let e = SecretProviderError::classify("request timed out after 10s");
        assert_eq!(e.category, SecretErrorCategory::Timeout);
        assert!(e.is_transient());
    }

    #[test]
    fn classify_connection_refused_is_transient() {
        let e = SecretProviderError::classify("connection refused");
        assert_eq!(e.category, SecretErrorCategory::Transport);
        assert!(e.is_transient());
    }

    #[test]
    fn classify_invalid_token_is_auth() {
        let e = SecretProviderError::classify("invalid token");
        assert_eq!(e.category, SecretErrorCategory::Auth);
    }

    #[test]
    fn classify_missing_field_is_config() {
        let e = SecretProviderError::classify("missing accessToken in gcp provider_config");
        assert_eq!(e.category, SecretErrorCategory::Config);
    }

    #[test]
    fn classify_unknown_is_other() {
        let e = SecretProviderError::classify("some completely new thing");
        assert_eq!(e.category, SecretErrorCategory::Other);
        assert!(!e.is_transient());
    }

    #[test]
    fn rate_limited_constructor_sets_status() {
        let e = SecretProviderError::rate_limited("slow down", Some(Duration::from_secs(2)));
        assert_eq!(e.category, SecretErrorCategory::RateLimited);
        assert_eq!(e.status, Some(429));
        assert_eq!(e.retry_after, Some(Duration::from_secs(2)));
    }

    #[test]
    fn error_display_includes_category_and_message() {
        let e = SecretProviderError::upstream(502, "bad gateway");
        let s = e.to_string();
        assert!(s.contains("Upstream"));
        assert!(s.contains("502"));
        assert!(s.contains("bad gateway"));
    }
}
