//! lark 的**判决回复器**：把 engine 的判决翻成给用户看的一条消息
//! （上游 `internal/integrations/lark/outcome_replier.go`，446 行）。
//!
//! - **写者**：M7-13（`docs/60-M7-PLAN.md` §3.3；写集勘误见 `docs/32` §32）。
//! - **上游定位**：`OutcomeReplier` —— 上游 `EventEmitter` 契约的**出站那一半**。
//!   `NeedsBinding` 把绑定提示发回发件人的 `open_id`；`AgentOffline` / `AgentArchived`
//!   往会话里发状态卡；`FreshPending` / `IssueUsage` 发命令指引；`OutcomeIngested` 归
//!   patcher（任务生命周期）；`OutcomeDropped` **沉默**。
//! - **判决的输入是 M7-12 的 [`super::resolvers::DispatchResult`]**（`dispatch_result_from_engine`
//!   是纯映射）⇒ 本文件**只写文案与投递**，不重新解释判决。
//!
//! # 判决 → 行为（上游 `Reply` 的 switch，**这是唯一真源**）
//!
//! | `Outcome` | 行为 |
//! | --- | --- |
//! | `NeedsBinding` | 铸一枚单次绑定令牌 + 发**私聊**绑定卡（发给发件人的 `open_id`）。群聊里 "私聊不可达"（平台码 [`crate::lark::client::CODE_NO_AVAILABILITY`]）⇒ 回落一条**会话内**文案 |
//! | `AgentOffline` | 一张灰头状态卡（正文 [`AGENT_OFFLINE_COPY`]） |
//! | `AgentArchived` | 一张灰头状态卡（正文 [`AGENT_ARCHIVED_COPY`]） |
//! | `FreshPending` | 一张确认卡（[`FRESH_PENDING_COPY`]） |
//! | `ChatStarted` | 一张确认卡（[`CHAT_STARTED_COPY`]） |
//! | `IssueUsage` | 用法提示卡（带媒体 ⇒ [`ISSUE_USAGE_WITH_MEDIA_COPY`]） |
//! | `Ingested` | **只有带 issue 的**才回：新建确认 / 活跃重复冲突（普通聊天消息**保持沉默** —— agent 自己的回复走 patcher） |
//! | `Dropped` | 不回 |
//!
//! # 从上游逐字搬来的四条细节
//!
//! 1. **回复目标与会话层回落**：[`inbound_reply_target`] 与 patcher 的 `threadReplyTarget`
//!    逐条对齐（上游注释逐字：*Keep the two in lockstep: a user cannot tell whether an answer
//!    came from the synchronous replier or the task patcher, so they must not place their replies
//!    differently.*）；投递共用 [`super::outbound::send_with_reply_fallback`] 那条**分类过的**
//!    回落（**只有**话题收不了才回落，传输 / 5xx / 限流一律不回落）。
//! 2. **issue 文案里的标题要消毒**：先 `break_markdown_link_adjacency`（拆掉不可信文本里的
//!    链接邻接）**再**把 `<` 换成 `&lt;` —— **顺序不能反**（上游注释逐字：*member-authored links
//!    and mentions are handled as visible text*）。
//! 3. **深链只认 `IssueIdentifier`**：`#42` 那个回落值是**降级标签**，永远不是可路由的标识符
//!    ⇒ [`crate::message::issue_web_link`] 拿的是 `issue_identifier`，不是显示值。
//! 4. **失败只告警不返回**：回复器跑在入站 ACK 路径**之外**，一次发送失败不该冒泡成"投递失败"
//!    （上游逐字：*Any command state or chat message is already durable by the time we get here,
//!    so a reply failure cannot roll it back.*）。
//!
//! # 本仓的形态差异（登记 `docs/32` §32.1）
//!
//! - **D11 上游 `noopReplier` 的降级判据 → [`build`]**：上游 `NewLarkOutcomeReplier` 在依赖缺失
//!   或 `APIClient.IsConfigured() == false` 时**降级成 noop**（并打一条 warn）。本仓的
//!   `Arc<dyn ApiClient>` 不在装配点判"配没配"（那会让"未装配"变成运行期惊喜）⇒ 同一个判据
//!   收进 [`build`] 一个函数，`None` 的字段就是"没接线"。`NoopOutcomeReplier` 照落（它同时是
//!   "诚实的空实现"的形态证据：装了它 = 判决被记下但**不**回复）。
//! - **D12 同步接缝 + 脱离任务**：engine 的 [`OutboundReplier::reply`] 是**同步**方法（调用点在
//!   `tokio::spawn` 里），而上游的 `Reply` 直接阻塞着发 HTTP ⇒ 同步方法只推一个脱离任务，真正
//!   的工作在 async 的 [`LarkOutcomeReplier::reply_now`] 里（与 `slack::replier` 逐字同款）。
//! - **D13 上游 `SendInteractiveCard` 用**同一条**传输**：上游的 notice 卡走
//!   `APIClient.SendInteractiveCard`，绑定卡走**另一个**专用方法 `SendBindingPromptCard`
//!   （模板留在客户端里）。本仓逐字保留这个分工（[`crate::lark::client::ApiClient`] 的两个方法
//!   都是 M7-10 落的）。

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use mc_core::channel::message::{ChatType, InboundMessage};
use mc_core::id::Id;

