#![forbid(unsafe_code)]

//! Tool policy time/rate validation.
//! R710: Direct port of tool-access-policy.ts::isoDateOrNull + trustRuleIsActive + rateLimitRule.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Parse a value to ISO date string or null.
pub fn iso_date_or_null(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(s) => {
            DateTime::parse_from_rfc3339(s).ok().map(|d| d.with_timezone(&Utc).to_rfc3339())
        }
        _ => None,
    }
}

/// Trust rule time/rate config (Node trustRuleConfig 1:1).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustRuleConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_action_request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_invocation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_threshold: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_approval_count: Option<u32>,
}

/// Extract trust rule config from policy config (Node trustRuleConfig 1:1).
pub fn trust_rule_config(policy_config: &Value) -> Option<TrustRuleConfig> {
    let trust_rule = policy_config.get("trustRule");
    match trust_rule {
        Some(v) => serde_json::from_value(v.clone()).ok(),
        None => None,
    }
}

/// Check if a trust rule is active (not revoked, not expired).
/// Node trustRuleIsActive 1:1 parity.
pub fn trust_rule_is_active(config: &TrustRuleConfig, now: DateTime<Utc>) -> bool {
    if config.revoked_at.is_some() { return false; }
    if let Some(ref expires) = config.expires_at {
        match DateTime::parse_from_rfc3339(expires) {
            Ok(expires_at) => {
                if expires_at.with_timezone(&Utc) <= now { return false; }
            }
            Err(_) => {} // invalid date, ignore (matches Node Number.isNaN check that returns false but we let it pass)
        }
    }
    true
}

/// Rate limit rule (Node rateLimitRule 1:1 parity).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitRule {
    pub limit: u32,
    pub window_seconds: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_by: Option<Vec<String>>,
}

