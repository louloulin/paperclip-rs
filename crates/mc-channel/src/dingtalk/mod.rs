//! `DingTalk` adapter（上游 `internal/integrations/dingtalk`：76 文件 / 25 非测试 / 5,910 上游行）。
//!
//! **状态：M7-7 落「入站 + Stream 连接」，M7-8 落「出站 / 媒体 / 回复 / 回执」**
//! （`LUM-1772` → `LUM-1773`）。本文件承载**每安装一条的 Stream WebSocket 连接**（建连 /
//! 帧服务 / 停机判决）、工厂与注册面；帧编解码与连接引导在 [`stream`]、归一化在 [`inbound`]、
//! per-conversation 串行队列在 [`dispatch`]、解析器面在 [`resolvers`]、表情契约在 [`emotion`]；
//! **出站发送**在 [`outbound`]、判决驱动的回复在 [`replier`]、入站媒体在 [`media`]、
//! 表情回执在 [`ack`]、分片与转义在 [`markdown`]。
//!
//! **入站 = Stream WebSocket**（每个 BYO installation 一条，网关把帧**推**过来）；部署密钥
//! `MULTICA_DINGTALK_SECRET_KEY`（凭据只经 `mc_secrets::secretbox` 与 `mc-telemetry` 的
//! redaction 通道，`docs/60` §2.3）；`group-routes` **必须保持 404**。
//!
//! # 本目录的写者表（M7-7 / M7-8 / M7-9；`mod.rs` 是本文件）
//!
//! | 文件 | 写者 | 上游 |
//! | --- | :-: | --- |
//! | `mod.rs` | **M7-7**（连接 / 工厂 / 注册面）+ **M7-8**（`pub mod` 与出站接线） | `dingtalk_channel.go` 的连接那一半 |
//! | `stream.rs` / `inbound.rs` / `dispatch.rs` / `emotion.rs` / `resolvers.rs`（各带 `tests/`） | **M7-7** | `ws_frame.go`（67）+ `ws_endpoint.go`（105）+ `ws_connector.go`（226）+ `inbound.go`（827）+ `inbound_card.go`（108）+ `dispatch.go`（202）+ `emotion.go`（67）+ `resolvers.go`（533）+ 上游四份 golden |
//! | `outbound.rs` + `outbound/{target,openapi,credentials,quote,source,tests}.rs` | **M7-8** | `outbound.go`（313）+ `outbound_send.go`（238）+ `outbound_quote.go`（51）+ `reply_source.go`（127）+ `client.go`/`token.go` 的出站两条 |
//! | `replier.rs` / `media.rs`（各带 `tests/`）、`ack.rs` + `ack/{batch,tests}.rs`、`markdown.rs` | **M7-8** | `replier.go`（342）/ `media.go`（385）/ `ack.go`（220）+ `ack_batch.go`（160）/ `markdown.go`（286）+ `outbound_send.go` 的转义三函数 |
//! | `config.rs` / `client.rs` / `install.rs` / `byo_install.rs` / `binding.rs` / `group_identity.rs` | **M7-9** | 见 `docs/60` §4.1 的那一行 |
//!
//! ## 写集勘误（**逐条登记**，照 M7-3 / M7-5 / M7-6 的先例；全文见 `docs/32` §19 / §22）
//!
//! `docs/60` §3.3 给两片的格子都是 5 个 `dingtalk/*.rs`，起手补充追加了本文件（**第二类漏项
//! 第 4 次**：不写进 `pub mod` 就根本不参与编译）。再追加的路径**只有两类**，都是**门 ⑩ 的
//! 800 行硬限**逼出来的切分（不是拆凑数字）：`*/tests.rs`（同目录先例 `slack/*/tests.rs`），
//! 以及**上游文件边界上**的再切分（`docs/32` §22 的 D1 逐条列了边界）。拆完每个文件都 ≤800 行，
//! 且**未动** `scripts/file_size_baseline.tsv`。
//!
//! # 注册约定（五个 adapter 一致，别各自发明）
//!
//! - 工厂必须校验 `raw` 配置并返回 `Err`，**不要**交出半成品（[`crate::channel::Factory`] 的契约）；
//! - 部署密钥缺失 ⇒ 该平台**整体不装配**（判据在 `apps/mc-server/src/channels.rs`，`docs/60`
//!   §2.6 第 3 条）。**路由仍然存在**，并按各端点自己的"未配置"语义回响应；
//! - adapter **不得**直接写 DB：只走 [`crate::engine::ChannelDeps`] 里注入的 port。
//!
//! # 解密器的接线（**交接项**，与 M7-5 / M7-6 同一落点）
//!
//! [`register`] 的签名里**没有**部署密钥的位置（`ChannelDeps` 是 M7-1 定死的形态），而密钥的
//! **唯一读取口**是 `mc_http::state::ChannelKeys`（`mc-channel` 不得自己 `std::env::var`）⇒
//! [`register`] 用**失败关闭**的凭据面（带密文的安装行**拒装配**），[`register_with`] 才是
//! 接线好的入口（[`DingTalkDeps::with_decrypter`]）；M7-9 给正式实现。
//!
//! # 状态：**入站 + Stream 连接闭环；出站闭环（须注入端口）；装配面仍是交接项**
//!
//! | 面 | 状态 |
//! | --- | --- |
//! | `Channel::connect`（Stream 帧循环） | **已闭环**（M7-7）：引导 → 拨号 → ping/pong → 回调入队 → ack；三种收尾各有用例 |
//! | `Channel::send`（出站） | **已闭环**（M7-8）：工厂注入 [`outbound::OpenApiTransport`] ⇒ 群发送；直接 `new`（不注入）仍失败关闭 |
//! | 出站回复 / 判决回复 / 入站媒体 / 表情回执 | **已闭环**：[`outbound::OutboundDelivery`]（`EventChatDone` 那条路的**显式调用**入口，本仓没有进程内事件总线 ⇒ 同 M7-6 先例）、[`replier::DingTalkOutboundReplier`]、[`media::DingTalkMediaResolver`]、[`ack::AckNotifier`]；四者的装配（`with_replier` / `with_media` / `with_typing`）是**交接项** |
//! | 入站流水线 / 群清单 / bot 身份 | **已闭环**（[`resolvers::DingTalkResolverSet`]，装配调用是交接项）；后两项是**诚实默认值**（三张表的写语句在 `mc-repos`，不在本片写集） |
//! | 7 条路由 | **归 M7-9**；本片 0 路由 |
//!
//! ## M7-8 交给 M7-9 / M7-21 的事项（逐条，别默默略过）
//!
//! 1. **`client.rs` 收敛**：令牌缓存 + `postJSON` 现在是 [`outbound::HttpOpenApi`] ⇒ 换实现
//!    即可，发送端的校验 / 分片 / 401 重试语义不动。
//! 2. **凭据解码**：`app_secret_encrypted` 的三条分支**共用** [`StreamInstallConfig`] /
//!    `resolve_app_secret`（本片把它 `pub(crate)` 了一下）；M7-9 的 `config.rs` 收敛 `Decrypter`
//!    时并成一处。
//! 3. **`reply_source` 的写入点**：上游在 resolver 的 append 成功后调 `rememberReplySource`；
//!    本片给出 [`outbound::ReplySourceCache`] 与 [`ack::AckNotifier::remember_source`]，
//!    **接线点**仍缺。
//! 4. **装配**：`with_replier` / `with_media` / `with_typing` 由宿主做（本片 0 路由）。
//!
//! # 连接的生命周期判决（上游 `stopDispatch`）与队列跨重连复用（上游 `dispatchSlot`）
//!
//! 被 supervisor 取消 = "这一代是生命周期停机"（要收口队列），自己返回 = "重连"（**保留**队列）。
//! 本仓的 supervisor 用**丢弃 `connect` 的 future** 表达取消 ⇒ 判决由 [`RelinquishGuard`] 在
//! `Drop` 里给出：被丢弃 ⇒ 置位；正常返回 ⇒ 显式"拆信管"（[`RelinquishGuard::defuse`]）。
//! 队列由**工厂**持有（[`dispatch::DispatchSlotRegistry`]，按 `AppKey` 一格）⇒ 重连**复用同一条
//! 队列** —— 上游逐字：*prevents an old in-flight turn and the next turn received after
//! reconnect from running concurrently*。
//!
pub mod ack;
pub mod dispatch;
pub mod emotion;
pub mod inbound;
pub mod jobs;
pub mod markdown;
pub mod media;
pub mod outbound;
pub mod replier;
pub mod resolvers;
pub mod stream;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mc_core::channel::message::{OutboundMessage, SendResult};
use mc_core::channel::ChannelKind;
use serde::Deserialize;

