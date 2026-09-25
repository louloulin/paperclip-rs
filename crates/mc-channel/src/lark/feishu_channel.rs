//! lark 的**入站归一化** + `Channel` 实现 + 注册工厂
//! （上游 `internal/integrations/lark/feishu_channel.go` 328 行 + `feishu_types.go` 115 行）。
//!
//! - **写者**：M7-12（`docs/60-M7-PLAN.md` §3.3；写集勘误见 `docs/32` §29）。
//! - **上游 `feishu_channel.go` 的传输那一半不在本文件**：`Connect` 不再自己跑帧循环，而是
//!   把 M7-11 的 [`EventConnector`] 接上（`docs/32` §28.4 的 **H1**：入站链的接缝是
//!   [`LarkInboundEvent`]）。本文件的 `connect` 只做四件事：解凭据 → 造 emitter →
//!   `run` 一条会话 → 把收尾翻译成 [`Channel`] 的契约。
//! - **本文件是 `docs/32` §28.3 的 D3 的落点**：`app_secret` 的解密需要解密器与安装行，
//!   所以凭据面在 [`credentials`]（本文件的子模块），明文 secret 的生命周期**只**覆盖一次会话。
//!
//! # 归一化三步（H1 交接的落地）
//!
//! | 步骤 | 上游 | 本仓 |
//! | --- | --- | --- |
//! | 正文摊平 | `resolveMentions(flattenContent(...))` | [`super::content_flatten`] |
//! | 提及改写 / `addressed_to_bot` | `containsMention(mentions, …)` | [`super::content_flatten::contains_mention`] |
//! | 富上下文装配 | `Enricher.Enrich`（在 **WS 循环里**、ACK 之前） | [`super::enricher`]（在 **emitter 里**、ACK 之前） |
//!
//! ⚠️ 第三步的**位置**与上游不同但**时机**相同：上游在 `ws_connector.go` 的接收循环里调
//! `Enrich`（帧 ACK 之前，`EnrichTimeout ≈ 2s` 兜底）；本仓由 M7-11 把"解码"与"装配"切开
//! （它只发 [`LarkInboundEvent`]），所以装配落在 [`LarkEventEmitter::emit`] —— 它同样在
//! ACK 之前被 `run_session` 调用，于是"ACK 延迟 = 富化预算"这条上游不变式**没有变**。
//! 差异只在谁持有它：上游是连接器的一个配置字段，本仓是 emitter 的构造依赖。
//!
//! # `raw` 的形状（两条读者的契约）
//!
//! [`mc_core::channel::message::InboundMessage::raw`] 装的是 [`LarkInboundMessage`] 的 JSON：
//! 它既有 M7-11 解出来的全部字段，也有本片派生的三个字段（`body` / `command_body` /
//! `addressed_to_bot`），还有 [`LarkInboundEvent::raw`] 那份**信封逐字**（字段 `envelope`）。
//! ⇒ [`super::resolvers`] 与 [`super::enricher`] 都能从 `raw` 读自己要的东西，而信封**一格不丢**。
//!
//! # 凭据面（`docs/60` §2.3 的四条判据，逐条落在这里）
//!
//! 1. 本文件**不新增**承载凭据的类型：明文 secret 只在
//!    [`super::params::InstallationCredentials`]（`Debug` 已脱敏）里过一手；密文行投影
//!    [`LarkInstallation`] 的 `Debug` 只报长度；
//! 2. `tracing::*` **只**插值 `app_id`、`installation_id`、`event_id`、`message_id`、
//!    `event_type`、`outcome` 与错误**码/类别** —— 从不插值 secret、正文、帧体；
//! 3. 「错误路径不回显凭据」由 [`credentials`] 的用例（坏 base64 / 认证失败 / 非 UTF-8
//!    三条各一）与工厂的拒装配路径钉住；
//! 4. 键名进 redaction 表那一侧在 `mc-telemetry`，本文件不新增日志字段。

