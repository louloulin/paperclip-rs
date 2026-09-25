//! `DingTalk` **表情回执（ACK）面**：入站受理时贴「收到」、任务终态时撤回、
//! 回复真正投递后才贴「Done」（上游 `internal/integrations/dingtalk/ack.go` 220 行
//! + `ack_batch.go` 160 行）。
//!
//! - **写者**：M7-8（`docs/60-M7-PLAN.md` §3.3；写集勘误见 `docs/32` §22）。
//! - **为什么是"表情"而不是"打字指示"**：`DingTalk` 没有 typing indicator，平台的内置表情
//!   就是"我在处理"的等价物（上游 `emotion.go` 的两个 enum 值）。本文件因此实现的是 engine 的
//!   [`TypingNotifier`] 接缝，而两个表情的平台规则在 [`crate::dingtalk::emotion`]。
//!
//! # 三条语义（上游注释逐字，别合并）
//!
//! 1. **凭据只有本地受理过的那一条**：提供方坐标（会话 id + 消息 id）**只**来自
//!    [`ReplySourceCache`]（受理时记下的），**从不**从投递快照里反推。重启 / 跨进程事件 /
//!    缓存淘汰 ⇒ 那一条输入的 Done 被**抑制**（不是错误）。
//! 2. **永远不因表情失败而拒收输入、也永远不因它而挡回复**：表情失败只记一条 warn。
//! 3. **Done 与「收到」是两件事**：「收到」在受理时贴（乐观），Done 只在**所有**答案分片都
//!    落地之后贴（由出站侧调 [`AckNotifier::on_reply_delivered`]）。
//!
//! # 批次的判据是**持久化的输入所有权**，不是第二个去抖计时器（上游 `ack_batch.go`）
//!
//! 同一个上下文里**未密封**的输入算一批；密封（`task_id` 落地）之后，`task_id` 把这一批和
//! 同一会话里的下一批分开。所以撤「收到」的时机由"这批输入都终态了"决定，而不是猜。
//!
//! # 与上游的形态差异（**逐条登记** `docs/32` §22）
//!
//! 1. **同步接缝 + 脱离任务**：本仓的 [`TypingNotifier`] 是同步方法（engine 的调用点绝不阻塞），
//!    所以 [`AckNotifier::on_ingested`] 只推一个 `tokio::spawn` 就返回；真正的流程在
//!    `on_ingested_now`（用例可以直接 `await` 它，不必睡真觉 —— 与 M7-5/M7-6 同一先例）。
//! 2. **三处"端口化"**：上游直接拿生成的 `db.Queries` 读 `chat_message`、拿 `*Client` 贴表情。
//!    本仓的 adapter 不直接写 DB（`docs/60` §2.6 第 1 条）⇒ 这里定义
//!    [`ReactionInputQueries`] 与 [`ReactionSender`] 两个端口，PG / HTTP 的实现是薄适配。
//! 3. **没有 `OnReplyDelivered` / `onAgentArchived` 的本地驱动**（本仓没有那条进程内事件总线）
//!    ⇒ 这两个入口保留为**显式调用**（同 M7-6 对 telegram `outbound.go` 的先例）。
//!
//! # 凭据面
//!
//! 本文件**没有**凭据字段：`AppSecret` 只在 [`SenderReaction`] 解凭据的那一瞬存在。
//! 任何 `tracing::*` 都不插值凭据；错误变体也不带。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use mc_core::channel::message::InboundMessage;
use mc_core::id::Id;
use mc_repos::chat_message::ChatMessageRow;

use crate::dingtalk::emotion::{Emotion, EmotionError};
use crate::dingtalk::outbound::spawn_detached;
use crate::dingtalk::outbound::{
    decode_credentials, Credentials, OpenApiTransport, ReplySource, ReplySourceCache, SendTarget,
    Sender,
};
use crate::dingtalk::resolvers::installation_row;
use crate::dingtalk::Decrypter;
use crate::engine::resolvers::{EngineResult, ResolvedInstallation, TypingNotifier};

