//! `WeCom` 对"机器人听见你了"的回答（上游 `internal/integrations/wecom/typing_indicator.go`，
//! **978 行**）。
//!
//! - **写者**：M7-20（`LUM-1785` / `docs/60-M7-PLAN.md` §3.3）。
//!
//! # 为什么这个平台的"打字指示"是**一条回答**
//!
//! 上游逐字：`Slack` 在那个人的消息上盖一个 👀，`Feishu` 盖一个 `Typing` 徽标。`WeCom` **两样都没有**：
//! 智能机器人协议不公布任何表情回应、没有已读回执、也**没有**打字信号。它**有**的是流式消息，所以
//! 同一个 `engine.TypingNotifier` 接口在这里意味着**另一件事** —— 指示器**就是**那条回答，早开、
//! 后填。两条后果随之而来，两条都塑造了这个文件：
//!
//! - **气泡必须被关掉。** 一个永不清除的表情回应只是不整洁；一条**永不完结**的流是一个**永远**留在
//!   用户聊天里的转圈。所以一次轮次能结束的**每一种**方式 —— 被回答、失败、被取消、从没开始 —— 都要在
//!   还有一个气泡时往它里面写一帧**带着可见文字**的收尾帧。
//! - **`OnSettled` 不是正常的结束方式。** 与别的平台一样，`Router` 只在 flush **没有**产出任务时调它；
//!   回答是从 `outbound.rs` 里的 chat-done 订阅者封掉气泡的，而那是**唯一**手上有那条用来收尾的答案的
//!   地方。
//!
//! # 一个气泡在等哪一次 run，是**从线上**知道的
//!
//! 上游逐字：引擎只告诉这个 notifier"一条消息被 ingest 了"、别的什么都不说 —— 不说去抖器一起收了哪
//! 几条、也不说那次 flush 造出了哪个 task —— 所以 run **自己**在每一条入队路径**已经**在发布的
//! `task:queued` 上宣告自己。一个气泡与"在它是那个等着的轮次时、为它的会话排队的那个 run"配对，而从
//! 那个绑定开始，此后每一个结束都按 task id 匹配。另一条路是让引擎为一个平台的好处把两个事实穿过整个
//! `Router` 带下来。
//!
//! **本仓的形态差异（登记 `docs/32` §38 的 D7）**：本仓**没有**进程内事件总线（M7-17 的 §34.4 H1
//! 逐字），所以那三个入口是**显式方法**（[`TypingIndicator::handle_task_queued`] 等），而事件落成一个
//! **值**（[`ports::TaskEvent`]）。上游那三个订阅读的字段逐条落在它上面。
//!
//! # 气泡是一个**缓存**，仅此而已
//!
//! 上游逐字（见 `stream_store.rs`）：找到一个气泡的收尾器写进它；找不到的那一个，在那些话**值得说**
//! 的时候把它们当一条普通消息说出去（一次失败的告知），在不值得的时候保持沉默。**一个不见了的气泡上
//! 谁也不欠谁。**
//!
//! # 每一轮**两帧**，不多
//!
//! 开场那一帧（画出转圈）与收尾那一帧（用答案替换它）。流自己的十分钟窗口**从开场帧算起**，中间写的
//! 任何东西都**不**延长它（`stream_store.rs` 的 `STREAM_MAX_AGE`）⇒ 跑得比它久的一轮丢掉它的气泡、
//! 以一条普通消息回答 —— **一条**消息，完整的一条。一边跑一边往气泡里填内容、以及把一次长运行接到一条
//! 新流上，是这个层**之上**的另一个层。

use std::sync::Arc;

use mc_core::channel::message::InboundMessage;
use mc_core::id::Id;

use super::outbound::DeliveryLookup;
use super::relay::NoticeRouter;
use super::resolvers::wecom_msg_from_raw;
use super::stream_store::{OpenVerdict, RootResolver, StreamHandle, StreamStore};
use super::strings::{copy_for, locale_for_destination};
use super::ws_frame::{
    aibot_chat_type_from_channel, new_stream_id, CHAT_TYPE_SINGLE_INT, STREAM_THINKING_PLACEHOLDER,
};
use crate::engine::resolvers::{ResolvedInstallation, TypingNotifier};

mod closing;
mod events;
pub mod ports;