pub mod credentials;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use mc_core::channel::message::{InboundMessage, OutboundMessage, SendResult, Source};
use mc_core::channel::ChannelKind;
use mc_core::id::Id;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::client::{ApiClient, ApiError};
use super::content_flatten::{
    contains_mention, flatten_content, mentions_from_event, message_kind, resolve_mentions,
    MSG_TYPE_MERGE_FORWARD,
};
use super::enricher::Enricher;
use super::params::{InstallationCredentials, ReplyTarget, SendTextParams};
use super::resolvers::{LarkInstallation, TYPE_LARK};
use super::types::{ChatId, ChatType, OpenId};
use super::ws_connector::{EventConnector, EventEmitter, SessionOutcome, StopSignal};
use super::ws_frame_decoder::{LarkEventMention, LarkInboundEvent};
use crate::capability::Capability;
use crate::channel::{Channel, ChannelConfig, ChannelError, ChannelResult, Factory};
use crate::message::SharedInboundHandler;

pub use credentials::{
    installation_credentials_for, ConfigError, DecryptError, Decrypter, LarkInstallConfig,
};

// =====================================================================
// 归一化后的入站载荷（上游 lark 包的局部 `InboundMessage`）
// =====================================================================

/// lark 的**已解码 + 已派生**入站事件 —— 本 adapter 的内部形态，也是 `raw` 的内容。
///
/// 字段名与上游逐条对应（便于两边的用例逐字段比对）。前半是 M7-11 的解码产物，
/// 后半（`body` … `has_selected_context`）是本片派生的三个字段加两个语义开关。
///
/// ⚠️ 它是**平台载荷**（含用户正文）⇒ [`Debug`] 只给用例用；接线方的日志只插值
/// `event_type` / `message_id`（本文件的 `tracing::*` 逐条遵守）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LarkInboundMessage {
    /// 事件类型（恒为 `im.message.receive_v1`）。
    pub event_type: String,
    /// 平台投递 id。
    pub event_id: String,
    /// 机器人自身的 `app_id`（安装路由键）。
    pub app_id: String,
    /// 租户键。
    #[serde(default)]
    pub tenant_key: String,
    /// 会话 id。
    pub chat_id: ChatId,
    /// 归一化后的会话类型。
    pub chat_type: ChatType,
    /// 平台消息 id。
    pub message_id: String,
    /// 发送者的按应用 id。
    pub sender_open_id: OpenId,
    /// 发送者的跨应用稳定 id（空 = 平台没给 / 未回填）。
    #[serde(default)]
    pub sender_union_id: String,
    /// 平台原始消息类型。
    pub message_type: String,
    /// 原样的正文（Lark 双重编码的 JSON 字符串）。
    #[serde(default)]
    pub content: String,
    /// 原样的提及数组（**WS 形状**）。
    #[serde(default)]
    pub mentions: Vec<LarkEventMention>,
    /// 平台给的创建时间（纪元毫秒的字串）。
    #[serde(default)]
    pub create_time: String,
    #[serde(default)]
    pub parent_id: String,
    #[serde(default)]
    pub root_id: String,
    #[serde(default)]
    pub thread_id: String,
    /// M7-11 的**信封逐字**（平台加字段不会丢）。
    #[serde(default)]
    pub envelope: serde_json::Value,

    // ---- M7-12 派生（`ws_frame_decoder.rs` 的交接表列的三个，加两个语义开关） ----
    /// 摊平 + 提及改写之后的正文（agent 可读）。
    pub body: String,
    /// 用户**自己**打的字（富化**之前**的 `body`）：`/issue` 从它解析，不从富化后的正文解析。
    pub command_body: String,
    /// 群聊里这条消息是否明确指向 bot（p2p 恒 `false`）。
    pub addressed_to_bot: bool,
    /// `/clear` 一类要求开新会话。
    #[serde(default)]
    pub force_fresh_session: bool,
    /// 正文里是否含**显式选中**的引用 / 转发（自动近况不算）。
    #[serde(default)]
    pub has_selected_context: bool,
}