use super::client::{ApiClient, ApiError, ErrorClass, CODE_NO_AVAILABILITY};
use super::feishu_channel::credentials::{installation_credentials_for, Decrypter};
use super::outbound::{
    render_notice_card, send_with_reply_fallback, DeliveryOutcome, FallbackError, SkipReason,
    DEFAULT_CARD_HEADER,
};
use super::params::{BindingPromptParams, SendCardParams, SendTextParams};
use super::resolvers::{
    dispatch_result_from_engine, DispatchResult, LarkInstallation, Outcome, TYPE_LARK,
};
use super::types::{ChatId, OpenId};
use crate::engine::resolvers::{
    EngineError, EngineResult, OutboundReplier, ResolvedInstallation, RouteResult,
};

// =====================================================================
// 常量（上游 `BindingPath` 零值与文案 const 块，**逐字**）
// =====================================================================

/// 绑定页的默认路径（上游 `bindingPath` 零值 ⇒ `"/lark/bind"`）。
pub const DEFAULT_BINDING_PATH: &str = "/lark/bind";

/// 状态卡的默认头部（上游 `noticeHeader: "Multica"`）。
pub const DEFAULT_NOTICE_HEADER: &str = DEFAULT_CARD_HEADER;

/// agent 离线（上游 `agentOfflineCopy`，**逐字**）。
pub const AGENT_OFFLINE_COPY: &str =
    "Agent 当前离线，消息已记录。下次 daemon 上线后会自动继续处理。";
/// agent 已归档（上游 `agentArchivedCopy`，**逐字**）。
pub const AGENT_ARCHIVED_COPY: &str =
    "这个 Agent 已被归档，无法继续处理消息。请联系工作区管理员恢复或重新绑定。";
/// `/clear` 后的空上下文确认（上游 `freshPendingCopy`，**逐字**）。
pub const FRESH_PENDING_COPY: &str =
    "✅ 已准备从空上下文运行。你的下一条聊天消息仍会进入当前对话，但不会带上之前的上下文。";
/// `/new` 的新会话确认（上游 `chatStartedCopy`，**逐字**）。
pub const CHAT_STARTED_COPY: &str = "✅ 已新建 Multica 对话。你的下一条消息会进入该对话。";
/// `/issue` 缺标题（上游 `issueUsageCopy`，**逐字**）。
pub const ISSUE_USAGE_COPY: &str =
    "请填写任务标题，格式如下：\n\n`/issue <标题>`\n`[描述]`（可选）";
