//! 归一化的用例（`inbound.rs` 的 `#[cfg(test)] mod tests;`）。
//!
//! 四份上游 golden 用 `include_str!` 钉住（编译期嵌入 ⇒ 用例不依赖运行目录）：
//! `testdata/{quoted_card_link_group,quoted_card_link_private,quoted_bot_channels,
//! quoted_interactive_card}.json`。它们与上游 `inbound_card_test.go` 的期望值是**同一对**
//! （上游钉的是 `msg.Text` 的逐字结果，本文件同款）。

use serde_json::Value;

use super::{
    append_image_placeholder, decode_dingtalk_raw, dingtalk_chat_type,
    exact_dingtalk_bot_mention_spans, inbound_from_callback, inbound_from_callback_with_bot_name,
    normalize_dingtalk_bot_mention, BotCallbackData, ChatType, DingtalkMediaResource, RichTextItem,
    IMAGE_PLACEHOLDER, IMAGE_UNAVAILABLE, RICH_TEXT_UNAVAILABLE, TYPE_DINGTALK,
};

const QUOTED_INTERACTIVE_CARD: &str = include_str!("../testdata/quoted_interactive_card.json");
const QUOTED_CARD_LINK_GROUP: &str = include_str!("../testdata/quoted_card_link_group.json");
const QUOTED_CARD_LINK_PRIVATE: &str = include_str!("../testdata/quoted_card_link_private.json");
const QUOTED_BOT_CHANNELS: &str = include_str!("../testdata/quoted_bot_channels.json");

fn decode(wire: &str) -> BotCallbackData {
    serde_json::from_str(wire).expect("golden callback is a valid callback")
}

fn decode_value(value: &Value) -> BotCallbackData {
    serde_json::from_value(value.clone()).expect("fixture callback is a valid callback")
}

/// 上游 `TestInboundQuotedCardObservedSnapshot` 的逐字对应。
#[test]
fn quoted_interactive_card_golden_decodes() {
    let callback = decode(QUOTED_INTERACTIVE_CARD);
    let message = inbound_from_callback(Some(&callback), "app").expect("has a sender");
    assert_eq!(
        message.text,
        "> Bananas. Literal text: . Link: https://example.com/a_(b)?x=a_b+c&y=2#part_2\n\
         > [quoted content unavailable]\n\n\
         explain"
    );
    assert_eq!(message.command_text, "explain");
    assert!(!message.force_fresh);
    assert!(message.has_selected_context);
    assert_eq!(
        message
            .reply_to
            .as_ref()
            .map(|reply| reply.message_id.as_str()),
        Some("selected-bot-message")
    );
}

/// 上游 `TestInboundQuotedCardCapturedLinks` 的逐字对应（`private` / `group` 两份 golden）。
#[test]
fn quoted_card_link_goldens_decode() {
    let want = "> DingTalk quote testParagraph one: Apples are red.\
                Paragraph two: Bananas are yellow. Marker: QUOTE-CONTEXT-7429.\
                Literal HTML: keep this visible\n\
                > [quoted content unavailable]\n\
                > Link: https://example.com/a_(b)?x=a_b+c&y=2#part_2\n\
                > [quoted content unavailable]\n\
                > Paragraph three: Cherries are sweet.\n\n\
                QUOTE-CAPTURE-7429";
    for (name, wire) in [
        ("private", QUOTED_CARD_LINK_PRIVATE),
        ("group", QUOTED_CARD_LINK_GROUP),
    ] {
        let callback = decode(wire);
        let message = inbound_from_callback(Some(&callback), "app").expect("has a sender");
        assert_eq!(message.text, want, "golden {name}");
        assert_eq!(message.command_text, "QUOTE-CAPTURE-7429", "golden {name}");
        assert!(!message.force_fresh, "golden {name}");
        assert!(message.has_selected_context, "golden {name}");
    }
}

