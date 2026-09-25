//! `outbound.rs` 的用例（M7-13）：卡片渲染 / 回复目标 / 提及 / 分类过的回落 / 卡片 patch 生命周期。
//!
//! 平台替身是 [`FakeApi`]（**逐字段**记下每次调用），数据层是 [`MemoryStore`]（两族各一张表）。

use std::sync::Arc;

use mc_core::id::Id;
use serde_json::json;
use uuid::Uuid;

use serde_json::Value as Json;

use super::*;
use crate::lark::params::{AppSecret, InstallationCredentials, ReplyTarget, SendTextParams};
use crate::lark::tests::support::{
    decrypter, delivery_row, installation, unsupported_reply_target_error, Call, FakeApi,
    MemoryStore,
};
use crate::lark::types::ChatId;
use crate::lark::types::ChatType;

const INST: u128 = 0x3000;
const SESSION: u128 = 0x2000;
const TASK: u128 = 0x4000;
const BINDING: u128 = 0x5000;

fn inst() -> LarkInstallation {
    installation(INST, "cli_a")
}

fn task() -> Id {
    Id(Uuid::from_u128(TASK))
}

/// 装好一条"群聊里 @bot 触发"的投递行 + 安装行，返回（投递桥，平台替身）。
async fn fixture(
    chat_id: &str,
    chat_type: ChatType,
    message_id: Option<&str>,
    thread_id: Option<&str>,
    config: Json,
) -> (Arc<FakeApi>, Arc<MemoryStore>, LarkOutboundDelivery) {
    let api = FakeApi::new();
    let store = MemoryStore::new();
    let platform = inst();
    store.put_installation(&platform);
    store.put_agent_name(platform.agent_id, "Bot");
    store.put_delivery(delivery_row(
        task(),
        Id(Uuid::from_u128(BINDING)),
        Id(Uuid::from_u128(INST)),
        chat_id,
        chat_type,
        message_id,
        thread_id,
        config,
    ));
    let bridged = store.store();
    let delivery = LarkOutboundDelivery::new(
        Arc::clone(&bridged) as Arc<dyn PatcherQueries>,
        Arc::clone(&api) as Arc<dyn ApiClient>,
    )
    .with_decrypter(decrypter());
    (api, store, delivery)
}

// ---------------------------------------------------------------------
// 回复目标（上游 `threadReplyTarget`）
// ---------------------------------------------------------------------

/// 三档：话题内 ⇒ `in_thread`；普通群 ⇒ 原生回复；p2p / 无触发 ⇒ 会话层。
#[test]
fn reply_target_has_three_tiers() {
    let group_binding = |message: Option<&str>, thread: Option<&str>| {
        ChatSessionBinding::from_delivery_row(&delivery_row(
            task(),
            Id(Uuid::from_u128(BINDING)),
            Id(Uuid::from_u128(INST)),
            "oc_main",
            ChatType::Group,
            message,
            thread,
            json!({}),
        ))
    };
    let topic = thread_reply_target(&group_binding(Some("om_1"), Some("t_1")));
    assert_eq!(topic.message_id, "om_1");
    assert!(topic.in_thread);
    let plain_group = thread_reply_target(&group_binding(Some("om_1"), None));
    assert_eq!(plain_group.message_id, "om_1");
    assert!(!plain_group.in_thread);
    assert!(!thread_reply_target(&group_binding(None, None)).is_set());
    assert!(
        !thread_reply_target(&group_binding(Some(""), Some("t_1"))).is_set(),
        "空 id = 没有触发"
    );

    let p2p = ChatSessionBinding::from_delivery_row(&delivery_row(
        task(),
        Id(Uuid::from_u128(BINDING)),
        Id(Uuid::from_u128(INST)),
        "oc_dm",
        ChatType::P2p,
        Some("om_1"),
        None,
        json!({}),
    ));
    assert!(!thread_reply_target(&p2p).is_set(), "1:1 引用是纯噪音");
}

