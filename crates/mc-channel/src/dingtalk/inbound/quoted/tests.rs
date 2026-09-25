//! 引用渲染与引用上下文的用例（`inbound/quoted.rs` 的 `#[cfg(test)] mod tests;`）。
//!
//! 这个文件的表驱动部分是**上游 `inbound.go` 引用段**的判据底稿：`reply_to` 的两级退路、
//! 控制指令只在"未被触碰的正文"上剥、以及媒体位次的三处偏移。

use serde_json::json;

use super::{
    apply_dingtalk_reply_context, dingtalk_current_visible_text, dingtalk_reply_metadata,
    render_dingtalk_quoted_message, render_dingtalk_quoted_rich_text,
};
use crate::dingtalk::inbound::{
    decode_dingtalk_raw, inbound_from_callback, BotCallbackData, BotCallbackRepliedContent,
    BotCallbackRepliedMessage, DingtalkRawEvent, InboundMessage, MessageKind, IMAGE_PLACEHOLDER,
};

fn decode(wire: &str) -> BotCallbackData {
    serde_json::from_str(wire).expect("fixture callback is a valid callback")
}

fn snapshot(wire: &str) -> BotCallbackRepliedMessage {
    serde_json::from_str::<BotCallbackRepliedMessage>(wire)
        .unwrap_or_else(|_| serde_json::from_value(serde_json::json!({})).expect("空快照"))
}

