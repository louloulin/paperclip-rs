//! lark 的**富上下文装配器**：把用户**显式附上**的上下文（引用回复 / 合并转发）
//! 以及**群近况**内联进正文（上游 `internal/integrations/lark/inbound_enricher.go` 737 行）。
//!
//! - **写者**：M7-12（`docs/60-M7-PLAN.md` §3.3；写集勘误见 `docs/32` §29）。
//! - **契约（上游注释逐字）**：**尽力而为** —— 每一次取回失败都降级成一行可读的注记或占位，
//!   [`Enricher::enrich`] **永不**返回错误、也**永不**阻塞摄取。没有任何可展开内容
//!   （没有 `parent_id`、不是 `merge_forward`）的消息**原样**返回，一次网络调用都不发。
//! - **位置**：上游在 `ws_connector.go` 的接收循环里调它（帧 ACK 之前，`EnrichTimeout ≈ 2s`）；
//!   本仓由 [`super::feishu_channel::LarkEventEmitter`] 调它 —— 同样在 ACK 之前
//!   （见 `feishu_channel.rs` 模块文档的"归一化三步"表）。
//!
//! # 装配顺序：宽 → 窄
//!
//! ```text
//! <recent_context …>…</recent_context>
//!
//! <quoted_message …>…</quoted_message>
//!
//! <[发言人名]: 用户自己的消息，或者转发出来的记录>
//! ```
//!
//! `<recent_context>` 只对**群聊里明确 @ 了 bot 的消息**生成，且只在
//! [`InboundEnricherConfig::recent_context_size`] > 0 时 —— 它是这里**唯一**不由用户显式附上的
//! 取回。当 `@` 发生在一个 Lark **话题**（`thread_id` 非空）里时，窗口收窄到那个话题，
//! 于是一个话题的上下文**永远不会**包含兄弟话题的消息（上游 #5835，见 [`InboundEnricher::fetch_recent_items`]）。
//!
//! # 发言人标签（群聊专用）
//!
//! 群聊里**所有**块（近况 + 引用 + 转发）的发言人，加上 `@` 了 bot 的那个发件人，用**一次**
//! Contact 批量调用解析成真实显示名 ⇒ agent 读到 `[Alice]: …` 而不是 `[User 1]: …`。
//! 这解释了为什么引用 / 转发的消息要**先**取回（阶段一）再解析名字（阶段二）。
//! 解析不出的发言人退回位置标签 `User N`；p2p 保留位置标签（1:1 里身份没有歧义）。
//!
//! # 与上游的三处**形态 / 语义**差异（登记 `docs/32` §29 的 D 项）
//!
//! 1. **取回预算用注入的时钟算，不用 `ctx`**：上游两次尝试复用**同一个** `ctx`
//!    （`ws_connector` 把它整个包在 `EnrichTimeout` 里），所以"预算耗尽 ⇒ 不重试"是
//!    `ctx.Err()` 判的。本仓的入口是同步签名的上游移植里没有 `ctx` 的位置 ⇒
//!    [`InboundEnricherConfig::budget`] + [`Clock`] 显式表达同一件事，且**可注入**
//!    （用例拿 [`ManualClock`] 推进时间即可，不必睡真觉）。单次取回仍用
//!    `tokio::time::timeout` 兜住"最坏一次调用"。
//! 2. **错误分类读 `ApiError` 的结构字段，不读错误文本**：上游 `classifyRecentContextFetchError`
//!    对 `err.Error()` 做子串匹配（`"code=230110"` / `"http 403"` / …）。本仓的
//!    [`super::client::ApiError`] 变体**结构上**就带平台码与 HTTP 状态码 ⇒
//!    [`classify`] 直接读它们（同一张分类表，且不会因为文案改动而漂移）。
//! 3. **`merge_forward` 的子消息上限与 p2p 标签**逐字照搬（没有差异）；列出来只是为了让
//!    下面两条的行号对得上。
//!
//! # 凭据面（`docs/60` §2.3）
//!
//! 本文件**零** `tracing::*` 插值凭据：只插 `message_id` / `chat_id` / `category` /
//! `attempts` / `err`（`ApiError` 自带脱敏 `Display`，不含平台 `msg` / URL / 请求体）。
//! 明文 `app_secret` 只经 [`InstallationCredentials`] 传给 [`ApiClient`]，**不**进任何日志。

