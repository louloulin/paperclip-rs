//! [`super`] 的共用夹具：安装行投影 / 事件 / 入站入口替身 / 脚本化连接器 / 假出站客户端。
//!
//! 拆文件是**门 ⑩** 的 800 行硬限（`tests.rs` 一度 946 行）；与
//! `ws_connector/tests/{harness,supervised}.rs`、`dingtalk/outbound/tests/http.rs` 同一手法。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use mc_core::channel::message::{ChatType, InboundMessage};
use mc_core::id::Id;
use serde_json::json;

use super::super::LarkInstallation;
use crate::channel::{ChannelConfig, ChannelError, ChannelResult};
use crate::lark::client::{ApiClient, ApiError, StubApiClient};
use crate::lark::params::{AppSecret, InstallationCredentials, SendTextParams};
use crate::lark::types::{ChatId, OpenId, Region};
use crate::lark::ws_connector::{EventConnector, EventEmitter, SessionOutcome, StopHandle};
use crate::lark::ws_frame_decoder::{LarkEventMention, LarkInboundEvent, LarkSenderId};
use crate::message::{InboundHandler, SharedInboundHandler};

pub(super) fn installation(union_id: Option<&str>) -> LarkInstallation {
    LarkInstallation {
        id: Id::new(),
        workspace_id: Id::new(),
        agent_id: Id::new(),
        app_id: "cli_test".to_string(),
        app_secret_encrypted: b"plain-secret".to_vec(),
        tenant_key: Some("tenant-1".to_string()),
        bot_open_id: OpenId::new("ou_bot"),
        bot_union_id: union_id.map(str::to_string),
        region: Region::Feishu,
        installer_user_id: Id::new(),
        status: "active".to_string(),
    }
}

pub(super) fn credentials() -> InstallationCredentials {
    InstallationCredentials::new("cli_test", AppSecret::new("plain-secret"))
}

pub(super) fn event(
    message_id: &str,
    message_type: &str,
    content: &str,
    chat_type: ChatType,
) -> LarkInboundEvent {
    LarkInboundEvent {
        event_type: "im.message.receive_v1".to_string(),
        event_id: format!("ev-{message_id}"),
        app_id: "cli_test".to_string(),
        tenant_key: "tenant-1".to_string(),
        chat_id: ChatId::new("oc-1"),
        chat_type,
        message_id: message_id.to_string(),
        sender_open_id: OpenId::new("ou_bob"),
        sender_union_id: "on_bob".to_string(),
        message_type: message_type.to_string(),
        content: content.to_string(),
        mentions: Vec::new(),
        create_time: "1700000000000".to_string(),
        parent_id: String::new(),
        root_id: String::new(),
        thread_id: String::new(),
        raw: json!({"schema": "2.0", "header": {"event_type": "im.message.receive_v1"}}),
    }
}

pub(super) fn bot_mention() -> LarkEventMention {
    LarkEventMention {
        key: "@_user_1".to_string(),
        id: LarkSenderId {
            open_id: "ou_bot".to_string(),
            union_id: "on_bot".to_string(),
            user_id: String::new(),
        },
        name: "Bot".to_string(),
    }
}

/// 记账的入站入口（engine 的共享 handler）。
pub(super) struct RecordingHandler {
    pub(super) messages: Mutex<Vec<InboundMessage>>,
    pub(super) fail: bool,
}

#[async_trait]
impl InboundHandler for RecordingHandler {
    async fn handle(&self, message: InboundMessage) -> ChannelResult<()> {
        self.messages.lock().expect("lock").push(message);
        if self.fail {
            return Err(ChannelError::Storage {
                message: "db down".to_string(),
            });
        }
        Ok(())
    }
}

pub(super) fn handler(fail: bool) -> (Arc<RecordingHandler>, SharedInboundHandler) {
    let recorder = Arc::new(RecordingHandler {
        messages: Mutex::new(Vec::new()),
        fail,
    });
    (Arc::clone(&recorder), recorder as SharedInboundHandler)
}

/// 脚本化的连接器：按顺序 emit 事件，然后给出脚本的收尾。
pub(super) struct ScriptedConnector {
    pub(super) events: Vec<LarkInboundEvent>,
    pub(super) outcome: Option<SessionOutcome>,
    pub(super) error: Option<ChannelError>,
    pub(super) runs: Mutex<usize>,
    pub(super) credentials_seen: Mutex<Vec<Region>>,
}

#[async_trait]
impl EventConnector for ScriptedConnector {
    async fn run(
        &self,
        creds: &InstallationCredentials,
        emit: Arc<dyn EventEmitter>,
        stop: StopHandle,
    ) -> ChannelResult<SessionOutcome> {
        *self.runs.lock().expect("lock") += 1;
        self.credentials_seen
            .lock()
            .expect("lock")
            .push(creds.region);
        for event in &self.events {
            emit.emit(event.clone()).await?;
            if stop.is_stopped() {
                return Ok(SessionOutcome::Cancelled);
            }
        }
        if let Some(error) = self.error.clone() {
            return Err(error);
        }
        Ok(self.outcome.unwrap_or(SessionOutcome::Closed))
    }
}