/// 收尾（撤回 / 记账）的预算（上游 `ackCleanupTimeout = 5s`）。
pub const ACK_CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);

// =====================================================================
// 端口
// =====================================================================

/// 读一条 `chat_message`（上游 `reactionInputQueries.GetChatMessage`）。
#[async_trait]
pub trait ReactionInputQueries: Send + Sync {
    /// 取一行；不存在 ⇒ `Ok(None)`。
    ///
    /// # Errors
    ///
    /// 链路失败（调用方按"拿不到就不动表情"处理，而不是把输入判失败）。
    async fn get_chat_message(&self, id: Id) -> EngineResult<Option<ChatMessageRow>>;
}

/// 贴 / 撤一次表情（上游 `ackNotifier.sendReaction` 那个可替换的函数值）。
///
/// 抽成端口是为了让用例能**只**钉住批次 / 状态机语义，而不必起 HTTP 服务端；生产实现是
/// [`SenderReaction`]。
#[async_trait]
pub trait ReactionSender: Send + Sync {
    /// 对 `target` 指向的源消息贴 / 撤 `emotion`。
    ///
    /// # Errors
    ///
    /// 见 [`EmotionError`]（调用方只记 warn）。
    async fn react(
        &self,
        installation: &ResolvedInstallation,
        target: &SendTarget,
        emotion: Emotion,
        recall: bool,
    ) -> Result<(), EmotionError>;
}

/// 生产实现：从安装行解凭据，走 [`Sender::set_emoji_reaction`]。
pub struct SenderReaction {
    transport: Arc<dyn OpenApiTransport>,
    decrypt: Decrypter,
}

impl std::fmt::Debug for SenderReaction {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SenderReaction")
            .field("transport", &"<dyn OpenApiTransport>")
            .field("decrypt", &self.decrypt)
            .finish()
    }
}

impl SenderReaction {
    /// 装配。
    #[must_use]
    pub fn new(transport: Arc<dyn OpenApiTransport>, decrypt: Decrypter) -> Self {
        Self { transport, decrypt }
    }

    /// 从安装行解出凭据（解不出 ⇒ 一条故障类错误，**不含**密文与明文）。
    fn credentials(
        &self,
        installation: &ResolvedInstallation,
    ) -> Result<Credentials, EmotionError> {
        let Some(row) = installation_row(installation) else {
            return Err(EmotionError::Transport {
                message: "installation platform row unavailable".to_string(),
            });
        };
        decode_credentials(&row.config, &self.decrypt).map_err(|_| EmotionError::Transport {
            message: "installation credentials unavailable".to_string(),
        })
    }
}

#[async_trait]
impl ReactionSender for SenderReaction {
    async fn react(
        &self,
        installation: &ResolvedInstallation,
        target: &SendTarget,
        emotion: Emotion,
        recall: bool,
    ) -> Result<(), EmotionError> {
        let credentials = self.credentials(installation)?;
        let sender = Sender::from_credentials(Arc::clone(&self.transport), &credentials);
        sender.set_emoji_reaction(target, emotion, recall).await
    }
}

// =====================================================================
// 状态
// =====================================================================

/// 一条输入的回执句柄（上游 `ackState`）。
///
/// 两个 `bool` 用原子表达：上游在 `mu` 下改它们，而**所有 I/O 都在锁外** ⇒ 这里用原子让
/// "锁外读一次"这条纪律在类型层面成立（不需要为读一个 bool 去抢全局锁）。
#[derive(Debug)]
struct AckHandle {
    installation: ResolvedInstallation,
    /// 反应坐标（会话 id + 源消息 id）——**不**保留回调正文。
    target: SendTarget,
    /// 被受理输入的主键（`chat_message.id`）；受理时反查不到 ⇒ `None`（那就不做批次分类）。
    input_id: Option<Id>,
    session_id: Id,
    settled: AtomicBool,
    attempted: AtomicBool,
}

