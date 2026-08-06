//! Attention feed 纯函数逻辑（排序 / 分组 / 去重 / dismissal）。
//!
//! 对齐 Node `services/attention.ts`：
//! - 常量 `ATTENTION_SOURCE_KINDS`（11 种 source kind）
//! - 常量 `SEVERITY_RANK`（critical=0, high=1, medium=2, low=3）
//! - 常量 `SOURCE_RANK`（按业务优先级排序：failed_run → recovery_action → ...）
//! - 常量 `PENDING_INTERACTION_STATUSES` / `OPEN_RECOVERY_STATUSES` /
//!   `HUMAN_RECOVERY_OWNER_TYPES` / `PRODUCTIVITY_REVIEW_TERMINAL_STATUSES` /
//!   `FAILED_RUN_STATUSES`
//! - 常量 `DETAIL_EXCERPT_LENGTH = 160` / `DETAIL_IMAGE_LIMIT = 3` /
//!   `OPEN_DECISION_DEFAULT_LIMIT = 500` / `OPEN_DECISION_MAX_LIMIT = 1_000`
//! - 类型 `AttentionSourceKind` / `AttentionSeverity` /
//!   `AttentionDecisionVerb` / `AttentionSubjectKind` /
//!   `AttentionItem` / `AttentionSubject` / `AttentionItemDetail` /
//!   `AttentionProjectRef` / `AttentionWorkspaceRef` /
//!   `AttentionDismissal` / `AttentionDetailImage` /
//!   `AttentionFeed` / `AttentionFeedCount` / `AttentionFeedOptions`
//! - 函数 `compare_attention_items(left, right)` —— 多维排序
//! - 函数 `better_duplicate(left, right)` —— 比较挑较优者
//! - 函数 `compute_dismissal_key(source_kind, dedup_key)` —— dismissal 唯一键
//! - 函数 `clamp_open_decision_limit(value, default)` —— 限制 limit 范围
//! - 函数 `summarize_counts(items)` —— 聚合各 source kind 的计数
//! - 函数 `is_active_dismissal(dismissal, now_ms)` —— 判断 dismissal 是否仍在有效期内
//! - 函数 `filter_dismissed(items, dismissals, now_ms)` —— 过滤被 dismiss 的项目
//!
//! 设计：
//! - 纯函数无副作用，方便单测
//! - 字符串字面量与 Node 完全一致（通过 serde rename）
//! - 用 `BTreeMap` 保证 reproducible 排序
//! - `AttentionItem` 字段顺序与 Node 1:1（camelCase via serde）

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ============================================================================
// Constants
// ============================================================================

/// Detail 摘要最大字符数（unicode-aware）。
pub const DETAIL_EXCERPT_LENGTH: usize = 160;

/// Detail 图片最大数量。
pub const DETAIL_IMAGE_LIMIT: usize = 3;

/// Open decision 列表默认 limit。
pub const OPEN_DECISION_DEFAULT_LIMIT: u32 = 500;

/// Open decision 列表最大 limit。
pub const OPEN_DECISION_MAX_LIMIT: u32 = 1_000;

/// Pending interaction 状态集合（["pending"]）。
pub const PENDING_INTERACTION_STATUSES: &[&str] = &["pending"];

/// Open recovery action 状态集合（["active", "escalated"]）。
pub const OPEN_RECOVERY_STATUSES: &[&str] = &["active", "escalated"];

/// Human recovery owner 类型集合（["user", "board"]）。
pub const HUMAN_RECOVERY_OWNER_TYPES: &[&str] = &["user", "board"];

/// Productivity review 终止状态集合（["done", "cancelled"]）。
pub const PRODUCTIVITY_REVIEW_TERMINAL_STATUSES: &[&str] = &["done", "cancelled"];

/// Failed run 状态集合（["failed", "timed_out"]）。
pub const FAILED_RUN_STATUSES: &[&str] = &["failed", "timed_out"];

// ============================================================================
// Enums
// ============================================================================

/// Attention source kind（与 Node `AttentionSourceKind` 1:1 对齐）。
///
/// 字符串字面量与 Node 完全一致，便于跨语言日志对照 + UI type 字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AttentionSourceKind {
    #[serde(rename = "approval")]
    Approval,
    #[serde(rename = "decision")]
    Decision,
    #[serde(rename = "issue_thread_interaction")]
    IssueThreadInteraction,
    #[serde(rename = "join_request")]
    JoinRequest,
    #[serde(rename = "recovery_action")]
    RecoveryAction,
    #[serde(rename = "productivity_review")]
    ProductivityReview,
    #[serde(rename = "blocker_attention")]
    BlockerAttention,
    #[serde(rename = "review")]
    Review,
    #[serde(rename = "failed_run")]
    FailedRun,
    #[serde(rename = "budget_alert")]
    BudgetAlert,
    #[serde(rename = "agent_error_alert")]
    AgentErrorAlert,
}