/// 上游 `TestInboundQuotedBotChannelFixtures` 的逐字对应：四份形态 × 两种**容器**
/// （`text` / `richText`）都给出同一段引用块。第二个容器正是"引用元信息放在 `content` 里"
/// 的那条路径（见 `quoted::dingtalk_reply_metadata`）。
#[test]
fn quoted_bot_channel_goldens_decode_in_both_containers() {
    let cases: Vec<Value> =
        serde_json::from_str(QUOTED_BOT_CHANNELS).expect("fixture is an array of cases");
    assert_eq!(cases.len(), 4, "四份配对探针");
    for case in &cases {
        let name = case["name"].as_str().expect("case name").to_string();
        for current_kind in ["text", "richText"] {
            // 上游用例的那种重写：把 `text` 里的引用快照搬到 `content` 里，
            // 于是引用元信息走的是"`content` 那一半"的路径。
            let mut wire = case["callback"].clone();
            if current_kind == "richText" {
                let replied = wire["text"]["repliedMsg"].take();
                wire["msgtype"] = serde_json::json!("richText");
                wire["text"] = serde_json::json!({});
                wire["content"] = serde_json::json!({
                    "richText": [{ "text": "引用测试" }],
                    "isReplyMsg": true,
                    "repliedMsg": replied,
                });
            }
            let callback = decode_value(&wire);
            let message = inbound_from_callback(Some(&callback), "app")
                .unwrap_or_else(|| panic!("{name}/{current_kind} 有发送者"));
            assert_eq!(
                message.text, "> DingTalk source probe 001.\n\n引用测试",
                "{name}/{current_kind}"
            );
            assert_eq!(message.command_text, "引用测试", "{name}/{current_kind}");
            assert_eq!(
                message
                    .reply_to
                    .as_ref()
                    .map(|reply| reply.message_id.as_str()),
                Some("selected"),
                "{name}/{current_kind}"
            );
        }
    }
}

/// 卡片里的内联图片与正文里的占位符位次（上游 `TestInboundCardInlineAndMediaOrder`）。
#[test]
fn card_inline_and_media_order_is_preserved() {
    let callback = decode(
        r#"{"senderStaffId":"sender","conversationType":"1","msgtype":"richText",
            "content":{"richText":[{"text":"current"},
                                  {"type":"picture","downloadCode":"current-code"}],
                       "repliedMsg":{"msgType":"interactiveCard","content":{"cardContent":[
                          {"elementType":"RICHTEXT","children":[
                            {"elementType":"UNKNOWN","value":"source"},
                            {"elementType":"TEXT","value":"一只"},
                            {"elementType":"TEXT","value":"木质调色板"},
                            {"elementType":"TEXT","value":"，literal [Image]"},
                            {"elementType":"IMAGE","downloadCode":"selected-code"},
                            {"elementType":"TEXT","value":"after"}]}]}}}}"#,
    );
    let message = inbound_from_callback(Some(&callback), "app").expect("has a sender");
    assert!(
        message
            .text
            .contains("一只木质调色板，literal [Image]\n> [Image]\n> after"),
        "卡片内联与媒体位次被改：{:?}",
        message.text
    );
    assert!(!message.text.contains("unavailable"), "{:?}", message.text);
    let raw = decode_dingtalk_raw(&message).expect("raw is ours");
    assert_eq!(raw.media.len(), 1);
    // 引用块里的 `[Image]`（含用户手打的那一个）各占一位 ⇒ 当前媒体是第 3 次出现（0 起 2）。
    assert_eq!(raw.media[0].reference, "current-code");
    assert_eq!(raw.media[0].inline_index, 2);
}

// =====================================================================
// 归一化的形态判据
// =====================================================================

