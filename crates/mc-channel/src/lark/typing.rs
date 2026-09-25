//! lark 的"处理中"打字指示：`Typing` 表情的**生命周期**
//! （上游 `internal/integrations/lark/typing_indicator.go`，282 行）。
//!
//! - **写者**：M7-13（`docs/60-M7-PLAN.md` §3.3；写集勘误见 `docs/32` §32）。
//! - **上游定位**：`TypingIndicatorManager` —— 入站消息**成功入库**时给那条消息贴一个
//!   `Typing` 表情；会话的 run 结束时（有回复 / 失败 / 取消）把该会话的表情**全部**撤掉。
//!
//! # 从上游逐字搬来的五条语义（每条都有用例）
//!
//! 1. **贴与撤是两个独立入口**：`Add`（入库成功后）与 `Clear`（run 结束）。`Clear` 是**幂等**的
//!    ——对一个没有跟踪状态的会话调用它是 no-op（上游逐字：*clearing a session with no tracked
//!    state is a no-op*）。
//! 2. **同一条消息贴两次只是多一条状态条目**（上游逐字：*adding a reaction to a message that
//!    already has one simply appends another state entry*）⇒ 内部不是 `Set` 而是 `Vec`，于是
//!    `Clear` 会把两次都撤掉。
//! 3. **太老的消息不贴**（[`TYPING_INDICATOR_MAX_AGE`] = 2 分钟）：WS 重连会重放旧事件，
//!    "正在处理"的徽标出现在早就结束的对话上会误导人。上游注释逐字：*Aligned with `OpenClaw`'s
//!    2-minute bound.*
//! 4. **`installation_id` 在贴的**那一刻**记下来**，因为那是它**一定**能解出的最后时刻：
//!    它可以从会话的渠道绑定行到达，而删除会话会在同一个事务里丢掉那一行，同时把它的取消
//!    发在路上。
//! 5. **撤时优先回查安装行，查不到才用快照**（`installSnapshot`）：运行时的拆除
//!    （`handler/runtime.go` 的 `DeleteChannelInstallationsBySystemRuntimeAgents`）在**同一个
//!    事务**里删掉安装行并取消那些任务 ⇒ 取消到达 `Clear` 时已经没有行可解。上游注释逐字：
//!    *It is a FALLBACK, never the primary. A live lookup picks up a credential rotation between
//!    add and clear; a snapshot cannot, so it is consulted only when the row is genuinely gone.*
//!
//! # 状态里**没有**明文
//!
//! 上游注释逐字：*It does not weaken "no decrypted secret lives in the state map": what is
//! held here is the same encrypted blob the database holds, and `DecryptAppSecret` still runs
//! at clear time.* ⇒ 本仓的状态条目持 **密文**安装投影（[`LarkInstallation`]，它的 `Debug` 只
//! 报密文长度），明文只在一次 HTTP 调用期间存在于 [`InstallationCredentials`] 里。
//!
//! # 本仓的形态差异（登记 `docs/32` §32.1）
//!
//! - **D6 "消息太老"用注入的**墙上**时钟**：上游 `time.Since(time.UnixMilli(ms))` 读的是
//!   墙上时间（`create_time` 是平台给的**纪元毫秒**），与 `super::enricher::Clock` 的**单调**
//!   毫秒不是一回事 ⇒ 本文件的 [`WallClock`] 单独一个口，用例注入 [`ManualWallClock`] 就能钉住
//!   "2 分钟边界"，**不睡真觉**（与 §29 的 D4 同款手法，但**不复用**那个 trait —— 复用会把
//!   "单调"与"纪元"两个语义混在一个类型上）。
//! - **D7 同步端口 + 脱离任务**：[`crate::engine::resolvers::TypingNotifier`] 的两个方法都是
//!   **同步**的（M7-1 定的契约，调用点在 `tokio::spawn` 里），而上游的 `Add` / `Clear` 直接
//!   阻塞着发 HTTP ⇒ 同步方法只把工作推给脱离任务，真正的工作在 async 的
//!   [`TypingIndicatorManager::add_now`] / [`TypingIndicatorManager::clear_now`] 里（与
//!   `slack::replier` 的 `reply` / `reply_now` 逐字同款），于是用例可以直接 `await` 完整路径。
//!   状态表因此必须是 `Arc<Mutex<…>>`（脱离任务与本体共用**同一张**表），**不是**每次
//!   `clone` 一份 —— 那会让"抄近路的 add ↛ 看得见的 clear"。
//! - **D8 上游的 `log` 字段不落**：上游持有 `*slog.Logger`；本仓的日志一律走 `tracing::*`，
//!   **不**把 logger 当字段（全仓一致）。

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use mc_core::channel::message::InboundMessage;
use mc_core::id::Id;