pub mod classify;
pub mod render;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mc_core::channel::message::ChatType;

use super::client::ApiClient;
use super::feishu_channel::LarkInboundMessage;
use super::params::{InstallationCredentials, ListMessagesParams};
use super::types::LarkMessage;
use crate::engine::commands::{parse_control_command, ControlCommandKind};

use classify::{
    classify_api_error, recent_context_unavailable_line, RecentContextFetchClassification,
};
use render::{
    forwarded_error_block, render_forwarded_items, render_quoted_block,
    render_recent_context_block, sender_open_ids,
};

// =====================================================================
// 词表与默认值（上游同名常量）
// =====================================================================

/// 一次转发里最多内联多少条子消息（上游 `defaultMaxForwardChildren`）。
///
/// Lark 自己把 `merge_forward` 限在 100 条，我们镜像它当安全阀，免得一份病态的转发包
/// 撑爆 agent 的上下文。超出上限的部分被丢掉，并留一个可见的 `... (N more truncated)` 标记。
pub const DEFAULT_MAX_FORWARD_CHILDREN: usize = 100;

/// 群近况预取的默认窗口（上游 `DefaultRecentContextSize`）。
///
/// 它是**取回预算**，不是"保证渲染这么多行"：触发消息自己与它引用的父消息都会从结果里
/// 滤掉，所以 `<recent_context>` 通常少渲染一两行。10 让 agent 的提示词有意义的上下文，
/// 又不至于膨胀、也不至于压到入站 ACK 预算（一次 list 调用，`page_size` 10）。
pub const DEFAULT_RECENT_CONTEXT_SIZE: usize = 10;

/// 单次富化的时间预算（上游 `ws_connector.go` 的 `EnrichTimeout` 默认值 2s）。
pub const DEFAULT_ENRICH_BUDGET: Duration = Duration::from_secs(2);

/// 近况预取最多尝试几次（上游 `recentContextMaxFetchAttempts`）。
pub const RECENT_CONTEXT_MAX_FETCH_ATTEMPTS: usize = 2;

/// 近况预取的端点名（上游 `recentContextEndpoint`，日志字段用）。
pub const RECENT_CONTEXT_ENDPOINT: &str = "im/v1/messages.list";

// =====================================================================
// 时钟（注入点）
// =====================================================================

/// 单调毫秒时钟（上游用 `context.Context` 的 deadline 表达同一件事，见模块文档差异 1）。
///
/// 只要求**单调**（不要求与墙上时间有关）：装配器只用它算"预算还剩多少"。
pub trait Clock: Send + Sync {
    /// 自某个固定原点起的毫秒数（单调不减）。
    fn now_millis(&self) -> u64;
}

/// 生产时钟：进程起点起的毫秒（`std::time::Instant` 是单调的，但没法凭空构造
/// ⇒ 用一个进程级的原点把它换算成可比较的整数）。
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

/// 进程起点（第一次用到 [`SystemClock`] 时捕获）。
fn clock_origin() -> std::time::Instant {
    static ORIGIN: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    *ORIGIN.get_or_init(std::time::Instant::now)
}

