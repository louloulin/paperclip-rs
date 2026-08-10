#![forbid(unsafe_code)]
//! `pc-inbox-agent-policy` —— inbox agent policy 纯逻辑 helper。
//!
//! 对应 Node `server/src/services/inbox-agent-policy.ts`（58 行）。
//!
//! 设计目标：1:1 复刻
//! - `InboxAgentPolicy` 默认值构造（未 material 时返回 fallback）
//! - `dedup_agent_ids(ids)` —— `[...new Set(ids)]` 去重并保留首次出现顺序
//! - `find_invalid_agent_ids(allowed, companyAgentIds)` —— 找出不在 company agent 列表里的 id
//!
//! DB 部分（`inboxAgentPolicyService(db)`）由上层接入 pc-repos。

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Inbox policy mode 枚举 —— 与 Node `InboxAgentPolicy["mode"]` 1:1 对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InboxAgentPolicyMode {
    Open,
    Allowlist,
}

impl InboxAgentPolicyMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Allowlist => "allowlist",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "open" => Some(Self::Open),
            "allowlist" => Some(Self::Allowlist),
            _ => None,
        }
    }
}

/// Inbox agent policy —— 与 Node `InboxAgentPolicy` 1:1 对齐。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxAgentPolicy {
    pub company_id: String,
    pub user_id: String,
    pub mode: InboxAgentPolicyMode,
    pub allowed_agent_ids: Vec<String>,
    pub materialized: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// 默认（未 material）的 inbox policy。
///
/// 与 Node fallback object 1:1 对齐。
pub fn default_inbox_agent_policy(company_id: &str, user_id: &str) -> InboxAgentPolicy {
    InboxAgentPolicy {
        company_id: company_id.to_string(),
        user_id: user_id.to_string(),
        mode: InboxAgentPolicyMode::Open,
        allowed_agent_ids: Vec::new(),
        materialized: false,
        created_at: None,
        updated_at: None,
    }
}

/// 去重并保留首次出现顺序。
///
/// 与 Node `[...new Set(input.allowedAgentIds)]` 1:1 对齐。
pub fn dedup_agent_ids(ids: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for id in ids {
        if seen.insert(id.clone()) {
            result.push(id.clone());
        }
    }
    result
}

/// 计算 `allowed_agent_ids` 的最终值（按 mode）。
///
/// 与 Node `update` 入参处理 1:1 对齐：
/// - mode == "allowlist" → dedup 后的 ids
/// - mode == "open" → `[]`
pub fn compute_allowed_agent_ids(mode: InboxAgentPolicyMode, ids: &[String]) -> Vec<String> {
    match mode {
        InboxAgentPolicyMode::Open => Vec::new(),
        InboxAgentPolicyMode::Allowlist => dedup_agent_ids(ids),
    }
}

/// 找出 `allowed` 中不在 `company_agent_ids` 集合里的 id（保留顺序）。
///
/// 与 Node 1:1 对齐：
/// ```ts
/// const invalidAgentIds = allowedAgentIds.filter((agentId) => !companyAgentIds.has(agentId));
/// ```
pub fn find_invalid_agent_ids<'a>(
    allowed: &'a [String],
    company_agent_ids: &HashSet<String>,
) -> Vec<&'a String> {
    allowed
        .iter()
        .filter(|id| !company_agent_ids.contains(*id))
        .collect()
}

