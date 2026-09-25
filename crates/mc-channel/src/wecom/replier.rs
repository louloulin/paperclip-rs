//! `WeCom` 的 [`engine::OutboundReplier`]：把引擎的判决翻成一条用户看得见的消息，
//! 走**同一条** aibot WebSocket（`aibot` 没有 REST 出站；每一次写都在 socket 上，经
//! `sendersRegistry` 找活的 socket）—— 上游 `internal/integrations/wecom/replier.go`，**298 行**。
//!
//! - **写者**：M7-17（`LUM-1782` / `docs/60-M7-PLAN.md` §3.3）。
//! - **上游定位**（文件头注释逐字）：它处理引擎的 `needs_binding` / `agent_offline` /
//!   `agent_archived` / `issue_created` 判决。
//!
//! # 判决 → 文案（上游逐字）
//!
//! | `Outcome` | 行为 |
//! | --- | --- |
//! | `NeedsBinding` | **私聊**铸一枚单次绑定令牌 + 「点这里绑定」提示（群里发这个链接会把令牌暴露给全群）；群里再补一条**不带令牌**的告知 |
//! | `AgentOffline` / `AgentArchived` | 一条状态告知，用户不至于干等 |
//! | `FreshPending` / `ChatStarted` | 新会话已就绪 / 已开始 |
//! | `IssueUsage` | `/issue` 缺标题 ⇒ 用法提示 |
//! | `Ingested` | **只有带 issue 的**才回（普通聊天消息**保持沉默** —— agent 自己的回答走出站订阅者） |
//!
//! # 与上游的形态差异（**逐条登记** `docs/32` §34）
//!
//! 1. **同步接缝 + 脱离任务**（D4）：engine 的 [`OutboundReplier::reply`] 是同步方法，
//!    本仓的同步方法只把一个脱离任务推出去，真正的工作在 [`WeComOutboundReplier::reply_now`] 里
//!    （与 M7-4 / M7-5 / M7-8 同款）。
//! 2. **令牌类型端口化**（D5）：上游直接依赖 `*BindingTokenService` 的 `pgtype.UUID` 形参；
//!    本仓的 adapter 不直接写 DB ⇒ [`Binder`] 端口用 `Id`，且只取**明文**那一半
//!    （[`MintedBinding::raw`]）。
//! 3. **成员文本的破坏闸是端口**（D6）：上游 `breakMemberLinks` 在 `markdown.go`（**M7-19**），
//!    它有两段（行内邻接 + 引用定义）。本片落 [`MemberLinks`] 端口，并给一段**现成的**实现
//!    [`AdjacencyBreaker`]（复用 M7-1 的 `break_markdown_link_adjacency`）；
//!    **没有**配置闸时，`/issue` 确认里的标题**整个省掉**（失败关闭，见 [`issue_created_text`]）。
//!
//! # 凭据纪律（`docs/60` §2.3）
//!
//! 绑定令牌的**明文只出现在一条 URL 里**（那是它唯一的用途），**不进**日志：任何 `tracing::*`
//! 都不插值 `bind_url`；[`MintedBinding`] 手写 `Debug` 输出 `<redacted>`。

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;

use mc_core::channel::message::{ChatType, InboundMessage};
use mc_core::id::Id;

use crate::engine::resolvers::{
    ChannelIssue, OutboundReplier, Outcome, ResolvedInstallation, RouteResult,
};
use crate::message::break_markdown_link_adjacency;
use crate::wecom::outbound::{spawn_detached, LiveSender, OutboundError, SenderLookup};
use crate::wecom::ws_frame::aibot_chat_type_from_channel;

/// 绑定页的默认路径（上游 `BindingPath` 零值 ⇒ `"/wecom/bind"`）。
pub const DEFAULT_BINDING_PATH: &str = "/wecom/bind";

// —— 文案（上游十二行常量，逐字） ——

/// 上游 `agentOfflineText`。
pub const AGENT_OFFLINE_TEXT: &str = "⚠️ 智能体当前不在线，你的消息已收到，等它上线后会处理。";

/// 上游 `agentArchivedText`。
pub const AGENT_ARCHIVED_TEXT: &str = "⚠️ 该智能体已归档，无法回复。请联系工作区管理员。";

/// 上游 `freshPendingText`。
pub const FRESH_PENDING_TEXT: &str =
    "✅ 已准备从空上下文运行。你的下一条聊天消息仍会进入当前对话，但不会带上之前的上下文。";