use crate::capability::Capability;
use crate::channel::{Channel, ChannelConfig, ChannelError, ChannelResult, Factory};
use crate::engine::ChannelDeps;
use crate::message::SharedInboundHandler;
use crate::registry::Registry;

use dispatch::{DispatchLimits, DispatchSlotRegistry, Dispatcher};
use inbound::{ORIGIN_DINGTALK_CHAT, TYPE_DINGTALK};
use jobs::{CallbackJobHandler, DispatchSink};
use stream::{
    AppSecret, ConnectionOpener, Connector, ReqwestOpener, SessionOutcome, StopHandle, StreamKnobs,
    TungsteniteDialer, WsDialer,
};

pub use inbound::{decode_dingtalk_raw, DingtalkRawEvent};
pub use jobs::{BotNameSource, NoBotName};

/// 本 adapter 的平台判别式（上游 `TypeDingTalk`；诊断 / 注册用）。
pub const KIND: ChannelKind = TYPE_DINGTALK;

/// 断连时给队列收口的预算（上游由 supervisor 的 ctx 给；本仓留出**小于** supervisor 的
/// `disconnect_timeout`（5s）的值 ⇒ 收口先结束，supervisor 的超时不会先响）。
pub const DISCONNECT_DRAIN_BUDGET: Duration = Duration::from_secs(4);

