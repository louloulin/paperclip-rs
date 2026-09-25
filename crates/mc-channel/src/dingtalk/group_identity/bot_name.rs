//! `DingTalk` 的 **bot 可读身份**与**群存在性观察**（上游 `bot_identity.go` 250 行 +
//! `groupPresenceObserver` 的写那一半）。
//!
//! - **写者**：M7-9（`docs/60-M7-PLAN.md` §3.3；写集勘误见 `docs/32` §23 的 D1）。
//! - **拆出来是门 ⑩（800 行硬限）的要求**：`group_identity.rs` 首版 924 行 ⇒ 按
//!   "读面（清单装配） / 写面 + 平台身份查询" 这条**单一接缝**切开（不是凑数字）。
//! - **两处形态差异见 [`super`] 的模块文档**：① `BotNameSource` 是同步签名而实现要打平台
//!   `OpenAPI`；② `crates/mc-repos/src/channel/**` 对本片只读 ⇒ 三张 `dingtalk_*` 表的语句
//!   以端口实现的形态落在 `crates/mc-http/src/routes/channels/dingtalk/store.rs`。

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use mc_core::channel::message::{ChatType, InboundMessage};
use mc_core::id::Id;

use crate::engine::resolvers::{EngineResult, ResolvedInstallation};

use super::GroupInventoryStore;
pub use crate::dingtalk::client::BotNameApi;
pub use crate::dingtalk::jobs::BotNameSource;
use crate::dingtalk::resolvers::{installation_row, GroupPresenceObserver};

// =====================================================================
// bot 名解析（上游 `BotNameResolver`）
// =====================================================================

/// 成功名字的缓存寿命（上游 `botNameCacheTTL = 1h`）。
pub const BOT_NAME_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(3600);

/// 其它失败的缓存寿命（上游 `botNameErrorCacheTTL = 1min`）。
pub const BOT_NAME_ERROR_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(60);

/// **权限被拒**的缓存寿命（上游 `botNamePermissionCacheTTL = 30s`）—— 跨群共享，见模块文档。
pub const BOT_NAME_PERMISSION_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(30);

/// 缓存容量上限（上游 `botNameCacheMaxSize = 4096`）。
pub const BOT_NAME_CACHE_MAX_SIZE: usize = 4096;

/// 单次解析的上限（上游 `botNameLookupTimeout = 3s`）。
pub const BOT_NAME_LOOKUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// `qyapi_chat_manage` 权限不足时写进 `bot_identity_issue` 的那条字面量（上游逐字）。
pub const ISSUE_MISSING_CHAT_MANAGE: &str = "missing_qyapi_chat_manage";

/// 一条缓存的名字（或错误）。
#[derive(Clone)]
struct CachedBotName {
    name: String,
    /// 失败文案（`None` = 成功）。**不**承载密文/明文。
    error: Option<String>,
    /// 这条错误是不是"权限被拒"（跨群共享、TTL 更短）。
    permission_denied: bool,
    expires_at: Instant,
}

/// bot 可读身份的解析器（上游 `BotNameResolver`）。
///
/// 实现 M7-7 定义的同步端口 [`BotNameSource`]，内部把 async 平台调用放到**独立线程**上跑
/// （见模块文档差异 1）。
pub struct BotNameResolver {
    api: Arc<dyn BotNameApi>,
    installations: Arc<dyn GroupInventoryStore>,
    cache: Mutex<HashMap<String, CachedBotName>>,
}

impl fmt::Debug for BotNameResolver {
    /// 手写：端口不可打印，只说明缓存条数。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let cached = self.cache.lock().map_or(0, |map| map.len());
        formatter
            .debug_struct("BotNameResolver")
            .field("api", &"<dyn BotNameApi>")
            .field("installations", &"<dyn GroupInventoryStore>")
            .field("cached", &cached)
            .finish()
    }
}

