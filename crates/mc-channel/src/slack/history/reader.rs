//! 历史读面的**执行**部分：解析上下文 → 拉取 → 四道过滤 → 归一化
//! （上游 `history.go` 的 `History` 类型与它的五个方法）。
//!
//! - **写者**：M7-4。切分理由见 `super` 的「文件布局」小节（门 ⑩ 的 800 行硬限）。
//! - `normalize_page` 把人名解析端口**作为形参**收下（`api`）：硬编码一个实现会让
//!   用例注入的替身失效（人名解析正是最该被替身覆盖的那一步）。

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Arc;

use mc_core::id::Id;

use super::flatten::{flatten_slack_text, resolve_user_names, HistoryLabeler};
use super::text::{
    clamp_history_limit, filter_context_generation, filter_route_generation, history_bounds,
    history_cursor_reached_start, history_next_cursor, history_window,
};
use super::{
    binding_thread_root, HistoryApi, HistoryError, HistoryMessage, HistoryOptions, HistoryPage,
    HistoryRole, HistoryStore, HttpHistoryApi, OutboundRow, RepoHistoryStore, SlackMessage,
    SlackTarget,
};
use crate::slack::config::{decode_credentials, Decrypter};

/// 按需读一个 Slack 会话（上游 `History`）。
pub struct History {
    store: Arc<dyn HistoryStore>,
    api: Arc<dyn HistoryApi>,
    decrypt: Decrypter,
}

impl fmt::Debug for History {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("History")
            .field("store", &"<dyn HistoryStore>")
            .field("api", &"<dyn HistoryApi>")
            .field("decrypt", &self.decrypt)
            .finish()
    }
}

impl History {
    /// 装配（任意端口；用例注入替身）。
    #[must_use]
    pub fn new(store: Arc<dyn HistoryStore>, api: Arc<dyn HistoryApi>, decrypt: Decrypter) -> Self {
        Self {
            store,
            api,
            decrypt,
        }
    }

    /// 生产形态：三个泛化仓储 + `reqwest`。
    #[must_use]
    pub fn repo(store: RepoHistoryStore, decrypt: Decrypter) -> Self {
        Self::new(Arc::new(store), Arc::new(HttpHistoryApi), decrypt)
    }

    /// 把 `chat_session` 映射到它的 Slack 频道 + bot 客户端（上游 `resolve`）。
    ///
    /// 频道在**这里**服务端派生，**永不**从调用方接收 —— 这就是
    /// `multica chat thread <id>` 的安全边界（agent 只提供频道内的线程定位符）。
    async fn resolve(&self, chat_session_id: Id) -> Result<SlackTarget, HistoryError> {
        let binding = self
            .store
            .current_binding(chat_session_id)
            .await
            .map_err(|message| HistoryError::Store { message })?
            .ok_or(HistoryError::NoSlackSession)?;
        let installation = self
            .store
            .installation(binding.installation_id)
            .await
            .map_err(|message| HistoryError::Store { message })?
            .ok_or(HistoryError::NoSlackSession)?;
        if !installation.active {
            // 撤销的安装：没有可读的东西（上游逐字：nothing to read）。
            return Err(HistoryError::NoSlackSession);
        }
        let credentials =
            decode_credentials(&installation.config, &self.decrypt).map_err(|_| {
                HistoryError::Credentials {
                    code: "decode_credentials",
                }
            })?;
        Ok(SlackTarget {
            bot_token: credentials.bot_token,
            binding_id: binding.id,
            channel_id: binding.channel_id.clone(),
            thread_root: binding_thread_root(&binding),
            bot_user_id: credentials.bot_user_id,
            history_start: binding.history_start_message_id.clone(),
            history_end: binding.history_end_message_id.clone(),
            boundary_pending: binding.history_boundary_pending,
            route_revision: binding.route_revision,
        })
    }

    /// 频道目录：最近的**顶层**消息（最旧在前），每条线程带 id + 回复数。
    ///
    /// **不**展开线程内容 —— 它是 agent 用来找线程的目录，找到后再用
    /// [`History::thread`] 钻进去。上游 `ChannelOverview`。
    pub async fn channel_overview(
        &self,
        chat_session_id: Id,
        opts: &HistoryOptions,
    ) -> Result<HistoryPage, HistoryError> {
        let target = self.resolve(chat_session_id).await?;
        let (start, end) = history_bounds(opts, &target);
        if opts.boundary_pending
            || target.boundary_pending
            || history_cursor_reached_start(&opts.before, &start)
        {
            return Ok(empty_page(&target));
        }
        let limit = clamp_history_limit(opts.limit);
        let window = history_window(&opts.before, &start, &end, &target.channel_id, limit);
        let raw = self
            .api
            .conversation_history(&target.bot_token, &target.channel_id, &window)
            .await?;
        let next_cursor = history_next_cursor(&raw, limit, &start);
        let raw = self
            .filtered(chat_session_id, &target, opts, raw, &start, &end)
            .await?;
        let mut page = normalize_page(self.api.as_ref(), &target, &raw, true).await;
        page.next_cursor = next_cursor;
        page.channel_type = crate::slack::inbound::TYPE_SLACK.storage_str().to_string();
        Ok(page)
    }