// =====================================================================
// 连接的生命周期判决（上游 `stopDispatch`）
// =====================================================================

/// "这一代是不是被**生命周期停机**取消的"判决。
///
/// 用法（见 [`Channel::connect`]）：
///
/// ```ignore
/// let verdict = RelinquishGuard::new(&self.relinquish);
/// let outcome = connector.run_session(stop).await;   // ← future 在这里可能被丢弃
/// verdict.defuse();                                  // 正常返回 ⇒ 拆信管
/// ```
///
/// 被丢弃（supervisor 的 `select!` 选中了停机 / 租约丢失那一支）⇒ 不拆信管 ⇒ `Drop` 置位。
struct RelinquishGuard<'a> {
    flag: &'a AtomicBool,
    defused: bool,
}

impl<'a> RelinquishGuard<'a> {
    fn new(flag: &'a AtomicBool) -> Self {
        Self {
            flag,
            defused: false,
        }
    }

    /// 正常返回：拆信管（`Drop` 不再置位）。
    fn defuse(mut self) {
        self.defused = true;
    }
}

impl Drop for RelinquishGuard<'_> {
    fn drop(&mut self) {
        if !self.defused {
            self.flag.store(true, Ordering::SeqCst);
        }
    }
}

// =====================================================================
// Channel 实现（上游 `dingtalkChannel`）
// =====================================================================

/// **一个安装的** Stream 连接（上游 `dingtalkChannel`）。
///
/// 每个安装带自己的机器人（自己的 `AppKey` + 密文 `AppSecret` 在安装配置里）⇒ 它有自己的连接，
/// 与 per-installation 的 slack / telegram 完全同形。`engine.Supervisor` 按活跃安装各建一条
/// （经注册的 [`Factory`]），并由它持有租约 / 重连生命周期。
pub struct DingTalkChannel {
    app_key: String,
    /// 明文 `AppSecret`（**手写脱敏**类型）：开 Stream 连接 + 铸访问令牌（后者归 M7-8）。
    app_secret: AppSecret,
    handler: Option<SharedInboundHandler>,
    opener: Arc<dyn ConnectionOpener>,
    dialer: Arc<dyn WsDialer>,
    /// 本安装的入站队列（**跨重连复用**，见模块文档）。
    dispatcher: Arc<Dispatcher>,
    slots: Arc<DispatchSlotRegistry>,
    knobs: StreamKnobs,
    /// 生命周期停机的判决（见 [`RelinquishGuard`]）。
    relinquish: AtomicBool,
    /// 出站端口（**M7-8** 注入；见 [`DingTalkChannel::with_outbound`]）。
    ///
    /// `None` ⇒ `send` **失败关闭**（构造器不隐式造 HTTP 客户端；工厂会注入）。
    outbound: Option<Arc<dyn outbound::OpenApiTransport>>,
    /// 机器人码（上游 `robotCodeOrAppID`）。`send` 的请求体要它。
    robot_code: String,
}

impl std::fmt::Debug for DingTalkChannel {
    /// 手写脱敏：`app_secret` 是 [`AppSecret`]（`<redacted>`），端口只打印存在性。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DingTalkChannel")
            .field("app_key", &self.app_key)
            .field("app_secret", &self.app_secret)
            .field("has_handler", &self.handler.is_some())
            .field("opener", &"<dyn ConnectionOpener>")
            .field("dialer", &"<dyn WsDialer>")
            .field("dispatcher", &self.dispatcher)
            .field("slots", &self.slots)
            .field("knobs", &self.knobs)
            .field("relinquish", &self.relinquish.load(Ordering::SeqCst))
            .field(
                "outbound",
                &self
                    .outbound
                    .as_ref()
                    .map_or("<none>", |_| "<dyn OpenApiTransport>"),
            )
            .field("robot_code", &self.robot_code)
            .finish()
    }
}

