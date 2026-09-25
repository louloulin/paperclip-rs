//! lark **端到端收发回路**（M7-13 承担本渠道的门禁证据，`docs/60` §4.2）。
//!
//! # 这条回路**真**到哪一步（逐层点明，别把替身当业务路径）
//!
//! | 层 | 本用例 | 说明 |
//! | --- | --- | --- |
//! | 平台 wire | **替身**（`FakeApi` + 内存数据层） | 平台 API 在 CI 里连不上（要 `app_id`/`app_secret`），所以 ① 出站 HTTP 换 `FakeApi`（它**逐字段**记下每次调用）；② 数据层换两族各一张内存表。WS 那一段的替身是 **M7-11/M7-12 的**（`ws_connector/tests/harness.rs`，本片写集之外）—— 本用例从"帧已解码"处接手。 |
//! | 入站归一化 | **真代码** | `LarkInboundMessage::from_event` + `to_inbound_message`（M7-12） |
//! | 安装 / 身份解析 | **真代码** | `LarkInstallationResolver` / `LarkIdentityResolver`（M7-12），端口是内存查询 |
//! | 流水线判决 | **真代码** | engine 的 `Router::dispatch`（M7-1/M7-2）；只有**别的片**的端口（去重 / 会话 / 触发）是替身 |
//! | 出站 | **真代码** | `LarkOutcomeReplier` / `LarkOutboundDelivery` / `LarkCardPatcher` / `TypingIndicatorManager`（本片） |
//! | 数据层 | **真代码** | `LarkChannelStore` 的桥（本片）——两族表的读写落点由它的用例钉住 |
//!
//! **真库**那一半不在本 crate（没有 `sqlx` 依赖）：泛化 `channel_*` 仓储由门 ⑥ 的
//! `crates/mc-http/tests/channels/*` 覆盖（M7-1 落的），遗留 `lark_*` 仓储由 `crates/mc-repos`
//! 的真库用例覆盖。⇒ §4.2 的"真 DB"由门 ⑥ 承担，"收 → 判决 → 发"的**整条业务链**由本文件承担。
//!
//! # 三段回路
//!
//! 1. `unbound_sender_from_a_decoded_frame_gets_a_binding_prompt`：帧 → 归一化 → 真 Router →
//!    真回复器 → 替身收到**发往发件人私聊**的绑定卡；
//! 2. `a_bound_turn_lights_the_indicator_then_answers_in_thread`：帧 → …→ `Ingested` →
//!    打字指示**先**贴上 → agent 回复按**触发那条消息**回帖；
//! 3. `a_task_failure_and_a_card_patch_stay_on_the_lark_side`：失败卡 + 卡片 patch 的
//!    `message_edit` 位。

use std::sync::Arc;

use mc_core::id::Id;
use serde_json::json;
use uuid::Uuid;

use crate::engine::resolvers::DropReason;
use crate::lark::outbound::DeliveryOutcome;
use crate::lark::tests::support::{decrypter, delivery_row, Call};
use crate::lark::types::ChatType;

use harness::{decode_envelope, normalize, Harness};

use harness::{BINDING, INST, SESSION, TASK};

mod harness;

// =====================================================================
// 回路 1：未绑定发件人 ⇒ 绑定卡
// =====================================================================

#[tokio::test]
async fn unbound_sender_from_a_decoded_frame_gets_a_binding_prompt() {
    let harness = Harness::new(false, true);
    let event = decode_envelope(
        "ev-1",
        "om-1",
        "cli_a",
        "oc_main",
        "group",
        "@_user_1 hello",
        &json!([{ "key": "@_user_1", "id": { "open_id": "ou_bot", "union_id": "on_bot" }, "name": "Bot" }]),
    );
    let message = normalize(event, &harness.platform);
    assert_eq!(message.source.chat_id, "oc_main");
    assert!(message.addressed_to_bot, "提及让群消息指向 bot");

    harness.router.route(message).await.expect("routes");

    // 出站是**脱离**的 ⇒ 让出一次调度。
    tokio::time::sleep(std::time::Duration::from_millis(40)).await;

    let calls = harness.api.log();
    assert_eq!(calls.len(), 1, "{calls:?}");
    match &calls[0] {
        Call::SendBindingPrompt { open_id, bind_url } => {
            assert_eq!(open_id, "ou_sender", "卡发给发件人**本人**的私聊");
            assert_eq!(
                bind_url,
                "https://app.example/lark/bind?token=raw-binding-token"
            );
        }
        other => panic!("expected a binding prompt, got {other:?}"),
    }
    // 判决是产品性的：审计记了 `unbound_user`，且**没有**排 run。
    assert_eq!(
        harness.audit.dropped.lock().expect("poisoned").clone(),
        vec![DropReason::UnboundUser]
    );
    assert!(harness.trigger.runs.lock().expect("poisoned").is_empty());
}