/// `msgType` 判别式逐条的渲染（`renderDingTalkQuotedMessage` 的七条分支）。
#[test]
fn quoted_message_renders_per_msg_type() {
    for (wire, want) in [
        // 文本：原样引用。
        (
            r#"{"msgType":"text","content":{"text":"hello"}}"#,
            "> hello".to_string(),
        ),
        // 引用里含 `||` ⇒ 保守判为不可用（当前输入不过这道闸门）。
        (
            r#"{"msgType":"text","content":{"text":"a || b"}}"#,
            "> [quoted content unavailable]".to_string(),
        ),
        // 图片：保留位置；快照的 `text` 没有文档化的 caption 含义 ⇒ 显式说明它被扣下。
        (
            r#"{"msgType":"picture","content":{"downloadCode":"d","text":"caption"}}"#,
            // 正文在图片占位符之后已经以换行结尾，而那条提示自己带一个前导换行 ⇒ 空行由
            // `format_quoted_message` 渲染成 `>`（上游 `FormatQuotedMessage` 逐字）。
            format!("> {IMAGE_PLACEHOLDER}\n>\n> [quoted content unavailable]"),
        ),
        // 图片但拿不到下载码 ⇒ 显式不可用（不是"编一个空图片"）。
        (
            r#"{"msgType":"image","content":{}}"#,
            "> [Image unavailable]".to_string(),
        ),
        // 文件：有名字带名字。
        (
            r#"{"msgType":"file","content":{"fileName":"a.pdf"}}"#,
            "> [File: a.pdf]".to_string(),
        ),
        (r#"{"msgType":"file","content":{}}"#, "> [File]".to_string()),
        // 语音：有识别文本用它，没有就占位。
        (
            r#"{"msgType":"audio","content":{"recognition":"转写"}}"#,
            "> 转写".to_string(),
        ),
        (
            r#"{"msgType":"audio","content":{}}"#,
            "> [Audio message]".to_string(),
        ),
        (
            r#"{"msgType":"video","content":{}}"#,
            "> [Video message]".to_string(),
        ),
        // 认不出的类型 / 没有 msgType ⇒ 不可用。
        (
            r#"{"msgType":"future","content":{}}"#,
            "> [quoted content unavailable]".to_string(),
        ),
        (
            r#"{"content":{}}"#,
            "> [quoted content unavailable]".to_string(),
        ),
        // 交互卡片：走卡片投影。
        (
            r#"{"msgType":"interactiveCard","content":{"cardContent":[{"elementType":"RICHTEXT","children":[{"elementType":"TEXT","value":"card body"}]}]}}"#,
            "> card body".to_string(),
        ),
    ] {
        let (block, media) = render_dingtalk_quoted_message(Some(&snapshot(wire)));
        assert_eq!(block, want, "fixture {wire}");
        // 只有"有下载码的图片"才产生媒体引用。
        let wanted_media = wire.contains("\"msgType\":\"picture\"");
        assert_eq!(media.len(), usize::from(wanted_media), "fixture {wire}");
    }
    // 没有快照 ⇒ 没有引用块（不是"空的引用块"）。
    assert_eq!(
        render_dingtalk_quoted_message(None),
        (String::new(), Vec::new())
    );
}

/// 作者名里的 `[Image]` **不会**变成媒体标记（转义挡住了它），而位次仍然按"数出来的差"算。
///
/// 上游 `FormatQuotedMessage` 的注释逐字点名了这条：名字是纯文本，转义它同时防止
/// `[Image]` 一类名字变成 adapter 的媒体标记。所以这里断言的是**转义生效**（前缀里不含
/// `[Image]`）且媒体位次**不被前缀推走**；而那行"数出来的差"仍然要按计数写，不能改成常量 0
/// —— 它是唯一能在格式化规则变化时自动保持正确的形态。
#[test]
fn a_placeholder_like_author_name_never_becomes_a_media_marker() {
    let snapshot =
        snapshot(r#"{"msgType":"picture","senderNick":"[Image]","content":{"downloadCode":"d"}}"#);
    let (block, media) = render_dingtalk_quoted_message(Some(&snapshot));
    assert_eq!(block, "> **\\[Image\\]:**\n>\n> [Image]");
    // 整个引用块里只有**引用正文**那一个占位符（前缀那个被转义拆开了）。
    assert_eq!(block.matches(IMAGE_PLACEHOLDER).count(), 1);
    assert_eq!(media.len(), 1);
    assert_eq!(media[0].inline_index, 0);
}

/// 富文本引用：只有媒体的快照会**显式说明**散文被扣下，且不猜预览的位置。
#[test]
fn quoted_rich_text_states_missing_prose_without_guessing() {
    let content = BotCallbackRepliedContent {
        text: "summary that must not be paired".to_string(),
        rich_text: vec![crate::dingtalk::inbound::RichTextItem {
            kind: "picture".to_string(),
            download_code: "d".to_string(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let (body, media) = render_dingtalk_quoted_rich_text(&content, 0);
    assert_eq!(
        body,
        format!("[quoted content unavailable]\n\n{IMAGE_PLACEHOLDER}")
    );
    assert_eq!(media.len(), 1);
    assert_eq!(media[0].inline_index, 0);

    // 空富文本 ⇒ 不可用，没有媒体。
    let empty = BotCallbackRepliedContent::default();
    assert_eq!(
        render_dingtalk_quoted_rich_text(&empty, 3),
        ("[quoted content unavailable]".to_string(), Vec::new())
    );
}

/// 引用元信息的两处位置：`text` 优先，缺失字段从 `content` 补。
#[test]
fn reply_metadata_prefers_text_and_fills_from_content() {
    // 两处都有：`text` 赢。
    let both = decode(
        r#"{"senderStaffId":"s","conversationId":"c","msgtype":"text",
            "text":{"content":"x","isReplyMsg":false,"repliedMsg":{"msgType":"text","content":{"text":"from-text"}}},
            "content":{"isReplyMsg":true,"repliedMsg":{"msgType":"text","content":{"text":"from-content"}}}}"#,
    );
    let metadata = dingtalk_reply_metadata(&both);
    // ⚠️ `content` **补**缺失字段（上游逐字：`if !metadata.IsReplyMsg { … }`）⇒ 两处都有时
    // `isReplyMsg` 会被 `content` 的 `true` 补上，而**快照**仍然取 `text` 的那一份。
    assert!(metadata.is_reply_msg);
    assert_eq!(
        metadata.replied_msg.expect("text 快照").content.text,
        "from-text"
    );

    // `text` 空、`isReplyMsg && repliedMsg` 不全 ⇒ 从 `content` 补。
    let from_content = decode(
        r#"{"senderStaffId":"s","conversationId":"c","msgtype":"richText",
            "content":{"richText":[{"text":"x"}],"isReplyMsg":true,
                       "repliedMsg":{"msgType":"text","content":{"text":"from-content"}}}}"#,
    );
    let metadata = dingtalk_reply_metadata(&from_content);
    assert!(metadata.is_reply_msg);
    assert_eq!(
        metadata.replied_msg.expect("content 快照").content.text,
        "from-content"
    );

    // `content` 解不进元信息结构（这里是字符串）⇒ 整段丢弃、保持 `text` 的判决。
    let undecodable = decode(
        r#"{"senderStaffId":"s","conversationId":"c","msgtype":"text",
            "text":{"content":"x"},"content":"plain-string"}"#,
    );
    let metadata = dingtalk_reply_metadata(&undecodable);
    assert!(!metadata.is_reply_msg);
    assert!(metadata.replied_msg.is_none());
}

/// `reply_to` 的两级退路：快照 id 优先、退到 `originalMsgId`；两者都空**也**记下引用关系。
#[test]
fn reply_to_falls_back_to_the_original_message_id() {
    let with_snapshot = decode(
        r#"{"senderStaffId":"s","conversationId":"c","originalMsgId":"original","msgtype":"text",
            "text":{"content":"x","isReplyMsg":true,
                    "repliedMsg":{"msgId":"snapshot-id","msgType":"text","content":{"text":"p"}}}}"#,
    );
    let message = inbound_from_callback(Some(&with_snapshot), "app").expect("has a sender");
    assert_eq!(
        message.reply_to.as_ref().map(|r| r.message_id.as_str()),
        Some("snapshot-id")
    );

    let without_snapshot_id = decode(
        r#"{"senderStaffId":"s","conversationId":"c","originalMsgId":"original","msgtype":"text",
            "text":{"content":"x","isReplyMsg":true,
                    "repliedMsg":{"msgType":"text","content":{"text":"p"}}}}"#,
    );
    let message = inbound_from_callback(Some(&without_snapshot_id), "app").expect("has a sender");
    assert_eq!(
        message.reply_to.as_ref().map(|r| r.message_id.as_str()),
        Some("original")
    );

    // 显式引用但没有快照、也没有 originalMsgId：坐标为空，但"用户选了东西"这个事实留下。
    let bare_quote = decode(
        r#"{"senderStaffId":"s","conversationId":"c","msgtype":"text",
            "text":{"content":"x","isReplyMsg":true}}"#,
    );
    let message = inbound_from_callback(Some(&bare_quote), "app").expect("has a sender");
    assert_eq!(
        message.reply_to.as_ref().map(|r| r.message_id.as_str()),
        Some("")
    );
    assert!(message.has_selected_context);

    // 裸的 `originalMsgId`（没有 isReplyMsg、没有快照）**不**算"选中了东西"。
    let bare_thread = decode(
        r#"{"senderStaffId":"s","conversationId":"c","originalMsgId":"original","msgtype":"text",
            "text":{"content":"x"}}"#,
    );
    let message = inbound_from_callback(Some(&bare_thread), "app").expect("has a sender");
    assert!(message.reply_to.is_some());
    assert!(!message.has_selected_context);
    assert_eq!(message.text, "x");
}