/// 上游 `chatStartedText`。
pub const CHAT_STARTED_TEXT: &str = "✅ 已新建 Multica 对话。你的下一条消息会进入该对话。";

/// 上游 `issueUsageText`。
pub const ISSUE_USAGE_TEXT: &str =
    "请填写任务标题，格式如下：\n\n`/issue <标题>`\n`[描述]`（可选）";

/// 上游那条"链接刚才已经发给你了"（节流命中时的替代文案）。
pub const BINDING_REUSED_TEXT: &str = "👋 绑定链接刚才已经发给你了，就在上方，请直接点击完成绑定。";

/// 群聊里的那条**不带令牌**的告知（上游 `sendBindingPrompt` 的最后一段）。
pub const BINDING_GROUP_ACK_TEXT: &str = "👋 已把绑定链接私发给你，请在与我的单聊里点击完成绑定。";

// =====================================================================
// 端口
// =====================================================================

/// 铸一枚绑定令牌的结果（上游 `BindingToken` 的**明文那一半**）。
#[derive(Clone, PartialEq, Eq)]
pub struct MintedBinding {
    /// 明文令牌（只进 URL）。节流命中时为空。
    pub raw: String,
    /// 节流命中：一条活着的链接已经在这个用户手上。
    pub reused: bool,
}

impl fmt::Debug for MintedBinding {
    /// 手写脱敏：明文令牌是**持票凭据**，绝不进 `Debug`（凭据纪律第 1 条）。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MintedBinding")
            .field(
                "raw",
                &if self.raw.is_empty() {
                    "<empty>"
                } else {
                    "<redacted>"
                },
            )
            .field("reused", &self.reused)
            .finish()
    }
}

/// [`super::binding::BindingTokenService`] 里 `send_binding_prompt` 要的那一小块。
///
/// 上游把它声明成接口，是为了让"群 vs 私聊"的路由能用替身验（不必真的铸一枚库里的令牌）；
/// 本仓同一个理由，而且更硬：adapter **不得**直接写 DB（`docs/60` §2.6 第 1 条）。
#[async_trait]
pub trait Binder: Send + Sync {
    /// 铸一枚令牌（上游 `Mint`）。
    ///
    /// # Errors
    ///
    /// 存储层故障；只报结构信息（**不含**令牌）。
    async fn mint(
        &self,
        workspace_id: Id,
        installation_id: Id,
        channel_user_id: &str,
    ) -> Result<MintedBinding, String>;
}

/// 把成员写的文本塞进**机器人签名**的消息之前要过的闸（上游 `breakMemberLinks`）。
///
/// 它的两段（行内邻接 + 引用定义）在上游是**一个**入口，理由逐字：一个调用点不可能只取一段、
/// 忘掉另一段。本仓的接口照旧是**一个**方法；M7-19 的 `markdown.rs` 落地时实现它即可。
pub trait MemberLinks: Send + Sync {
    /// 破坏文本里的 Markdown 链接构造。
    fn break_links(&self, text: &str) -> String;
}

/// 现成的这一段实现：**行内邻接**（`](` → `] (`），复用 M7-1 的
/// [`break_markdown_link_adjacency`]。
///
/// 🔴 它**不**覆盖引用定义那一段（`[标签]: https://…`）—— 那一段归 M7-19 的 `markdown.rs`
/// （上游 `breakLinkReferenceDefinitions`，约两百行）。在它落地之前，一个只配了本实现的部署
/// 对**引用定义**形态的标题是**降级**而不是等价；见 `docs/32` §34 的 D6 与 R3。
#[derive(Debug, Clone, Copy, Default)]
pub struct AdjacencyBreaker;

impl MemberLinks for AdjacencyBreaker {
    fn break_links(&self, text: &str) -> String {
        break_markdown_link_adjacency(text)
    }
}

// =====================================================================
// 回复器
// =====================================================================

/// `WeCom` 的判决驱动回复器（上游 `OutboundReplier`）。
pub struct WeComOutboundReplier {
    binding: Option<Arc<dyn Binder>>,
    senders: Option<Arc<dyn SenderLookup>>,
    app_url: String,
    binding_path: String,
    member_links: Option<Arc<dyn MemberLinks>>,
}