/// 群里的**闲话**（没 @bot）⇒ 丢弃，且**不**刷绑定卡。
#[tokio::test]
async fn group_chatter_without_a_mention_is_dropped_before_identity() {
    let harness = Harness::new(false, true);
    let event = decode_envelope(
        "ev-2",
        "om-2",
        "cli_a",
        "oc_main",
        "group",
        "just chatting",
        &json!([]),
    );
    let message = normalize(event, &harness.platform);
    assert!(!message.addressed_to_bot);

    harness.router.route(message).await.expect("routes");
    tokio::time::sleep(std::time::Duration::from_millis(40)).await;

    assert!(harness.api.log().is_empty(), "群闲聊不该触发任何出站");
    assert_eq!(
        harness.audit.dropped.lock().expect("poisoned").clone(),
        vec![DropReason::NotAddressedInGroup]
    );
}

/// 认不出的 `app_id` ⇒ `invalid_event` 丢弃（**不**是错误）。
#[tokio::test]
async fn an_unknown_app_id_is_dropped_as_an_invalid_event() {
    let harness = Harness::new(true, true);
    let event = decode_envelope(
        "ev-3",
        "om-3",
        "cli_unknown",
        "oc_main",
        "group",
        "hi",
        &json!([]),
    );
    let message = normalize(event, &harness.platform);

    harness.router.route(message).await.expect("routes");
    tokio::time::sleep(std::time::Duration::from_millis(40)).await;
    assert!(harness.api.log().is_empty());
    assert_eq!(
        harness.audit.dropped.lock().expect("poisoned").clone(),
        vec![DropReason::InvalidEvent]
    );
}

// =====================================================================
// 回路 2：绑定发件人 ⇒ 入库 + 打字指示 + 按触发回帖
// =====================================================================

#[tokio::test]
async fn a_bound_turn_lights_the_indicator_then_answers_in_thread() {
    let harness = Harness::new(true, true);
    let event = decode_envelope(
        "ev-4",
        "om-4",
        "cli_a",
        "oc_main",
        "group",
        "@_user_1 what is up",
        &json!([{ "key": "@_user_1", "id": { "open_id": "ou_bot", "union_id": "on_bot" }, "name": "Bot" }]),
    );
    let message = normalize(event, &harness.platform);

    harness.router.route(message).await.expect("routes");
    tokio::time::sleep(std::time::Duration::from_millis(40)).await;

    // 入库 ⇒ run 排上了（触发端口是 engine 的，判决由真 Router 给出）。
    assert_eq!(harness.trigger.runs.lock().expect("poisoned").len(), 1);
    // 打字指示**先**贴上（`Outcome::Ingested && run_scheduled`）。
    let calls = harness.api.log();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(
        calls[0],
        Call::AddReaction {
            message_id: "om-4".to_string(),
            emoji_type: "Typing".to_string(),
        }
    );
    assert_eq!(
        harness
            .typing
            .tracked_reactions(Id(Uuid::from_u128(SESSION))),
        1
    );
    // 普通聊天消息**不**回（agent 自己的回复走 patcher）。
    assert!(
        !calls
            .iter()
            .any(|call| matches!(call, Call::SendText { .. } | Call::SendCard { .. })),
        "判决回复器对 Ingested 保持沉默"
    );

    // agent 的回复（`chat:done` 那条路）⇒ 按触发那条消息回帖 + `@` 发件人，
    // 并且**先**把"处理中"撤掉。
    harness.arm_delivery("oc_main", ChatType::Group, "om-4");
    let task = Id(Uuid::from_u128(TASK));
    let outcome = harness
        .delivery
        .on_chat_done(task, "All good")
        .await
        .expect("reply");
    assert!(matches!(outcome, DeliveryOutcome::Text { .. }));

    let calls = harness.api.log();
    let deletions: Vec<&Call> = calls
        .iter()
        .filter(|call| matches!(call, Call::DeleteReaction { .. }))
        .collect();
    assert_eq!(deletions.len(), 1, "回复可见之前撤掉表情");
    let sends: Vec<&Call> = calls
        .iter()
        .filter(|call| matches!(call, Call::SendText { .. }))
        .collect();
    assert_eq!(sends.len(), 1);
    match sends[0] {
        Call::SendText {
            text,
            reply_message_id,
            in_thread,
            ..
        } => {
            assert!(text.contains("All good"));
            assert!(text.contains("ou_sender"), "群里 @ 上发件人");
            assert_eq!(reply_message_id, "om-4", "回复触发那条消息");
            assert!(!in_thread, "普通群是原生回复，不是话题回复");
        }
        other => panic!("expected SendText, got {other:?}"),
    }
    assert_eq!(
        harness
            .typing
            .tracked_reactions(Id(Uuid::from_u128(SESSION))),
        0
    );
}