/// Extract rate limit rule from policy config.
pub fn rate_limit_rule(policy_config: &Value) -> Option<RateLimitRule> {
    let raw = policy_config.get("rateLimit").unwrap_or(policy_config);
    let limit = raw.get("limit").and_then(|v| v.as_u64());
    let window_seconds = raw.get("windowSeconds").and_then(|v| v.as_u64());
    match (limit, window_seconds) {
        (Some(l), Some(w)) if l > 0 && w > 0 => {
            let key_by = raw.get("keyBy").and_then(|v| v.as_array()).map(|arr| {
                arr.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect::<Vec<String>>()
            });
            Some(RateLimitRule {
                limit: l as u32,
                window_seconds: w as u32,
                key_by,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod internal_tests {
    use super::*;
    use serde_json::json;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-08-16T00:00:00+00:00").unwrap().with_timezone(&Utc)
    }

    #[test]
    fn iso_date_or_null_valid() {
        let r = iso_date_or_null(&json!("2026-08-16T00:00:00Z"));
        assert!(r.is_some());
        let s = r.unwrap();
        assert!(s.starts_with("2026"));
    }

    #[test]
    fn iso_date_or_null_invalid_returns_none() {
        assert!(iso_date_or_null(&json!("not-a-date")).is_none());
        assert!(iso_date_or_null(&json!(null)).is_none());
        assert!(iso_date_or_null(&json!(123)).is_none());
        assert!(iso_date_or_null(&json!({})).is_none());
    }

    #[test]
    fn trust_rule_active_when_no_revoked_or_expires() {
        let c = TrustRuleConfig::default();
        assert!(trust_rule_is_active(&c, now()));
    }

    #[test]
    fn trust_rule_revoked_is_inactive() {
        let c = TrustRuleConfig { revoked_at: Some("2026-08-01T00:00:00Z".into()), ..Default::default() };
        assert!(!trust_rule_is_active(&c, now()));
    }

    #[test]
    fn trust_rule_expired_is_inactive() {
        let c = TrustRuleConfig { expires_at: Some("2026-01-01T00:00:00Z".into()), ..Default::default() };
        assert!(!trust_rule_is_active(&c, now()));
    }

    #[test]
    fn trust_rule_not_yet_expired_is_active() {
        let c = TrustRuleConfig { expires_at: Some("2027-01-01T00:00:00Z".into()), ..Default::default() };
        assert!(trust_rule_is_active(&c, now()));
    }

    #[test]
    fn trust_rule_revoked_takes_priority() {
        let c = TrustRuleConfig {
            revoked_at: Some("2026-08-15T00:00:00Z".into()),
            expires_at: Some("2027-01-01T00:00:00Z".into()),
            ..Default::default()
        };
        assert!(!trust_rule_is_active(&c, now()));
    }

    #[test]
    fn trust_rule_config_extraction() {
        let policy = json!({"trustRule": {"revokedAt": "2026-08-01T00:00:00Z"}});
        let c = trust_rule_config(&policy).unwrap();
        assert_eq!(c.revoked_at, Some("2026-08-01T00:00:00Z".into()));
    }

    #[test]
    fn trust_rule_config_no_trust_rule_returns_none() {
        let policy = json!({});
        assert!(trust_rule_config(&policy).is_none());
    }

    #[test]
    fn rate_limit_rule_basic() {
        let policy = json!({"rateLimit": {"limit": 100, "windowSeconds": 60}});
        let r = rate_limit_rule(&policy).unwrap();
        assert_eq!(r.limit, 100);
        assert_eq!(r.window_seconds, 60);
        assert!(r.key_by.is_none());
    }

    #[test]
    fn rate_limit_rule_with_key_by() {
        let policy = json!({"rateLimit": {"limit": 50, "windowSeconds": 30, "keyBy": ["agentId", "companyId"]}});
        let r = rate_limit_rule(&policy).unwrap();
        assert_eq!(r.limit, 50);
        assert_eq!(r.window_seconds, 30);
        assert_eq!(r.key_by, Some(vec!["agentId".to_string(), "companyId".to_string()]));
    }

    #[test]
    fn rate_limit_rule_flat_config() {
        // Node allows limit/windowSeconds at top level if no rateLimit wrapper
        let policy = json!({"limit": 10, "windowSeconds": 5});
        let r = rate_limit_rule(&policy).unwrap();
        assert_eq!(r.limit, 10);
    }

    #[test]
    fn rate_limit_rule_invalid_returns_none() {
        let p1 = json!({"rateLimit": {"limit": 0, "windowSeconds": 60}});
        assert!(rate_limit_rule(&p1).is_none());
        let p2 = json!({"rateLimit": {"limit": 100, "windowSeconds": 0}});
        assert!(rate_limit_rule(&p2).is_none());
        let p3 = json!({});
        assert!(rate_limit_rule(&p3).is_none());
    }

    #[test]
    fn trust_rule_config_serde_camel_case() {
        let c = TrustRuleConfig { revoked_at: Some("2026-08-01T00:00:00Z".into()), ..Default::default() };
        let j = serde_json::to_string(&c).unwrap();
        assert!(j.contains("revokedAt"));
    }

    #[test]
    fn rate_limit_rule_serde_camel_case() {
        let r = RateLimitRule { limit: 1, window_seconds: 1, key_by: None };
        let j = serde_json::to_string(&r).unwrap();
        assert!(j.contains("windowSeconds"));
    }

    // ---- Round 763: pc-tool policy_validation 集成测试 ----

    use super::*;
    use chrono::TimeZone;

    /// trust_rule_is_active: 未 revoked + 未过期 → true。
    #[test]
    fn r763_trust_rule_active_no_revoke_no_expire() {
        let cfg = TrustRuleConfig {
            revoked_at: None,
            expires_at: None,
            source_action_request_id: None,
            source_invocation_id: None,
            approval_threshold: None,
            source_approval_count: None,
        };
        let now = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
        assert!(trust_rule_is_active(&cfg, now));
    }

    /// trust_rule_is_active: revoked → false (即使未过期)。
    #[test]
    fn r763_trust_rule_revoked_inactive() {
        let cfg = TrustRuleConfig {
            revoked_at: Some("2026-01-01T00:00:00Z".into()),
            expires_at: None,
            source_action_request_id: None,
            source_invocation_id: None,
            approval_threshold: None,
            source_approval_count: None,
        };
        let now = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
        assert!(!trust_rule_is_active(&cfg, now));
    }

    /// trust_rule_is_active: 已过期 (expires_at <= now) → false。
    #[test]
    fn r763_trust_rule_expired_inactive() {
        let cfg = TrustRuleConfig {
            revoked_at: None,
            expires_at: Some("2026-01-01T00:00:00Z".into()),
            source_action_request_id: None,
            source_invocation_id: None,
            approval_threshold: None,
            source_approval_count: None,
        };
        let now = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
        assert!(!trust_rule_is_active(&cfg, now));
    }

    /// trust_rule_is_active: 未到期 → true。
    #[test]
    fn r763_trust_rule_not_yet_expired_active() {
        let cfg = TrustRuleConfig {
            revoked_at: None,
            expires_at: Some("2030-01-01T00:00:00Z".into()),
            source_action_request_id: None,
            source_invocation_id: None,
            approval_threshold: None,
            source_approval_count: None,
        };
        let now = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
        assert!(trust_rule_is_active(&cfg, now));
    }

    /// rate_limit_rule: 合法 limit + window 返回 Some; 缺字段返回 None。
    #[test]
    fn r763_rate_limit_rule_extract() {
        let cfg = serde_json::json!({"rateLimit": {"limit": 100, "windowSeconds": 60}});
        let rule = rate_limit_rule(&cfg);
        assert!(rule.is_some());
        let r = rule.unwrap();
        assert_eq!(r.limit, 100);
        assert_eq!(r.window_seconds, 60);

        // 缺字段 → None
        let bad = serde_json::json!({"rateLimit": {"limit": 100}});
        assert!(rate_limit_rule(&bad).is_none());

        // 顶层 fallback 到 policy_config
        let top = serde_json::json!({"limit": 10, "windowSeconds": 5});
        let r2 = rate_limit_rule(&top).unwrap();
        assert_eq!(r2.limit, 10);

        // limit=0 → None
        let zero = serde_json::json!({"limit": 0, "windowSeconds": 5});
        assert!(rate_limit_rule(&zero).is_none());
    }
}