impl AckHandle {
    fn is_settled(&self) -> bool {
        self.settled.load(Ordering::SeqCst)
    }

    fn settle(&self) {
        self.settled.store(true, Ordering::SeqCst);
    }

    fn was_attempted(&self) -> bool {
        self.attempted.load(Ordering::SeqCst)
    }

    fn mark_attempted(&self) {
        self.attempted.store(true, Ordering::SeqCst);
    }
}

/// `session key`（上游用 `util.UUIDToString(sessionID)`）。
fn session_key(session_id: Id) -> String {
    session_id.to_string()
}

/// 已终态输入的**有界**围栏（上游 `settledInputs`）：让一条"迟到且还没开始"的受理钩子
/// 无法把已经撤掉的表情重新贴上。
#[derive(Debug, Default)]
struct SettledInputs {
    ids: HashSet<Id>,
    order: Vec<Id>,
    next: usize,
}

impl SettledInputs {
    fn remember(&mut self, id: Id) {
        if self.ids.contains(&id) {
            return;
        }
        if self.order.len() == crate::dingtalk::outbound::MAX_REPLY_SOURCES {
            let victim = self.order[self.next];
            self.ids.remove(&victim);
            self.order[self.next] = id;
            self.next = (self.next + 1) % crate::dingtalk::outbound::MAX_REPLY_SOURCES;
        } else {
            self.order.push(id);
        }
        self.ids.insert(id);
    }

    fn contains(&self, id: Option<Id>) -> bool {
        id.is_some_and(|id| self.ids.contains(&id))
    }
}

/// 活动句柄表（`session key` → 这一批的句柄）。
#[derive(Debug, Default)]
struct ActiveMap {
    by_session: HashMap<String, Vec<Arc<AckHandle>>>,
    settled: SettledInputs,
}

// =====================================================================
// 通知器
// =====================================================================

/// 表情回执的生命周期所有者（上游 `ackNotifier`）。
///
/// 三个"所有者"分别是：本结构（活动句柄）、[`ReplySourceCache`]（提供方坐标）、
/// 端口（DB / HTTP）。它们共同保证"Done 只在真的投递过之后才可能出现"。
pub struct AckNotifier {
    inner: Arc<AckInner>,
}

struct AckInner {
    /// 提供方坐标缓存（**唯一**来源；上游的 `Client.sources`）。
    sources: ReplySourceCache,
    inputs: Option<Arc<dyn ReactionInputQueries>>,
    reaction: Arc<dyn ReactionSender>,
    active: Mutex<ActiveMap>,
}

impl std::fmt::Debug for AckNotifier {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AckNotifier")
            .field("sources", &self.inner.sources)
            .field("has_inputs", &self.inner.inputs.is_some())
            .field("reaction", &"<dyn ReactionSender>")
            .finish_non_exhaustive()
    }
}

impl Clone for AckNotifier {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl AckNotifier {
    /// 装配。
    ///
    /// `inputs` 为 `None` ⇒ 跳过后端的批次分类（只按会话分组）——用例与"没有输入读面"的
    /// 部署形态都能跑；上游的 `n.inputs != nil` 分支就是这个语义。
    #[must_use]
    pub fn new(
        reaction: Arc<dyn ReactionSender>,
        inputs: Option<Arc<dyn ReactionInputQueries>>,
    ) -> Self {
        Self {
            inner: Arc::new(AckInner {
                sources: ReplySourceCache::new(),
                inputs,
                reaction,
                active: Mutex::new(ActiveMap::default()),
            }),
        }
    }

    /// 生产形态（`SenderReaction` + 给定输入读面）。
    #[must_use]
    pub fn http(
        transport: Arc<dyn OpenApiTransport>,
        decrypt: Decrypter,
        inputs: Option<Arc<dyn ReactionInputQueries>>,
    ) -> Self {
        Self::new(Arc::new(SenderReaction::new(transport, decrypt)), inputs)
    }

