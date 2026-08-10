//! Types —— Inbox dismissals DTOs、常量、错误码。
//!
//! 与 Node `server/src/services/inbox-dismissals.ts` + shared types 1:1 对齐。

use serde::{Deserialize, Serialize};
use thiserror::Error;

use pc_repos::inbox::DismissKind;

// ============================================================================
// Constants
// ============================================================================

/// 所有支持的 dismissal kind（与 Node `kind in {dismiss, snooze}` 1:1 对齐）。
///
/// 可枚举，用于 dashboard / filter 等场景。
pub const KINDS: &[DismissKind] = &[DismissKind::Dismiss, DismissKind::Snooze];

// ============================================================================
// Error codes
// ============================================================================

/// Inbox dismissal 错误码常量。
///
/// 与 Node `forbidden({ code: ... })` / `unprocessable(...)` 1:1 对齐。
pub mod codes {
    /// `unprocessable` —— snooze 必须提供未来的 `snoozedUntil`。
    pub const INBOX_DISMISSAL_SNOOZE_IN_PAST: &str = "inbox_dismissal_snooze_in_past";
    /// `unprocessable` —— snooze 必须提供 `snoozedUntil`。
    pub const INBOX_DISMISSAL_SNOOZE_REQUIRES_UNTIL: &str = "inbox_dismissal_snooze_requires_until";
    /// `unprocessable` —— dismiss 不允许带 `snoozedUntil`。
    pub const INBOX_DISMISSAL_DISMISS_WITH_UNTIL: &str = "inbox_dismissal_dismiss_with_until";
    /// `unprocessable` —— `userId` / `itemKey` 不能为空。
    pub const INBOX_DISMISSAL_EMPTY_IDENTIFIER: &str = "inbox_dismissal_empty_identifier";
}

// ============================================================================
// Errors
// ============================================================================

/// Inbox dismissal service 错误。
#[derive(Debug, Error)]
pub enum InboxDismissalServiceError {
    /// Repo 校验错误 —— 转换为业务层 error code 透传给 caller。
    #[error("validation error: {0}")]
    Validation(String),

    /// `pc-repos` / sqlx 错误。
    #[error("repo error: {0}")]
    Repo(#[from] pc_repos::RepoError),

    /// 透传 sqlx 错误。
    #[error("database error: {0}")]
    Database(String),
}

impl From<sqlx::Error> for InboxDismissalServiceError {
    fn from(e: sqlx::Error) -> Self {
        Self::Database(e.to_string())
    }
}

impl InboxDismissalServiceError {
    /// 推断对应的 Node error code（与 `forbidden({ code })` / `unprocessable({ code })` 1:1 对齐）。
    pub fn infer_code(&self) -> Option<&'static str> {
        match self {
            Self::Validation(msg) => {
                if msg.contains("snooze requires snoozed_until") {
                    Some(codes::INBOX_DISMISSAL_SNOOZE_REQUIRES_UNTIL)
                } else if msg.contains("in the future") {
                    Some(codes::INBOX_DISMISSAL_SNOOZE_IN_PAST)
                } else if msg.contains("dismiss must not carry") {
                    Some(codes::INBOX_DISMISSAL_DISMISS_WITH_UNTIL)
                } else if msg.contains("must not be empty") {
                    Some(codes::INBOX_DISMISSAL_EMPTY_IDENTIFIER)
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

pub type InboxDismissalServiceResult<T> = Result<T, InboxDismissalServiceError>;

// ============================================================================
// Filter
// ============================================================================

/// 列表过滤（用于内存侧过滤，避免一次拉所有行再筛）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InboxDismissalFilter {
    /// 仅保留指定 kind。
    pub kind: Option<DismissKind>,
    /// `now` 用于过滤 active（snooze 但 `snoozed_until` 已过期的会被视为 inactive）。
    pub active_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl InboxDismissalFilter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_kind(mut self, kind: DismissKind) -> Self {
        self.kind = Some(kind);
        self
    }

    pub fn with_active_at(mut self, now: chrono::DateTime<chrono::Utc>) -> Self {
        self.active_at = Some(now);
        self
    }

    /// 判断一行是否匹配 filter（与 Node 内存过滤 1:1 对齐）。
    pub fn matches(&self, row: &InboxDismissalRowBorrowed<'_>) -> bool {
        if let Some(kind) = self.kind {
            if row.kind != kind {
                return false;
            }
        }
        if let Some(now) = self.active_at {
            if !row.active_at(now) {
                return false;
            }
        }
        true
    }
}

// 借用视图，方便上层 caller 在不复制的情况下过滤；
// 但 inactive 检测需要 snoozed_until 时间戳 → 这里用一个自有 enum 表示活性状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboxDismissalActivity {
    Dismiss,
    SnoozeActive,
    SnoozeExpired,
}

/// 判断一行在 `now` 时的活性状态（与 Node `row.activeAt(now)` 1:1 对齐）。
pub fn activity_at_kind(kind: DismissKind, snoozed_until: Option<pc_core::Timestamp>, now: pc_core::Timestamp) -> InboxDismissalActivity {
    match kind {
        DismissKind::Dismiss => InboxDismissalActivity::Dismiss,
        DismissKind::Snooze => match snoozed_until {
            Some(until) if until.as_datetime() > now.as_datetime() => InboxDismissalActivity::SnoozeActive,
            _ => InboxDismissalActivity::SnoozeExpired,
        },
    }
}

/// 内存侧过滤 helper —— 假设 rows 已按时间倒序取回（与 Node `sortRowsByUpdatedAtDesc` 等价）。
pub fn filter_by_kind(rows: Vec<pc_repos::inbox::InboxDismissalRow>, kind: DismissKind) -> Vec<pc_repos::inbox::InboxDismissalRow> {
    rows.into_iter().filter(|r| r.parsed_kind() == Some(kind)).collect()
}

/// `InboxDismissalRow` 的借用视图（用来在 `InboxDismissalFilter::matches` 中复用底层 fields）。
pub struct InboxDismissalRowBorrowed<'a> {
    pub kind: DismissKind,
    pub snoozed_until: Option<pc_core::Timestamp>,
    _row: &'a pc_repos::inbox::InboxDismissalRow,
}

impl<'a> InboxDismissalRowBorrowed<'a> {
    pub fn new(row: &'a pc_repos::inbox::InboxDismissalRow) -> Option<Self> {
        let kind = row.parsed_kind()?;
        Some(Self { kind, snoozed_until: row.snoozed_until, _row: row })
    }

    pub fn active_at(&self, now: chrono::DateTime<chrono::Utc>) -> bool {
        match self.kind {
            DismissKind::Dismiss => true,
            DismissKind::Snooze => match self.snoozed_until {
                Some(until) => until.as_datetime() > now,
                None => false,
            },
        }
    }
}