impl AttentionSourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approval => "approval",
            Self::Decision => "decision",
            Self::IssueThreadInteraction => "issue_thread_interaction",
            Self::JoinRequest => "join_request",
            Self::RecoveryAction => "recovery_action",
            Self::ProductivityReview => "productivity_review",
            Self::BlockerAttention => "blocker_attention",
            Self::Review => "review",
            Self::FailedRun => "failed_run",
            Self::BudgetAlert => "budget_alert",
            Self::AgentErrorAlert => "agent_error_alert",
        }
    }

    /// 业务优先级排序（与 Node `SOURCE_RANK` 1:1 对齐）。
    /// 数字越小优先级越高。
    pub fn rank(self) -> u32 {
        match self {
            Self::FailedRun => 0,
            Self::RecoveryAction => 1,
            Self::BlockerAttention => 2,
            Self::BudgetAlert => 3,
            Self::AgentErrorAlert => 4,
            Self::Approval => 5,
            Self::Decision => 6,
            Self::IssueThreadInteraction => 7,
            Self::Review => 8,
            Self::ProductivityReview => 9,
            Self::JoinRequest => 10,
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "approval" => Some(Self::Approval),
            "decision" => Some(Self::Decision),
            "issue_thread_interaction" => Some(Self::IssueThreadInteraction),
            "join_request" => Some(Self::JoinRequest),
            "recovery_action" => Some(Self::RecoveryAction),
            "productivity_review" => Some(Self::ProductivityReview),
            "blocker_attention" => Some(Self::BlockerAttention),
            "review" => Some(Self::Review),
            "failed_run" => Some(Self::FailedRun),
            "budget_alert" => Some(Self::BudgetAlert),
            "agent_error_alert" => Some(Self::AgentErrorAlert),
            _ => None,
        }
    }
}

/// 所有 source kinds（按 rank 升序，与 Node `ATTENTION_SOURCE_KINDS` 顺序一致）。
pub const ALL_SOURCE_KINDS: &[AttentionSourceKind] = &[
    AttentionSourceKind::Approval,
    AttentionSourceKind::Decision,
    AttentionSourceKind::IssueThreadInteraction,
    AttentionSourceKind::JoinRequest,
    AttentionSourceKind::RecoveryAction,
    AttentionSourceKind::ProductivityReview,
    AttentionSourceKind::BlockerAttention,
    AttentionSourceKind::Review,
    AttentionSourceKind::FailedRun,
    AttentionSourceKind::BudgetAlert,
    AttentionSourceKind::AgentErrorAlert,
];

/// Attention severity（与 Node `AttentionSeverity` 1:1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AttentionSeverity {
    #[serde(rename = "critical")]
    Critical,
    #[serde(rename = "high")]
    High,
    #[serde(rename = "medium")]
    Medium,
    #[serde(rename = "low")]
    Low,
}

impl AttentionSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
        }
    }

    /// 严重度排序（与 Node `SEVERITY_RANK` 1:1）。
    pub fn rank(self) -> u32 {
        match self {
            Self::Critical => 0,
            Self::High => 1,
            Self::Medium => 2,
            Self::Low => 3,
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "critical" => Some(Self::Critical),
            "high" => Some(Self::High),
            "medium" => Some(Self::Medium),
            "low" => Some(Self::Low),
            _ => None,
        }
    }
}

/// Attention decision verb（与 Node `AttentionDecisionVerb` 1:1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AttentionDecisionVerb {
    #[serde(rename = "approve")]
    Approve,
    #[serde(rename = "reject")]
    Reject,
    #[serde(rename = "acknowledge")]
    Acknowledge,
    #[serde(rename = "snooze")]
    Snooze,
    #[serde(rename = "open")]
    Open,
    #[serde(rename = "review")]
    Review,
    #[serde(rename = "escalate")]
    Escalate,
    #[serde(rename = "restart")]
    Restart,
    #[serde(rename = "dismiss")]
    Dismiss,
}

impl AttentionDecisionVerb {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Reject => "reject",
            Self::Acknowledge => "acknowledge",
            Self::Snooze => "snooze",
            Self::Open => "open",
            Self::Review => "review",
            Self::Escalate => "escalate",
            Self::Restart => "restart",
            Self::Dismiss => "dismiss",
        }
    }
}

// ============================================================================
// DTOs
// ============================================================================

/// Attention 项目（与 Node `AttentionItem` 字段 1:1）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttentionItem {
    pub id: String,
    pub company_id: String,
    pub source_kind: AttentionSourceKind,
    pub severity: AttentionSeverity,
    pub subject: AttentionSubject,
    pub activity_at: DateTime<Utc>,
    pub dedup_key: String,
    pub dismissal_key: String,
    pub rank: i32,
    #[serde(default)]
    pub dismissal: Option<AttentionDismissal>,
    #[serde(default)]
    pub project: Option<AttentionProjectRef>,
    #[serde(default)]
    pub workspace: Option<AttentionWorkspaceRef>,
    #[serde(default)]
    pub detail: Option<AttentionItemDetail>,
    #[serde(default)]
    pub training_example_id: Option<String>,
    #[serde(default)]
    pub decision_verbs: Vec<AttentionDecisionVerb>,
    #[serde(default)]
    pub related_count: i32,
    #[serde(default)]
    pub reason_codes: Vec<String>,
}

