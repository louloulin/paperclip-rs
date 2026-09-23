//! chat 会话的**纯领域规则**（M4-3 / LUM-1474）：标题校验、状态机、pin / 已读语义。
//!
//! 本模块**不做 I/O**：SQL 在 `mc_repos::chat_session`，HTTP 在
//! `mc_http::routes::chat::session`。这里的每条规则都能对回上游原文：
//!
//! | 规则 | 上游出处 |
//! | --- | --- |
//! | 标题 trim 后非空、`>` 200 字符拒绝 | `chat.go:39` `chatSessionTitleMaxLen` + `UpdateChatSession` |
//! | `title` 与 `project_id` **恰好给一个** | `UpdateChatSession` 的 `hasTitle == hasProjectID` 分支 |
//! | `status` 只在 `active` / `archived` 之间翻转 | `SetChatSessionArchived` + `chat_session_status_check` |
//! | pin **不碰 `updated_at`**、重复 pin 保留原 `pinned_at` | `chat.sql` 的 `SetChatSessionPinned` |
//! | archive **碰 `updated_at`**（行要重排到列表顶部） | `chat.sql` 的 `SetChatSessionArchived` |
//! | `has_unread == (unread_count > 0)`，`unread_count` 只数读游标之后的 assistant 行 | `chat.sql` 的 `ListChatSessionsByCreator` |
//! | 归档行 `unread_count` 恒 0（未读无法清理，不许点灯） | `chat.sql` 的 `ListAllChatSessionsByCreator` |

use chrono::{DateTime, Utc};

/// 标题长度上限（上游 `chat.go:39` 的 `chatSessionTitleMaxLen`，按**字符**计）。
pub const TITLE_MAX_LEN: usize = 200;

/// `chat_session.status` 的两个合法取值（`chat_session_status_check`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    /// 活跃会话，出现在默认列表里。
    Active,
    /// 已归档：仍在「归档」视图里可读，但发消息会被拒（M4-4 的 send 路径）。
    Archived,
}

impl SessionStatus {
    /// 列值字面量（与迁移的 CHECK 逐字一致）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Archived => "archived",
        }
    }

    /// 列值 → 枚举；未知值**不**猜测（上游是 CHECK 约束，出现未知值即数据损坏）。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "active" => Some(Self::Active),
            "archived" => Some(Self::Archived),
            _ => None,
        }
    }

    /// 归档视图 / 未读抑制的唯一判据。
    pub fn is_archived(self) -> bool {
        matches!(self, Self::Archived)
    }
}

/// 标题校验失败的原因（对应上游两条 400）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TitleError {
    /// trim 之后为空 → 400 `title is required`。
    #[error("title is required")]
    Empty,
    /// 字符数超过 [`TITLE_MAX_LEN`] → 400 `title is too long`。
    #[error("title is too long")]
    TooLong,
}

/// 上游 `UpdateChatSession` 的标题分支：`strings.TrimSpace` 后判空 + 按 **rune** 数上限。
///
/// 返回 trim 之后的值（上游把 trim 过的 title 写库，不回写原始输入）。
pub fn validate_title(raw: &str) -> Result<String, TitleError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(TitleError::Empty);
    }
    if trimmed.chars().count() > TITLE_MAX_LEN {
        return Err(TitleError::TooLong);
    }
    Ok(trimmed.to_owned())
}

/// 上游 `SetChatSessionArchived`：`archived` 决定目标状态，**两者都** bump `updated_at`。
pub fn status_after_archive(archived: bool) -> SessionStatus {
    if archived {
        SessionStatus::Archived
    } else {
        SessionStatus::Active
    }
}

/// 上游 `chat.sql` 的 `SetChatSessionPinned` 的 `CASE`：
/// `pinned = true` 时只在原值为 NULL 时盖上 `now()`（重复 pin **保留**原顺序），
/// `pinned = false` 时清空。`now` 由调用方注入，便于纯函数测试。
pub fn pinned_at_after(
    current: Option<DateTime<Utc>>,
    pinned: bool,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    if pinned {
        Some(current.unwrap_or(now))
    } else {
        None
    }
}

/// 列表行上的未读投影：`has_unread` 是 `unread_count > 0` 的**派生**值。
///
/// 上游把它作为独立字段下发（老客户端兼容），两者必须同源 —— 所以只在 Rust 侧派生一次。
pub fn unread_projection(status: SessionStatus, unread_count: i64) -> (i64, bool) {
    let count = if status.is_archived() {
        0
    } else {
        unread_count
    };
    (count, count > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(secs: i64) -> DateTime<Utc> {
        DateTime::<Utc>::from_timestamp(secs, 0).expect("valid timestamp")
    }

    #[test]
    fn title_is_trimmed_and_bounded_by_characters() {
        assert_eq!(validate_title("  hello  ").unwrap(), "hello");
        assert_eq!(validate_title("   ").unwrap_err(), TitleError::Empty);
        // 200 个汉字合法（按字符而非字节计；按字节会误拒）。
        let wide = "汉".repeat(TITLE_MAX_LEN);
        assert!(validate_title(&wide).is_ok());
        let too_long = "汉".repeat(TITLE_MAX_LEN + 1);
        assert_eq!(validate_title(&too_long).unwrap_err(), TitleError::TooLong);
    }

    #[test]
    fn status_round_trips_through_column_literals() {
        assert_eq!(SessionStatus::Active.as_str(), "active");
        assert_eq!(SessionStatus::Archived.as_str(), "archived");
        assert_eq!(
            SessionStatus::parse("archived"),
            Some(SessionStatus::Archived)
        );
        assert_eq!(SessionStatus::parse("deleted"), None);
        assert!(status_after_archive(true).is_archived());
        assert!(!status_after_archive(false).is_archived());
    }

    #[test]
    fn pinning_keeps_the_original_pin_time_and_unpinning_clears_it() {
        let first = ts(1_000);
        let now = ts(2_000);
        assert_eq!(pinned_at_after(None, true, now), Some(now));
        // 重复 pin：保留首次时间 ⇒ 列表顺序不被刷新。
        assert_eq!(pinned_at_after(Some(first), true, now), Some(first));
        assert_eq!(pinned_at_after(Some(first), false, now), None);
        assert_eq!(pinned_at_after(None, false, now), None);
    }

    #[test]
    fn archived_sessions_never_report_unread() {
        assert_eq!(
            unread_projection(SessionStatus::Archived, 7),
            (0, false),
            "归档行不可清理未读，必须归零"
        );
        assert_eq!(unread_projection(SessionStatus::Active, 0), (0, false));
        assert_eq!(unread_projection(SessionStatus::Active, 3), (3, true));
    }
}
