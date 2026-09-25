//! Telegram 出站回复器：把 engine 的**判决**翻译成给用户看的一条消息
//! （上游 `internal/integrations/telegram/replier.go`，267 行）。
//!
//! - **写者**：M7-5（`docs/60-M7-PLAN.md` §3.3；本片的写集勘误见 `docs/32` §17.1）。
//! - **上游定位**：`engine.OutboundReplier` —— 判决驱动的回复接缝。它走的是与入站回路
//!   **同一条** bot token 发送路径（纯文本 `sendMessage`），所以本文件不需要新传输。
//!
//! # 判决 → 文案（上游逐字）
//!
//! | `Outcome` | 行为 |
//! | --- | --- |
//! | `NeedsBinding` | **私聊**才铸一枚单次绑定令牌 + 「点这里绑定」提示；**群聊只回一条指路** |
//! | `AgentOffline` / `AgentArchived` | 一条状态告知，用户不至于干等 |
//! | `FreshPending` / `ChatStarted` | 新会话已就绪 / 已开始 |
//! | `IssueUsage` | `/issue` 缺标题 ⇒ 用法提示 |
//! | `Ingested` | **只有带 issue 的**才回（普通聊天消息**保持沉默** —— agent 自己的回复走出站发送器） |
//! | `Dropped` | 只有**被寻址的 `/issue`** 被拒时才回（成员 / 安装已撤销两种文案） |
//!
//! # 群聊里**不**发持有凭据的链接（上游逐字）
//!
//! 群里可见的 bearer 链接会被**别的**群成员兑换掉，于是原来那个人的 Telegram 身份会被绑到
//! **错误的** Multica 用户上。所以群聊里只回一条"先私聊我"的指路，**只有私聊**才带令牌。
//!
//! # 与上游的两处形态差异（登记 `docs/32` §17.2）
//!
//! 1. engine 的 [`OutboundReplier::reply`] 是**同步**方法（调用点在 `tokio::spawn` 里），
//!    而上游的 `Reply` 直接阻塞着发 HTTP。本仓的同步方法只把一个脱离任务推出去，真正的工作在
//!    async 的 [`TelegramOutboundReplier::reply_now`] 里 —— 于是（a）引擎调用点绝不阻塞在
//!    Telegram HTTP 上，（b）用例可以直接 await 完整路径，不必 sleep 等后台任务
//!    （与 M7-4 的 `SlackOutboundReplier` 同款）。
//! 2. **没有出站记账**：上游 Telegram 的 `post` 不写 `channel_outbound_message`（那是 Slack 的
//!    历史过滤面要的），本仓照上游不记。
//!
//! # 凭据纪律（`docs/60` §2.3）
//!
//! 绑定令牌的**明文只出现在一条 URL 里**（那是它唯一的用途），**不进**日志：任何
//! `tracing::*` 调用都不插值 `bind_url`。安装的 bot token 只以形参流动。

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use mc_core::channel::message::{ChatType, InboundMessage};
use mc_core::id::Id;

use crate::engine::commands::parse_issue_command;
use crate::engine::resolvers::{
    ChannelIssue, OutboundReplier, Outcome, ResolvedInstallation, RouteResult,
};
use crate::telegram::api::{SendMessage, TelegramApi};
use crate::telegram::config::{decode_credentials, Decrypter};
use crate::telegram::inbound::parse_message_ref;

/// 绑定页的默认路径（上游 `BindingPath` 零值 ⇒ `"/telegram/bind"`）。
pub const DEFAULT_BINDING_PATH: &str = "/telegram/bind";

/// 绑定链接的寿命文案（上游逐字）。
pub const BINDING_LINK_TTL_HINT: &str = "(This link expires in 15 minutes.)";

// =====================================================================
// 文案（上游 `sender.go` 的 const 块 + `replier.go` 的 const 块，逐字）
// =====================================================================

/// agent 离线。
pub const AGENT_OFFLINE_TEXT: &str = "⚠️ The agent is offline right now. Your message was received and will be handled once it's back online.";
/// agent 已归档。
pub const AGENT_ARCHIVED_TEXT: &str =
    "⚠️ This agent has been archived and can't respond. Please contact your workspace admin.";
/// 非文本消息的礼貌告知（入站回路也用它）。
pub const UNSUPPORTED_TYPE_TEXT: &str =
    "Sorry, I can't handle this kind of message yet. Please send text.";