/// 控制指令：引用富化之后 Router 再也比不了 `text` 与 `commandText` ⇒ 可见的那一份在这里剥，
/// `force_fresh` 由 adapter 记下。
#[test]
fn a_quoted_clear_is_stripped_from_the_visible_instruction() {
    let callback = decode(
        r#"{"senderStaffId":"s","conversationId":"c","msgtype":"text",
            "text":{"content":"/clear continue please","isReplyMsg":true,
                    "repliedMsg":{"msgType":"text","content":{"text":"parent"}}}}"#,
    );
    let message = inbound_from_callback(Some(&callback), "app").expect("has a sender");
    assert_eq!(message.command_text, "/clear continue please");
    assert_eq!(message.text, "> parent\n\ncontinue please");
    assert!(message.force_fresh);

    // 裸 `/clear` 引用：可见正文只剩引用块。
    let bare = decode(
        r#"{"senderStaffId":"s","conversationId":"c","msgtype":"text",
            "text":{"content":"/clear","isReplyMsg":true,
                    "repliedMsg":{"msgType":"text","content":{"text":"parent"}}}}"#,
    );
    let message = inbound_from_callback(Some(&bare), "app").expect("has a sender");
    assert_eq!(message.text, "> parent");
    assert!(message.force_fresh);
}

