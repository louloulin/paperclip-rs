//! `/api/chat/history` 与 `/api/chat/thread` 的**纯领域规则**（M4-4 / LUM-1475）：
//! 分页钳制、`?before` 游标编解码、渠道侧作者词表、以及「读不到」时的说明文案。
//!
//! 本模块**不做 I/O**：SQL 在 `mc_repos::chat_history`，HTTP 在
//! `mc_http::routes::chat::task`。每条规则对回上游原文（`chat_history.go` 418 行）：
//!
//! | 规则 | 上游出处 |
//! | --- | --- |
//! | `limit` 缺省 30、上限 50；junk / 负数 → 0 → 走缺省 | `chat_history.go:132-140` `parseHistoryLimit` + `clampTranscriptLimit` |
//! | 游标 = `<RFC3339Nano>\|<uuid>`，坏游标 → 从头读（零值 tuple） | `chat_history.go:148-176` `transcriptCursor` / `parseTranscriptCursor` |
//! | 读回的是 `chat_message` 行，**newest-first → oldest-first 反转** | `chat_history.go:98-120` `chatMessageHistory` |
//! | 命中整页（`len == limit`）才给 `next_cursor` | 同上 |
//! | 作者词表 `user` → `User`、`assistant` → `Bot` | `chat_history.go:180-186` `transcriptAuthor` |
//! | `ts` 用 **`RFC3339Nano`**（与 `messages/page` 的游标同款） | `chat_history.go:113` + `chat.go:1248` |
//! | 无渠道阅读器时 thread 直接 200 + note | `chat_history.go:362-367` `writeNoChannelIntegration` |
//! | note 随 `channel_type` 二选一 | `chat_history.go:390-395` `noHistoryNote` |
//!
//! ⚠️ 范围硬边界（`docs/42` §4.3 第 3 条）：本波只落**非渠道分支**。上游 `SlackHistory == nil`
//! （本仓恒真：没有 slack/lark 集成）时，`/api/chat/history` 走 `chatMessageHistory`
//! —— 这正是「无渠道阅读器时服务本会话自己的 transcript」；`/api/chat/thread` 走
//! `writeNoChannelIntegration`。渠道**阅读器**（slack/lark 的 `ChannelOverview` / `Thread`）
//! 随 M7 补齐并在 `docs/45` 的 `known_gap` 登记。

use chrono::{DateTime, Utc};
use uuid::Uuid;

/// 缺省页长（上游 `defaultTranscriptLimit`）。
pub const DEFAULT_LIMIT: usize = 30;
/// 页长上限（上游 `maxTranscriptLimit`）——防止 agent 把整段会话灌进上下文。
pub const MAX_LIMIT: usize = 50;

/// 上游 `parseHistoryLimit`（`chat_history.go:132`）的逐字移植：空串 / 非数字 / 负数一律
/// 归 0（=「用阅读器的缺省值」），**不**报 400 —— 垃圾参数被静默忽略是上游的可观察行为。
pub fn parse_limit(raw: Option<&str>) -> Option<u32> {
    let raw = raw?;
    if raw.is_empty() {
        return None;
    }
    match raw.parse::<i64>() {
        Ok(n) if n >= 0 => u32::try_from(n).ok(),
        _ => None,
    }
}

/// 上游 `clampTranscriptLimit`（`chat_history.go:126`）：`<= 0` → 30，`> 50` → 50。
pub fn clamp_limit(parsed: Option<u32>) -> usize {
    match parsed {
        None | Some(0) => DEFAULT_LIMIT,
        Some(n) => usize::min(n as usize, MAX_LIMIT),
    }
}

/// `?limit` 的一次性换算（解析 + 钳制），HTTP 层与测试共用。
pub fn transcript_limit(raw: Option<&str>) -> usize {
    clamp_limit(parse_limit(raw))
}

/// transcript 的分页游标：`(created_at, id)` 两半。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TranscriptCursor {
    /// `chat_message.created_at`。
    pub created_at: DateTime<Utc>,
    /// `chat_message.id`。
    pub id: Uuid,
}

/// 上游 `transcriptCursor`：`RFC3339Nano + "|" + uuid`。
///
/// 用 Nano（不是全仓 `ts` 那种秒精度）是上游刻意的：游标必须**无损**，
/// 秒精度会让同一秒内的两行互相跳过或重复。
pub fn encode_cursor(created_at: DateTime<Utc>, id: Uuid) -> String {
    format!("{}|{}", nano(created_at), id)
}

