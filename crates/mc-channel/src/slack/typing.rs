//! Slack 的「处理中」指示器：在用户消息上挂/摘 👀 反应
//! （上游 `internal/integrations/slack/typing_indicator.go`，298 行）。
//!
//! - **写者**：M7-4（`docs/60-M7-PLAN.md` §3.3）。
//! - **上游语义**：Slack 没有「对方正在输入」这种可编程提示，所以用**反应**表达
//!   「已看到、在处理」；`typingEmoji = "eyes"`。
//! - **状态在内存里**，键是 `chat_session_id`；**令牌不在内存里** —— 摘反应时按
//!   `installation_id` 重新解析（上游注释逐字：only the installation id is held in the map
//!   between add and clear, never the token）。
//! - **快照是 FALLBACK，不是主路径**：安装行可能在「加反应」与「摘反应」之间被
//!   运行时拆除（`handler/runtime.go` 的 `DeleteChannelInstallationsBySystemRuntimeAgents`
//!   在同一个事务里删安装 + 取消任务）⇒ 那时只剩加反应时存下的密文快照。**活查优先**
//!   （能拿到凭据轮换后的新令牌），行没了才用快照。
//! - **尽力而为**：每一次失败都只告警，绝不阻断或失败一条真回复（上游逐字）。
//!
//! # 与上游的两处形态差异（登记 `docs/32` §15）
//!
//! 1. **同步接缝 + 脱离式执行**：engine 的 [`TypingNotifier`] 是两个**同步**方法
//!    （它已经在 `tokio::spawn` 的上下文里被调用，见 `engine/router/outbound.rs`），
//!    而上游的 `Add` 是**阻塞**着做完 HTTP 调用的。本仓在同步方法里只做
//!    「判定 + `tokio::spawn`」，把 HTTP 调用放到脱离任务里 —— 于是引擎的调用点
//!    **绝不**阻塞在 Slack 的 HTTP 上（严格优于上游；语义等价）。
//!    没有运行时上下文时（纯同步测试）退化成**只记录状态**并打一条 warn。
//! 2. **`now` 可注入**：`isMessageTooOld` 要判「这条消息是否超过 2 分钟」，
//!    上游直接 `time.Since`；本仓注入 `NowFn`，否则那条判据只能靠 sleep 测。

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use mc_core::channel::message::InboundMessage;
use mc_core::id::Id;
use serde_json::Value;

use crate::engine::resolvers::{ResolvedInstallation, TypingNotifier};
use crate::slack::config::Decrypter;
use crate::slack::outbound::{ApiResult, HttpSlackApi, MessageRef};
use crate::slack::resolvers::InstallationRow;

/// 用作「处理中」的反应名（上游 `typingEmoji`，逐字）。
///
/// 改这一个常量就换指示器；安装的 Slack app 需要 `reactions:write` 作用域，
/// 没有时只是**摘挂失败并被记录**，不影响任何真回复。
pub const TYPING_EMOJI: &str = "eyes";

/// 超过这个年龄的入站消息**不**挂反应（上游 `typingIndicatorMaxAge = 2 * time.Minute`）。
///
/// 理由逐字：Socket Mode 重连会重放旧事件，不设这一条就会给早就结束的对话盖上
/// 「处理中」的戳。
pub const TYPING_MAX_AGE: Duration = Duration::from_secs(120);

/// 当前时刻的可注入来源（用例把 `now` 钉住，否则年龄判据只能靠 sleep 测）。
pub type NowFn = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

/// 一个反应面（上游 `reactionAPI`）：`*slack.Client` 直接满足它，用例注入替身。
#[async_trait]
pub trait ReactionApi: Send + Sync {
    /// `reactions.add`。
    async fn add_reaction(&self, token: &str, emoji: &str, item: &MessageRef) -> ApiResult<()>;
    /// `reactions.remove`（按 emoji 名 + item 定位；**没有** reaction id 可存）。
    async fn remove_reaction(&self, token: &str, emoji: &str, item: &MessageRef) -> ApiResult<()>;
}

/// 生产实现：两个 `reactions.*` 方法。
#[derive(Debug, Default, Clone)]
pub struct HttpReactions;

#[async_trait]
impl ReactionApi for HttpReactions {
    async fn add_reaction(&self, token: &str, emoji: &str, item: &MessageRef) -> ApiResult<()> {
        HttpSlackApi::call("reactions.add", token, reaction_body(emoji, item))
            .await
            .map(|_| ())
    }

    async fn remove_reaction(&self, token: &str, emoji: &str, item: &MessageRef) -> ApiResult<()> {
        HttpSlackApi::call("reactions.remove", token, reaction_body(emoji, item))
            .await
            .map(|_| ())
    }
}

