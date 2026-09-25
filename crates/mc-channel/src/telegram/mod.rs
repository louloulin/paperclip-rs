//! Telegram adapter（上游 `internal/integrations/telegram`（23 文件 / 12 非测试 / 4,504 上游行））。
//!
//! **状态：M7-5 已落「入站回路 + 安装与绑定面」**（`LUM-1770`）—— 本文件承载
//! **每安装一条的 `getUpdates` 长轮询回路**、工厂与注册面；归一化在 [`inbound`]、传输在
//! [`api`]、安装/绑定在 [`install`] / [`binding`]、判决回复在 [`replier`]、解析器面在
//! [`resolvers`]。
//!
//! # 这个平台的面
//!
//! - **入站 = `getUpdates` 长轮询**（50s 服务端挂起）：Telegram **没有** WebSocket 传输，
//!   长轮询就是"每安装一条持久连接"的等价物，由 engine 的 `Supervisor` 逐安装监管
//!   （与 Feishu 的 WS 长连接 / Slack 的 Socket Mode 同形）。**单消费者约束**：同一条
//!   `getUpdates` 流的第二个消费者会拿到 **409**，那正对应 Supervisor 的"每安装至多一条
//!   活跃回路"保证（[`api::ApiError::Conflict`]）；
//! - BYO 安装（3 条 workspace 路由 + `/api/telegram/binding/redeem`）—— [`install`] / [`binding`]；
//! - 判决回复（绑定卡 / 离线告知 / `/issue` 确认）—— [`replier`]；
//! - 解析器面（安装路由 / 身份 / 去重 / 会话 / 审计 / 打字指示）—— [`resolvers`]；
//! - 出站发送（Markdown → HTML / 分片 / 流式编辑 / 投递状态机）—— **归 M7-6**，见下。
//!
//! # 本目录的写者表（M7-5 / M7-6）
//!
//! | 文件 | 写者 | 上游 |
//! | --- | :-: | --- |
//! | `config.rs` | **M7-5** | `config.go`（安装配置 + 凭据解密 + bot id 形状） |
//! | `inbound.rs` + `inbound/tests.rs` | **M7-5** | `inbound.go`（更新归一化 + Bot API wire 类型） |
//! | `api.rs` + `api/tests.rs` | **M7-5 落入站/校验/判决半边**；**M7-6 补出站流式半边** | `api.go`（`getMe` / `getWebhookInfo` / `getUpdates` / `sendMessage` / `sendChatAction`） |
//! | `install.rs` + `install/tests.rs` | **M7-5** | `install.go` |
//! | `binding.rs` + `binding/tests.rs` | **M7-5** | `binding.go` |
//! | `resolvers.rs` + `resolvers/tests.rs` | **M7-5** | `resolvers.go` |
//! | `replier.rs` + `replier/tests.rs` | **M7-5** | `replier.go` |
//! | `mod.rs` + `tests.rs` | **M7-5**（长轮询回路 / 工厂 / 注册面） | `telegram_channel.go` |
//! | `outbound.rs` / `delivery.rs` / `sender.rs` / `markdown.rs` | **M7-6** | `outbound.go`（1,633）/ `delivery.go`（457）/ `sender.go`（194）/ `markdown.go`（119） |
//!
//! ## 写集勘误（**逐条登记**，照 M7-3 / M7-4 的先例；全文见 `docs/32` §17.1）
//!
//! `docs/60` §3.3 给 M7-5 的格子是 6 个 `telegram/*.rs` + 路由文件；起手补充已追加本文件
//! （`telegram/mod.rs`：新模块要可见、`register()` 要填、长轮询回路要有家）。本片**再追加**
//! 的路径只有两类：
//!
//! - `telegram/api.rs`：上游 `api.go` 的**入站/校验/判决半边**（本片三处调用都用它，而
//!   M7-6 的硬前置是 M7-5 ⇒ 传输只能是先落的那片写；anchor 的 `Cargo.toml` 注释也已点名
//!   "telegram `api.rs`"）。出站流式半边（`editMessageText` / 429 的一次重试 / HTML
//!   parse mode）留给 **M7-6 在同一文件内扩展**；
//! - 六个 `*/tests.rs` 与 `inbound`/`api`/`install`/`binding`/`resolvers`/`replier` 的子模块：
//!   全是**门 ⑩ 的 800 行硬限**逼出来的切分（不是拆凑数字）。
//!
//! 拆完每个文件都 ≤ 800 行，且**未动** `scripts/file_size_baseline.tsv`（只减不增）。
//!
//! # 注册约定（五个 adapter 一致，别各自发明）
//!
//! - 工厂必须校验 `raw` 配置并返回 `Err`，**不要**交出半成品（[`crate::channel::Factory`] 的契约）；
//! - 部署密钥缺失 ⇒ 该平台**整体不装配**（判据在 `apps/mc-server/src/channels.rs`，
//!   `docs/60` §2.6 第 3 条）。**路由仍然存在**，并按各端点自己的"未配置"语义回响应
//!   （Telegram 列表是 200 空 + `install_supported:false`，**不是**统一 503）；
//! - 一切凭据只经 `mc_secrets::secretbox` 与 `mc-telemetry` 的 redaction 通道（`docs/60` §2.3）；
//! - adapter **不得**直接写 DB：只走 [`crate::engine::ChannelDeps`] 里注入的 port。
//!
//! # 解密器的接线（**交接项**，见 PR 描述与 `docs/32` §17.4）
//!
//! [`register`] 的签名（`&Registry` + `&ChannelDeps`）里**没有**部署密钥的位置 ——
//! `ChannelDeps` 是 M7-1 定死的形态，而密钥的**唯一读取口**是
//! `mc_http::state::ChannelKeys`（`mc-channel` 不得自己 `std::env::var`）。所以：
//!
//! - [`register`]（宿主当前调用的那个）用**失败关闭**的解密器注册工厂：配置里带密文令牌时，
//!   工厂**拒装配**并明说"没接线"，而不是把密文当明文用；同时打一条 `warn`；
//! - `register_with` 是**接线好的**入口：宿主把 `ChannelKeys::get(Telegram)` 交给它即可
//!   （[`TelegramDeps::with_secret_box`]）。
//!
//! # M7-5 的状态：**入站 + 安装/绑定面闭环，出站发送是"最小可用"**
//!
//! | 面 | 状态 |
//! | --- | --- |
//! | `Channel::connect`（入站长轮询） | **已闭环**：真 `getUpdates` + offset 推进 + 409/429/传输失败的三种处置 |
//! | `Channel::send`（出站） | **最小可用**：纯文本 `sendMessage`（判决回复正是这条路径）。Markdown→HTML、分片、流式编辑、投递状态机归 **M7-6** |
//! | 4 条路由（安装 / 绑定） | **已闭环**：`routes/channels/telegram.rs` 自带 PG 端口实现与用例 |
//! | 解析器面（回复器 / 打字指示） | **已闭环**（工厂 + [`resolvers::TelegramResolverSet`]）；宿主的一次装配调用仍是**交接项**（`apps/mc-server/src/channels.rs` 属 anchor 写集） |
//!
//! 这张表就是"缺口登记"的形式：**哪一半闭环、哪一半等谁**，一眼可查（同 §13.5 / §15 的手法）。
//!
//! # offset 的"持久化"语义（本片专属验收）
//!
//! 上游（与 Bots API 的语义）都**不在本地存 offset**：确认消费这件事发生在 **Telegram 的
//! 服务端** —— `getUpdates?offset=N` 一旦发出，`update_id < N` 的更新即在平台侧出队。
//! 于是三件事合起来才是"重启不重复消费、不丢更新"：
//!
//! 1. **每次 `connect` 从 0 开始**：Telegram 会把**全部**尚未确认的更新重投一遍 ⇒ **不丢**；
//! 2. **批内先推进 offset 再逐条投递**：进程在批中途死掉 ⇒ 没被确认的那部分下次重投；
//! 3. **engine 的 `(installation, message_id)` 去重**吸收重投 ⇒ **不重复消费**。
//!
//! ⇒ 本地**不写** offset 表/文件；把 offset 落库反而会引入"落库成功但投递失败"的两难。
//! 这三条各有用例（`tests.rs` 的 `the_polling_loop_advances_offset_after_each_batch` /
//! `a_restart_replays_pending_updates`），合起来就是"不重复消费、不丢更新"的可观察证据。

