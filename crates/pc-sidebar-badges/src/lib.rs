#![forbid(unsafe_code)]
//! `pc-sidebar-badges` —— sidebar badge 计数 helper（纯函数）。
//!
//! 对应 Node `server/src/services/sidebar-badges.ts`（86 行）。
//!
//! 设计目标：1:1 复刻
//! - `ACTIONABLE_APPROVAL_STATUSES = ["pending", "revision_requested"]`
//! - `FAILED_HEARTBEAT_STATUSES = ["failed", "timed_out"]`
//! - `normalizeTimestamp(value)` —— Date / string / null / undefined → ms（无效 → 0）
//! - `isDismissed(dismissedAtByKey, itemKey, activityAt)` —— 当 dismiss 时间 >=
//!   activity 时间时返回 true
//!
//! DB 部分（`sidebarBadgeService(db)`）由上层接入 pc-repos。

/// 可操作的 approval 状态集合 —— 与 Node `ACTIONABLE_APPROVAL_STATUSES` 1:1 对齐。
pub const ACTIONABLE_APPROVAL_STATUSES: &[&str] = &["pending", "revision_requested"];

/// 失败的 heartbeat 状态集合 —— 与 Node `FAILED_HEARTBEAT_STATUSES` 1:1 对齐。
pub const FAILED_HEARTBEAT_STATUSES: &[&str] = &["failed", "timed_out"];

/// 规范化时间戳为 ms 整数。
///
/// 与 Node `normalizeTimestamp` 1:1 对齐：
/// - `null` / `undefined` → 0
/// - Date / string → `new Date(value).getTime()`
/// - 无效时间（NaN / Infinity） → 0
pub fn normalize_timestamp(value: Option<&serde_json::Value>) -> i64 {
    let Some(v) = value else {
        return 0;
    };
    // 接受 string / 数字 / null / object (含 Date 序列化形式)
    let ts = match v {
        serde_json::Value::String(s) => {
            // ISO string or RFC3339
            chrono::DateTime::parse_from_rfc3339(s)
                .map(|dt| dt.timestamp_millis())
                .unwrap_or_else(|_| {
                    // Try plain unix ms string
                    s.parse::<i64>().unwrap_or(0)
                })
        }
        serde_json::Value::Number(n) => n.as_i64().unwrap_or(0),
        _ => return 0,
    };
    ts
}

/// 简化的 normalize（接受 chrono 兼容输入）。
///
/// 与上面的 `normalize_timestamp` 行为一致，但接受原生 `Option<&str>` / `Option<i64>`。
pub fn normalize_timestamp_str(value: Option<&str>) -> i64 {
    let Some(s) = value else {
        return 0;
    };
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return dt.timestamp_millis();
    }
    if let Ok(n) = s.parse::<i64>() {
        return n;
    }
    0
}

/// 判断 activity 是否已被 dismiss。
///
/// 与 Node `isDismissed` 1:1 对齐：
/// - 没有 dismiss → false
/// - `dismissedAt >= activityAt` → true（用 normalizeTimestamp 比较）
pub fn is_dismissed(
    dismissed_at_by_key: &std::collections::HashMap<String, i64>,
    item_key: &str,
    activity_at_ms: i64,
) -> bool {
    let Some(&dismissed_at) = dismissed_at_by_key.get(item_key) else {
        return false;
    };
    dismissed_at >= activity_at_ms
}

/// 公开版本：接受字符串活动日期。
pub fn is_dismissed_str(
    dismissed_at_by_key: &std::collections::HashMap<String, i64>,
    item_key: &str,
    activity_at: Option<&str>,
) -> bool {
    let activity_at_ms = normalize_timestamp_str(activity_at);
    is_dismissed(dismissed_at_by_key, item_key, activity_at_ms)
}

/// 判断 status 是否在 actionable approvals 中。
pub fn is_actionable_approval_status(status: &str) -> bool {
    ACTIONABLE_APPROVAL_STATUSES.contains(&status)
}