/// `reactions.*` 的请求体（上游 SDK 的线形态）。
fn reaction_body(emoji: &str, item: &MessageRef) -> Value {
    serde_json::json!({
        "name": emoji,
        "channel": item.channel,
        "timestamp": item.timestamp,
    })
}

/// 安装行的读取口（上游 `TypingIndicatorQueries.GetChannelInstallation`）。
///
/// 只回**密文 config**（调用方自己解密）：本端口不知道明文的存在。
#[async_trait]
pub trait InstallationConfigs: Send + Sync {
    /// 读一条 Slack 安装的 config；行不存在 ⇒ `Ok(None)`（那正是快照接管的情形）。
    async fn config_of(&self, installation_id: Id) -> Result<Option<Value>, String>;
}

/// 生产形态：泛化安装行仓储（**零新 SQL** —— `get` 已存在）。
///
/// 只回**密文 config**：本端口不知道明文的存在（凭据纪律第 1 条）。
#[derive(Clone)]
pub struct RepoInstallationConfigs {
    installations: mc_repos::channel::installation::ChannelInstallationRepo,
}

impl RepoInstallationConfigs {
    /// 装配。
    #[must_use]
    pub fn new(installations: mc_repos::channel::installation::ChannelInstallationRepo) -> Self {
        Self { installations }
    }
}

#[async_trait]
impl InstallationConfigs for RepoInstallationConfigs {
    async fn config_of(&self, installation_id: Id) -> Result<Option<Value>, String> {
        match self.installations.get(installation_id).await {
            Ok(row) => Ok(Some(row.config)),
            Err(mc_repos::RepoError::NotFound) => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }
}

/// 摘反应需要的定位信息（上游 `typingState`）。
///
/// `config_snapshot` 是**加反应那一刻**的密文 config（见模块文档的 FALLBACK 说明）。
#[derive(Debug, Clone, PartialEq)]
pub struct TypingState {
    pub channel_id: String,
    pub message_ts: String,
    pub installation_id: Id,
    pub config_snapshot: Value,
}

/// 「处理中」指示器的生命周期管理（上游 `TypingIndicatorManager`）。
pub struct TypingIndicatorManager {
    api: Arc<dyn ReactionApi>,
    installations: Option<Arc<dyn InstallationConfigs>>,
    decrypt: Decrypter,
    /// 会话 → 已挂上的反应。**`Arc` 是必须的**：engine 的 [`TypingNotifier`] 是
    /// `&self` 方法，而实际调用发生在脱离任务里 ⇒ 句柄必须共享同一张表，
    /// 否则「加」记到一张表、「摘」查另一张表（表里永远是空的）。
    states: Arc<Mutex<HashMap<String, Vec<TypingState>>>>,
    now: NowFn,
}

impl fmt::Debug for TypingIndicatorManager {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let tracked = self
            .states
            .lock()
            .map(|guard| guard.len())
            .unwrap_or_default();
        formatter
            .debug_struct("TypingIndicatorManager")
            .field("api", &"<dyn ReactionApi>")
            .field("installations", &self.installations.is_some())
            .field("decrypt", &self.decrypt)
            .field("sessions", &tracked)
            .field("now", &"<clock>")
            .finish()
    }
}

impl TypingIndicatorManager {
    /// 装配（显式端口 + 解密器）。
    #[must_use]
    pub fn new(
        api: Arc<dyn ReactionApi>,
        installations: Option<Arc<dyn InstallationConfigs>>,
        decrypt: Decrypter,
    ) -> Self {
        Self {
            api,
            installations,
            decrypt,
            states: Arc::new(Mutex::new(HashMap::new())),
            now: Arc::new(Utc::now),
        }
    }

    /// 生产形态：`reactions.*` 直连。
    #[must_use]
    pub fn http(installations: Option<Arc<dyn InstallationConfigs>>, decrypt: Decrypter) -> Self {
        Self::new(Arc::new(HttpReactions), installations, decrypt)
    }

    /// 注入时钟（用例钉住年龄判据）。
    #[must_use]
    pub fn with_now(mut self, now: NowFn) -> Self {
        self.now = now;
        self
    }

    /// 当前被追踪的会话数（诊断；**不含**任何令牌）。
    #[must_use]
    pub fn tracked_sessions(&self) -> usize {
        self.states
            .lock()
            .map(|guard| guard.len())
            .unwrap_or_default()
    }

