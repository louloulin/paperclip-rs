//! [`super`] 的替身与夹具：一个**脚本化**的 [`ApiClient`]、可注入的时钟，
//! 以及构造 `LarkInboundMessage` / `InstallationCredentials` 的小工具。
//!
//! # 替身纪律（`docs/60` §4.2 的三条里的第 1 条）
//!
//! 只替**平台 wire**，不替业务路径：本文件里的 `FakeApi` 就是"假 Lark"，
//! 断言链是 `脚本造响应 → 真装配器 → 真渲染块`，中间零 mock。
//!
//! **不用的面转发给 [`StubApiClient`]**（它对每个传输调用回 [`ApiError::NotConfigured`]）：
//! 卡片 / 出站 / 反应那些面归 M7-13 / M7-14，本片的用例不碰它们 —— 用替身转发比在这里
//! 手写一遍"永不调用"的桩更诚实（一旦将来有人误调，会拿到替身的响亮失败而不是静默的空值）。

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use mc_core::channel::message::ChatType;

use super::super::super::client::{ApiClient, ApiError, StubApiClient};
use super::super::super::feishu_channel::LarkInboundMessage;
use super::super::super::http_client::resource::{DownloadedResource, DownloadedResourceStream};
use super::super::super::params::{
    AddReactionParams, AppSecret, BindingPromptParams, DeleteReactionParams,
    DownloadResourceParams, InstallationCredentials, ListMessagesParams, PatchCardParams,
    SendCardParams, SendMarkdownCardParams, SendTextParams,
};
use super::super::super::resolvers::LarkInstallation;
use super::super::super::types::{BotInfo, ChatId, LarkMessage, OpenId, Region};
use super::super::super::ws_frame_decoder::{LarkEventMention, LarkInboundEvent};
use super::super::{Clock, InboundEnricherConfig, ManualClock};

// =====================================================================
// 脚本化的替身
// =====================================================================

/// 一次 `list_chat_messages` 的脚本结果（第一次用 `first`，之后依次用 `rest`）。
struct ListScript {
    first: Option<Result<Vec<LarkMessage>, ApiError>>,
    rest: VecDeque<Result<Vec<LarkMessage>, ApiError>>,
    /// 每调一次 `list_chat_messages` 就把注入的时钟往前推这么多毫秒（模拟"调用吃光了预算"）。
    advance_clock: Option<(Arc<ManualClock>, u64)>,
}

/// 替身的可观测状态（`Mutex` 是因为 `ApiClient` 的方法都取 `&self`）。
#[derive(Default)]
struct Recorded {
    get_message_calls: usize,
    list_calls: usize,
    list_params: Vec<ListMessagesParams>,
    regions: Vec<Region>,
}

/// 脚本化的假 Lark（见模块文档的替身纪律）。
pub struct FakeApi {
    configured: bool,
    get_message: Mutex<HashMap<String, Result<Vec<LarkMessage>, ApiError>>>,
    list: Mutex<ListScript>,
    users: Mutex<Result<HashMap<String, String>, ApiError>>,
    recorded: Mutex<Recorded>,
}

impl FakeApi {
    pub fn list_calls(&self) -> usize {
        self.recorded.lock().expect("lock").list_calls
    }

    pub fn get_message_calls(&self) -> usize {
        self.recorded.lock().expect("lock").get_message_calls
    }

    /// 第 `index` 次 `list` 调用用的 `end_time`（秒）。
    pub fn list_end_time(&self, index: usize) -> i64 {
        self.recorded.lock().expect("lock").list_params[index].end_time
    }

    /// 第 `index` 次 `list` 调用有没有走话题容器。
    pub fn list_used_thread(&self, index: usize) -> bool {
        !self.recorded.lock().expect("lock").list_params[index]
            .thread_id
            .is_empty()
    }

    /// 最近一次调用带的 region（**入站路径上 region 的可观测面**）。
    pub fn last_region(&self) -> Option<Region> {
        self.recorded.lock().expect("lock").regions.last().copied()
    }

    /// 把"没被本片用到的面"交给替身（见模块文档）。
    fn stub() -> StubApiClient {
        StubApiClient::new()
    }
}

/// 构造替身（链式）。
pub struct ApiBuilder {
    api: FakeApi,
}

impl ApiBuilder {
    /// 标记替身为"未接线"。
    #[must_use]
    pub fn unconfigured(mut self) -> Self {
        self.api.configured = false;
        self
    }

    /// 给 `get_message(id)` 脚本一个结果。
    #[must_use]
    pub fn get_message(self, id: &str, result: Result<Vec<LarkMessage>, ApiError>) -> Self {
        self.api
            .get_message
            .lock()
            .expect("lock")
            .insert(id.to_string(), result);
        self
    }

    /// 第一次 `list` 的结果。
    #[must_use]
    pub fn list(self, result: Result<Vec<LarkMessage>, ApiError>) -> Self {
        self.api.list.lock().expect("lock").first = Some(result);
        self
    }

    /// 后续 `list` 的结果（依次取用）。
    #[must_use]
    pub fn then_list(self, result: Result<Vec<LarkMessage>, ApiError>) -> Self {
        self.api.list.lock().expect("lock").rest.push_back(result);
        self
    }

