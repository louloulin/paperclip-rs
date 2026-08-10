//! Sidebar badge 计数 / dismiss 判定 helper（纯函数）。
//!
//! 与原 `crates/pc-sidebar-badges/src/lib.rs` 等价。
//!
//! 对应 Node `server/src/services/sidebar-badges.ts`（86 行）。

/// 可操作的 approval 状态集合 —— 与 Node `ACTIONABLE_APPROVAL_STATUSES` 1:1 对齐。
pub const ACTIONABLE_APPROVAL_STATUSES: &[&str] = &["pending", "revision_requested"];

/// 失败的 heartbeat 状态集合 —— 与 Node `FAILED_HEARTBEAT_STATUSES` 1:1 对齐。
pub const FAILED_HEARTBEAT_STATUSES: &[&str] = &["failed", "timed_out"];

/// 规范化时间戳为 ms 整数。
pub fn normalize_timestamp(value: Option<&serde_json::Value>) -> i64 {
    let Some(v) = value else {
        return 0;
    };
    let ts = match v {
        serde_json::Value::String(s) => chrono::DateTime::parse_from_rfc3339(s)
            .map(|dt| dt.timestamp_millis())
            .unwrap_or_else(|_| s.parse::<i64>().unwrap_or(0)),
        serde_json::Value::Number(n) => n.as_i64().unwrap_or(0),
        _ => return 0,
    };
    ts
}

/// 简化版：接受原生字符串。
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
        use serde_json::json;
        assert_eq!(normalize_timestamp(Some(&json!(null))), 0);
        assert_eq!(normalize_timestamp(Some(&json!(""))), 0);
        assert_eq!(normalize_timestamp(Some(&json!("2024-01-01T00:00:00Z"))), 1_704_067_200_000);
        assert_eq!(normalize_timestamp(Some(&json!(1_704_067_200_000i64))), 1_704_067_200_000);
        assert_eq!(normalize_timestamp(Some(&json!(42))), 42);
        assert_eq!(normalize_timestamp(Some(&json!(true))), 0);
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
        assert!(is_dismissed(&map, "key1", 1000));
        assert!(is_dismissed(&map, "key1", 2000));
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