impl LarkInboundMessage {
    /// 从 M7-11 的解码产物派生一条完整入站载荷（上游 `decodeInbound` 的后半）。
    ///
    /// 摊平 + 提及改写**同步**做（无外部调用 —— 上游注释逐字：解码器必须快且无依赖）；
    /// `merge_forward` 的 `body` 留空（展开它要一次 HTTP 往返，归 [`super::enricher`]）。
    /// `command_body` 在任何富化**之前**快照下来。
    #[must_use]
    pub fn from_event(event: LarkInboundEvent, installation: &LarkInstallation) -> Self {
        let mentions = mentions_from_event(&event.mentions);
        let bot_open_id = installation.bot_open_id.as_str();
        let bot_union_id = installation.bot_union_id_or_empty();
        let body = match event.message_type.as_str() {
            "text" | "post" => resolve_mentions(
                &flatten_content(&event.message_type, &event.content),
                &mentions,
                bot_open_id,
                bot_union_id,
            ),
            "image" | "file" | "audio" | "media" | "video" => {
                flatten_content(&event.message_type, &event.content)
            }
            // `merge_forward` 的正文要一次 HTTP 往返才能展开（归 enricher）；
            // 其余认不出的类型也不留噪音。
            _ => String::new(),
        };
        let addressed_to_bot = event.chat_type == ChatType::Group
            && contains_mention(&mentions, bot_open_id, bot_union_id);
        Self {
            event_type: event.event_type,
            event_id: event.event_id,
            app_id: event.app_id,
            tenant_key: event.tenant_key,
            chat_id: event.chat_id,
            chat_type: event.chat_type,
            message_id: event.message_id,
            sender_open_id: event.sender_open_id,
            sender_union_id: event.sender_union_id,
            message_type: event.message_type,
            content: event.content,
            mentions: event.mentions,
            create_time: event.create_time,
            parent_id: event.parent_id,
            root_id: event.root_id,
            thread_id: event.thread_id,
            envelope: event.raw,
            body: body.clone(),
            command_body: body,
            addressed_to_bot,
            force_fresh_session: false,
            has_selected_context: false,
        }
    }

    /// 是否是对某条消息的回复 / 引用（上游 `isReply`）。
    #[must_use]
    pub fn is_reply(&self) -> bool {
        !self.parent_id.is_empty() || !self.root_id.is_empty()
    }

    /// 是否在话题里（空 = 顶层消息）。
    #[must_use]
    pub fn is_threaded(&self) -> bool {
        !self.thread_id.is_empty()
    }

    /// 这条消息是不是合并转发（要一次 HTTP 往返才能展开）。
    #[must_use]
    pub fn is_merge_forward(&self) -> bool {
        self.message_type == MSG_TYPE_MERGE_FORWARD
    }

    /// 归一化成 engine 消费的跨平台信封（上游 `channelMessageFromLark`）。
    ///
    /// `raw` = 本结构的 JSON（见模块文档的 `raw` 一节）；`media_refs` **留空** ——
    /// 它是 engine 的输出通道（`mc_core` 的形态纪律第 1 条）。
    ///
    /// # Errors
    ///
    /// 本结构**总是**可序列化（字段全是最普通的类型）⇒ 实际不可失败；返回
    /// `Result` 是为了不在这一层 `expect` 掉一个将来可能出现的不可序列化字段。
    pub fn to_inbound_message(&self) -> Result<InboundMessage, ChannelError> {
        let raw = serde_json::to_value(self).map_err(|error| ChannelError::Storage {
            message: format!("lark: encode inbound payload: {error}"),
        })?;
        let reply_to = self
            .is_reply()
            .then(|| mc_core::channel::message::ReplyCtx {
                message_id: self.parent_id.clone(),
                root_id: self.root_id.clone(),
            });
        Ok(InboundMessage {
            event_id: self.event_id.clone(),
            message_id: self.message_id.clone(),
            source: Source {
                channel_type: TYPE_LARK,
                chat_id: self.chat_id.as_str().to_string(),
                chat_type: self.chat_type,
                sender_id: self.sender_open_id.as_str().to_string(),
                sender_stable_id: self.sender_union_id.clone(),
                thread_id: self.thread_id.clone(),
            },
            kind: message_kind(&self.message_type),
            text: self.body.clone(),
            command_text: self.command_body.clone(),
            has_selected_context: self.has_selected_context,
            media_refs: Vec::new(),
            reply_to,
            addressed_to_bot: self.addressed_to_bot,
            force_fresh: self.force_fresh_session,
            skip_agent_run: false,
            raw,
        })
    }
}