/// 没有发送者 staff id ⇒ **不进核心**（系统消息 / bot 自己发的）。
#[test]
fn a_callback_without_a_sender_never_reaches_the_core() {
    let mut callback = decode(r#"{"conversationId":"c","msgtype":"text","text":{"content":"hi"}}"#);
    assert!(inbound_from_callback(Some(&callback), "app").is_none());
    assert!(inbound_from_callback(None, "app").is_none());
    callback.sender_staff_id = "staff".into();
    assert!(inbound_from_callback(Some(&callback), "app").is_some());
}

/// `msgtype` → 归一化种类的映射（`audio` / `video` / `file` / 未知各自的占位文案）。
#[test]
fn message_kinds_and_placeholder_texts() {
    for (msgtype, kind, text) in [
        ("audio", "audio", "[Audio message]"),
        ("video", "video", "[Video message]"),
        ("file", "file", "[File]"),
        ("futureKind", "unknown", "[Unsupported DingTalk message]"),
    ] {
        let callback = decode(&format!(
            r#"{{"senderStaffId":"s","conversationId":"c","msgtype":"{msgtype}"}}"#
        ));
        let message = inbound_from_callback(Some(&callback), "app").expect("has a sender");
        assert_eq!(message.kind.as_str(), kind, "msgtype {msgtype}");
        assert_eq!(message.text, text, "msgtype {msgtype}");
        assert_eq!(message.command_text, text, "msgtype {msgtype}");
    }
}

/// 直聊恒为"在跟 bot 说话"；群聊只有 `isInAtList` 才算。
#[test]
fn addressed_to_bot_follows_conversation_type_and_at_list() {
    for (conversation_type, in_at_list, wanted) in [
        ("1", false, true),
        ("1", true, true),
        ("2", false, false),
        ("2", true, true),
        // 未知取值按群处理（失败关闭的方向）。
        ("9", true, true),
        ("9", false, false),
    ] {
        let callback = decode(&format!(
            r#"{{"senderStaffId":"s","conversationId":"c","conversationType":"{conversation_type}",
                 "isInAtList":{in_at_list},"msgtype":"text","text":{{"content":"hi"}}}}"#
        ));
        let message = inbound_from_callback(Some(&callback), "app").expect("has a sender");
        assert_eq!(
            message.addressed_to_bot, wanted,
            "type={conversation_type} at={in_at_list}"
        );
    }
    assert_eq!(dingtalk_chat_type("1"), ChatType::P2p);
    assert_eq!(dingtalk_chat_type("2"), ChatType::Group);
    assert_eq!(dingtalk_chat_type(""), ChatType::Group);
}

/// 富文本：有序的文本 run 与图片项各自计数、`command_text` **不含**图片占位符。
#[test]
fn rich_text_keeps_order_and_separates_command_text() {
    let callback = decode(
        r#"{"senderStaffId":"s","conversationId":"c","conversationType":"1","msgtype":"richText",
            "content":{"richText":[{"text":"[Image] user typed [Image]\n"},
                                   {"type":"picture","downloadCode":"pic-1"},
                                   {"text":"tail"},
                                   {"type":"picture","pictureDownloadCode":"pic-2"}]}}"#,
    );
    let message = inbound_from_callback(Some(&callback), "app").expect("has a sender");
    assert_eq!(message.kind.as_str(), "image");
    // 用户手打的两处 `[Image]` 已把计数推到 2 ⇒ 第一个平台图片是第 3 次出现；
    // `appendImagePlaceholder` 只保证“前面非空就补一个换行”，所以手打的尾部换行会留下一个空行。
    assert_eq!(
        message.text,
        format!("[Image] user typed [Image]\n\n{IMAGE_PLACEHOLDER}\ntail\n{IMAGE_PLACEHOLDER}")
    );
    assert_eq!(message.command_text, "[Image] user typed [Image]\ntail");
    let raw = decode_dingtalk_raw(&message).expect("raw is ours");
    assert_eq!(
        raw.media,
        vec![
            DingtalkMediaResource::at("pic-1", "", 2),
            DingtalkMediaResource::at("pic-2", "", 3)
        ]
    );
    // 冻结的当前可见正文与 `text` 一致（Router 消费控制指令之前的值）。
    assert_eq!(raw.current_text, message.text);
}