    /// 一个线程的消息（最旧在前）。`thread_id` 空 = 读**会话自己**所在的那个线程；
    /// 非空 = 读该线程，但**始终**在会话钉住的频道内。DM（无线程）读它的线性对话。
    pub async fn thread(
        &self,
        chat_session_id: Id,
        thread_id: &str,
        opts: &HistoryOptions,
    ) -> Result<HistoryPage, HistoryError> {
        let target = self.resolve(chat_session_id).await?;
        let limit = clamp_history_limit(opts.limit);
        let ts = if thread_id.is_empty() {
            target.thread_root.clone()
        } else {
            thread_id.to_string()
        };
        let (start, end) = history_bounds(opts, &target);
        if opts.boundary_pending || target.boundary_pending {
            return Ok(empty_page_with_thread(&target, &ts));
        }
        if history_cursor_reached_start(&opts.before, &start) {
            return Ok(empty_page_with_thread(&target, &ts));
        }
        let window = history_window(&opts.before, &start, &end, &target.channel_id, limit);
        let raw = if ts.is_empty() {
            // 没有线程可读（DM，或群聊里线程根没恢复出来）⇒ 回落到频道的线性对话。
            self.api
                .conversation_history(&target.bot_token, &target.channel_id, &window)
                .await?
        } else {
            let mut window = window;
            window.ts = ts.clone();
            self.api
                .conversation_replies(&target.bot_token, &target.channel_id, &window)
                .await?
        };
        let next_cursor = history_next_cursor(&raw, limit, &start);
        let raw = self
            .filtered(chat_session_id, &target, opts, raw, &start, &end)
            .await?;
        let mut page = normalize_page(self.api.as_ref(), &target, &raw, false).await;
        page.next_cursor = next_cursor;
        page.channel_type = crate::slack::inbound::TYPE_SLACK.storage_str().to_string();
        page.thread_id = ts;
        Ok(page)
    }

    /// 第 1 道 + 第 3 道过滤（上游 `filterRouteGeneration` + `filterContextGeneration`）。
    async fn filtered(
        &self,
        chat_session_id: Id,
        target: &SlackTarget,
        opts: &HistoryOptions,
        raw: Vec<SlackMessage>,
        start: &str,
        end: &str,
    ) -> Result<Vec<SlackMessage>, HistoryError> {
        let rows = self
            .store
            .outbound_for_binding(target.binding_id, target.route_revision)
            .await
            .map_err(|message| HistoryError::Store { message })?;
        let owners: HashMap<String, OutboundRow> = rows
            .into_iter()
            .map(|row| (row.channel_message_id.clone(), row))
            .collect();
        let window = filter_route_generation(raw, target, start, end, &owners);
        let allowed = self
            .allowed_bot_messages(chat_session_id, target, opts, &window)
            .await?;
        Ok(filter_context_generation(
            window,
            opts,
            start,
            end,
            &target.bot_user_id,
            &allowed,
        ))
    }

    /// 代际 > 1 时，本 bot 的消息只有在"这一代持久化过它的 id"时才可信
    /// （上游 `allowedBotMessages`；粒度差异见模块文档差异 1）。
    async fn allowed_bot_messages(
        &self,
        _chat_session_id: Id,
        target: &SlackTarget,
        opts: &HistoryOptions,
        raw: &[SlackMessage],
    ) -> Result<HashSet<String>, HistoryError> {
        if opts.context_revision <= 1 {
            return Ok(HashSet::new());
        }
        let candidates: HashSet<&str> = raw
            .iter()
            .filter(|message| !message.user.is_empty() && message.user == target.bot_user_id)
            .map(|message| message.ts.as_str())
            .collect();
        if candidates.is_empty() {
            return Ok(HashSet::new());
        }
        let rows = self
            .store
            .outbound_for_binding(target.binding_id, target.route_revision)
            .await
            .map_err(|message| HistoryError::Store { message })?;
        Ok(rows
            .into_iter()
            .filter(|row| candidates.contains(row.channel_message_id.as_str()))
            .map(|row| row.channel_message_id)
            .collect())
    }
}

/// 把原始消息归一化成最旧在前的页面（上游 `normalizePage`）。
///
/// `api` 是形参而不是硬编码实现：人名解析要能被用例替身接管。
async fn normalize_page(
    api: &dyn HistoryApi,
    target: &SlackTarget,
    raw: &[SlackMessage],
    overview: bool,
) -> HistoryPage {
    let mut sorted = raw.to_vec();
    sorted.sort_by(|left, right| {
        super::text::parse_slack_ts(&left.ts)
            .partial_cmp(&super::text::parse_slack_ts(&right.ts))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let names = resolve_user_names(api, &target.bot_token, &sorted, &target.bot_user_id).await;
    let mut labeler = HistoryLabeler::new(names);
    let mut messages = Vec::with_capacity(sorted.len());
    for message in sorted {
        let text = flatten_slack_text(&message);
        if text.is_empty() {
            continue; // 真正的 join / 系统 / edit 标记：没有可读正文
        }
        let own = !message.user.is_empty() && message.user == target.bot_user_id;
        let author = labeler.label(&message, own);
        let mut entry = HistoryMessage {
            id: message.ts.clone(),
            author,
            author_id: message.user.clone(),
            role: if own {
                HistoryRole::Assistant
            } else {
                HistoryRole::User
            },
            text,
            ts: message.ts.clone(),
            ..HistoryMessage::default()
        };
        if overview && message.reply_count > 0 {
            entry.thread_id.clone_from(&message.ts);
            entry.reply_count = message.reply_count;
            entry.latest_reply.clone_from(&message.latest_reply);
        }
        messages.push(entry);
    }
    HistoryPage {
        messages,
        ..HistoryPage::default()
    }
}

/// 一个空页（上游在三种"边界已到"的情形下返回它）。
fn empty_page(target: &SlackTarget) -> HistoryPage {
    empty_page_with_thread(target, "")
}

/// 一个带线程 id 的空页。
fn empty_page_with_thread(_target: &SlackTarget, thread_id: &str) -> HistoryPage {
    HistoryPage {
        channel_type: crate::slack::inbound::TYPE_SLACK.storage_str().to_string(),
        thread_id: thread_id.to_string(),
        ..HistoryPage::default()
    }
}