impl BotNameResolver {
    /// 装配。
    #[must_use]
    pub fn new(api: Arc<dyn BotNameApi>, installations: Arc<dyn GroupInventoryStore>) -> Self {
        Self {
            api,
            installations,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// 身份键：**应用级**（成功与权限拒绝都跨群共享，上游逐字）。
    fn identity_key(app_key: &str, robot_code: &str) -> String {
        format!("identity\u{0}{app_key}\u{0}{robot_code}")
    }

    /// 群键：群相关的失败只在这一个群里缓存。
    fn group_key(app_key: &str, robot_code: &str, conversation_id: &str) -> String {
        format!("group\u{0}{app_key}\u{0}{robot_code}\u{0}{conversation_id}")
    }

    fn cached(&self, key: &str) -> Option<CachedBotName> {
        let map = self.cache.lock().ok()?;
        let entry = map.get(key)?;
        if entry.expires_at > Instant::now() {
            return Some(entry.clone());
        }
        None
    }

    fn store(&self, key: String, entry: CachedBotName) {
        let Ok(mut map) = self.cache.lock() else {
            return;
        };
        if map.len() >= BOT_NAME_CACHE_MAX_SIZE && !map.contains_key(&key) {
            if let Some(victim) = map.keys().next().cloned() {
                map.remove(&victim);
            }
        }
        map.insert(key, entry);
    }

    /// 解析结果（成功 ⇒ 名字；失败 ⇒ 文案 + 是否权限拒绝）。
    fn resolve(
        &self,
        app_key: &str,
        robot_code: &str,
        conversation_id: &str,
    ) -> Result<String, (String, bool)> {
        let identity_key = Self::identity_key(app_key, robot_code);
        // 应用级缓存优先（成功与权限拒绝都在这里，上游逐字）。
        if let Some(entry) = self.cached(&identity_key) {
            return match entry.error {
                Some(error) => Err((error, entry.permission_denied)),
                None => Ok(entry.name),
            };
        }
        let group_key = Self::group_key(app_key, robot_code, conversation_id);
        if let Some(entry) = self.cached(&group_key) {
            return match entry.error {
                Some(error) => Err((error, entry.permission_denied)),
                None => Ok(entry.name),
            };
        }

        let api = Arc::clone(&self.api);
        let installations = Arc::clone(&self.installations);
        let app_key_owned = app_key.to_string();
        let conversation = conversation_id.to_string();
        let robot_code_owned = robot_code.to_string();
        let lookup: Option<Result<String, (String, bool)>> = block_on_engine(async move {
            let credentials = installations
                .credentials_by_app_key(&app_key_owned)
                .await
                .ok()
                .flatten();
            let Some(credentials) = credentials else {
                return Err(("installation credentials unavailable".to_string(), false));
            };
            match api
                .bot_name_in_group(
                    &credentials.app_key,
                    &credentials.app_secret,
                    &robot_code_owned,
                    &conversation,
                )
                .await
            {
                Ok(name) => Ok(name),
                Err(error) => Err((error.to_string(), error.code() == "refused")),
            }
        });
        match lookup {
            Some(Ok(name)) => {
                self.store(
                    identity_key,
                    CachedBotName {
                        name: name.clone(),
                        error: None,
                        permission_denied: false,
                        expires_at: Instant::now() + BOT_NAME_CACHE_TTL,
                    },
                );
                Ok(name)
            }
            Some(Err((reason, permission_denied))) => {
                let ttl = if permission_denied {
                    BOT_NAME_PERMISSION_CACHE_TTL
                } else {
                    BOT_NAME_ERROR_CACHE_TTL
                };
                // 权限拒绝是**应用级**的（跨群共享）；其它失败只属于这一个群。
                let key = if permission_denied {
                    identity_key
                } else {
                    group_key
                };
                self.store(
                    key,
                    CachedBotName {
                        name: String::new(),
                        error: Some(reason.clone()),
                        permission_denied,
                        expires_at: Instant::now() + ttl,
                    },
                );
                Err((reason, permission_denied))
            }
            None => Err(("bot name lookup executor unavailable".to_string(), false)),
        }
    }

    /// 诊断用：清空缓存。
    pub fn clear_cache(&self) {
        if let Ok(mut map) = self.cache.lock() {
            map.clear();
        }
    }
}

impl BotNameSource for BotNameResolver {
    /// **失败关闭**的取值面：拿不到名字 ⇒ `None`（调用方保留每一个可见提及，不猜跨度）。
    ///
    /// 权限被拒**也**是 `None` —— 但调用方要能区分"没名字"与"没权限"，所以
    /// [`GroupPresenceObserver`] 用的是 [`BotNameResolver::resolve`] 那条能拿到
    /// `bot_identity_issue` 的内部路径。
    fn bot_name(&self, app_key: &str, conversation_id: &str) -> Option<String> {
        self.resolve(app_key, app_key, conversation_id).ok()
    }
}

impl BotNameResolver {
    /// 群观察用的形态：名字 + `bot_identity_issue`（上游 `groupPresenceObserver` 写两列）。
    #[must_use]
    pub fn describe(
        &self,
        app_key: &str,
        robot_code: &str,
        conversation_id: &str,
    ) -> (String, String) {
        match self.resolve(app_key, robot_code, conversation_id) {
            Ok(name) => (name, String::new()),
            Err((_reason, permission_denied)) => {
                if permission_denied {
                    (String::new(), ISSUE_MISSING_CHAT_MANAGE.to_string())
                } else {
                    (String::new(), String::new())
                }
            }
        }
    }
}

/// 在同步上下文里跑一个 async 调用（独立线程 + current-thread 运行时）。
///
/// 与 `slack::media::ThreadedFetcher` / `dingtalk::media::block_on_engine` 同款：`bot_name`
/// 是**同步**签名（M7-7 定死的端口），而实现要打平台 `OpenAPI`。
fn block_on_engine<F, T>(future: F) -> Option<T>
where
    F: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    std::thread::scope(|scope| {
        scope
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .ok()?;
                Some(runtime.block_on(future))
            })
            .join()
            .ok()
            .flatten()
    })
}

