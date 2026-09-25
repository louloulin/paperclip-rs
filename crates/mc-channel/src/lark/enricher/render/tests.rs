//! [`super`]（三个渲染块 + 发言人标签器）的用例。
//!
//! 上游 `renderRecentContextBlock` / `renderQuotedBlock` / `renderForwardedItems` /
//! `speakerLabeler` 的等价集合，外加三条上游注释点名的语义：
//! **子消息上限 + 截断标记**、**嵌套转发不递归**、**`Bot` / 真名 / `User N` 的三档标签**。

use super::*;
use crate::lark::types::LarkMessageMention;

fn message(id: &str, sender: &str, sender_type: &str, text: &str) -> LarkMessage {
    LarkMessage {
        message_id: id.to_string(),
        message_type: "text".to_string(),
        content: format!(r#"{{"text":{}}}"#, serde_json::to_string(text).unwrap()),
        sender_id: sender.to_string(),
        sender_type: sender_type.to_string(),
        ..LarkMessage::default()
    }
}

fn at(create_time: &str, msg: LarkMessage) -> LarkMessage {
    LarkMessage {
        create_time: create_time.to_string(),
        ..msg
    }
}

fn names(pairs: &[(&str, &str)]) -> SpeakerNames {
    pairs
        .iter()
        .map(|(id, name)| ((*id).to_string(), (*name).to_string()))
        .collect()
}

// =====================================================================
// 一、发言人集合与标签
// =====================================================================

/// `sender_open_ids`：丢掉 app 与空 id，按首次出现顺序去重。
#[test]
fn sender_open_ids_are_distinct_and_ordered() {
    let messages = vec![
        message("om-1", "ou_a", "user", "hi"),
        message("om-2", "cli_app", "app", "bot"),
        message("om-3", "ou_b", "user", "hi"),
        message("om-4", "ou_a", "user", "again"),
        message("om-5", "", "user", "no sender"),
    ];
    assert_eq!(sender_open_ids(&messages), vec!["ou_a", "ou_b"]);
}

/// 三档标签：app 恒 `Bot`；在映射里的用真名；不在的按**首次出现**顺序给 `User N`。
#[test]
fn speaker_labels_are_real_names_then_positional() {
    let mut labeler = SpeakerLabeler::new(Some(&names(&[("ou_a", "Alice")])));
    let alice = message("om-1", "ou_a", "user", "a");
    let bob = message("om-2", "ou_b", "user", "b");
    let bot = message("om-3", "cli_app", "app", "c");
    assert_eq!(labeler.label(&bot), "Bot");
    assert_eq!(labeler.label(&bob), "User 1");
    assert_eq!(labeler.label(&alice), "Alice");
    // 同一个发件人第二次拿到同一个标签（映射不清空）。
    assert_eq!(labeler.label(&bob), "User 1");
    // 空 sender_id 归到 `unknown` 这一个键上（不各算一个 User）。
    let anonymous = message("om-4", "", "user", "d");
    assert_eq!(labeler.label(&anonymous), "User 2");
    assert_eq!(labeler.label(&anonymous), "User 2");
}

/// 名字为空串等于没解析出（不当成真名）。
#[test]
fn empty_resolved_names_fall_back_to_positional() {
    let mut labeler = SpeakerLabeler::new(Some(&names(&[("ou_a", "")])));
    assert_eq!(
        labeler.label(&message("om-1", "ou_a", "user", "a")),
        "User 1"
    );
}

// =====================================================================
// 二、近况块
// =====================================================================

/// `<recent_context count="N">` + 每行 `[发言人]: 正文`，最旧在前由调用方保证。
#[test]
fn recent_context_block_shape_is_pinned() {
    let kept = vec![
        message("om-1", "ou_a", "user", "先说这句"),
        message("om-2", "cli_app", "app", "bot 回一句"),
    ];
    let rendered = render_recent_context_block(&kept, Some(&names(&[("ou_a", "Alice")])));
    assert_eq!(
        rendered,
        "<recent_context count=\"2\">\n[Alice]: 先说这句\n[Bot]: bot 回一句\n</recent_context>"
    );
}

/// 空正文 ⇒ `[empty message]`；嵌套转发 ⇒ 「手动展开」占位（不递归）。
#[test]
fn recent_context_placeholders_for_empty_and_forward() {
    let kept = vec![
        LarkMessage {
            message_id: "om-1".to_string(),
            message_type: "text".to_string(),
            content: r#"{"text":""}"#.to_string(),
            sender_id: "ou_a".to_string(),
            sender_type: "user".to_string(),
            ..LarkMessage::default()
        },
        LarkMessage {
            message_id: "om-2".to_string(),
            message_type: MSG_TYPE_MERGE_FORWARD.to_string(),
            sender_id: "ou_b".to_string(),
            sender_type: "user".to_string(),
            ..LarkMessage::default()
        },
    ];
    let rendered = render_recent_context_block(&kept, None);
    assert!(rendered.contains("[User 1]: [empty message]"), "{rendered}");
    assert!(
        rendered.contains("[User 2]: [merge_forward, expand manually]"),
        "{rendered}"
    );
}

// =====================================================================
// 三、引用块
// =====================================================================

/// 引用块逐字：`message_id` / `sender` / `type` 三个属性都带引号（`{:?}` 的形态）。
#[test]
fn quoted_block_shape_is_pinned() {
    let parent = message("om-parent", "ou_a", "user", "被引用的那句");
    let rendered = render_quoted_block(
        "om-parent",
        std::slice::from_ref(&parent),
        None,
        Some(&names(&[("ou_a", "Alice")])),
        100,
    );
    assert_eq!(
        rendered,
        "<quoted_message message_id=\"om-parent\" sender=\"Alice\" type=\"text\">\n被引用的那句\n</quoted_message>"
    );
}

/// 取回失败 / 空 / 已删三种父消息都退化成同一个错误块。
#[test]
fn quoted_block_degrades_on_every_missing_parent_shape() {
    let expected = "<quoted_message message_id=\"om-parent\" type=\"error\">[unable to fetch]</quoted_message>";
    assert_eq!(
        render_quoted_block("om-parent", &[], None, None, 100),
        expected
    );
    assert_eq!(
        render_quoted_block(
            "om-parent",
            &[],
            Some(&ApiError::Http {
                op: "get_message",
                status: 403,
            }),
            None,
            100,
        ),
        expected
    );
    let deleted = LarkMessage {
        message_id: "om-parent".to_string(),
        message_type: "text".to_string(),
        content: r#"{"text":"x"}"#.to_string(),
        sender_id: "ou_a".to_string(),
        sender_type: "user".to_string(),
        deleted: true,
        ..LarkMessage::default()
    };
    assert_eq!(
        render_quoted_block("om-parent", std::slice::from_ref(&deleted), None, None, 100),
        expected
    );
}

/// 被引用的父消息**自己**是转发 ⇒ 引用块里嵌套一份 `<forwarded_messages>` 记录。
#[test]
fn quoted_block_nests_a_forward_transcript() {
    let sentinel = LarkMessage {
        message_id: "om-forward".to_string(),
        message_type: MSG_TYPE_MERGE_FORWARD.to_string(),
        sender_id: "ou_a".to_string(),
        sender_type: "user".to_string(),
        ..LarkMessage::default()
    };
    let child = at(
        "1700000001000",
        message("om-c1", "ou_b", "user", "转发的第一句"),
    );
    let items = vec![sentinel, child];
    let rendered = render_quoted_block("om-forward", &items, None, None, 100);
    assert!(
        rendered.starts_with("<quoted_message message_id=\"om-forward\""),
        "{rendered}"
    );
    assert!(rendered.contains("type=\"merge_forward\""), "{rendered}");
    assert!(
        rendered.contains(
            "<forwarded_messages count=\"1\">\n[User 1]: 转发的第一句\n</forwarded_messages>"
        ),
        "{rendered}"
    );
}

/// 一条 `parent_id` 里的双引号不会破坏属性（`{:?}` 转义）—— 与上游的 `%q` 同款。
#[test]
fn quoted_block_escapes_the_parent_id() {
    let parent = message("om\"weird", "ou_a", "user", "x");
    let rendered = render_quoted_block("om\"weird", std::slice::from_ref(&parent), None, None, 100);
    assert!(
        rendered.contains("message_id=\"om\\\"weird\""),
        "{rendered}"
    );
}

// =====================================================================
// 四、转发块
// =====================================================================

/// 子消息按**最旧在前**排序，逐条 `[发言人]: 正文`。
#[test]
fn forwarded_items_are_sorted_oldest_first() {
    let sentinel = LarkMessage {
        message_id: "om-fwd".to_string(),
        message_type: MSG_TYPE_MERGE_FORWARD.to_string(),
        ..LarkMessage::default()
    };
    let newer = at("1700000002000", message("om-b", "ou_b", "user", "后一句"));
    let older = at("1700000001000", message("om-a", "ou_a", "user", "前一句"));
    let items = vec![sentinel, newer, older];
    let rendered = render_forwarded_items(&items, "om-fwd", None, 100);
    assert_eq!(
        rendered,
        "<forwarded_messages count=\"2\">\n[User 1]: 前一句\n[User 2]: 后一句\n</forwarded_messages>"
    );
}

/// 超过上限 ⇒ 截断并留一条 `... (N more truncated)`（`count` 仍是**总数**）。
#[test]
fn forwarded_items_are_capped_with_a_visible_marker() {
    let sentinel = LarkMessage {
        message_id: "om-fwd".to_string(),
        message_type: MSG_TYPE_MERGE_FORWARD.to_string(),
        ..LarkMessage::default()
    };
    let mut items = vec![sentinel];
    for index in 0..5 {
        items.push(at(
            &format!("170000000{}000", index + 1),
            message(
                &format!("om-{index}"),
                "ou_a",
                "user",
                &format!("第 {index} 句"),
            ),
        ));
    }
    let rendered = render_forwarded_items(&items, "om-fwd", None, 3);
    assert!(
        rendered.starts_with("<forwarded_messages count=\"5\">"),
        "{rendered}"
    );
    assert!(rendered.contains("... (2 more truncated)"), "{rendered}");
    assert!(!rendered.contains("第 4 句"), "{rendered}");
}

/// 转发里套转发 ⇒ 「手动展开」占位（**不**递归，HTTP 扇出因此有界）。
#[test]
fn nested_forward_children_are_not_recursed() {
    let sentinel = LarkMessage {
        message_id: "om-fwd".to_string(),
        message_type: MSG_TYPE_MERGE_FORWARD.to_string(),
        ..LarkMessage::default()
    };
    let nested = at(
        "1700000001000",
        LarkMessage {
            message_id: "om-nested".to_string(),
            message_type: MSG_TYPE_MERGE_FORWARD.to_string(),
            sender_id: "ou_b".to_string(),
            sender_type: "user".to_string(),
            ..LarkMessage::default()
        },
    );
    let rendered = render_forwarded_items(&[sentinel, nested], "om-fwd", None, 100);
    assert!(
        rendered.contains("[User 1]: [nested merge_forward, expand manually]"),
        "{rendered}"
    );
}

/// 只有哨兵（没有子消息）⇒ 上游那条"没有可转发内容"的块。
#[test]
fn empty_forward_renders_the_unavailable_block() {
    let sentinel = LarkMessage {
        message_id: "om-fwd".to_string(),
        message_type: MSG_TYPE_MERGE_FORWARD.to_string(),
        ..LarkMessage::default()
    };
    assert_eq!(
        render_forwarded_items(&[sentinel], "om-fwd", None, 100),
        "<forwarded_messages count=\"0\">\n[no forwarded content available]\n</forwarded_messages>"
    );
    assert_eq!(
        forwarded_error_block(),
        "<forwarded_messages type=\"error\">[unable to fetch]</forwarded_messages>"
    );
}

// =====================================================================
// 五、摊平（`flattenMessage`）
// =====================================================================

/// 已删 ⇒ `[deleted message]`；空正文 ⇒ 空串（由调用方决定占位）。
#[test]
fn flatten_message_handles_deleted_and_empty() {
    let deleted = LarkMessage {
        message_id: "om-1".to_string(),
        message_type: "text".to_string(),
        content: r#"{"text":"x"}"#.to_string(),
        deleted: true,
        ..LarkMessage::default()
    };
    assert_eq!(flatten_message(&deleted), "[deleted message]");

    let empty = LarkMessage {
        message_id: "om-2".to_string(),
        message_type: "text".to_string(),
        content: r#"{"text":""}"#.to_string(),
        ..LarkMessage::default()
    };
    assert_eq!(flatten_message(&empty), "");
}

/// 历史消息里的提及**全部**渲染成可读的 `@名字`（富化路径不剥 bot：两个标识传空）。
#[test]
fn flatten_message_renders_every_historic_mention_by_name() {
    let historic = LarkMessage {
        message_id: "om-1".to_string(),
        message_type: "text".to_string(),
        content: r#"{"text":"@_user_1 你看下"}"#.to_string(),
        sender_id: "ou_a".to_string(),
        sender_type: "user".to_string(),
        mentions: vec![LarkMessageMention {
            key: "@_user_1".to_string(),
            id: "ou_bot".to_string(),
            name: "ReviewBot".to_string(),
        }],
        ..LarkMessage::default()
    };
    assert_eq!(flatten_message(&historic), "@ReviewBot 你看下");
}
