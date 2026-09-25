//! `WeCom` **智能机器人（aibot）的 Channel 实现 + 工厂**：把一条安装接成一条阻塞的长连接
//! （上游 `internal/integrations/wecom/wecom_channel.go`，**672 行**，含"帧路由器"那一半）。
//!
//! - **写者**：M7-19（`LUM-1784` / `docs/60-M7-PLAN.md` §3.3）。
//! - **上游定位**（文件头注释逐字）：本文件是 `engine.Supervisor` 驱动的 **Channel + Factory**，
//!   外加**一条** aibot 连接的 WebSocket 运行循环。`WeCom` 每个机器人只允许一个活连接；supervisor
//!   的 WS 租约在进程层强制同一条"每副本至多一个"的不变式，于是两者合起来给出"每个安装全局只有一个
//!   连接"，而**不需要** wecom 自己的协调。
//! - **读循环为什么不放在共享连接器里**（像 lark 的 `ws_connector.go` 那样）：aibot 协议小到一个
//!   按安装的循环比一层 `EventConnector` 抽象更清楚。slack 在 `slack_channel.go` 里是同一种形状。
//!
//! # 本目录的分工（本片 5 个上游文件的落点）
//!
//! | 上游 | 本地 | 内容 |
//! | --- | --- | --- |
//! | `wecom_channel.go` 的 Channel / 工厂 | **本文件** | `WeComChannel` / `WeComDeps` / `factory` / 注册面 |
//! | `wecom_channel.go` 的 `Connect` / `subscribe` / `dispatchFrame` / `pingLoop` | [`r#loop`] | 握手、心跳、读循环、回调 worker |
//! | `ws_frame.go` 的后半段（约 640 行） | [`inbound`] | 入站归一化（`own_text` / `channel_message_from_callback` / `strip_leading_mentions`…） |
//! | `wecom_resolvers.go` | [`crate::wecom::resolvers`] | 五个端口 |
//! | `inbox_message.go` | [`crate::wecom::inbox_message`] | 收件箱卡片 |
//! | `markdown.go` | [`crate::wecom::markdown`] | 成员文本的两道闸 |
//! | `seal_outcome.go` | [`crate::wecom::seal`] | 收尾判决（交接 H2 的收敛） |
//!
//! # 凭据纪律（`docs/60` §2.3）
//!
//! [`WeComChannel`] **手写 `Debug`**：`secret` 只报有没有、`handler` / 拨号器只报存在性。明文密钥
//! 只经 [`PlaintextSecret`] 出现，且它的唯一出口是 `subscribe_body` 的 `into_value`。任何
//! `tracing::*` 都不插值它。
//!
//! # 与上游的两处**形态**差异（登记 `docs/32` §36）
//!
//! 1. **拨号器是端口**（D1）：上游的 `Dialer` 是 `func(...) (*gorilla.Conn, ...)`；本仓是
//!    [`WsDialer`] trait，生产实现 [`TungsteniteDialer`]，用例注入内存替身（本仓**没有**
//!    `tokio-tungstenite` 的 `handshake` feature ⇒ 起不了真实 WS 服务端）。
//! 2. **发送者登记表是端口**（D5）：上游的 `sendersRegistry`（`senders_registry.go`，204 行）按
//!    `docs/60` §3.3 的写集表归 **M7-20**（`senders.rs`）。本片需要它的**写入面**（连接建立时
//!    `set`、退出时 `clear`），所以这里落一个 [`SenderRegistry`] 端口；M7-20 的实现将同时满足它
//!    与 M7-17 已经落下的读侧端口 [`crate::wecom::outbound::SenderLookup`]。

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use mc_core::channel::ChannelKind;
use mc_core::id::Id;

use crate::capability::Capability;
use crate::channel::{Channel, ChannelConfig, ChannelError, ChannelResult, Factory};
use crate::registry::Registry;
use crate::wecom::credentials::{CredentialsResolver, PlaintextSecret};
use crate::wecom::metrics::{or_nop_metrics, Metrics};
use crate::wecom::types::{ConfigError, InstallConfig, Installation, CHANNEL_TYPE, KIND};
use crate::wecom::ws_sender::WsSender;

pub mod inbound;
pub mod socket;

// `loop` 是 Rust 的保留字 ⇒ 模块名用原始标识符 `r#loop`，文件仍是普普通通的
// `wecom_channel/loop.rs`。
pub mod r#loop;