/// `/issue` 缺标题但这条消息**带媒体**（上游 `issueUsageWithMediaCopy`，**逐字**）。
pub const ISSUE_USAGE_WITH_MEDIA_COPY: &str = "请添加标题，并与图片或视频一起重新发送（*图片或视频可以位于命令之前或之后*）：\n\n`/issue <标题>`\n`[描述]`（可选）";
/// 群聊里私聊不可达时的回落文案（上游 `bindingPromptUnavailableCopy`，**逐字**）。
pub const BINDING_PROMPT_UNAVAILABLE_COPY: &str = "你还未绑定 Multica 账户，绑定卡片未能发送到你的私聊。\n请先打开机器人对话并发送一条消息，再回到群里重试；仍失败请联系管理员检查应用可用范围。";

// =====================================================================
// 端口
// =====================================================================

/// 铸一枚绑定令牌（上游 `BindingTokenMinter`；`*BindingTokenService` 满足它）。
///
/// 上游注释逐字：*Keeping this as an interface lets tests pin the Lark binding URL without
/// constructing a database-backed token service.*
#[async_trait]
pub trait BindingTokenMinter: Send + Sync {
    /// 铸一枚单次令牌；**明文只在返回值里出现一次**（落库只存哈希）。
    ///
    /// # Errors
    ///
    /// 任何失败 ⇒ `Err(String)`（调用方只记 warn，**不**回显令牌）。
    async fn mint(
        &self,
        workspace_id: Id,
        installation_id: Id,
        open_id: &str,
    ) -> Result<MintedBinding, String>;
}

/// 一枚刚铸出的绑定令牌（上游 `BindingToken`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MintedBinding {
    /// 明文（**只在绑定 URL 里出现一次**；绝不落日志）。
    pub raw: String,
    /// 过期时刻（上游用它决定文案里的"15 分钟"，本仓只把它带出来）。
    pub expires_at: DateTime<Utc>,
}

/// 状态卡头部要的 agent 名字（上游 `OutcomeReplierQueries` 的 `GetAgent`）。
///
/// 上游只要 `Name` 这一个字段 —— 本仓照落成"要名字"而不是"要整行 AgentRow"，于是端口不必
/// 依赖 `mc-repos` 的 agent 面形态。
#[async_trait]
pub trait OutcomeReplierQueries: Send + Sync {
    /// 取 agent 的显示名；查不到 ⇒ `Ok(None)`（回落 [`DEFAULT_NOTICE_HEADER`]）。
    ///
    /// # Errors
    ///
    /// 链路失败（调用方**降级**成默认头部，**不**让整次回复失败 —— 上游逐字：
    /// `if agent, aerr := ...; aerr == nil && agent.Name != ""`）。
    async fn agent_name(&self, agent_id: Id) -> EngineResult<Option<String>>;
}

// =====================================================================
// 装配（上游 `OutcomeReplierConfig` + `NewLarkOutcomeReplier`）
// =====================================================================

/// 回复器的装配袋（上游 `OutcomeReplierConfig`）。
///
/// `app_url` 是 **Multica web app** 的主机（用户点进去兑换 / 打开 issue），与
/// `MULTICA_PUBLIC_URL`（后端 / API 的公开地址）**故意分开**（上游注释逐字）。空 ⇒ 绑定流程
/// 只能在日志里打出 `open_id`，产不出可点的卡。
#[derive(Clone, Default)]
pub struct OutcomeReplierConfig {
    /// 出站 HTTP 客户端；`None` ⇒ 没有发送面。
    pub client: Option<Arc<dyn ApiClient>>,
    /// 绑定令牌服务；`None` ⇒ 绑定卡那一支关掉。
    pub binding: Option<Arc<dyn BindingTokenMinter>>,
    /// 解密器；`None` ⇒ 一条凭据都解不开。
    pub decrypt: Option<Decrypter>,
    /// agent 名字的查询口；`None` ⇒ 状态卡头部回落 [`DEFAULT_NOTICE_HEADER`]。
    pub queries: Option<Arc<dyn OutcomeReplierQueries>>,
    /// web app 主机（尾斜杠会被剥掉）。
    pub app_url: String,
    /// 绑定页路径；空 ⇒ [`DEFAULT_BINDING_PATH`]（不带前导斜杠会被补上）。
    pub binding_path: String,
}