use super::client::ApiClient;
use super::feishu_channel::credentials::Decrypter;
use super::params::{AddReactionParams, DeleteReactionParams, InstallationCredentials};
use super::resolvers::{platform_installation, LarkInstallation};
use crate::engine::resolvers::{ResolvedInstallation, TypingNotifier};

// =====================================================================
// 常量（上游 `typingEmoji` / `typingIndicatorMaxAge`）
// =====================================================================

/// "处理中"用的 Lark `emoji_type`（上游 `typingEmoji`，**逐字**）。
///
/// 它在消息上渲染成一个小小的打字动画徽标。值不能改 —— 改了就只是换了个表情。
pub const TYPING_EMOJI: &str = "Typing";

/// 跳过指示器的消息年龄上限（上游 `typingIndicatorMaxAge`，**逐字** 2 分钟）。
///
/// 上游注释逐字：*This prevents stale reactions when a WebSocket reconnect replays old events.*
pub const TYPING_INDICATOR_MAX_AGE: Duration = Duration::from_mins(2);

/// [`TYPING_INDICATOR_MAX_AGE`] 的毫秒数（判据用的是纪元毫秒，见 [`is_message_too_old`]）。
pub const TYPING_INDICATOR_MAX_AGE_MILLIS: i64 = 120_000;

// =====================================================================
// 墙上时钟（见模块文档差异 D6）
// =====================================================================

/// **纪元毫秒**时钟（"消息太老"的判据要它）。
pub trait WallClock: Send + Sync {
    /// Unix 纪元起的毫秒数。
    fn now_epoch_millis(&self) -> i64;
}

/// 生产时钟（`chrono::Utc::now()` 的毫秒）。
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemWallClock;

impl WallClock for SystemWallClock {
    fn now_epoch_millis(&self) -> i64 {
        chrono::Utc::now().timestamp_millis()
    }
}

/// 用例时钟：可手动推进（`AtomicI64`）。
#[derive(Debug, Default, Clone)]
pub struct ManualWallClock(Arc<AtomicI64>);

impl ManualWallClock {
    /// 从一个纪元毫秒起点造。
    #[must_use]
    pub fn new(start_epoch_millis: i64) -> Self {
        Self(Arc::new(AtomicI64::new(start_epoch_millis)))
    }

    /// 前进 `ms` 毫秒。
    pub fn advance(&self, ms: i64) {
        self.0.fetch_add(ms, Ordering::SeqCst);
    }

    /// 直接置位。
    pub fn set(&self, epoch_millis: i64) {
        self.0.store(epoch_millis, Ordering::SeqCst);
    }
}