pub use inbound::{
    channel_message_from_callback, channel_msg_type, is_issue_command, media_for,
    media_placeholder, normalize_wecom_control_layout, strip_leading_mentions, InboundMedia,
    MediaKind, WeComInboundMessage, MAX_QUOTED_RUNES, QUOTE_PREFIX, UNSUPPORTED_MSG_TYPE_RECEIPT,
};
pub use r#loop::{CALLBACK_QUEUE_DEPTH, READ_DEADLINE};
pub use socket::{DialedConnection, NoDialer, TungsteniteDialer, WsDialer, WsReader};

/// aibot 长连接的默认地址（上游 `DefaultWSURL`）。
///
/// `WeCom` 为**每一个**机器人公布同一个全局端点；握手之后 `aibot_subscribe` 帧里带的
/// `(bot_id, secret)` 才说明这条连接属于哪个机器人。
pub const DEFAULT_WS_URL: &str = "wss://openws.work.weixin.qq.com";

/// 从"发出 `aibot_subscribe`"到"收到 `errcode 0` 的 ack"之间的上限（上游 `subscribeTimeout`）。
///
/// 服务端实践中几百毫秒就回；这个界是防一条**静默**的 socket。
pub const SUBSCRIBE_TIMEOUT: Duration = Duration::from_secs(10);

/// `Channel::send` **不**支持（上游 `ErrSendNotSupported`）。
///
/// 理由逐字：`WeCom` 上通用的 `Channel::send` 接缝**没有**诚实的实现 ——
/// `channel.OutboundMessage` 不带 `chat_type`，而出站已经走 `OutboundReplier` / `Outbound`，
/// 它们从入站帧上读到真实的 chat type。曾经这里有一个用 `len(ChatID) > 32` 猜"单聊还是群聊"的桩，
/// 那是整个包里**唯一**一处 chat-type 推断 ⇒ 删掉它，返回"不支持"，而不是留着第二条启发式的出站路径。
pub const SEND_NOT_SUPPORTED: &str =
    "wecom: Channel::send is not supported; outbound goes through OutboundReplier/Outbound";

// =====================================================================
// 发送者登记表（写入面；实现归 M7-20）
// =====================================================================

/// "一条安装**此刻**活着的那把 socket"的**写入面**（上游 `sendersRegistry` 的 `set` / `clear`）。
///
/// 出站回复器与中继需要的是**读**面（M7-17 的 [`crate::wecom::outbound::SenderLookup`] 与
/// [`crate::wecom::outbound::LiveSender`]，而 [`WsSender`] 已经实现了后者）；本片需要的是**写**面。
/// 两个面由 M7-20 的同一张表一起实现 —— 见模块文档的 D5。
///
/// `WsSender`（而不是 [`dyn LiveSender`](crate::wecom::outbound::LiveSender)）是这里的元素类型，
/// 因为流的收尾还要它的 `stream_sender` 那一半（M7-17 的端口默认给 `None`，M7-20 会覆盖）。
///
/// [`dyn LiveSender`]: crate::wecom::outbound::LiveSender
pub trait SenderRegistry: Send + Sync {
    /// 装上这条安装的活 socket（上游 `senders.set`）。
    fn set(&self, installation_id: Id, sender: Arc<WsSender>);

    /// 撤下它 —— **只在这条安装当前登记的仍是 `sender` 时**（上游 `senders.clear` 的令牌语义：
    /// 晚到的 `clear` 不许把一条**新**连接从表里抹掉）。
    fn clear(&self, installation_id: Id, sender: &Arc<WsSender>);
}

// =====================================================================
// 装配袋
// =====================================================================

/// aibot 工厂要的**共享依赖**（上游 `ChannelDeps`）。
///
/// 上游把它写成一个结构，让 `RegisterWecom` 与它的调用点之间只传一个值；本仓同款，且去掉了
/// 上游那两个"可为 nil"的字段（`Logger` 换成 `tracing`；`Dialer` / `WSURL` 是给用例的覆写，
/// 而本仓的用例走 [`WsDialer`] 端口 ⇒ 不需要）。
pub struct WeComDeps {
    /// 解封存储里的长连接密钥。**必填**：没有它就不该有一个能造出 Channel 的工厂。
    pub credentials: Arc<dyn CredentialsResolver>,
    /// 发送者登记表。`None` = 本部署不提供出站面 ⇒ 连接照起，但出站回复拿不到 socket
    /// （上游注释逐字：`nil in tests that don't exercise outbound`）。
    pub senders: Option<Arc<dyn SenderRegistry>>,
    /// 健康信号的汇。`None` ⇒ 每个计数器被丢掉（`/metrics` 关掉的部署拿到的就是它）。
    pub metrics: Option<&'static dyn Metrics>,
    /// 拨号端口（生产 [`TungsteniteDialer`]；用例注入内存替身）。
    pub dialer: Arc<dyn WsDialer>,
    /// 覆写 [`DEFAULT_WS_URL`]（**只为用例**；上游同款）。
    pub ws_url: String,
}