/// `@` 目标只来自**按任务冻结**的发件人，且只在群里。
#[test]
fn mention_target_comes_from_the_frozen_sender_and_only_in_groups() {
    let binding = |chat_type: ChatType, config: Json| {
        ChatSessionBinding::from_delivery_row(&delivery_row(
            task(),
            Id(Uuid::from_u128(BINDING)),
            Id(Uuid::from_u128(INST)),
            "oc_main",
            chat_type,
            Some("om_1"),
            None,
            config,
        ))
    };
    assert_eq!(
        mention_open_id(&binding(ChatType::Group, json!({"sender_id": "ou_who"}))),
        "ou_who"
    );
    assert_eq!(
        mention_open_id(&binding(ChatType::P2p, json!({"sender_id": "ou_who"}))),
        "",
        "p2p 不 @"
    );
    assert_eq!(mention_open_id(&binding(ChatType::Group, json!({}))), "");
}

/// 话题会话没有触发消息 ⇒ 拒发（模块文档第 3 条）。
#[test]
fn a_topic_session_without_a_trigger_must_not_send() {
    let topic = ChatSessionBinding::from_delivery_row(&delivery_row(
        task(),
        Id(Uuid::from_u128(BINDING)),
        Id(Uuid::from_u128(INST)),
        "oc_main#t_1",
        ChatType::Group,
        None,
        Some("t_1"),
        json!({"chat_id": "oc_main"}),
    ));
    assert!(topic_send_without_trigger(&topic));

    let with_trigger = ChatSessionBinding::from_delivery_row(&delivery_row(
        task(),
        Id(Uuid::from_u128(BINDING)),
        Id(Uuid::from_u128(INST)),
        "oc_main#t_1",
        ChatType::Group,
        Some("om_1"),
        Some("t_1"),
        json!({"chat_id": "oc_main"}),
    ));
    assert!(!topic_send_without_trigger(&with_trigger));

    let plain = ChatSessionBinding::from_delivery_row(&delivery_row(
        task(),
        Id(Uuid::from_u128(BINDING)),
        Id(Uuid::from_u128(INST)),
        "oc_main",
        ChatType::Group,
        None,
        None,
        json!({}),
    ));
    assert!(!plain.is_topic_isolated());
    assert!(
        !topic_send_without_trigger(&plain),
        "非话题会话没有这条限制"
    );
}

// ---------------------------------------------------------------------
// 分类过的会话层回落（上游 `sendWithReplyFallback`）
// ---------------------------------------------------------------------

/// 只有"这条触发消息收不到回复"那一类才回落；回落成功后**看不到错误**。
#[tokio::test]
async fn a_classified_failure_falls_back_to_the_chat_level() {
    let api = FakeApi::new();
    api.fail_send_with(unsupported_reply_target_error());
    let target = ReplyTarget {
        message_id: "om_1".to_string(),
        in_thread: true,
    };
    let client = Arc::clone(&api);
    let message_id = send_with_reply_fallback("send text message", target, |reply_target| {
        let client = Arc::clone(&client);
        async move {
            client
                .send_text_message(SendTextParams {
                    credentials: credentials(),
                    chat_id: ChatId::new("oc_main"),
                    text: "hi".to_string(),
                    reply_target,
                })
                .await
        }
    })
    .await
    .expect("fallback succeeds");
    assert!(message_id.starts_with("om_text"));

    let calls = api.log();
    assert_eq!(calls.len(), 2);
    assert!(matches!(
        &calls[0],
        Call::SendText {
            in_thread: true,
            ..
        }
    ));
    assert!(matches!(
        &calls[1],
        Call::SendText {
            in_thread: false,
            reply_message_id,
            ..
        } if reply_message_id.is_empty()
    ));
}

/// 传输失败 / 5xx / 限流 **不**回落（盲目回落会重复回复或把话题内的回复泄进主群）。
#[tokio::test]
async fn a_transport_failure_does_not_fall_back() {
    let api = FakeApi::new();
    api.fail_send_with(crate::lark::tests::support::transport_error());
    let target = ReplyTarget {
        message_id: "om_1".to_string(),
        in_thread: true,
    };
    let client = Arc::clone(&api);
    let error = send_with_reply_fallback("send text message", target, |reply_target| {
        let client = Arc::clone(&client);
        async move {
            client
                .send_text_message(SendTextParams {
                    credentials: credentials(),
                    chat_id: ChatId::new("oc_main"),
                    text: "hi".to_string(),
                    reply_target,
                })
                .await
        }
    })
    .await
    .expect_err("must fail");

    assert_eq!(api.log().len(), 1, "**没有**第二次调用");
    assert!(!error.fell_back());
    assert_eq!(error.op(), "send text message");
    assert_eq!(
        error.original().class(),
        crate::lark::client::ErrorClass::Transport
    );
}