/// 已绑定但**不是** workspace 成员 ⇒ `non_workspace_member` 丢弃（也不刷绑定卡）。
#[tokio::test]
async fn a_bound_non_member_is_dropped_without_a_prompt() {
    let harness = Harness::new(true, false);
    let event = decode_envelope(
        "ev-5",
        "om-5",
        "cli_a",
        "oc_main",
        "group",
        "@_user_1 hi",
        &json!([{ "key": "@_user_1", "id": { "open_id": "ou_bot", "union_id": "on_bot" }, "name": "Bot" }]),
    );
    let message = normalize(event, &harness.platform);

    harness.router.route(message).await.expect("routes");
    tokio::time::sleep(std::time::Duration::from_millis(40)).await;

    assert!(harness.api.log().is_empty());
    assert_eq!(
        harness.audit.dropped.lock().expect("poisoned").clone(),
        vec![DropReason::NonWorkspaceMember]
    );
}

/// 话题里的触发 ⇒ 出站留在**话题内**（`reply_in_thread`）。
#[tokio::test]
async fn a_topic_trigger_keeps_the_answer_inside_the_topic() {
    let harness = Harness::new(true, true);
    let mut event = decode_envelope(
        "ev-6",
        "om-6",
        "cli_a",
        "oc_main",
        "group",
        "@_user_1 in a topic",
        &json!([{ "key": "@_user_1", "id": { "open_id": "ou_bot", "union_id": "on_bot" }, "name": "Bot" }]),
    );
    event.thread_id = "t_9".to_string();
    let message = normalize(event, &harness.platform);
    harness.router.route(message).await.expect("routes");
    tokio::time::sleep(std::time::Duration::from_millis(40)).await;

    // 出站面的会话绑定：复合键 + config 里的真实 chat id。
    harness.store.put_delivery(delivery_row(
        Id(Uuid::from_u128(TASK)),
        Id(Uuid::from_u128(BINDING)),
        Id(Uuid::from_u128(INST)),
        "oc_main#t_9",
        ChatType::Group,
        Some("om-6"),
        Some("t_9"),
        json!({"chat_id": "oc_main", "sender_id": "ou_sender"}),
    ));

    harness
        .delivery
        .on_chat_done(Id(Uuid::from_u128(TASK)), "answer")
        .await
        .expect("reply");

    let calls = harness.api.log();
    match calls
        .iter()
        .find(|call| matches!(call, Call::SendText { .. }))
        .expect("a text send")
    {
        Call::SendText {
            chat_id,
            in_thread,
            reply_message_id,
            ..
        } => {
            assert_eq!(chat_id, "oc_main", "复合键解回真实 chat id");
            assert!(*in_thread, "留在话题里");
            assert_eq!(reply_message_id, "om-6");
        }
        other => panic!("expected SendText, got {other:?}"),
    }
}