// =====================================================================
// 事件汇（上游 `EventConnector` 的 `onMessage` 收口）
// =====================================================================

/// 一条会话的事件汇：解码产物 → 富化 → 归一化 → engine 的共享入站入口
/// （M7-11 的 [`EventEmitter`]，见 `docs/32` §28.4 的 H1）。
///
/// 它**每条会话一份**（凭据只覆盖一次会话），并且：
///
/// - 富化在**投递之前**跑（ACK 之前，与上游同）；
/// - 富化的失败**从不**上抛：它按契约降级成可读的注记（见 [`super::enricher`]），
///   于是"一次富化超时"不会把整条链路判成基础设施失败（那会触发无谓的重连）；
/// - 投递（handler）的非 `Ok` 是**真**基础设施失败 ⇒ 上抛，连接器按"这次尝试失败"收尾。
pub struct LarkEventEmitter {
    /// 本安装的行投影（提及判据与 region 都在里面）。
    installation: LarkInstallation,
    /// 明文凭据（**只**覆盖本会话；`Debug` 已脱敏）。
    credentials: InstallationCredentials,
    /// 富上下文装配器；`None` = 这个部署不做富化（上游 `Enricher == nil` 同义）。
    enricher: Option<Arc<dyn Enricher>>,
    handler: SharedInboundHandler,
}

impl std::fmt::Debug for LarkEventEmitter {
    /// 手写：`installation` 与 `credentials` 都自带脱敏 `Debug`；端口只报存在性。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LarkEventEmitter")
            .field("installation", &self.installation)
            .field("credentials", &self.credentials)
            .field("has_enricher", &self.enricher.is_some())
            .field("handler", &"<shared inbound handler>")
            .finish()
    }
}

impl LarkEventEmitter {
    /// 装配（每次会话一份）。
    #[must_use]
    pub fn new(
        installation: LarkInstallation,
        credentials: InstallationCredentials,
        enricher: Option<Arc<dyn Enricher>>,
        handler: SharedInboundHandler,
    ) -> Self {
        Self {
            installation,
            credentials,
            enricher,
            handler,
        }
    }
}

#[async_trait]
impl EventEmitter for LarkEventEmitter {
    async fn emit(&self, event: LarkInboundEvent) -> ChannelResult<()> {
        let mut payload = LarkInboundMessage::from_event(event, &self.installation);
        if let Some(enricher) = &self.enricher {
            payload = enricher.enrich(payload, &self.credentials).await;
        }
        let message = payload.to_inbound_message()?;
        self.handler.handle(message).await
    }
}

// =====================================================================
// Channel 实现
// =====================================================================

/// **一个安装的** lark 长连接（上游 `feishuChannel`）。
///
/// - `connect` 阻塞跑**一条**会话（`EventConnector::run`）；连接的整个生命周期都圈在
///   `connect` 里，返回即已释放 ⇒ `disconnect` 只需置位停机信号；
/// - 一次 `connect` = 一代：停机信号**每代新建**（同一个 channel 对象会被反复 `connect`，
///   `docs/32` §28 的 R3 逐字），`disconnect` 置位的是**当前**那一代。
pub struct FeishuChannel {
    installation: LarkInstallation,
    connector: Arc<dyn EventConnector>,
    handler: Option<SharedInboundHandler>,
    sender: Arc<dyn ApiClient>,
    decrypter: Decrypter,
    enricher: Option<Arc<dyn Enricher>>,
    /// 当前代的停机信号（`connect` 开头换新，`disconnect` 置位）。
    stop: Mutex<Option<StopSignal>>,
}

impl std::fmt::Debug for FeishuChannel {
    /// 手写脱敏：`installation` 只报密文长度，解密器只报类别，端口只报存在性。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FeishuChannel")
            .field("installation", &self.installation)
            .field("connector", &"<dyn EventConnector>")
            .field("has_handler", &self.handler.is_some())
            .field("sender", &"<dyn ApiClient>")
            .field("decrypter", &self.decrypter)
            .field("has_enricher", &self.enricher.is_some())
            .finish_non_exhaustive()
    }
}