pub use closing::{RoundSenders, FALLBACK_SEND_BUDGET};
pub use events::TASK_LOOKUP_TIMEOUT;
pub use ports::{
    failure_text, origin_of, task_failed_content, DeploymentLanguage, LanguageLookup,
    OriginVerdict, TaskEvent, TaskLookupRoots, TaskQueries, TaskRouting, TASK_FAILED_PREFIX,
};

// =====================================================================
// 管理器
// =====================================================================

/// 一个"每次 ingest 一个流式气泡"、并且拥有它直到有什么东西把它关掉的管理器
/// （上游 `TypingIndicatorManager`）。
///
/// 六个可选端口各对应上游的一个字段；**每一个都可以缺席**，而缺席时的行为逐条写在各自
/// `with_*` 的文档里 —— 这正是"没有配置"必须是**可观测**而不是"悄悄什么都不做"的原因。
#[derive(Default)]
pub struct TypingIndicator {
    /// 收尾要的发送面（上游 `senders *sendersRegistry`）。
    senders: Option<Arc<dyn RoundSenders>>,
    /// 轮次表（上游 `streams *streamStore`）。
    streams: Option<Arc<StreamStore>>,
    /// 一次 run 的输入来自哪里（上游 `tasks taskOrigin`）。`None` ⇒ 一次失败的 run 的告知被**拒绝**
    /// 而不是被宣告（见 [`ports::origin_of`] 的"不确定不是许可"）。
    tasks: Option<Arc<dyn TaskQueries>>,
    /// 一次没有气泡的失败该往哪个聊说（上游 `deliveries deliveryLookup`）：与回答**同一个**投递行。
    /// `None` ⇒ 失败告知只限本进程**握着气泡**的那些轮次。
    deliveries: Option<Arc<dyn DeliveryLookup>>,
    /// 一条气泡用哪种语言收尾（上游 `languages languageLookup`）。`None` ⇒ 一律部署语言。
    languages: Option<Arc<dyn LanguageLookup>>,
    /// 把一次结束交给握着 socket 的副本（上游 `relay noticeRouter`）。`None` ⇒ 单副本行为不变。
    relay: Option<Arc<dyn NoticeRouter>>,
    /// 自动重试的血缘查询（上游 `taskLookup` 的 `ChatInputTaskID`，由 [`TaskLookupRoots`] 实现）。
    roots: Option<Arc<dyn RootResolver>>,
}

impl std::fmt::Debug for TypingIndicator {
    /// 手写：六个端口只报**存在性**（与 `ChannelDeps` / `Outbound` 同款）。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TypingIndicator")
            .field("senders", &self.senders.is_some())
            .field("streams", &self.streams.is_some())
            .field("tasks", &self.tasks.is_some())
            .field("deliveries", &self.deliveries.is_some())
            .field("languages", &self.languages.is_some())
            .field("relay", &self.relay.is_some())
            .field("roots", &self.roots.is_some())
            .finish()
    }
}

impl TypingIndicator {
    /// 一个**什么都没配**的管理器（每一个入口都是 no-op；用例从一个明确的空开始）。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 挂上发送面与轮次表（这两个是上游的必需项：缺任一个，`OnIngested` 直接返回）。
    #[must_use]
    pub fn with_streams(mut self, streams: Arc<StreamStore>) -> Self {
        self.streams = Some(streams);
        self
    }

    /// 挂上发送面（上游 `TypingIndicatorConfig.Senders`）。
    #[must_use]
    pub fn with_senders(mut self, senders: Arc<dyn RoundSenders>) -> Self {
        self.senders = Some(senders);
        self
    }

    /// 挂上 task 面（上游 `TypingIndicatorConfig.Tasks`）。
    #[must_use]
    pub fn with_tasks(mut self, tasks: Arc<dyn TaskQueries>) -> Self {
        self.tasks = Some(tasks);
        self
    }

    /// 挂上投递面（上游 `TypingIndicatorConfig.Deliveries`）。
    #[must_use]
    pub fn with_deliveries(mut self, deliveries: Arc<dyn DeliveryLookup>) -> Self {
        self.deliveries = Some(deliveries);
        self
    }