/// 变体：接受 HashMap（DB 查询结果转 `Set` 后的 fallback）。
pub fn find_invalid_agent_ids_from_map<'a>(
    allowed: &'a [String],
    company_agent_ids: &HashMap<String, bool>,
) -> Vec<&'a String> {
    allowed
        .iter()
        .filter(|id| !company_agent_ids.contains_key(*id))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r708_default_policy_has_open_mode_and_empty() {
        let p = default_inbox_agent_policy("co1", "u1");
        assert_eq!(p.company_id, "co1");
        assert_eq!(p.user_id, "u1");
        assert_eq!(p.mode, InboxAgentPolicyMode::Open);
        assert!(p.allowed_agent_ids.is_empty());
        assert!(!p.materialized);
        assert!(p.created_at.is_none());
        assert!(p.updated_at.is_none());
    }

    #[test]
    fn r708_mode_round_trip() {
        for m in [InboxAgentPolicyMode::Open, InboxAgentPolicyMode::Allowlist] {
            assert_eq!(InboxAgentPolicyMode::from_str(m.as_str()), Some(m));
        }
        assert_eq!(InboxAgentPolicyMode::from_str("unknown"), None);
    }

    #[test]
    fn r708_dedup_preserves_first() {
        let ids = vec!["a".to_string(), "b".to_string(), "a".to_string(), "c".to_string()];
        assert_eq!(dedup_agent_ids(&ids), vec!["a", "b", "c"]);
    }

    #[test]
    fn r708_dedup_empty() {
        let empty: Vec<String> = vec![];
        assert!(dedup_agent_ids(&empty).is_empty());
    }

    #[test]
    fn r708_dedup_no_duplicates() {
        let ids = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        assert_eq!(dedup_agent_ids(&ids), vec!["a", "b", "c"]);
    }

    #[test]
    fn r708_compute_allowed_open_returns_empty() {
        let ids = vec!["a".to_string(), "b".to_string()];
        assert!(compute_allowed_agent_ids(InboxAgentPolicyMode::Open, &ids).is_empty());
    }

    #[test]
    fn r708_compute_allowed_allowlist_dedups() {
        let ids = vec!["a".to_string(), "b".to_string(), "a".to_string()];
        let r = compute_allowed_agent_ids(InboxAgentPolicyMode::Allowlist, &ids);
        assert_eq!(r, vec!["a", "b"]);
    }

    #[test]
    fn r708_find_invalid_empty() {
        let set: HashSet<String> = ["a".to_string(), "b".to_string()].into_iter().collect();
        let allowed = vec!["a".to_string(), "b".to_string()];
        assert!(find_invalid_agent_ids(&allowed, &set).is_empty());
    }

    #[test]
    fn r708_find_invalid_all_invalid() {
        let set: HashSet<String> = HashSet::new();
        let allowed = vec!["a".to_string(), "b".to_string()];
        let r = find_invalid_agent_ids(&allowed, &set);
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn r708_find_invalid_partial() {
        let set: HashSet<String> = ["a".to_string()].into_iter().collect();
        let allowed = vec!["a".to_string(), "b".to_string()];
        let r = find_invalid_agent_ids(&allowed, &set);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0], "b");
    }

    #[test]
    fn r708_find_invalid_preserves_order() {
        let set: HashSet<String> = HashSet::new();
        let allowed = vec!["c".to_string(), "a".to_string(), "b".to_string()];
        let r = find_invalid_agent_ids(&allowed, &set);
        assert_eq!(r, vec![&"c", &"a", &"b"]);
    }

    #[test]
    fn r708_find_invalid_from_map() {
        let mut map = HashMap::new();
        map.insert("a".to_string(), true);
        let allowed = vec!["a".to_string(), "b".to_string()];
        let r = find_invalid_agent_ids_from_map(&allowed, &map);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0], "b");
    }

    #[test]
    fn r708_serialization_camel_case() {
        let p = default_inbox_agent_policy("co1", "u1");
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["companyId"], "co1");
        assert_eq!(v["userId"], "u1");
        assert_eq!(v["mode"], "open");
        assert_eq!(v["allowedAgentIds"], serde_json::json!([]));
        assert_eq!(v["materialized"], false);
        assert!(v.get("createdAt").is_none());
        assert!(v.get("updatedAt").is_none());
    }

    #[test]
    fn r708_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<InboxAgentPolicy>();
    }
}