impl DingTalkChannel {
    /// 装配一条连接。
    #[must_use]
    pub fn new(
        app_key: impl Into<String>,
        app_secret: AppSecret,
        handler: Option<SharedInboundHandler>,
        opener: Arc<dyn ConnectionOpener>,
        dialer: Arc<dyn WsDialer>,
        dispatcher: Arc<Dispatcher>,
        slots: Arc<DispatchSlotRegistry>,
    ) -> Self {
        let app_key = app_key.into();
        Self {
            robot_code: app_key.clone(),
            app_key,
            app_secret,
            handler,
            opener,
            dialer,
            dispatcher,
            slots,
            knobs: StreamKnobs::default(),
            relinquish: AtomicBool::new(false),
            outbound: None,
        }
    }

    /// 注入出站端口（**M7-8**；工厂在装配时调它）。
    ///
    /// 不注入也可以构造（M7-7 的用例走的就是那条路）—— 那时 `send` **失败关闭**，
    /// 而不是偷偷去造一个 `reqwest` 客户端。生产路径由 [`factory_with_slots`] 注入。
    #[must_use]
    pub fn with_outbound(mut self, transport: Arc<dyn outbound::OpenApiTransport>) -> Self {
        self.outbound = Some(transport);
        self
    }

    /// 换机器人码（上游 `robotCodeOrAppID`）。
    #[must_use]
    pub fn with_robot_code(mut self, robot_code: impl Into<String>) -> Self {
        self.robot_code = robot_code.into();
        self
    }

    /// 换时间旋钮（用例用；上游的 30s / 90s / 10s 是生产默认值）。
    #[must_use]
    pub fn with_knobs(mut self, knobs: StreamKnobs) -> Self {
        self.knobs = knobs;
        self
    }

    /// 本安装的 AppKey（路由键；回调的信封要盖上它）。
    #[must_use]
    pub fn app_key(&self) -> &str {
        &self.app_key
    }

    /// 本安装的入站队列（诊断 / 用例）。
    #[must_use]
    pub fn dispatcher(&self) -> &Arc<Dispatcher> {
        &self.dispatcher
    }

    /// 这一代是否被判为"生命周期停机"（诊断 / 用例）。
    #[must_use]
    pub fn relinquished(&self) -> bool {
        self.relinquish.load(Ordering::SeqCst)
    }

    /// 用例用：把判决置成"生命周期停机"（等价于 `connect` 的 future 被 supervisor 丢弃）。
    #[cfg(test)]
    pub(crate) fn relinquish_for_test(&self) {
        self.relinquish.store(true, Ordering::SeqCst);
    }
}

#[async_trait]
impl Channel for DingTalkChannel {
    fn kind(&self) -> ChannelKind {
        TYPE_DINGTALK
    }

    /// 建连并跑帧循环（上游 `dingtalkChannel.Connect`）。
    ///
    /// 三种收尾**各不相同**（上游逐字）：
    ///
    /// - 网关发 `SYSTEM/disconnect` ⇒ 干净返回（supervisor 退避后重拨，**队列保留**）；
    /// - 停机信号（队列收口 / supervisor 丢弃本 future）⇒ 干净返回，且**判为生命周期停机**
    ///   （`disconnect` 会收口队列）；
    /// - 链路断 / 读超时 / 引导失败 ⇒ `Err`（supervisor 按"这次尝试失败"退避重连）。
    async fn connect(&self) -> ChannelResult<()> {
        if self.handler.is_none() {
            return Err(ChannelError::InvalidConfig {
                kind: TYPE_DINGTALK.as_str().to_string(),
                reason: "inbound handler not configured".to_string(),
            });
        }
        if self.app_secret.is_empty() {
            return Err(ChannelError::InvalidConfig {
                kind: TYPE_DINGTALK.as_str().to_string(),
                reason: "app secret not configured".to_string(),
            });
        }
        // 每一代重新开始判决（同一个 channel 对象可能被反复 connect）。
        self.relinquish.store(false, Ordering::SeqCst);
        let verdict = RelinquishGuard::new(&self.relinquish);

        let connector = Connector::new(
            Arc::clone(&self.opener),
            Arc::clone(&self.dialer),
            self.app_key.clone(),
            self.app_secret.clone(),
            Arc::new(DispatchSink {
                app_key: self.app_key.clone(),
                dispatcher: Arc::clone(&self.dispatcher),
            }),
        )
        .with_knobs(self.knobs);

        // 队列收口 ⇒ 会话也优雅退出（宿主停机时先收队列，帧循环随之收尾）。
        let stop = StopHandle::from_receiver(self.dispatcher.closed_receiver());
        let outcome = connector.run_session(stop).await;
        verdict.defuse();
        match outcome {
            Ok(SessionOutcome::Cancelled | SessionOutcome::DisconnectRequested) => {
                tracing::info!(
                    app_key = self.app_key,
                    outcome = ?outcome,
                    "dingtalk: stream session ended cleanly"
                );
                Ok(())
            }
            Err(error) => {
                tracing::warn!(
                    app_key = self.app_key,
                    code = error.code(),
                    "dingtalk: stream session failed"
                );
                Err(error)
            }
        }
    }