    /// 挂上语言面（上游 `TypingIndicatorConfig.Languages`）。
    #[must_use]
    pub fn with_languages(mut self, languages: Arc<dyn LanguageLookup>) -> Self {
        self.languages = Some(languages);
        self
    }

    /// 挂上中继（上游 `WithRelay`）：让一次运行的结束从**不握着**这条安装 socket 的副本到达提问者。
    ///
    /// 没有它，告知只在这次 run 恰好结束在**握着租约的那个副本**上时送达 —— 而租约保证只有一个副本
    /// 如此，所以在任何多副本部署上那是一次**抛硬币**，不是一个边角。上游 main 正是为此把告知按普通的
    /// `relayKindReply` 路由；气泡把告知从回答的路径上拿了下来，却把路由落在了后面。
    #[must_use]
    pub fn with_relay(mut self, relay: Arc<dyn NoticeRouter>) -> Self {
        self.relay = Some(relay);
        self
    }

    /// 挂上血缘查询（上游 `roundTaker` 里的 `taskLookup`）。
    #[must_use]
    pub fn with_roots(mut self, roots: Arc<dyn RootResolver>) -> Self {
        self.roots = Some(roots);
        self
    }

    /// 六个端口各自配没配（诊断与用例用）。
    #[must_use]
    pub fn wired(&self) -> Wired {
        Wired {
            senders: self.senders.is_some(),
            streams: self.streams.is_some(),
            tasks: self.tasks.is_some(),
            deliveries: self.deliveries.is_some(),
            languages: self.languages.is_some(),
            relay: self.relay.is_some(),
            roots: self.roots.is_some(),
        }
    }

    // =================================================================
    // 同步接缝（上游 `engine.TypingNotifier` 的两个方法）
    // =================================================================

