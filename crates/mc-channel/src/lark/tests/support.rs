//! `lark` 模块级用例的**共用件**（M7-13）：平台替身、内存数据层、回路装配。
//!
//! 与各子模块的 `tests.rs` 分开是**门 ⑩**（单文件 800 行硬限）的要求；切点是「装置 / 断言」
//! ——本文件只有装置，断言全在调用方。
//!
//! # 三层装置，两个文件
//!
//! - [`FakeApi`]：**平台 wire 的替身**。它实现 [`ApiClient`]（M7-10 定的端口），把每次调用
//!   **逐字段**记下来（用例断言的就是它），并按脚本决定成功 / 失败 / 平台码。**不**开真 socket、
//!   不睡真觉。
//! - [`FakeMinter`]：铸绑定令牌的替身（上游 `BindingTokenMinter`）。
//! - [`MemoryStore`]（定义在 [`store`] 子模块里）：**数据层的替身**，实现
//!   [`crate::lark::channel_store`] 的六个仓储端口。它插进
//!   `LarkChannelStore` ⇒ 于是（a）出站 / 打字 / 回复器三条路径都拿到真实的桥，
//!   （b）"哪一族表"这件事在用例里也是可见的（内存里就是两族各一张表）。
//!
//! **真库**形态不在本 crate（本 crate 没有 `sqlx` 依赖）：泛化 `channel_*` 仓储由门 ⑥ 的
//! `crates/mc-http/tests/channels/*` 覆盖（M7-1 落的），遗留 `lark_*` 仓储由
//! `crates/mc-repos/tests` 覆盖。业务路径（判决 / 文案 / 卡片 / 提及 / 回落 / 状态机）在两边
//! 都是**真代码**。
//!
//! # 凭据面：真 `secretbox`
//!
//! [`installation`] 用真 [`SecretBox`] 封装 `app_secret`（不是把明文塞进密文列）⇒
//! "解一次密、过一手 [`InstallationCredentials`]"这条链路在用例里是**真的**。

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use mc_core::id::Id;
use mc_repos::channel::delivery::ChannelTaskDeliveryRow;
use mc_repos::channel::installation::LarkInstallationRow;
use mc_repos::channel::session::LarkChatSessionBindingRow;
use mc_secrets::secretbox::SecretBox;
use serde_json::{json, Value as Json};
use uuid::Uuid;

use crate::lark::client::{ApiClient, ApiError};
use crate::lark::feishu_channel::credentials::Decrypter;
use crate::lark::http_client::resource::{DownloadedResource, DownloadedResourceStream};
use crate::lark::params::{
    AddReactionParams, BindingPromptParams, DeleteReactionParams, DownloadResourceParams,
    InstallationCredentials, ListMessagesParams, PatchCardParams, SendCardParams,
    SendMarkdownCardParams, SendTextParams,
};
use crate::lark::resolvers::LarkInstallation;
use crate::lark::types::{BotInfo, ChatType, LarkMessage, Region};

/// 固定密钥（32 字节）——用例里"部署密钥"的替身。
pub(crate) fn key() -> [u8; 32] {
    [0x11; 32]
}

/// 生产形态的解密器（真 `secretbox`）。
pub(crate) fn decrypter() -> Decrypter {
    Decrypter::secret_box(SecretBox::new(&key()).expect("key size"))
}

/// 把明文封成密文（`lark_installation.app_secret_encrypted` 的 `BYTEA` 形态）。
pub(crate) fn sealed(plaintext: &str) -> Vec<u8> {
    SecretBox::new(&key())
        .expect("key size")
        .seal(plaintext.as_bytes())
        .expect("seal")
}

/// 造一条安装投影（真密文）。
pub(crate) fn installation(id_seed: u128, app_id: &str) -> LarkInstallation {
    LarkInstallation {
        id: Id(Uuid::from_u128(id_seed)),
        workspace_id: Id(Uuid::from_u128(0x9000)),
        agent_id: Id(Uuid::from_u128(0x9100)),
        app_id: app_id.to_string(),
        app_secret_encrypted: sealed("app-secret"),
        tenant_key: Some("tk".to_string()),
        bot_open_id: crate::lark::types::OpenId::new("ou_bot"),
        bot_union_id: Some("on_bot".to_string()),
        region: Region::Feishu,
        installer_user_id: Id(Uuid::from_u128(0x9200)),
        status: "active".to_string(),
    }
}