    /// 挂反应并记下状态（上游 `Add`）。
    ///
    /// 顺序逐字照上游：**先**判年龄、**再**解密、**再**调用、**最后**记录 ——
    /// 所以「加成功了但记录前就该摘」这条竞态与上游**同样**存在（上游注释逐字承认
    /// 它需要一个 per-session generation 才能关掉，那是另一张票）。
    ///
    /// 错误只告警、不返回（尽力而为）。
    pub async fn add(
        &self,
        installation: &InstallationRow,
        session_id: Id,
        channel_id: &str,
        message_ts: &str,
    ) {
        if channel_id.is_empty() || message_ts.is_empty() {
            return;
        }
        if self.is_too_old(message_ts) {
            tracing::debug!(
                chat_session_id = %session_id,
                message_ts,
                "slack typing indicator: message too old, skipping"
            );
            return;
        }
        let credentials =
            match crate::slack::config::decode_credentials(&installation.config, &self.decrypt) {
                Ok(credentials) => credentials,
                Err(error) => {
                    tracing::warn!(
                        chat_session_id = %session_id,
                        code = error_code(&error),
                        "slack typing indicator: decode credentials failed"
                    );
                    return;
                }
            };
        let item = MessageRef {
            channel: channel_id.to_string(),
            timestamp: message_ts.to_string(),
        };
        if let Err(error) = self
            .api
            .add_reaction(&credentials.bot_token, TYPING_EMOJI, &item)
            .await
        {
            tracing::warn!(
                chat_session_id = %session_id,
                message_ts,
                code = error.code(),
                "slack typing indicator: add reaction failed"
            );
            return;
        }
        if let Ok(mut guard) = self.states.lock() {
            guard
                .entry(session_id.to_string())
                .or_default()
                .push(TypingState {
                    channel_id: channel_id.to_string(),
                    message_ts: message_ts.to_string(),
                    installation_id: installation.id,
                    config_snapshot: installation.config.clone(),
                });
        }
    }

    /// 摘掉该会话的全部反应并丢弃状态（上游 `Clear`）。
    ///
    /// 同一会话的反应通常同属一个安装 ⇒ 解析出来的客户端按 `installation_id` 记忆；
    /// `None` 也记下来（解析失败的安装**不**被每条反应重试一次）。
    pub async fn clear(&self, session_id: Id) {
        let key = session_id.to_string();
        let states = match self.states.lock() {
            Ok(mut guard) => guard.remove(&key).unwrap_or_default(),
            Err(_) => return,
        };
        if states.is_empty() {
            return;
        }
        let mut resolved: HashMap<String, Option<String>> = HashMap::new();
        for state in states {
            let installation_key = state.installation_id.to_string();
            let token = if let Some(cached) = resolved.get(&installation_key) {
                cached.clone()
            } else {
                let found = self
                    .api_for_installation(state.installation_id, &state.config_snapshot)
                    .await
                    .map_err(|error| {
                        tracing::warn!(
                            chat_session_id = %key,
                            installation_id = %installation_key,
                            "slack typing indicator: resolve installation for clear failed: {error}"
                        );
                    })
                    .ok()
                    .flatten();
                resolved.insert(installation_key.clone(), found.clone());
                found
            };
            let Some(token) = token else {
                continue;
            };
            let item = MessageRef {
                channel: state.channel_id.clone(),
                timestamp: state.message_ts.clone(),
            };
            if let Err(error) = self.api.remove_reaction(&token, TYPING_EMOJI, &item).await {
                tracing::warn!(
                    chat_session_id = %key,
                    message_ts = state.message_ts,
                    code = error.code(),
                    "slack typing indicator: remove reaction failed"
                );
            }
        }
    }

    /// 解析某个安装的明文令牌，**只在这通调用期间存在**（上游 `apiForInstallation`）。
    ///
    /// 活查优先；行没了（运行时拆除）才用加反应时的快照 —— 快照拿不到凭据轮换，
    /// 所以它永远只是 FALLBACK。
    async fn api_for_installation(
        &self,
        installation_id: Id,
        snapshot: &Value,
    ) -> Result<Option<String>, String> {
        let mut config = snapshot.clone();
        if let Some(installations) = &self.installations {
            match installations.config_of(installation_id).await {
                Ok(Some(fresh)) => config = fresh,
                Ok(None) if !snapshot.is_null() => {
                    // 行没了：本路径上意味着运行时拆除在同一事务里删掉了它。
                    // 反应还在消息上，快照是唯一还能把它摘下来的东西。
                }
                Ok(None) => return Ok(None),
                Err(error) => return Err(error),
            }
        }
        let credentials = crate::slack::config::decode_credentials(&config, &self.decrypt)
            .map_err(|error| error_code(&error).to_string())?;
        Ok(Some(credentials.bot_token))
    }

