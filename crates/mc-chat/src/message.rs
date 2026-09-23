//! chat **消息读取**的纯领域规则（M4-3 / LUM-1474）：游标分页参数、可见性过滤、消息种类归一。
//!
//! 本模块**不做 I/O**：SQL 在 `mc_repos::chat_message`，HTTP 在
//! `mc_http::routes::chat::message`。规则逐条对回上游：
//!
//! | 规则 | 上游出处 |
//! | --- | --- |
//! | `limit` 默认 50，合法区间 `1..=100`，否则 400 `invalid limit` | `chat.go:1047 parseChatMessagesPageParams` |
//! | 游标必须**成对**给（`before_created_at` + `before_id`），只给一个 → 400 `invalid cursor` | 同上 |
//! | 游标时间是 RFC3339 **Nano**，id 必须是 UUID | 同上 |
//! | 取 `limit + 2` 行：`+1` 判 `has_more`，`+1` 补偿被隐藏的 onboarding kickoff | `ListChatMessagesPage` |
//! | `has_more` 由**可见**行数是否超过 `limit` 决定 | 同上 |
//! | `next_cursor` = 本页**最旧**（未反转前最后一行）那行的 `(created_at, id)` | 同上 |
//! | 每个游标页要**反转**成时间升序后才序列化 | 同上（「Reverse each cursor page … chronological within the viewport」）|
//! | `message_kind = 'channel_command'` 永不出现在用户面 | `chat.sql` 两条 message 查询的 `WHERE` |
//! | `message_kind = 'onboarding_kickoff'` 从用户面隐藏，但**不**影响游标 | `visibleChatMessages` |

use chrono::{DateTime, Utc};
use uuid::Uuid;

/// 分页默认窗口（上游 `parseChatMessagesPageParams` 的 `limit := 50`）。
pub const DEFAULT_PAGE_LIMIT: i64 = 50;
/// 分页窗口下界。
pub const MIN_PAGE_LIMIT: i64 = 1;
/// 分页窗口上界（`> 100` → 400 `invalid limit`）。
pub const MAX_PAGE_LIMIT: i64 = 100;
/// SQL 的 lookahead 补偿：多取一行为判 `has_more`，再多取一行补偿被隐藏的 kickoff。
pub const PAGE_LOOKAHEAD: i64 = 2;

/// `chat_message.message_kind` 的已知取值（`protocol.ChatMessageKind*`）。
pub const KIND_MESSAGE: &str = "message";
/// `no_response`：assistant 侧「本次没有回答」的显式行。
pub const KIND_NO_RESPONSE: &str = "no_response";
/// `onboarding_kickoff`：服务端写给 agent 的上下文，成员面不可见。
pub const KIND_ONBOARDING_KICKOFF: &str = "onboarding_kickoff";
/// `onboarding_opening`：Mika 的开场白，成员面**可见**。
pub const KIND_ONBOARDING_OPENING: &str = "onboarding_opening";
/// `channel_command`：外部渠道控制面记录，两条 message 查询都在 SQL 层排除。
pub const KIND_CHANNEL_COMMAND: &str = "channel_command";

/// 上游 `normalizeMessageKind`：未知 / 空值一律降级成 `message`（未来新增种类不能让老客户端崩）。
pub fn normalize_message_kind(raw: &str) -> &'static str {
    match raw {
        KIND_NO_RESPONSE => KIND_NO_RESPONSE,
        KIND_ONBOARDING_KICKOFF => KIND_ONBOARDING_KICKOFF,
        KIND_ONBOARDING_OPENING => KIND_ONBOARDING_OPENING,
        _ => KIND_MESSAGE,
    }
}

/// 上游 `visibleChatMessages`：成员面是否下发该行（`channel_command` 已在 SQL 层排除）。
pub fn is_visible_kind(raw: &str) -> bool {
    raw != KIND_ONBOARDING_KICKOFF
}

/// 分页参数错误 → 400（两条上游错误文案）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PageError {
    /// `limit` 非整数或越界 → 400 `invalid limit`。
    #[error("invalid limit")]
    InvalidLimit,
    /// 游标不成对 / 时间或 id 解析失败 → 400 `invalid cursor`。
    #[error("invalid cursor")]
    InvalidCursor,
}