impl WallClock for ManualWallClock {
    fn now_epoch_millis(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// 平台给的 `create_time`（**纪元毫秒的十进制字串**）是不是太老（上游 `isMessageTooOld`）。
///
/// 上游逐条的回落：空串 ⇒ `false`（没有时间戳就**不**拦，历史行与替身都走这条）；解析不出来
/// ⇒ `false`（同上）；解析出来才比较。**不**把"解析不了"当"太老" —— 那会让一次平台字段漂移
/// 静默吃掉所有打字指示。
#[must_use]
pub fn is_message_too_old(create_time: &str, now_epoch_millis: i64) -> bool {
    if create_time.is_empty() {
        return false;
    }
    let Ok(millis) = create_time.parse::<i64>() else {
        return false;
    };
    now_epoch_millis.saturating_sub(millis) > TYPING_INDICATOR_MAX_AGE_MILLIS
}

// =====================================================================
// 状态与端口
// =====================================================================

/// 一条已贴出的指示（上游 `TypingIndicatorState`）。
///
/// `installation_id` 与 `installation_snapshot` 的用途见模块文档第 4 / 5 条。
///
/// [`Debug`] 是**手写**的：`installation_snapshot` 是安装投影（它自己脱敏），这里只报
/// `reaction_id` / `message_id` / 安装 id —— 都是平台标识，**没有**凭据。
#[derive(Clone)]
pub struct TypingIndicatorState {
    /// 贴了表情的那条消息。
    pub message_id: String,
    /// Lark 返回的 `reaction_id`（撤它要它）；空 = 这次贴没有拿到 id。
    pub reaction_id: String,
    /// 贴的时候那条消息所属的安装。
    pub installation_id: Id,
    /// 贴的那一刻的安装投影（**密文**形态；只在行真的没了时用）。
    pub installation_snapshot: LarkInstallation,
}

impl fmt::Debug for TypingIndicatorState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TypingIndicatorState")
            .field("message_id", &self.message_id)
            .field("reaction_id", &self.reaction_id)
            .field("installation_id", &self.installation_id)
            .field("has_installation_snapshot", &true)
            .finish_non_exhaustive()
    }
}

/// 撤表情时要的安装行（上游 `TypingIndicatorQueries`）。
#[async_trait]
pub trait TypingIndicatorQueries: Send + Sync {
    /// 按 id 取安装行；没有 ⇒ `Ok(None)`。
    ///
    /// # Errors
    ///
    /// 链路失败（调用方按"这次撤不了"处理，**不**终止循环）。
    async fn installation(&self, id: Id) -> EngineResult<Option<LarkInstallation>>;
}

use crate::engine::resolvers::EngineResult;

// =====================================================================
// 管理器
// =====================================================================

/// 会话 id 字串 → 该会话贴出的全部指示。
///
/// **内层是 `Vec`**（模块文档第 2 条）；外层用 `Arc` 让脱离任务与本体共用**同一张**表。
type StateTable = Arc<Mutex<HashMap<String, Vec<TypingIndicatorState>>>>;

/// "处理中"表情的生命周期管理器（上游 `TypingIndicatorManager`）。
///
/// 并发安全；对缺失 / 陈旧状态**宽容**（见模块文档第 1 / 2 条）。
pub struct TypingIndicatorManager {
    client: Arc<dyn ApiClient>,
    decrypt: Option<Decrypter>,
    queries: Option<Arc<dyn TypingIndicatorQueries>>,
    clock: Arc<dyn WallClock>,
    states: StateTable,
}

impl fmt::Debug for TypingIndicatorManager {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TypingIndicatorManager")
            .field("client", &"<dyn ApiClient>")
            .field("has_decrypter", &self.decrypt.is_some())
            .field("has_queries", &self.queries.is_some())
            .field("clock", &"<dyn WallClock>")
            .field("tracked_sessions", &self.tracked_session_count())
            .finish_non_exhaustive()
    }
}

