//! [`super`]（事件载荷解码器）的用例 —— 上游 `ws_frame_decoder_test.go` 的等价集合。
//!
//! ⚠️ 上游那批用例里**摊平 / 提及改写**那一半（`TestLarkJSONFrameDecoderPostMessageFlattened`
//! / `…MentionPlaceholderRewrite` / `…GroupMentionDiscrimination`）**不在本片**：那些 helper
//! （`content_flatten.go` / `mention.go`）归 M7-12（`docs/32` §28 的交接表）。
//! 本片钉的是**解码器本身**：信封三分支 + 字段抽取 + `chat_type` 归一化 + `raw` 逐字。

use serde_json::json;

use super::*;
use crate::lark::types::ChatType;

fn decode(payload: &str) -> Result<DecodeOutcome, DecodeError> {
    LarkJsonFrameDecoder::new().decode(payload.as_bytes())
}

fn message(payload: &str) -> LarkInboundEvent {
    match decode(payload).expect("应当解码成功") {
        DecodeOutcome::Message(event) => *event,
        DecodeOutcome::Ignored => panic!("这条载荷应当被认出来"),
    }
}

/// 一条 p2p 文本事件（字段最全的那一版）。
const P2P_TEXT_PAYLOAD: &str = r#"{
  "schema": "2.0",
  "header": {
    "event_id": "ev-1",
    "event_type": "im.message.receive_v1",
    "create_time": "1700000000000",
    "app_id": "cli_app_x",
    "tenant_key": "tenant-1"
  },
  "event": {
    "sender": {
      "sender_id": { "open_id": "ou_sender", "union_id": "on_sender", "user_id": "u-1" },
      "sender_type": "user",
      "tenant_key": "tenant-1"
    },
    "message": {
      "message_id": "om-1",
      "chat_id": "oc-1",
      "chat_type": "p2p",
      "message_type": "text",
      "content": "{\"text\":\"hello\"}",
      "create_time": "1700000000000",
      "parent_id": "om-parent",
      "root_id": "om-root",
      "thread_id": "omt-1"
    }
  }
}"#;

// =====================================================================
// 一、正路：字段逐个抽出来
// =====================================================================