/// Attention 主题（kind + id + title）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum AttentionSubject {
    #[serde(rename = "approval")]
    Approval {
        id: String,
        company_id: String,
        title: String,
        identifier: Option<String>,
    },
    #[serde(rename = "decision")]
    Decision {
        id: String,
        company_id: String,
        title: String,
        identifier: Option<String>,
    },
    #[serde(rename = "issue_thread_interaction")]
    IssueThreadInteraction {
        id: String,
        company_id: String,
        title: String,
        identifier: Option<String>,
    },
    #[serde(rename = "join_request")]
    JoinRequest {
        id: String,
        company_id: String,
        title: String,
        identifier: Option<String>,
    },
    #[serde(rename = "recovery_action")]
    RecoveryAction {
        id: String,
        company_id: String,
        title: String,
        identifier: Option<String>,
    },
    #[serde(rename = "productivity_review")]
    ProductivityReview {
        id: String,
        company_id: String,
        title: String,
        identifier: Option<String>,
    },
    #[serde(rename = "blocker_attention")]
    Blocker {
        id: String,
        company_id: String,
        title: String,
        identifier: Option<String>,
    },
    #[serde(rename = "review")]
    Review {
        id: String,
        company_id: String,
        title: String,
        identifier: Option<String>,
    },
    #[serde(rename = "failed_run")]
    FailedRun {
        id: String,
        company_id: String,
        title: String,
        identifier: Option<String>,
    },
    #[serde(rename = "budget_alert")]
    BudgetAlert {
        id: String,
        company_id: String,
        title: String,
        identifier: Option<String>,
    },
    #[serde(rename = "agent_error_alert")]
    AgentErrorAlert {
        id: String,
        company_id: String,
        title: String,
        identifier: Option<String>,
    },
}

/// Attention 项目 detail（与 Node `AttentionItemDetail` 1:1，使用 tagged enum）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum AttentionItemDetail {
    #[serde(rename = "generic")]
    Generic {
        summary_excerpt: Option<String>,
        images: Vec<AttentionDetailImage>,
    },
    #[serde(rename = "approval")]
    Approval {
        approval_type: String,
        payload: Value,
    },
    #[serde(rename = "interaction")]
    Interaction {
        interaction_kind: String,
        prompt: String,
        images: Vec<AttentionDetailImage>,
    },
    #[serde(rename = "blocker")]
    Blocker {
        reason: String,
        dependency_path: Vec<String>,
    },
    #[serde(rename = "failed_run")]
    FailedRun {
        agent_name: Option<String>,
        exit_code: Option<i32>,
        signal: Option<String>,
        timed_out: bool,
        first_error_line: Option<String>,
    },
    #[serde(rename = "budget_alert")]
    BudgetAlert {
        kind: String,
        current_amount_cents: i64,
        limit_amount_cents: i64,
        utilization: f64,
    },
    #[serde(rename = "recovery_action")]
    RecoveryAction {
        kind: String,
        status: String,
        fingerprint: String,
        evidence_excerpt: Option<String>,
    },
}

impl AttentionItemDetail {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Generic { .. } => "generic",
            Self::Approval { .. } => "approval",
            Self::Interaction { .. } => "interaction",
            Self::Blocker { .. } => "blocker",
            Self::FailedRun { .. } => "failed_run",
            Self::BudgetAlert { .. } => "budget_alert",
            Self::RecoveryAction { .. } => "recovery_action",
        }
    }
}

/// Attention 项目关联图片。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttentionDetailImage {
    pub url: String,
    #[serde(default)]
    pub alt: Option<String>,
    #[serde(default)]
    pub caption: Option<String>,
}

/// Attention 项目 project 引用。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttentionProjectRef {
    pub id: String,
    pub slug: Option<String>,
    pub name: String,
}

/// Attention 项目 workspace 引用。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttentionWorkspaceRef {
    pub id: String,
    pub slug: Option<String>,
    pub name: String,
}

/// Attention 项目 dismissal。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttentionDismissal {
    pub user_id: Option<String>,
    pub dismissed_at: DateTime<Utc>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    pub source: String,
    pub is_active: bool,
}

/// Attention feed（顶层结构）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AttentionFeed {
    pub company_id: String,
    pub items: Vec<AttentionItem>,
    pub counts: BTreeMap<AttentionSourceKind, i64>,
    #[serde(default)]
    pub total: i64,
    #[serde(default)]
    pub generated_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub prefix: Option<String>,
}

/// Feed options。
#[derive(Debug, Clone, Default)]
pub struct AttentionFeedOptions {
    pub user_id: Option<String>,
    pub include_dismissed: bool,
    pub limit: Option<u32>,
}