    /// 换掉提供方坐标缓存（宿主与出站侧**共享同一个**缓存：Done 只认它记下的坐标）。
    #[must_use]
    pub fn with_sources(mut self, sources: ReplySourceCache) -> Self {
        self.inner = Arc::new(AckInner {
            sources,
            inputs: self.inner.inputs.clone(),
            reaction: Arc::clone(&self.inner.reaction),
            active: Mutex::new(ActiveMap::default()),
        });
        self
    }

    /// 提供方坐标缓存（出站侧要在投递成功时**先**记住才能贴 Done）。
    #[must_use]
    pub fn sources(&self) -> &ReplySourceCache {
        &self.inner.sources
    }

    /// 记一条被受理输入的提供方坐标（上游 `rememberReplySource`）。
    pub fn remember_source(&self, source: ReplySource) {
        self.inner.sources.remember(source);
    }

    /// 本会话还有回执工作要做吗（终态事件据此决定要不要进来）。
    ///
    /// 活动回执可以活得比源缓存久 ⇒ 两个所有者都要看（上游逐字）。
    #[must_use]
    pub fn has_session(&self, session_id: Id) -> bool {
        if self.inner.sources.has_session(session_id) {
            return true;
        }
        let key = session_key(session_id);
        self.inner.active.lock().is_ok_and(|active| {
            active
                .by_session
                .get(&key)
                .is_some_and(|states| !states.is_empty())
        })
    }

    /// 活动句柄数（诊断 / 用例）。
    #[must_use]
    pub fn active_count(&self, session_id: Id) -> usize {
        self.inner.active.lock().map_or(0, |active| {
            active
                .by_session
                .get(&session_key(session_id))
                .map_or(0, Vec::len)
        })
    }

    // -----------------------------------------------------------------
    // 入口：同步接缝 + 显式调用
    // -----------------------------------------------------------------

    /// 受理成功 ⇒ 贴「收到」（上游 `OnIngested`）。
    ///
    /// 同步方法：只推一个脱离任务（见模块文档差异 1）。
    pub fn ingest(
        &self,
        installation: &ResolvedInstallation,
        message: &InboundMessage,
        session_id: Id,
    ) {
        let notifier = self.clone();
        let (installation, message) = (installation.clone(), message.clone());
        spawn_detached(async move {
            notifier
                .ingest_now(&installation, &message, session_id)
                .await;
        });
    }

    /// [`AckNotifier::ingest`] 的可 `await` 形态（用例直接跑完整路径，不睡真觉）。
    pub async fn ingest_now(
        &self,
        installation: &ResolvedInstallation,
        message: &InboundMessage,
        session_id: Id,
    ) {
        if message.message_id.is_empty() || message.source.chat_id.is_empty() {
            return;
        }
        // 提交前登记兴趣：在飞引用与保留引用一起决定"这个会话还有没有回执工作"。
        let release = self.inner.sources.begin_input(session_id);
        let key = session_key(session_id);
        let handle = Arc::new(AckHandle {
            installation: installation.clone(),
            target: SendTarget::reaction_from_message(message),
            input_id: self.inner.sources.input_for(installation.id, message),
            session_id,
            settled: AtomicBool::new(false),
            attempted: AtomicBool::new(false),
        });
        let (superseded, accepted) = self.prepare_ingest(&key, &handle).await;
        release();
        if !accepted {
            return;
        }
        self.recall(&superseded).await;
        if handle.is_settled() {
            return;
        }
        handle.mark_attempted();
        self.send_reaction(&handle, Emotion::Acknowledged, false)
            .await;
        // 撤「收到」可能在这条贴表情在飞时赢 ⇒ 给这次已完成的贴一次有界撤回。
        if handle.is_settled() {
            self.recall(std::slice::from_ref(&handle)).await;
        }
    }

