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
pub mod binding;
pub mod client;
pub mod config;
pub mod dispatch;
pub mod emotion;
pub mod group_identity;
pub mod inbound;
pub mod install;
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

mod channel;
pub use channel::*;

// =====================================================================
// 配置与凭据（本片的**收窄**形态）
// =====================================================================

mod deps;
pub use deps::*;

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