// ============================================================================
// Pure functions
// ============================================================================

/// 计算 dismissal 唯一 key（与 Node `itemId(sourceKind, dedupKey)` 1:1）。
///
/// 格式：`{source_kind}:{dedup_key}`
pub fn compute_dismissal_key(source_kind: AttentionSourceKind, dedup_key: &str) -> String {
    format!("{}:{}", source_kind.as_str(), dedup_key)
}

/// 多维排序 attention items（与 Node `compareAttentionItems` 1:1）。
///
/// 排序维度（按优先级）：
/// 1. activityAt 降序（最新活动在前）
/// 2. severity 升序（critical 在前）
/// 3. sourceKind 升序（业务优先级高的在前）
/// 4. dedupKey 升序（确定性 tie-break）
pub fn compare_attention_items(left: &AttentionItem, right: &AttentionItem) -> std::cmp::Ordering {
    // 1. activityAt 降序：Node 返回 `right.activityAt - left.activityAt`，等价于
    //    `right.cmp(&left)` 的反向，即 newer（在 left 时）排前面。
    let time_diff = right.activity_at.timestamp_millis() - left.activity_at.timestamp_millis();
    if time_diff != 0 {
        return time_diff.cmp(&0);
    }
    // 2. severity 升序（critical 在前 → rank 小在前）
    let severity_diff = left.severity.rank() as i64 - right.severity.rank() as i64;
    if severity_diff != 0 {
        return if severity_diff < 0 {
            std::cmp::Ordering::Less
        } else {
            std::cmp::Ordering::Greater
        };
    }
    // 3. sourceKind 升序（rank 小在前）
    let source_diff = left.source_kind.rank() as i64 - right.source_kind.rank() as i64;
    if source_diff != 0 {
        return if source_diff < 0 {
            std::cmp::Ordering::Less
        } else {
            std::cmp::Ordering::Greater
        };
    }
    // 4. dedupKey 升序
    left.dedup_key.cmp(&right.dedup_key)
}

/// 挑较优者（与 Node `betterDuplicate` 1:1）。
pub fn better_duplicate<'a>(
    left: &'a AttentionItem,
    right: &'a AttentionItem,
) -> &'a AttentionItem {
    if compare_attention_items(left, right) <= std::cmp::Ordering::Equal {
        left
    } else {
        right
    }
}

/// 排序 attention items（不可变）。
pub fn sort_attention_items(mut items: Vec<AttentionItem>) -> Vec<AttentionItem> {
    items.sort_by(compare_attention_items);
    items
}

/// 限制 open decision limit 范围。
///
/// 默认 `OPEN_DECISION_DEFAULT_LIMIT = 500`，最大 `OPEN_DECISION_MAX_LIMIT = 1_000`，
/// 最小 1。
pub fn clamp_open_decision_limit(value: Option<u32>) -> u32 {
    let raw = value.unwrap_or(OPEN_DECISION_DEFAULT_LIMIT);
    raw.clamp(1, OPEN_DECISION_MAX_LIMIT)
}

/// 聚合各 source kind 的计数。
pub fn summarize_counts(items: &[AttentionItem]) -> BTreeMap<AttentionSourceKind, i64> {
    let mut counts: BTreeMap<AttentionSourceKind, i64> = BTreeMap::new();
    for kind in ALL_SOURCE_KINDS {
        counts.insert(*kind, 0);
    }
    for item in items {
        *counts.entry(item.source_kind).or_insert(0) += 1;
    }
    counts
}

/// 判断 dismissal 是否仍在有效期内（与 Node `activeDismissalState` 1:1）。
///
/// - `dismissal.is_active == false` → false
/// - `dismissal.expires_at` 设置且 ≤ now → false
/// - 否则 true
pub fn is_active_dismissal(dismissal: &AttentionDismissal, now_ms: i64) -> bool {
    if !dismissal.is_active {
        return false;
    }
    if let Some(expires_at) = dismissal.expires_at {
        if expires_at.timestamp_millis() <= now_ms {
            return false;
        }
    }
    true
}

/// 用 dismissal 表过滤 items（与 Node `add` 闭包 1:1）。
///
/// - 若 `include_dismissed == false` 且对应 dismissal 仍 active，则跳过该 item
/// - 否则把 item 复制并附上 dismissal 字段
pub fn filter_dismissed(
    items: Vec<AttentionItem>,
    dismissals: &BTreeMap<String, AttentionDismissal>,
    include_dismissed: bool,
    now_ms: i64,
) -> Vec<AttentionItem> {
    items
        .into_iter()
        .filter_map(|item| {
            let dismissal = dismissals.get(&item.dismissal_key);
            let active = dismissal
                .map(|d| is_active_dismissal(d, now_ms))
                .unwrap_or(false);
            if !include_dismissed && active {
                return None;
            }
            Some(AttentionItem {
                dismissal: dismissal.cloned(),
                ..item
            })
        })
        .collect()
}