    /// 每次 `list` 都推进时钟（模拟"这次调用吃光了预算"）。
    #[must_use]
    pub fn advance_clock_on_list(self, clock: Arc<ManualClock>, ms: u64) -> Self {
        self.api.list.lock().expect("lock").advance_clock = Some((clock, ms));
        self
    }

    /// 批量查名的结果（**并入**已有映射：多次调用是"再加几个发言人"，不是覆盖）。
    #[must_use]
    pub fn users(self, pairs: &[(&str, &str)]) -> Self {
        {
            let mut guard = self.api.users.lock().expect("lock");
            if guard.is_err() {
                // 之前被脚本设成失败 ⇒ 成功脚本重新建空映射。
                *guard = Ok(HashMap::new());
            }
            if let Ok(map) = guard.as_mut() {
                for (id, name) in pairs {
                    map.insert((*id).to_string(), (*name).to_string());
                }
            }
        }
        self
    }

    /// 批量查名的显式失败。
    #[must_use]
    pub fn users_fail(self, error: ApiError) -> Self {
        *self.api.users.lock().expect("lock") = Err(error);
        self
    }

    /// [`ApiBuilder::users`] 的别名（读起来更顺的写法）。
    #[must_use]
    pub fn user(self, pairs: &[(&str, &str)]) -> Self {
        self.users(pairs)
    }

    /// 收口成 `Arc`。
    #[must_use]
    pub fn build(self) -> Arc<FakeApi> {
        Arc::new(self.api)
    }
}

/// 起一个替身构造器。
#[must_use]
pub fn api() -> ApiBuilder {
    ApiBuilder {
        api: FakeApi {
            configured: true,
            get_message: Mutex::new(HashMap::new()),
            list: Mutex::new(ListScript {
                first: None,
                rest: VecDeque::new(),
                advance_clock: None,
            }),
            users: Mutex::new(Ok(HashMap::new())),
            recorded: Mutex::new(Recorded::default()),
        },
    }
}

#[async_trait]
impl ApiClient for FakeApi {
    fn is_configured(&self) -> bool {
        self.configured
    }

    async fn get_message(
        &self,
        credentials: InstallationCredentials,
        message_id: &str,
    ) -> Result<Vec<LarkMessage>, ApiError> {
        let mut recorded = self.recorded.lock().expect("lock");
        recorded.get_message_calls += 1;
        recorded.regions.push(credentials.region);
        drop(recorded);
        self.get_message
            .lock()
            .expect("lock")
            .get(message_id)
            .cloned()
            .unwrap_or_else(|| Err(ApiError::Malformed { op: "get_message" }))
    }

    async fn list_chat_messages(
        &self,
        credentials: InstallationCredentials,
        params: ListMessagesParams,
    ) -> Result<Vec<LarkMessage>, ApiError> {
        let mut script = self.list.lock().expect("lock");
        let advance = script.advance_clock.clone();
        let attempt = {
            let mut recorded = self.recorded.lock().expect("lock");
            recorded.list_calls += 1;
            recorded.regions.push(credentials.region);
            recorded.list_params.push(params);
            recorded.list_calls
        };
        // 先按脚本取结果，再推进时钟（"这次调用花掉的时间"在返回之后才可见）。
        let result = if attempt == 1 {
            script.first.clone()
        } else {
            script.rest.pop_front()
        };
        drop(script);
        if let Some((clock, ms)) = advance {
            clock.advance(ms);
        }
        result.unwrap_or(Err(ApiError::NotConfigured))
    }

    async fn batch_get_users(
        &self,
        credentials: InstallationCredentials,
        _open_ids: Vec<String>,
    ) -> Result<HashMap<String, String>, ApiError> {
        self.recorded
            .lock()
            .expect("lock")
            .regions
            .push(credentials.region);
        self.users.lock().expect("lock").clone()
    }

    // ---- 以下的六个面归 M7-13 / M7-14；本片转发给替身（见模块文档）----

    async fn send_interactive_card(&self, params: SendCardParams) -> Result<String, ApiError> {
        Self::stub().send_interactive_card(params).await
    }

    async fn patch_interactive_card(&self, params: PatchCardParams) -> Result<(), ApiError> {
        Self::stub().patch_interactive_card(params).await
    }

    async fn send_text_message(&self, params: SendTextParams) -> Result<String, ApiError> {
        Self::stub().send_text_message(params).await
    }

    async fn send_markdown_card(&self, params: SendMarkdownCardParams) -> Result<String, ApiError> {
        Self::stub().send_markdown_card(params).await
    }

    async fn send_binding_prompt_card(&self, params: BindingPromptParams) -> Result<(), ApiError> {
        Self::stub().send_binding_prompt_card(params).await
    }

    async fn get_bot_info(
        &self,
        credentials: InstallationCredentials,
    ) -> Result<BotInfo, ApiError> {
        Self::stub().get_bot_info(credentials).await
    }