pub mod api;
pub mod binding;
pub mod config;
pub mod delivery;
pub mod inbound;
pub mod install;
pub mod markdown;
pub mod outbound;
pub mod replier;
pub mod resolvers;
pub mod sender;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mc_core::channel::message::{
    ChatType, InboundMessage, MessageKind, OutboundMessage, SendResult,
};
use mc_core::channel::ChannelKind;

use crate::capability::Capability;
use crate::channel::{Channel, ChannelConfig, ChannelError, ChannelResult, Factory};
use crate::engine::ChannelDeps;
use crate::message::SharedInboundHandler;
use crate::registry::Registry;
use api::SendMessage;
use config::{parse_stored_bot_id, Decrypter, Sensitive, TelegramDeps};
use inbound::{inbound_from_update, parse_message_ref, Update, TYPE_TELEGRAM};
use replier::{is_addressed_issue_command, UNSUPPORTED_TYPE_TEXT};

pub use api::{JsonBotApi, TelegramApi, WebhookInfo};

/// 一次瞬态 `getUpdates` 失败之后、把错误交给 Supervisor 退避之前的分隔（上游
/// `pollRetryDelay = 2 * time.Second`）。
pub const POLL_RETRY_DELAY: Duration = Duration::from_secs(2);

/// "被寻址的 `/issue` 派发失败"告知的超时（上游 `issueErrorReplyTimeout = 5 * time.Second`）。
pub const ISSUE_ERROR_REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// `/issue` 派发失败时的告知文案（上游 `issueDispatchFailedText`，逐字）。
pub const ISSUE_DISPATCH_FAILED_TEXT: &str =
    "⚠️ I couldn't create that issue because an internal error occurred. Please try again.";