    /// 这条 ts 是否已超过 [`TYPING_MAX_AGE`]（上游 `isMessageTooOld`）。
    ///
    /// `ts` 是 `"<秒>.<微秒>"`；**解不开或空 ⇒ 当作新鲜**（宁可多挂一个反应，
    /// 也不漏掉一条真消息 —— 上游注释逐字）。
    #[must_use]
    pub fn is_too_old(&self, ts: &str) -> bool {
        if ts.is_empty() {
            return false;
        }
        // 只取整数秒部分（Slack 的 ts 是 `"<秒>.<微秒>"`）：于是整条判据里**没有**任何
        // 浮点转换，也没有 `f64 → i32/u32` 的截断（clippy pedantic 的两条硬失败）。
        // 代价是亚秒精度被抹掉（119.9 秒会读成 119 秒）—— 对"2 分钟"这条阈值无影响。
        let seconds = ts.split('.').next().unwrap_or_default();
        let Ok(seconds) = seconds.parse::<i64>() else {
            return false;
        };
        let Some(message_at) = Utc.timestamp_opt(seconds, 0).single() else {
            return false;
        };
        let max_age =
            chrono::Duration::seconds(i64::try_from(TYPING_MAX_AGE.as_secs()).unwrap_or(i64::MAX));
        (self.now)().signed_duration_since(message_at) > max_age
    }

    /// 脱离式执行（模块文档差异 1）。
    ///
    /// 有 tokio 运行时 ⇒ 起一个任务；没有（纯同步单测）⇒ 打一条 warn 后放弃。
    /// **不 panic**：指示器是尽力而为的装饰，绝不该拖垮引擎的调用点。
    fn spawn_detached<F>(future: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::spawn(future);
        } else {
            tracing::warn!("slack typing indicator: no async runtime; skipping the detached call");
        }
    }
}

/// 从 `ResolvedInstallation.platform` 取回 adapter 自己的安装行（M7-3 放进去的那个）。
#[must_use]
pub fn installation_row(installation: &ResolvedInstallation) -> Option<Arc<InstallationRow>> {
    installation
        .platform
        .as_ref()
        .and_then(|value| Arc::clone(value).downcast::<InstallationRow>().ok())
}

impl TypingNotifier for TypingIndicatorManager {
    /// 入库成功后点亮指示器。上游由 Router 在**脱离式** goroutine 里调用。
    fn on_ingested(
        &self,
        installation: &ResolvedInstallation,
        message: &InboundMessage,
        session_id: Id,
    ) {
        let Some(row) = installation_row(installation) else {
            return;
        };
        let installation_id = installation.id;
        let channel_id = message.source.chat_id.clone();
        let message_ts = message.message_id.clone();
        // 持有 `installation_id` 只为日志与将来可能的对账；这里用不到的字段就不复制。
        let _ = installation_id;
        let manager = self.handle();
        Self::spawn_detached(async move {
            manager
                .add(&row, session_id, &channel_id, &message_ts)
                .await;
        });
    }

    /// 会话没有产出任务时清除指示器（agent 离线 / 归档 / 入队失败）——幂等。
    fn on_settled(&self, session_id: Id) {
        let manager = self.handle();
        Self::spawn_detached(async move {
            manager.clear(session_id).await;
        });
    }
}

/// 稳定错误码（诊断用；**不含**密文）。
fn error_code(error: &crate::slack::config::ConfigError) -> &'static str {
    match error {
        crate::slack::config::ConfigError::Empty => "config_empty",
        crate::slack::config::ConfigError::Decode { .. } => "config_decode",
        crate::slack::config::ConfigError::Base64 { .. } => "config_base64",
        crate::slack::config::ConfigError::Decrypt { .. } => "config_decrypt",
    }
}

/// `on_ingested` / `on_settled` 要把状态搬进脱离任务 ⇒ 需要一份可 `'static` 的句柄。
///
/// [`TypingIndicatorManager::handle`] 返回的代理**共享同一张 `states` 表**（见字段注释），
/// 且不复制任何令牌（本结构根本不持令牌）。
impl TypingIndicatorManager {
    /// 共享句柄（脱离任务的唯一安全形态；`Arc<Self>` 装配时就是它自己）。
    #[must_use]
    pub fn shared(self: &Arc<Self>) -> Arc<Self> {
        Arc::clone(self)
    }

    /// `&self` 形态下的句柄：共享状态表与全部端口。
    #[must_use]
    fn handle(&self) -> Arc<Self> {
        Arc::new(Self {
            api: Arc::clone(&self.api),
            installations: self.installations.clone(),
            decrypt: self.decrypt.clone(),
            states: Arc::clone(&self.states),
            now: Arc::clone(&self.now),
        })
    }
}