impl FeishuChannel {
    /// 装配**凭据面**（上游 `newFeishuFactory` 里造出来的那个 channel）。
    #[must_use]
    pub fn new(
        installation: LarkInstallation,
        connector: Arc<dyn EventConnector>,
        handler: Option<SharedInboundHandler>,
        sender: Arc<dyn ApiClient>,
        decrypter: Decrypter,
    ) -> Self {
        Self {
            installation,
            connector,
            handler,
            sender,
            decrypter,
            enricher: None,
            stop: Mutex::new(None),
        }
    }

    /// 接上富上下文装配器（`None` = 不富化）。
    #[must_use]
    pub fn with_enricher(mut self, enricher: Arc<dyn Enricher>) -> Self {
        self.enricher = Some(enricher);
        self
    }

    /// 本安装的行投影（诊断 / 路由键）。
    #[must_use]
    pub fn installation(&self) -> &LarkInstallation {
        &self.installation
    }

    /// 解本安装的明文凭据（**唯一**出口；见模块文档的凭据一节）。
    ///
    /// # Errors
    ///
    /// 密文解不开 ⇒ [`ChannelError::InvalidConfig`]（**不回显**密文；文案只带类别）。
    fn installation_credentials(&self) -> ChannelResult<InstallationCredentials> {
        installation_credentials_for(&self.installation, &self.decrypter).map_err(|error| {
            ChannelError::InvalidConfig {
                kind: TYPE_LARK.as_str().to_string(),
                reason: format!("app_secret unavailable ({error})"),
            }
        })
    }
}

impl Drop for FeishuChannel {
    /// 对象被丢掉时置位停机信号（supervisor 也可能直接丢弃 `connect` 的 future ⇒
    /// `StopHandle` 的发送端随之丢掉，`changed()` 返回 `None`，同样等价于停机）。
    fn drop(&mut self) {
        if let Ok(mut guard) = self.stop.lock() {
            if let Some(signal) = guard.take() {
                signal.stop();
            }
        }
    }
}

#[async_trait]
impl Channel for FeishuChannel {
    fn kind(&self) -> ChannelKind {
        TYPE_LARK
    }

    /// 建连并**阻塞**跑一条会话（上游 `feishuChannel.Connect`）。
    ///
    /// 收尾映射（[`SessionOutcome`] → [`Channel`] 的契约）：
    ///
    /// | 连接器返回 | 本方法 | 理由 |
    /// | --- | --- | --- |
    /// | `Ok(Cancelled)` | `Ok(())` | 停机信号置位 = 取消，**不是**错误（契约逐字） |
    /// | `Ok(Closed)` | `Ok(())` | 对端**正常**关闭 ⇔ 上游 `IsCloseError(CloseNormalClosure/CloseGoingAway)` 那一支，上游也返回 nil（监管器照样退避重连，见 `supervisor::supervise`） |
    /// | `Err(_)` | `Err(_)` | 引导 / 拨号 / 读 / ACK 写失败 ⇒ "这次尝试失败" |
    async fn connect(&self) -> ChannelResult<()> {
        let Some(handler) = self.handler.clone() else {
            return Err(ChannelError::InvalidConfig {
                kind: TYPE_LARK.as_str().to_string(),
                reason: "inbound handler not configured".to_string(),
            });
        };
        if self.installation.app_id.is_empty() {
            return Err(ChannelError::InvalidConfig {
                kind: TYPE_LARK.as_str().to_string(),
                reason: "installation has no app_id".to_string(),
            });
        }
        let credentials = self.installation_credentials()?;
        let (signal, handle) = StopSignal::pair();
        // 换新的一代：`connect` 会被反复调用（supervisor 的 connect → 返回 → 退避 → 再 connect）。
        if let Ok(mut guard) = self.stop.lock() {
            *guard = Some(signal);
        }
        let emitter: Arc<dyn EventEmitter> = Arc::new(LarkEventEmitter::new(
            self.installation.clone(),
            credentials.clone(),
            self.enricher.clone(),
            handler,
        ));
        let outcome = self.connector.run(&credentials, emitter, handle).await;
        match outcome {
            Ok(SessionOutcome::Cancelled) => {
                tracing::info!(
                    app_id = self.installation.app_id,
                    installation_id = %self.installation.id,
                    "lark: ws session cancelled"
                );
                Ok(())
            }
            Ok(SessionOutcome::Closed) => {
                tracing::info!(
                    app_id = self.installation.app_id,
                    installation_id = %self.installation.id,
                    "lark: ws session closed by peer"
                );
                Ok(())
            }
            Err(error) => {
                tracing::warn!(
                    app_id = self.installation.app_id,
                    installation_id = %self.installation.id,
                    code = error.code(),
                    "lark: ws session failed"
                );
                Err(error)
            }
        }
    }

