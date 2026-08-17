#![forbid(unsafe_code)]

//! Decision bundle validation pure helpers — 1:1 port of
//! paperclip/server/src/services/decisions.ts::DecisionService input validation.
//!
//! R736: 零 DB 校验 helpers（uuid nil / title trim / filter normalization）。

use uuid::Uuid;

/// 校验 uuid 非 nil（用于 bundle_id / company_id / origin_*_id）。
pub fn require_non_nil(id: Uuid, field: &str) -> Result<(), String> {
    if id.is_nil() {
        return Err(format!("{field} is required"));
    }
    Ok(())
}

/// 校验 bundle title：trim + 非空。
pub fn validate_bundle_title(title: &str) -> Result<(), String> {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return Err("title must not be empty".into());
    }
    if trimmed.chars().count() > 256 {
        return Err("title must be at most 256 characters".into());
    }
    Ok(())
}

/// Decision bundle filter（创建时可选过滤条件）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DecisionBundleFilter {
    pub state: Option<String>,
    pub origin_issue_id: Option<Uuid>,
    pub limit: Option<i64>,
}

/// 规范化 + 校验 filter。
pub fn normalize_bundle_filter(mut filter: DecisionBundleFilter) -> DecisionBundleFilter {
    if let Some(s) = filter.state.as_ref() {
        filter.state = Some(s.trim().to_lowercase());
    }
    if let Some(l) = filter.limit {
        filter.limit = Some(l.clamp(1, 500));
    }
    filter
}

/// 判断 bundle state 是否合法（done / open / cancelled / pending）。
pub fn is_valid_bundle_state(state: &str) -> bool {
    let s = state.trim().to_lowercase();
    matches!(s.as_str(), "done" | "open" | "cancelled" | "pending" | "expired")
}

/// 解析 bundle state 为枚举（None 表示非法）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundleState {
    Done,
    Open,
    Cancelled,
    Pending,
    Expired,
}

impl BundleState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Open => "open",
            Self::Cancelled => "cancelled",
            Self::Pending => "pending",
            Self::Expired => "expired",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        let normalized = s.trim().to_lowercase();
        match normalized.as_str() {
            "done" => Some(Self::Done),
            "open" => Some(Self::Open),
            "cancelled" => Some(Self::Cancelled),
            "pending" => Some(Self::Pending),
            "expired" => Some(Self::Expired),
            _ => None,
        }
    }
}

#[cfg(test)]
mod internal_tests {
    use super::*;

    #[test]
    fn require_non_nil_accepts_real_uuid() {
        let id = Uuid::new_v4();
        assert!(require_non_nil(id, "bundleId").is_ok());
    }

    #[test]
    fn require_non_nil_rejects_nil() {
        assert!(require_non_nil(Uuid::nil(), "bundleId").is_err());
    }

    #[test]
    fn validate_bundle_title_accepts() {
        assert!(validate_bundle_title("Approve Q3 plan").is_ok());
    }

    #[test]
    fn validate_bundle_title_rejects_empty() {
        assert!(validate_bundle_title("").is_err());
        assert!(validate_bundle_title("   ").is_err());
    }

    #[test]
    fn validate_bundle_title_rejects_too_long() {
        let s = "a".repeat(257);
        assert!(validate_bundle_title(&s).is_err());
    }

    #[test]
    fn normalize_bundle_filter_lowercases() {
        let f = DecisionBundleFilter {
            state: Some("OPEN".into()),
            origin_issue_id: None,
            limit: None,
        };
        let n = normalize_bundle_filter(f);
        assert_eq!(n.state, Some("open".to_string()));
    }

    #[test]
    fn normalize_bundle_filter_clamps_limit() {
        let f = DecisionBundleFilter {
            state: None,
            origin_issue_id: None,
            limit: Some(10_000),
        };
        let n = normalize_bundle_filter(f);
        assert_eq!(n.limit, Some(500));
    }

    #[test]
    fn normalize_bundle_filter_zero_limit() {
        let f = DecisionBundleFilter {
            state: None,
            origin_issue_id: None,
            limit: Some(0),
        };
        let n = normalize_bundle_filter(f);
        assert_eq!(n.limit, Some(1)); // clamp to min 1
    }

    #[test]
    fn is_valid_bundle_state_known() {
        for s in ["done", "open", "cancelled", "pending", "expired"] {
            assert!(is_valid_bundle_state(s), "{s} should be valid");
        }
    }

    #[test]
    fn is_valid_bundle_state_unknown() {
        assert!(!is_valid_bundle_state("unknown"));
        assert!(!is_valid_bundle_state(""));
    }

    #[test]
    fn is_valid_bundle_state_case_insensitive() {
        assert!(is_valid_bundle_state("DONE"));
        assert!(is_valid_bundle_state("Cancelled"));
    }

    #[test]
    fn bundle_state_round_trip() {
        for s in [BundleState::Done, BundleState::Open, BundleState::Cancelled, BundleState::Pending, BundleState::Expired] {
            assert_eq!(BundleState::from_str(s.as_str()), Some(s));
        }
    }
}