/// 群聊里的绑定指路（**不带**令牌）。
pub const BINDING_GROUP_HINT: &str =
    "Please message me in a direct chat first, then link your Multica account.";
/// `/clear` 后待开新会话。
pub const FRESH_PENDING_TEXT: &str =
    "✅ Fresh start ready. Your next chat message will run without previous context.";
/// `/new` 已开新会话。
pub const CHAT_STARTED_TEXT: &str =
    "✅ Started a new Multica chat. Your next message will enter it.";
/// `/issue` 缺标题。
pub const ISSUE_USAGE_TEXT: &str =
    "Please include an issue title. Use:\n\n/issue <title>\n[description] (optional)";
/// 被寻址的 `/issue` 由**非成员**发出。
pub const ISSUE_NOT_MEMBER_TEXT: &str = "You're not a member of this Multica workspace, so I can't file an issue for you. Ask a workspace admin to invite you, then send the command again.";
/// 被寻址的 `/issue` 落在**已撤销**的安装上。
pub const ISSUE_DISABLED_TEXT: &str = "This Telegram bot isn't connected to Multica (or was disconnected). Ask a workspace admin to reconnect it.";

// =====================================================================
// 端口
// =====================================================================

/// 铸绑定令牌（上游 `bindingMinter`；`*BindingTokenService` 满足它）。
#[async_trait]
pub trait BindingMinter: Send + Sync {
    /// 铸一枚单次令牌；**明文只在返回值里出现一次**（落库只存哈希）。
    async fn mint(
        &self,
        workspace_id: Id,
        installation_id: Id,
        channel_user_id: &str,
    ) -> Result<MintedBinding, String>;
}

/// 一枚刚铸出的绑定令牌（上游 `BindingToken`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MintedBinding {
    /// 明文（**只在绑定 URL 里出现一次**；绝不落日志）。
    pub raw: String,
    pub expires_at: DateTime<Utc>,
}

// =====================================================================
// 回复器
// =====================================================================

/// engine 的判决 → Telegram 消息（上游 `OutboundReplier`）。
pub struct TelegramOutboundReplier {
    api: Arc<dyn TelegramApi>,
    decrypt: Decrypter,
    binding: Option<Arc<dyn BindingMinter>>,
    app_url: String,
    binding_path: String,
}

impl fmt::Debug for TelegramOutboundReplier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TelegramOutboundReplier")
            .field("api", &"<dyn TelegramApi>")
            .field("decrypt", &self.decrypt)
            .field("binding", &self.binding.is_some())
            .field("app_url", &self.app_url)
            .field("binding_path", &self.binding_path)
            .finish()
    }
}

