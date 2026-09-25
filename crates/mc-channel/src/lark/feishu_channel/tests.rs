//! [`super`]（入站归一化 + 事件汇）的用例。
//!
//! `Channel` 的四个方法 / 工厂 / `union_id` / `region` 的入站可见性在 [`channel`]，
//! 共用夹具在 [`fixtures`]；拆文件是门 ⑩ 的 800 行硬限。

mod channel;
mod fixtures;

use std::sync::Arc;

use mc_core::channel::message::ChatType;
use serde_json::json;

use super::*;
use crate::lark::params::InstallationCredentials;
use fixtures::*;

// =====================================================================
// 一、`from_event`：逐字段归一化
// =====================================================================

/// p2p 文本：正文摊平、`command_body` 快照、`addressed_to_bot` 恒假、信封逐字保留。
#[test]
fn p2p_text_event_is_normalized_field_by_field() {
    let installed = installation(Some("on_bot"));
    let normalized = LarkInboundMessage::from_event(
        event("om-1", "text", r#"{"text":"hello"}"#, ChatType::P2p),
        &installed,
    );
    assert_eq!(normalized.event_type, "im.message.receive_v1");
    assert_eq!(normalized.event_id, "ev-om-1");
    assert_eq!(normalized.app_id, "cli_test");
    assert_eq!(normalized.tenant_key, "tenant-1");
    assert_eq!(normalized.chat_id.as_str(), "oc-1");
    assert_eq!(normalized.chat_type, ChatType::P2p);
    assert_eq!(normalized.message_id, "om-1");
    assert_eq!(normalized.sender_open_id.as_str(), "ou_bob");
    assert_eq!(normalized.sender_union_id, "on_bob");
    assert_eq!(normalized.message_type, "text");
    assert_eq!(normalized.create_time, "1700000000000");
    assert_eq!(normalized.body, "hello");
    assert_eq!(normalized.command_body, "hello", "命令源 = 用户自己打的字");
    assert!(!normalized.addressed_to_bot, "p2p 恒假");
    assert!(!normalized.force_fresh_session);
    assert!(!normalized.has_selected_context);
    assert!(!normalized.is_reply());
    assert!(!normalized.is_threaded());
    assert!(!normalized.is_merge_forward());
    // 信封逐字（`raw`）。
    assert_eq!(
        normalized.envelope["header"]["event_type"],
        json!("im.message.receive_v1")
    );
}

/// 群聊 `@bot`：提及被剥掉、`addressed_to_bot` 置位；`command_body` 是**剥后**的正文。
#[test]
fn group_mention_is_stripped_and_addressed_flag_is_set() {
    let installed = installation(Some("on_bot"));
    let mut raw = event(
        "om-1",
        "text",
        r#"{"text":"@_user_1 总结一下"}"#,
        ChatType::Group,
    );
    raw.mentions = vec![bot_mention()];
    let normalized = LarkInboundMessage::from_event(raw, &installed);
    assert_eq!(normalized.body, "总结一下");
    assert_eq!(normalized.command_body, "总结一下");
    assert!(normalized.addressed_to_bot);
}

/// 群聊**没有** @ bot ⇒ `addressed_to_bot` 假（engine 据此丢弃）。
#[test]
fn group_message_without_a_mention_is_not_addressed() {
    let installed = installation(Some("on_bot"));
    let normalized = LarkInboundMessage::from_event(
        event("om-1", "text", r#"{"text":"随便聊聊"}"#, ChatType::Group),
        &installed,
    );
    assert!(!normalized.addressed_to_bot);
    assert_eq!(normalized.body, "随便聊聊");
}

/// 富文本 `post`：摊平 + `at` span 的占位被换成显示名。
#[test]
fn post_event_is_flattened_then_mention_resolved() {
    let installed = installation(Some("on_bot"));
    let content = r#"{"title":"周报","content":[[{"tag":"text","text":"看下这个"},{"tag":"a","href":"https://x.test","text":"链接"}]]}"#;
    let normalized =
        LarkInboundMessage::from_event(event("om-1", "post", content, ChatType::P2p), &installed);
    assert_eq!(normalized.body, "周报\n看下这个 链接 (https://x.test)");
    assert_eq!(normalized.command_body, normalized.body);
}

/// 媒体类型：正文只留方括号占位（下载归 [`super::super::media`]）。
#[test]
fn media_types_get_a_bracket_placeholder() {
    let installed = installation(None);
    for (message_type, content, expected) in [
        ("image", r#"{"image_key":"img_x"}"#, "[Image]"),
        ("file", r#"{"file_key":"fk"}"#, "[File]"),
        ("audio", r#"{"file_key":"fk"}"#, "[Audio]"),
        ("video", r#"{"file_key":"fk"}"#, "[Video]"),
        ("media", r#"{"file_key":"fk"}"#, "[Video]"),
    ] {
        let normalized = LarkInboundMessage::from_event(
            event("om-1", message_type, content, ChatType::P2p),
            &installed,
        );
        assert_eq!(normalized.body, expected, "{message_type}");
    }
}

/// `merge_forward` 与认不出的类型：正文留空（展开要一次 HTTP 往返，归富化器）。
#[test]
fn forward_and_unknown_types_get_an_empty_body() {
    let installed = installation(None);
    let forward = LarkInboundMessage::from_event(
        event(
            "om-1",
            "merge_forward",
            r#"{"content":"Merged and Forwarded Message"}"#,
            ChatType::P2p,
        ),
        &installed,
    );
    assert_eq!(forward.body, "");
    assert!(forward.is_merge_forward());

    let unknown = LarkInboundMessage::from_event(
        event("om-1", "totally_new", "{}", ChatType::P2p),
        &installed,
    );
    assert_eq!(unknown.body, "");
    assert!(!unknown.is_merge_forward());
}

/// 回复 / 话题的坐标被搬进归一化字段。
#[test]
fn reply_and_thread_coordinates_survive_normalization() {
    let installed = installation(None);
    let mut raw = event("om-1", "text", r#"{"text":"回复"}"#, ChatType::Group);
    raw.parent_id = "om-parent".to_string();
    raw.root_id = "om-root".to_string();
    raw.thread_id = "omt-1".to_string();
    let normalized = LarkInboundMessage::from_event(raw, &installed);
    assert!(normalized.is_reply());
    assert!(normalized.is_threaded());
    assert_eq!(normalized.parent_id, "om-parent");
    assert_eq!(normalized.thread_id, "omt-1");
}

// =====================================================================
// 二、`to_inbound_message`：跨平台信封
// =====================================================================

/// `source` / `kind` / `media_refs` / `reply_to` / `raw` 逐项对齐 `mc_core` 的形态纪律。
#[test]
fn normalized_message_maps_onto_the_cross_platform_envelope() {
    let installed = installation(Some("on_bot"));
    let mut raw = event("om-1", "image", r#"{"image_key":"img_x"}"#, ChatType::Group);
    raw.parent_id = "om-parent".to_string();
    raw.thread_id = "omt-1".to_string();
    let normalized = LarkInboundMessage::from_event(raw, &installed);

    let message = normalized.to_inbound_message().expect("应当能编码");
    assert_eq!(message.event_id, "ev-om-1");
    assert_eq!(message.message_id, "om-1");
    assert_eq!(message.source.channel_type, TYPE_LARK);
    assert_eq!(message.source.chat_id, "oc-1");
    assert_eq!(message.source.chat_type, ChatType::Group);
    assert_eq!(message.source.sender_id, "ou_bob");
    assert_eq!(
        message.source.sender_stable_id, "on_bob",
        "union_id 走稳定身份那一格"
    );
    assert_eq!(message.source.thread_id, "omt-1");
    assert_eq!(message.kind, mc_core::channel::message::MessageKind::Image);
    assert_eq!(message.text, "[Image]");
    assert_eq!(message.command_text, "[Image]");
    assert!(
        message.media_refs.is_empty(),
        "adapter **不得**预填 media_refs"
    );
    let reply = message.reply_to.expect("引用坐标应当保留");
    assert_eq!(reply.message_id, "om-parent");
    assert_eq!(reply.root_id, "");
    assert_eq!(message.raw["app_id"], json!("cli_test"), "raw 装整份载荷");
    assert_eq!(
        message.raw["envelope"]["schema"],
        json!("2.0"),
        "信封逐字留在 raw 里"
    );
}

/// 没有引用坐标 ⇒ `reply_to` 是 `None`（不是空结构）。
#[test]
fn missing_reply_coordinates_produce_no_reply_context() {
    let installed = installation(None);
    let normalized = LarkInboundMessage::from_event(
        event("om-1", "text", r#"{"text":"hi"}"#, ChatType::P2p),
        &installed,
    );
    assert!(normalized
        .to_inbound_message()
        .expect("编码")
        .reply_to
        .is_none());
}

// =====================================================================
// 三、事件汇：富化在投递**之前**
// =====================================================================

/// 一个把正文改成 `[enriched] …` 的假富化器（只证明**顺序**，不重复测富化本身）。
struct MarkingEnricher;

#[async_trait]
impl Enricher for MarkingEnricher {
    async fn enrich(
        &self,
        mut message: LarkInboundMessage,
        _credentials: &InstallationCredentials,
    ) -> LarkInboundMessage {
        message.body = format!("[enriched] {}", message.body);
        message.has_selected_context = true;
        message
    }
}

/// 接了富化器 ⇒ 投递给 handler 的正文是**富化之后**的（上游：在帧 ACK 之前富化）。
#[tokio::test]
async fn the_emitter_enriches_before_handing_to_the_engine() {
    let (recorder, shared) = handler(false);
    let emitter = LarkEventEmitter::new(
        installation(Some("on_bot")),
        credentials(),
        Some(Arc::new(MarkingEnricher) as Arc<dyn Enricher>),
        shared,
    );
    emitter
        .emit(event("om-1", "text", r#"{"text":"原文"}"#, ChatType::P2p))
        .await
        .expect("投递应当成功");

    let messages = recorder.messages.lock().expect("lock");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].text, "[enriched] 原文");
    assert!(messages[0].has_selected_context);
    // `command_text` **不**被富化污染（命令从用户自己打的字解析）。
    assert_eq!(messages[0].command_text, "原文");
}

/// 没接富化器 ⇒ 正文就是解码器的产物（`Enricher == nil` 的语义）。
#[tokio::test]
async fn without_an_enricher_the_body_passes_through() {
    let (recorder, shared) = handler(false);
    let emitter = LarkEventEmitter::new(installation(None), credentials(), None, shared);
    emitter
        .emit(event("om-1", "text", r#"{"text":"原文"}"#, ChatType::P2p))
        .await
        .expect("投递应当成功");
    assert_eq!(recorder.messages.lock().expect("lock")[0].text, "原文");
}

/// handler 报基础设施错 ⇒ **上抛**（连接器据此判"这次尝试失败"）。
#[tokio::test]
async fn handler_failure_propagates_as_an_infrastructure_error() {
    let (_recorder, shared) = handler(true);
    let emitter = LarkEventEmitter::new(installation(None), credentials(), None, shared);
    let error = emitter
        .emit(event("om-1", "text", r#"{"text":"hi"}"#, ChatType::P2p))
        .await
        .expect_err("应当上抛");
    assert_eq!(error.code(), "channel_storage_error");
    assert!(error.is_retryable(), "存储失败是可重试的");
}

/// `LarkEventEmitter` 的 `Debug` 不回显凭据与正文。
#[test]
fn emitter_debug_redacts_credentials() {
    let (_recorder, shared) = handler(false);
    let emitter = LarkEventEmitter::new(installation(Some("on_bot")), credentials(), None, shared);
    let rendered = format!("{emitter:?}");
    assert!(!rendered.contains("plain-secret"), "{rendered}");
    assert!(rendered.contains("<redacted>"), "{rendered}");
    assert!(rendered.contains("cli_test"), "app_id 不是秘密：{rendered}");
}