    /// 上游 `OnIngested`：为这条消息所属的那一轮画一个"正在处理"的气泡，并记下回来填它需要什么。
    ///
    /// 一个在**某一轮还在等它的 run** 时到达的消息**加入**那一轮；没有轮次时它立刻开自己的一个气泡，
    /// 因为**屏幕上什么都没有的等待读起来就是一条丢了**。气泡在等待时不带任何文字：那个 think 标签
    /// 渲染成客户端自己的动画点，那**就是**回执，而文字需要先有一个语言才能说。
    ///
    /// `Router` 在一个**脱离的** goroutine 上带着它自己的截止时刻调它，所以这里没有哪一件需要为 ack
    /// 而快 —— 但**这里的一切都是尽力而为**：一个开不出来的气泡只让用户多几秒不确定，而回答仍然以一条
    /// 普通消息到达。
    ///
    /// # 本仓的形态差异
    ///
    /// 上游的 `ctx` 属于那个脱离的 goroutine（`Router` 的回复超时兜住它）。本仓的端口
    /// （[`crate::engine::resolvers::TypingNotifier`]）是**同步**的 ⇒ 这里只推一个脱离任务，真正的工作
    /// 在 [`TypingIndicator::on_ingested_now`]（与 `lark::typing` 的 `add` / `add_now`、
    /// `dingtalk::ack` 的 `on_ingested` / `on_ingested_now` 逐字同款）。
    pub async fn on_ingested_now(
        &self,
        installation: &ResolvedInstallation,
        message: &InboundMessage,
        session_id: Id,
    ) {
        let (Some(senders), Some(streams)) = (self.senders.as_ref(), self.streams.as_ref()) else {
            return;
        };
        // 一条独立的 `/issue` 由 replier 回答，并且**刻意**从不触发一次 agent run ⇒ 永远不会有一个
        // chat-done 事件来关掉为它开的气泡。
        if message.skip_agent_run {
            return;
        }
        let raw = match wecom_msg_from_raw(message) {
            Ok(raw) => raw,
            Err(error) => {
                tracing::warn!(
                    chat_session_id = %session_id,
                    error = %error,
                    "wecom typing: cannot read the inbound envelope"
                );
                return;
            }
        };
        // 没有回调的 `req_id` 就没有流可开：服务端会拒掉任何别的值，而一个**事件回调**的 req_id 会被
        // 直接拒（846605）。
        if raw.req_id.is_empty() {
            return;
        }
        let chat_id = if message.source.chat_id.is_empty() {
            raw.chat_id.clone()
        } else {
            message.source.chat_id.clone()
        };
        if chat_id.is_empty() {
            return;
        }

        let chat_type = aibot_chat_type_from_channel(message.source.chat_type);
        let handle = StreamHandle {
            req_id: raw.req_id.clone(),
            stream_id: new_stream_id(),
            installation_id: Some(installation.id),
            chat_id,
            chat_type,
            // **在这里**解出来，趁提问的人还在手上：每一个收尾器都是**之后**从一个命名了 task、
            // 不命名别人的事件上跑的，离这个 goroutine 已经过去几分钟。
            locale: self.locale_for(installation.id, chat_type, &message.source.sender_id),
            // `Instant` 没有零值 ⇒ 调用方必须给一个真实时刻（M7-16 的 D8）。
            created_at: std::time::Instant::now(),
        };
        // `Instant` 计时的那条：本仓的 `open` 不接受"零时刻"（`stream_store.rs` 的 D8）。
        let (seq, verdict) = streams.open(session_id, handle.clone());
        if verdict != OpenVerdict::Opened {
            // `Joined`：已经有一轮在屏幕上、而且在等那个会回答**这条**消息的 run，而那个气泡就是这条
            // 消息的回执。再开一个就是一个谁也关不掉的气泡。
            return;
        }

        // 开场帧落不下来的三条路，而只有一条是放弃这个气泡的理由（上游逐字）。
        //
        // 一个**没回来的 ack** 对"帧到没到"什么都没说：稍后重发同一个 stream id 会在开场帧真的丢了时
        // 把那条消息建出来，所以最坏情况是一个**没有转圈**地等待的用户，而不是一个永远拿不到回答的人。
        //
        // **Busy 与 Superseded** 说的是更强的事：两者都说这个 `req_id` 上的**另一帧**先到了 socket，
        // 而那一帧带着它**自己**的 stream id —— 那正是气泡被建出来的方式。所以转圈**在用户屏幕上**。
        // 在那里丢掉句柄会留下一个**再没有东西能关掉**的东西。
        //
        // **来自服务端的判决**才是结束它的那一格：846605 / 846608 意味着这条流永远接不下一帧，所以
        // 没有气泡被画出来，而留着句柄会**吞掉**那条回答而不是投递它。
        if let Err(error) = senders
            .stream(&handle, STREAM_THINKING_PLACEHOLDER, false)
            .await
        {
            match error {
                super::ws_sender::SenderError::StreamAckTimeout
                | super::ws_sender::SenderError::StreamBusy
                | super::ws_sender::SenderError::StreamSuperseded
                | super::ws_sender::SenderError::WriteAttempted { .. } => {
                    tracing::debug!(
                        chat_session_id = %session_id,
                        error = %error,
                        "wecom typing: opening frame did not land, keeping the handle"
                    );
                }
                other => {
                    streams.drop_round(session_id, seq);
                    tracing::warn!(
                        chat_session_id = %session_id,
                        error = %other,
                        "wecom typing: opening frame refused"
                    );
                    return;
                }
            }
        }
        // 过了每一条把句柄还回去的路。一个气泡在屏幕上，或者会在它的 stream id 下一次被写时在 ——
        // 而从这里开始有东西欠它一个收尾（见 [`RoundSenders::record_opened`]）。
        senders.record_opened();
    }

    /// 上游 `OnSettled`：关掉一个**从没变成 run** 的轮次的气泡（agent 离线 / 归档，或者一次失败的
    /// 入队）。
    ///
    /// 这是停下那个转圈的**唯一**机会：没有 task 就没有任务生命周期事件，于是 chat-done 订阅者与失败
    /// 订阅者**都**永远不会开火。文案刻意很薄，因为 replier 自己的告知会作为**另一条**消息带着原因
    /// 跟上来。
    ///
    /// 它关掉那个会话里**最老的、从没变成 run** 的轮次 —— 那正是这次落定的 flush 在回答的那一个：
    /// 排在它前面的一轮有自己的 run 与自己的结束会来，排在它后面的是更晚一个问题的。
    ///
    /// 没有气泡就什么都不用说：replier 的告知是用户被告知的全部，而没有轮次可以再收一条话。
    pub async fn on_settled_now(&self, session_id: Id) {
        let (Some(_senders), Some(streams)) = (self.senders.as_ref(), self.streams.as_ref()) else {
            return;
        };
        let Some(turn) = streams.take_oldest_unbound(session_id) else {
            return;
        };
        if !turn.has_bubble {
            return;
        }
        let text = copy_for(turn.handle.locale).stream_not_started.to_string();
        self.write_closing(session_id, &turn.handle, &text, "settled")
            .await;
    }

