//! `DingTalk` **判决驱动的出站回复器**：绑定卡 / 状态告知 / `/issue` 确认
//! （上游 `internal/integrations/dingtalk/replier.go` 342 行）。
//!
//! - **写者**：M7-8（`docs/60-M7-PLAN.md` §3.3；写集勘误见 `docs/32` §22）。
//! - **上游定位**：engine 的 [`OutboundReplier`] —— 它走的是与任务回复**同一条**发送路径
//!   （`sendInstallationText`），只是把"引擎判决"翻成用户看得见的一条消息。
//!
//! # 判决 → 文案（上游逐字）
//!
//! | `Outcome` | 行为 |
//! | --- | --- |
//! | `NeedsBinding` | **私聊**铸一枚单次绑定令牌 + 「点这里绑定」提示（群里发这个链接会把令牌暴露给全群） |
//! | `AgentOffline` / `AgentArchived` | 一条状态告知，用户不至于干等 |
//! | `FreshPending` / `ChatStarted` | 新会话已就绪 / 已开始 |
//! | `IssueUsage` | `/issue` 缺标题 ⇒ 用法提示（有媒体时换一条更明确的） |
//! | `Ingested` | **只有带 issue 的**才回（普通聊天消息**保持沉默** —— agent 自己的回复走出站发送器） |
//! | `Dropped` | 只有**被寻址的 `/issue`** 被拒时才回（非成员 / 安装已撤销两种文案） |
//!
//! # 与上游的形态差异（**逐条登记** `docs/32` §22）
//!
//! 1. **同步接缝 + 脱离任务**：engine 的 [`OutboundReplier::reply`] 是同步方法，本仓的同步
//!    方法只把一个脱离任务推出去，真正的工作在 async 的
//!    [`DingTalkOutboundReplier::reply_now`] 里（与 M7-4 / M7-5 同款）。
//! 2. **令牌类型换成端口化的 [`MintedBinding`]**：上游直接依赖 `*BindingTokenService` 的
//!    `pgtype.UUID` 形参；本仓的 adapter 不直接写 DB ⇒ 端口用 `Id`，且只取**明文**那一半
//!    （`raw`）。
//! 3. **没有出站记账**：上游 `DingTalk` 的 `post` 不写 `channel_outbound_message`（那是 Slack
//!    的历史过滤面要的），本仓照上游不记。
//!
//! # 凭据纪律（`docs/60` §2.3）
//!
//! 绑定令牌的**明文只出现在一条 URL 里**（那是它唯一的用途），**不进**日志：任何
//! `tracing::*` 都不插值 `bind_url`；[`MintedBinding`] 手写 `Debug` 输出 `<redacted>`。

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;

use mc_core::channel::message::{ChatType, InboundMessage};
use mc_core::id::Id;
use reqwest::Url;

use crate::dingtalk::markdown::escape_markdown_text;
use crate::dingtalk::outbound::spawn_detached;
use crate::dingtalk::outbound::{
    decode_credentials, Credentials, OpenApiTransport, SendTarget, Sender,
};
use crate::dingtalk::resolvers::installation_row;
use crate::dingtalk::Decrypter;
use crate::engine::commands::parse_issue_command;
use crate::engine::resolvers::{
    ChannelIssue, DropReason, OutboundReplier, Outcome, ResolvedInstallation, RouteResult,
};
use crate::message::issue_web_link;

/// 绑定页的默认路径（上游 `BindingPath` 零值 ⇒ `"/dingtalk/bind"`）。
pub const DEFAULT_BINDING_PATH: &str = "/dingtalk/bind";

// =====================================================================
// 文案（上游 `replier.go` 的 const 块，逐字）
// =====================================================================

/// agent 离线。
pub const AGENT_OFFLINE_TEXT: &str =
    "⚠️ The agent is offline, so this message won't be processed automatically.";
/// agent 已归档。
pub const AGENT_ARCHIVED_TEXT: &str =
    "⚠️ This agent has been archived and can't respond. Please contact your workspace admin.";
/// `/clear` 之后待开新会话。
pub const FRESH_PENDING_TEXT: &str =
    "✅ Fresh start ready. Your next chat message will run without previous context.";
/// `/new` 已开新会话。
pub const CHAT_STARTED_TEXT: &str =
    "✅ Started a new Multica chat. Your next message will enter it.";
/// `/issue` 缺标题。
pub const ISSUE_USAGE_TEXT: &str =
    "Please include an issue title. Use:\n\n`/issue <title>`\n\n`[description]` (optional)";