/// 富文本的空 / 坏载荷 ⇒ 显式不可用占位（**不**静默丢弃）。`errorCode 20001` 的超额回调
/// 会把 `text` / `content` 整个剥掉，那是这条路径在生产里的形态。
#[test]
fn unreadable_media_becomes_an_explicit_placeholder() {
    for (msgtype, wanted, content) in [
        ("picture", IMAGE_UNAVAILABLE, Value::Null),
        ("richText", RICH_TEXT_UNAVAILABLE, Value::Null),
        ("picture", IMAGE_UNAVAILABLE, serde_json::json!({})),
        (
            "picture",
            IMAGE_UNAVAILABLE,
            serde_json::json!({"downloadCode": "", "pictureDownloadCode": ""}),
        ),
        (
            "richText",
            RICH_TEXT_UNAVAILABLE,
            serde_json::json!({"richText": []}),
        ),
        (
            "richText",
            RICH_TEXT_UNAVAILABLE,
            serde_json::json!({"richText": "not-an-array"}),
        ),
    ] {
        let callback = decode(&format!(
            r#"{{"senderStaffId":"s","conversationId":"c","conversationType":"1",
                 "msgtype":"{msgtype}","content":{content}}}"#
        ));
        let message = inbound_from_callback(Some(&callback), "app").expect("有发送者 ⇒ 进核心");
        assert_eq!(message.text, wanted, "msgtype {msgtype} content {content}");
        assert_eq!(message.command_text, wanted);
        let raw = decode_dingtalk_raw(&message).expect("raw is ours");
        assert!(raw.media.is_empty(), "媒体不可用时不许有引用");
    }
}

/// 富文本里的控制指令：可见正文里剥掉它、`force_fresh` 置位（Router 仍从 `command_text`
/// 读真值）。
#[test]
fn rich_text_control_layout_is_stripped_from_the_visible_body() {
    let callback = decode(
        r#"{"senderStaffId":"s","conversationId":"c","conversationType":"1","msgtype":"richText",
            "content":{"richText":[{"text":"/clear running report"},
                                   {"type":"picture","downloadCode":"pic"}]}}"#,
    );
    let message = inbound_from_callback(Some(&callback), "app").expect("has a sender");
    assert_eq!(message.command_text, "/clear running report");
    assert_eq!(message.text, format!("running report\n{IMAGE_PLACEHOLDER}"));
    assert!(message.force_fresh);
}

/// `/new` 只记路由轮换，**不**置 `force_fresh`（两条控制指令的语义不同）。
#[test]
fn new_chat_does_not_force_a_fresh_session() {
    let callback = decode(
        r#"{"senderStaffId":"s","conversationId":"c","conversationType":"1","msgtype":"richText",
            "content":{"richText":[{"text":"/new hello"}]}}"#,
    );
    let message = inbound_from_callback(Some(&callback), "app").expect("has a sender");
    assert_eq!(message.command_text, "/new hello");
    assert_eq!(message.text, "hello");
    assert!(!message.force_fresh);
}

/// 纯文本消息里的控制指令**不**在这里剥（那是共享 Router 的事），但 `[Image]` 的计数同款。
#[test]
fn plain_text_keeps_the_control_directive_for_the_router() {
    let callback = decode(
        r#"{"senderStaffId":"s","conversationId":"c","conversationType":"1","msgtype":"text",
            "text":{"content":"  /clear hello  "}}"#,
    );
    let message = inbound_from_callback(Some(&callback), "app").expect("has a sender");
    assert_eq!(message.text, "/clear hello");
    assert_eq!(message.command_text, "/clear hello");
    assert!(!message.force_fresh);
    assert_eq!(message.source.channel_type, TYPE_DINGTALK);
}

// =====================================================================
// bot 提及剥离（失败关闭）
// =====================================================================

fn group_callback(is_in_at_list: bool) -> BotCallbackData {
    decode(&format!(
        r#"{{"senderStaffId":"s","conversationId":"c","conversationType":"2",
             "isInAtList":{is_in_at_list},"msgtype":"text","text":{{"content":"x"}}}}"#
    ))
}

