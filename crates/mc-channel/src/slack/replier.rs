//! Slack 出站回复器：把 engine 的**判决**翻译成给用户看的一条消息
//! （上游 `internal/integrations/slack/replier.go`，256 行）。
//!
//! - **写者**：M7-4（`docs/60-M7-PLAN.md` §3.3）。
//! - **上游定位**：`engine.OutboundReplier` —— 判决驱动的回复接缝（`Replier=nil` 的尾巴）。
//!   它走的是和 `outbound.rs` **同一条** bot-token 发送路径（mrkdwn / 分片 / 线程化都在那里），
//!   所以本文件不需要新传输。
//!
//! # 判决 → 文案（上游逐字）
//!
//! | `Outcome` | 行为 |
//! | --- | --- |
//! | `NeedsBinding` | 铸一枚单次绑定令牌 + 「点这里绑定」提示（指向 `AppURL + BindingPath`） |
//! | `AgentOffline` / `AgentArchived` | 一条状态告知，用户不至于干等 |
//! | `FreshPending` / `ChatStarted` | 新会话已就绪 / 已开始 |
//! | `IssueUsage` | `/issue` 缺标题 ⇒ 用法提示 |
//! | `Ingested` | **只有带 issue 的**才回（普通聊天消息**保持沉默** —— agent 自己的回复走 `ChatDone`） |
//! | `Dropped` | 不回 |
//!
//! # 三条从上游逐字搬来的细节
//!
//! 1. **绑定链接包成显式 Slack 链接 `<url|label>`**：`format_mrkdwn` 会保护这种形态，
//!    于是 base64url 令牌里的 `_` / `-` 不会被 markdown 处理成斜体（上游注释逐字）。
//! 2. **issue 标题要消毒**：`break_markdown_link_adjacency` **先**跑（拆掉不可信文本里的
//!    链接邻接），**再**把 `<` 换成 `&lt;` —— 顺序不能反（上游注释逐字：member-authored
//!    links and mentions are handled as visible text）。`mrkdwn.rs` 刻意保留既有的
//!    `<url|label>` / `<@user>` 实体，所以这一步必须发生在它**之前**。
//! 3. **控制回执要记账**（`channel_outbound_message`，`kind = control_ack` / `issue_ack`）：
//!    历史读面正是按这张表把「控制回执」从 agent 上下文里剔掉
//!    （`history.rs` 的 `filterRouteGeneration`）。没有它，历史里会混进一堆「⏳ 稍等」。
//!
//! # 与上游的一处形态差异（登记 `docs/32` §15）
//!
//! engine 的 [`OutboundReplier::reply`] 是**同步**方法（调用点在 `tokio::spawn` 里，
//! 见 `engine/router/outbound.rs`），而上游的 `Reply` 直接阻塞着发 HTTP。本仓的
//! 同步方法只把一个脱离任务推出去，真正的工作在 async 的 [`SlackOutboundReplier::reply_now`]
//! 里 —— 于是（a）引擎调用点绝不阻塞在 Slack HTTP 上，（b）用例可以直接 await 完整路径，
//! 不必 sleep 等后台任务。
//!
//! # 凭据纪律（`docs/60` §2.3）
//!
//! 绑定令牌的**明文只出现在一条 URL 里**（那是它唯一的用途），**不进**日志：
//! 任何 `tracing::*` 调用都不插值 `bind_url`。安装的 bot token 只以形参流动。

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use mc_core::channel::message::InboundMessage;
use mc_core::id::Id;

use crate::engine::resolvers::{
    ChannelIssue, OutboundReplier, Outcome, ResolvedInstallation, RouteResult,
};
use crate::message::break_markdown_link_adjacency;
use crate::slack::config::{decode_credentials, Decrypter};
use crate::slack::outbound::{kind, OutboundMetadata, Sender};
use crate::slack::typing::installation_row;

/// 绑定页的默认路径（上游 `BindingPath` 零值 ⇒ `"/slack/bind"`）。
pub const DEFAULT_BINDING_PATH: &str = "/slack/bind";

// =====================================================================
// 文案（上游 const 块，逐字）
// =====================================================================

/// agent 离线。
pub const AGENT_OFFLINE_TEXT: &str = "⚠️ The agent is offline right now. Your message was received and will be handled once it's back online.";
/// agent 已归档。
pub const AGENT_ARCHIVED_TEXT: &str =
    "⚠️ This agent has been archived and can't respond. Please contact your workspace admin.";
/// `/clear` 后待开新会话。
pub const FRESH_PENDING_TEXT: &str =
    "✅ Fresh start ready. Your next chat message will run without previous context.";
/// `/new` 已开新会话。
pub const CHAT_STARTED_TEXT: &str =
    "✅ Started a new Multica chat. Your next message will enter it.";