/// 目标本来就是会话层 ⇒ 没有可回落的东西。
#[tokio::test]
async fn a_chat_level_failure_has_nothing_to_fall_back_to() {
    let api = FakeApi::new();
    api.fail_send_with(unsupported_reply_target_error());
    let client = Arc::clone(&api);
    let error = send_with_reply_fallback(
        "send text message",
        ReplyTarget::default(),
        |reply_target| {
            let client = Arc::clone(&client);
            async move {
                client
                    .send_text_message(SendTextParams {
                        credentials: credentials(),
                        chat_id: ChatId::new("oc_main"),
                        text: "hi".to_string(),
                        reply_target,
                    })
                    .await
            }
        },
    )
    .await
    .expect_err("must fail");
    assert_eq!(api.log().len(), 1);
    assert!(!error.fell_back());
}

/// 回落**也**失败 ⇒ 两个原因都在，且 `fell_back()` 为真。
#[tokio::test]
async fn a_failed_fallback_reports_both_reasons() {
    let api = FakeApi::new();
    api.fail_send_with(unsupported_reply_target_error());
    let target = ReplyTarget {
        message_id: "om_1".to_string(),
        in_thread: false,
    };
    let client = Arc::clone(&api);
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let error = send_with_reply_fallback("send text message", target, |reply_target| {
        let client = Arc::clone(&client);
        let counter = Arc::clone(&counter);
        async move {
            // 第一次按脚本失败；第二次注入新的失败。
            if counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 1 {
                client.fail_send_with(crate::lark::tests::support::transport_error());
            }
            client
                .send_text_message(SendTextParams {
                    credentials: credentials(),
                    chat_id: ChatId::new("oc_main"),
                    text: "hi".to_string(),
                    reply_target,
                })
                .await
        }
    })
    .await
    .expect_err("both fail");

    assert_eq!(api.log().len(), 2);
    assert!(error.fell_back());
    let rendered = error.to_string();
    assert!(rendered.contains("send text message"));
}

fn credentials() -> InstallationCredentials {
    InstallationCredentials::new("cli_a", AppSecret::new("secret"))
}

// ---------------------------------------------------------------------
// `chat:done`：wire 形态 / 提及 / 各条失败关闭
// ---------------------------------------------------------------------

/// 纯散文 ⇒ `msg_type=text`；含 markdown ⇒ markdown 卡（**先选形态再挂提及**）。
#[tokio::test]
async fn wire_shape_is_chosen_from_the_body_before_the_mention_is_attached() {
    let (api, _, delivery) = fixture(
        "oc_main",
        ChatType::Group,
        Some("om_1"),
        None,
        json!({"sender_id": "ou_who"}),
    )
    .await;

    let outcome = delivery
        .on_chat_done(task(), "Hi there")
        .await
        .expect("send");
    let text_calls = api.log();
    let DeliveryOutcome::Text {
        message_id,
        mentioned,
    } = outcome
    else {
        panic!("expected a text message, got {outcome:?}");
    };
    assert!(mentioned);
    assert!(message_id.starts_with("om_text"));
    match &text_calls[0] {
        Call::SendText {
            text,
            reply_message_id,
            in_thread,
            ..
        } => {
            assert!(text.contains("ou_who"), "提及被挂上了: {text}");
            assert!(text.contains("Hi there"));
            assert_eq!(reply_message_id, "om_1");
            assert!(!in_thread);
        }
        other => panic!("expected SendText, got {other:?}"),
    }
}

