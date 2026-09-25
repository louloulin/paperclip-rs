//! [`super`] 的共用夹具（拆文件是门 ⑩ 的 800 行硬限）。

use mc_core::channel::message::{InboundMessage, MessageKind};
use mc_core::id::Id;
use std::sync::Arc;

use crate::engine::resolvers::ResolvedInstallation;
use crate::lark::feishu_channel::LarkInboundMessage;
use crate::lark::resolvers::LarkInstallation;
use crate::lark::types::{ChatId, OpenId, Region};

pub(super) fn payload(message_type: &str, content: &str) -> LarkInboundMessage {
    LarkInboundMessage {
        event_type: "im.message.receive_v1".to_string(),
        event_id: "ev-1".to_string(),
        app_id: "cli_test".to_string(),
        tenant_key: String::new(),
        chat_id: ChatId::new("oc-1"),
        chat_type: crate::lark::types::ChatType::P2p,
        message_id: "om-1".to_string(),
        sender_open_id: OpenId::new("ou_sender"),
        sender_union_id: String::new(),
        message_type: message_type.to_string(),
        content: content.to_string(),
        mentions: Vec::new(),
        create_time: "1700000000000".to_string(),
        parent_id: String::new(),
        root_id: String::new(),
        thread_id: String::new(),
        envelope: serde_json::Value::Null,
        body: String::new(),
        command_body: String::new(),
        addressed_to_bot: false,
        force_fresh_session: false,
        has_selected_context: false,
    }
}

/// 一条挂载资源的 `InboundMessage`（`raw` 就是 payload 的 JSON）。
pub(super) fn envelope(payload: &LarkInboundMessage) -> InboundMessage {
    InboundMessage {
        event_id: payload.event_id.clone(),
        message_id: payload.message_id.clone(),
        source: mc_core::channel::message::Source {
            channel_type: crate::lark::resolvers::TYPE_LARK,
            chat_id: payload.chat_id.as_str().to_string(),
            chat_type: payload.chat_type,
            sender_id: payload.sender_open_id.as_str().to_string(),
            sender_stable_id: String::new(),
            thread_id: String::new(),
        },
        kind: MessageKind::Image,
        text: String::new(),
        command_text: String::new(),
        has_selected_context: false,
        media_refs: Vec::new(),
        reply_to: None,
        addressed_to_bot: false,
        force_fresh: false,
        skip_agent_run: false,
        raw: serde_json::to_value(payload).expect("序列化 payload"),
    }
}

pub(super) fn installation(union_id: Option<&str>) -> LarkInstallation {
    LarkInstallation {
        id: Id::new(),
        workspace_id: Id::new(),
        agent_id: Id::new(),
        app_id: "cli_test".to_string(),
        app_secret_encrypted: b"plain".to_vec(),
        tenant_key: None,
        bot_open_id: OpenId::new("ou_bot"),
        bot_union_id: union_id.map(str::to_string),
        region: Region::Feishu,
        installer_user_id: Id::new(),
        status: "active".to_string(),
    }
}

pub(super) fn resolved(installation: LarkInstallation) -> ResolvedInstallation {
    ResolvedInstallation {
        id: installation.id,
        workspace_id: installation.workspace_id,
        agent_id: installation.agent_id,
        installer_user_id: installation.installer_user_id,
        active: true,
        kind: crate::lark::resolvers::TYPE_LARK,
        platform: Some(Arc::new(installation)),
    }
}