impl fmt::Debug for OutcomeReplierConfig {
    /// 手写：端口只报存在性，解密器自带脱敏 `Debug`，**没有任何令牌字段**。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OutcomeReplierConfig")
            .field("has_client", &self.client.is_some())
            .field("has_binding", &self.binding.is_some())
            .field("has_decrypt", &self.decrypt.is_some())
            .field("has_queries", &self.queries.is_some())
            .field("app_url", &self.app_url)
            .field("binding_path", &self.binding_path)
            .finish()
    }
}

/// `build` 的判决（纯函数 ⇒ 可单测；`build` 只是它的执行）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplierChoice {
    /// 依赖齐全且客户端已配置 ⇒ 装生产回复器。
    Lark,
    /// 缺依赖 / 客户端没配 ⇒ 装 noop（上游逐字的降级）。
    Noop,
}

/// 按上游的降级判据给出**判决**（不构造任何东西）。
#[must_use]
pub fn choose(cfg: &OutcomeReplierConfig) -> ReplierChoice {
    let wired = cfg.client.is_some()
        && cfg.binding.is_some()
        && cfg.decrypt.is_some()
        && cfg.queries.is_some();
    if !wired {
        return ReplierChoice::Noop;
    }
    match &cfg.client {
        Some(client) if client.is_configured() => ReplierChoice::Lark,
        _ => ReplierChoice::Noop,
    }
}

/// 按上游的降级判据造一个回复器端口（**这是接线点**）。
///
/// 上游 `NewLarkOutcomeReplier` 的逐条判据：
///
/// - 四个依赖（`APIClient` / `BindingSvc` / `Credentials` / `Queries`）任一缺失 ⇒ noop；
/// - `APIClient.IsConfigured() == false` ⇒ noop（并打一条 warn）；
/// - `AppURL` 空 ⇒ **不**降级，只打一条 warn（绑定卡的 CTA 点不动，其余照发）。
///
/// 本仓照落，且把 warn 也照落 —— "为什么没有回复"必须能从日志里看出来。
#[must_use]
pub fn build(cfg: OutcomeReplierConfig) -> Arc<dyn OutboundReplier> {
    let OutcomeReplierConfig {
        client,
        binding,
        decrypt,
        queries,
        app_url,
        binding_path,
    } = cfg;
    let (Some(client), Some(binding), Some(decrypt), Some(queries)) =
        (client, binding, decrypt, queries)
    else {
        tracing::warn!("lark outcome replier: wiring incomplete; downgrading to the no-op replier");
        return Arc::new(NoopOutcomeReplier);
    };
    if !client.is_configured() {
        tracing::warn!(
            "lark outcome replier: ApiClient.is_configured()=false; downgrading to the no-op \
             replier"
        );
        return Arc::new(NoopOutcomeReplier);
    }
    if app_url.is_empty() {
        tracing::warn!(
            "lark outcome replier: app url not set; the binding prompt CTA will not work"
        );
    }
    Arc::new(LarkOutcomeReplier {
        client,
        binding,
        decrypt,
        queries,
        app_url: app_url.trim_end_matches('/').to_string(),
        binding_path: normalize_binding_path(&binding_path),
        notice_header: DEFAULT_NOTICE_HEADER.to_string(),
    })
}

/// 绑定路径的归一化（上游 `if !strings.HasPrefix(bindingPath, "/") { … }`）。
#[must_use]
pub fn normalize_binding_path(path: &str) -> String {
    if path.is_empty() {
        return DEFAULT_BINDING_PATH.to_string();
    }
    if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    }
}

// =====================================================================
// noop（上游 `noopReplier` / `NewNoopOutcomeReplier`）
// =====================================================================