pub(super) fn connector(
    events: Vec<LarkInboundEvent>,
    outcome: Option<SessionOutcome>,
) -> Arc<ScriptedConnector> {
    Arc::new(ScriptedConnector {
        events,
        outcome,
        error: None,
        runs: Mutex::new(0),
        credentials_seen: Mutex::new(Vec::new()),
    })
}

/// 记账的假出站客户端。
#[derive(Default)]
pub(super) struct RecordingApi {
    pub(super) sent: Mutex<Vec<SendTextParams>>,
    pub(super) error: Option<ApiError>,
}

#[async_trait]
impl ApiClient for RecordingApi {
    fn is_configured(&self) -> bool {
        true
    }

    async fn send_text_message(&self, params: SendTextParams) -> Result<String, ApiError> {
        if let Some(error) = self.error.clone() {
            return Err(error);
        }
        self.sent.lock().expect("lock").push(params);
        Ok("om-sent".to_string())
    }

    // 其余面转发给替身（本片的用例不碰它们）。
    async fn send_interactive_card(&self, params: SendCardParamsAlias) -> Result<String, ApiError> {
        StubApiClient::new().send_interactive_card(params).await
    }
    async fn patch_interactive_card(
        &self,
        params: crate::lark::params::PatchCardParams,
    ) -> Result<(), ApiError> {
        StubApiClient::new().patch_interactive_card(params).await
    }
    async fn send_markdown_card(
        &self,
        params: crate::lark::params::SendMarkdownCardParams,
    ) -> Result<String, ApiError> {
        StubApiClient::new().send_markdown_card(params).await
    }
    async fn send_binding_prompt_card(
        &self,
        params: crate::lark::params::BindingPromptParams,
    ) -> Result<(), ApiError> {
        StubApiClient::new().send_binding_prompt_card(params).await
    }
    async fn get_bot_info(
        &self,
        credentials: InstallationCredentials,
    ) -> Result<crate::lark::types::BotInfo, ApiError> {
        StubApiClient::new().get_bot_info(credentials).await
    }
    async fn get_message(
        &self,
        credentials: InstallationCredentials,
        message_id: &str,
    ) -> Result<Vec<crate::lark::types::LarkMessage>, ApiError> {
        StubApiClient::new()
            .get_message(credentials, message_id)
            .await
    }
    async fn list_chat_messages(
        &self,
        credentials: InstallationCredentials,
        params: crate::lark::params::ListMessagesParams,
    ) -> Result<Vec<crate::lark::types::LarkMessage>, ApiError> {
        StubApiClient::new()
            .list_chat_messages(credentials, params)
            .await
    }
    async fn download_message_resource(
        &self,
        credentials: InstallationCredentials,
        params: crate::lark::params::DownloadResourceParams,
    ) -> Result<crate::lark::http_client::resource::DownloadedResource, ApiError> {
        StubApiClient::new()
            .download_message_resource(credentials, params)
            .await
    }
    async fn download_message_resource_stream(
        &self,
        credentials: InstallationCredentials,
        params: crate::lark::params::DownloadResourceParams,
    ) -> Result<crate::lark::http_client::resource::DownloadedResourceStream, ApiError> {
        StubApiClient::new()
            .download_message_resource_stream(credentials, params)
            .await
    }
    async fn batch_get_users(
        &self,
        credentials: InstallationCredentials,
        open_ids: Vec<String>,
    ) -> Result<std::collections::HashMap<String, String>, ApiError> {
        StubApiClient::new()
            .batch_get_users(credentials, open_ids)
            .await
    }
    async fn add_message_reaction(
        &self,
        params: crate::lark::params::AddReactionParams,
    ) -> Result<String, ApiError> {
        StubApiClient::new().add_message_reaction(params).await
    }
    async fn delete_message_reaction(
        &self,
        params: crate::lark::params::DeleteReactionParams,
    ) -> Result<(), ApiError> {
        StubApiClient::new().delete_message_reaction(params).await
    }
}

/// `SendCardParams` 的别名（避免在本文件的 use 列表里塞一堆只出现一次的类型）。
pub(super) type SendCardParamsAlias = crate::lark::params::SendCardParams;

/// 把 `LarkInstallation` 装成工厂配置（`channel_installation.config` 的 lark 形状）。
pub(super) fn factory_config(raw: serde_json::Value) -> ChannelConfig {
    ChannelConfig {
        kind: mc_core::channel::ChannelKind::Lark,
        raw,
        installation_id: Some(Id::new()),
        handler: None,
    }
}