/// `RFC3339Nano` + UTC `Z`（上游 `time.RFC3339Nano` 的等价渲染）。
///
/// Go 的 `'9'` 语义会**裁掉小数末尾的 0**，小数全为 0 时连小数点一起丢掉
/// （`…:20Z` 而不是 `…:20.000000000Z`）；`chrono` 没有等价格式项
/// （`SecondsFormat::Nanos` 固定 9 位、`AutoSi` 只在 0/3/6/9 位里挑）⇒ 渲染后再裁。
/// PG `TIMESTAMPTZ` 是微秒精度，所以裁完最多 6 位。本仓 `session::support::cursor_ts`
/// 是同一条规则的 HTTP 侧副本（那边先于本模块落地，两者输出逐字节相同）。
pub fn nano(ts: DateTime<Utc>) -> String {
    let rendered = ts
        .with_timezone(&Utc)
        .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
    let Some((head, tail)) = rendered.split_once('.') else {
        return rendered;
    };
    let digits = tail.strip_suffix('Z').unwrap_or(tail);
    let trimmed = digits.trim_end_matches('0');
    if trimmed.is_empty() {
        format!("{head}Z")
    } else {
        format!("{head}.{trimmed}Z")
    }
}

/// 上游 `parseTranscriptCursor`：任意一步失败都返回 `None`（= 从最新消息开始读），
/// **不**报 400。切分点取**第一个** `|`（Go `strings.Cut`）。
pub fn parse_cursor(raw: Option<&str>) -> Option<TranscriptCursor> {
    let raw = raw.filter(|s| !s.is_empty())?;
    let (ts, id) = raw.split_once('|')?;
    let created_at = DateTime::parse_from_rfc3339(ts).ok()?.with_timezone(&Utc);
    let id = Uuid::parse_str(id).ok()?;
    Some(TranscriptCursor { created_at, id })
}

/// 渠道历史里的作者词表（上游 `channel.HistoryRole`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryRole {
    /// `user` 行。
    User,
    /// `assistant` 行。
    Assistant,
}

impl HistoryRole {
    /// 上游 `chatMessageHistory` 的映射：**只有** `assistant` 是 assistant，其余（含未知值）
    /// 都算 user（上游写的是 `role := channel.HistoryRoleUser; if m.Role == "assistant" { … }`）。
    pub fn from_row_role(role: &str) -> Self {
        if role == "assistant" {
            Self::Assistant
        } else {
            Self::User
        }
    }

    /// 线上字面量（`channel.HistoryRoleUser` / `HistoryRoleAssistant`）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }

    /// 上游 `transcriptAuthor`：`Bot` / `User` —— 与 Slack 路径同一套词汇，
    /// 不让 agent 看到裸的 `user` / `assistant`。
    pub fn author(self) -> &'static str {
        match self {
            Self::User => "User",
            Self::Assistant => "Bot",
        }
    }
}

/// 一条 transcript 消息（上游 `channel.HistoryMessage`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryMessage {
    /// `chat_message.id`。
    pub id: Uuid,
    /// 作者角色。
    pub role: HistoryRole,
    /// 正文。
    pub text: String,
    /// 作者标签（`User` / `Bot`）。
    pub author: String,
    /// `created_at`（`ts` 字段按 `RFC3339Nano` 渲染，游标也用它）。
    pub created_at: DateTime<Utc>,
}

impl HistoryMessage {
    /// 线上 `ts`：`RFC3339Nano` + UTC `Z`。
    pub fn ts(&self) -> String {
        nano(self.created_at)
    }
}

/// 上游 `chatMessageHistory` 的收尾：反转成 oldest-first + 决定 `next_cursor`。
///
/// 入参 `newest_first` 是 `ListChatMessagesPage` 的原序（`created_at DESC, id DESC`）。
/// 游标条件逐字：**命中整页**（`rows.len() == limit`）**且**有行（`out` 非空）才给游标；
/// 游标取的是**最旧**那行（原序的最后一行）。
pub fn transcript_page(
    newest_first: Vec<HistoryMessage>,
    limit: usize,
) -> (Vec<HistoryMessage>, Option<String>) {
    let full_page = newest_first.len() == limit && !newest_first.is_empty();
    let cursor = if full_page {
        let oldest = newest_first.last().expect("non-empty checked above");
        Some(encode_cursor(oldest.created_at, oldest.id))
    } else {
        None
    };
    let mut out = newest_first;
    out.reverse();
    (out, cursor)
}