/// 四道闸门：群 + 被 @ + 有名字 + 两侧空白/串端。
#[test]
fn bot_mention_is_removed_only_under_all_four_gates() {
    // 群 + @ + 名字 ⇒ 剥掉；**首尾空白由调用方 `TrimSpace` 收掉**（本函数的输出逐字是
    // 上游 `removeDingTalkBotMention` 的输出：只动提及两侧的那一个空白）。
    assert_eq!(
        normalize_dingtalk_bot_mention(&group_callback(true), "  @Bot-DEV run it  ", "Bot-DEV")
            .trim(),
        "run it"
    );
    // 提及在串首 ⇒ 后面的那个空白收掉、后面的原文保留。
    assert_eq!(
        normalize_dingtalk_bot_mention(&group_callback(true), "@Bot-DEV   run", "Bot-DEV"),
        "run"
    );
    // 中间出现 ⇒ 只剥名字、留一个空格。
    assert_eq!(
        normalize_dingtalk_bot_mention(&group_callback(true), "please @Bot-DEV run", "Bot-DEV"),
        "please run"
    );
    // 标点后的同名**不**剥（标点可能是另一个显示名的一部分 ⇒ 失败关闭）。
    assert_eq!(
        normalize_dingtalk_bot_mention(&group_callback(true), "@Bot-DEV-2 hi", "Bot-DEV"),
        "@Bot-DEV-2 hi"
    );
    // 没有名字 ⇒ 原样保留。
    assert_eq!(
        normalize_dingtalk_bot_mention(&group_callback(true), "@Bot-DEV run", "  "),
        "@Bot-DEV run"
    );
    // 直聊 ⇒ 不剥（`/clear` 一类出现在直聊里时平台不带 mention 信封）。
    let mut p2p = group_callback(true);
    p2p.conversation_type = "1".into();
    assert_eq!(
        normalize_dingtalk_bot_mention(&p2p, "@Bot-DEV run", "Bot-DEV"),
        "@Bot-DEV run"
    );
    // 群但没被 @ ⇒ 不剥。
    assert_eq!(
        normalize_dingtalk_bot_mention(&group_callback(false), "@Bot-DEV run", "Bot-DEV"),
        "@Bot-DEV run"
    );
}

/// 跨 run 的提及（富文本）：DingTalk 可能把那个 run 放在媒体之前或之后。
#[test]
fn bot_mention_spans_are_found_across_runs() {
    let runs = vec![
        "one".to_string(),
        "@Bot-DEV and".to_string(),
        "three".to_string(),
    ];
    // 第三个 run 里的 `@Bot-DEV and`：右侧 ` and` 是空白 ⇒ 成立。
    assert_eq!(
        exact_dingtalk_bot_mention_spans(&runs, "Bot-DEV"),
        vec![super::MentionSpan {
            run: 1,
            start: 0,
            end: 8
        }]
    );
    // run 中间出现两次时两个跨度都算（从后往前删 ⇒ 前面的下标不失效）。
    let twice = vec!["@Bot hi @Bot".to_string()];
    assert_eq!(
        exact_dingtalk_bot_mention_spans(&twice, "Bot"),
        vec![
            super::MentionSpan {
                run: 0,
                start: 0,
                end: 4
            },
            super::MentionSpan {
                run: 0,
                start: 8,
                end: 12
            }
        ]
    );
}

/// 多字节正文里的提及：下标是**字节**下标，切分不能落在字符中间。
#[test]
fn bot_mention_removal_is_utf8_safe() {
    assert_eq!(
        normalize_dingtalk_bot_mention(&group_callback(true), "中文 @机器人 请继续", "机器人"),
        "中文 请继续"
    );
    assert_eq!(
        normalize_dingtalk_bot_mention(&group_callback(true), "@机器人：继续", "机器人"),
        "@机器人：继续"
    );
}