/// 一条 `(created_at, id)` 游标（上游 `ChatMessagesCursorResponse` 的反向输入）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    /// 该行的 `created_at`（SQL 用 `(created_at, id) < (…)` 元组比较）。
    pub created_at: DateTime<Utc>,
    /// 该行的 `id`（同 `created_at` 时的二级排序键）。
    pub id: Uuid,
}

/// 解析后的分页参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageParams {
    /// 本页最多返回多少条可见消息。
    pub limit: i64,
    /// 游标；`None` = 空游标（从**最近**的尾巴开始翻）。
    pub cursor: Option<Cursor>,
}

impl PageParams {
    /// SQL 实际 `LIMIT`：`limit + `[`PAGE_LOOKAHEAD`]（判 `has_more` + 补偿隐藏行）。
    pub fn fetch_limit(self) -> i64 {
        self.limit + PAGE_LOOKAHEAD
    }
}

/// 上游 `parseChatMessagesPageParams` 的逐字等价实现（含「成对或全不给」）。
pub fn parse_page_params(
    raw_limit: Option<&str>,
    raw_before_created_at: Option<&str>,
    raw_before_id: Option<&str>,
) -> Result<PageParams, PageError> {
    let limit = match raw_limit {
        None => DEFAULT_PAGE_LIMIT,
        Some(raw) => raw
            .parse::<i64>()
            .ok()
            .filter(|v| (MIN_PAGE_LIMIT..=MAX_PAGE_LIMIT).contains(v))
            .ok_or(PageError::InvalidLimit)?,
    };

    let before_created_at = non_empty(raw_before_created_at);
    let before_id = non_empty(raw_before_id);
    let (Some(raw_time), Some(raw_id)) = (before_created_at, before_id) else {
        // 两个都缺 ⇒ 空游标；只缺一个 ⇒ 400（上游先判 `== "" && == ""`，再判任一为空）。
        return if before_created_at.is_none() && before_id.is_none() {
            Ok(PageParams {
                limit,
                cursor: None,
            })
        } else {
            Err(PageError::InvalidCursor)
        };
    };

    let created_at = DateTime::parse_from_rfc3339(raw_time)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|_| PageError::InvalidCursor)?;
    let id = Uuid::parse_str(raw_id.trim()).map_err(|_| PageError::InvalidCursor)?;
    Ok(PageParams {
        limit,
        cursor: Some(Cursor { created_at, id }),
    })
}

/// 上游查询串取值的空值语义：`?limit=` 与没传等价（Go 的 `Get` 拿到 `""` 会走默认分支）。
fn non_empty(raw: Option<&str>) -> Option<&str> {
    raw.map(str::trim).filter(|v| !v.is_empty())
}

/// 分页结果：一页消息 + `has_more` + 下一页游标。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageWindow<T> {
    /// 本页消息，**时间升序**（viewport 内按时间递增渲染）。
    pub messages: Vec<T>,
    /// 后面还有更老的消息。
    pub has_more: bool,
    /// 下一页游标；`has_more == false` 时为 `None`（上游 `omitempty`）。
    pub next_cursor: Option<Cursor>,
}