    /// 拆链路（上游 `dingtalkChannel.Disconnect`）。
    ///
    /// 判决见模块文档：**只有**判为"生命周期停机"的那一代才收口队列 —— 传输错误 / 网关要求
    /// 重连之后，队列必须活着（跨重连的会话顺序优先）。收到口后的槽不再复用 ⇒ 下一代会建新队列。
    async fn disconnect(&self) -> ChannelResult<()> {
        if !self.relinquished() {
            return Ok(());
        }
        let drained = self
            .dispatcher
            .drain_and_close(DISCONNECT_DRAIN_BUDGET)
            .await;
        self.slots.release(&self.app_key, &self.dispatcher);
        if drained {
            Ok(())
        } else {
            // 收口预算用尽（上游同样回错误：`dingtalk: dispatcher drain: …`）。supervisor 只
            // 记一条 warn（`disconnect` 不在退避路径上）。
            Err(ChannelError::Shutdown)
        }
    }

    /// 出站：用本安装的机器人往 `out.chat_id` 发一条群消息（上游 `dingtalkChannel.Send`）。
    ///
    /// 上游逐字：`Send` 只给 `out.ChatID`，所以目标是**群**（引用 / 直聊那些形态由
    /// `outbound.rs` / `replier.rs` 的完整目标构造）。
    /// 没注入端口 ⇒ **失败关闭**（见 [`DingTalkChannel::with_outbound`]）。
    async fn send(&self, out: OutboundMessage) -> ChannelResult<SendResult> {
        let Some(transport) = self.outbound.as_ref() else {
            // `send` 不在 supervisor 的退避路径上，用 `Transport` 表达"这条链路不可用"即可。
            return Err(ChannelError::Transport {
                message: "dingtalk: outbound send is not wired for this installation (M7-8)"
                    .to_string(),
            });
        };
        let sender = outbound::Sender::new(
            Arc::clone(transport),
            self.robot_code.clone(),
            self.app_key.clone(),
            self.app_secret.clone(),
        );
        let target = outbound::SendTarget::group(out.chat_id.clone());
        let key = sender
            .send(&target, &out.text)
            .await
            .map_err(outbound::DingTalkApiError::into_channel_error)?;
        Ok(SendResult::single(key))
    }

    /// 上游 `CapText | CapAttachment`。
    ///
    /// `ATTACHMENT` 的**实现**（引用卡片 / 互动卡片 / 媒体出站）归 M7-8；本片照上游声明位图，
    /// 并在 `M7-8` 接上出站之后才真正成立（同 M7-3 → M7-4 的先例）。
    fn capabilities(&self) -> Capability {
        Capability::TEXT.union(Capability::ATTACHMENT)
    }
}

// =====================================================================
// 配置与凭据（本片的**收窄**形态）
// =====================================================================

/// 安装配置里的**本片所需字段**（上游 `installConfig` 的收窄形态）。
///
/// ⚠️ 完整的 `installConfig`（含 `robot_code` 的显式语义、`secretbox` 密文的读写、
/// `token.go` 的访问令牌缓存）归 **M7-9** 的 `config.rs`（上游 `config.go` + `token.go`）。
/// 本片只落 Stream 连接真正要的三件事：路由键 `app_id`、明文 `app_secret`、以及
/// **失败关闭**要认出来的密文列名。
///
/// `Debug` 手写脱敏（两个 secret 字段只打印"非空与否"）。
#[derive(Clone, Default, Deserialize)]
pub struct StreamInstallConfig {
    /// `AppKey`（= 路由键；上游把它明文放在 `config->>'app_id'`）。
    #[serde(default)]
    pub app_id: String,
    /// 显式 robot code（上游 `robot_code`；Stream 机器人下它等于 `app_id`）。
    #[serde(default)]
    pub robot_code: String,
    /// `secretbox` 密文（base64）；生产形态，**必须**有解密器才能装配。
    #[serde(default)]
    pub app_secret_encrypted: String,
    /// 明文 `AppSecret`：**只**给本地 / 用例（生产形态一律走密文列）。登记在 `docs/32` §19。
    #[serde(default)]
    pub app_secret: String,
}