    /// 置位当前代的停机信号（见 [`FeishuChannel::connect`] 的"一代"说明）。
    ///
    /// `connect` 失败后调用**安全**、重复调用**安全**（没有在飞的一代 ⇒ 无事发生）。
    async fn disconnect(&self) -> ChannelResult<()> {
        if let Ok(mut guard) = self.stop.lock() {
            if let Some(signal) = guard.take() {
                signal.stop();
            }
        }
        Ok(())
    }

    /// 用本安装的凭据发一条纯文本（上游 `feishuChannel.Send`）。
    ///
    /// 富卡片 / 媒体 / 流式 patch **不在**这条路上（它们归 M7-13 的
    /// `outbound.rs` / `replier.rs`）；这是跨平台的 `OutboundMessage` 通道。
    async fn send(&self, out: OutboundMessage) -> ChannelResult<SendResult> {
        let credentials = self.installation_credentials()?;
        let reply_target = if out.reply_to.is_empty() {
            ReplyTarget::default()
        } else {
            ReplyTarget {
                message_id: out.reply_to.clone(),
                in_thread: !out.thread_id.is_empty(),
            }
        };
        let message_id = self
            .sender
            .send_text_message(SendTextParams {
                credentials,
                chat_id: ChatId::new(out.chat_id),
                text: out.text,
                reply_target,
            })
            .await
            .map_err(ApiError::into_channel_error)?;
        Ok(SendResult::single(message_id))
    }

    /// 上游 `feishuChannel.Capabilities`：文本 + 富卡片 + 线程回复 + 引用回复 +
    /// 附件 + 打字指示 + 消息编辑（**声明**；本包不做降级）。
    fn capabilities(&self) -> Capability {
        Capability::TEXT
            .union(Capability::RICH_CARD)
            .union(Capability::THREAD_REPLY)
            .union(Capability::QUOTE_REPLY)
            .union(Capability::ATTACHMENT)
            .union(Capability::TYPING_INDICATOR)
            .union(Capability::MESSAGE_EDIT)
    }
}

// =====================================================================
// 工厂
// =====================================================================

/// 造本平台工厂所需的三件外部世界（上游 `FeishuChannelDeps`）。
///
/// 上游注释逐字：`inbound handler` 由 engine 通过 `channel.Config.Handler` **每条 build 一次**
/// 交给工厂；本结构里的三件是**跨安装共享**的。
pub struct FeishuChannelDeps {
    /// WS 长连接（**一个共享实例**，M7-11 的产物）。
    pub connector: Option<Arc<dyn EventConnector>>,
    /// 出站 HTTP 客户端（M7-10 的产物；未装配时用 [`super::client::StubApiClient`]）。
    pub api_client: Arc<dyn ApiClient>,
    /// 解密器（部署密钥的唯一读口是 `mc_http::state::ChannelKeys`，本 crate **不**读 env）。
    pub decrypter: Decrypter,
    /// 富上下文装配器（`None` = 不富化）。
    pub enricher: Option<Arc<dyn Enricher>>,
}

impl std::fmt::Debug for FeishuChannelDeps {
    /// 手写：端口只报存在性，解密器自带脱敏 `Debug`。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FeishuChannelDeps")
            .field("has_connector", &self.connector.is_some())
            .field("api_client", &"<dyn ApiClient>")
            .field("decrypter", &self.decrypter)
            .field("has_enricher", &self.enricher.is_some())
            .finish()
    }
}