impl Clock for SystemClock {
    fn now_millis(&self) -> u64 {
        u64::try_from(clock_origin().elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

/// 用例时钟：可手动推进（`AtomicU64`）。
///
/// 存在的理由是模块文档差异 1：**不睡真觉**就能钉住"预算耗尽 ⇒ 不重试"这条判决。
#[derive(Debug, Default, Clone)]
pub struct ManualClock(Arc<std::sync::atomic::AtomicU64>);

impl ManualClock {
    /// 从一个起点造（通常 `0`）。
    #[must_use]
    pub fn new(start_millis: u64) -> Self {
        Self(Arc::new(std::sync::atomic::AtomicU64::new(start_millis)))
    }

    /// 前进 `ms` 毫秒。
    pub fn advance(&self, ms: u64) {
        self.0.fetch_add(ms, std::sync::atomic::Ordering::SeqCst);
    }

    /// 直接置位。
    pub fn set(&self, millis: u64) {
        self.0.store(millis, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_millis(&self) -> u64 {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}

// =====================================================================
// 装配器
// =====================================================================

/// 一次富化的消费者（上游 `Enricher` 接口）。
///
/// **契约**：永不返回错误、永不阻塞摄取；取回失败降级成可读注记。
#[async_trait]
pub trait Enricher: Send + Sync {
    /// 把 `message` 的正文改写成"带着显式上下文"的形态；**不可失败**。
    async fn enrich(
        &self,
        message: LarkInboundMessage,
        credentials: &InstallationCredentials,
    ) -> LarkInboundMessage;
}

/// [`InboundEnricher`] 的时间与规模旋钮（上游 `InboundEnricherConfig`）。
///
/// 全部字段有默认值；`clock` 是唯一**必须**注入的（见 [`InboundEnricherConfig::new`]）。
#[derive(Clone)]
pub struct InboundEnricherConfig {
    /// 最多内联多少条转发子消息；`0` ⇒ [`DEFAULT_MAX_FORWARD_CHILDREN`]。
    pub max_forward_children: usize,
    /// 群近况窗口；`0` ⇒ **整个关掉**预取（只用显式附上的引用 / 转发上下文）。
    /// 生产装配用 [`DEFAULT_RECENT_CONTEXT_SIZE`]。
    pub recent_context_size: usize,
    /// 单次富化的时间预算（见模块文档差异 1）。
    pub budget: Duration,
    /// 单调时钟（注入点）。
    pub clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for InboundEnricherConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InboundEnricherConfig")
            .field("max_forward_children", &self.max_forward_children)
            .field("recent_context_size", &self.recent_context_size)
            .field("budget", &self.budget)
            .field("clock", &"<dyn Clock>")
            .finish()
    }
}

impl InboundEnricherConfig {
    /// 生产默认 + 注入时钟。
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            max_forward_children: DEFAULT_MAX_FORWARD_CHILDREN,
            recent_context_size: DEFAULT_RECENT_CONTEXT_SIZE,
            budget: DEFAULT_ENRICH_BUDGET,
            clock,
        }
    }

    /// 换时间预算。
    #[must_use]
    pub fn with_budget(mut self, budget: Duration) -> Self {
        self.budget = budget;
        self
    }

    /// 换近况窗口（`0` = 关掉预取）。
    #[must_use]
    pub fn with_recent_context_size(mut self, size: usize) -> Self {
        self.recent_context_size = size;
        self
    }

    /// 换转发子消息上限。
    #[must_use]
    pub fn with_max_forward_children(mut self, limit: usize) -> Self {
        self.max_forward_children = limit;
        self
    }
}

impl Default for InboundEnricherConfig {
    /// 生产默认（[`SystemClock`]）。
    fn default() -> Self {
        Self::new(Arc::new(SystemClock))
    }
}

/// 上游 `inboundEnricher` 的移植。
pub struct InboundEnricher {
    client: Arc<dyn ApiClient>,
    max_forward_children: usize,
    recent_context_size: usize,
    budget: Duration,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for InboundEnricher {
    /// 端口是 trait 对象 ⇒ 只列存在性与旋钮。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InboundEnricher")
            .field("client", &"<dyn ApiClient>")
            .field("max_forward_children", &self.max_forward_children)
            .field("recent_context_size", &self.recent_context_size)
            .field("budget", &self.budget)
            .finish_non_exhaustive()
    }
}

impl InboundEnricher {
    /// 装配（上游 `NewInboundEnricher`）：`max_forward_children == 0` 取默认值，
    /// `recent_context_size == 0` **保持 0**（= 关掉近况预取，那是显式的选择）。
    #[must_use]
    pub fn new(client: Arc<dyn ApiClient>, mut config: InboundEnricherConfig) -> Self {
        if config.max_forward_children == 0 {
            config.max_forward_children = DEFAULT_MAX_FORWARD_CHILDREN;
        }
        Self {
            client,
            max_forward_children: config.max_forward_children,
            recent_context_size: config.recent_context_size,
            budget: config.budget,
            clock: config.clock,
        }
    }
}

/// 阶段一所取回的三组消息 + 各自的失败（上游 `Enrich` 里的那六个局部变量）。
#[derive(Default)]
struct FetchedContext {
    recent_items: Vec<LarkMessage>,
    recent_error: Option<super::client::ApiError>,
    quoted_items: Vec<LarkMessage>,
    quoted_error: Option<super::client::ApiError>,
    forward_items: Vec<LarkMessage>,
    forward_error: Option<super::client::ApiError>,
}

#[async_trait]
impl Enricher for InboundEnricher {
    /// 上游 `Enrich`：见模块文档的装配顺序。**不可失败**。
    ///
    /// `too_many_lines`：这**就是**上游那一条流水线（命令剥离 → 判要不要富化 → 阶段一取回
    /// → 阶段二查名 → 阶段三渲染），顺序本身是语义，拆函数只会把"三个阶段的依赖关系"
    /// 藏到调用图里（与 `engine/router.rs` 的同一判断）。
    #[allow(clippy::too_many_lines)]
    async fn enrich(
        &self,
        mut message: LarkInboundMessage,
        credentials: &InstallationCredentials,
    ) -> LarkInboundMessage {
        // 命令判定读用户**自己**打的字（`command_body`），不读已经被前一轮富化过的正文。
        let fresh_source = if message.command_body.is_empty() {
            message.body.clone()
        } else {
            message.command_body.clone()
        };
        let mut start_chat = false;
        if let Some(control) = parse_control_command(&fresh_source) {
            message.body = control.body;
            match control.kind {
                ControlCommandKind::FreshSession => message.force_fresh_session = true,
                ControlCommandKind::NewChat => {
                    // 一条新 Chat **不得**继承上一条路由遗留的组装近况。显式附上的引用 /
                    // 转发仍要展开：那是用户**本回合**附着在这个命令上的。
                    start_chat = true;
                }
            }
        }

        let is_forward = message.is_merge_forward();
        let want_recent = !start_chat
            && self.recent_context_size > 0
            && message.chat_type == ChatType::Group
            && message.addressed_to_bot;
        if message.parent_id.is_empty() && !is_forward && !want_recent {
            // 没有可展开的东西、也不要群近况 ⇒ 一次网络调用都不发。
            return message;
        }
        // 传输层没接线（无 lark 应用的部署上的替身客户端）⇒ 直接跳过，而不是给每条回复
        // 盖一个取回失败的戳。正文保持解码器产出的样子。
        if !self.client.is_configured() {
            return message;
        }

        let deadline = self
            .clock
            .now_millis()
            .saturating_add(as_millis(self.budget));
        let mut fetched = FetchedContext::default();

        // ---- 阶段一：把可能要渲染的每一组消息取回来（各自尽力而为）----
        //
        // 一起取（而不是"取一组渲染一组"）是为了让阶段二能用**一次** Contact 批量调用
        // 解析**所有**块的发言人 —— 否则一个不在近况窗口里的引用 / 转发发言人会退化成 "User N"。
        if want_recent {
            match self
                .fetch_recent_items(credentials, &message, deadline)
                .await
            {
                Ok(items) => fetched.recent_items = items,
                Err(error) => fetched.recent_error = Some(error),
            }
        }
        if !message.parent_id.is_empty() {
            match self
                .client
                .get_message(credentials.clone(), &message.parent_id)
                .await
            {
                Ok(items) => fetched.quoted_items = items,
                Err(error) => fetched.quoted_error = Some(error),
            }
        }
        if is_forward {
            match self
                .client
                .get_message(credentials.clone(), &message.message_id)
                .await
            {
                Ok(items) => fetched.forward_items = items,
                Err(error) => fetched.forward_error = Some(error),
            }
        }

        // ---- 阶段二：一次批量解析所有发言人的显示名（只有群聊）----
        let names = if message.chat_type == ChatType::Group {
            let mut ids = sender_open_ids(&fetched.recent_items);
            ids.extend(sender_open_ids(&fetched.quoted_items));
            ids.extend(sender_open_ids(&fetched.forward_items));
            if !message.sender_open_id.is_empty() {
                ids.push(message.sender_open_id.as_str().to_string());
            }
            self.resolve_names(credentials, ids, deadline).await
        } else {
            None
        };

        // ---- 阶段三：按"宽 → 窄"渲染 ----
        let mut out = String::new();
        if want_recent {
            if let Some(error) = &fetched.recent_error {
                out.push_str(&recent_context_unavailable_line(
                    classify_api_error(error).category,
                ));
            } else if !fetched.recent_items.is_empty() {
                out.push_str(&render_recent_context_block(
                    &fetched.recent_items,
                    names.as_ref(),
                ));
            }
        }
        if !message.parent_id.is_empty() {
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str(&render_quoted_block(
                &message.parent_id,
                &fetched.quoted_items,
                fetched.quoted_error.as_ref(),
                names.as_ref(),
                self.max_forward_children,
            ));
            message.has_selected_context = true;
        }

        let core = if is_forward {
            message.has_selected_context = true;
            match &fetched.forward_error {
                Some(error) => {
                    tracing::warn!(
                        message_id = message.message_id,
                        category = classify_api_error(error).category,
                        "lark enricher: forward fetch failed"
                    );
                    forwarded_error_block()
                }
                None => render_forwarded_items(
                    &fetched.forward_items,
                    &message.message_id,
                    names.as_ref(),
                    self.max_forward_children,
                ),
            }
        } else {
            // 给用户自己的消息打上真实名字的标签，让 agent 知道**谁** @ 了它。
            // 只有解析出名字时才加（群聊路径）；否则正文原样透传。
            names
                .as_ref()
                .and_then(|names| names.get(message.sender_open_id.as_str()))
                .map_or_else(
                    || message.body.clone(),
                    |name| format!("[{}]: {}", name, message.body),
                )
        };
        if !out.is_empty() && !core.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&core);
        message.body = out;
        message
    }
}

impl InboundEnricher {
    /// 剩余预算（毫秒）；`0` = 已经用光。
    fn remaining_millis(&self, deadline: u64) -> u64 {
        deadline.saturating_sub(self.clock.now_millis())
    }

    /// 取回近况窗口，返回**要渲染**的那些消息（上游 `fetchRecentItems`）。
    ///
    /// 触发消息自己与它直接引用的父消息被滤掉；结果按**最旧在前**排序。
    /// 取回失败上抛给调用方（它渲染一行安全的降级注记）—— 永不阻塞摄取。
    ///
    /// 触发消息在 Lark 话题里时（`thread_id` 非空），窗口收窄到那个话题
    /// （`container_id_type=thread`），于是共享同一个 `chat_id` 的兄弟话题**不会**泄进
    /// 本话题的上下文或它持久化的那一轮（上游 #5835）。因为 thread 容器拒绝 `end_time`，
    /// 话题路径**在客户端侧**锚定到触发时刻；它同时对 `thread_id` **失败关闭** ——
    /// 任何 `thread_id` 缺失或不匹配的返回项都被丢掉，而不是被信任。话题取回失败与
    /// 会话路径一样降级，且**永不**回落到会话级取回（那会把泄露重新打开）。
    /// 话题之外会话路径不变：用 `end_time` 锚到触发时刻。
    async fn fetch_recent_items(
        &self,
        credentials: &InstallationCredentials,
        message: &LarkInboundMessage,
        deadline: u64,
    ) -> Result<Vec<LarkMessage>, super::client::ApiError> {
        if message.chat_id.is_empty() {
            return Err(super::client::ApiError::InvalidRequest {
                op: "list_chat_messages",
                reason: "missing chat_id for recent context",
            });
        }

        // Lark 把 create_time 发成纪元毫秒；缺失 / 解不开得 0。会话路径把它换算成秒给
        // `end_time`；话题路径用原始毫秒做客户端侧锚点。
        let trigger_millis = parse_lark_millis(&message.create_time);
        let mut params = ListMessagesParams {
            chat_id: message.chat_id.clone(),
            page_size: self.recent_context_size,
            ..ListMessagesParams::default()
        };
        if message.thread_id.is_empty() {
            // 0 告诉客户端"没有 end_time"（取最新 N 条）。
            params.end_time = trigger_millis / 1000;
        } else {
            params.thread_id = message.thread_id.clone();
        }

        let mut last_error: Option<super::client::ApiError> = None;
        for attempt in 1..=RECENT_CONTEXT_MAX_FETCH_ATTEMPTS {
            let call = self
                .client
                .list_chat_messages(credentials.clone(), params.clone());
            match timeout_remaining(self.remaining_millis(deadline), call).await {
                Ok(items) => {
                    if attempt > 1 {
                        tracing::info!(
                            layer = "lark_inbound_enricher",
                            endpoint = RECENT_CONTEXT_ENDPOINT,
                            status = "recovered",
                            attempts = attempt,
                            chat_id = message.chat_id.as_str(),
                            message_id = message.message_id,
                            "lark enricher: recent context fetch recovered after retry"
                        );
                    }
                    return Ok(filter_recent_items(items, message, trigger_millis));
                }
                Err(error) => last_error = Some(error),
            }
            let error = last_error.as_ref().expect("失败分支刚刚写过这一项").clone();
            let classified = classify_api_error(&error);
            // 重试只在共享预算还有时间时才有意义。两次尝试复用同一份预算（上游用一个
            // `ctx`）；预算一空，第二次调用会立刻失败 ⇒ 现在降级，别烧掉一次注定失败的请求。
            // 这正是"第一次尝试超时 ⇒ 永不恢复"的原因。
            if !classified.retryable
                || attempt == RECENT_CONTEXT_MAX_FETCH_ATTEMPTS
                || self.remaining_millis(deadline) == 0
            {
                Self::log_recent_context_failure(message, &classified, attempt, &error);
                return Err(error);
            }
            tracing::warn!(
                layer = "lark_inbound_enricher",
                endpoint = RECENT_CONTEXT_ENDPOINT,
                status = "retrying",
                category = classified.category,
                retryable = classified.retryable,
                attempt = attempt,
                next_attempt = attempt + 1,
                chat_id = message.chat_id.as_str(),
                message_id = message.message_id,
                "lark enricher: recent context fetch failed; retrying"
            );
        }
        // 最后一圈必然在上面的 `attempt == RECENT_CONTEXT_MAX_FETCH_ATTEMPTS` 分支返回；
        // 这一行只兜住"常数为 0 ⇒ 循环没跑"的退化形态。
        Err(last_error.unwrap_or(super::client::ApiError::NotConfigured))
    }

    /// 记一条近况取回失败的告警（字段与上游逐条对应）。
    fn log_recent_context_failure(
        message: &LarkInboundMessage,
        classified: &RecentContextFetchClassification,
        attempts: usize,
        error: &super::client::ApiError,
    ) {
        tracing::warn!(
            layer = "lark_inbound_enricher",
            endpoint = RECENT_CONTEXT_ENDPOINT,
            status = "failed",
            category = classified.category,
            retryable = classified.retryable,
            attempts = attempts,
            chat_id = message.chat_id.as_str(),
            message_id = message.message_id,
            err = %error,
            "lark enricher: recent context fetch failed"
        );
    }

    /// 批量把 `open_id` 解析成显示名 —— **尽力而为**：失败（通讯录范围受限、链路错误）
    /// 只记一条告警并返回 `None`，于是每个发言人标签器都退化成位置标签 `User N`，
    /// 而不是阻塞摄取。重复 / 空 id 先丢掉。
    async fn resolve_names(
        &self,
        credentials: &InstallationCredentials,
        ids: Vec<String>,
        deadline: u64,
    ) -> Option<std::collections::HashMap<String, String>> {
        let mut unique = Vec::with_capacity(ids.len());
        let mut seen = std::collections::HashSet::with_capacity(ids.len());
        for id in ids {
            if id.is_empty() || !seen.insert(id.clone()) {
                continue;
            }
            unique.push(id);
        }
        if unique.is_empty() {
            return None;
        }
        let call = self
            .client
            .batch_get_users(credentials.clone(), unique.clone());
        match timeout_remaining(self.remaining_millis(deadline), call).await {
            Ok(names) => Some(names),
            Err(error) => {
                // 一次调用到底是链路失败还是预算耗尽，由分类给出（`enrich_deadline_exceeded`
                // / `enrich_budget_exhausted` 归 `timeout`，其余链路失败归 `temporary`）。
                tracing::warn!(
                    ids = unique.len(),
                    category = classify_api_error(&error).category,
                    "lark enricher: speaker name resolution failed"
                );
                None
            }
        }
    }
}

/// 把 [`Duration`] 换算成毫秒（饱和到 `u64`）。
fn as_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// 在剩余预算内跑一个 future；预算为 0 ⇒ 一个确定性的"超时"错误。
///
/// 刻意**不用** `tokio::time::timeout(0)`（它仍会跑一次轮询）；这里让"预算已空"这件事
/// 立刻、可预测地失败 —— 用例因此不依赖调度器时序。
async fn timeout_remaining<F, T>(
    remaining_millis: u64,
    future: F,
) -> Result<T, super::client::ApiError>
where
    F: std::future::Future<Output = Result<T, super::client::ApiError>>,
{
    if remaining_millis == 0 {
        return Err(super::client::ApiError::Transport {
            op: "enrich_budget_exhausted",
        });
    }
    match tokio::time::timeout(Duration::from_millis(remaining_millis), future).await {
        Ok(result) => result,
        Err(_) => Err(super::client::ApiError::Transport {
            op: "enrich_deadline_exceeded",
        }),
    }
}

/// 近况窗口的**本地**过滤（上游 `fetchRecentItems` 的循环体）。
///
/// 失败关闭的话题隔离与客户端侧锚点都在这里；见 [`InboundEnricher::fetch_recent_items`]。
fn filter_recent_items(
    items: Vec<LarkMessage>,
    message: &LarkInboundMessage,
    trigger_millis: i64,
) -> Vec<LarkMessage> {
    let mut exclude = std::collections::HashSet::new();
    exclude.insert(message.message_id.clone());
    if !message.parent_id.is_empty() {
        exclude.insert(message.parent_id.clone());
    }
    let in_thread = !message.thread_id.is_empty();
    let mut kept: Vec<LarkMessage> = Vec::with_capacity(items.len());
    for item in items {
        if exclude.contains(&item.message_id) {
            continue;
        }
        // Bot 的 markdown 回复是以 schema-2.0 互动卡发出的，摊平后只剩零信号的
        // "[interactive card]" 占位 ⇒ 丢掉它们而不是渲染噪音（#5835）。
        if item.sender_type == "app" && item.message_type == "interactive" {
            continue;
        }
        if in_thread {
            if item.thread_id != message.thread_id {
                continue;
            }
            // thread 容器忽略 end_time ⇒ 在客户端侧锚定：丢掉严格晚于 @ 时刻的那些。
            // 触发时间为 0（解不开）⇒ 锚点失效（上游同）。
            if trigger_millis > 0 && parse_lark_millis(&item.create_time) > trigger_millis {
                continue;
            }
        }
        kept.push(item);
    }
    // list 端点按最新在前返回；按**最旧在前**渲染，让记录像聊天那样从上往下读。
    kept.sort_by_key(|item| parse_lark_millis(&item.create_time));
    kept
}

/// Lark 的 `create_time`（纪元毫秒的**字串**）→ `i64`（上游 `parseLarkMillis`）。
///
/// 解不开 ⇒ `0`（调用方按"没有锚点"处理）。
#[must_use]
pub fn parse_lark_millis(raw: &str) -> i64 {
    raw.trim().parse::<i64>().unwrap_or(0)
}

#[cfg(test)]
mod tests;