/// 造一条安装投影，密文列里放**裸明文**（给 [`Decrypter::login_plaintext`] 用）。
pub(crate) fn installation_plaintext(id_seed: u128, app_id: &str) -> LarkInstallation {
    LarkInstallation {
        app_secret_encrypted: b"app-secret".to_vec(),
        ..installation(id_seed, app_id)
    }
}

/// 造一行遗留 `lark_installation`（[`InstallationLookup`] 替身返回的行源）。
pub(crate) fn installation_row(installation: &LarkInstallation) -> LarkInstallationRow {
    LarkInstallationRow {
        id: installation.id.0,
        workspace_id: installation.workspace_id.0,
        agent_id: installation.agent_id.0,
        app_id: installation.app_id.clone(),
        app_secret_encrypted: installation.app_secret_encrypted.clone(),
        tenant_key: installation.tenant_key.clone(),
        bot_open_id: installation.bot_open_id.as_str().to_string(),
        bot_union_id: installation.bot_union_id.clone(),
        region: installation.region.as_str().to_string(),
        installer_user_id: installation.installer_user_id.0,
        status: installation.status.clone(),
        ws_lease_token: None,
        ws_lease_expires_at: None,
        installed_at: Utc::now(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

/// 一条投递行（`channel_task_delivery`；泛化族）。
#[allow(clippy::too_many_arguments)]
pub(crate) fn delivery_row(
    task_id: Id,
    binding_id: Id,
    installation_id: Id,
    chat_id: &str,
    chat_type: ChatType,
    message_id: Option<&str>,
    thread_id: Option<&str>,
    config: Json,
) -> ChannelTaskDeliveryRow {
    ChannelTaskDeliveryRow {
        task_id: task_id.0,
        binding_id: binding_id.0,
        installation_id: installation_id.0,
        channel_type: "feishu".to_string(),
        channel_chat_id: chat_id.to_string(),
        chat_type: chat_type.as_str().to_string(),
        channel_message_id: message_id.map(str::to_string),
        channel_thread_id: thread_id.map(str::to_string),
        route_revision: 3,
        config,
        created_at: Utc::now(),
    }
}

/// 一行遗留 `lark_chat_session_binding`。
pub(crate) fn legacy_binding_row(
    session_id: Id,
    installation_id: Id,
    chat_id: &str,
    chat_type: ChatType,
) -> LarkChatSessionBindingRow {
    LarkChatSessionBindingRow {
        id: Uuid::new_v4(),
        chat_session_id: session_id.0,
        installation_id: installation_id.0,
        lark_chat_id: chat_id.to_string(),
        lark_chat_type: chat_type.as_str().to_string(),
        created_at: Utc::now(),
        last_lark_message_id: None,
        last_lark_thread_id: None,
    }
}

pub(crate) mod store;

pub(crate) use store::MemoryStore;

// =====================================================================
// 平台替身（`ApiClient`）
// =====================================================================

/// 替身记下的一次调用（用例断言的就是它）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Call {
    /// `send_interactive_card`（卡片体的 JSON 原样带出来）。
    SendCard {
        chat_id: String,
        card_json: String,
        reply_message_id: String,
        in_thread: bool,
    },
    /// `patch_interactive_card`。
    PatchCard {
        card_message_id: String,
        card_json: String,
    },
    /// `send_text_message`。
    SendText {
        chat_id: String,
        text: String,
        reply_message_id: String,
        in_thread: bool,
    },
    /// `send_markdown_card`。
    SendMarkdown {
        chat_id: String,
        markdown: String,
        summary: String,
        reply_message_id: String,
        in_thread: bool,
    },
    /// `send_binding_prompt_card`。
    SendBindingPrompt { open_id: String, bind_url: String },
    /// `add_message_reaction`。
    AddReaction {
        message_id: String,
        emoji_type: String,
    },
    /// `delete_message_reaction`。
    DeleteReaction {
        message_id: String,
        reaction_id: String,
    },
}

/// 出站端口的**脚本化替身**（不开 socket、不睡真觉）。
#[derive(Default)]
pub(crate) struct FakeApi {
    /// 每次调用按发生顺序记下来。
    pub(crate) calls: Mutex<Vec<Call>>,
    /// 发消息类调用的返回值脚本（缺省 ⇒ 自增的 `om_x`）。
    pub(crate) send_error: Mutex<Option<ApiError>>,
    /// 贴表情的返回值脚本（缺省 ⇒ 自增的 `re_x`）。
    pub(crate) reaction_error: Mutex<Option<ApiError>>,
    /// 删表情的返回值脚本。
    pub(crate) delete_error: Mutex<Option<ApiError>>,
    /// 绑定卡的返回值脚本。
    pub(crate) binding_error: Mutex<Option<ApiError>>,
    /// patch 的返回值脚本。
    pub(crate) patch_error: Mutex<Option<ApiError>>,
    /// 自增计数器（造 id）。
    pub(crate) counter: Mutex<u32>,
    /// [`ApiClient::is_configured`] 的返回值。
    pub(crate) configured: bool,
}

impl std::fmt::Debug for FakeApi {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FakeApi")
            .field("calls", &self.calls.lock().map_or(0, |log| log.len()))
            .finish_non_exhaustive()
    }
}

impl FakeApi {
    /// 已配置的替身（`is_configured() == true`）。
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            configured: true,
            ..Self::default()
        })
    }

    /// 未配置的替身（上游 `APIClient.IsConfigured() == false` 那一支）。
    pub(crate) fn unconfigured() -> Arc<Self> {
        Arc::new(Self {
            configured: false,
            ..Self::default()
        })
    }

    /// 一次调用。
    pub(crate) fn record(&self, call: Call) {
        self.calls.lock().expect("poisoned").push(call);
    }

    /// 调用日志的克隆（断言用）。
    pub(crate) fn log(&self) -> Vec<Call> {
        self.calls.lock().expect("poisoned").clone()
    }

    /// 让下一次发消息失败。
    pub(crate) fn fail_send_with(&self, error: ApiError) {
        *self.send_error.lock().expect("poisoned") = Some(error);
    }

    /// 让下一次贴表情失败。
    pub(crate) fn fail_reaction_with(&self, error: ApiError) {
        *self.reaction_error.lock().expect("poisoned") = Some(error);
    }

    /// 让下一次删表情失败。
    pub(crate) fn fail_delete_with(&self, error: ApiError) {
        *self.delete_error.lock().expect("poisoned") = Some(error);
    }

    /// 让下一次绑定卡失败。
    pub(crate) fn fail_binding_with(&self, error: ApiError) {
        *self.binding_error.lock().expect("poisoned") = Some(error);
    }

    /// 让下一次 patch 失败。
    pub(crate) fn fail_patch_with(&self, error: ApiError) {
        *self.patch_error.lock().expect("poisoned") = Some(error);
    }

    fn next(&self, prefix: &str) -> String {
        let mut counter = self.counter.lock().expect("poisoned");
        *counter += 1;
        format!("{prefix}_{counter}")
    }

    fn take_send_error(&self) -> Option<ApiError> {
        self.send_error.lock().expect("poisoned").take()
    }

    fn take_reaction_error(&self) -> Option<ApiError> {
        self.reaction_error.lock().expect("poisoned").take()
    }
}