/// 引用块把当前媒体的位次整体右移；有媒体 ⇒ 消息种类是图片。
#[test]
fn the_quoted_block_shifts_the_current_media_indices() {
    let callback = decode(
        r#"{"senderStaffId":"s","conversationId":"c","msgtype":"richText",
            "content":{"richText":[{"text":"current [Image]"},
                                   {"type":"picture","downloadCode":"pic"}],
                       "isReplyMsg":true,
                       "repliedMsg":{"msgType":"text","content":{"text":"parent"}}}}"#,
    );
    let message = inbound_from_callback(Some(&callback), "app").expect("has a sender");
    assert_eq!(message.kind, MessageKind::Image);
    let raw = decode_dingtalk_raw(&message).expect("raw is ours");
    // 引用块本身不含占位符 ⇒ 偏移 0；当前正文里用户手打了一个 ⇒ 平台图片是第 2 次出现。
    assert_eq!(raw.media[0].inline_index, 1);
    assert!(message.text.starts_with("> parent\n\ncurrent [Image]"));
}

/// `dingtalk_current_visible_text` 在 bot 提及剥离**之后**、控制指令消费**之前**取值。
#[test]
fn current_visible_text_is_frozen_before_the_router_consumes_commands() {
    let callback = decode(
        r#"{"senderStaffId":"s","conversationId":"c","conversationType":"1","msgtype":"text",
            "text":{"content":"  /clear go  "}}"#,
    );
    let message = inbound_from_callback(Some(&callback), "app").expect("has a sender");
    assert_eq!(dingtalk_current_visible_text(&message), "/clear go");
    let raw = decode_dingtalk_raw(&message).expect("raw is ours");
    assert_eq!(raw.current_text, "/clear go");
}

/// 直接驱动 `apply_dingtalk_reply_context`：没有引用元信息时**什么都不做**。
#[test]
fn applying_reply_context_without_a_quote_is_a_no_op() {
    let callback = decode(
        r#"{"senderStaffId":"s","conversationId":"c","msgtype":"text","text":{"content":"plain"}}"#,
    );
    let mut message = InboundMessage {
        event_id: "e".into(),
        message_id: "e".into(),
        source: crate::dingtalk::inbound::inbound_from_callback(Some(&callback), "app")
            .expect("has a sender")
            .source,
        kind: MessageKind::Text,
        text: "plain".into(),
        command_text: "plain".into(),
        has_selected_context: false,
        media_refs: Vec::new(),
        reply_to: None,
        addressed_to_bot: true,
        force_fresh: false,
        skip_agent_run: false,
        raw: serde_json::Value::Null,
    };
    let mut raw = DingtalkRawEvent {
        app_id: "app".into(),
        ..Default::default()
    };
    apply_dingtalk_reply_context(&callback, &mut message, &mut raw);
    assert!(message.reply_to.is_none());
    assert!(!message.has_selected_context);
    assert_eq!(message.text, "plain");
    assert!(raw.media.is_empty());
    assert_eq!(json!({}), json!({}));
}