/// 安全的默认值：lark 在**没有**出站面（stub 客户端）或**没有**绑定令牌服务时被装配成它。
///
/// 上游逐字：*It logs each outcome that would have produced a reply so an operator can see the
/// gap in production logs.* ⇒ "没接线"必须是**响亮**的，不是静默的。
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopOutcomeReplier;

impl NoopOutcomeReplier {
    /// 构造。
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// 哪些判决本来会产出回复（上游 switch 的那七支；`Ingested` 无 issue 与 `Dropped` 不在内）。
    #[must_use]
    pub fn would_reply(outcome: Outcome) -> bool {
        matches!(
            outcome,
            Outcome::NeedsBinding
                | Outcome::AgentOffline
                | Outcome::AgentArchived
                | Outcome::FreshPending
                | Outcome::ChatStarted
                | Outcome::IssueUsage
        )
    }
}

impl OutboundReplier for NoopOutcomeReplier {
    fn reply(
        &self,
        installation: &ResolvedInstallation,
        message: &InboundMessage,
        result: &RouteResult,
    ) {
        let outcome = Outcome::from_engine(&result.outcome);
        if !Self::would_reply(outcome) {
            return;
        }
        tracing::warn!(
            outcome = ?outcome,
            installation_id = %installation.id,
            chat_id = message.source.chat_id,
            sender = message.source.sender_id,
            "lark outcome replier: outbound reply skipped (replier not wired)"
        );
    }
}

// =====================================================================
// 生产回复器（上游 `LarkOutcomeReplier`）
// =====================================================================

/// engine 的判决 → Lark 侧的卡片 / 文本（上游 `LarkOutcomeReplier`）。
pub struct LarkOutcomeReplier {
    client: Arc<dyn ApiClient>,
    binding: Arc<dyn BindingTokenMinter>,
    decrypt: Decrypter,
    queries: Arc<dyn OutcomeReplierQueries>,
    app_url: String,
    binding_path: String,
    notice_header: String,
}

impl fmt::Debug for LarkOutcomeReplier {
    /// 手写：端口只报存在性，解密器自带脱敏 `Debug`，**没有任何令牌字段**。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LarkOutcomeReplier")
            .field("client", &"<dyn ApiClient>")
            .field("binding", &"<dyn BindingTokenMinter>")
            .field("decrypt", &self.decrypt)
            .field("queries", &"<dyn OutcomeReplierQueries>")
            .field("app_url", &self.app_url)
            .field("binding_path", &self.binding_path)
            .field("notice_header", &self.notice_header)
            .finish()
    }
}

impl LarkOutcomeReplier {
    /// 装配（**不**做上游的降级判据 —— 那在 [`build`] 里；这里要的就是"给我一个能用的"）。
    #[must_use]
    pub fn new(
        client: Arc<dyn ApiClient>,
        binding: Arc<dyn BindingTokenMinter>,
        decrypt: Decrypter,
        queries: Arc<dyn OutcomeReplierQueries>,
        app_url: impl Into<String>,
        binding_path: &str,
    ) -> Self {
        Self {
            client,
            binding,
            decrypt,
            queries,
            app_url: app_url.into().trim_end_matches('/').to_string(),
            binding_path: normalize_binding_path(binding_path),
            notice_header: DEFAULT_NOTICE_HEADER.to_string(),
        }
    }

    /// 换状态卡头部（上游 `noticeHeader` 是构造期常量；本仓给它一个显式 setter，便于诊断）。
    #[must_use]
    pub fn with_notice_header(mut self, header: impl Into<String>) -> Self {
        self.notice_header = header.into();
        self
    }