impl fmt::Debug for WeComDeps {
    /// 手写：四个端口只报**存在性**，URL 报出来（它是公开端点，不是凭据）。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WeComDeps")
            .field("credentials", &"<dyn CredentialsResolver>")
            .field("senders", &self.senders.is_some())
            .field("metrics", &self.metrics.is_some())
            .field("dialer", &"<dyn WsDialer>")
            .field("ws_url", &self.ws_url)
            .finish()
    }
}

impl WeComDeps {
    /// 生产形态：`tokio-tungstenite` 拨号 + 默认端点。
    #[must_use]
    pub fn new(credentials: Arc<dyn CredentialsResolver>) -> Self {
        Self {
            credentials,
            senders: None,
            metrics: None,
            dialer: Arc::new(TungsteniteDialer),
            ws_url: DEFAULT_WS_URL.to_string(),
        }
    }

    /// 挂上传送者登记表。
    #[must_use]
    pub fn with_senders(mut self, senders: Arc<dyn SenderRegistry>) -> Self {
        self.senders = Some(senders);
        self
    }

    /// 挂上健康汇。
    #[must_use]
    pub fn with_metrics(mut self, metrics: &'static dyn Metrics) -> Self {
        self.metrics = Some(metrics);
        self
    }

    /// 覆写端点（用例）。
    #[must_use]
    pub fn with_ws_url(mut self, ws_url: impl Into<String>) -> Self {
        self.ws_url = ws_url.into();
        self
    }

    /// 覆写拨号器（用例）。
    #[must_use]
    pub fn with_dialer(mut self, dialer: Arc<dyn WsDialer>) -> Self {
        self.dialer = dialer;
        self
    }
}

// =====================================================================
// Channel
// =====================================================================

/// 一条安装的 aibot 智能机器人 WebSocket 连接（上游 `wecomChannel`）。
pub struct WeComChannel {
    installation_id: Option<Id>,
    bot_id: String,
    secret: PlaintextSecret,
    /// 机器人在聊里的显示名（来自安装配置）。没填的安装上是空串；空值时的回落见
    /// [`strip_leading_mentions`]。
    bot_display_name: String,
    handler: Option<crate::message::SharedInboundHandler>,
    dialer: Arc<dyn WsDialer>,
    ws_url: String,
    senders: Option<Arc<dyn SenderRegistry>>,
    /// 健康汇。存的是 `Option`（"配没配"是**部署事实**，得能被 `Debug` 报出来）；调用点一律经
    /// [`or_nop_metrics`] 换成一个永远可调用的借用。
    metrics: Option<&'static dyn Metrics>,
}

impl fmt::Debug for WeComChannel {
    /// 手写脱敏（凭据纪律第 1 条）：密钥只报有没有，端口只报存在性。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WeComChannel")
            .field("installation_id", &self.installation_id)
            .field("bot_id", &self.bot_id)
            .field(
                "secret",
                &if self.secret.is_empty() {
                    "<empty>"
                } else {
                    "<redacted>"
                },
            )
            .field("bot_display_name", &self.bot_display_name)
            .field("handler", &self.handler.is_some())
            .field("dialer", &"<dyn WsDialer>")
            .field("ws_url", &self.ws_url)
            .field("senders", &self.senders.is_some())
            .field("metrics", &self.metrics.is_some())
            .finish()
    }
}

impl WeComChannel {
    /// 装配一条连接。工厂用；用例也可以直接构造（于是那条路径不走配置解码）。
    #[must_use]
    pub fn new(
        bot_id: impl Into<String>,
        secret: PlaintextSecret,
        handler: Option<crate::message::SharedInboundHandler>,
        deps: &WeComDeps,
    ) -> Self {
        Self {
            installation_id: None,
            bot_id: bot_id.into(),
            secret,
            bot_display_name: String::new(),
            handler,
            dialer: Arc::clone(&deps.dialer),
            ws_url: deps.ws_url.clone(),
            senders: deps.senders.clone(),
            metrics: deps.metrics,
        }
    }

