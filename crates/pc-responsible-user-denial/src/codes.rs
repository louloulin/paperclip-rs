//! Responsible-user denial code 规范化。
//!
//! 与原 `crates/pc-responsible-user-denial/src/lib.rs` 等价。
//!
//! 对应 Node `server/src/services/responsible-user-denial-run-outcomes.ts`
//! 中 `normalizeResponsibleUserDenialCode` / `isResponsibleUserDenialCode`。

/// Responsible-user denial code 枚举 —— 与 Node `ResponsibleUserDenialCode` 1:1 对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResponsibleUserDenialCode {
    RateLimited,
    UnsupportedChannel,
    QuotaExceeded,
    NotEntitled,
    Other,
}

impl ResponsibleUserDenialCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RateLimited => "rate_limited",
            Self::UnsupportedChannel => "unsupported_channel",
            Self::QuotaExceeded => "quota_exceeded",
            Self::NotEntitled => "not_entitled",
            Self::Other => "other",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "rate_limited" => Some(Self::RateLimited),
            "unsupported_channel" => Some(Self::UnsupportedChannel),
            "quota_exceeded" => Some(Self::QuotaExceeded),
            "not_entitled" => Some(Self::NotEntitled),
            "other" => Some(Self::Other),
            _ => None,
        }
    }
}

/// 所有合法 code —— 与 Node `isResponsibleUserDenialCode` 1:1 对齐。
pub fn is_valid_code(s: &str) -> bool {
    ResponsibleUserDenialCode::from_str(s).is_some()
}

/// 规范化任意值为合法 code，非合法值返回 `None`。
///
/// 与 Node `normalizeResponsibleUserDenialCode` 1:1 对齐：
/// - 非 string → None
/// - 字符串但不是合法 code → None
/// - 合法 code → Some(code)
pub fn normalize_responsible_user_denial_code(value: &str) -> Option<ResponsibleUserDenialCode> {
    ResponsibleUserDenialCode::from_str(value)
}

/// Overload —— 接受 `&serde_json::Value`。
pub fn normalize_responsible_user_denial_code_value(
    value: &serde_json::Value,
) -> Option<ResponsibleUserDenialCode> {
    value.as_str().and_then(normalize_responsible_user_denial_code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn r706_all_codes_round_trip() {
        for code in [
            ResponsibleUserDenialCode::RateLimited,
            ResponsibleUserDenialCode::UnsupportedChannel,
            ResponsibleUserDenialCode::QuotaExceeded,
            ResponsibleUserDenialCode::NotEntitled,
            ResponsibleUserDenialCode::Other,
        ] {
            assert_eq!(ResponsibleUserDenialCode::from_str(code.as_str()), Some(code));
        }
    }

    #[test]
    fn r706_unknown_string_returns_none() {
        assert_eq!(ResponsibleUserDenialCode::from_str("unknown"), None);
        assert_eq!(ResponsibleUserDenialCode::from_str(""), None);
    }

    #[test]
    fn r706_is_valid_code() {
        assert!(is_valid_code("rate_limited"));
        assert!(is_valid_code("not_entitled"));
        assert!(!is_valid_code("RateLimited")); // 大小写敏感
        assert!(!is_valid_code("rate-limited")); // 中划线不是下划线
    }

    #[test]
    fn r706_normalize_accepts_valid_codes() {
        assert_eq!(
            normalize_responsible_user_denial_code("rate_limited"),
            Some(ResponsibleUserDenialCode::RateLimited)
        );
        assert_eq!(
            normalize_responsible_user_denial_code("other"),
            Some(ResponsibleUserDenialCode::Other)
        );
    }

    #[test]
    fn r706_normalize_rejects_invalid() {
        assert_eq!(normalize_responsible_user_denial_code("unknown"), None);
        assert_eq!(normalize_responsible_user_denial_code(""), None);
        assert_eq!(normalize_responsible_user_denial_code("RateLimited"), None);
    }

    #[test]
    fn r706_normalize_value_with_non_string() {
        assert_eq!(normalize_responsible_user_denial_code_value(&json!(null)), None);
        assert_eq!(normalize_responsible_user_denial_code_value(&json!(42)), None);
        assert_eq!(normalize_responsible_user_denial_code_value(&json!(true)), None);
    }

    #[test]
    fn r706_normalize_value_with_string() {
        assert_eq!(
            normalize_responsible_user_denial_code_value(&json!("rate_limited")),
            Some(ResponsibleUserDenialCode::RateLimited)
        );
        assert_eq!(
            normalize_responsible_user_denial_code_value(&json!("invalid")),
            None
        );
    }

    #[test]
    fn r706_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ResponsibleUserDenialCode>();
    }
}