impl TelegramOutboundReplier {
    /// 装配。
    ///
    /// `binding` + `app_url` 是绑定提示的**前提**；缺任一个时该提示被跳过
    /// （离线 / 归档 / issue 三类告知照发 —— 上游注释逐字）。
    ///
    /// `app_url` 是 **web app** 的主机（用户点进去兑换），不是 API 主机
    /// （上游注释逐字：`MULTICA_APP_URL`，回落 `FRONTEND_ORIGIN`）。本仓不读 env
    /// （唯一 env 读取口在宿主），由调用方注入。
    #[must_use]
    pub fn new(
        api: Arc<dyn TelegramApi>,
        decrypt: Decrypter,
        binding: Option<Arc<dyn BindingMinter>>,
        app_url: impl Into<String>,
        binding_path: Option<&str>,
    ) -> Self {
        let path = binding_path.unwrap_or(DEFAULT_BINDING_PATH);
        let binding_path = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("/{path}")
        };
        Self {
            api,
            decrypt,
            binding,
            app_url: app_url.into().trim_end_matches('/').to_string(),
            binding_path,
        }
    }

    /// 装配（绑定面缺席的最小形态：只有状态告知）。
    #[must_use]
    pub fn notices_only(api: Arc<dyn TelegramApi>, decrypt: Decrypter) -> Self {
        Self::new(api, decrypt, None, String::new(), None)
    }

    /// 完整路径（上游 `Reply` 的实体；见模块文档的形态差异）。
    ///
    /// 错误**只告警不返回**：回复器跑在入站 ACK 路径**之外**，一次发送失败不该冒泡成
    /// "投递失败"（上游逐字）。
    pub async fn reply_now(
        &self,
        installation: &ResolvedInstallation,
        message: &InboundMessage,
        result: &RouteResult,
    ) {
        let text = match result.outcome {
            Outcome::Dropped => dropped_reply_text(result, message),
            Outcome::NeedsBinding => match self.binding_prompt(installation, message, result).await
            {
                Ok(text) => text,
                Err(error) => {
                    tracing::warn!(
                        installation_id = %installation.id,
                        "telegram replier: binding prompt failed: {error}"
                    );
                    return;
                }
            },
            Outcome::AgentOffline => AGENT_OFFLINE_TEXT.to_string(),
            Outcome::AgentArchived => AGENT_ARCHIVED_TEXT.to_string(),
            Outcome::FreshPending => FRESH_PENDING_TEXT.to_string(),
            Outcome::ChatStarted => CHAT_STARTED_TEXT.to_string(),
            Outcome::IssueUsage => ISSUE_USAGE_TEXT.to_string(),
            // 普通聊天消息**保持沉默**：agent 自己的回复走出站发送器。
            Outcome::Ingested => {
                let Some(issue) = result.issue.as_ref() else {
                    return;
                };
                if result.issue_duplicate {
                    issue_duplicate_text(issue, &result.issue_identifier)
                } else {
                    issue_created_text(issue, &result.issue_identifier)
                }
            }
        };
        if text.is_empty() {
            return;
        }
        if let Err(error) = self.post(installation, message, &text).await {
            tracing::warn!(
                installation_id = %installation.id,
                outcome = result.outcome.as_str(),
                "telegram replier: reply failed: {error}"
            );
        }
    }

    /// 发一条纯文本（上游 `post`）：解密本安装的令牌 → 解析 chat / 话题 → 引用回复。
    async fn post(
        &self,
        installation: &ResolvedInstallation,
        message: &InboundMessage,
        text: &str,
    ) -> Result<(), String> {
        let Some(row) = crate::telegram::resolvers::installation_row(installation) else {
            return Err("installation platform row unavailable".to_string());
        };
        let credentials =
            decode_credentials(&row.config, &self.decrypt).map_err(|error| error.to_string())?;
        let chat_id = message
            .source
            .chat_id
            .parse::<i64>()
            .map_err(|_| format!("bad chat id {:?}", message.source.chat_id))?;
        let thread_id = message.source.thread_id.parse::<i64>().unwrap_or(0);
        let reply_to = parse_message_ref(&message.message_id);
        let params = SendMessage::text(chat_id, text)
            .in_thread(thread_id)
            .with_reply_to(reply_to);
        self.api
            .send_message(&credentials.bot_token, &params)
            .await
            .map(|_| ())
            .map_err(|error| format!("post telegram reply failed ({})", error.method()))
    }

    /// 绑定卡：铸令牌 + 拼 URL（上游 `sendBindingPrompt`）。
    ///
    /// 三条前置各自给出**明确**的失败原因（上游用三条 `errors.New`）：没有 sender id /
    /// 没接绑定服务 / 没配 app url。
    async fn binding_prompt(
        &self,
        installation: &ResolvedInstallation,
        message: &InboundMessage,
        result: &RouteResult,
    ) -> Result<String, String> {
        // 群里**不**发持有凭据的链接（见模块文档）。
        if message.source.chat_type == ChatType::Group {
            return Ok(BINDING_GROUP_HINT.to_string());
        }
        let sender = if result.sender.is_empty() {
            message.source.sender_id.clone()
        } else {
            result.sender.clone()
        };
        if sender.is_empty() {
            return Err("missing sender id".to_string());
        }
        let Some(binding) = &self.binding else {
            return Err("binding service not configured".to_string());
        };
        if self.app_url.is_empty() {
            return Err("app url not configured".to_string());
        }
        let token = binding
            .mint(installation.workspace_id, installation.id, &sender)
            .await
            .map_err(|error| format!("mint binding token: {error}"))?;
        let bind_url = format!(
            "{}{}?token={}",
            self.app_url,
            self.binding_path,
            url_encode(&token.raw)
        );
        Ok(format!(
            "👋 To start chatting with me, link your Telegram account to Multica:\n{bind_url}\n{BINDING_LINK_TTL_HINT}"
        ))
    }
}