    /// 安装行的 id（诊断用；`None` = 这条构造路径没有安装行）。
    #[must_use]
    pub fn installation_id(&self) -> Option<Id> {
        self.installation_id
    }

    /// 机器人的标识（**不是**凭据，可以照常进日志）。
    #[must_use]
    pub fn bot_id(&self) -> &str {
        &self.bot_id
    }

    /// 机器人的显示名（空 = 没填，`@` 提及回落到空白启发式）。
    #[must_use]
    pub fn bot_display_name(&self) -> &str {
        &self.bot_display_name
    }

    /// 本连接汇报的汇（**永远**可调用：没配时是 no-op）。
    #[must_use]
    pub fn metrics(&self) -> &'static dyn Metrics {
        or_nop_metrics(self.metrics)
    }
}

#[async_trait]
impl Channel for WeComChannel {
    fn kind(&self) -> ChannelKind {
        KIND
    }

    /// 拨号 → `aibot_subscribe` → 读循环（见 [`r#loop::run`]）。
    async fn connect(&self) -> ChannelResult<()> {
        r#loop::run(self).await
    }

    /// **no-op**（上游注释逐字）：WS 连接的整个生命周期都被 `connect` 圈住（它在运行上下文被取消时
    /// 返回），所以这里没有长寿资源要释放。与 `feishuChannel` / `slackChannel` 同款。
    async fn disconnect(&self) -> ChannelResult<()> {
        Ok(())
    }

    /// **不支持**（见 [`SEND_NOT_SUPPORTED`]）。
    async fn send(
        &self,
        _out: mc_core::channel::message::OutboundMessage,
    ) -> ChannelResult<mc_core::channel::message::SendResult> {
        Err(ChannelError::InvalidConfig {
            kind: CHANNEL_TYPE.to_string(),
            reason: SEND_NOT_SUPPORTED.to_string(),
        })
    }

    /// 上游 `Capabilities` 逐字：入站附件会被下载 / 解密 / 绑定（M7-18 的 `media_ingest.rs`），
    /// 所以 `CapAttachment` 在与 dingtalk 相同的方向上成立。把媒体**发出去**是另一件事 —— 它要
    /// `WeCom` 的 `aibot_upload_media_*` 握手 —— 这里不声称它。
    fn capabilities(&self) -> Capability {
        Capability::TEXT.union(Capability::ATTACHMENT)
    }
}

// =====================================================================
// 工厂
// =====================================================================

/// 造 `WeComChannel` 的工厂（上游 `newWecomFactory`）。
///
/// 它**校验** `raw` 并返回 `Err`，而不是交出一个半成品（`Factory` 的契约）。
#[must_use]
pub fn factory(deps: Arc<WeComDeps>) -> Factory {
    Arc::new(move |config: ChannelConfig| {
        let installation = installation_from_config(&config)?;
        let credentials = deps.credentials.credentials(&installation).map_err(|_| {
            ChannelError::InvalidConfig {
                kind: CHANNEL_TYPE.to_string(),
                reason: "the stored long-connection secret could not be decrypted".to_string(),
            }
        })?;
        Ok(Arc::new(WeComChannel {
            installation_id: config.installation_id.or(Some(installation.id)),
            bot_id: credentials.bot_id,
            secret: credentials.secret,
            bot_display_name: installation.bot_display_name,
            handler: config.handler,
            dialer: Arc::clone(&deps.dialer),
            ws_url: deps.ws_url.clone(),
            senders: deps.senders.clone(),
            metrics: deps.metrics,
        }) as Arc<dyn Channel>)
    })
}