/// 上游 `ListChatMessagesPage` 的裁剪 + 游标构造：输入是 **SQL 原序**
/// （`created_at DESC, id DESC`，新 → 旧）的一页，`key` 抽出每行的 `(created_at, id)`。
///
/// 与上游一致：`has_more` 由行数是否超过 `limit` 决定 → 超了就截断 → 游标取
/// 「截断后最后一行」（时间上**最旧**的那条）→ 最后整体反转成时间升序。
pub fn page_window<T>(rows_desc: Vec<T>, limit: i64, key: impl Fn(&T) -> Cursor) -> PageWindow<T> {
    let mut messages = rows_desc;
    let has_more = i64::try_from(messages.len()).unwrap_or(i64::MAX) > limit;
    if has_more {
        messages.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    }
    let next_cursor = if has_more {
        messages.last().map(&key)
    } else {
        None
    };
    // 反转成时间升序（viewport 内按时间递增渲染）。
    messages.reverse();
    PageWindow {
        messages,
        has_more,
        next_cursor,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(secs: i64) -> DateTime<Utc> {
        DateTime::<Utc>::from_timestamp(secs, 0).expect("valid timestamp")
    }

    fn row(secs: i64) -> (DateTime<Utc>, Uuid) {
        (
            ts(secs),
            Uuid::from_u128(u128::try_from(secs).expect("small")),
        )
    }

    #[test]
    fn message_kind_degrades_unknown_values_to_message() {
        assert_eq!(normalize_message_kind(KIND_NO_RESPONSE), KIND_NO_RESPONSE);
        assert_eq!(
            normalize_message_kind(KIND_ONBOARDING_OPENING),
            KIND_ONBOARDING_OPENING
        );
        assert_eq!(normalize_message_kind(""), KIND_MESSAGE);
        assert_eq!(normalize_message_kind("future_kind"), KIND_MESSAGE);
        assert!(!is_visible_kind(KIND_ONBOARDING_KICKOFF));
        assert!(is_visible_kind(KIND_ONBOARDING_OPENING));
    }

    #[test]
    fn limit_defaults_to_50_and_rejects_out_of_range() {
        let p = parse_page_params(None, None, None).expect("defaults");
        assert_eq!(p.limit, DEFAULT_PAGE_LIMIT);
        assert_eq!(p.cursor, None);
        assert_eq!(p.fetch_limit(), DEFAULT_PAGE_LIMIT + 2);

        assert_eq!(
            parse_page_params(Some("0"), None, None).unwrap_err(),
            PageError::InvalidLimit
        );
        assert_eq!(
            parse_page_params(Some("101"), None, None).unwrap_err(),
            PageError::InvalidLimit
        );
        assert_eq!(
            parse_page_params(Some("abc"), None, None).unwrap_err(),
            PageError::InvalidLimit
        );
        assert_eq!(
            parse_page_params(Some("100"), None, None).unwrap().limit,
            100
        );
        assert_eq!(parse_page_params(Some("1"), None, None).unwrap().limit, 1);
    }

    #[test]
    fn cursor_must_be_paired_and_rfc3339_nano() {
        let id = Uuid::from_u128(7);
        let paired = parse_page_params(
            Some("10"),
            Some("2026-09-23T06:27:16.123456789Z"),
            Some(&id.to_string()),
        )
        .expect("paired cursor");
        let cursor = paired.cursor.expect("cursor present");
        assert_eq!(cursor.id, id);
        // 纳秒精度必须原样保留（上游 `time.Parse(time.RFC3339Nano)`）。
        assert_eq!(cursor.created_at.to_rfc3339(), "2026-09-23T06:27:16.123456789+00:00");

        assert_eq!(
            parse_page_params(Some("10"), Some("2026-09-23T06:27:16Z"), None).unwrap_err(),
            PageError::InvalidCursor
        );
        assert_eq!(
            parse_page_params(Some("10"), None, Some(&id.to_string())).unwrap_err(),
            PageError::InvalidCursor
        );
        assert_eq!(
            parse_page_params(Some("10"), Some("not-a-time"), Some(&id.to_string())).unwrap_err(),
            PageError::InvalidCursor
        );
        assert_eq!(
            parse_page_params(Some("10"), Some("2026-09-23T06:27:16Z"), Some("nope")).unwrap_err(),
            PageError::InvalidCursor
        );
    }

    fn key_of(row: &(DateTime<Utc>, Uuid)) -> Cursor {
        Cursor {
            created_at: row.0,
            id: row.1,
        }
    }

    #[test]
    fn empty_cursor_starts_at_the_recent_tail_and_pages_are_reversed() {
        // 空游标：SQL 取最近 3 行（limit=1 ⇒ fetch 3），新 → 旧。
        let page = page_window(vec![row(30), row(20), row(10)], 1, key_of);
        assert!(page.has_more);
        // 裁到 1 行（最新那行），再反转（单行不动）。
        assert_eq!(page.messages.len(), 1);
        assert_eq!(page.messages[0].1, row(30).1);
        // 游标取「截断后最后一行」= 本页最旧那行。
        assert_eq!(page.next_cursor.expect("cursor").id, row(30).1);
    }

    #[test]
    fn last_page_has_no_cursor_and_keeps_chronological_order() {
        let page = page_window(vec![row(30), row(20)], 5, key_of);
        assert!(!page.has_more);
        assert_eq!(page.next_cursor, None);
        assert_eq!(page.messages, vec![row(20), row(30)], "反转后时间升序");
    }
}
