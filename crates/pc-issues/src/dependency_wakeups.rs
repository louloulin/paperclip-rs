#![forbid(unsafe_code)]
//! Issue blocker-resolved wakeup 幂等键 / 状态集合（原 `pc-issue-dependency-wakeups` 已下沉） —— issue blocker-resolved wakeup 的幂等键 / 状态集合。
//!
//! 对应 Node `server/src/services/issue-dependency-wakeups.ts`（72 行）。
//!
//! 设计目标：1:1 复刻
//! - `ISSUE_BLOCKERS_RESOLVED_WAKE_REASON = "issue_blockers_resolved"` 常量
//! - `IDEMPOTENT_DEPENDENCY_WAKE_STATUSES` 集合（4 个状态）
//! - `buildIssueBlockersResolvedWakeIdempotencyKey` —— 用 `:` 拼接
//!
//! DB 部分（`findExistingIssueBlockersResolvedWake`）由上层接入 pc-repos。

/// Wakeup 原因常量 —— 与 Node `ISSUE_BLOCKERS_RESOLVED_WAKE_REASON` 1:1 对齐。
pub const ISSUE_BLOCKERS_RESOLVED_WAKE_REASON: &str = "issue_blockers_resolved";

/// 幂等依赖 wakeup 状态集合 —— 与 Node `IDEMPOTENT_DEPENDENCY_WAKE_STATUSES` 1:1 对齐。
///
/// 这些状态表示 wakeup 仍在处理中或已完成；再次发现相同 idempotency key 的
/// wakeup 时应跳过（避免重复入队）。
pub const IDEMPOTENT_DEPENDENCY_WAKE_STATUSES: &[&str] =
    &["queued", "deferred_issue_execution", "claimed", "completed"];

/// 输入参数 —— 与 Node 入参 1:1 对齐。
#[derive(Debug, Clone)]
pub struct BuildIdempotencyKeyInput {
    pub dependent_issue_id: String,
    pub resolved_blocker_issue_id: String,
}

/// 构造 issue blockers-resolved wakeup 的幂等键。
///
/// 与 Node `buildIssueBlockersResolvedWakeIdempotencyKey` 1:1 对齐：
/// ```ts
/// return [
///   ISSUE_BLOCKERS_RESOLVED_WAKE_REASON,
///   input.dependentIssueId,
///   input.resolvedBlockerIssueId,
/// ].join(":");
/// ```
pub fn build_issue_blockers_resolved_wake_idempotency_key(
    input: &BuildIdempotencyKeyInput,
) -> String {
    format!(
        "{}:{}:{}",
        ISSUE_BLOCKERS_RESOLVED_WAKE_REASON,
        input.dependent_issue_id,
        input.resolved_blocker_issue_id
    )
}

/// 判断 status 是否在幂等集合内。
pub fn is_idempotent_dependency_wake_status(status: &str) -> bool {
    IDEMPOTENT_DEPENDENCY_WAKE_STATUSES.contains(&status)
}

/// 构造 idempotencyKey 列表（用于批量查询）。
///
/// 与 Node 1:1 对齐：
/// ```ts
/// const idempotencyKeys = [...new Set(input.idempotencyKeys.filter(Boolean))];
/// ```
pub fn normalize_idempotency_keys(keys: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();
    for k in keys {
        if k.is_empty() {
            continue;
        }
        if seen.insert(k.clone()) {
            result.push(k.clone());
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r708_wake_reason_constant() {
        assert_eq!(
            ISSUE_BLOCKERS_RESOLVED_WAKE_REASON,
            "issue_blockers_resolved"
        );
    }

    #[test]
    fn r708_idempotent_status_set() {
        assert_eq!(IDEMPOTENT_DEPENDENCY_WAKE_STATUSES.len(), 4);
        for s in ["queued", "deferred_issue_execution", "claimed", "completed"] {
            assert!(is_idempotent_dependency_wake_status(s));
        }
        for s in ["failed", "running", "cancelled", "unknown"] {
            assert!(!is_idempotent_dependency_wake_status(s));
        }
    }

    #[test]
    fn r708_build_idempotency_key() {
        let k = build_issue_blockers_resolved_wake_idempotency_key(&BuildIdempotencyKeyInput {
            dependent_issue_id: "i-1".to_string(),
            resolved_blocker_issue_id: "i-2".to_string(),
        });
        assert_eq!(k, "issue_blockers_resolved:i-1:i-2");
    }

    #[test]
    fn r708_build_idempotency_key_with_uuids() {
        let k = build_issue_blockers_resolved_wake_idempotency_key(&BuildIdempotencyKeyInput {
            dependent_issue_id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            resolved_blocker_issue_id: "550e8400-e29b-41d4-a716-446655440001".to_string(),
        });
        assert_eq!(
            k,
            "issue_blockers_resolved:550e8400-e29b-41d4-a716-446655440000:550e8400-e29b-41d4-a716-446655440001"
        );
    }

    #[test]
    fn r708_normalize_keys_dedup_and_filter_empty() {
        let keys = vec![
            "k1".to_string(),
            "".to_string(),
            "k2".to_string(),
            "k1".to_string(),
            "k3".to_string(),
        ];
        let r = normalize_idempotency_keys(&keys);
        assert_eq!(r, vec!["k1", "k2", "k3"]);
    }

    #[test]
    fn r708_normalize_keys_empty() {
        let r = normalize_idempotency_keys(&[]);
        assert!(r.is_empty());
    }

    #[test]
    fn r708_normalize_keys_only_empty() {
        let keys = vec!["".to_string(), "".to_string()];
        let r = normalize_idempotency_keys(&keys);
        assert!(r.is_empty());
    }

    #[test]
    fn r708_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<BuildIdempotencyKeyInput>();
    }

    // ---- Round 766: pc-issues dependency_wakeups 集成测试 ----

    /// build_issue_blockers_resolved_wake_idempotency_key: 格式正确。
    #[test]
    fn r766_build_wake_idempotency_key_format() {
        let input = BuildIdempotencyKeyInput {
            dependent_issue_id: "i-1".into(),
            resolved_blocker_issue_id: "i-2".into(),
        };
        let key = build_issue_blockers_resolved_wake_idempotency_key(&input);
        assert!(key.contains("i-1"));
        assert!(key.contains("i-2"));
        assert_eq!(key.split(':').count(), 3, "key should have 3 segments");
    }

    /// is_idempotent_dependency_wake_status: 4 个 idempotent statuses (queued/deferred_issue_execution/claimed/completed).
    #[test]
    fn r766_is_idempotent_wake_status_set() {
        assert!(is_idempotent_dependency_wake_status("queued"));
        assert!(is_idempotent_dependency_wake_status("deferred_issue_execution"));
        assert!(is_idempotent_dependency_wake_status("claimed"));
        assert!(is_idempotent_dependency_wake_status("completed"));
        assert!(!is_idempotent_dependency_wake_status("failed"));
        assert!(!is_idempotent_dependency_wake_status("running"));
        assert!(!is_idempotent_dependency_wake_status(""));
    }

    /// normalize_idempotency_keys: 去重 + 跳过空字符串 + 保序。
    #[test]
    fn r766_normalize_idempotency_keys() {
        let keys = vec!["a".into(), "b".into(), "a".into(), "".into(), "c".into(), "b".into()];
        let norm = normalize_idempotency_keys(&keys);
        assert_eq!(norm, vec!["a", "b", "c"]);
    }
}