#[test]
fn a_p2p_text_event_is_decoded_field_by_field() {
    let event = message(P2P_TEXT_PAYLOAD);
    assert_eq!(event.event_type, EVENT_TYPE_MESSAGE_RECEIVE);
    assert_eq!(event.event_id, "ev-1");
    assert_eq!(event.app_id, "cli_app_x");
    assert_eq!(event.tenant_key, "tenant-1");
    assert_eq!(event.chat_id.as_str(), "oc-1");
    assert_eq!(event.chat_type, ChatType::P2p);
    assert_eq!(event.message_id, "om-1");
    assert_eq!(event.sender_open_id.as_str(), "ou_sender");
    assert_eq!(event.sender_union_id, "on_sender");
    assert_eq!(event.message_type, "text");
    // `content` **原样透传**（Lark 双重编码的字符串）：摊平是 M7-12 的事。
    assert_eq!(event.content, r#"{"text":"hello"}"#);
    assert!(event.mentions.is_empty());
    assert_eq!(event.create_time, "1700000000000");
    assert_eq!(event.parent_id, "om-parent");
    assert_eq!(event.root_id, "om-root");
    assert_eq!(event.thread_id, "omt-1");

    // 三个派生访问器。
    assert!(event.is_threaded());
    assert!(event.is_reply());
    assert!(event.is_direct());
}

/// `raw` 是信封**逐字**：解析器要读的 `app_id` / `event_type` / `create_time` 都在里面，
/// 平台加字段也不会在这里丢掉。
#[test]
fn raw_keeps_the_verbatim_envelope() {
    let event = message(P2P_TEXT_PAYLOAD);
    assert_eq!(event.raw["schema"], json!("2.0"));
    assert_eq!(event.raw["header"]["app_id"], json!("cli_app_x"));
    assert_eq!(
        event.raw["header"]["event_type"],
        json!("im.message.receive_v1")
    );
    assert_eq!(event.raw["header"]["create_time"], json!("1700000000000"));
    assert_eq!(event.raw["event"]["message"]["message_id"], json!("om-1"));
    // 平台新加的字段不会被丢掉（上游 `json.RawMessage` 的等价物）。
    assert_eq!(event.raw["header"]["brand_new_field"], json!(null));
}

#[test]
fn a_group_event_carries_the_mentions_and_normalizes_to_group() {
    let payload = json!({
        "schema": "2.0",
        "header": { "event_type": EVENT_TYPE_MESSAGE_RECEIVE, "app_id": "cli_app_x" },
        "event": {
            "sender": { "sender_id": { "open_id": "ou_sender" }, "sender_type": "user" },
            "message": {
                "message_id": "om-2",
                "chat_id": "oc-group",
                "chat_type": "group",
                "message_type": "post",
                "content": "{\"title\":\"t\"}",
                "mentions": [
                    { "key": "@_user_1", "id": { "open_id": "ou_bot", "union_id": "on_bot" }, "name": "Bot" },
                    { "key": "@_user_10", "id": { "open_id": "ou_bob" }, "name": "Bob" }
                ]
            }
        }
    })
    .to_string();

    let event = message(&payload);
    assert_eq!(event.chat_type, ChatType::Group);
    assert!(!event.is_direct());
    assert!(!event.is_threaded());
    assert!(!event.is_reply());
    assert_eq!(event.mentions.len(), 2);
    // WS 形状的提及是**三字段嵌套对象**（与 REST 侧 `types::LarkMessageMention` 的裸
    // `open_id` 字串不同 —— M7-10 在 `types.rs` 里逐字记过这条）。
    assert_eq!(event.mentions[0].key, "@_user_1");
    assert_eq!(event.mentions[0].id.open_id, "ou_bot");
    assert_eq!(event.mentions[0].id.union_id, "on_bot");
    assert_eq!(event.mentions[0].name, "Bot");
    assert_eq!(event.mentions[1].key, "@_user_10");
    assert_eq!(event.mentions[1].id.union_id, "");
}

#[test]
fn a_media_message_keeps_the_raw_content_for_the_ingest_side() {
    let payload = json!({
        "schema": "2.0",
        "header": { "event_type": EVENT_TYPE_MESSAGE_RECEIVE },
        "event": {
            "message": {
                "message_id": "om-3",
                "chat_id": "oc-1",
                "chat_type": "p2p",
                "message_type": "image",
                "content": "{\"image_key\":\"img-1\"}"
            }
        }
    })
    .to_string();
    let event = message(&payload);
    assert_eq!(event.message_type, "image");
    assert_eq!(event.content, r#"{"image_key":"img-1"}"#);
    // 缺字段一律回落空（不是错误：Lark 并不保证每个字段都出现）。
    assert_eq!(event.event_id, "");
    assert_eq!(event.sender_open_id.as_str(), "");
    assert_eq!(event.sender_union_id, "");
    assert_eq!(event.parent_id, "");
}

// =====================================================================
// 二、三分支：忽略 / 错误
// =====================================================================

#[test]
fn a_heartbeat_shaped_payload_is_ignored() {
    // 合法 JSON，但没有 `header.event_type` ⇒ 静默丢弃（连接器仍会 ACK 200）。
    assert_eq!(decode(r#"{"schema":"2.0"}"#), Ok(DecodeOutcome::Ignored));
    assert_eq!(
        decode(r#"{"schema":"2.0","header":{"event_type":"im.chat.access_event_v1"}}"#),
        Ok(DecodeOutcome::Ignored)
    );
    // 空载荷同样是"心跳形状"。
    assert_eq!(
        LarkJsonFrameDecoder::new().decode(&[]),
        Ok(DecodeOutcome::Ignored)
    );
}

/// 老式 webhook v1 信封的 `type`：长连接上不用，但**防御性接受**（上游逐字）。
#[test]
fn a_legacy_event_callback_envelope_is_still_accepted() {
    let payload = json!({
        "type": "event_callback",
        "header": { "event_type": EVENT_TYPE_MESSAGE_RECEIVE },
        "event": { "message": { "message_id": "om-4", "chat_id": "oc-1", "chat_type": "p2p" } }
    })
    .to_string();
    let event = message(&payload);
    assert_eq!(event.message_id, "om-4");

    // 但**别**的 `type` 取值一律丢弃。
    let other = json!({
        "type": "card_action",
        "header": { "event_type": EVENT_TYPE_MESSAGE_RECEIVE },
        "event": { "message": { "message_id": "om-5" } }
    })
    .to_string();
    assert_eq!(decode(&other), Ok(DecodeOutcome::Ignored));
}

#[test]
fn a_malformed_envelope_is_an_error() {
    assert_eq!(decode("not json"), Err(DecodeError::Envelope));
    assert_eq!(decode("[1,2,3]"), Err(DecodeError::Envelope));
    // 信封是好的，但 `header` 不是对象。
    assert_eq!(decode(r#"{"header":"nope"}"#), Err(DecodeError::Envelope));
}

#[test]
fn an_event_callback_without_a_payload_is_an_error() {
    let payload = json!({
        "schema": "2.0",
        "header": { "event_type": EVENT_TYPE_MESSAGE_RECEIVE }
    })
    .to_string();
    assert_eq!(decode(&payload), Err(DecodeError::EmptyEvent));
}

#[test]
fn a_malformed_event_body_is_an_error_naming_only_the_type() {
    let payload = json!({
        "schema": "2.0",
        "header": { "event_type": EVENT_TYPE_MESSAGE_RECEIVE },
        "event": { "message": { "message_id": 42 } }
    })
    .to_string();
    match decode(&payload) {
        Err(DecodeError::Event { event_type }) => {
            assert_eq!(event_type, EVENT_TYPE_MESSAGE_RECEIVE);
        }
        other => panic!("期望 Event 错误，得到 {other:?}"),
    }
}

/// 错误文案**不含载荷内容**（载荷里是用户正文）。
#[test]
fn decode_errors_never_echo_the_payload() {
    let payload = r#"{"header":{"event_type":"im.message.receive_v1"},"event":{"message":{"message_id":42,"content":"TOP-SECRET"}}}"#;
    let error = decode(payload).expect_err("必须失败");
    let rendered = error.to_string();
    assert!(!rendered.contains("TOP-SECRET"), "{rendered}");
    assert!(!rendered.contains("message_id"), "{rendered}");
}

// =====================================================================
// 三、`chat_type` 的归一化（失败关闭）
// =====================================================================

#[test]
fn chat_type_normalization_fails_closed_to_group() {
    assert_eq!(normalize_chat_type("p2p"), ChatType::P2p);
    assert_eq!(normalize_chat_type("P2P"), ChatType::P2p, "上游 ToLower");
    assert_eq!(normalize_chat_type("group"), ChatType::Group);
    // 空 / 未知一律**归群**：群会走 engine 的"必须 @ bot"过滤，最坏是漏一条闲聊。
    assert_eq!(normalize_chat_type(""), ChatType::Group);
    assert_eq!(normalize_chat_type("topic"), ChatType::Group);
}

#[test]
fn an_unknown_chat_type_decodes_as_group() {
    let payload = json!({
        "header": { "event_type": EVENT_TYPE_MESSAGE_RECEIVE },
        "event": { "message": { "message_id": "om-6", "chat_id": "oc-1", "chat_type": "topic" } }
    })
    .to_string();
    assert_eq!(message(&payload).chat_type, ChatType::Group);
}

/// `Debug` 会带上正文（用例需要它）—— 这里只把这条**事实**钉住，好让"别插进日志"这条纪律
/// 有据可依：接线方（[`crate::lark::ws_connector`]）确实只插值 `message_id` / `event_type`。
#[test]
fn the_event_debug_does_contain_the_body_so_it_stays_out_of_logs() {
    let event = message(P2P_TEXT_PAYLOAD);
    assert!(format!("{event:?}").contains("hello"));
}