/// `/issue` 缺标题。
pub const ISSUE_USAGE_TEXT: &str =
    "Please include an issue title. Use:\n\n`/issue <title>`\n`[description]` (optional)";

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

/// 出站记账（上游 `controlAckLedger`；`*db.Queries` 满足它）。
#[async_trait]
pub trait OutboundLedger: Send + Sync {
    /// 记一条已投递的出站消息（`channel_outbound_message`）。
    async fn record_outbound(&self, record: &OutboundRecord) -> Result<(), String>;
}

/// 一条出站记账（上游 `RecordChannelOutboundMessageParams` 的契约子集）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundRecord {
    pub installation_id: Id,
    /// 存储口径的渠道名（Slack：`slack`）。
    pub channel_type: String,
    pub channel_message_id: String,
    pub binding_id: Id,
    pub route_revision: i64,
    /// 出站种类（见 [`kind`]）。
    pub outbound_kind: String,
}

// =====================================================================
// 回复器
// =====================================================================

/// engine 的判决 → Slack 消息（上游 `OutboundReplier`）。
pub struct SlackOutboundReplier {
    binding: Option<Arc<dyn BindingMinter>>,
    ledger: Option<Arc<dyn OutboundLedger>>,
    sender: Arc<Sender>,
    decrypt: Decrypter,
    app_url: String,
    binding_path: String,
}

impl fmt::Debug for SlackOutboundReplier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SlackOutboundReplier")
            .field("binding", &self.binding.is_some())
            .field("ledger", &self.ledger.is_some())
            .field("sender", &self.sender)
            .field("decrypt", &self.decrypt)
            .field("app_url", &self.app_url)
            .field("binding_path", &self.binding_path)
            .finish()
    }
}

impl SlackOutboundReplier {
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
        sender: Arc<Sender>,
        decrypt: Decrypter,
        binding: Option<Arc<dyn BindingMinter>>,
        ledger: Option<Arc<dyn OutboundLedger>>,
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
            binding,
            ledger,
            sender,
            decrypt,
            app_url: app_url.into().trim_end_matches('/').to_string(),
            binding_path,
        }
    }

    /// 装配（绑定面缺席的最小形态：只有状态告知）。
    #[must_use]
    pub fn notices_only(sender: Arc<Sender>, decrypt: Decrypter) -> Self {
        Self::new(sender, decrypt, None, None, String::new(), None)
    }

    /// 完整路径（上游 `Reply` 的实体；见模块文档的形态差异）。
    ///
    /// 错误**只告警不返回**：回复器跑在入站 ACK 路径**之外**，一次发送失败
    /// 不该冒泡成"投递失败"（上游逐字）。
    pub async fn reply_now(
        &self,
        installation: &ResolvedInstallation,
        message: &InboundMessage,
        result: &RouteResult,
    ) {
        let Some(row) = installation_row(installation) else {
            tracing::warn!(
                installation_id = %installation.id,
                outcome = result.outcome.as_str(),
                "slack replier: installation platform row unavailable"
            );
            return;
        };
        let text = match result.outcome {
            Outcome::Dropped => return,
            Outcome::NeedsBinding => {
                match self.binding_prompt(installation, message, result).await {
                    Ok(text) => text,
                    Err(error) => {
                        tracing::warn!(
                            installation_id = %installation.id,
                            "slack replier: binding prompt failed: {error}"
                        );
                        return;
                    }
                }
            }
            Outcome::AgentOffline => AGENT_OFFLINE_TEXT.to_string(),
            Outcome::AgentArchived => AGENT_ARCHIVED_TEXT.to_string(),
            Outcome::FreshPending => FRESH_PENDING_TEXT.to_string(),
            Outcome::ChatStarted => CHAT_STARTED_TEXT.to_string(),
            Outcome::IssueUsage => ISSUE_USAGE_TEXT.to_string(),
            // 普通聊天消息**保持沉默**：agent 自己的回复走 ChatDone。
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
        let out_kind = if result.outcome == Outcome::Ingested && result.issue.is_some() {
            kind::ISSUE_ACK
        } else {
            kind::CONTROL_ACK
        };
        if let Err(error) = self
            .post(installation, message, result, &row, &text, out_kind)
            .await
        {
            tracing::warn!(
                installation_id = %installation.id,
                outcome = result.outcome.as_str(),
                "slack replier: reply failed: {error}"
            );
        }
    }

    /// 发一条判决回复并记账（上游 `postResult`）。
    async fn post(
        &self,
        installation: &ResolvedInstallation,
        message: &InboundMessage,
        result: &RouteResult,
        row: &crate::slack::resolvers::InstallationRow,
        text: &str,
        out_kind: &str,
    ) -> Result<(), String> {
        let credentials =
            decode_credentials(&row.config, &self.decrypt).map_err(|error| error.to_string())?;
        let outbound = mc_core::channel::message::OutboundMessage {
            chat_id: message.source.chat_id.clone(),
            text: text.to_string(),
            thread_id: message.source.thread_id.clone(),
            reply_to: String::new(),
        };
        let metadata = OutboundMetadata::new(
            result.channel_binding_id,
            result.channel_route_revision,
            out_kind,
        );
        let frame = self
            .sender
            .send_with_metadata(&credentials.bot_token, &outbound, Some(&metadata))
            .await
            .map_err(|error| error.to_string())?;
        let Some(ledger) = &self.ledger else {
            return Ok(());
        };
        let Some(binding_id) = result.channel_binding_id else {
            return Ok(());
        };
        for message_id in &frame.timestamps {
            ledger
                .record_outbound(&OutboundRecord {
                    installation_id: installation.id,
                    channel_type: crate::slack::inbound::TYPE_SLACK.storage_str().to_string(),
                    channel_message_id: message_id.clone(),
                    binding_id,
                    route_revision: result.channel_route_revision,
                    outbound_kind: out_kind.to_string(),
                })
                .await?;
        }
        Ok(())
    }

    /// 绑定卡：铸令牌 + 拼 URL（上游 `sendBindingPrompt`）。
    ///
    /// 三条前置各自给出**明确**的失败原因（上游用三条 `errors.New`）：
    /// 没有 sender id / 没接绑定服务 / 没配 app url。
    async fn binding_prompt(
        &self,
        installation: &ResolvedInstallation,
        message: &InboundMessage,
        result: &RouteResult,
    ) -> Result<String, String> {
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
        // 包成显式 Slack 链接：base64url 里的 `_`/`-` 不会被 mrkdwn 折成斜体（上游逐字）。
        Ok(format!(
            "👋 To start chatting with me, link your Slack account to Multica: <{bind_url}|link your account>\n(This link expires in 15 minutes.)"
        ))
    }
}