// =====================================================================
// 回路 3：失败卡与卡片 patch
// =====================================================================

#[tokio::test]
async fn a_task_failure_and_a_card_patch_stay_on_the_lark_side() {
    let harness = Harness::new(true, true);
    harness.arm_delivery("oc_main", ChatType::Group, "om-7");
    let task = Id(Uuid::from_u128(TASK));

    // 失败 ⇒ 错误卡（视觉上与正常回复区分开）。
    let failed = harness
        .delivery
        .on_task_failed(task, "daemon went away")
        .await
        .expect("error card");
    assert!(matches!(failed, DeliveryOutcome::ErrorCard { .. }));

    // 卡片 patch 的生命周期（`message_edit` 能力位）。
    let patcher = crate::lark::outbound::LarkCardPatcher::new(
        Arc::clone(&harness.store.store()) as Arc<dyn crate::lark::outbound::PatcherQueries>,
        Arc::clone(&harness.api) as Arc<dyn crate::lark::client::ApiClient>,
    )
    .with_decrypter(decrypter());
    let session = Id(Uuid::from_u128(SESSION));
    let opened = patcher
        .begin(
            Id(Uuid::from_u128(INST)),
            session,
            task,
            crate::lark::outbound::CardKind::Thinking,
            &crate::lark::outbound::RenderInput {
                task_id: Some(task),
                ..crate::lark::outbound::RenderInput::default()
            },
        )
        .await
        .expect("begin");
    let DeliveryOutcome::Patched { card_id, status } = opened else {
        panic!("expected a patch outcome, got {opened:?}");
    };
    assert_eq!(status, crate::lark::store::CardStatus::Pending);

    let streamed = patcher
        .patch(
            Id(Uuid::from_u128(INST)),
            card_id,
            crate::lark::outbound::CardKind::Final,
            &crate::lark::outbound::RenderInput {
                task_id: Some(task),
                content: "done".to_string(),
                ..crate::lark::outbound::RenderInput::default()
            },
        )
        .await
        .expect("patch");
    assert_eq!(
        streamed,
        DeliveryOutcome::Patched {
            card_id,
            status: crate::lark::store::CardStatus::Final
        }
    );

    // 替身收到的东西：1 张错误卡 + 1 张开卡 + 1 次 patch，且每次 patch 都是**整卡**。
    let calls = harness.api.log();
    assert_eq!(
        calls
            .iter()
            .filter(|c| matches!(c, Call::SendCard { .. }))
            .count(),
        2,
        "{calls:?}"
    );
    assert_eq!(
        calls
            .iter()
            .filter(|c| matches!(c, Call::PatchCard { .. }))
            .count(),
        1
    );
    let patch = calls
        .iter()
        .find_map(|call| match call {
            Call::PatchCard { card_json, .. } => Some(card_json),
            _ => None,
        })
        .expect("a patch");
    let doc: serde_json::Value = serde_json::from_str(patch).expect("valid card json");
    assert_eq!(doc["config"]["update_multi"], json!(true));
    assert_eq!(doc["elements"][0]["text"]["content"], json!("done"));
}

/// 取消 ⇒ 只撤表情（回路里的"没有答案"那一支）。
#[tokio::test]
async fn a_cancelled_turn_only_takes_the_indicator_off() {
    let harness = Harness::new(true, true);
    let session = Id(Uuid::from_u128(SESSION));
    harness
        .typing
        .add_now(&harness.platform, session, "om-8", "1700000000000")
        .await;
    assert_eq!(harness.typing.tracked_reactions(session), 1);

    let outcome = harness
        .delivery
        .on_task_cancelled(Some(session))
        .await
        .expect("clear");
    assert_eq!(
        outcome,
        DeliveryOutcome::TypingCleared {
            session_id: session
        }
    );

    let calls = harness.api.log();
    assert_eq!(
        calls
            .iter()
            .filter(|call| matches!(call, Call::DeleteReaction { .. }))
            .count(),
        1
    );
    assert!(
        !calls.iter().any(|call| matches!(
            call,
            Call::SendText { .. } | Call::SendCard { .. } | Call::SendMarkdown { .. }
        )),
        "取消不发任何消息"
    );
}