/// 从一条 [`ChannelConfig`] 解出 `WeCom` 的安装投影（上游工厂里那段 `json.Unmarshal` + 校验）。
///
/// 它走 M7-15 的 [`Installation::from_row`]，也就是说 4 条"别改的细节"（`app_id == bot_id`、
/// 密文是 base64 字符串、`bot_display_name` 可缺、键名不是 lark 的 `app_secret_encrypted`）在这里
/// **继承**，而不是在这里重写一遍。
///
/// ⚠️ 工厂手上只有 `config` 包 + 可选的 `installation_id`，而 [`CredentialsResolver`] 要一个完整的
/// [`Installation`] ⇒ 工作区 / agent / 安装者三列在这条路径上没有来源、填成 nil。这不是"猜"：
/// 解封器只读 `bot_id` 与 `secret_encrypted`，而两者都来自 `config`。
fn installation_from_config(config: &ChannelConfig) -> ChannelResult<Installation> {
    let install = InstallConfig::from_value(&config.raw).map_err(|error| match error {
        ConfigError::Empty => ChannelError::InvalidConfig {
            kind: CHANNEL_TYPE.to_string(),
            reason: "the installation has no config blob".to_string(),
        },
        other => ChannelError::InvalidConfig {
            kind: CHANNEL_TYPE.to_string(),
            reason: format!("decode the installation config failed: {other}"),
        },
    })?;
    if install.bot_id.is_empty() && install.app_id.is_empty() {
        return Err(ChannelError::InvalidConfig {
            kind: CHANNEL_TYPE.to_string(),
            reason: "the installation config is missing bot_id".to_string(),
        });
    }
    let now = chrono::Utc::now();
    let nil = uuid::Uuid::nil();
    let row = mc_repos::channel::installation::ChannelInstallationRow {
        id: config.installation_id.map_or(nil, |id| id.0),
        workspace_id: nil,
        agent_id: nil,
        channel_type: CHANNEL_TYPE.to_string(),
        config: config.raw.clone(),
        status: "active".to_string(),
        ws_lease_token: None,
        ws_lease_expires_at: None,
        installer_user_id: nil,
        installed_at: now,
        created_at: now,
        updated_at: now,
    };
    Installation::from_row(&row).map_err(|error| ChannelError::InvalidConfig {
        kind: CHANNEL_TYPE.to_string(),
        reason: format!("decode the installation config failed: {error}"),
    })
}

// =====================================================================
// 注册面
// =====================================================================

/// 把本平台的工厂注册进 `registry`（**失败关闭**的凭据面，与 slack / dingtalk 同款）。
///
/// `apps/mc-server/src/channels.rs` 只拿得到 [`crate::engine::ChannelDeps`]（它没有部署密钥），
/// 所以这条路径注册的是一个**造不出任何 Channel** 的工厂：任何一次 `build` 都会以
/// `channel_invalid_config` 失败，而不是假装接上了。
pub fn register(registry: &Registry, _deps: &crate::engine::ChannelDeps) {
    tracing::warn!(
        "wecom: registering the factory without a credential resolver; every installation will be \
         refused at build time (call `mc_channel::wecom::wecom_channel::register_with` with the \
         deployment key to wire it)"
    );
    registry.register(KIND, fail_closed_factory());
}

/// 接线好的注册入口（宿主把部署密钥与登记表交进来）。
pub fn register_with(registry: &Registry, deps: Arc<WeComDeps>) {
    registry.register(KIND, factory(deps));
}

/// 把本平台的解析器面注册进 `router`（与 `slack::register_resolvers` 同款）。
pub fn register_resolvers(
    router: &crate::engine::Router,
    set: crate::wecom::resolvers::WeComResolverSet,
) {
    router.register(KIND, set.into_engine_set());
}

/// **失败关闭**的工厂：没有凭据解封器 ⇒ 每一次 `build` 都失败，且错误里没有半个凭据字节。
///
/// 显式给出来，是为了让自检能断言"就是它"，而不是靠一条 `warn!` 的字符串。
#[must_use]
pub fn fail_closed_factory() -> Factory {
    Arc::new(|_config: ChannelConfig| {
        Err(ChannelError::InvalidConfig {
            kind: CHANNEL_TYPE.to_string(),
            reason: "no credential resolver is wired (fail closed)".to_string(),
        })
    })
}

/// 本 adapter 的平台判别式（诊断 / 注册用）。
#[must_use]
pub fn kind() -> ChannelKind {
    KIND
}

/// `/issue` 的来源标签（**逐字** `wecom_chat`）。
#[must_use]
pub fn origin_type() -> &'static str {
    crate::wecom::resolvers::ORIGIN_WECOM_CHAT
}

/// 给宿主的自检：`WsSender` 同时满足 M7-17 的读侧端口与本片的写侧端口 —— 一条编译期的事实，
/// 而不是三处各自 `impl` 的约定。
#[allow(dead_code)]
fn live_sender_is_shareable(sender: Arc<WsSender>) -> Arc<dyn crate::wecom::outbound::LiveSender> {
    sender
}

#[cfg(test)]
mod tests;