impl fmt::Debug for WeComOutboundReplier {
    /// 手写：端口只报**存在性**。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WeComOutboundReplier")
            .field("binding", &self.binding.is_some())
            .field("senders", &self.senders.is_some())
            .field("app_url_configured", &!self.app_url.is_empty())
            .field("binding_path", &self.binding_path)
            .field("member_links", &self.member_links.is_some())
            .finish()
    }
}

impl WeComOutboundReplier {
    /// 建一个回复器。`binding` 与 `app_url` 是 `NeedsBinding` 提示的前置；缺了它，
    /// 提示被跳过（离线 / 归档 / `/issue` 的告知照发）。
    #[must_use]
    pub fn new(senders: Option<Arc<dyn SenderLookup>>, app_url: &str, binding_path: &str) -> Self {
        let binding_path = if binding_path.is_empty() {
            DEFAULT_BINDING_PATH.to_string()
        } else if binding_path.starts_with('/') {
            binding_path.to_string()
        } else {
            format!("/{binding_path}")
        };
        Self {
            binding: None,
            senders,
            app_url: app_url.trim_end_matches('/').to_string(),
            binding_path,
            member_links: None,
        }
    }

    /// 接上铸令牌的端口（上游 `OutboundReplierConfig.Binding`）。
    ///
    /// 上游逐字：**只在非 nil 时**赋进接口 —— 一个装在接口里的 `nil *BindingTokenService` 是一个
    /// 非 nil 的接口值握着一个类型化的 nil，会击穿 `r.binding == nil` 那道守卫并在 `Mint` 上 panic。
    /// 本仓的 `Option` 从类型上就没有这个坑。
    #[must_use]
    pub fn with_binder(mut self, binder: Arc<dyn Binder>) -> Self {
        self.binding = Some(binder);
        self
    }

    /// 接上成员文本的破坏闸（上游硬依赖 `breakMemberLinks`；本仓是端口，见 D6）。
    #[must_use]
    pub fn with_member_links(mut self, member_links: Arc<dyn MemberLinks>) -> Self {
        self.member_links = Some(member_links);
        self
    }

    /// 绑定页的路径（诊断用）。
    #[must_use]
    pub fn binding_path(&self) -> &str {
        &self.binding_path
    }

    /// 上游 `Reply` 的异步那一半：把每个判决路由到它那条用户可见的消息。
    ///
    /// 错误**只记日志、不向上传**：回复器跑在入站 ACK 路径**之外**（那条 goroutine 归
    /// engine 的 Router）。
    pub async fn reply_now(
        &self,
        installation: &ResolvedInstallation,
        message: &InboundMessage,
        result: &RouteResult,
    ) {
        let installation_id = installation.id;
        let outcome = result.outcome;
        let sent = match outcome {
            Outcome::NeedsBinding => {
                self.send_binding_prompt(installation, message, result)
                    .await
            }
            Outcome::AgentOffline => self.post(installation, message, AGENT_OFFLINE_TEXT).await,
            Outcome::AgentArchived => self.post(installation, message, AGENT_ARCHIVED_TEXT).await,
            Outcome::FreshPending => self.post(installation, message, FRESH_PENDING_TEXT).await,
            Outcome::ChatStarted => self.post(installation, message, CHAT_STARTED_TEXT).await,
            Outcome::IssueUsage => self.post(installation, message, ISSUE_USAGE_TEXT).await,
            Outcome::Ingested => {
                // 只有 `/issue` 建成的消息值得一条确认；普通的聊天消息保持沉默
                // （agent 自己的回答走出站订阅者 / `Channel::send`）。
                match result.issue.as_ref() {
                    Some(issue) => {
                        let text = issue_reply_text(result, issue, self.member_links.as_deref());
                        self.post(installation, message, &text).await
                    }
                    None => return,
                }
            }
            // 被丢弃的消息不是这个回复器的业务（上游 `Reply` 在这里没有分支）。
            Outcome::Dropped => return,
        };
        if let Err(error) = sent {
            tracing::warn!(
                outcome = outcome.as_str(),
                installation_id = %installation_id.0,
                error = %error,
                "wecom replier: notice not delivered"
            );
        }
    }

