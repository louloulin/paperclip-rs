//! `outbound` 的**附件准入 / 收件箱 / 纯函数**用例。

use super::*;

/// 准入在 **spawn 之前**认领：上限满了就削减，而且**只**记 `attachment_delivery_shed`
/// （那一刻没人知道这一轮带不带文件）。
#[tokio::test]
async fn attachment_admission_is_claimed_before_the_spawn() {
    let counting: &'static Counting = Box::leak(Box::new(Counting::default()));
    let installation = Id::new();
    let task_id = Id::new();
    // 上限 1 ⇒ 第二个投递被拒。
    let gates = AttachmentGates::new(1, 8);
    let port = Arc::new(FakeAttachments::default());
    let queries = FakeQueries::with_task(task(task_id));
    let outbound = Outbound::new(queries as Arc<dyn OutboundQueries>, None)
        .with_attachments(port.clone())
        .with_attachment_gates(gates)
        .with_outbound_metrics(counting);
    let event = chat_done(task_id, session(), "answer");
    let addr = RoundAddress {
        installation_id: Some(installation),
        chat_id: "chat-1".to_string(),
        chat_type: CHAT_TYPE_SINGLE_INT,
    };
    outbound.deliver_attachments(&event, &addr, true);
    outbound.deliver_attachments(&event, &addr, false);
    assert_eq!(counting.attachment_shed.load(Ordering::SeqCst), 1);
    assert_eq!(counting.attachment_dropped.load(Ordering::SeqCst), 0);
    assert_eq!(counting.dropped.load(Ordering::SeqCst), 0);
}

/// 没有对象存储 ⇒ 连查表都不花（`may_carry_attachments` 是纯函数）。

#[tokio::test]
async fn without_object_storage_a_turn_carries_no_files() {
    let task_id = Id::new();
    let outbound = Outbound::new(FakeQueries::with_task(task(task_id)), None);
    let event = chat_done(task_id, session(), "answer");
    assert!(!outbound.may_carry_attachments(&event));
    let with_port = Outbound::new(Arc::new(FakeQueries::default()), None)
        .with_attachments(Arc::new(FakeAttachments::default()));
    assert!(with_port.may_carry_attachments(&event));
    // 一个不命名消息的事件同样不花查询。
    let mut bare = event;
    bare.message_id = String::new();
    assert!(!with_port.may_carry_attachments(&bare));
}

// =====================================================================
// 收件箱（渲染归 M7-19）
// =====================================================================

/// 没有渲染器 ⇒ 不投递（失败关闭），而不是伪造一张卡片。

#[tokio::test]
async fn an_inbox_push_without_a_renderer_is_not_delivered() {
    let installation = Id::new();
    let outbound = Outbound::new(Arc::new(FakeQueries::default()), None);
    let push = InboxPush {
        item_id: "item-1".to_string(),
        item_type: "issue_assigned".to_string(),
        issue_id: "issue-1".to_string(),
        recipient_type: "member".to_string(),
        recipient_id: Id::new().0.to_string(),
        workspace_id: Id::new().0.to_string(),
    };
    assert!(!outbound.handle_inbox_new(&push).await);
    let _ = installation;
}

/// 收件人不是成员 ⇒ 什么都不做（agent 不经聊天渠道收通知）。

#[tokio::test]
async fn a_non_member_recipient_gets_nothing() {
    let outbound = Outbound::new(Arc::new(FakeQueries::default()), None);
    let push = InboxPush {
        item_id: "item-1".to_string(),
        item_type: "issue_assigned".to_string(),
        issue_id: "issue-1".to_string(),
        recipient_type: "agent".to_string(),
        recipient_id: Id::new().0.to_string(),
        workspace_id: Id::new().0.to_string(),
    };
    assert!(!outbound.handle_inbox_new(&push).await);
}

/// `payload` 的投影：两层 `item`，缺 id 时回落到"类型 + issue"。

#[test]
fn inbox_payload_projection_matches_the_upstream_shape() {
    let payload = serde_json::json!({
        "item": {
            "id": "item-1",
            "type": "issue_assigned",
            "issue_id": "issue-1",
            "recipient_type": "member",
            "recipient_id": "user-1",
            "workspace_id": "ws-1",
        }
    });
    let push = InboxPush::from_payload(&payload).expect("payload");
    assert_eq!(push.item_id, "item-1");
    assert!(push.is_member_recipient());
    let legacy = serde_json::json!({
        "item": { "type": "issue_assigned", "issue_id": "issue-1", "recipient_type": "member" }
    });
    let push = InboxPush::from_payload(&legacy).expect("legacy");
    assert_eq!(push.item_id, "issue_assigned:issue-1");
    // 没有 `item` 那一层 ⇒ 这条 payload 不是一条通知。
    assert!(InboxPush::from_payload(&serde_json::json!({ "other": 1 })).is_none());
}