/// 截取字符串摘要（unicode-aware，对齐 Node `excerpt`）。
pub fn excerpt(value: Option<&str>, max_length: usize) -> Option<String> {
    let s = value?;
    let cleaned = s.trim();
    if cleaned.is_empty() {
        return None;
    }
    let count = cleaned.chars().count();
    if count <= max_length {
        return Some(cleaned.to_string());
    }
    let truncated: String = cleaned.chars().take(max_length.saturating_sub(1)).collect();
    Some(format!("{}…", truncated.trim_end()))
}

/// 字符串转 ISO 8601（与 Node `toIso` 1:1）。
pub fn to_iso(value: Option<DateTime<Utc>>) -> Option<String> {
    value.map(|dt| dt.to_rfc3339())
}

/// 时间戳毫秒数（与 Node `timestamp` 1:1，null → 0）。
pub fn timestamp_ms(value: Option<DateTime<Utc>>) -> i64 {
    match value {
        Some(dt) => {
            let ms = dt.timestamp_millis();
            ms
        }
        None => 0,
    }
}

/// 决定 unique item ids 集合（去重）。
pub fn unique_dedup_keys(items: &[AttentionItem]) -> BTreeSet<String> {
    items.iter().map(|i| i.dedup_key.clone()).collect()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ts(year: i32, month: u32, day: u32, hour: u32, min: u32) -> DateTime<Utc> {
        chrono::Utc
            .with_ymd_and_hms(year, month, day, hour, min, 0)
            .unwrap()
    }

    fn sample_item(
        id: &str,
        source: AttentionSourceKind,
        severity: AttentionSeverity,
        at: DateTime<Utc>,
    ) -> AttentionItem {
        AttentionItem {
            id: id.to_string(),
            company_id: "company-1".to_string(),
            source_kind: source,
            severity,
            subject: AttentionSubject::Approval {
                id: "approval-1".to_string(),
                company_id: "company-1".to_string(),
                title: "Approve".to_string(),
                identifier: None,
            },
            activity_at: at,
            dedup_key: id.to_string(),
            dismissal_key: compute_dismissal_key(source, id),
            rank: 0,
            dismissal: None,
            project: None,
            workspace: None,
            detail: None,
            training_example_id: None,
            decision_verbs: vec![],
            related_count: 0,
            reason_codes: vec![],
        }
    }

    #[test]
    fn source_kind_string_round_trip() {
        for kind in ALL_SOURCE_KINDS {
            let s = kind.as_str();
            assert_eq!(AttentionSourceKind::from_str(s), Some(*kind));
        }
        assert_eq!(AttentionSourceKind::from_str("bogus"), None);
    }

    #[test]
    fn severity_rank_ordering() {
        // critical=0, high=1, medium=2, low=3
        assert!(AttentionSeverity::Critical.rank() < AttentionSeverity::High.rank());
        assert!(AttentionSeverity::High.rank() < AttentionSeverity::Medium.rank());
        assert!(AttentionSeverity::Medium.rank() < AttentionSeverity::Low.rank());
    }

    #[test]
    fn source_kind_business_priority_ordering() {
        // failed_run (0) → recovery_action (1) → blocker_attention (2) → ...
        assert!(AttentionSourceKind::FailedRun.rank() < AttentionSourceKind::RecoveryAction.rank());
        assert!(
            AttentionSourceKind::RecoveryAction.rank()
                < AttentionSourceKind::BlockerAttention.rank()
        );
        assert!(
            AttentionSourceKind::BlockerAttention.rank() < AttentionSourceKind::BudgetAlert.rank()
        );
        assert!(AttentionSourceKind::BudgetAlert.rank() < AttentionSourceKind::Approval.rank());
        assert!(AttentionSourceKind::Approval.rank() < AttentionSourceKind::Decision.rank());
        assert!(
            AttentionSourceKind::JoinRequest.rank()
                > AttentionSourceKind::ProductivityReview.rank()
        );
    }

    #[test]
    fn compare_orders_by_time_first_desc() {
        let newer = sample_item(
            "newer",
            AttentionSourceKind::Approval,
            AttentionSeverity::Low,
            ts(2025, 1, 1, 12, 0),
        );
        let older = sample_item(
            "older",
            AttentionSourceKind::Approval,
            AttentionSeverity::Low,
            ts(2025, 1, 1, 11, 0),
        );
        assert_eq!(
            compare_attention_items(&newer, &older),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn compare_orders_by_severity_when_same_time() {
        let same_time = ts(2025, 1, 1, 12, 0);
        let critical = sample_item(
            "c",
            AttentionSourceKind::Approval,
            AttentionSeverity::Critical,
            same_time,
        );
        let high = sample_item(
            "h",
            AttentionSourceKind::Approval,
            AttentionSeverity::High,
            same_time,
        );
        let medium = sample_item(
            "m",
            AttentionSourceKind::Approval,
            AttentionSeverity::Medium,
            same_time,
        );
        assert_eq!(
            compare_attention_items(&critical, &high),
            std::cmp::Ordering::Less
        );
        assert_eq!(
            compare_attention_items(&high, &medium),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn compare_orders_by_source_when_same_time_and_severity() {
        let same_time = ts(2025, 1, 1, 12, 0);
        let failed = sample_item(
            "f",
            AttentionSourceKind::FailedRun,
            AttentionSeverity::Critical,
            same_time,
        );
        let approval = sample_item(
            "a",
            AttentionSourceKind::Approval,
            AttentionSeverity::Critical,
            same_time,
        );
        assert_eq!(
            compare_attention_items(&failed, &approval),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn compare_orders_by_dedup_key_tie_break() {
        let same_time = ts(2025, 1, 1, 12, 0);
        let a = sample_item(
            "alpha",
            AttentionSourceKind::Approval,
            AttentionSeverity::Critical,
            same_time,
        );
        let b = sample_item(
            "beta",
            AttentionSourceKind::Approval,
            AttentionSeverity::Critical,
            same_time,
        );
        assert_eq!(compare_attention_items(&a, &b), std::cmp::Ordering::Less);
    }

    #[test]
    fn better_duplicate_picks_lower() {
        let same_time = ts(2025, 1, 1, 12, 0);
        let newer = sample_item(
            "newer",
            AttentionSourceKind::FailedRun,
            AttentionSeverity::Critical,
            same_time,
        );
        let older = sample_item(
            "older",
            AttentionSourceKind::Approval,
            AttentionSeverity::Low,
            ts(2025, 1, 1, 11, 0),
        );
        assert_eq!(better_duplicate(&newer, &older).id, "newer");
        assert_eq!(better_duplicate(&older, &newer).id, "newer");
    }

    #[test]
    fn sort_attention_items_orders_correctly() {
        let items = vec![
            sample_item(
                "low_old",
                AttentionSourceKind::JoinRequest,
                AttentionSeverity::Low,
                ts(2025, 1, 1, 11, 0),
            ),
            sample_item(
                "critical_new",
                AttentionSourceKind::Approval,
                AttentionSeverity::Critical,
                ts(2025, 1, 1, 12, 0),
            ),
            sample_item(
                "medium_new",
                AttentionSourceKind::Approval,
                AttentionSeverity::Medium,
                ts(2025, 1, 1, 12, 0),
            ),
            sample_item(
                "high_new",
                AttentionSourceKind::Approval,
                AttentionSeverity::High,
                ts(2025, 1, 1, 12, 0),
            ),
        ];
        let sorted = sort_attention_items(items);
        assert_eq!(sorted[0].id, "critical_new");
        assert_eq!(sorted[1].id, "high_new");
        assert_eq!(sorted[2].id, "medium_new");
        assert_eq!(sorted[3].id, "low_old");
    }

    #[test]
    fn clamp_open_decision_limit_clamps() {
        assert_eq!(clamp_open_decision_limit(None), 500);
        assert_eq!(clamp_open_decision_limit(Some(0)), 1);
        assert_eq!(clamp_open_decision_limit(Some(100)), 100);
        assert_eq!(clamp_open_decision_limit(Some(5_000)), 1_000);
    }

    #[test]
    fn summarize_counts_aggregates() {
        let items = vec![
            sample_item(
                "a",
                AttentionSourceKind::Approval,
                AttentionSeverity::Low,
                ts(2025, 1, 1, 12, 0),
            ),
            sample_item(
                "b",
                AttentionSourceKind::Approval,
                AttentionSeverity::Low,
                ts(2025, 1, 1, 12, 0),
            ),
            sample_item(
                "c",
                AttentionSourceKind::FailedRun,
                AttentionSeverity::Low,
                ts(2025, 1, 1, 12, 0),
            ),
        ];
        let counts = summarize_counts(&items);
        assert_eq!(counts.get(&AttentionSourceKind::Approval), Some(&2));
        assert_eq!(counts.get(&AttentionSourceKind::FailedRun), Some(&1));
        assert_eq!(counts.get(&AttentionSourceKind::RecoveryAction), Some(&0));
        // All 11 kinds must be present
        assert_eq!(counts.len(), 11);
    }

    #[test]
    fn is_active_dismissal_when_inactive() {
        let d = AttentionDismissal {
            user_id: Some("alice".to_string()),
            dismissed_at: ts(2025, 1, 1, 12, 0),
            expires_at: None,
            source: "user".to_string(),
            is_active: false,
        };
        assert!(!is_active_dismissal(
            &d,
            ts(2025, 1, 1, 13, 0).timestamp_millis()
        ));
    }

    #[test]
    fn is_active_dismissal_when_expired() {
        let d = AttentionDismissal {
            user_id: Some("alice".to_string()),
            dismissed_at: ts(2025, 1, 1, 12, 0),
            expires_at: Some(ts(2025, 1, 1, 13, 0)),
            source: "user".to_string(),
            is_active: true,
        };
        assert!(!is_active_dismissal(
            &d,
            ts(2025, 1, 1, 13, 30).timestamp_millis()
        ));
    }

    #[test]
    fn is_active_dismissal_when_not_expired() {
        let d = AttentionDismissal {
            user_id: Some("alice".to_string()),
            dismissed_at: ts(2025, 1, 1, 12, 0),
            expires_at: Some(ts(2025, 1, 1, 13, 0)),
            source: "user".to_string(),
            is_active: true,
        };
        assert!(is_active_dismissal(
            &d,
            ts(2025, 1, 1, 12, 30).timestamp_millis()
        ));
    }

    #[test]
    fn filter_dismissed_drops_active() {
        let item = sample_item(
            "a",
            AttentionSourceKind::Approval,
            AttentionSeverity::Low,
            ts(2025, 1, 1, 12, 0),
        );
        let mut dismissals = BTreeMap::new();
        dismissals.insert(
            item.dismissal_key.clone(),
            AttentionDismissal {
                user_id: Some("alice".to_string()),
                dismissed_at: ts(2025, 1, 1, 12, 0),
                expires_at: None,
                source: "user".to_string(),
                is_active: true,
            },
        );
        let filtered = filter_dismissed(
            vec![item.clone()],
            &dismissals,
            false,
            ts(2025, 1, 1, 13, 0).timestamp_millis(),
        );
        assert!(filtered.is_empty());

        let kept = filter_dismissed(
            vec![item.clone()],
            &dismissals,
            true,
            ts(2025, 1, 1, 13, 0).timestamp_millis(),
        );
        assert_eq!(kept.len(), 1);
        assert!(kept[0].dismissal.is_some());
    }

    #[test]
    fn compute_dismissal_key_format() {
        assert_eq!(
            compute_dismissal_key(AttentionSourceKind::Approval, "abc"),
            "approval:abc"
        );
    }

    #[test]
    fn excerpt_truncates_unicode() {
        let long: String = "界".repeat(200);
        let truncated = excerpt(Some(&long), 10).unwrap();
        assert!(truncated.chars().count() <= 10);
        assert!(truncated.ends_with('…'));
    }

    #[test]
    fn excerpt_returns_none_for_empty() {
        assert_eq!(excerpt(None, 10), None);
        assert_eq!(excerpt(Some(""), 10), None);
        assert_eq!(excerpt(Some("   "), 10), None);
    }

    #[test]
    fn excerpt_keeps_short_strings() {
        assert_eq!(excerpt(Some("hello"), 100), Some("hello".to_string()));
    }

    #[test]
    fn timestamp_ms_handles_none() {
        assert_eq!(timestamp_ms(None), 0);
        assert_eq!(timestamp_ms(Some(ts(2025, 1, 1, 0, 0))), 1735689600000);
    }

    #[test]
    fn unique_dedup_keys_dedupes() {
        let items = vec![
            sample_item(
                "a",
                AttentionSourceKind::Approval,
                AttentionSeverity::Low,
                ts(2025, 1, 1, 12, 0),
            ),
            sample_item(
                "a",
                AttentionSourceKind::Approval,
                AttentionSeverity::Low,
                ts(2025, 1, 1, 12, 0),
            ),
            sample_item(
                "b",
                AttentionSourceKind::Approval,
                AttentionSeverity::Low,
                ts(2025, 1, 1, 12, 0),
            ),
        ];
        let unique = unique_dedup_keys(&items);
        assert_eq!(unique.len(), 2);
        assert!(unique.contains("a"));
        assert!(unique.contains("b"));
    }

    #[test]
    fn pending_interaction_statuses_is_pending() {
        assert_eq!(PENDING_INTERACTION_STATUSES, &["pending"]);
    }

    #[test]
    fn open_recovery_statuses_match_node() {
        assert_eq!(OPEN_RECOVERY_STATUSES, &["active", "escalated"]);
    }

    #[test]
    fn detail_kind_tagged_enum() {
        let d = AttentionItemDetail::Generic {
            summary_excerpt: Some("summary".to_string()),
            images: vec![],
        };
        assert_eq!(d.kind(), "generic");
    }

    #[test]
    fn decision_verb_string_round_trip() {
        assert_eq!(AttentionDecisionVerb::Approve.as_str(), "approve");
        assert_eq!(AttentionDecisionVerb::Dismiss.as_str(), "dismiss");
        assert_eq!(AttentionDecisionVerb::Escalate.as_str(), "escalate");
    }
}

/// Node `interactionLabel` 的纯函数复刻。
pub fn interaction_label(kind: &str) -> &'static str {
    match kind {
        "request_confirmation" => "Confirmation requested",
        "request_checkbox_confirmation" => "Selection confirmation requested",
        "ask_user_questions" => "Questions need answers",
        "suggest_tasks" => "Suggested tasks need a decision",
        "request_item_verdicts" => "Item verdicts need a decision",
        _ => "Interaction needs a decision",
    }
}

/// Node `interactionVerbs` 的可序列化结果。
pub fn interaction_verbs(kind: &str, payload: &Value) -> Vec<Value> {
    let verb = |id: &str, label: String, description: &str| serde_json::json!({"id": id, "label": label, "description": description});
    if kind == "ask_user_questions" {
        return vec![verb(
            "respond",
            "Respond".into(),
            "Submit answers to the pending questions.",
        )];
    }
    if kind == "request_confirmation" {
        let label = |key: &str, fallback: &str| {
            payload
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .unwrap_or(fallback)
                .to_owned()
        };
        return vec![
            verb(
                "accept",
                label("acceptLabel", "Confirm"),
                "Accept the pending confirmation.",
            ),
            verb(
                "reject",
                label("rejectLabel", "Decline"),
                "Decline the pending confirmation.",
            ),
        ];
    }
    vec![
        verb("accept", "Accept".into(), "Accept the pending interaction."),
        verb(
            "reject",
            "Reject".into(),
            "Reject the pending interaction and provide a reason when required.",
        ),
    ]
}

#[cfg(test)]
mod interaction_rules_tests {
    use super::*;
    #[test]
    fn labels_match_node_interaction_kinds() {
        assert_eq!(
            interaction_label("ask_user_questions"),
            "Questions need answers"
        );
        assert_eq!(interaction_label("unknown"), "Interaction needs a decision");
    }
    #[test]
    fn confirmation_uses_trimmed_custom_labels() {
        let verbs = interaction_verbs(
            "request_confirmation",
            &serde_json::json!({"acceptLabel":"  Ship  ", "rejectLabel":""}),
        );
        assert_eq!(verbs[0]["label"], "Ship");
        assert_eq!(verbs[1]["label"], "Decline");
    }
    #[test]
    fn questions_have_respond_only() {
        let verbs = interaction_verbs("ask_user_questions", &serde_json::json!({}));
        assert_eq!(verbs.len(), 1);
        assert_eq!(verbs[0]["id"], "respond");
    }
}

/// Node `interactionDetail` 的纯函数部分；外部 issue/文档/images 上下文由调用方补充。
pub fn interaction_detail(kind: &str, payload: &Value) -> Value {
    let record = payload.as_object();
    let get = |key: &str| record.and_then(|v| v.get(key));
    let array_len = |key: &str| get(key).and_then(Value::as_array).map_or(0, Vec::len);
    let excerpt_value = |key: &str| {
        get(key)
            .and_then(Value::as_str)
            .and_then(|v| excerpt(Some(v), DETAIL_EXCERPT_LENGTH))
    };
    if kind == "ask_user_questions" {
        let first = get("questions")
            .and_then(Value::as_array)
            .and_then(|v| v.first())
            .and_then(|v| v.get("prompt"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        return serde_json::json!({"kind":"questions","questionCount":array_len("questions"),"firstQuestionText":first,"images":[]});
    }
    if kind == "suggest_tasks" {
        return serde_json::json!({"kind":"suggested_tasks","taskCount":array_len("tasks"),"firstTaskTitle":get("tasks").and_then(Value::as_array).and_then(|v| v.first()).and_then(|v| v.get("title")).and_then(Value::as_str),"images":[]});
    }
    if kind == "request_checkbox_confirmation" {
        return serde_json::json!({"kind":"checkbox_confirmation","optionCount":array_len("options"),"promptExcerpt":excerpt_value("prompt"),"images":[]});
    }
    if kind == "request_item_verdicts" {
        return serde_json::json!({"kind":"item_verdicts","itemCount":array_len("items"),"promptExcerpt":excerpt_value("prompt"),"images":[]});
    }
    let plan_target = kind == "request_confirmation"
        && get("target")
            .and_then(Value::as_object)
            .is_some_and(|target| {
                target.get("type").and_then(Value::as_str) == Some("issue_document")
                    && target.get("key").and_then(Value::as_str) == Some("plan")
            });
    if plan_target {
        return serde_json::json!({"kind":"plan_approval","issueTitle":null,"planTitle":"Plan","summaryExcerpt":excerpt_value("detailsMarkdown").or_else(|| excerpt_value("prompt")),"images":[]});
    }
    serde_json::json!({"kind":"confirmation","promptExcerpt":excerpt_value("prompt").or_else(|| excerpt_value("detailsMarkdown")),"isPlanTarget":false,"images":[]})
}

#[cfg(test)]
mod interaction_detail_tests {
    use super::*;
    #[test]
    fn classifies_question_detail() {
        let d = interaction_detail(
            "ask_user_questions",
            &serde_json::json!({"questions":[{"prompt":"Need input"}]}),
        );
        assert_eq!(d["kind"], "questions");
        assert_eq!(d["questionCount"], 1);
    }
    #[test]
    fn classifies_plan_target() {
        let d = interaction_detail(
            "request_confirmation",
            &serde_json::json!({"target":{"type":"issue_document","key":"plan"},"prompt":"Approve"}),
        );
        assert_eq!(d["kind"], "plan_approval");
    }
}