impl TypingIndicatorManager {
    /// 装配（生产时钟；持有安装行查询）。
    #[must_use]
    pub fn new(
        client: Arc<dyn ApiClient>,
        decrypt: Decrypter,
        queries: Arc<dyn TypingIndicatorQueries>,
    ) -> Self {
        Self {
            client,
            decrypt: Some(decrypt),
            queries: Some(queries),
            clock: Arc::new(SystemWallClock),
            states: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 不接安装行查询的形态（**撤时只能靠快照**）。
    ///
    /// 上游的 `queries` 是必需项；本仓允许它是可选项，好让"快照回落"这一支**单独**可测
    /// （否则它只在运行时拆除那一条罕见路径上被走到）。
    #[must_use]
    pub fn with_snapshot_only(client: Arc<dyn ApiClient>, decrypt: Decrypter) -> Self {
        Self {
            client,
            decrypt: Some(decrypt),
            queries: None,
            clock: Arc::new(SystemWallClock),
            states: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 换时钟（用例注入 [`ManualWallClock`]）。
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn WallClock>) -> Self {
        self.clock = clock;
        self
    }

    /// 丢掉解密器（**失败关闭**：撤时解不开凭据 ⇒ 只记 warn，不回显任何字节）。
    #[must_use]
    pub fn without_decrypter(mut self) -> Self {
        self.decrypt = None;
        self
    }

    /// 当前跟踪着指示的会话数（诊断 / 用例）。
    #[must_use]
    pub fn tracked_session_count(&self) -> usize {
        self.states.lock().map_or(0, |guard| guard.len())
    }

    /// 某个会话跟踪着的指示条数（诊断 / 用例）。
    #[must_use]
    pub fn tracked_reactions(&self, session_id: Id) -> usize {
        self.states
            .lock()
            .ok()
            .and_then(|guard| guard.get(&session_key(session_id)).map(Vec::len))
            .unwrap_or(0)
    }

    /// 现在这个会话跟踪着哪些 `reaction_id`（用例断言"撤的是**这一批**"）。
    #[must_use]
    pub fn tracked_reaction_ids(&self, session_id: Id) -> Vec<String> {
        self.states
            .lock()
            .ok()
            .and_then(|guard| {
                guard.get(&session_key(session_id)).map(|states| {
                    states
                        .iter()
                        .map(|state| state.reaction_id.clone())
                        .collect()
                })
            })
            .unwrap_or_default()
    }

    /// 贴一个指示（上游 `Add` 的实体）。
    ///
    /// 顺序逐字照上游：空 `message_id` 直接返回 → 太老直接返回 → 解凭据 → 贴 → **才**记状态。
    /// 上游注释逐字的那条竞态本仓照样保留：*an ending during `Add` can still clear nothing,
    /// because `Add` records its state only after the Lark call returns* ⇒ 这里也**不**提前记。
    ///
    /// 错误**只记日志不返回**（上游逐字：*Errors are logged and swallowed*）：一次贴表情失败
    /// 不该让入站流水线报错。
    pub async fn add_now(
        &self,
        installation: &LarkInstallation,
        session_id: Id,
        message_id: &str,
        create_time: &str,
    ) {
        if message_id.is_empty() {
            return;
        }
        if is_message_too_old(create_time, self.clock.now_epoch_millis()) {
            tracing::debug!(
                chat_session_id = %session_id,
                message_id,
                create_time,
                "lark typing indicator: message too old, skipping"
            );
            return;
        }
        let Some(credentials) =
            self.resolve_credentials(installation, session_id, message_id, "add")
        else {
            return;
        };
        let reaction_id = match self
            .client
            .add_message_reaction(AddReactionParams {
                credentials,
                message_id: message_id.to_string(),
                emoji_type: TYPING_EMOJI.to_string(),
            })
            .await
        {
            Ok(reaction_id) => reaction_id,
            Err(error) => {
                tracing::warn!(
                    chat_session_id = %session_id,
                    message_id,
                    class = error.class().as_str(),
                    "lark typing indicator: add reaction failed"
                );
                return;
            }
        };
        if let Ok(mut guard) = self.states.lock() {
            guard
                .entry(session_key(session_id))
                .or_default()
                .push(TypingIndicatorState {
                    message_id: message_id.to_string(),
                    reaction_id,
                    installation_id: installation.id,
                    installation_snapshot: installation.clone(),
                });
        }
    }

    /// 撤掉该会话跟踪着的**全部**指示（上游 `Clear` 的实体）。
    ///
    /// 上游逐字的四条：状态**先**取走（`delete(map, key)`）⇒ 重复调用是 no-op；每条撤失败
    /// **只记日志不中断循环**；同一会话的指示通常共享一个安装 ⇒ 解出的凭据**备忘**（`None`
    /// 条目同样进备忘 ⇒ "这个安装解不出来"不会每条都重试一次）；状态里的安装 id 可能在撤的
    /// 时候已经查不到了 ⇒ 回落**快照**。
    pub async fn clear_now(&self, session_id: Id) {
        let key = session_key(session_id);
        let states = match self.states.lock() {
            Ok(mut guard) => guard.remove(&key).unwrap_or_default(),
            Err(_) => return,
        };
        if states.is_empty() {
            return;
        }
        let mut resolved: HashMap<String, Option<InstallationCredentials>> = HashMap::new();
        for state in states {
            if state.reaction_id.is_empty() {
                continue;
            }
            let installation_key = state.installation_id.to_string();
            let credentials = if let Some(cached) = resolved.get(&installation_key) {
                cached.clone()
            } else {
                let resolved_one = self.credentials_for_installation(session_id, &state).await;
                resolved.insert(installation_key, resolved_one.clone());
                resolved_one
            };
            let Some(credentials) = credentials else {
                continue;
            };
            if let Err(error) = self
                .client
                .delete_message_reaction(DeleteReactionParams {
                    credentials,
                    message_id: state.message_id.clone(),
                    reaction_id: state.reaction_id.clone(),
                })
                .await
            {
                tracing::warn!(
                    chat_session_id = %session_id,
                    message_id = state.message_id,
                    reaction_id = state.reaction_id,
                    class = error.class().as_str(),
                    "lark typing indicator: delete reaction failed"
                );
                continue;
            }
            tracing::debug!(
                chat_session_id = %session_id,
                message_id = state.message_id,
                reaction_id = state.reaction_id,
                "lark typing indicator: reaction removed"
            );
        }
    }

    /// 撤一条指示要的凭据：**先**回查安装行，行没了才用快照（模块文档第 5 条）。
    async fn credentials_for_installation(
        &self,
        session_id: Id,
        state: &TypingIndicatorState,
    ) -> Option<InstallationCredentials> {
        let installation = match &self.queries {
            Some(queries) => match queries.installation(state.installation_id).await {
                Ok(Some(installation)) => installation,
                Ok(None) => {
                    // 行真的没了 ⇒ 运行时拆除那一支；快照是唯一还能把表情摘下来的东西。
                    tracing::debug!(
                        chat_session_id = %session_id,
                        installation_id = %state.installation_id,
                        "lark typing indicator: installation gone, clearing from the snapshot \
                         taken at add time"
                    );
                    state.installation_snapshot.clone()
                }
                Err(error) => {
                    tracing::warn!(
                        chat_session_id = %session_id,
                        installation_id = %state.installation_id,
                        "lark typing indicator: failed to lookup installation for clear: {error}"
                    );
                    return None;
                }
            },
            // 没接查询 ⇒ 只能靠快照（见 [`TypingIndicatorManager::with_snapshot_only`]）。
            None => state.installation_snapshot.clone(),
        };
        self.resolve_credentials(&installation, session_id, &state.message_id, "clear")
    }

    /// 解一条安装的明文凭据（**唯一**出口；失败只记 warn）。
    fn resolve_credentials(
        &self,
        installation: &LarkInstallation,
        session_id: Id,
        message_id: &str,
        phase: &'static str,
    ) -> Option<InstallationCredentials> {
        let Some(decrypt) = &self.decrypt else {
            tracing::warn!(
                chat_session_id = %session_id,
                message_id,
                phase,
                "lark typing indicator: no credential decrypter wired"
            );
            return None;
        };
        match super::feishu_channel::installation_credentials_for(installation, decrypt) {
            Ok(credentials) => Some(credentials),
            Err(error) => {
                tracing::warn!(
                    chat_session_id = %session_id,
                    message_id,
                    phase,
                    "lark typing indicator: failed to resolve credentials: {error}"
                );
                None
            }
        }
    }

    /// 脱离任务用的句柄：克隆 `Arc` 与**密文**投影，`states` 是**同一张**表。
    #[must_use]
    fn handle(&self) -> Self {
        Self {
            client: Arc::clone(&self.client),
            decrypt: self.decrypt.clone(),
            queries: self.queries.clone(),
            clock: Arc::clone(&self.clock),
            states: Arc::clone(&self.states),
        }
    }
}

/// 会话 id → 状态表的键（上游 `uuidString(chatSessionID)`）。
fn session_key(session_id: Id) -> String {
    session_id.to_string()
}

/// 从入站消息里取平台给的 `create_time`（纪元毫秒字串）。
///
/// `raw` 是 M7-12 写的 [`super::feishu_channel::LarkInboundMessage`] 的 JSON（`raw` 的形状是
/// 两条读者的契约，见该文件的模块文档）⇒ 这里**不**猜字段名，直接按那个结构读；读不到 ⇒ 空串
/// ⇒ [`is_message_too_old`] 判"不拦"。
#[must_use]
pub fn create_time_of(message: &InboundMessage) -> String {
    message
        .raw
        .get("create_time")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// 从 engine 的消息信封取"贴给哪条平台消息"的 id（上游 `InboundMessage.MessageID`）。
///
/// 走 [`InboundMessage::dedup_message_id`]（M7-1 定的 dedup 键）而**不是**裸字段：对 lark 两者
/// 目前相同，但前者是"平台消息 id"的**契约出口** —— 将来若去重键改成分片形态，只有这一处要动。
/// 它**不**回落事件 id（空 `message_id` ⇒ 空串 ⇒ [`TypingIndicatorManager::add_now`] 直接返回）。
#[must_use]
pub fn reaction_target_of(message: &InboundMessage) -> &str {
    message.dedup_message_id()
}

/// 从 engine 的安装信封取本 adapter 的安装投影（见 [`platform_installation`]）。
///
/// `None` = 这条 `ResolvedInstallation` 不是 lark 造的（纯出站路径 / 用例）⇒ 调用方**降级跳过**。
#[must_use]
pub fn installation_of(installation: &ResolvedInstallation) -> Option<&LarkInstallation> {
    platform_installation(installation)
}

impl TypingNotifier for TypingIndicatorManager {
    /// 入库成功后点亮指示器（同步接缝 ⇒ 推一个脱离任务）。
    ///
    /// 上游的 `Add` 由路由在"入库成功"那一刻调；本仓的 engine 契约
    /// （[`crate::engine::resolvers::TypingNotifier::on_ingested`]）**同样**只在
    /// `Outcome::Ingested && run_scheduled` 时被调（见 `engine/router/outbound.rs`）。
    fn on_ingested(
        &self,
        installation: &ResolvedInstallation,
        message: &InboundMessage,
        session_id: Id,
    ) {
        let Some(platform) = installation_of(installation) else {
            tracing::warn!(
                installation_id = %installation.id,
                "lark typing indicator: installation platform row unavailable"
            );
            return;
        };
        let platform = platform.clone();
        let message_id = reaction_target_of(message).to_string();
        let create_time = create_time_of(message);
        let handle = self.handle();
        spawn_detached(async move {
            handle
                .add_now(&platform, session_id, &message_id, &create_time)
                .await;
        });
    }

    /// 会话的 run 没有产出任务时清除指示器（同步接缝 ⇒ 推一个脱离任务）。
    ///
    /// 上游注释逐字：那种情况下**永远**不会发布任务生命周期事件，平台自己的"任务结束即清除"
    /// 也就不会触发 ⇒ 必须在这里清，否则"处理中"会一直粘在用户消息上。幂等。
    fn on_settled(&self, session_id: Id) {
        let handle = self.handle();
        spawn_detached(async move {
            handle.clear_now(session_id).await;
        });
    }
}

/// 脱离式执行（模块文档差异 D7）：有运行时 ⇒ 起任务；没有 ⇒ 打一条 warn。
fn spawn_detached<F>(future: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::spawn(future);
    } else {
        tracing::warn!("lark typing indicator: no async runtime; skipping the detached work");
    }
}

#[cfg(test)]
mod tests;