/// 含 markdown 的正文走卡片路径（上游逐字：`**粗**` 不该以原文出现在聊天里）。
#[tokio::test]
async fn markdown_bodies_take_the_card_path() {
    let (api, _, delivery) = fixture(
        "oc_main",
        ChatType::Group,
        Some("om_1"),
        None,
        json!({"sender_id": "ou_who"}),
    )
    .await;

    let outcome = delivery
        .on_chat_done(task(), "**bold** answer")
        .await
        .expect("send");
    assert!(matches!(outcome, DeliveryOutcome::MarkdownCard { .. }));
    match &api.log()[0] {
        Call::SendMarkdown {
            markdown, summary, ..
        } => {
            assert!(markdown.contains("**bold** answer"));
            assert!(markdown.contains("ou_who"), "卡片正文也挂提及");
            assert!(summary.is_empty());
        }
        other => panic!("expected SendMarkdown, got {other:?}"),
    }
}

/// 空正文 ⇒ **静默丢弃**（上游逐字：宁可什么都不显示，也不要 "Done."）。
#[tokio::test]
async fn an_empty_answer_is_silently_dropped() {
    let (api, _, delivery) =
        fixture("oc_main", ChatType::Group, Some("om_1"), None, json!({})).await;
    assert_eq!(
        delivery.on_chat_done(task(), "").await.expect("no-op"),
        DeliveryOutcome::Skipped(SkipReason::EmptyContent)
    );
    assert!(api.log().is_empty());
    assert_eq!(SkipReason::EmptyContent.as_str(), "empty_content");
}

/// 没有投递行 ⇒ 直接任务，**失败关闭**。
#[tokio::test]
async fn a_direct_task_without_a_delivery_row_is_skipped() {
    let api = FakeApi::new();
    let store = MemoryStore::new();
    let bridged = store.store();
    let delivery = LarkOutboundDelivery::new(
        Arc::clone(&bridged) as Arc<dyn PatcherQueries>,
        Arc::clone(&api) as Arc<dyn ApiClient>,
    )
    .with_decrypter(decrypter());

    assert_eq!(
        delivery.on_chat_done(task(), "hi").await.expect("skip"),
        DeliveryOutcome::Skipped(SkipReason::NoDeliveryRow)
    );
    assert!(api.log().is_empty());
}

/// 别的渠道的投递行 ⇒ 跳过（**不**猜）。
#[tokio::test]
async fn another_channels_delivery_row_is_skipped() {
    let api = FakeApi::new();
    let store = MemoryStore::new();
    let mut row = delivery_row(
        task(),
        Id(Uuid::from_u128(BINDING)),
        Id(Uuid::from_u128(INST)),
        "oc_main",
        ChatType::Group,
        Some("om_1"),
        None,
        json!({}),
    );
    row.channel_type = "slack".to_string();
    store.put_delivery(row);
    let bridged = store.store();
    let delivery = LarkOutboundDelivery::new(
        Arc::clone(&bridged) as Arc<dyn PatcherQueries>,
        Arc::clone(&api) as Arc<dyn ApiClient>,
    )
    .with_decrypter(decrypter());

    assert_eq!(
        delivery.on_chat_done(task(), "hi").await.expect("skip"),
        DeliveryOutcome::Skipped(SkipReason::NotFeishu)
    );
    assert!(api.log().is_empty());
    assert_eq!(SkipReason::NotFeishu.as_str(), "not_feishu");
}

/// 出处不是渠道（web 任务复用了 lark 会话）⇒ 跳过（D2 的端口能收紧它）。
#[tokio::test]
async fn a_task_that_is_not_channel_ingested_is_skipped() {
    let api = FakeApi::new();
    let store = MemoryStore::new();
    let platform = inst();
    store.put_installation(&platform);
    store.put_delivery(delivery_row(
        task(),
        Id(Uuid::from_u128(BINDING)),
        Id(Uuid::from_u128(INST)),
        "oc_main",
        ChatType::Group,
        Some("om_1"),
        None,
        json!({}),
    ));
    *store.channel_ingested.lock().expect("poisoned") = Some(false);
    // D2：批次戳为假还不够 —— `chat_input_task_id` 那一半也必须给出来，判决才会收紧。
    *store.chat_input_task_id.lock().expect("poisoned") = Some(Id(Uuid::from_u128(0x7777)));
    let bridged = store.store();
    let delivery = LarkOutboundDelivery::new(
        Arc::clone(&bridged) as Arc<dyn PatcherQueries>,
        Arc::clone(&api) as Arc<dyn ApiClient>,
    )
    .with_decrypter(decrypter());

    assert_eq!(
        delivery.on_chat_done(task(), "hi").await.expect("skip"),
        DeliveryOutcome::Skipped(SkipReason::NotChannelIngested)
    );
    assert!(api.log().is_empty());
    assert_eq!(
        SkipReason::NotChannelIngested.as_str(),
        "not_channel_ingested"
    );
}