/// 上游 `writeNoChannelIntegration` 的固定文案（`/api/chat/thread` 的 200 响应）。
pub const NO_CHANNEL_INTEGRATION_NOTE: &str =
    "No chat channel integration is configured on this server.";

/// 上游 `noHistoryNote`：空读也要让 agent 能行动。无绑定时说不认识渠道；
/// 有绑定时明确「房间其余内容读不到」而不是谎称这是 web-only 会话。
pub fn no_history_note(channel_type: &str) -> String {
    if channel_type.is_empty() {
        return "This conversation is not connected to a chat channel, so there is no channel history to read."
            .to_owned();
    }
    format!(
        "This conversation is on {channel_type}, whose backlog this server cannot read. \
         You can see the messages addressed to you in this session, but not the rest of the room."
    )
}

/// `/api/chat/history` 的响应体形状（上游 `ChatChannelHistoryResponse`）。
///
/// `messages` **永不为 null**（上游 `if messages == nil { messages = []… }`），
/// `channel_type` 恒出现（可能是 `""`），其余三个字段 `omitempty`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelHistoryPage {
    /// 绑定的渠道平台；无绑定时 `""`。
    pub channel_type: String,
    /// 单线程读时给出线程 id（history 路径恒 `None`）。
    pub thread_id: Option<String>,
    /// 消息（oldest-first）。
    pub messages: Vec<HistoryMessage>,
    /// 还有更旧的消息时的游标。
    pub next_cursor: Option<String>,
    /// 空结果的原因说明。
    pub note: Option<String>,
}

impl ChannelHistoryPage {
    /// 上游 `writeNoChannelIntegration` 的完整响应：`messages: []` + note。
    ///
    /// `channel_type` 是 **`""`** 而不是缺字段 —— `ChatChannelHistoryResponse` 那三个
    /// 字段里没有 `channel_type` 的 `omitempty`，所以空串照样序列化出来；有绑定但读不到
    /// 的那条 200（[`ChannelHistoryPage::unreadable_channel`]）与它在体形状上唯一的差别
    /// 就是 note 的文案。
    pub fn no_channel_integration() -> Self {
        Self {
            channel_type: String::new(),
            thread_id: None,
            messages: Vec::new(),
            next_cursor: None,
            note: Some(NO_CHANNEL_INTEGRATION_NOTE.to_owned()),
        }
    }