// =====================================================================
// 纯函数
// =====================================================================

/// task id **信封优先**（上游 `service.broadcastChatDone` 填的是 payload 那一份）。

#[test]
fn the_envelope_task_id_wins_over_the_payload() {
    let mut event = ChatDone {
        payload_task_id: "payload".to_string(),
        ..ChatDone::default()
    };
    assert_eq!(event.task_id(), "payload");
    event.envelope_task_id = "envelope".to_string();
    assert_eq!(event.task_id(), "envelope");
    assert!(event.parsed_task_id().is_none(), "不是 uuid");
    let id = Id::new();
    event.envelope_task_id = id.0.to_string();
    assert_eq!(event.parsed_task_id(), Some(id));
}

/// `parse_uuid` 丢掉空串与零值（上游 `Valid` 的两半）。

#[test]
fn parsing_a_uuid_rejects_blank_and_nil() {
    assert_eq!(parse_uuid(""), None);
    assert_eq!(parse_uuid("   "), None);
    assert_eq!(parse_uuid(&uuid::Uuid::nil().to_string()), None);
    let id = Id::new();
    assert_eq!(parse_uuid(&id.0.to_string()), Some(id));
    assert_eq!(parse_uuid("not-a-uuid"), None);
}

/// `delivery_budget` 的 `fallback`：只剩不到一个 ack 的预算时换一份新的（气泡花不掉它）。

#[test]
fn the_fallback_budget_is_replaced_only_when_the_bubble_could_have_spent_it() {
    let now = Instant::now();
    // 还剩很多 ⇒ 原样。
    let roomy = DeliveryBudget::at(now + Duration::from_secs(30));
    assert_eq!(roomy.fallback(now), roomy);
    // 剩得比一个 ack 还少 ⇒ 换一份新的。
    let tight = DeliveryBudget::at(now + Duration::from_millis(10));
    let replaced = tight.fallback(now);
    assert!(replaced.remaining(now) >= FALLBACK_SEND_TIMEOUT);
    // 已经过期 ⇒ 也换（诚实：过期的预算写不出任何东西）。
    let expired = DeliveryBudget::at(now);
    assert!(expired.fallback(now).remaining(now) >= FALLBACK_SEND_TIMEOUT);
}

/// `ChatType::P2p` 在帧层是 1、群里是 2（收件箱推送用单聊）。

#[test]
fn the_single_chat_type_is_one() {
    assert_eq!(aibot_chat_type_from_channel(ChatType::P2p), 1);
    assert_eq!(aibot_chat_type_from_channel(ChatType::Group), 2);
    assert_eq!(CHAT_TYPE_SINGLE_INT, 1);
}

/// `Debug` 不吐端口内部（trait 对象没有 `Debug` ⇒ 只报存在性）。

#[test]
fn the_outbound_debug_reports_presence_only() {
    let outbound = Outbound::new(Arc::new(FakeQueries::default()), None);
    let rendered = format!("{outbound:?}");
    assert!(rendered.contains("Outbound"), "{rendered}");
    assert!(rendered.contains("senders: false"), "{rendered}");
}

/// 一个 `InboundMessage` 的最小构造（本文件只在收件箱那一支用到它，见下）。
#[allow(dead_code)]
fn inbound_message() -> InboundMessage {
    use mc_core::channel::message::{MessageKind, Source};
    use mc_core::channel::ChannelKind;
    InboundMessage {
        event_id: "ev".to_string(),
        message_id: "msg".to_string(),
        source: Source {
            channel_type: ChannelKind::WeCom,
            chat_id: "chat-1".to_string(),
            chat_type: ChatType::P2p,
            sender_id: "user-1".to_string(),
            sender_stable_id: String::new(),
            thread_id: String::new(),
        },
        kind: MessageKind::Text,
        text: "hi".to_string(),
        command_text: String::new(),
        has_selected_context: false,
        media_refs: Vec::new(),
        reply_to: None,
        addressed_to_bot: true,
        force_fresh: false,
        skip_agent_run: false,
        raw: serde_json::Value::Null,
    }
}