    /// 失败冲刷钩子 ⇒ 清会话（上游 `OnSettled`）。
    ///
    /// 这个钩子只带会话 id，**不**区分重叠的失败 / 待处理代际 ⇒ 它**从不**把输入标记成 Done。
    pub async fn settled_now(&self, session_id: Id) {
        let key = session_key(session_id);
        let states = {
            let Ok(mut active) = self.inner.active.lock() else {
                return;
            };
            let states = active.by_session.remove(&key).unwrap_or_default();
            for state in &states {
                state.settle();
            }
            states
        };
        self.recall(&states).await;
    }

    /// 终态事件只退休它**自己那一批**的密封输入（上游 `onInputsSettled`）。
    pub async fn inputs_settled_now(&self, session_id: Id, inputs: &[ChatMessageRow]) {
        let ids: Vec<Id> = inputs
            .iter()
            .filter(|input| input.channel_ingested)
            .map(|input| Id(input.id))
            .collect();
        let key = session_key(session_id);
        let mut settled = Vec::new();
        {
            let Ok(mut active) = self.inner.active.lock() else {
                return;
            };
            for id in &ids {
                active.settled.remember(*id);
            }
            let Some(states) = active.by_session.remove(&key) else {
                return;
            };
            let mut remaining = Vec::new();
            for state in states {
                if state.input_id.is_some_and(|id| ids.contains(&id)) {
                    state.settle();
                    settled.push(state);
                } else {
                    remaining.push(state);
                }
            }
            if !remaining.is_empty() {
                active.by_session.insert(key, remaining);
            }
        }
        self.recall(&settled).await;
    }

    /// agent 归档 ⇒ 不需要任务也能退休本地登记过的回执（上游 `onAgentArchived`）。
    pub async fn agent_archived_now(&self, agent_id: Id) {
        let mut retired = Vec::new();
        {
            let Ok(mut active) = self.inner.active.lock() else {
                return;
            };
            // 拆字段借用：`settled` 与 `by_session` 是两块，互不重叠。
            let ActiveMap {
                by_session,
                settled,
            } = &mut *active;
            let mut empty_keys = Vec::new();
            for (key, states) in by_session.iter_mut() {
                let mut remaining = Vec::new();
                for state in states.drain(..) {
                    if state.installation.agent_id == agent_id {
                        state.settle();
                        if let Some(input_id) = state.input_id {
                            settled.remember(input_id);
                        }
                        retired.push(state);
                    } else {
                        remaining.push(state);
                    }
                }
                *states = remaining;
                if states.is_empty() {
                    empty_keys.push(key.clone());
                }
            }
            for key in empty_keys {
                by_session.remove(&key);
            }
        }
        self.recall(&retired).await;
    }

    /// 回复**所有**分片都落地之后贴 Done（上游 `OnReplyDelivered`）。
    ///
    /// 只认本地受理时记下的提供方坐标：重启 / 缓存未命中 ⇒ 跳过（不是错误）。
    pub async fn on_reply_delivered(&self, installation: &ResolvedInstallation, input_id: Id) {
        let Some(source) = self.inner.sources.source_for(installation.id, input_id) else {
            return;
        };
        let handle = AckHandle {
            installation: installation.clone(),
            target: source.reaction_target(),
            input_id: Some(input_id),
            session_id: source.session_id,
            settled: AtomicBool::new(false),
            attempted: AtomicBool::new(true),
        };
        self.send_reaction(&handle, Emotion::Done, false).await;
    }

    // -----------------------------------------------------------------
    // 内部
    // -----------------------------------------------------------------