    /// 上游 `sendBindingPrompt`。
    async fn send_binding_prompt(
        &self,
        installation: &ResolvedInstallation,
        message: &InboundMessage,
        result: &RouteResult,
    ) -> Result<(), OutboundError> {
        let sender_id = if result.sender.is_empty() {
            message.source.sender_id.clone()
        } else {
            result.sender.clone()
        };
        if sender_id.is_empty() {
            return Err(OutboundError::lookup("bind prompt", "missing sender id"));
        }
        let Some(binding) = self.binding.as_ref() else {
            return Err(OutboundError::lookup(
                "bind prompt",
                "binding service not configured",
            ));
        };
        if self.app_url.is_empty() {
            return Err(OutboundError::lookup(
                "bind prompt",
                "app url not configured",
            ));
        }
        let token = binding
            .mint(installation.workspace_id, installation.id, &sender_id)
            .await
            .map_err(|message| OutboundError::lookup("mint binding token", message))?;
        // 节流压住了这次铸币：一条活着的链接已经在用户手上。库里**只**存过它的哈希，
        // 所以没有任何 URL 可以重建 —— 指回他们手上那条消息。节流窗口比 TTL 短得多，
        // 所以那条链接还剩它大部分寿命。
        //
        // 这段文案由 `post_private` 发出去，而它总是落在**单聊**里 —— 与早前那条链接所在的
        // 同一段对话，无论这次是哪个房间触发的。所以它指的是**当前这条线**，而不是让读者去一个
        // 他们正在读的聊。只有下面那段群里的 ack 跑在房间里，而它正是那个点名"单聊"的。
        let text = if token.reused {
            BINDING_REUSED_TEXT.to_string()
        } else {
            format!(
                "👋 请先绑定你的 Multica 账号，才能与我对话：\n{}{}?token={}\n（链接 15 分钟内有效）",
                self.app_url,
                self.binding_path,
                url_encode(&token.raw)
            )
        };
        // 上游逐字：一枚绑定令牌是**持票凭据** —— `Redeem` 只检查兑换者属于令牌的 workspace，
        // 而绑定页会在加载时以"当前登录的那个人"兑换。把它发到 `msg.Source.ChatID`（在群里
        // **就是那个群**）会让任何一个成员抢先点击、把发送者的 `WeCom` userid 绑到自己的
        // Multica 账号上，此后发送者（含 `/issue`）的消息就解析成那个劫持者。
        // ⇒ 用 `chat_type=1` 私发给发送者**自己的** userid（与 `outbound.rs` 的收件箱推送同一个
        // 地址），**绝不**发到房间里。
        self.post_private(installation, &sender_id, &text).await?;
        // 一次群里的触发**仍然**需要一个回答 —— 沉默读起来像一个坏掉的机器人 —— 但那条回答
        // **不带令牌**、也不点名任何人。它只在私聊那条**被接受之后**才发，所以房间永远不会被
        // 指向一条线上拒绝了的消息。一次单聊触发已经在它唯一的房间里拿到提示了。
        if aibot_chat_type_from_channel(message.source.chat_type) == 1 {
            return Ok(());
        }
        self.post(installation, message, BINDING_GROUP_ACK_TEXT)
            .await
    }

    /// 上游 `postPrivate`：把文本投到某个用户的**单聊**（`chat_type=1`），不论哪个房间触发了
    /// 这条消息。用于"持票凭据"内容（绑定链接）—— 它绝不许落在群里。
    async fn post_private(
        &self,
        installation: &ResolvedInstallation,
        user_id: &str,
        text: &str,
    ) -> Result<(), OutboundError> {
        if user_id.is_empty() {
            return Err(OutboundError::lookup("private post", "missing user id"));
        }
        let sender = self.live_sender(installation)?;
        let single = aibot_chat_type_from_channel(ChatType::P2p);
        sender
            .send_text(user_id, single, text, None)
            .await
            .map_err(OutboundError::Send)
    }

    /// 上游 `post`：在注册表里找这条安装的活 `wsSender`，并用给定文本推一条 `aibot_send_msg`。
    /// 监管器没有活连接（租约翻转后的重连中、或者刚被撤销）时报"连接没准备好"。
    async fn post(
        &self,
        installation: &ResolvedInstallation,
        message: &InboundMessage,
        text: &str,
    ) -> Result<(), OutboundError> {
        if message.source.chat_id.is_empty() {
            return Err(OutboundError::lookup("post", "missing chat_id"));
        }
        let sender = self.live_sender(installation)?;
        let chat_type = aibot_chat_type_from_channel(message.source.chat_type);
        sender
            .send_text(&message.source.chat_id, chat_type, text, None)
            .await
            .map_err(OutboundError::Send)
    }