impl std::fmt::Debug for StreamInstallConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StreamInstallConfig")
            .field("app_id", &self.app_id)
            .field("robot_code", &self.robot_code)
            .field(
                "app_secret_encrypted",
                &redaction_of(&self.app_secret_encrypted),
            )
            .field("app_secret", &redaction_of(&self.app_secret))
            .finish()
    }
}

/// 只报告"这个凭据字段**有没有**"，绝不打印它的值。
fn redaction_of(value: &str) -> &'static str {
    if value.is_empty() {
        "<empty>"
    } else {
        "<redacted>"
    }
}

impl StreamInstallConfig {
    /// robot code（上游 `robotCodeOrAppID`：显式值优先，退到 `app_id`）。
    #[must_use]
    pub fn robot_code_or_app_id(&self) -> &str {
        if self.robot_code.is_empty() {
            &self.app_id
        } else {
            &self.robot_code
        }
    }
}

/// 密文解密函数（宿主交进来的那个）。
pub type DecryptFn = dyn Fn(&str) -> Result<String, String> + Send + Sync;

/// 密文解密接缝（上游 `ChannelDeps.Decrypt`）。
///
/// 与 `slack::config::Decrypter` / `telegram::config::Decrypter` 同形（本 crate 的**第三份**；
/// 收敛不在本片写集 —— 见 [`AppSecret`] 的注释与 `docs/32` §19 的 D 项）。
#[derive(Clone)]
pub struct Decrypter {
    inner: Arc<DecryptFn>,
    label: &'static str,
}

impl std::fmt::Debug for Decrypter {
    /// 手写脱敏：函数值不可打印，只打印**类别**（生产排查要能区分接的是哪个解密器）。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Decrypter")
            .field("inner", &"<fn>")
            .field("label", &self.label)
            .finish()
    }
}

impl Decrypter {
    /// 就位构造（`label` 只用于诊断）。
    #[must_use]
    pub fn new(label: &'static str, decrypt: Arc<DecryptFn>) -> Self {
        Self {
            inner: decrypt,
            label,
        }
    }

    /// 一个**总是拒绝**的解密器（失败关闭的默认值：`register()` 用它）。
    #[must_use]
    pub fn fail_closed() -> Self {
        Self::new(
            "fail-closed",
            Arc::new(|_ciphertext: &str| {
                Err("no credential decrypter wired for this process".to_string())
            }),
        )
    }

    /// 解密；失败 ⇒ **不带密文**的配置错误（`docs/60` §2.3 第 3 条）。
    ///
    /// # Errors
    ///
    /// 解密器报错 ⇒ [`ChannelError::InvalidConfig`]。
    pub fn decrypt(&self, ciphertext: &str) -> ChannelResult<String> {
        (self.inner)(ciphertext).map_err(|error| ChannelError::InvalidConfig {
            kind: TYPE_DINGTALK.as_str().to_string(),
            reason: format!("decrypt app secret: {error}"),
        })
    }

    /// 解密器类别（诊断）。
    #[must_use]
    pub fn label(&self) -> &'static str {
        self.label
    }
}

/// 工厂关闭需要的三件共享件（上游 `ChannelDeps`）。
#[derive(Clone)]
pub struct DingTalkDeps {
    /// 密文解密器；[`register`] 用失败关闭的那一个，[`register_with`] 用宿主交的这一个。
    pub decrypt: Decrypter,
    /// 连接引导端口（生产 = `reqwest`）。
    pub opener: Arc<dyn ConnectionOpener>,
    /// 拨号端口（生产 = `tokio-tungstenite`）。
    pub dialer: Arc<dyn WsDialer>,
    /// bot 名来源（M7-9；默认 [`NoBotName`] = 失败关闭）。
    pub bot_names: Arc<dyn BotNameSource>,
    /// 时间旋钮（生产默认 = 上游的 30s / 90s / 10s）。
    pub knobs: StreamKnobs,
    /// 队列旋钮（生产默认 = 上游的 8 / 256 / 2048 / 120s）。
    pub limits: DispatchLimits,
    /// **出站**端口（M7-8）：OpenAPI 的令牌铸造 + JSON POST。
    pub outbound: Arc<dyn outbound::OpenApiTransport>,
}

