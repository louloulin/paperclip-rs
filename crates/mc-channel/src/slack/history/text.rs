//! 历史读面的**纯函数**一半：时间窗 / 游标 / 页大小 + 两道过滤
//! （上游 `historyBounds` / `historyWindow` / `historyNextCursor` / `clampHistoryLimit`
//! / `filterRouteGeneration` / `filterContextGeneration`）。
//!
//! - **写者**：M7-4。切分理由见 `super` 的「文件布局」小节。
//! - 本文件**不碰网络也不碰存储**：全部入参都是值 ⇒ 每一道过滤都能单独钉一条用例。

use std::collections::{HashMap, HashSet};

use crate::engine::commands::{parse_fresh_session_command, parse_new_chat_command};

use super::{
    HistoryOptions, HistoryWindow, OutboundRow, SlackMessage, SlackTarget, DEFAULT_HISTORY_LIMIT,
    MAX_HISTORY_LIMIT, META_BINDING_ID, META_KIND,
};

// =====================================================================
// 纯函数：窗口 / 游标（上游同名）
// =====================================================================

/// 时间窗：`opts.after` 与绑定的 `history_start` 取较晚者；`opts.until` 与
/// `history_end` 取较早者（上游 `historyBounds`）。
#[must_use]
pub fn history_bounds(opts: &HistoryOptions, target: &SlackTarget) -> (String, String) {
    let mut start = opts.after.clone();
    if !target.history_start.is_empty()
        && (start.is_empty() || slack_ts_less(&start, &target.history_start))
    {
        start.clone_from(&target.history_start);
    }
    let mut end = opts.until.clone();
    if !target.history_end.is_empty()
        && (end.is_empty() || slack_ts_less(&target.history_end, &end))
    {
        end.clone_from(&target.history_end);
    }
    (start, end)
}

/// 一次拉取的时间窗（上游 `historyWindow` 的三条分支）。
#[must_use]
pub fn history_window(
    before: &str,
    start: &str,
    end: &str,
    channel: &str,
    limit: i64,
) -> HistoryWindow {
    let mut latest = before.to_string();
    if !end.is_empty() && (latest.is_empty() || slack_ts_less(end, &latest)) {
        latest = end.to_string();
    }
    if before.is_empty() {
        return HistoryWindow {
            channel: channel.to_string(),
            ts: String::new(),
            latest,
            oldest: start.to_string(),
            inclusive: !start.is_empty() || !end.is_empty(),
            limit,
        };
    }
    HistoryWindow {
        channel: channel.to_string(),
        ts: String::new(),
        latest,
        oldest: String::new(),
        inclusive: false,
        limit,
    }
}

/// 游标是否已经翻到了历史下界（上游 `historyCursorReachedStart`）。
#[must_use]
pub fn history_cursor_reached_start(before: &str, start: &str) -> bool {
    !before.is_empty() && !start.is_empty() && !slack_ts_less(start, before)
}