/// `/issue` 缺标题**且**带了图片。
pub const ISSUE_USAGE_WITH_MEDIA_TEXT: &str = "Please add a title and resend with the image (*image can come before or after the command*):\n\n`/issue <title>`\n\n`[description]` (optional)";
/// 发件人不是 workspace 成员（上游从被删掉的命令处理器搬过来的拒绝文案）。
pub const ISSUE_NOT_MEMBER_TEXT: &str = "You're not a member of this Multica workspace, so I can't file an issue for you. Ask a workspace admin to invite you, then send the command again.";
/// 安装已撤销 / 这个机器人没接上 Multica。
pub const ISSUE_DISABLED_TEXT: &str = "This DingTalk robot isn't connected to Multica (or was disconnected). Ask the agent owner or a workspace owner/admin to reconnect it.";
/// 绑定提示的前半（后半是那条带令牌的链接）。
pub const BINDING_PROMPT_PREFIX: &str =
    "👋 To start chatting with me, link your DingTalk account to Multica: [link your account](";
/// 绑定链接的寿命文案（上游逐字）。
pub const BINDING_LINK_TTL_HINT: &str = "(This link expires in 15 minutes.)";

// =====================================================================
// 端口：铸绑定令牌
// =====================================================================

/// 一枚刚铸出的绑定令牌（上游 `BindingToken` 的**收窄**形态：只要明文那一次）。
///
/// 手写 `Debug`（`docs/60` §2.3 第 1 条）：明文令牌绝不进日志。
#[derive(Clone, PartialEq, Eq)]
pub struct MintedBinding {
    /// 明文（**只在绑定 URL 里出现一次**）。
    pub raw: String,
    /// 所属 workspace（诊断用）。
    pub workspace_id: Id,
    /// 所属安装（诊断用）。
    pub installation_id: Id,
}

impl fmt::Debug for MintedBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MintedBinding")
            .field("raw", &"<redacted>")
            .field("workspace_id", &self.workspace_id)
            .field("installation_id", &self.installation_id)
            .finish()
    }
}

/// 铸绑定令牌（上游 `bindingMinter`；`*BindingTokenService` 满足它）。
#[async_trait]
pub trait BindingMinter: Send + Sync {
    /// 铸一枚单次令牌；**明文只在返回值里出现一次**（落库只存哈希）。
    ///
    /// # Errors
    ///
    /// 仓储 / 随机源失败 ⇒ 人可读描述（**不得**含明文令牌）。
    async fn mint(
        &self,
        workspace_id: Id,
        installation_id: Id,
        channel_user_id: &str,
    ) -> Result<MintedBinding, String>;
}

// =====================================================================
// 错误
// =====================================================================

/// 回复器内部的失败（**所有**变体都不携带凭据 / 明文令牌）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReplierError {
    /// 没有发件人 id（上游 `missing sender id`）。
    #[error("dingtalk replier: missing sender id")]
    MissingSender,
    /// 没接绑定服务（上游 `binding service not configured`）。
    #[error("dingtalk replier: binding service not configured")]
    BindingUnavailable,
    /// 没配 Web 应用地址（上游 `app url not configured`）。
    #[error("dingtalk replier: app url not configured")]
    AppUrlUnavailable,
    /// 安装行不在载体里 / 凭据解不开。
    #[error("dingtalk replier: installation credentials unavailable")]
    CredentialsUnavailable,
    /// 铸令牌失败（**只**带上下文，回显仓储的文案可能含内部细节）。
    #[error("dingtalk replier: mint binding token failed")]
    Mint,
    /// 发送失败（只带**类别码**：`DingTalkApiError::code()` 保证不含凭据 / URL）。
    #[error("dingtalk replier: send failed: {code}")]
    Send { code: &'static str },
}

// =====================================================================
// 回复器
// =====================================================================

/// engine 的判决 → `DingTalk` 消息（上游 `OutboundReplier`）。
pub struct DingTalkOutboundReplier {
    binding: Option<Arc<dyn BindingMinter>>,
    decrypt: Decrypter,
    transport: Arc<dyn OpenApiTransport>,
    app_url: String,
    binding_path: String,
}

impl fmt::Debug for DingTalkOutboundReplier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DingTalkOutboundReplier")
            .field("binding", &self.binding.is_some())
            .field("decrypt", &self.decrypt)
            .field("transport", &"<dyn OpenApiTransport>")
            .field("app_url", &self.app_url)
            .field("binding_path", &self.binding_path)
            .finish()
    }
}