/// 把一段 future 推到脱离任务上（有运行时 ⇒ 起任务；没有 ⇒ 打一条 warn）。
///
/// engine 的两个**同步**接缝（[`crate::engine::OutboundReplier`] 与
/// [`crate::engine::TypingNotifier`]）与入站回路的"礼貌告知"都用它 —— 于是引擎的调用点
/// 绝不阻塞在 Telegram HTTP 上（`docs/60` §2.6 第 5 条）。
pub(crate) fn spawn_detached<F>(future: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::spawn(future);
    } else {
        tracing::warn!("telegram: no async runtime; skipping the detached task");
    }
}

// =====================================================================
// 一条安装的轮询回路（上游 `telegramChannel`）
// =====================================================================

/// **一条**安装的 `getUpdates` 长轮询回路。
///
/// Telegram 没有 WebSocket 传输；长轮询就是"持久连接"的等价物 —— 每个 bot token 一条阻塞
/// 回路，由 `engine.Supervisor` 逐安装监管（与 Feishu 的 WS 长连接 / Slack 的 Socket Mode
/// 完全同形）。单消费者约束（Telegram 对第二个 `getUpdates` 消费者回 **409**）与
/// Supervisor 的"跨副本至多一条活跃回路"保证一一对应。
pub struct TelegramChannel {
    /// bot 的数值 id（路由键 `config->>'app_id'` 的数值形态）。
    bot_id: i64,
    /// bot 用户名（`@-mention` 判定要用）。
    bot_username: String,
    /// 明文 bot token（**手写脱敏**类型；绝不出现在日志 / `Debug` 里）。
    bot_token: Sensitive,
    api: Arc<dyn TelegramApi>,
    handler: Option<SharedInboundHandler>,
    /// 瞬态失败后的分隔（用例调到 0 以免睡真觉）。
    retry_delay: Duration,
}

impl std::fmt::Debug for TelegramChannel {
    /// 手写脱敏：令牌字段只说明**配没配**（`docs/60` §2.3 第 1 条）。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TelegramChannel")
            .field("bot_id", &self.bot_id)
            .field("bot_username", &self.bot_username)
            .field("bot_token", &self.bot_token)
            .field("api", &"<dyn TelegramApi>")
            .field("handler", &self.handler.is_some())
            .field("retry_delay", &self.retry_delay)
            .finish()
    }
}

impl TelegramChannel {
    /// 装配。
    #[must_use]
    pub fn new(
        bot_id: i64,
        bot_username: impl Into<String>,
        bot_token: Sensitive,
        api: Arc<dyn TelegramApi>,
        handler: Option<SharedInboundHandler>,
    ) -> Self {
        Self {
            bot_id,
            bot_username: bot_username.into(),
            bot_token,
            api,
            handler,
            retry_delay: POLL_RETRY_DELAY,
        }
    }