    /// 一个目的地该用哪种语言（上游 `localeFor` 的参数表；本片把它收成一个方法）。
    fn locale_for(
        &self,
        installation_id: Id,
        chat_type: i32,
        sender_id: &str,
    ) -> super::strings::Locale {
        match self.languages.as_ref() {
            Some(languages) => languages.locale_for(installation_id, chat_type, sender_id),
            // 没配语言面 ⇒ 部署语言（对群聊等价，对 1:1 降级而不是错误）。
            None => locale_for_destination(chat_type == CHAT_TYPE_SINGLE_INT, None),
        }
    }

    /// 本进程在**任何地方**有没有东西在册（上游 `m.streams.holding()` 的那个问题）。
    #[must_use]
    pub fn holding(&self) -> bool {
        self.streams
            .as_ref()
            .is_some_and(|streams| streams.holding())
    }

    /// 屏幕上开了几个气泡（诊断与用例用；上游 `depth`）。
    #[must_use]
    pub fn depth(&self) -> usize {
        self.streams.as_ref().map_or(0, |streams| streams.depth())
    }
}

/// 六个端口各配没配（诊断与看板用；上游 `TypingIndicatorConfig` 的"可为 nil"那几格）。
///
/// 七格布尔是**刻意**的：每一格对应上游一个可为 nil 的字段，而"哪几个配了"正是宿主接线时要看的
/// 那**一件事**。换成一个状态机或几个二值枚举只会把这张表拆成几个更难核对的类型。
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wired {
    pub senders: bool,
    pub streams: bool,
    pub tasks: bool,
    pub deliveries: bool,
    pub languages: bool,
    pub relay: bool,
    pub roots: bool,
}

/// 一台**同步**的 `TypingNotifier`，把两个接缝推给脱离任务（上游那两个方法直接阻塞着跑）。
///
/// 与 `dingtalk::ack::AckNotifier` / `lark::typing::TypingIndicatorManager` 同款：真正的实现在
/// `*_now` 里，于是用例可以直接 `await` 完整路径，不必睡真觉。没有 async 运行时（一个纯单元用例）
/// 时它**只记一条 warn**、绝不 panic —— 那是这两个同步接缝的纪律（`dingtalk/ack/tests.rs` 逐字）。
pub struct DetachedTypingNotifier {
    inner: Arc<TypingIndicator>,
}

impl std::fmt::Debug for DetachedTypingNotifier {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DetachedTypingNotifier")
            .field("wired", &self.inner.wired())
            .finish()
    }
}

impl DetachedTypingNotifier {
    /// 把一个管理器包成引擎那个同步接缝。
    #[must_use]
    pub fn new(inner: Arc<TypingIndicator>) -> Self {
        Self { inner }
    }

    /// 里面那个管理器（宿主与用例用）。
    #[must_use]
    pub fn indicator(&self) -> &Arc<TypingIndicator> {
        &self.inner
    }
}

/// 把一个 future 推到脱离任务上（有运行时 ⇒ 起任务；没有 ⇒ 记一条 warn，不 panic）。
fn spawn_detached<F>(future: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::spawn(future);
    } else {
        tracing::warn!("wecom typing: no async runtime; skipping the detached notifier task");
    }
}

impl TypingNotifier for DetachedTypingNotifier {
    fn on_ingested(
        &self,
        installation: &ResolvedInstallation,
        message: &InboundMessage,
        session_id: Id,
    ) {
        let inner = Arc::clone(&self.inner);
        let installation = installation.clone();
        let message = message.clone();
        spawn_detached(async move {
            inner
                .on_ingested_now(&installation, &message, session_id)
                .await;
        });
    }

    fn on_settled(&self, session_id: Id) {
        let inner = Arc::clone(&self.inner);
        spawn_detached(async move {
            inner.on_settled_now(session_id).await;
        });
    }
}
#[cfg(test)]
mod tests;