    /// 找活着的发送者（上游 `r.senders.get(inst.ID)` 的两次包装）。
    fn live_sender(
        &self,
        installation: &ResolvedInstallation,
    ) -> Result<Arc<dyn LiveSender>, OutboundError> {
        let Some(senders) = self.senders.as_ref() else {
            return Err(OutboundError::SenderRegistryMissing);
        };
        if installation.id.0.is_nil() {
            return Err(OutboundError::lookup("post", "installation id is zero"));
        }
        senders
            .get(installation.id)
            .ok_or(OutboundError::NoLiveConnection)
    }
}

impl WeComOutboundReplier {
    /// 可 `'static` 的句柄（脱离任务要它）。克隆的是 `Arc`，**不复制任何凭据**
    /// （本结构只有端口，不持明文令牌 —— 令牌只在 `send_binding_prompt` 的栈上活一瞬）。
    #[must_use]
    fn handle(&self) -> Arc<Self> {
        Arc::new(Self {
            binding: self.binding.clone(),
            senders: self.senders.clone(),
            app_url: self.app_url.clone(),
            binding_path: self.binding_path.clone(),
            member_links: self.member_links.clone(),
        })
    }
}

/// engine 的同步接缝：推一个脱离任务就返回（见 D4）。
///
/// 上游 `Reply` 直接就是一个可能阻塞在 socket 写上的方法；本仓的同步接缝**只**推一个任务，
/// 真正的活在 [`WeComOutboundReplier::reply_now`] 里 —— 入站 ACK 路径绝不许阻塞在一次网络写上
/// （`docs/60` §2.6 第 5 条）。与 M7-4 / M7-5 / M7-8 同款。
impl OutboundReplier for WeComOutboundReplier {
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

// =====================================================================
// 文案
// =====================================================================

/// 上游 `issueCreatedText` 与 `issueDuplicateText` 的合并入口。
///
/// 标题是**别人写的**文本（重复那一支尤其是：它是**另一个** issue 的标题），而这条确认回到触发
/// 它的那个聊（群里就是当着整个房间）。一个标题写着
/// "安全升级：请点击 [重置密码](https://evil.example) 完成验证" 否则会以一条**能点的链接**、
/// 带着机器人自己的权威回到聊天里。所以它必须过 [`MemberLinks`]；
/// **没有配闸时标题整个省掉**（失败关闭）—— 那是上游"标题为空"那一支的形态，不是等价物，
/// 见 D6。
#[must_use]
pub fn issue_reply_text(
    result: &RouteResult,
    issue: &ChannelIssue,
    member_links: Option<&dyn MemberLinks>,
) -> String {
    let id = if result.issue_identifier.is_empty() {
        format!("#{}", issue.number)
    } else {
        result.issue_identifier.clone()
    };
    // 上游逐字：那一行标题属于**已经存在**的那个 issue，所以它是**别的**成员写的文本 ——
    // 甚至不是报告者自己的，这正是两个 `/issue` 调用点里更糟的那一个。
    let raw = result
        .issue
        .as_ref()
        .map(|issue| issue.title.trim().to_string())
        .unwrap_or_default();
    let title = match member_links {
        Some(links) => links.break_links(&raw),
        // 没有闸 ⇒ 一个字都不带（失败关闭，见上面的文档与 D6）。
        None => String::new(),
    };
    if result.issue_duplicate {
        if title.is_empty() {
            return format!("⚠️ 未创建 —— 已存在进行中的 {id}");
        }
        return format!("⚠️ 未创建 —— 已存在进行中的 {id} — {title}");
    }
    if title.is_empty() {
        return format!("✅ 已创建 {id}");
    }
    format!("✅ 已创建 {id} — {title}")
}

/// 上游 `url.QueryEscape`（本仓用 `percent-encoding` 的表，与 `mc-channel::message` 同源）。
///
/// 令牌里只有 URL-安全字符，所以这条函数在实践中是恒等的 —— 它存在是为了让"令牌进 URL"
/// 这件事**只有一处**（`url.QueryEscape` 的契约逐字保留）。
#[must_use]
pub fn url_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            other => {
                use std::fmt::Write as _;
                let _ = write!(out, "%{other:02X}");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests;