    /// 注入瞬态失败的分隔（用例用）。
    #[must_use]
    pub fn with_retry_delay(mut self, delay: Duration) -> Self {
        self.retry_delay = delay;
        self
    }

    /// 归一化 + 投递一条更新（上游 `telegramChannel.dispatch`）。
    ///
    /// - 认不出 / 非文本 / 空正文 ⇒ 返回 `Ok(())`（**产品性丢弃不是错误**）；
    /// - 非文本但**在跟 bot 互动**（私聊，或群里被 @）⇒ 先回一条"暂不支持"的告知；
    /// - handler 返回 `Err` ⇒ **基础设施失败**，向 Supervisor 冒泡（它退避重连，未推进的
    ///   更新会重投，由去重吸收）。
    async fn dispatch(&self, update: &Update) -> ChannelResult<()> {
        let Some(handler) = self.handler.clone() else {
            return Err(ChannelError::Transport {
                message: "telegram: inbound handler not configured".to_string(),
            });
        };
        let Some(message) = inbound_from_update(update, self.bot_id, &self.bot_username) else {
            return Ok(());
        };
        if message.kind != MessageKind::Text {
            if message.source.chat_type == ChatType::P2p || message.addressed_to_bot {
                self.notify_unsupported(update);
            }
            return Ok(());
        }
        if message.text.is_empty() {
            return Ok(());
        }
        if let Err(error) = handler.handle(message.clone()).await {
            self.notify_issue_dispatch_error(&message);
            return Err(error);
        }
        Ok(())
    }

    /// 礼貌告知"这类消息还不支持"（上游 `notifyUnsupported`）：保留话题路由、引用触发消息。
    ///
    /// 尽力而为，**脱离**接收循环（它不该拖慢下一次 `getUpdates`）。
    fn notify_unsupported(&self, update: &Update) {
        let Some(message) = update.message.as_deref() else {
            return;
        };
        let token = self.bot_token.clone();
        let api = Arc::clone(&self.api);
        let params = SendMessage::text(message.chat.id, UNSUPPORTED_TYPE_TEXT)
            .in_thread(message.message_thread_id)
            .with_reply_to(message.message_id);
        spawn_detached(async move {
            if let Err(error) = api.send_message(token.expose(), &params).await {
                tracing::warn!(
                    "telegram: unsupported-type notice failed ({})",
                    error.method()
                );
            }
        });
    }

    /// 被寻址的 `/issue` 在 engine 里**基础设施性失败**时回一条告知（上游
    /// `notifyIssueDispatchError`）。
    ///
    /// 脱离轮询回路（Supervisor 不该为了这条尽力而为的告知而等待）。token 从安装的配置里
    /// **已经解好**（工厂只在本结构里放过一次），所以这里不需要解密器。
    fn notify_issue_dispatch_error(&self, message: &InboundMessage) {
        if !is_addressed_issue_command(message) {
            return;
        }
        let Ok(chat_id) = message.source.chat_id.parse::<i64>() else {
            tracing::warn!("telegram: issue dispatch-error reply has invalid chat id");
            return;
        };
        let thread_id = message.source.thread_id.parse::<i64>().unwrap_or(0);
        let token = self.bot_token.clone();
        let api = Arc::clone(&self.api);
        let params = SendMessage::text(chat_id, ISSUE_DISPATCH_FAILED_TEXT)
            .in_thread(thread_id)
            .with_reply_to(parse_message_ref(&message.message_id));
        spawn_detached(async move {
            if let Err(error) = api.send_message(token.expose(), &params).await {
                tracing::warn!(
                    "telegram: issue dispatch-error reply failed ({})",
                    error.method()
                );
            }
        });
    }
}

#[async_trait]
impl Channel for TelegramChannel {
    fn kind(&self) -> ChannelKind {
        TYPE_TELEGRAM
    }