    /// 上游 `Reply` 的实体（见模块文档差异 D12）。
    ///
    /// 错误**只告警不返回**（模块文档第 4 条）⇒ 这是 async 的"完整路径"，但**不**向外抛。
    pub async fn reply_now(
        &self,
        installation: &LarkInstallation,
        message: &InboundMessage,
        result: &DispatchResult,
    ) -> DeliveryOutcome {
        match result.outcome {
            Outcome::Dropped => DeliveryOutcome::Skipped(SkipReason::NoDeliveryRow),
            Outcome::NeedsBinding => match self
                .send_binding_prompt(installation, message, result)
                .await
            {
                Ok(outcome) => outcome,
                Err(error) => {
                    Self::warn("binding prompt failed", installation, message, &error);
                    DeliveryOutcome::Skipped(SkipReason::NoDeliveryRow)
                }
            },
            Outcome::AgentOffline => self.notice(installation, message, AGENT_OFFLINE_COPY).await,
            Outcome::AgentArchived => {
                self.notice(installation, message, AGENT_ARCHIVED_COPY)
                    .await
            }
            Outcome::FreshPending => self.notice(installation, message, FRESH_PENDING_COPY).await,
            Outcome::ChatStarted => self.notice(installation, message, CHAT_STARTED_COPY).await,
            Outcome::IssueUsage => {
                let copy = if result.issue_usage_had_media {
                    ISSUE_USAGE_WITH_MEDIA_COPY
                } else {
                    ISSUE_USAGE_COPY
                };
                self.notice(installation, message, copy).await
            }
            // agent 自己的聊天回复走 patcher。只有 `/issue` 命令会拿到一个立即可见的产品结果。
            // 按 `issue_id` 把门，于是普通聊天消息在这里**保持沉默**。
            Outcome::Ingested => match result.issue_id {
                None => DeliveryOutcome::Skipped(SkipReason::EmptyContent),
                Some(_) => match self.send_issue_outcome(installation, message, result).await {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        Self::warn("issue outcome reply failed", installation, message, &error);
                        DeliveryOutcome::Skipped(SkipReason::NoDeliveryRow)
                    }
                },
            },
        }
    }

    /// 一条 warn（**只插值判决与 id**，不插值正文 / 令牌）。
    fn warn(
        what: &'static str,
        installation: &LarkInstallation,
        message: &InboundMessage,
        error: &dyn fmt::Display,
    ) {
        tracing::warn!(
            installation_id = %installation.id,
            chat_id = message.source.chat_id,
            "lark outcome replier: {what}: {error}"
        );
    }

    /// 解本安装的明文凭据（上游 `installationCredentials`）。
    fn credentials(
        &self,
        installation: &LarkInstallation,
    ) -> Result<super::params::InstallationCredentials, EngineError> {
        installation_credentials_for(installation, &self.decrypt)
            .map_err(|error| super::outbound::credentials_error(&error))
    }

    /// 发一条状态卡（上游 `sendChatNotice`）。
    async fn notice(
        &self,
        installation: &LarkInstallation,
        message: &InboundMessage,
        body: &str,
    ) -> DeliveryOutcome {
        match self.send_notice(installation, message, body).await {
            Ok(outcome) => outcome,
            Err(error) => {
                Self::warn("notice card failed", installation, message, &error);
                DeliveryOutcome::Skipped(SkipReason::NoDeliveryRow)
            }
        }
    }

    /// 发一条状态卡（上游 `sendChatNotice` 的实体）。
    async fn send_notice(
        &self,
        installation: &LarkInstallation,
        message: &InboundMessage,
        body: &str,
    ) -> Result<DeliveryOutcome, ApiFailure> {
        if message.source.chat_id.is_empty() {
            return Err(ApiFailure::Missing("missing chat_id"));
        }
        let credentials = self.credentials(installation).map_err(ApiFailure::Engine)?;
        // 头部优先用 agent 名字；查询失败 / 空名 ⇒ 默认头部（**不**让回复失败）。
        let header = match self.queries.agent_name(installation.agent_id).await {
            Ok(Some(name)) if !name.is_empty() => name,
            _ => self.notice_header.clone(),
        };
        let card_json = render_notice_card(&header, body);
        let params = SendCardParams {
            credentials,
            chat_id: ChatId::new(message.source.chat_id.clone()),
            card_json,
            reply_target: inbound_reply_target(message),
        };
        let target = params.reply_target.clone();
        let client = Arc::clone(&self.client);
        let message_id = send_with_reply_fallback("send notice card", target, |reply_target| {
            let client = Arc::clone(&client);
            let mut params = params.clone();
            params.reply_target = reply_target;
            async move { client.send_interactive_card(params).await }
        })
        .await?;
        Ok(DeliveryOutcome::MarkdownCard {
            message_id,
            mentioned: false,
        })
    }

    /// 绑定提示（上游 `sendBindingPrompt`）。
    ///
    /// 三条前置各自给出**明确**的失败原因（上游用三条 `errors.New`）：没有 sender `open_id` /
    /// 没配 app url / 没有令牌服务。群聊里私聊不可达（[`CODE_NO_AVAILABILITY`]）⇒ 回落一条
    /// **会话内**文案（上游逐字：*send binding prompt fallback failed after private prompt
    /// unavailable*）。
    async fn send_binding_prompt(
        &self,
        installation: &LarkInstallation,
        message: &InboundMessage,
        result: &DispatchResult,
    ) -> Result<DeliveryOutcome, ApiFailure> {
        if result.sender_open_id.is_empty() {
            return Err(ApiFailure::Missing("missing sender open_id"));
        }
        if self.app_url.is_empty() {
            return Err(ApiFailure::Missing("app_url not configured"));
        }
        let token = self
            .binding
            .mint(
                installation.workspace_id,
                installation.id,
                &result.sender_open_id,
            )
            .await
            .map_err(ApiFailure::Mint)?;
        let bind_url = format!(
            "{}{}?token={}",
            self.app_url,
            self.binding_path,
            url_encode(&token.raw)
        );
        let credentials = self.credentials(installation).map_err(ApiFailure::Engine)?;
        let outcome = self
            .client
            .send_binding_prompt_card(BindingPromptParams {
                credentials: credentials.clone(),
                open_id: OpenId::new(result.sender_open_id.clone()),
                bind_url,
            })
            .await;
        match outcome {
            Ok(()) => Ok(DeliveryOutcome::Text {
                message_id: String::new(),
                mentioned: false,
            }),
            Err(error) => {
                if message.source.chat_type == ChatType::Group
                    && is_binding_prompt_unavailable(&error)
                {
                    // 私聊不可达 ⇒ 回落会话内文案（**不**是错误）。
                    return self
                        .send_notice(installation, message, BINDING_PROMPT_UNAVAILABLE_COPY)
                        .await;
                }
                Err(ApiFailure::Api(error))
            }
        }
    }

    /// `/issue` 的产品结果（上游 `sendIssueOutcome`）：新建确认或活跃重复冲突。
    async fn send_issue_outcome(
        &self,
        installation: &LarkInstallation,
        message: &InboundMessage,
        result: &DispatchResult,
    ) -> Result<DeliveryOutcome, ApiFailure> {
        if message.source.chat_id.is_empty() {
            return Err(ApiFailure::Missing("missing chat_id"));
        }
        let credentials = self.credentials(installation).map_err(ApiFailure::Engine)?;
        let text = if result.issue_duplicate {
            issue_duplicate_text(result, &self.app_url)
        } else {
            issue_created_text(result, &self.app_url)
        };
        let params = SendTextParams {
            credentials,
            chat_id: ChatId::new(message.source.chat_id.clone()),
            text,
            reply_target: inbound_reply_target(message),
        };
        let target = params.reply_target.clone();
        let client = Arc::clone(&self.client);
        let message_id =
            send_with_reply_fallback("send issue outcome text", target, |reply_target| {
                let client = Arc::clone(&client);
                let mut params = params.clone();
                params.reply_target = reply_target;
                async move { client.send_text_message(params).await }
            })
            .await?;
        Ok(DeliveryOutcome::Text {
            message_id,
            mentioned: false,
        })
    }
}