/// 安装已撤销（触发与事件之间被撤）⇒ 跳过。
#[tokio::test]
async fn a_revoked_installation_is_skipped() {
    let api = FakeApi::new();
    let store = MemoryStore::new();
    let mut platform = inst();
    platform.status = "revoked".to_string();
    store.put_installation(&platform);
    store.put_delivery(delivery_row(
        task(),
        Id(Uuid::from_u128(BINDING)),
        Id(Uuid::from_u128(INST)),
        "oc_main",
        ChatType::Group,
        Some("om_1"),
        None,
        json!({}),
    ));
    let bridged = store.store();
    let delivery = LarkOutboundDelivery::new(
        Arc::clone(&bridged) as Arc<dyn PatcherQueries>,
        Arc::clone(&api) as Arc<dyn ApiClient>,
    )
    .with_decrypter(decrypter());

    assert_eq!(
        delivery.on_chat_done(task(), "hi").await.expect("skip"),
        DeliveryOutcome::Skipped(SkipReason::InstallationRevoked)
    );
    assert!(api.log().is_empty());
}

/// 安装行没了（会话删除）⇒ 跳过。
#[tokio::test]
async fn a_missing_installation_row_is_skipped() {
    let api = FakeApi::new();
    let store = MemoryStore::new();
    store.put_delivery(delivery_row(
        task(),
        Id(Uuid::from_u128(BINDING)),
        Id(Uuid::from_u128(INST)),
        "oc_main",
        ChatType::Group,
        Some("om_1"),
        None,
        json!({}),
    ));
    let bridged = store.store();
    let delivery = LarkOutboundDelivery::new(
        Arc::clone(&bridged) as Arc<dyn PatcherQueries>,
        Arc::clone(&api) as Arc<dyn ApiClient>,
    )
    .with_decrypter(decrypter());

    assert_eq!(
        delivery.on_chat_done(task(), "hi").await.expect("skip"),
        DeliveryOutcome::Skipped(SkipReason::InstallationMissing)
    );
    assert!(api.log().is_empty());
}

/// 话题会话没有触发消息 ⇒ 拒发（**不**落到父群）。
#[tokio::test]
async fn a_topic_session_without_a_trigger_never_sends() {
    let (api, _, delivery) = fixture(
        "oc_main#t_1",
        ChatType::Group,
        None,
        Some("t_1"),
        json!({"chat_id": "oc_main"}),
    )
    .await;
    assert_eq!(
        delivery.on_chat_done(task(), "hi").await.expect("skip"),
        DeliveryOutcome::Skipped(SkipReason::TopicWithoutTrigger)
    );
    assert!(api.log().is_empty());
    assert_eq!(
        SkipReason::TopicWithoutTrigger.as_str(),
        "topic_without_trigger"
    );
}

/// 没有解密器 ⇒ `Err`（凭据面失败是**基础设施**失败，不是产品判决）。
#[tokio::test]
async fn a_missing_decrypter_is_an_infrastructure_failure() {
    let api = FakeApi::new();
    let store = MemoryStore::new();
    let platform = inst();
    store.put_installation(&platform);
    store.put_delivery(delivery_row(
        task(),
        Id(Uuid::from_u128(BINDING)),
        Id(Uuid::from_u128(INST)),
        "oc_main",
        ChatType::Group,
        Some("om_1"),
        None,
        json!({}),
    ));
    let bridged = store.store();
    let delivery = LarkOutboundDelivery::new(
        Arc::clone(&bridged) as Arc<dyn PatcherQueries>,
        Arc::clone(&api) as Arc<dyn ApiClient>,
    );
    let error = delivery
        .on_chat_done(task(), "hi")
        .await
        .expect_err("must fail");
    assert!(error.to_string().contains("credentials resolver missing"));
    assert!(!error.to_string().contains("secret"), "不回显任何凭据");
}