    async fn download_message_resource(
        &self,
        credentials: InstallationCredentials,
        params: DownloadResourceParams,
    ) -> Result<DownloadedResource, ApiError> {
        Self::stub()
            .download_message_resource(credentials, params)
            .await
    }

    async fn download_message_resource_stream(
        &self,
        credentials: InstallationCredentials,
        params: DownloadResourceParams,
    ) -> Result<DownloadedResourceStream, ApiError> {
        Self::stub()
            .download_message_resource_stream(credentials, params)
            .await
    }

    async fn add_message_reaction(&self, params: AddReactionParams) -> Result<String, ApiError> {
        Self::stub().add_message_reaction(params).await
    }

    async fn delete_message_reaction(&self, params: DeleteReactionParams) -> Result<(), ApiError> {
        Self::stub().delete_message_reaction(params).await
    }
}

// =====================================================================
// 夹具
// =====================================================================

/// 生产默认旋钮 + 注入时钟（`ManualClock`，**不睡真觉**）。
#[must_use]
pub fn config() -> InboundEnricherConfig {
    InboundEnricherConfig::new(Arc::new(ManualClock::new(0)) as Arc<dyn Clock>)
        .with_budget(Duration::from_secs(2))
}

/// 一条明文凭据（用例只关心 region，secret 是占位）。
#[must_use]
pub fn credentials() -> InstallationCredentials {
    InstallationCredentials::new("cli_test", AppSecret::new("app-secret-value"))
}

/// 一条安装行投影（`bot_union_id = None` = **回填之前**的安装）。
#[must_use]
pub fn installation(bot_union_id: Option<&str>) -> LarkInstallation {
    LarkInstallation {
        id: mc_core::id::Id::new(),
        workspace_id: mc_core::id::Id::new(),
        agent_id: mc_core::id::Id::new(),
        app_id: "cli_test".to_string(),
        app_secret_encrypted: b"plain".to_vec(),
        tenant_key: None,
        bot_open_id: OpenId::new("ou_bot"),
        bot_union_id: bot_union_id.map(str::to_string),
        region: Region::Feishu,
        installer_user_id: mc_core::id::Id::new(),
        status: "active".to_string(),
    }
}

/// 一条 WS 事件（M7-11 的解码产物；只填本片读的字段）。
#[must_use]
pub fn event(id: &str, message_type: &str, content: &str, chat_type: ChatType) -> LarkInboundEvent {
    LarkInboundEvent {
        event_type: "im.message.receive_v1".to_string(),
        event_id: format!("ev-{id}"),
        app_id: "cli_test".to_string(),
        tenant_key: "tenant-1".to_string(),
        chat_id: ChatId::new("oc-1"),
        chat_type,
        message_id: id.to_string(),
        sender_open_id: OpenId::new("ou_bob"),
        sender_union_id: String::new(),
        message_type: message_type.to_string(),
        content: content.to_string(),
        mentions: Vec::<LarkEventMention>::new(),
        create_time: "1700000000000".to_string(),
        parent_id: String::new(),
        root_id: String::new(),
        thread_id: String::new(),
        raw: serde_json::Value::Null,
    }
}

/// 一条已归一化的 p2p 群载荷（`@` 判据为假）。
#[must_use]
pub fn p2p_payload(id: &str, body: &str) -> LarkInboundMessage {
    let mut payload = LarkInboundMessage::from_event(
        event(
            id,
            "text",
            &format!(r#"{{"text":{}}}"#, serde_json::to_string(body).unwrap()),
            ChatType::P2p,
        ),
        &installation(Some("on_bot")),
    );
    payload.sender_open_id = OpenId::new("ou_bob");
    payload
}

/// 一条已归一化的群载荷（`addressed_to_bot` 默认假，调用方按需置位）。
#[must_use]
pub fn group_payload(id: &str, body: &str) -> LarkInboundMessage {
    let mut payload = LarkInboundMessage::from_event(
        event(
            id,
            "text",
            &format!(r#"{{"text":{}}}"#, serde_json::to_string(body).unwrap()),
            ChatType::Group,
        ),
        &installation(Some("on_bot")),
    );
    payload.sender_open_id = OpenId::new("ou_bob");
    payload
}

/// 一条取回到的文本消息。
#[must_use]
pub fn text_message(id: &str, sender: &str, sender_type: &str, text: &str) -> LarkMessage {
    LarkMessage {
        message_id: id.to_string(),
        message_type: "text".to_string(),
        content: format!(r#"{{"text":{}}}"#, serde_json::to_string(text).unwrap()),
        sender_id: sender.to_string(),
        sender_type: sender_type.to_string(),
        ..LarkMessage::default()
    }
}

/// 带 `create_time` 的文本消息（排序与客户端侧锚点要用）。
#[must_use]
pub fn text_message_at(
    id: &str,
    sender: &str,
    sender_type: &str,
    text: &str,
    create_time: &str,
) -> LarkMessage {
    LarkMessage {
        create_time: create_time.to_string(),
        ..text_message(id, sender, sender_type, text)
    }
}

/// 一条被引用的父消息。
#[must_use]
pub fn quoted_parent(id: &str, sender: &str, text: &str) -> LarkMessage {
    text_message(id, sender, "user", text)
}