impl FeishuChannelDeps {
    /// 装配（连接器缺失 ⇒ 工厂拒装配，见 [`factory`]）。
    #[must_use]
    pub fn new(connector: Arc<dyn EventConnector>, decrypter: Decrypter) -> Self {
        Self {
            connector: Some(connector),
            api_client: Arc::new(super::client::StubApiClient::new()),
            decrypter,
            enricher: None,
        }
    }

    /// 换掉出站 HTTP 客户端。
    #[must_use]
    pub fn with_api_client(mut self, api_client: Arc<dyn ApiClient>) -> Self {
        self.api_client = api_client;
        self
    }

    /// 接上富上下文装配器。
    #[must_use]
    pub fn with_enricher(mut self, enricher: Arc<dyn Enricher>) -> Self {
        self.enricher = Some(enricher);
        self
    }
}

/// 造本平台工厂（上游 `newFeishuFactory` + `RegisterFeishu`）。
///
/// 工厂**校验**配置并返回 `Err`，而不是交出半成品（[`crate::channel::Factory`] 的契约）：
///
/// - `cfg.raw` 解不出 lark 的配置形状 ⇒ `InvalidConfig`；
/// - 没有 `app_id` / 没有密文列 ⇒ `InvalidConfig`；
/// - 密文不是合法 base64 ⇒ `InvalidConfig`（**只报长度**）；
/// - 连接器缺失 ⇒ `InvalidConfig`（"接线未完成"要响亮，而不是假装连上）。
///
/// ⚠️ 注册进 `Registry` 的**调用点**是 `super::register`（anchor 的空实现，归 **M7-14**）；
/// 本工厂是它的**可用面**（见 `docs/32` §29 的 H 项）。
#[must_use]
pub fn factory(deps: FeishuChannelDeps) -> Factory {
    let deps = Arc::new(deps);
    Arc::new(move |config: ChannelConfig| {
        let raw: LarkInstallConfig =
            serde_json::from_value(config.raw.clone()).map_err(|error| {
                ChannelError::InvalidConfig {
                    kind: TYPE_LARK.as_str().to_string(),
                    reason: format!("decode installation config failed at {error}"),
                }
            })?;
        if raw.app_id.is_empty() {
            return Err(ChannelError::InvalidConfig {
                kind: TYPE_LARK.as_str().to_string(),
                reason: "installation config has no app_id".to_string(),
            });
        }
        if raw.app_secret_encrypted.is_empty() {
            return Err(ChannelError::InvalidConfig {
                kind: TYPE_LARK.as_str().to_string(),
                reason: "installation config has no app_secret_encrypted".to_string(),
            });
        }
        let Some(connector) = deps.connector.clone() else {
            return Err(ChannelError::InvalidConfig {
                kind: TYPE_LARK.as_str().to_string(),
                reason: "ws connector is not wired for this build path".to_string(),
            });
        };
        // 身份字段不在 config blob 里 ⇒ workspace / agent / installer 是零值占位
        // （上游 `Installation` 的 `pgtype.UUID` 零值同义）：它们**逐条消息**由 Router 的
        // 安装解析器补上（`docs/60` §2.1），工厂里只有凭据。
        let installation = raw
            .into_installation(
                config.installation_id.unwrap_or_else(|| Id(Uuid::nil())),
                Id(Uuid::nil()),
                Id(Uuid::nil()),
                Id(Uuid::nil()),
                "active",
            )
            .map_err(|error| ChannelError::InvalidConfig {
                kind: TYPE_LARK.as_str().to_string(),
                reason: error.to_string(),
            })?;
        let mut channel = FeishuChannel::new(
            installation,
            connector,
            config.handler,
            Arc::clone(&deps.api_client),
            deps.decrypter.clone(),
        );
        if let Some(enricher) = deps.enricher.clone() {
            channel = channel.with_enricher(enricher);
        }
        Ok(Arc::new(channel) as Arc<dyn Channel>)
    })
}

#[cfg(test)]
mod tests;
