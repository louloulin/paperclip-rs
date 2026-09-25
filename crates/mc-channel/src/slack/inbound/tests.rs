//! 入站归一化的用例（上游 `inbound_test.go` 的逐条移植）。

use super::*;

// ---- 事件归一化（上游 `inbound_test.go` 的逐条移植） ----

#[allow(clippy::needless_pass_by_value)]
fn events_api(inner: Value) -> EventsApiEvent {
    serde_json::from_value(serde_json::json!({
        "team_id": "T1",
        "api_app_id": "A1",
        "event": inner,
    }))
    .expect("events api event")
}

/// 上游 `TestInboundFromMessage_DM`。
#[test]
fn inbound_from_message_dm() {
    let event = events_api(serde_json::json!({
        "type": "message", "user": "UALICE", "text": "hello bot",
        "channel": "D123", "channel_type": "im", "ts": "1700000000.000100",
    }));
    let message = inbound_from_event(&event, "UBOT").expect("DM 可摄入");
    assert_eq!(message.source.chat_type, ChatType::P2p);
    assert!(message.addressed_to_bot);
    assert_eq!(message.source.channel_type, TYPE_SLACK);
    assert_eq!(message.message_id, "1700000000.000100");
    assert_eq!(message.event_id, message.message_id);
    assert_eq!(message.source.sender_id, "UALICE");
    assert_eq!(message.source.chat_id, "D123");
    assert_eq!(message.text, "hello bot");
    assert_eq!(message.command_text, "hello bot");
    assert!(message.media_refs.is_empty(), "adapter 不得预填 media_refs");
    // team_id 必须在 raw 里，安装解析器要按它路由。
    let raw: RawEvent = serde_json::from_value(message.raw).expect("decode raw");
    assert_eq!(raw.team_id, "T1");
    assert_eq!(raw.event_type, "message");
    assert_eq!(raw.api_app_id, "A1");
}

/// 上游 `TestInboundFromMessage_ChannelMention` / `_ChannelNoMention` / `TestMpimRequiresMention`。
#[test]
fn group_addressing_follows_mentions() {
    let mention = events_api(serde_json::json!({
        "type": "message", "user": "UALICE", "text": "<@UBOT> create an issue",
        "channel": "C123", "channel_type": "channel", "ts": "1.1",
    }));
    let message = inbound_from_event(&mention, "UBOT").expect("可摄入");
    assert_eq!(message.source.chat_type, ChatType::Group);
    assert!(message.addressed_to_bot);
    assert_eq!(message.text, "create an issue", "提及被剥掉");
    assert_eq!(message.command_text, "create an issue");

    let plain = events_api(serde_json::json!({
        "type": "message", "user": "UALICE", "text": "just chatting with the team",
        "channel": "C123", "channel_type": "channel", "ts": "1.2",
    }));
    let message = inbound_from_event(&plain, "UBOT").expect("仍要摄入（群过滤在 engine）");
    assert!(!message.addressed_to_bot);

    let mpim = events_api(serde_json::json!({
        "type": "message", "user": "UALICE", "text": "team lunch?",
        "channel": "G123", "channel_type": "mpim", "ts": "1.3",
    }));
    let message = inbound_from_event(&mpim, "UBOT").expect("mpim 仍摄入");
    assert_eq!(message.source.chat_type, ChatType::Group);
    assert!(!message.addressed_to_bot, "多人 DM 里的闲聊不算对 bot 说话");
}

/// 上游 `TestInboundFromMessage_ThreadReply`。
#[test]
fn thread_reply_carries_the_root() {
    let event = events_api(serde_json::json!({
        "type": "message", "user": "UALICE", "text": "<@UBOT> follow up",
        "channel": "C123", "channel_type": "channel",
        "ts": "1700000000.000500", "thread_ts": "1700000000.000400",
    }));
    let message = inbound_from_event(&event, "UBOT").expect("可摄入");
    assert_eq!(message.source.thread_id, "1700000000.000400");
    let reply = message.reply_to.expect("线程回复带引用");
    assert_eq!(reply.message_id, "1700000000.000400");
    assert_eq!(reply.root_id, "1700000000.000400");
}