// =====================================================================
// 群存在性观察（上游 `groupPresenceObserver` 的写那一半）
// =====================================================================

/// 群存在性 / bot 身份的**写**口（三张 `dingtalk_*` 表）。
#[async_trait]
pub trait GroupPresenceStore: Send + Sync {
    /// upsert 一条群存在性观察（`mention_count + 1`，刷新 `last_active_at`）。
    async fn observe_presence(
        &self,
        workspace_id: Id,
        installation_id: Id,
        conversation_id: &str,
        conversation_title: &str,
        bot_name: &str,
        bot_identity_issue: &str,
    ) -> Result<(), String>;

    /// upsert 安装级 bot 身份（**空身份也要写**：它记录"这个安装被观察过"）。
    async fn observe_bot_identity(
        &self,
        workspace_id: Id,
        installation_id: Id,
        bot_name: &str,
        bot_identity_issue: &str,
    ) -> Result<(), String>;

    /// 只推进群活动时间与提及计数（`observe` 已经建过行）。
    async fn record_activity(
        &self,
        installation_id: Id,
        conversation_id: &str,
    ) -> Result<(), String>;
}

/// 上游 `groupPresenceObserver` 的真实现（替换 M7-7 的诚实默认值 `NoGroupPresence`）。
///
/// **尽力而为**（上游逐字）：任何失败只记 warn，**绝不**让一条有效消息失败
/// （`resolvers::DingTalkSessionBinder` 的调用点就是这么包它的）。
pub struct PresenceObserver {
    store: Arc<dyn GroupPresenceStore>,
    names: Option<Arc<BotNameResolver>>,
}

impl fmt::Debug for PresenceObserver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PresenceObserver")
            .field("store", &"<dyn GroupPresenceStore>")
            .field("names", &self.names.as_ref().map(|_| "<BotNameResolver>"))
            .finish()
    }
}

impl PresenceObserver {
    /// 装配（没有名字解析器 = 只记存在性，不记身份）。
    #[must_use]
    pub fn new(store: Arc<dyn GroupPresenceStore>, names: Option<Arc<BotNameResolver>>) -> Self {
        Self { store, names }
    }
}

/// 从入站消息的原始载荷里读群标题（`conversationTitle`，M7-7 的 `DingtalkRawEvent` 那个键）。
fn conversation_title_of(message: &InboundMessage) -> String {
    message
        .raw
        .get("conversation_title")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// 只对**群聊**观察（直聊没有"群存在性"这回事；上游逐字）。
fn group_conversation(message: &InboundMessage) -> Option<&str> {
    if message.source.chat_type != ChatType::Group {
        return None;
    }
    let chat_id = message.source.chat_id.trim();
    if chat_id.is_empty() {
        return None;
    }
    Some(chat_id)
}

#[async_trait]
impl GroupPresenceObserver for PresenceObserver {
    async fn observe(
        &self,
        installation: &ResolvedInstallation,
        message: &InboundMessage,
    ) -> EngineResult<()> {
        let Some(conversation_id) = group_conversation(message) else {
            return Ok(());
        };
        let app_key = installation_row(installation)
            .map(|row| row.app_id().to_string())
            .unwrap_or_default();
        let (bot_name, bot_identity_issue) = match (&self.names, app_key.is_empty()) {
            (Some(names), false) => names.describe(&app_key, &app_key, conversation_id),
            _ => (String::new(), String::new()),
        };
        let title = conversation_title_of(message);
        let result = self
            .store
            .observe_presence(
                installation.workspace_id,
                installation.id,
                conversation_id,
                &title,
                &bot_name,
                &bot_identity_issue,
            )
            .await;
        if let Err(reason) = result {
            tracing::warn!(
                installation_id = %installation.id,
                code = "presence_observe",
                reason = reason.as_str(),
                "dingtalk: group presence observation failed"
            );
            return Ok(());
        }
        if let Err(reason) = self
            .store
            .observe_bot_identity(
                installation.workspace_id,
                installation.id,
                &bot_name,
                &bot_identity_issue,
            )
            .await
        {
            tracing::warn!(
                installation_id = %installation.id,
                code = "bot_identity_observe",
                reason = reason.as_str(),
                "dingtalk: bot identity observation failed"
            );
        }
        Ok(())
    }

    async fn record_activity(
        &self,
        installation_id: Id,
        message: &InboundMessage,
    ) -> EngineResult<()> {
        let Some(conversation_id) = group_conversation(message) else {
            return Ok(());
        };
        if let Err(reason) = self
            .store
            .record_activity(installation_id, conversation_id)
            .await
        {
            tracing::warn!(
                installation_id = %installation_id,
                code = "presence_activity",
                reason = reason.as_str(),
                "dingtalk: group activity update failed"
            );
        }
        Ok(())
    }
}
