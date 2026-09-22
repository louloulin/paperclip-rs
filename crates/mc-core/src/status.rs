//! Issue status: 内置 7 个 key + workspace 自定义 status（key 在 database 校验）。
//!
//! 与 multica `validIssueStatuses` / `issuestatus.Canonical()` 等价。

use serde::{Deserialize, Serialize};

/// 内置 status 的 key（与 multica `issuestatus.Canonical()` 等价）。
pub const CANONICAL_KEYS: &[&str] = &[
    "backlog",
    "todo",
    "in_progress",
    "in_review",
    "done",
    "cancelled",
    "triage",
];

/// Wire enum 7 值（用于 protocol legacy 兼容）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueStatus {
    Backlog,
    Todo,
    InProgress,
    InReview,
    Done,
    Cancelled,
    Triage,
}

impl IssueStatus {
    pub fn key(self) -> &'static str {
        match self {
            IssueStatus::Backlog => "backlog",
            IssueStatus::Todo => "todo",
            IssueStatus::InProgress => "in_progress",
            IssueStatus::InReview => "in_review",
            IssueStatus::Done => "done",
            IssueStatus::Cancelled => "cancelled",
            IssueStatus::Triage => "triage",
        }
    }

    pub fn from_key(s: &str) -> Option<Self> {
        match s {
            "backlog" => Some(Self::Backlog),
            "todo" => Some(Self::Todo),
            "in_progress" => Some(Self::InProgress),
            "in_review" => Some(Self::InReview),
            "done" => Some(Self::Done),
            "cancelled" => Some(Self::Cancelled),
            "triage" => Some(Self::Triage),
            _ => None,
        }
    }

    /// Lifecycle category: open / closed。
    pub fn category(self) -> StatusCategory {
        match self {
            IssueStatus::Done | IssueStatus::Cancelled => StatusCategory::Closed,
            _ => StatusCategory::Open,
        }
    }

    pub fn canonical() -> Vec<&'static str> {
        CANONICAL_KEYS.to_vec()
    }
}

impl Default for IssueStatus {
    fn default() -> Self {
        Self::Backlog
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StatusCategory {
    Open,
    Closed,
}

/// Transition guard: 哪些 status 转换是合法的。
/// 与 multica issue_guard 模块一致：open→open 永远允许；closed→open 仅 undo 操作允许。
pub fn is_valid_transition(from: IssueStatus, to: IssueStatus) -> bool {
    use IssueStatus::*;
    if from == to {
        return true;
    }
    // 终态只能去 Cancelled
    if matches!(from, Done | Cancelled) {
        return false;
    }
    // Triage 只能转到 backlog / todo / cancelled
    if from == Triage {
        return matches!(to, Backlog | Todo | Cancelled);
    }
    // 其他都允许
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_keys_unique() {
        let mut v = CANONICAL_KEYS.to_vec();
        v.sort();
        v.dedup();
        assert_eq!(v.len(), CANONICAL_KEYS.len());
    }

    #[test]
    fn canonical_has_seven_entries() {
        assert_eq!(CANONICAL_KEYS.len(), 7);
    }

    #[test]
    fn closed_to_open_is_rejected() {
        assert!(!is_valid_transition(IssueStatus::Done, IssueStatus::Todo));
        assert!(!is_valid_transition(IssueStatus::Cancelled, IssueStatus::Todo));
    }

    #[test]
    fn triage_can_only_go_to_backlog_todo_cancelled() {
        assert!(is_valid_transition(IssueStatus::Triage, IssueStatus::Backlog));
        assert!(is_valid_transition(IssueStatus::Triage, IssueStatus::Todo));
        assert!(is_valid_transition(IssueStatus::Triage, IssueStatus::Cancelled));
        assert!(!is_valid_transition(IssueStatus::Triage, IssueStatus::InProgress));
        assert!(!is_valid_transition(IssueStatus::Triage, IssueStatus::Done));
    }

    #[test]
    fn open_to_open_is_allowed() {
        assert!(is_valid_transition(IssueStatus::Todo, IssueStatus::InProgress));
        assert!(is_valid_transition(IssueStatus::InProgress, IssueStatus::InReview));
    }

    #[test]
    fn category_round_trip() {
        assert_eq!(IssueStatus::Done.category(), StatusCategory::Closed);
        assert_eq!(IssueStatus::Cancelled.category(), StatusCategory::Closed);
        assert_eq!(IssueStatus::Todo.category(), StatusCategory::Open);
    }
}