/// 装配参数（上游 `OutboundReplierConfig`）。
///
/// `Binding` 与 `AppURL` 是 `NeedsBinding` 提示的**必需**两项；缺任一项 ⇒ 提示被跳过
/// （状态告知与命令回执照常，上游逐字）。
pub struct OutboundReplierConfig {
    pub binding: Option<Arc<dyn BindingMinter>>,
    pub decrypt: Decrypter,
    pub transport: Arc<dyn OpenApiTransport>,
    /// 用户点进去兑换令牌的 Web 应用地址（例如 `https://multica.example`）。
    /// 绑定页由 Web 应用提供 ⇒ 必须指向 **app host**，不是 API host（与 Slack 的 `AppURL` 同款）。
    pub app_url: String,
    /// 绑定页路径；零值 ⇒ [`DEFAULT_BINDING_PATH`]。
    pub binding_path: String,
}

impl fmt::Debug for OutboundReplierConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OutboundReplierConfig")
            .field("binding", &self.binding.is_some())
            .field("decrypt", &self.decrypt)
            .field("transport", &"<dyn OpenApiTransport>")
            .field("app_url", &self.app_url)
            .field("binding_path", &self.binding_path)
            .finish()
    }
}

impl DingTalkOutboundReplier {
    /// 装配（上游 `NewOutboundReplier`）。
    #[must_use]
    pub fn new(config: OutboundReplierConfig) -> Self {
        let mut binding_path = config.binding_path;
        if binding_path.is_empty() {
            binding_path = DEFAULT_BINDING_PATH.to_string();
        }
        if !binding_path.starts_with('/') {
            binding_path.insert(0, '/');
        }
        Self {
            binding: config.binding,
            decrypt: config.decrypt,
            transport: config.transport,
            app_url: config.app_url.trim_end_matches('/').to_string(),
            binding_path,
        }
    }

    /// 可 `'static` 的句柄（脱离任务要它）。克隆的是 `Arc`，**不复制任何凭据**
    /// （本结构只有解密器与端口，不持明文令牌）。
    #[must_use]
    fn handle(&self) -> Arc<Self> {
        Arc::new(Self {
            binding: self.binding.clone(),
            decrypt: self.decrypt.clone(),
            transport: Arc::clone(&self.transport),
            app_url: self.app_url.clone(),
            binding_path: self.binding_path.clone(),
        })
    }

    /// 判决 → 文案 + 发送（上游 `Reply`）。错误只记 warn：**它跑在入站 ACK 路径之外**。
    pub async fn reply_now(
        &self,
        installation: &ResolvedInstallation,
        message: &InboundMessage,
        result: &RouteResult,
    ) {
        // 状态告知可能是在说**恢复出来的**待处理输入：实时回调可能属于另一个代际
        // ⇒ 只用它的会话路由（上游逐字）。
        let message = if matches!(
            result.outcome,
            Outcome::AgentOffline | Outcome::AgentArchived
        ) {
            let mut sanitized = InboundMessage {
                source: message.source.clone(),
                ..message.clone()
            };
            if sanitized.source.chat_type == ChatType::Group {
                sanitized.source.sender_id = String::new();
            }
            sanitized.message_id = String::new();
            sanitized
        } else {
            message.clone()
        };

        let outcome = match result.outcome {
            Outcome::NeedsBinding => {
                self.send_binding_prompt(installation, &message, result)
                    .await
            }
            Outcome::AgentOffline => self.post(installation, &message, AGENT_OFFLINE_TEXT).await,
            Outcome::AgentArchived => self.post(installation, &message, AGENT_ARCHIVED_TEXT).await,
            Outcome::FreshPending => self.post(installation, &message, FRESH_PENDING_TEXT).await,
            Outcome::ChatStarted => self.post(installation, &message, CHAT_STARTED_TEXT).await,
            Outcome::IssueUsage => {
                let text = if result.issue_usage_had_media {
                    ISSUE_USAGE_WITH_MEDIA_TEXT
                } else {
                    ISSUE_USAGE_TEXT
                };
                self.post(installation, &message, text).await
            }
            Outcome::Ingested => {
                if result.issue.is_none() {
                    // 普通聊天消息保持沉默。
                    return;
                }
                let text = if result.issue_duplicate {
                    issue_duplicate_text(result, &self.app_url)
                } else {
                    issue_created_text(result, &self.app_url)
                };
                self.post(installation, &message, &text).await
            }
            Outcome::Dropped => {
                let text = dropped_reply_text(result, &message);
                if text.is_empty() {
                    return;
                }
                self.post(installation, &message, &text).await
            }
        };
        if let Err(error) = outcome {
            tracing::warn!(
                installation_id = %installation.id,
                outcome = result.outcome.as_str(),
                error = %error,
                "dingtalk replier: reply failed"
            );
        }
    }