    /// 贴 / 撤一次表情，**带** [`ACK_CLEANUP_TIMEOUT`] 预算（上游的 cleanup ctx）。
    ///
    /// 返回是否成功；失败（含超时）只记一条 warn —— 表情**永不**拒收输入，也**永不**挡回复。
    async fn react_bounded(&self, handle: &AckHandle, emotion: Emotion, recall: bool) -> bool {
        let call = self
            .inner
            .reaction
            .react(&handle.installation, &handle.target, emotion, recall);
        match tokio::time::timeout(ACK_CLEANUP_TIMEOUT, call).await {
            Ok(Ok(())) => true,
            Ok(Err(error)) => {
                tracing::warn!(
                    message_id = handle.target.source_message_id,
                    recall,
                    emotion = emotion.platform_name(),
                    error = %error,
                    "dingtalk reaction failed"
                );
                false
            }
            Err(_elapsed) => {
                tracing::warn!(
                    message_id = handle.target.source_message_id,
                    recall,
                    "dingtalk reaction timed out"
                );
                false
            }
        }
    }

    /// 贴一次（结果只影响日志）。
    async fn send_reaction(&self, handle: &AckHandle, emotion: Emotion, recall: bool) {
        let _ = self.react_bounded(handle, emotion, recall).await;
    }

    /// 撤回一组（上游 `recallStates`）：给"还没贴过"的那条免费放行。
    async fn recall(&self, states: &[Arc<AckHandle>]) {
        for state in states {
            if !state.was_attempted()
                || self.react_bounded(state, Emotion::Acknowledged, true).await
            {
                self.remove_recalled(state);
            }
        }
    }

    /// 从活动表里摘掉一条**已终态**的句柄。
    fn remove_recalled(&self, state: &Arc<AckHandle>) {
        let key = session_key(state.session_id);
        let Ok(mut active) = self.inner.active.lock() else {
            return;
        };
        let Some(states) = active.by_session.get_mut(&key) else {
            return;
        };
        if let Some(position) = states
            .iter()
            .position(|current| Arc::ptr_eq(current, state) && state.is_settled())
        {
            states.remove(position);
            if states.is_empty() {
                active.by_session.remove(&key);
            }
        }
    }

    /// 批次的分类与登记（上游 `prepareIngest`）：所有 I/O 都在锁外。
    ///
    /// 返回"被这一条取代的旧句柄"与"是否受理"（`false` 不改任何可见锚点）。
    async fn prepare_ingest(
        &self,
        key: &str,
        state: &Arc<AckHandle>,
    ) -> (Vec<Arc<AckHandle>>, bool) {
        loop {
            let Some((previous, candidates)) = self.take_batch(key, state) else {
                return (Vec::new(), false);
            };
            let superseded = match self.classify(state, &candidates).await {
                Classification::Reject => return (Vec::new(), false),
                // 密封在两次读之间提交过 ⇒ 重新分类（改了可见锚点会更糟）。
                Classification::Retry => continue,
                Classification::Accept(superseded) => superseded,
            };

            let Ok(mut active) = self.inner.active.lock() else {
                return (Vec::new(), false);
            };
            if active.settled.contains(state.input_id) {
                return (Vec::new(), false);
            }
            let unchanged = active
                .by_session
                .get(key)
                .is_none_or(|current| same_handles(current, &previous));
            if !unchanged {
                // 表在我们做 I/O 时动过 ⇒ 重新分类。
                continue;
            }
            for handle in &superseded {
                handle.settle();
            }
            active
                .by_session
                .entry(key.to_string())
                .or_default()
                .push(Arc::clone(state));
            return (superseded, true);
        }
    }

    /// 锁内的一步：取当前批次、判重复、算同安装的候选；`None` = 直接拒。
    fn take_batch(&self, key: &str, state: &AckHandle) -> Option<BatchView> {
        let active = self.inner.active.lock().ok()?;
        if active.settled.contains(state.input_id) {
            return None;
        }
        let previous = active.by_session.get(key).cloned().unwrap_or_default();
        let mut candidates = Vec::new();
        for existing in &previous {
            if existing.installation.id != state.installation.id {
                continue;
            }
            if existing.target.source_message_id == state.target.source_message_id {
                // 同一个源消息重复受理 ⇒ 短路（上游逐字）。
                return None;
            }
            candidates.push(Arc::clone(existing));
        }
        Some((previous, candidates))
    }

