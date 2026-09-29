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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum IssueStatus {
    #[default]
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

    /// Lifecycle category: open / closed（本仓 compat 两档，见 [`StatusCategory`]）。
    ///
    /// 这是「是否终态」的粗粒度问法，**不**回答「处在生命周期的哪一段」。
    /// 要区分 `unstarted` / `started` 的调用方用 [`Self::lifecycle_category`]。
    pub fn category(self) -> StatusCategory {
        match self {
            IssueStatus::Done | IssueStatus::Cancelled => StatusCategory::Closed,
            _ => StatusCategory::Open,
        }
    }

    /// 内置 key 的真实生命周期档（上游 `issuestatus.CategoryForBehavior` 的四值词汇）。
    ///
    /// 与 [`Self::category`] 的差别只在 `Done`：本仓的 compat 档把 `done` 与
    /// `cancelled` 一起算成 `closed`，而上游把它们分成 `done` / `closed` 两档
    /// （注意 `category = 'done'` 与「状态键叫 `done`」是两件事，前者是生命周期档）。
    ///
    /// `triage` 在上游不是内置 key（内置第 7 个是 `blocked`），本仓多出来的这个
    /// 按「尚未开工」归入 `unstarted`。
    pub fn lifecycle_category(self) -> StatusCategory {
        match self {
            IssueStatus::Backlog | IssueStatus::Todo | IssueStatus::Triage => {
                StatusCategory::Unstarted
            }
            IssueStatus::InProgress | IssueStatus::InReview => StatusCategory::Started,
            IssueStatus::Done => StatusCategory::Done,
            IssueStatus::Cancelled => StatusCategory::Closed,
        }
    }

    pub fn canonical() -> Vec<&'static str> {
        CANONICAL_KEYS.to_vec()
    }
}

/// status 的生命周期分类。
///
/// 上游 `server/internal/issuestatus` 用四值词汇：`unstarted` / `started` /
/// `done` / `closed`。本仓历史上只建模 `open` / `closed` 两档，其中 `open`
/// 覆盖上游的 `unstarted` + `started`（见
/// `migrations/compat/539_status_and_role_vocabulary.up.sql`，DB CHECK 已按并集放宽）。
///
/// 现在两者共存：
/// - [`StatusCategory::Unstarted`] / `Started` / `Done` / `Closed` 与上游逐值对应，
///   可写入、可回显，是分类过滤与排序的**依据**；
/// - [`StatusCategory::Open`] 是**本仓 compat 别名**，语义 =「非终态、阶段未细分」，
///   与 `unstarted` / `started` 都相容（判定用
///   `mc_repos::issue_status::category_matches`，不要用 `==`）。
///   它仍然要保留：本仓 `issue_status::DEFAULT_STATUSES` 的 7 行与历史数据写的都是它。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StatusCategory {
    /// compat 别名：非终态、阶段未细分（= 上游 `unstarted` ∪ `started`）。
    Open,
    /// 上游 `unstarted`。
    Unstarted,
    /// 上游 `started`。
    Started,
    /// 上游 `done`。
    Done,
    /// 上游 `closed`。
    Closed,
}

impl StatusCategory {
    /// 是否终态：`done` / `closed` 是；compat 别名 `open` 与 `unstarted` /
    /// `started` 都不是。
    pub fn is_terminal(self) -> bool {
        matches!(self, StatusCategory::Done | StatusCategory::Closed)
    }

    /// 生命周期相对次序（上游 `issuestatus.categoryRank`）：
    /// `unstarted(0) < started(1) < done(2) < closed(3)`。
    ///
    /// compat 别名 `Open` 取 **0**（排在最早的未开工档）—— 它跨两档，排在前面让
    /// 粗粒度数据不会插进具体档中间造成抖动。要求严格区分 `unstarted` / `started`
    /// 的排序**必须**先把内置 key 归一到 [`IssueStatus::lifecycle_category`]，
    /// 不能直接读 DB 里可能是 `'open'` 的列值。
    pub fn rank(self) -> u8 {
        match self {
            StatusCategory::Open | StatusCategory::Unstarted => 0,
            StatusCategory::Started => 1,
            StatusCategory::Done => 2,
            StatusCategory::Closed => 3,
        }
    }
}

/// Transition guard: 哪些 status 转换是合法的。
/// 与 multica `issue_guard` 模块一致：open→open 永远允许；closed→open 仅 undo 操作允许。
pub fn is_valid_transition(from: IssueStatus, to: IssueStatus) -> bool {
    use IssueStatus::{Backlog, Cancelled, Done, Todo, Triage};
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
        v.sort_unstable();
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
        assert!(!is_valid_transition(
            IssueStatus::Cancelled,
            IssueStatus::Todo
        ));
    }

    #[test]
    fn triage_can_only_go_to_backlog_todo_cancelled() {
        assert!(is_valid_transition(
            IssueStatus::Triage,
            IssueStatus::Backlog
        ));
        assert!(is_valid_transition(IssueStatus::Triage, IssueStatus::Todo));
        assert!(is_valid_transition(
            IssueStatus::Triage,
            IssueStatus::Cancelled
        ));
        assert!(!is_valid_transition(
            IssueStatus::Triage,
            IssueStatus::InProgress
        ));
        assert!(!is_valid_transition(IssueStatus::Triage, IssueStatus::Done));
    }

    #[test]
    fn open_to_open_is_allowed() {
        assert!(is_valid_transition(
            IssueStatus::Todo,
            IssueStatus::InProgress
        ));
        assert!(is_valid_transition(
            IssueStatus::InProgress,
            IssueStatus::InReview
        ));
    }

    #[test]
    fn lifecycle_category_uses_the_four_value_vocabulary() {
        // `done` 与 `cancelled` 在 compat 档里同为 closed，在真实生命周期里分属两档
        assert_eq!(IssueStatus::Done.category(), StatusCategory::Closed);
        assert_eq!(IssueStatus::Cancelled.category(), StatusCategory::Closed);
        assert_eq!(IssueStatus::Todo.category(), StatusCategory::Open);
        assert_eq!(
            IssueStatus::Todo.lifecycle_category(),
            StatusCategory::Unstarted
        );
        assert_eq!(
            IssueStatus::InProgress.lifecycle_category(),
            StatusCategory::Started
        );
        assert_eq!(IssueStatus::Done.lifecycle_category(), StatusCategory::Done);
        assert_eq!(
            IssueStatus::Cancelled.lifecycle_category(),
            StatusCategory::Closed
        );
    }

    #[test]
    fn terminal_and_rank() {
        assert!(!StatusCategory::Open.is_terminal());
        assert!(!StatusCategory::Unstarted.is_terminal());
        assert!(!StatusCategory::Started.is_terminal());
        assert!(StatusCategory::Done.is_terminal());
        assert!(StatusCategory::Closed.is_terminal());
        assert!(StatusCategory::Unstarted.rank() < StatusCategory::Started.rank());
        assert!(StatusCategory::Started.rank() < StatusCategory::Done.rank());
        assert!(StatusCategory::Done.rank() < StatusCategory::Closed.rank());
        // compat 别名排在最早的未开工档
        assert_eq!(
            StatusCategory::Open.rank(),
            StatusCategory::Unstarted.rank()
        );
    }

    #[test]
    fn category_round_trip() {
        assert_eq!(IssueStatus::Done.category(), StatusCategory::Closed);
        assert_eq!(IssueStatus::Cancelled.category(), StatusCategory::Closed);
        assert_eq!(IssueStatus::Todo.category(), StatusCategory::Open);
    }
}