/// 富文本路径里的提及剥离（`inbound_rich_text` 走的是同一个函数）。
#[test]
fn rich_text_mention_is_removed_from_its_own_run() {
    let callback = decode(
        r#"{"senderStaffId":"s","conversationId":"c","conversationType":"2","isInAtList":true,
            "msgtype":"richText","content":{"richText":[{"type":"picture","downloadCode":"p"},
                                                         {"text":"@Bot hello"}]}}"#,
    );
    let message =
        inbound_from_callback_with_bot_name(Some(&callback), "app", "Bot").expect("有发送者");
    assert_eq!(message.text, format!("{IMAGE_PLACEHOLDER}\nhello"));
    let raw = decode_dingtalk_raw(&message).expect("raw is ours");
    // 图片在正文里是**第一个**占位符（提及剥离不改媒体位次）。
    assert_eq!(raw.media[0].inline_index, 0);
}

// =====================================================================
// 平台原始载荷
// =====================================================================

/// `raw` 的往返：AppKey 盖章、群标题、媒体与位次都活着；解不开的 `raw` 是**基础设施失败**。
#[test]
fn raw_event_round_trips_and_a_missing_raw_is_infrastructure_failure() {
    let callback = decode(
        r#"{"senderStaffId":"s","conversationId":"c","conversationType":"2",
            "conversationTitle":"  team  ","isInAtList":true,"msgtype":"text",
            "text":{"content":"hi"}}"#,
    );
    let message = inbound_from_callback(Some(&callback), "app-key").expect("has a sender");
    let raw = decode_dingtalk_raw(&message).expect("raw is ours");
    assert_eq!(raw.app_id, "app-key");
    assert_eq!(raw.conversation_title, "team");
    assert_eq!(raw.current_text, "hi");
    assert!(raw.media.is_empty());

    let mut stripped = message;
    stripped.raw = Value::Null;
    assert!(decode_dingtalk_raw(&stripped).is_err());
    stripped.raw = serde_json::json!({ "app_id": 42 });
    assert!(decode_dingtalk_raw(&stripped).is_err());
}

/// 富文本项的宽容解码（上游 `rich_text_item` 的三条降级路径）。
#[test]
fn rich_text_items_degrade_locally() {
    // 未知类型 ⇒ 整个项不可用。
    assert_eq!(
        RichTextItem::from_value(&serde_json::json!({"type": "video", "text": "x"}), false).text,
        RICH_TEXT_UNAVAILABLE
    );
    // 坏节点（不是对象）⇒ 不可用，但**不**影响相邻项（由 `rich_text_content` 那层保证）。
    assert_eq!(
        RichTextItem::from_value(&serde_json::json!(42), false).text,
        RICH_TEXT_UNAVAILABLE
    );
    // 既无文本也无图片码 ⇒ 不可用。
    assert_eq!(
        RichTextItem::from_value(&serde_json::json!({}), false).text,
        RICH_TEXT_UNAVAILABLE
    );
    // 只有图片码 ⇒ 文本留空、`has_picture` 为真。
    let picture = RichTextItem::from_value(
        &serde_json::json!({"type": "picture", "downloadCode": "d"}),
        false,
    );
    assert!(picture.has_picture());
    assert!(picture.text.is_empty());
    assert_eq!(picture.codes(), ("d".to_string(), String::new()));
    // 引用快照的 `content` 包装 + `msgType` 别名（只在 `quoted = true` 时生效）。
    let quoted = RichTextItem::from_value(
        &serde_json::json!({"msgType": "text", "content": {"text": "wrapped"}}),
        true,
    );
    assert_eq!(quoted.text, "wrapped");
    assert_eq!(quoted.kind, "text");
    let current = RichTextItem::from_value(
        &serde_json::json!({"msgType": "text", "content": {"text": "wrapped"}}),
        false,
    );
    assert_eq!(current.text, RICH_TEXT_UNAVAILABLE, "当前消息不吃引用别名");
}

/// 图片占位符的拼装（首项不前置换行、每项后随换行）。
#[test]
fn image_placeholder_placement() {
    let mut body = String::new();
    append_image_placeholder(&mut body);
    append_image_placeholder(&mut body);
    // 第二次追加时"前面非空"⇒ 先补一个换行（上游 `appendImagePlaceholder` 逐字）。
    assert_eq!(
        body,
        format!("{IMAGE_PLACEHOLDER}\n\n{IMAGE_PLACEHOLDER}\n")
    );
}