/// 判断 status 是否在 failed heartbeats 中。
pub fn is_failed_heartbeat_status(status: &str) -> bool {
    FAILED_HEARTBEAT_STATUSES.contains(&status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r708_normalize_timestamp_none_returns_zero() {
        assert_eq!(normalize_timestamp_str(None), 0);
    }

    #[test]
    fn r708_normalize_timestamp_iso_string() {
        let ts = normalize_timestamp_str(Some("2024-01-01T00:00:00Z"));
        assert!(ts > 0);
        // 2024-01-01T00:00:00Z = 1704067200000 ms
        assert_eq!(ts, 1_704_067_200_000);
    }

    #[test]
    fn r708_normalize_timestamp_unix_ms_string() {
        assert_eq!(normalize_timestamp_str(Some("1704067200000")), 1_704_067_200_000);
    }

    #[test]
    fn r708_normalize_timestamp_invalid_returns_zero() {
        assert_eq!(normalize_timestamp_str(Some("not-a-date")), 0);
        assert_eq!(normalize_timestamp_str(Some("")), 0);
    }

    #[test]
    fn r708_normalize_timestamp_value() {
        // serde_json::Value overload
        use serde_json::json;
        assert_eq!(normalize_timestamp(Some(&json!(null)), ), 0);
        assert_eq!(normalize_timestamp(Some(&json!("")), ), 0);
        assert_eq!(normalize_timestamp(Some(&json!("2024-01-01T00:00:00Z"))), 1_704_067_200_000);
        assert_eq!(normalize_timestamp(Some(&json!(1_704_067_200_000i64))), 1_704_067_200_000);
        assert_eq!(normalize_timestamp(Some(&json!(42))), 42);
        assert_eq!(normalize_timestamp(Some(&json!(true))), 0); // 非 string/number
        assert_eq!(normalize_timestamp(Some(&json!([]))), 0);
        assert_eq!(normalize_timestamp(Some(&json!({}))), 0);
    }

    #[test]
    fn r708_is_dismissed_no_entry() {
        let map = std::collections::HashMap::new();
        assert!(!is_dismissed(&map, "key1", 1000));
    }

    #[test]
    fn r708_is_dismissed_after_activity() {
        let mut map = std::collections::HashMap::new();
        map.insert("key1".to_string(), 2000);
        assert!(is_dismissed(&map, "key1", 1000)); // 2000 >= 1000
        assert!(is_dismissed(&map, "key1", 2000)); // 2000 >= 2000
    }

    #[test]
    fn r708_is_dismissed_before_activity() {
        let mut map = std::collections::HashMap::new();
        map.insert("key1".to_string(), 1000);
        assert!(!is_dismissed(&map, "key1", 2000));
    }

    #[test]
    fn r708_is_dismissed_str_with_iso_activity() {
        let mut map = std::collections::HashMap::new();
        map.insert("key1".to_string(), 1_704_067_200_000);
        assert!(is_dismissed_str(&map, "key1", Some("2024-01-01T00:00:00Z")));
        assert!(!is_dismissed_str(&map, "key1", Some("2024-06-01T00:00:00Z")));
    }

    #[test]
    fn r708_is_dismissed_str_none_activity() {
        let mut map = std::collections::HashMap::new();
        map.insert("key1".to_string(), 1000);
        // None activity → 0 → dismissed >= 0
        assert!(is_dismissed_str(&map, "key1", None));
    }

    #[test]
    fn r708_actionable_approval_statuses() {
        assert!(is_actionable_approval_status("pending"));
        assert!(is_actionable_approval_status("revision_requested"));
        assert!(!is_actionable_approval_status("approved"));
        assert!(!is_actionable_approval_status("rejected"));
    }

    #[test]
    fn r708_failed_heartbeat_statuses() {
        assert!(is_failed_heartbeat_status("failed"));
        assert!(is_failed_heartbeat_status("timed_out"));
        assert!(!is_failed_heartbeat_status("running"));
        assert!(!is_failed_heartbeat_status("completed"));
    }

    #[test]
    fn r708_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<fn(&str) -> bool>();
    }
}