    /// 绑定提示：**私聊**送达一枚带一次性令牌的链接（上游 `sendBindingPrompt`）。
    ///
    /// # Errors
    ///
    /// 缺发件人 / 没接绑定服务 / 没配 app url / 铸令牌失败 / 发送失败。
    pub async fn send_binding_prompt(
        &self,
        installation: &ResolvedInstallation,
        message: &InboundMessage,
        result: &RouteResult,
    ) -> Result<(), ReplierError> {
        let sender = if result.sender.is_empty() {
            message.source.sender_id.clone()
        } else {
            result.sender.clone()
        };
        if sender.is_empty() {
            return Err(ReplierError::MissingSender);
        }
        let Some(binding) = &self.binding else {
            return Err(ReplierError::BindingUnavailable);
        };
        if self.app_url.is_empty() {
            return Err(ReplierError::AppUrlUnavailable);
        }
        let token = binding
            .mint(installation.workspace_id, installation.id, &sender)
            .await
            .map_err(|_| ReplierError::Mint)?;
        let bind_url = format!(
            "{}{}?token={}",
            self.app_url,
            self.binding_path,
            percent_encode(&token.raw)
        );
        let text = format!("{BINDING_PROMPT_PREFIX}{bind_url})\n\n{BINDING_LINK_TTL_HINT}");
        // 单次绑定链接**私聊**送达：在群里发这个链接，任何其他成员都能先兑换它，把发件人的
        // `DingTalk` 身份绑到**别人的**账号上（身份错绑）——上游逐字。
        let target = SendTarget::direct(sender);
        send_installation_text(&self.transport, &self.decrypt, installation, &target, &text)
            .await?;
        Ok(())
    }

    /// 把 `text` 发回**发起那条消息所在的会话**（上游 `post`）。
    ///
    /// # Errors
    ///
    /// 凭据解不开 / 发送失败。
    pub async fn post(
        &self,
        installation: &ResolvedInstallation,
        message: &InboundMessage,
        text: &str,
    ) -> Result<(), ReplierError> {
        let target = SendTarget::from_message(message);
        send_installation_text(&self.transport, &self.decrypt, installation, &target, text).await?;
        Ok(())
    }
}

impl OutboundReplier for DingTalkOutboundReplier {
    /// 同步接缝：推一个脱离任务后立刻返回（engine 的调用点绝不阻塞在 `DingTalk` HTTP 上）。
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

/// 从安装行解凭据并发一条文本（上游 `sendInstallationText`）。
///
/// 回复器与回执面（`ack.rs`）共用同一份凭据解码判据。
///
/// # Errors
///
/// 安装行不在载体里 / 凭据解不开 / 平台拒绝 / 传输失败。
pub async fn send_installation_text(
    transport: &Arc<dyn OpenApiTransport>,
    decrypt: &Decrypter,
    installation: &ResolvedInstallation,
    target: &SendTarget,
    text: &str,
) -> Result<String, ReplierError> {
    let credentials: Credentials = installation_row(installation)
        .and_then(|row| decode_credentials(&row.config, decrypt).ok())
        .ok_or(ReplierError::CredentialsUnavailable)?;
    let sender = Sender::from_credentials(Arc::clone(transport), &credentials);
    sender
        .send(target, text)
        .await
        .map_err(|error| ReplierError::Send { code: error.code() })
}

// =====================================================================
// 纯函数：地址、判决文案
// =====================================================================

/// 百分号编码（上游 `url.QueryEscape` 的等价物：空格是 `+`，其余非 unreserved 字节编 `%XX`）。
#[must_use]
pub fn percent_encode(raw: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(byte));
            }
            b' ' => out.push('+'),
            other => {
                // `write!` 到一个 `String` 永不失败（`String` 的 `fmt::Write` 是 infallible 的）。
                let _ = write!(out, "%{other:02X}");
            }
        }
    }
    out
}

/// 绑定页地址（诊断 / 用例）。
#[must_use]
pub fn binding_url(app_url: &str, binding_path: &str, raw_token: &str) -> String {
    format!(
        "{}{}?token={}",
        app_url.trim_end_matches('/'),
        binding_path,
        percent_encode(raw_token)
    )
}