    /// 读面驱动的批次分类（上游 `prepareIngest` 的 `n.inputs != nil` 分支）。
    async fn classify(
        &self,
        state: &Arc<AckHandle>,
        candidates: &[Arc<AckHandle>],
    ) -> Classification {
        let Some(inputs) = &self.inner.inputs else {
            return Classification::Accept(Vec::new());
        };
        let Some(input_id) = state.input_id else {
            return Classification::Reject;
        };
        let Ok(Some(current)) = inputs.get_chat_message(input_id).await else {
            tracing::warn!(
                message_id = state.target.source_message_id,
                "dingtalk reaction: accepted input unavailable"
            );
            return Classification::Reject;
        };
        if !current.channel_ingested
            || current.role != "user"
            || Id(current.chat_session_id) != state.session_id
        {
            tracing::warn!(
                message_id = state.target.source_message_id,
                "dingtalk reaction: accepted input unusable"
            );
            return Classification::Reject;
        }
        let mut older = false;
        let mut superseded = Vec::new();
        for candidate in candidates {
            let Some(candidate_input_id) = candidate.input_id else {
                continue;
            };
            let Ok(Some(input)) = inputs.get_chat_message(candidate_input_id).await else {
                tracing::warn!(
                    message_id = state.target.source_message_id,
                    "dingtalk reaction: batch input unavailable"
                );
                return Classification::Reject;
            };
            if !same_reaction_batch(&current, &input) {
                continue;
            }
            // 与 `ListChatInputMessages` 的排序（含 id 决胜）对齐。
            if input.created_at > current.created_at
                || (input.created_at == current.created_at && input.id > current.id)
            {
                older = true;
            }
            superseded.push(Arc::clone(candidate));
        }
        let Ok(Some(latest)) = inputs.get_chat_message(input_id).await else {
            return Classification::Reject;
        };
        if latest.task_id != current.task_id
            || latest.channel_context_revision != current.channel_context_revision
        {
            return Classification::Retry;
        }
        if older {
            return Classification::Reject;
        }
        Classification::Accept(superseded)
    }
}

/// 现役批次 + 同安装的候选（上游 `prepareIngest` 在锁内取的那两样）。
type BatchView = (Vec<Arc<AckHandle>>, Vec<Arc<AckHandle>>);

/// [`AckNotifier::classify`] 的三态。
#[derive(Debug)]
enum Classification {
    /// 受理，并取代这些旧句柄。
    Accept(Vec<Arc<AckHandle>>),
    /// 拒绝（重复 / 归属不符 / 读面失败 / 被更晚的输入压过 / 反查不到输入 id）。
    Reject,
    /// 密封在两次读之间提交过 ⇒ 重新分类。
    Retry,
}

/// 两个句柄列表是否**逐元素**相同（上游 `slices.Equal`）。
fn same_handles(current: &[Arc<AckHandle>], previous: &[Arc<AckHandle>]) -> bool {
    current.len() == previous.len()
        && current
            .iter()
            .zip(previous)
            .all(|(left, right)| Arc::ptr_eq(left, right))
}

impl TypingNotifier for AckNotifier {
    /// engine 的同步接缝（`docs/60` §2.6 第 5 条）：推一个脱离任务就返回。
    fn on_ingested(
        &self,
        installation: &ResolvedInstallation,
        message: &InboundMessage,
        session_id: Id,
    ) {
        self.ingest(installation, message, session_id);
    }

    /// 会话的 run 触发没有产出任务 ⇒ 清除回执（幂等）。
    fn on_settled(&self, session_id: Id) {
        let notifier = self.clone();
        spawn_detached(async move {
            notifier.settled_now(session_id).await;
        });
    }
}

pub mod batch;

pub use batch::{same_reaction_batch, NoReactionInputs};
#[cfg(test)]
mod tests;