    /// 建连并跑 `getUpdates` 接收循环（上游 `telegramChannel.Connect`）。
    ///
    /// **offset 的语义见模块文档**：每次 `connect` 从 **0** 开始（Telegram 会重投全部未确认
    /// 更新），批内**先推进 offset 再逐条投递**，重投由 engine 的去重吸收。
    ///
    /// 三种失败的处置**各不相同**（上游逐字）：
    ///
    /// - **409 Conflict** ⇒ 每次尝试都是**致命**的：另一个消费者在轮询这个 bot token
    ///   （另一副本 / 另一个 workdir / 外部进程）。退避修不好它，所以回一个准确文案让运维
    ///   去处置；
    /// - **429** ⇒ 按 Telegram 强制的 `retry_after` 睡一次再继续（**不算**这次尝试失败）；
    /// - **其余瞬态** ⇒ 先睡 [`Self::retry_delay`] 一次，再把错误交给 Supervisor 的退避。
    ///
    /// 取消由 Supervisor 侧 `abort` 承载（anchor 的契约：`connect` 的 future 直接被丢弃）。
    async fn connect(&self) -> ChannelResult<()> {
        if self.handler.is_none() {
            return Err(ChannelError::Transport {
                message: "telegram: inbound handler not configured".to_string(),
            });
        }
        if self.bot_token.is_empty() {
            return Err(ChannelError::Transport {
                message: "telegram: bot token not configured".to_string(),
            });
        }
        // 0 = "把全部尚未确认的更新给我"（Telegram 的服务器端 offset 语义）。
        let mut offset: i64 = 0;
        loop {
            let updates = match self.api.get_updates(self.bot_token.expose(), offset).await {
                Ok(updates) => updates,
                Err(error) if error.is_conflict() => {
                    tracing::warn!(
                        bot_id = self.bot_id,
                        "telegram: getUpdates conflict — this bot token is polled by another \
                         instance; stop the other consumer or use a distinct bot per environment"
                    );
                    return Err(ChannelError::Transport {
                        message: "telegram: bot is already being polled by another instance \
                                  (409 conflict)"
                            .to_string(),
                    });
                }
                Err(error) => {
                    if let Some(wait) = error.retry_after() {
                        tracing::warn!(
                            bot_id = self.bot_id,
                            retry_after_secs = wait.as_secs(),
                            "telegram: getUpdates rate limited"
                        );
                        tokio::time::sleep(wait).await;
                        continue;
                    }
                    // 瞬态网络 / API 失败：尝试内先隔一次，免得一次抖动就搅动 Supervisor 的
                    // 退避；持续失败仍然靠反复报错升级（上游逐字）。
                    tracing::warn!(
                        bot_id = self.bot_id,
                        method = error.method(),
                        "telegram: getUpdates failed"
                    );
                    tokio::time::sleep(self.retry_delay).await;
                    return Err(ChannelError::Transport {
                        message: format!("telegram: getUpdates failed ({})", error.method()),
                    });
                }
            };
            for update in updates {
                if update.update_id >= offset {
                    offset = update.update_id + 1;
                }
                self.dispatch(&update).await?;
            }
        }
    }

    /// 拆链路：轮询回路的整个生命周期都关在 `connect` 里（它随任务被 abort 而结束），
    /// 所以这里是空操作（上游 `Disconnect` 逐字：no-op）。
    async fn disconnect(&self) -> ChannelResult<()> {
        Ok(())
    }

    /// 出站：走 M7-6 的**发送器**（上游 `sender.go` 的 `Send` 逐字）。
    ///
    /// 这是本片**唯一**改动的出站替换点（M7-5 的交接第 2 条：判决回复与 agent 答复必须走
    /// **同一条**发送器，上游 `replier.go` 就是这么做）。形态由 [`crate::telegram::sender`]
    /// 负责：**分片（按 UTF-16 码元）→ 每片 Markdown→HTML → 发 → HTML 被拒则回落纯文本**，
    /// 只有第一片引用触发消息，`SendResult` 带**末片**的复合键。
    async fn send(&self, out: OutboundMessage) -> ChannelResult<SendResult> {
        let sender = crate::telegram::sender::Sender::new(Arc::clone(&self.api));
        let frame = sender
            .send(self.bot_token.expose(), &out)
            .await
            .map_err(|error| ChannelError::Transport {
                // 只带方法名 / 目标错误码，**绝不**带 token 或请求 URL（§2.3）。
                message: match &error {
                    crate::telegram::sender::SendError::BadChatId { .. } => {
                        "telegram: outbound chat id is not a number".to_string()
                    }
                    crate::telegram::sender::SendError::Api(api) => {
                        format!("telegram: sendMessage failed ({})", api.method())
                    }
                },
            })?;
        Ok(frame.to_send_result())
    }