/// 上游 `TestInboundFromMessage_SkipsBotAndOwnAndEdits`。
#[test]
fn bot_own_and_edit_events_are_skipped() {
    let cases: &[(&str, Value)] = &[
        (
            "own message",
            serde_json::json!({"type":"message","user":"UBOT","text":"hi","channel":"D1","channel_type":"im","ts":"1.1"}),
        ),
        (
            "other bot",
            serde_json::json!({"type":"message","user":"UX","bot_id":"B1","text":"hi","channel":"C1","ts":"1.2"}),
        ),
        (
            "bot_message subtype",
            serde_json::json!({"type":"message","subtype":"bot_message","text":"hi","channel":"C1","ts":"1.3"}),
        ),
        (
            "edit",
            serde_json::json!({"type":"message","user":"UALICE","subtype":"message_changed","text":"hi","channel":"C1","ts":"1.4"}),
        ),
        (
            "delete",
            serde_json::json!({"type":"message","user":"UALICE","subtype":"message_deleted","channel":"C1","ts":"1.5"}),
        ),
        (
            "empty user",
            serde_json::json!({"type":"message","text":"hi","channel":"C1","ts":"1.6"}),
        ),
    ];
    for (name, inner) in cases {
        assert!(
            inbound_from_event(&events_api(inner.clone()), "UBOT").is_none(),
            "{name} 不该被摄入"
        );
    }
}

/// 上游 `TestInboundFromAppMention`（含"bot 自己的 `app_mention` 回声要跳过"）。
#[test]
fn app_mention_is_addressed_and_stripped() {
    let event = events_api(serde_json::json!({
        "type": "app_mention", "user": "UALICE", "text": "<@UBOT> hi",
        "channel": "C123", "ts": "1700000000.000700",
    }));
    let message = inbound_from_event(&event, "UBOT").expect("app_mention 可摄入");
    assert_eq!(message.source.chat_type, ChatType::Group);
    assert!(message.addressed_to_bot);
    assert_eq!(message.text, "hi");

    let echo = events_api(serde_json::json!({
        "type": "app_mention", "user": "UBOT", "channel": "C1", "ts": "1.9",
    }));
    assert!(
        inbound_from_event(&echo, "UBOT").is_none(),
        "bot 自己的提及要跳过"
    );

    // 认不出的事件类型 ⇒ 丢弃且不报错。
    let other = events_api(serde_json::json!({
        "type": "reaction_added", "user": "UALICE", "channel": "C1", "ts": "1.10",
    }));
    assert!(inbound_from_event(&other, "UBOT").is_none());
}