impl std::fmt::Debug for DingTalkDeps {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DingTalkDeps")
            .field("decrypt", &self.decrypt)
            .field("opener", &"<dyn ConnectionOpener>")
            .field("dialer", &"<dyn WsDialer>")
            .field("bot_names", &"<dyn BotNameSource>")
            .field("knobs", &self.knobs)
            .field("limits", &self.limits)
            .field("outbound", &"<dyn OpenApiTransport>")
            .finish()
    }
}

impl Default for DingTalkDeps {
    /// 生产默认值（**但凭据面失败关闭**：`register()` 的形态）。
    fn default() -> Self {
        Self {
            decrypt: Decrypter::fail_closed(),
            opener: Arc::new(ReqwestOpener::new()),
            dialer: Arc::new(TungsteniteDialer),
            bot_names: Arc::new(NoBotName),
            knobs: StreamKnobs::default(),
            limits: DispatchLimits::default(),
            outbound: Arc::new(outbound::HttpOpenApi::new()),
        }
    }
}

impl DingTalkDeps {
    /// 接上真正的凭据解密器（宿主把部署密钥交给它）。
    #[must_use]
    pub fn with_decrypter(mut self, decrypt: Decrypter) -> Self {
        self.decrypt = decrypt;
        self
    }

    /// 接上 bot 名来源（M7-9 的 `bot_identity.go` 面）。
    #[must_use]
    pub fn with_bot_names(mut self, bot_names: Arc<dyn BotNameSource>) -> Self {
        self.bot_names = bot_names;
        self
    }

    /// 换时间旋钮（用例用）。
    #[must_use]
    pub fn with_knobs(mut self, knobs: StreamKnobs) -> Self {
        self.knobs = knobs;
        self
    }

    /// 换队列旋钮（用例用）。
    #[must_use]
    pub fn with_limits(mut self, limits: DispatchLimits) -> Self {
        self.limits = limits;
        self
    }

    /// 换出站端口（用例注入替身；生产一般不动）。
    #[must_use]
    pub fn with_outbound(mut self, outbound: Arc<dyn outbound::OpenApiTransport>) -> Self {
        self.outbound = outbound;
        self
    }
}

/// 从安装配置解出 Stream 连接要用的明文 `AppSecret`。
///
/// 三条分支（**按上游的优先级**）：
/// 1. `app_secret_encrypted` 非空 ⇒ **必须**有可用解密器（失败关闭的默认值在这里把带密文的
///    安装行拒在装配期，而不是把密文当明文用）；
/// 2. 否则用明文 `app_secret`（本片的本地 / 用例形态，登记在 `docs/32` §19）；
/// 3. 都没有 ⇒ 配置错误。
///
/// 出站面（M7-8 的 `outbound.rs`）**复用同一条**判据 —— 上游
/// `decodeCredentials`（`config.go`）也只有这一份 ⇒ 这里 `pub(crate)`，不各自再写一遍。
pub(crate) fn resolve_app_secret(
    config: &StreamInstallConfig,
    decrypt: &Decrypter,
) -> ChannelResult<AppSecret> {
    if !config.app_secret_encrypted.is_empty() {
        let plaintext = decrypt.decrypt(&config.app_secret_encrypted)?;
        if plaintext.is_empty() {
            return Err(ChannelError::InvalidConfig {
                kind: TYPE_DINGTALK.as_str().to_string(),
                reason: "decrypted app secret is empty".to_string(),
            });
        }
        return Ok(AppSecret::new(plaintext));
    }
    if config.app_secret.is_empty() {
        return Err(ChannelError::InvalidConfig {
            kind: TYPE_DINGTALK.as_str().to_string(),
            reason: "installation has no app secret".to_string(),
        });
    }
    Ok(AppSecret::new(config.app_secret.clone()))
}

// =====================================================================
// 工厂
// =====================================================================

/// 造本平台工厂（上游 `newDingTalkFactory`）。
///
/// 工厂**校验**配置并返回 `Err`，而不是交出半成品（[`crate::channel::Factory`] 的契约）：
/// 配置解不开、`app_id` 为空、凭据缺失 / 解不开 —— 四类都在这里拒掉。
///
/// 队列槽注册表由工厂**持有**（上游 `newDingTalkFactory` 里 `newDispatchSlotRegistry()` 的
/// 位置）⇒ 重连复用同一条队列。需要检查注册表的用例走 [`factory_with_slots`]。
#[must_use]
pub fn factory(deps: &DingTalkDeps) -> Factory {
    let slots = Arc::new(DispatchSlotRegistry::with_limits(deps.limits));
    factory_with_slots(deps, slots)
}