/// 这条消息是不是一条**被寻址的** `/issue` 命令（上游 `isAddressedIssueCommand`）。
///
/// 只有这种消息值得一条错误 / 拒绝回复：发件人**要求**了一个动作，沉默会被当成"接受"。
#[must_use]
pub fn is_addressed_issue_command(message: &InboundMessage) -> bool {
    if !message.addressed_to_bot {
        return false;
    }
    let source = message.command_source_text();
    parse_issue_command(source).is_some()
}

/// 被丢弃的判决 → 用户看得见的拒绝（上游 `droppedReplyText`）。
#[must_use]
pub fn dropped_reply_text(result: &RouteResult, message: &InboundMessage) -> String {
    if !is_addressed_issue_command(message) {
        return String::new();
    }
    match result.drop_reason {
        Some(DropReason::NonWorkspaceMember) => ISSUE_NOT_MEMBER_TEXT.to_string(),
        Some(DropReason::RevokedInstallation) => ISSUE_DISABLED_TEXT.to_string(),
        _ => String::new(),
    }
}

/// `/issue` 建成文案（上游 `issueCreatedText`）。
#[must_use]
pub fn issue_created_text(result: &RouteResult, app_url: &str) -> String {
    let identifier = issue_markdown_identifier(result, app_url);
    let title = result
        .issue
        .as_ref()
        .map_or("", |issue| issue.title.as_str());
    if title.is_empty() {
        format!("✅ Created {identifier}")
    } else {
        format!("✅ Created {identifier} — {title}")
    }
}

/// `/issue` 因重复守卫未建成（上游 `issueDuplicateText`）。
#[must_use]
pub fn issue_duplicate_text(result: &RouteResult, app_url: &str) -> String {
    let identifier = issue_markdown_identifier(result, app_url);
    let title = result
        .issue
        .as_ref()
        .map_or("", |issue| issue.title.as_str());
    if title.is_empty() {
        format!("⚠️ Not created — active issue {identifier} already exists.")
    } else {
        format!("⚠️ Not created — active issue {identifier} already exists: {title}")
    }
}

/// 把显示出来的 issue 标识符链到它**稳定的 UUID**（上游 `issueMarkdownIdentifier`）。
///
/// 老的 `/issues/{key}` 链接依赖读者的"上一次 workspace"，而裸 `#number` 不是可路由的
/// issue 标识符 —— 所以链接指向 `/{slug}/issues/{uuid}`。
#[must_use]
pub fn issue_markdown_identifier(result: &RouteResult, app_url: &str) -> String {
    let identifier = issue_result_identifier(result);
    let slug = result.issue_workspace_slug.trim();
    let Some(issue) = result.issue.as_ref() else {
        return identifier;
    };
    if slug.is_empty() {
        return identifier;
    }
    // 基址必须是干净的 http(s) 根：带 userinfo / 查询串 / 片段的一律不发链接。
    let Ok(base) = Url::parse(app_url.trim()) else {
        return identifier;
    };
    if !matches!(base.scheme(), "http" | "https")
        || base.host_str().unwrap_or_default().is_empty()
        || !base.username().is_empty()
        || base.password().is_some()
        || base.query().is_some()
        || base.fragment().is_some()
    {
        return identifier;
    }
    let href = issue_web_link(base.as_str(), slug, &issue.id.to_string());
    if href.is_empty() {
        return identifier;
    }
    // 应用基址路径里的括号**不能**终止 Markdown 链接目标（即使它们是合法的 URL 路径字符）。
    let href = href.replace('(', "%28").replace(')', "%29");
    format!("[{}]({href})", escape_markdown_text(&identifier))
}

/// issue 的展示标识符：有 `ABC-42` 用它，否则 `#<number>`，再否则 UUID（上游
/// `issueResultIdentifier`）。
#[must_use]
pub fn issue_result_identifier(result: &RouteResult) -> String {
    if !result.issue_identifier.is_empty() {
        return result.issue_identifier.clone();
    }
    match result.issue.as_ref() {
        Some(issue) if issue.number > 0 => format!("#{}", issue.number),
        Some(issue) => issue.id.to_string(),
        None => String::new(),
    }
}

/// 让"未使用"的导入在**文档层面**有出处：`ChannelIssue` 是 [`RouteResult::issue`] 的元素类型。
const _: fn(&ChannelIssue) = |_| {};

#[cfg(test)]
mod tests;