/// 下一页游标：不满一页（或已到历史下界）⇒ 空串（上游 `historyNextCursor`）。
#[must_use]
pub fn history_next_cursor(raw: &[SlackMessage], limit: i64, history_start: &str) -> String {
    let page = i64::try_from(raw.len()).unwrap_or(i64::MAX);
    if raw.is_empty() || page < limit {
        return String::new();
    }
    let oldest = raw
        .iter()
        .map(|message| message.ts.as_str())
        .min_by(|left, right| {
            parse_slack_ts(left)
                .partial_cmp(&parse_slack_ts(right))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap_or_default()
        .to_string();
    if !history_start.is_empty() && !slack_ts_less(history_start, &oldest) {
        return String::new();
    }
    oldest
}

/// 页大小收敛（上游 `clampHistoryLimit`）。
#[must_use]
pub fn clamp_history_limit(limit: i64) -> i64 {
    if limit <= 0 {
        DEFAULT_HISTORY_LIMIT
    } else {
        limit.min(MAX_HISTORY_LIMIT)
    }
}

/// 两个 Slack 时间戳的先后（`"<秒>.<微秒>"`；解不开按 0 处理）。
#[must_use]
pub fn slack_ts_less(left: &str, right: &str) -> bool {
    parse_slack_ts(left) < parse_slack_ts(right)
}

pub(super) fn parse_slack_ts(ts: &str) -> f64 {
    ts.parse::<f64>().unwrap_or(0.0)
}

// =====================================================================
// 纯函数：第 1 / 第 3 道过滤
// =====================================================================

/// 路由代际过滤（上游 `filterRouteGeneration`）。
///
/// 三条判据：时间窗 → 控制回执 / 别的绑定的出站 → 边界那条消息上的 `/new` 前缀剥离。
#[must_use]
pub fn filter_route_generation<S: std::hash::BuildHasher>(
    raw: Vec<SlackMessage>,
    target: &SlackTarget,
    start: &str,
    end: &str,
    owners: &HashMap<String, OutboundRow, S>,
) -> Vec<SlackMessage> {
    let mut kept = Vec::with_capacity(raw.len());
    for mut message in raw {
        if !start.is_empty() && slack_ts_less(&message.ts, start) {
            continue;
        }
        if !end.is_empty() && !slack_ts_less(&message.ts, end) {
            continue;
        }
        if message.is_our_outbound() {
            let kind = message.metadata_field(META_KIND);
            let binding_id = message.metadata_field(META_BINDING_ID);
            let own_binding = target.binding_id.to_string();
            if kind == crate::slack::outbound::kind::CONTROL_ACK
                || (!binding_id.is_empty() && binding_id != own_binding)
            {
                continue;
            }
        }
        if let Some(owner) = owners.get(&message.ts) {
            if owner.outbound_kind == crate::slack::outbound::kind::CONTROL_ACK
                || owner.binding_id != target.binding_id
            {
                continue;
            }
        }
        if message.ts == target.history_start {
            let stripped = strip_bot_mention(&message.text, &target.bot_user_id);
            if let Some(body) = parse_new_chat_command(&stripped) {
                if body.trim().is_empty() {
                    continue;
                }
                message.text = body;
            }
        }
        kept.push(message);
    }
    kept
}

/// 上下文代际过滤（上游 `filterContextGeneration`）。
///
/// 代际 > 1 时本 bot 的消息要过白名单；另外边界那条消息上的 `/clear` 前缀会被剥掉。
#[must_use]
pub fn filter_context_generation<S: std::hash::BuildHasher>(
    raw: Vec<SlackMessage>,
    opts: &HistoryOptions,
    start: &str,
    end: &str,
    bot_user_id: &str,
    allowed: &HashSet<String, S>,
) -> Vec<SlackMessage> {
    let mut kept = Vec::with_capacity(raw.len());
    for mut message in raw {
        if !start.is_empty() && slack_ts_less(&message.ts, start) {
            continue;
        }
        if !end.is_empty() && !slack_ts_less(&message.ts, end) {
            continue;
        }
        if opts.context_revision > 1
            && !message.user.is_empty()
            && message.user == bot_user_id
            && !allowed.contains(&message.ts)
        {
            continue;
        }
        if message.ts == opts.after {
            let stripped = strip_bot_mention(&message.text, bot_user_id);
            if let Some(body) = parse_fresh_session_command(&stripped) {
                if body.trim().is_empty() {
                    continue;
                }
                message.text = body;
            }
        }
        kept.push(message);
    }
    kept
}

/// 剥掉正文里的 `<@BOT>` 提及（上游 `strings.ReplaceAll(text, "<@"+bot+">", "")`）。
#[must_use]
pub fn strip_bot_mention(text: &str, bot_user_id: &str) -> String {
    if bot_user_id.is_empty() {
        return text.to_string();
    }
    text.replace(&format!("<@{bot_user_id}>"), "")
}