/// 造工厂，并复用调用方给的队列槽注册表（上游 `newDingTalkFactoryWithRegistry`）。
#[must_use]
pub fn factory_with_slots(deps: &DingTalkDeps, slots: Arc<DispatchSlotRegistry>) -> Factory {
    let deps = deps.clone();
    Arc::new(move |config: ChannelConfig| {
        let cfg: StreamInstallConfig =
            serde_json::from_value(config.raw.clone()).map_err(|error| {
                ChannelError::InvalidConfig {
                    kind: TYPE_DINGTALK.as_str().to_string(),
                    reason: format!("decode installation config failed at {error}"),
                }
            })?;
        if cfg.app_id.is_empty() {
            return Err(ChannelError::InvalidConfig {
                kind: TYPE_DINGTALK.as_str().to_string(),
                reason: "installation has no app_id".to_string(),
            });
        }
        let secret = resolve_app_secret(&cfg, &deps.decrypt)?;
        let handler = config.handler.clone();
        // 队列按 AppKey 复用：重连不会让"重连前的在飞轮次"与"重连后的新轮次"并发
        // （上游 `dispatchSlot` 的唯一目的）。
        let dispatcher = slots.acquire(
            &cfg.app_id,
            Arc::new(CallbackJobHandler {
                app_key: cfg.app_id.clone(),
                handler: handler.clone(),
                bot_names: Arc::clone(&deps.bot_names),
            }),
        );
        let channel = DingTalkChannel::new(
            cfg.app_id.clone(),
            secret,
            handler,
            Arc::clone(&deps.opener),
            Arc::clone(&deps.dialer),
            dispatcher,
            Arc::clone(&slots),
        )
        .with_knobs(deps.knobs)
        // 出站端口（M7-8）：工厂**注入**它，于是 `Channel::send` 在宿主径上真的能发；
        // 直接走 `DingTalkChannel::new` 的用例不注入 ⇒ 那条路径失败关闭。
        .with_outbound(Arc::clone(&deps.outbound))
        .with_robot_code(cfg.robot_code_or_app_id());
        Ok(Arc::new(channel) as Arc<dyn Channel>)
    })
}

/// 工厂的显式解密器形态。
#[must_use]
pub fn factory_with_decrypter(decrypt: Decrypter) -> Factory {
    factory(&DingTalkDeps::default().with_decrypter(decrypt))
}

// =====================================================================
// 注册面
// =====================================================================

/// 把本平台的工厂注册进 `registry`（**失败关闭**的凭据面，见模块文档的接线一节）。
///
/// 签名里的两个实参就是 adapter 能拿到的全部外部世界：一个共享注册表 + 一个 port 袋。
pub fn register(registry: &Registry, _deps: &ChannelDeps) {
    tracing::warn!(
        "dingtalk: registering the factory without a credential decrypter; encrypted installation \
         app secrets will be refused at build time (call `mc_channel::dingtalk::register_with` with \
         the deployment key to wire it)"
    );
    registry.register(TYPE_DINGTALK, factory(&DingTalkDeps::default()));
}

/// 接线好的注册入口（宿主把部署密钥与 M7-9 的 bot 名来源交进来）。
pub fn register_with(registry: &Registry, deps: &DingTalkDeps) {
    registry.register(TYPE_DINGTALK, factory(deps));
}

/// 把本平台的解析器集合注册进 router（与 `slack::register_resolvers` 同款）。
///
/// 入站流水线（归一化之后的那一半）走这里接上；宿主装配仍是交接项
/// （`apps/mc-server/src/channels.rs` 属 anchor 写集）。
pub fn register_resolvers(router: &crate::engine::Router, set: resolvers::DingTalkResolverSet) {
    router.register(TYPE_DINGTALK, set.into_engine_set());
}

/// 注册表工厂的**失败关闭**形态（显式给出，便于测试与自检断言"就是它"）。
#[must_use]
pub fn fail_closed_deps() -> DingTalkDeps {
    DingTalkDeps::default()
}

/// 本 adapter 的平台判别式（诊断 / 注册用）。
#[must_use]
pub fn kind() -> ChannelKind {
    TYPE_DINGTALK
}

/// `/issue` 的来源标签（**逐字** `dingtalk_chat`）。
#[must_use]
pub fn origin_type() -> &'static str {
    ORIGIN_DINGTALK_CHAT
}

#[cfg(test)]
mod tests;