/// 「这条触发消息收不到回复」那一类平台码（上游 `THREAD_REPLY_UNSUPPORTED_CODES`）。
pub(crate) fn unsupported_reply_target_error() -> ApiError {
    ApiError::Refused {
        op: "reply",
        status: None,
        code: crate::lark::client::THREAD_REPLY_UNSUPPORTED_CODES[0],
    }
}

/// 传输层失败（**不得**触发会话层回落）。
pub(crate) fn transport_error() -> ApiError {
    ApiError::Transport { op: "send" }
}

#[async_trait]
impl ApiClient for FakeApi {
    fn is_configured(&self) -> bool {
        self.configured
    }

    async fn send_interactive_card(&self, params: SendCardParams) -> Result<String, ApiError> {
        self.record(Call::SendCard {
            chat_id: params.chat_id.as_str().to_string(),
            card_json: params.card_json,
            reply_message_id: params.reply_target.message_id,
            in_thread: params.reply_target.in_thread,
        });
        if let Some(error) = self.take_send_error() {
            return Err(error);
        }
        Ok(self.next("om_card"))
    }

    async fn patch_interactive_card(&self, params: PatchCardParams) -> Result<(), ApiError> {
        self.record(Call::PatchCard {
            card_message_id: params.card_message_id,
            card_json: params.card_json,
        });
        if let Some(error) = self.patch_error.lock().expect("poisoned").take() {
            return Err(error);
        }
        Ok(())
    }

