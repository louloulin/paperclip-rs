//! Release policy validation + mode parsing.
//!
//! - `MODE_*` 常量与 Node `IssueTreeControlMode` 严格对齐。
//! - `IssueTreeReleasePolicyStrategy` 枚举与 Node 端 `IssueTreeHoldReleasePolicy` 形状对齐。
//! - `default_release_policy()` 返回 `strategy = "manual"`（与 Node `DEFAULT_RELEASE_POLICY` 对齐）。

use serde::{Deserialize, Serialize};

use super::types::IssueTreeControlMode;

pub const MODE_PAUSE: &str = "pause";
pub const MODE_STOP: &str = "stop";
pub const MODE_THROTTLE: &str = "throttle";
pub const MODE_ISOLATE: &str = "isolate";

/// 与 Node 端 `IssueTreeHoldReleasePolicy` 对齐的 release strategy。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueTreeReleasePolicyStrategy {
    /// 手动释放（默认；与 Node `DEFAULT_RELEASE_POLICY` 对齐）。
    Manual,
    /// 当所有 member 都进入 `done` / `cancelled` 状态时自动释放。
    AllMembersTerminal,
    /// 在指定时间后自动释放（带 `releaseAt` 时间戳）。
    ScheduledAt,
    /// 当 root issue 进入 `done` 状态时自动释放。
    OnRootDone,
}

impl IssueTreeReleasePolicyStrategy {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::AllMembersTerminal => "all_members_terminal",
            Self::ScheduledAt => "scheduled_at",
            Self::OnRootDone => "on_root_done",
        }
    }
}

/// 把任意字符串解析成 `IssueTreeControlMode`。
pub fn parse_mode(value: &str) -> Option<IssueTreeControlMode> {
    match value {
        MODE_PAUSE => Some(IssueTreeControlMode::Pause),
        MODE_STOP => Some(IssueTreeControlMode::Stop),
        MODE_THROTTLE => Some(IssueTreeControlMode::Throttle),
        MODE_ISOLATE => Some(IssueTreeControlMode::Isolate),
        _ => None,
    }
}

/// 校验 mode 字符串是否合法。
pub fn validate_mode(value: &str) -> Result<IssueTreeControlMode, String> {
    parse_mode(value).ok_or_else(|| {
        format!(
            "invalid mode {value:?}: must be one of {MODE_PAUSE:?}, {MODE_STOP:?},              {MODE_THROTTLE:?}, {MODE_ISOLATE:?}"
        )
    })
}

/// 返回默认 release policy（manual）。
pub fn default_release_policy() -> serde_json::Value {
    serde_json::json!({ "strategy": "manual" })
}

/// 校验 release policy JSON：
/// - 必须是 object
/// - 必须有 `strategy` 字段
/// - `strategy` 必须是已知枚举值
/// - `scheduled_at` 策略下 `releaseAt` 字段必须是字符串时间戳
pub fn validate_release_policy(policy: &serde_json::Value) -> Result<(), String> {
    if !policy.is_object() {
        return Err("release_policy must be a JSON object".to_string());
    }
    let strategy = policy
        .get("strategy")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "release_policy.strategy is required".to_string())?;
    let parsed = match strategy {
        "manual" => Some(IssueTreeReleasePolicyStrategy::Manual),
        "all_members_terminal" => Some(IssueTreeReleasePolicyStrategy::AllMembersTerminal),
        "scheduled_at" => Some(IssueTreeReleasePolicyStrategy::ScheduledAt),
        "on_root_done" => Some(IssueTreeReleasePolicyStrategy::OnRootDone),
        _ => None,
    };
    let parsed = parsed.ok_or_else(|| {
        format!(
            "invalid release_policy.strategy {strategy:?}: must be one of manual,              all_members_terminal, scheduled_at, on_root_done"
        )
    })?;
    if matches!(parsed, IssueTreeReleasePolicyStrategy::ScheduledAt) {
        let release_at = policy
            .get("releaseAt")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                "release_policy.releaseAt (string timestamp) is required for                  scheduled_at strategy"
                    .to_string()
            })?;
        if release_at.trim().is_empty() {
            return Err("release_policy.releaseAt must be non-empty".to_string());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_modes() {
        assert_eq!(parse_mode("pause"), Some(IssueTreeControlMode::Pause));
        assert_eq!(parse_mode("stop"), Some(IssueTreeControlMode::Stop));
        assert_eq!(parse_mode("throttle"), Some(IssueTreeControlMode::Throttle));
        assert_eq!(parse_mode("isolate"), Some(IssueTreeControlMode::Isolate));
        assert_eq!(parse_mode("nope"), None);
    }

    #[test]
    fn validate_mode_accepts_known() {
        for m in ["pause", "stop", "throttle", "isolate"] {
            assert!(validate_mode(m).is_ok());
        }
    }

    #[test]
    fn validate_mode_rejects_unknown() {
        assert!(validate_mode("nope").is_err());
    }

    #[test]
    fn default_release_policy_is_manual() {
        let p = default_release_policy();
        assert_eq!(p.get("strategy").and_then(|v| v.as_str()), Some("manual"));
    }

    #[test]
    fn validate_release_policy_accepts_manual() {
        let p = serde_json::json!({ "strategy": "manual" });
        assert!(validate_release_policy(&p).is_ok());
    }

    #[test]
    fn validate_release_policy_accepts_all_members_terminal() {
        let p = serde_json::json!({ "strategy": "all_members_terminal" });
        assert!(validate_release_policy(&p).is_ok());
    }

    #[test]
    fn validate_release_policy_accepts_on_root_done() {
        let p = serde_json::json!({ "strategy": "on_root_done" });
        assert!(validate_release_policy(&p).is_ok());
    }

    #[test]
    fn validate_release_policy_requires_release_at_for_scheduled() {
        let p = serde_json::json!({ "strategy": "scheduled_at" });
        assert!(validate_release_policy(&p).is_err());
        let p2 = serde_json::json!({ "strategy": "scheduled_at", "releaseAt": "  " });
        assert!(validate_release_policy(&p2).is_err());
        let p3 = serde_json::json!({ "strategy": "scheduled_at", "releaseAt": "2099-01-01T00:00:00Z" });
        assert!(validate_release_policy(&p3).is_ok());
    }

    #[test]
    fn validate_release_policy_rejects_non_object() {
        assert!(validate_release_policy(&serde_json::json!("manual")).is_err());
        assert!(validate_release_policy(&serde_json::json!(42)).is_err());
        assert!(validate_release_policy(&serde_json::json!(null)).is_err());
    }

    #[test]
    fn validate_release_policy_rejects_missing_strategy() {
        let p = serde_json::json!({});
        assert!(validate_release_policy(&p).is_err());
    }

    #[test]
    fn validate_release_policy_rejects_unknown_strategy() {
        let p = serde_json::json!({ "strategy": "wat" });
        assert!(validate_release_policy(&p).is_err());
    }
}