    /// 上游 `CapText | CapThreadReply | CapQuoteReply | CapTypingIndicator | CapMessageEdit`。
    ///
    /// 位图是**声明**：`TEXT` / `THREAD_REPLY` / `QUOTE_REPLY` 由本片的最小发送器支撑，
    /// `TYPING_INDICATOR` 由 [`resolvers::TelegramTypingNotifier`] 支撑；`MESSAGE_EDIT`
    /// 的**实现**归 M7-6（`editMessageText` 在 [`api`] 里留给它）。
    fn capabilities(&self) -> Capability {
        Capability::TEXT
            .union(Capability::THREAD_REPLY)
            .union(Capability::QUOTE_REPLY)
            .union(Capability::TYPING_INDICATOR)
            .union(Capability::MESSAGE_EDIT)
    }
}

// =====================================================================
// 工厂
// =====================================================================

/// 造本平台工厂（上游 `newTelegramFactory` / `RegisterTelegram`）。
///
/// 工厂**校验**配置并返回 `Err`，而不是交出半成品（[`crate::channel::Factory`] 的契约）：
/// 配置解不开、密文解不开、令牌为空、`app_id` 不是数值 id —— 四种都在这里拒掉。
#[must_use]
pub fn factory(deps: &TelegramDeps) -> Factory {
    let deps = deps.clone();
    Arc::new(move |config: ChannelConfig| {
        let raw = config.raw.clone();
        let cfg: config::InstallConfig =
            serde_json::from_value(raw).map_err(|error| ChannelError::InvalidConfig {
                kind: TYPE_TELEGRAM.as_str().to_string(),
                reason: format!("decode installation config failed at {error}"),
            })?;
        let token =
            config::decrypt_token(&cfg.bot_token_encrypted, &deps.decrypt).map_err(|error| {
                ChannelError::InvalidConfig {
                    kind: TYPE_TELEGRAM.as_str().to_string(),
                    reason: error.to_string(),
                }
            })?;
        if token.is_empty() {
            return Err(ChannelError::InvalidConfig {
                kind: TYPE_TELEGRAM.as_str().to_string(),
                reason: "installation has no bot token".to_string(),
            });
        }
        let bot_id =
            parse_stored_bot_id(&cfg.app_id).ok_or_else(|| ChannelError::InvalidConfig {
                kind: TYPE_TELEGRAM.as_str().to_string(),
                reason: "installation app_id is not a bot id".to_string(),
            })?;
        Ok(Arc::new(TelegramChannel::new(
            bot_id,
            cfg.bot_username,
            Sensitive::new(token),
            Arc::new(JsonBotApi::new()),
            config.handler,
        )) as Arc<dyn Channel>)
    })
}

/// 工厂的显式解密器形态（[`register`] 的默认值见 [`TelegramDeps::default`]）。
#[must_use]
pub fn factory_with_decrypter(decrypt: Decrypter) -> Factory {
    factory(&TelegramDeps { decrypt })
}

// =====================================================================
// 注册面
// =====================================================================

/// 把本平台的工厂注册进 `registry`（**失败关闭**的解密器，见模块文档的接线一节）。
///
/// 签名里的两个实参就是 adapter 能拿到的全部外部世界：一个共享注册表 + 一个 port 袋。
pub fn register(registry: &Registry, _deps: &ChannelDeps) {
    tracing::warn!(
        "telegram: registering the factory without a credential decrypter; encrypted installation \
         tokens will be refused at build time (call `mc_channel::telegram::register_with` with the \
         deployment key to wire it)"
    );
    registry.register(TYPE_TELEGRAM, factory(&TelegramDeps::default()));
}

/// 接线好的注册入口（宿主把部署密钥交进来；见模块文档的接线一节）。
pub fn register_with(registry: &Registry, deps: &TelegramDeps) {
    registry.register(TYPE_TELEGRAM, factory(deps));
}

/// 注册表工厂的**失败关闭**形态（显式给出，便于测试与自检断言"就是它"）。
#[must_use]
pub fn fail_closed_deps() -> TelegramDeps {
    TelegramDeps::default()
}

/// 本 adapter 的平台判别式（诊断 / 注册用）。
#[must_use]
pub fn kind() -> ChannelKind {
    TYPE_TELEGRAM
}

#[cfg(test)]
mod tests;