// ---------------------------------------------------------------------
// `task:failed`
// ---------------------------------------------------------------------

/// 失败走**错误卡**（上游逐字：视觉区分对用户真的有用）。
#[tokio::test]
async fn a_failure_sends_an_error_card() {
    let (api, _, delivery) =
        fixture("oc_main", ChatType::Group, Some("om_1"), None, json!({})).await;
    let outcome = delivery
        .on_task_failed(task(), "boom")
        .await
        .expect("send error card");
    assert!(matches!(outcome, DeliveryOutcome::ErrorCard { .. }));
    match &api.log()[0] {
        Call::SendCard {
            card_json,
            reply_message_id,
            ..
        } => {
            assert!(card_json.contains("Run failed: boom"));
            assert_eq!(reply_message_id, "om_1");
        }
        other => panic!("expected SendCard, got {other:?}"),
    }
}

/// 失败卡也要先过话题守卫。
#[tokio::test]
async fn a_failure_in_a_triggerless_topic_is_skipped() {
    let (api, _, delivery) = fixture(
        "oc_main#t_1",
        ChatType::Group,
        None,
        Some("t_1"),
        json!({"chat_id": "oc_main"}),
    )
    .await;
    assert_eq!(
        delivery.on_task_failed(task(), "boom").await.expect("skip"),
        DeliveryOutcome::Skipped(SkipReason::TopicWithoutTrigger)
    );
    assert!(api.log().is_empty());
}

// ---------------------------------------------------------------------
// `task:cancelled`
// ---------------------------------------------------------------------

/// 取消 ⇒ **只**撤打字指示，什么都不发。
#[tokio::test]
async fn cancellation_only_clears_the_typing_indicator() {
    let api = FakeApi::new();
    let store = MemoryStore::new();
    let platform = inst();
    store.put_installation(&platform);
    let bridged = store.store();
    let clock = Arc::new(crate::lark::typing::ManualWallClock::new(1_700_000_000_000));
    let typing = Arc::new(
        crate::lark::typing::TypingIndicatorManager::with_snapshot_only(
            Arc::clone(&api) as Arc<dyn ApiClient>,
            decrypter(),
        )
        .with_clock(Arc::clone(&clock) as Arc<dyn crate::lark::typing::WallClock>),
    );
    let delivery = LarkOutboundDelivery::new(
        Arc::clone(&bridged) as Arc<dyn PatcherQueries>,
        Arc::clone(&api) as Arc<dyn ApiClient>,
    )
    .with_decrypter(decrypter())
    .with_typing(Arc::clone(&typing));
    let session = Id(Uuid::from_u128(SESSION));
    typing
        .add_now(&platform, session, "om_1", "1700000000000")
        .await;
    assert_eq!(typing.tracked_reactions(session), 1, "夹具：表情先贴上");

    let outcome = delivery
        .on_task_cancelled(Some(session))
        .await
        .expect("clear");
    assert_eq!(
        outcome,
        DeliveryOutcome::TypingCleared {
            session_id: session
        }
    );

    let calls = api.log();
    assert!(
        calls
            .iter()
            .any(|call| matches!(call, Call::DeleteReaction { .. })),
        "表情被撤了: {calls:?}"
    );
    assert!(
        !calls
            .iter()
            .any(|call| matches!(call, Call::SendText { .. } | Call::SendCard { .. })),
        "取消不发任何消息: {calls:?}"
    );
}

/// 没有会话 id（非 chat 任务）⇒ 什么都不做（也**不**报错）。
#[tokio::test]
async fn cancellation_without_a_session_is_a_noop() {
    let api = FakeApi::new();
    let store = MemoryStore::new();
    let bridged = store.store();
    let delivery = LarkOutboundDelivery::new(
        Arc::clone(&bridged) as Arc<dyn PatcherQueries>,
        Arc::clone(&api) as Arc<dyn ApiClient>,
    )
    .with_decrypter(decrypter());

    assert_eq!(
        delivery.on_task_cancelled(None).await.expect("no-op"),
        DeliveryOutcome::Skipped(SkipReason::NoDeliveryRow)
    );
    assert!(api.log().is_empty());
}

mod cards;