/// `url.QueryEscape` / `encodeURIComponent` 的等价物：**未保留字符集之外**一律百分号编码。
///
/// base64url 令牌只含 `A-Za-z0-9-_`（其中 `-` / `_` 是 unreserved）⇒ 实际不会被编码；
/// 这里逐字保留 Go 的行为（空格 → `+`）以免将来换成别的令牌字形时形态漂移 —— 与
/// `slack::replier::url_encode` 同一份实现（两侧各自持有一份，见 `docs/32` §17.2）。
#[must_use]
pub fn url_encode(raw: &str) -> String {
    use std::fmt::Write as _;

    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(byte));
            }
            b' ' => out.push('+'),
            // `write!` 到一个 `String` 永不失败（`String` 的 `fmt::Write` 是 infallible 的）。
            other => {
                let _ = write!(out, "%{other:02X}");
            }
        }
    }
    out
}

/// `/issue` 建成文案（上游 `issueCreatedText`）。
///
/// Telegram 的判决回复走**纯文本**（不发 `parse_mode`）⇒ 不需要 Slack 那套 mrkdwn 消毒。
#[must_use]
pub fn issue_created_text(issue: &ChannelIssue, identifier: &str) -> String {
    let id = issue_result_identifier(issue, identifier);
    let title = issue.title.trim();
    if title.is_empty() {
        format!("✅ Created {id}")
    } else {
        format!("✅ Created {id} — {title}")
    }
}

/// `/issue` 因重复守卫未建成（上游 `issueDuplicateText`）。
#[must_use]
pub fn issue_duplicate_text(issue: &ChannelIssue, identifier: &str) -> String {
    let id = issue_result_identifier(issue, identifier);
    let title = issue.title.trim();
    if title.is_empty() {
        format!("⚠️ Not created — active issue {id} already exists.")
    } else {
        format!("⚠️ Not created — active issue {id} already exists: {title}")
    }
}

/// issue 的展示标识符：有 `ABC-42` 用它，否则 `#<number>`（上游 `issueResultIdentifier`）。
#[must_use]
pub fn issue_result_identifier(issue: &ChannelIssue, identifier: &str) -> String {
    if identifier.is_empty() {
        format!("#{}", issue.number)
    } else {
        identifier.to_string()
    }
}

/// 这条消息是不是"**被寻址的** `/issue` 命令"（上游 `isAddressedIssueCommand`）。
///
/// 入站回路（`mod.rs` 的 `notifyIssueDispatchError`）与判决回复（[`dropped_reply_text`]）
/// 共用它 —— 只有这类消息的失败需要一条用户可见的告知，别的失败保持沉默。
#[must_use]
pub fn is_addressed_issue_command(message: &InboundMessage) -> bool {
    if !message.addressed_to_bot {
        return false;
    }
    parse_issue_command(message.command_source_text()).is_some()
}

/// 被拒绝的**被寻址 `/issue`** 的回绝文案（上游 `droppedReplyText`）。
///
/// 只有这两类丢弃值得回一条：非成员 / 安装已撤销。其余（重复、未寻址、bot 消息…）**保持沉默**。
#[must_use]
pub fn dropped_reply_text(result: &RouteResult, message: &InboundMessage) -> String {
    if !is_addressed_issue_command(message) {
        return String::new();
    }
    match result.drop_reason {
        Some(crate::engine::DropReason::NonWorkspaceMember) => ISSUE_NOT_MEMBER_TEXT.to_string(),
        Some(crate::engine::DropReason::RevokedInstallation) => ISSUE_DISABLED_TEXT.to_string(),
        _ => String::new(),
    }
}

impl OutboundReplier for TelegramOutboundReplier {
    /// 同步接缝：推一个脱离任务后立刻返回（engine 的调用点绝不阻塞在 Telegram HTTP 上）。
    fn reply(
        &self,
        installation: &ResolvedInstallation,
        message: &InboundMessage,
        result: &RouteResult,
    ) {
        let (installation, message, result) =
            (installation.clone(), message.clone(), result.clone());
        let handle = self.handle();
        crate::telegram::spawn_detached(async move {
            handle.reply_now(&installation, &message, &result).await;
        });
    }
}

impl TelegramOutboundReplier {
    /// 可 `'static` 的句柄（脱离任务要它）。克隆的是 `Arc`，不复制任何凭据
    /// （本结构只有 API 端口与解密器，**不持明文令牌**）。
    #[must_use]
    fn handle(&self) -> Arc<Self> {
        Arc::new(Self {
            api: Arc::clone(&self.api),
            decrypt: self.decrypt.clone(),
            binding: self.binding.clone(),
            app_url: self.app_url.clone(),
            binding_path: self.binding_path.clone(),
        })
    }
}

#[cfg(test)]
mod tests;