    async fn send_text_message(&self, params: SendTextParams) -> Result<String, ApiError> {
        self.record(Call::SendText {
            chat_id: params.chat_id.as_str().to_string(),
            text: params.text,
            reply_message_id: params.reply_target.message_id,
            in_thread: params.reply_target.in_thread,
        });
        if let Some(error) = self.take_send_error() {
            return Err(error);
        }
        Ok(self.next("om_text"))
    }

    async fn send_markdown_card(&self, params: SendMarkdownCardParams) -> Result<String, ApiError> {
        self.record(Call::SendMarkdown {
            chat_id: params.chat_id.as_str().to_string(),
            markdown: params.markdown,
            summary: params.summary,
            reply_message_id: params.reply_target.message_id,
            in_thread: params.reply_target.in_thread,
        });
        if let Some(error) = self.take_send_error() {
            return Err(error);
        }
        Ok(self.next("om_md"))
    }

    async fn send_binding_prompt_card(&self, params: BindingPromptParams) -> Result<(), ApiError> {
        self.record(Call::SendBindingPrompt {
            open_id: params.open_id.as_str().to_string(),
            bind_url: params.bind_url,
        });
        if let Some(error) = self.binding_error.lock().expect("poisoned").take() {
            return Err(error);
        }
        Ok(())
    }

    async fn get_bot_info(
        &self,
        _credentials: InstallationCredentials,
    ) -> Result<BotInfo, ApiError> {
        Ok(BotInfo::default())
    }

    async fn get_message(
        &self,
        _credentials: InstallationCredentials,
        _message_id: &str,
    ) -> Result<Vec<LarkMessage>, ApiError> {
        Ok(Vec::new())
    }

    async fn list_chat_messages(
        &self,
        _credentials: InstallationCredentials,
        _params: ListMessagesParams,
    ) -> Result<Vec<LarkMessage>, ApiError> {
        Ok(Vec::new())
    }

    async fn download_message_resource(
        &self,
        _credentials: InstallationCredentials,
        _params: DownloadResourceParams,
    ) -> Result<DownloadedResource, ApiError> {
        Err(ApiError::NotConfigured)
    }

    async fn download_message_resource_stream(
        &self,
        _credentials: InstallationCredentials,
        _params: DownloadResourceParams,
    ) -> Result<DownloadedResourceStream, ApiError> {
        Err(ApiError::NotConfigured)
    }

    async fn batch_get_users(
        &self,
        _credentials: InstallationCredentials,
        _open_ids: Vec<String>,
    ) -> Result<HashMap<String, String>, ApiError> {
        Ok(HashMap::new())
    }

    async fn add_message_reaction(&self, params: AddReactionParams) -> Result<String, ApiError> {
        self.record(Call::AddReaction {
            message_id: params.message_id,
            emoji_type: params.emoji_type,
        });
        if let Some(error) = self.take_reaction_error() {
            return Err(error);
        }
        Ok(self.next("re"))
    }