/// 上游 `TestInboundFromMessage_FileShare`：两份可取回的文件进 `raw`，
/// 无 URL 的与站外的被丢掉；`url_private_download` 优先、`url_private` 兜底。
#[test]
fn file_share_keeps_only_fetchable_files() {
    let event = events_api(serde_json::json!({
        "type": "message", "subtype": "file_share", "channel": "D123", "channel_type": "im",
        "user": "UALICE", "text": "here is the doc", "ts": "1700000000.000800",
        "files": [
            {"id":"F1","name":"report.pdf","mimetype":"application/pdf","size":42,
             "url_private":"https://files.slack.com/files-pri/T1-F1/report.pdf",
             "url_private_download":"https://files.slack.com/files-pri/T1-F1/download/report.pdf"},
            {"id":"F2","name":"shot.png","mimetype":"image/png",
             "url_private":"https://files.slack.com/files-pri/T1-F2/shot.png"},
            {"id":"F3","name":"external.doc"},
            {"id":"F4","name":"spec.gdoc","mode":"external",
             "url_private":"https://docs.google.com/document/d/abc/edit"}
        ]
    }));
    let message = inbound_from_event(&event, "UBOT").expect("file_share 可摄入");
    let raw: RawEvent = serde_json::from_value(message.raw).expect("decode raw");
    assert_eq!(raw.files.len(), 2, "只留 F1/F2：无 URL 与站外的被丢");
    assert_eq!(raw.files[0].id, "F1");
    assert_eq!(raw.files[0].name, "report.pdf");
    assert_eq!(raw.files[0].mimetype, "application/pdf");
    assert_eq!(raw.files[0].size, 42);
    assert_eq!(
        raw.files[0].download_url,
        "https://files.slack.com/files-pri/T1-F1/download/report.pdf"
    );
    assert_eq!(raw.files[1].id, "F2");
    assert_eq!(
        raw.files[1].download_url,
        "https://files.slack.com/files-pri/T1-F2/shot.png"
    );
    // `HasMedia` 只认可取回的文件（`raw` 里没有文件的 ⇒ 不承诺媒体）。
    let without = events_api(serde_json::json!({
        "type": "message", "subtype": "file_share", "channel": "D1", "channel_type": "im",
        "user": "UALICE", "text": "shared a doc", "ts": "1.11",
        "files": [{"id":"F1","name":"spec.gdoc","url_private":"https://docs.google.com/x"}]
    }));
    let message = inbound_from_event(&without, "UBOT").expect("正文仍可摄入");
    assert!(!crate::slack::media::has_media(&message));
}

/// `app_mention` 的文件同样进 `raw`（上游 `TestInboundFromAppMention_Files`）。
#[test]
fn app_mention_files_land_in_raw() {
    let event = events_api(serde_json::json!({
        "type": "app_mention", "user": "UALICE", "text": "<@UBOT> look at this",
        "channel": "C123", "ts": "1700000000.000900",
        "files": [{"id":"F9","name":"log.txt","mimetype":"text/plain",
                   "url_private_download":"https://files.slack.com/files-pri/T1-F9/download/log.txt"}]
    }));
    let message = inbound_from_event(&event, "UBOT").expect("可摄入");
    let raw: RawEvent = serde_json::from_value(message.raw).expect("decode raw");
    assert_eq!(raw.files.len(), 1);
    assert_eq!(raw.files[0].id, "F9");
}

/// 上游 `TestSlackChatType`。
#[test]
fn chat_type_mapping() {
    let cases = [
        ("D123", "im", ChatType::P2p),
        ("G123", "mpim", ChatType::Group),
        ("C123", "channel", ChatType::Group),
        ("C123", "private_channel", ChatType::Group),
        ("D999", "", ChatType::P2p),
        ("C999", "", ChatType::Group),
    ];
    for (channel_id, channel_type, want) in cases {
        assert_eq!(slack_chat_type(channel_id, channel_type), want);
    }
}

/// 提及匹配器：`<@U|name>` 形态、空 id ⇒ `None`、剥离所有提及。
#[test]
fn mention_matcher_covers_both_forms() {
    let mention = MentionRe::new("UBOT").expect("非空 id");
    assert!(mention.is_match("hi <@UBOT> there"));
    assert!(mention.is_match("hi <@UBOT|bot> there"));
    assert!(!mention.is_match("hi <@UOTHER> there"));
    assert_eq!(mention.strip("<@UBOT> a <@UBOT|b> c"), " a  c");
    assert_eq!(clean_text("<@UBOT>  spaced  ", Some(&mention)), "spaced");
    assert!(MentionRe::new("").is_none(), "空 id ⇒ 提及判定是 no-op");
    assert_eq!(clean_text("  raw  ", None), "raw");
}

/// 摄入的 subtype 白名单（上游 `isIngestableSubtype`）。
#[test]
fn ingestable_subtypes() {
    for ok in ["", "thread_broadcast", "file_share"] {
        assert!(is_ingestable_subtype(ok), "{ok} 应可摄入");
    }
    for no in ["message_changed", "message_deleted", "channel_join"] {
        assert!(!is_ingestable_subtype(no), "{no} 不该摄入");
    }
}