/// 回复器内部的失败（三类来源，**都不含**凭据 / 正文）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApiFailure {
    /// 入参不全（上游那批 `errors.New("missing chat_id")`）。
    #[error("lark: {0}")]
    Missing(&'static str),
    /// 铸令牌失败（原因串来自服务，**不含**令牌）。
    #[error("lark: mint binding token: {0}")]
    Mint(String),
    /// 平台调用失败（会话层回落已经跑过）。
    #[error("lark: {0}")]
    Api(#[from] ApiError),
    /// 凭据面失败。
    #[error("lark: {0}")]
    Engine(EngineError),
}

impl ApiFailure {
    /// 归类成一个 engine 的错误（供 warn 用）。
    #[must_use]
    pub fn into_engine(self) -> EngineError {
        match self {
            Self::Engine(error) => error,
            other => EngineError::infra(other.to_string()),
        }
    }
}

impl From<FallbackError> for ApiFailure {
    fn from(error: FallbackError) -> Self {
        match error {
            FallbackError::Send { original, .. } => Self::Api(original),
        }
    }
}

/// 私聊不可达 ⇒ 群聊里回落会话内文案（上游 `isBindingPromptUnavailable`）。
#[must_use]
pub fn is_binding_prompt_unavailable(error: &ApiError) -> bool {
    error.code() == Some(CODE_NO_AVAILABILITY) && error.class() != ErrorClass::NotConfigured
}

pub mod text;

pub use text::{
    inbound_reply_target, inbound_reply_target_of_binding, issue_created_text,
    issue_duplicate_text, issue_result_identifier, issue_title_sanitized, url_encode,
};

impl OutboundReplier for LarkOutcomeReplier {
    /// 同步接缝：推一个脱离任务后立刻返回（engine 的调用点绝不阻塞在 Lark HTTP 上）。
    ///
    /// `ResolvedInstallation` 里没有本 adapter 的安装投影（纯出站路径 / 用例）⇒ 打一条 warn
    /// 后返回，**不**猜（与 `slack::replier` 同款）。
    fn reply(
        &self,
        installation: &ResolvedInstallation,
        message: &InboundMessage,
        result: &RouteResult,
    ) {
        let Some(platform) = super::resolvers::platform_installation(installation) else {
            tracing::warn!(
                installation_id = %installation.id,
                kind = %TYPE_LARK,
                "lark outcome replier: installation platform row unavailable"
            );
            return;
        };
        let platform = platform.clone();
        let message = message.clone();
        let dispatch = dispatch_result_from_engine(result);
        let handle = self.handle();
        spawn_detached(async move {
            handle.reply_now(&platform, &message, &dispatch).await;
        });
    }
}

impl LarkOutcomeReplier {
    /// 可 `'static` 的句柄（脱离任务要它）。克隆的是 `Arc` 与解密器，
    /// **不**复制任何令牌 / 明文。
    #[must_use]
    fn handle(&self) -> Arc<Self> {
        Arc::new(Self {
            client: Arc::clone(&self.client),
            binding: Arc::clone(&self.binding),
            decrypt: self.decrypt.clone(),
            queries: Arc::clone(&self.queries),
            app_url: self.app_url.clone(),
            binding_path: self.binding_path.clone(),
            notice_header: self.notice_header.clone(),
        })
    }
}

/// 脱离式执行（模块文档差异 D12）：有运行时 ⇒ 起任务；没有 ⇒ 打一条 warn。
fn spawn_detached<F>(future: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::spawn(future);
    } else {
        tracing::warn!("lark outcome replier: no async runtime; skipping the detached reply");
    }
}

/// 把 `Arc<LarkOutcomeReplier>` 当端口用。
///
/// 标准库的 blanket impl 已经提供 `Arc<T>: Trait`（`T: Trait`），这里只是给接线方一个
/// **读得出来**的出口。
#[must_use]
pub fn replier(replier: Arc<LarkOutcomeReplier>) -> Arc<dyn OutboundReplier> {
    replier
}

#[cfg(test)]
mod tests;