    async fn delete_message_reaction(&self, params: DeleteReactionParams) -> Result<(), ApiError> {
        self.record(Call::DeleteReaction {
            message_id: params.message_id,
            reaction_id: params.reaction_id,
        });
        if let Some(error) = self.delete_error.lock().expect("poisoned").take() {
            return Err(error);
        }
        Ok(())
    }
}

// =====================================================================
// 绑定令牌铸币的替身
// =====================================================================

/// 铸币替身（上游 `BindingTokenMinter`）：记下每次铸币的三个入参，返回固定明文。
#[derive(Debug, Default)]
pub(crate) struct FakeMinter {
    /// 每次铸币的 `(workspace, installation, open_id)`。
    pub(crate) minted: Mutex<Vec<(Id, Id, String)>>,
    /// 铸币失败的原因脚本。
    pub(crate) failure: Mutex<Option<String>>,
}

impl FakeMinter {
    /// 空替身。
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// 记下来的铸币请求。
    pub(crate) fn requests(&self) -> Vec<(Id, Id, String)> {
        self.minted.lock().expect("poisoned").clone()
    }
}

#[async_trait]
impl crate::lark::replier::BindingTokenMinter for FakeMinter {
    async fn mint(
        &self,
        workspace_id: Id,
        installation_id: Id,
        open_id: &str,
    ) -> Result<crate::lark::replier::MintedBinding, String> {
        self.minted.lock().expect("poisoned").push((
            workspace_id,
            installation_id,
            open_id.to_string(),
        ));
        if let Some(reason) = self.failure.lock().expect("poisoned").clone() {
            return Err(reason);
        }
        Ok(crate::lark::replier::MintedBinding {
            raw: "raw-binding-token".to_string(),
            expires_at: Utc::now() + chrono::Duration::minutes(15),
        })
    }
}

// =====================================================================
// 装配件
// =====================================================================

/// 一条入站消息的信封（M7-12 的形状：`raw` 里带 `create_time` 与派生字段）。
pub(crate) fn inbound_message(
    chat_id: &str,
    chat_type: ChatType,
    message_id: &str,
    thread_id: &str,
    create_time: &str,
) -> mc_core::channel::message::InboundMessage {
    mc_core::channel::message::InboundMessage {
        event_id: "ev-1".to_string(),
        message_id: message_id.to_string(),
        source: mc_core::channel::message::Source {
            channel_type: crate::lark::resolvers::TYPE_LARK,
            chat_id: chat_id.to_string(),
            chat_type,
            sender_id: "ou_sender".to_string(),
            sender_stable_id: "on_sender".to_string(),
            thread_id: thread_id.to_string(),
        },
        kind: mc_core::channel::message::MessageKind::Text,
        text: "hello".to_string(),
        command_text: "hello".to_string(),
        has_selected_context: false,
        media_refs: Vec::new(),
        reply_to: None,
        addressed_to_bot: true,
        force_fresh: false,
        skip_agent_run: false,
        raw: json!({
            "event_type": "im.message.receive_v1",
            "message_id": message_id,
            "chat_id": chat_id,
            "create_time": create_time,
        }),
    }
}

/// 一条 `ResolvedInstallation`（把安装投影塞进 `platform`）。
pub(crate) fn resolved(
    installation: &LarkInstallation,
) -> crate::engine::resolvers::ResolvedInstallation {
    let mut resolved = crate::engine::resolvers::ResolvedInstallation::new(
        installation.id,
        installation.workspace_id,
        installation.agent_id,
        installation.installer_user_id,
        crate::lark::resolvers::TYPE_LARK,
        true,
    );
    resolved.platform = Some(Arc::new(installation.clone()));
    resolved
}

/// 一条判决（M7-12 的 `DispatchResult`，出站回复器的输入）。
pub(crate) fn dispatch(
    outcome: crate::lark::resolvers::Outcome,
) -> crate::lark::resolvers::DispatchResult {
    crate::lark::resolvers::DispatchResult {
        outcome,
        sender_open_id: "ou_sender".to_string(),
        ..crate::lark::resolvers::DispatchResult::default()
    }
}