/// `url.QueryEscape` 的等价物：**未保留字符集之外**一律百分号编码。
///
/// 上游用的是 Go 的 `url.QueryEscape`（空格 → `+`）。base64url 令牌只含
/// `A-Za-z0-9-_`（其中 `-` / `_` 是 unreserved）⇒ 实际不会被编码；这里逐字保留
/// Go 的行为（空格 → `+`）以免将来换成别的令牌字形时形态漂移。
#[must_use]
pub fn url_encode(raw: &str) -> String {
    use std::fmt::Write as _;

    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
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
#[must_use]
pub fn issue_created_text(issue: &ChannelIssue, identifier: &str) -> String {
    let id = issue_result_identifier(issue, identifier);
    let title = sanitize_issue_title(issue.title.trim());
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
    let title = sanitize_issue_title(issue.title.trim());
    if title.is_empty() {
        format!("⚠️ Not created — active issue {id} already exists.")
    } else {
        format!("⚠️ Not created — active issue {id} already exists: {title}")
    }
}

/// 成员可控标题的消毒（上游 `memberIssueTitle`，**顺序不能反**）。
#[must_use]
pub fn sanitize_issue_title(title: &str) -> String {
    break_markdown_link_adjacency(title).replace('<', "&lt;")
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

/// 脱离式执行（模块文档的形态差异）：有运行时 ⇒ 起任务；没有 ⇒ 打一条 warn。
fn spawn_detached<F>(future: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::spawn(future);
    } else {
        tracing::warn!("slack replier: no async runtime; skipping the detached reply");
    }
}

impl OutboundReplier for SlackOutboundReplier {
    /// 同步接缝：推一个脱离任务后立刻返回（engine 的调用点绝不阻塞在 Slack HTTP 上）。
    fn reply(
        &self,
        installation: &ResolvedInstallation,
        message: &InboundMessage,
        result: &RouteResult,
    ) {
        let (installation, message, result) =
            (installation.clone(), message.clone(), result.clone());
        let replier = self.handle();
        spawn_detached(async move {
            replier.reply_now(&installation, &message, &result).await;
        });
    }
}

impl SlackOutboundReplier {
    /// 可 `'static` 的句柄（脱离任务要它）。克隆的是 `Arc`，不复制任何凭据
    /// （本结构只有解密器与端口，**不持明文令牌**）。
    #[must_use]
    fn handle(&self) -> Arc<Self> {
        Arc::new(Self {
            binding: self.binding.clone(),
            ledger: self.ledger.clone(),
            sender: Arc::clone(&self.sender),
            decrypt: self.decrypt.clone(),
            app_url: self.app_url.clone(),
            binding_path: self.binding_path.clone(),
        })
    }
}

#[cfg(test)]
mod tests;