    /// 上游「有绑定但本服务器读不到」（`respondChatHistory` 的 `ErrNoSlackSession` 分支）：
    /// 带 `channel_type` + 空消息 + 对应 note。
    pub fn unreadable_channel(channel_type: &str) -> Self {
        Self {
            channel_type: channel_type.to_owned(),
            thread_id: None,
            messages: Vec::new(),
            next_cursor: None,
            note: Some(no_history_note(channel_type)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn msg(secs: i64, role: HistoryRole) -> HistoryMessage {
        HistoryMessage {
            id: Uuid::from_u128(u128::from(secs.unsigned_abs())),
            role,
            text: format!("m{secs}"),
            author: role.author().to_owned(),
            created_at: Utc.timestamp_opt(secs, 0).unwrap(),
        }
    }

    #[test]
    fn limit_is_junk_tolerant_and_clamped() {
        // 空 → 缺省；垃圾 / 负数 → 缺省；0 → 缺省（上游 `<= 0` 分支）。
        assert_eq!(transcript_limit(None), DEFAULT_LIMIT);
        assert_eq!(transcript_limit(Some("")), DEFAULT_LIMIT);
        assert_eq!(transcript_limit(Some("abc")), DEFAULT_LIMIT);
        assert_eq!(transcript_limit(Some("-3")), DEFAULT_LIMIT);
        assert_eq!(transcript_limit(Some("0")), DEFAULT_LIMIT);
        assert_eq!(transcript_limit(Some("5")), 5);
        assert_eq!(transcript_limit(Some("50")), MAX_LIMIT);
        assert_eq!(transcript_limit(Some("5000")), MAX_LIMIT);
    }

    #[test]
    fn cursor_round_trips_at_nanosecond_precision() {
        let ts = Utc.timestamp_opt(1_700_000_000, 123_456_789).unwrap();
        let id = Uuid::from_u128(7);
        let encoded = encode_cursor(ts, id);
        // 尾零裁剪 = Go RFC3339Nano（`…123456789Z` 全保留、整秒 `…:20Z`）。
        assert!(encoded.starts_with("2023-11-14T22:13:20.123456789Z|"));
        assert_eq!(
            nano(Utc.timestamp_opt(1_700_000_000, 0).unwrap()),
            "2023-11-14T22:13:20Z"
        );
        assert_eq!(
            nano(Utc.timestamp_opt(1_700_000_000, 100).unwrap()),
            "2023-11-14T22:13:20.0000001Z"
        );
        assert!(encoded.ends_with("Z|00000000-0000-0000-0000-000000000007"));
        let parsed = parse_cursor(Some(&encoded)).expect("round trip");
        assert_eq!(parsed.id, id);
        assert_eq!(parsed.created_at, ts);
    }

    #[test]
    fn malformed_cursor_reads_from_the_newest_window() {
        for bad in [
            None,
            Some(""),
            Some("no-separator"),
            Some("nope|also-nope"),
            Some("|"),
        ] {
            assert!(parse_cursor(bad).is_none(), "坏游标必须静默忽略: {bad:?}");
        }
        // 只切**第一个** `|`（Go `strings.Cut`）：多余的 `|` 让 uuid 解析失败 → 从头读。
        assert!(parse_cursor(Some("2023-11-14T22:13:20Z|a|b")).is_none());
    }

    #[test]
    fn role_vocabulary_is_channel_shaped() {
        assert_eq!(
            HistoryRole::from_row_role("assistant"),
            HistoryRole::Assistant
        );
        assert_eq!(HistoryRole::from_row_role("user"), HistoryRole::User);
        // 未知 role 归 user（上游只把 assistant 特判）。
        assert_eq!(HistoryRole::from_row_role("system"), HistoryRole::User);
        assert_eq!(HistoryRole::Assistant.author(), "Bot");
        assert_eq!(HistoryRole::User.author(), "User");
        assert_eq!(HistoryRole::Assistant.as_str(), "assistant");
    }

    #[test]
    fn transcript_is_reversed_and_cursor_only_on_a_full_page() {
        let newest_first = vec![
            msg(3, HistoryRole::Assistant),
            msg(2, HistoryRole::User),
            msg(1, HistoryRole::Assistant),
        ];
        // 3 行 = 页长 → 给游标，且游标指向**最旧**那行。
        let (messages, cursor) = transcript_page(newest_first.clone(), 3);
        assert_eq!(
            messages.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(),
            vec!["m1", "m2", "m3"]
        );
        assert_eq!(
            cursor,
            Some(encode_cursor(messages[0].created_at, messages[0].id))
        );
        // 不满页 → 没有更旧的了，不给游标。
        let (messages, cursor) = transcript_page(newest_first, 10);
        assert_eq!(messages.len(), 3);
        assert!(cursor.is_none());
        // 空页 → 不给游标（上游 `len(out) > 0` 的门）。
        let (messages, cursor) = transcript_page(Vec::new(), 0);
        assert!(messages.is_empty());
        assert!(cursor.is_none());
    }

    #[test]
    fn notes_match_the_bound_and_unbound_cases() {
        assert_eq!(
            no_history_note(""),
            "This conversation is not connected to a chat channel, so there is no channel history to read."
        );
        let note = no_history_note("lark");
        assert!(note.starts_with("This conversation is on lark,"));
        assert!(note.contains("not the rest of the room"));

        let page = ChannelHistoryPage::no_channel_integration();
        assert_eq!(page.channel_type, "");
        assert!(page.messages.is_empty());
        assert_eq!(page.note.as_deref(), Some(NO_CHANNEL_INTEGRATION_NOTE));

        let page = ChannelHistoryPage::unreadable_channel("slack");
        assert_eq!(page.channel_type, "slack");
        assert_eq!(
            page.note.as_deref(),
            Some(no_history_note("slack").as_str())
        );
    }
}
