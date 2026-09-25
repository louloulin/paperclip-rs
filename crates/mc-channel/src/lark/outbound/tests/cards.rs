//! `outbound/tests.rs` 的**卡片面**用例：渲染器 / 卡片 patch 的生命周期 / 诊断与凭据纪律。
//!
//! 拆出本文件是**门 ⑩**（单文件 800 行硬限）的要求；切点与 `outbound/cards.rs` 一致
//! （卡片渲染与卡行生命周期自成一段）。共用装置在 [`super`]。

use std::sync::Arc;

use mc_core::id::Id;
use serde_json::{json, Value as Json};
use uuid::Uuid;

use super::super::cards::render_error;
use super::{fixture, inst, task, INST, SESSION};
use crate::lark::client::ApiClient;
use crate::lark::outbound::{
    CardKind, CardStatus, DefaultRenderer, DeliveryOutcome, LarkCardPatcher, LarkOutboundDelivery,
    PatcherQueries, RenderInput, Renderer, SkipReason, DEFAULT_CARD_HEADER,
};
use crate::lark::tests::support::{decrypter, legacy_binding_row, Call, FakeApi, MemoryStore};
use crate::lark::types::ChatType;

// ---------------------------------------------------------------------
// 卡片渲染（上游 `defaultRenderer`）
// ---------------------------------------------------------------------

/// **`update_multi: true` 必须出现在每一种变体上**（模块文档第 1 条：缺了它，第二次之后的
/// patch 会在 Lark 侧静默 no-op，而本地状态照样翻成 `streaming`）。
#[test]
fn every_rendered_card_declares_update_multi() {
    let renderer = DefaultRenderer;
    for kind in [
        CardKind::Thinking,
        CardKind::Running,
        CardKind::Final,
        CardKind::Error,
    ] {
        let render = renderer
            .render(&RenderInput {
                kind,
                agent_name: "Bot".to_string(),
                content: "answer".to_string(),
                error_message: "boom".to_string(),
                ..RenderInput::default()
            })
            .expect("render");
        let doc: Json = serde_json::from_str(&render.json).expect("valid json");
        assert_eq!(
            doc["config"]["update_multi"],
            json!(true),
            "{kind:?} 的卡缺 update_multi"
        );
        assert_eq!(doc["config"]["wide_screen_mode"], json!(true));
        assert_eq!(doc["header"]["title"]["content"], json!("Bot"));
    }
}

/// 每种变体的正文逐字（上游 `defaultRenderer` 的 switch）。
#[test]
fn card_bodies_match_the_upstream_table() {
    let renderer = DefaultRenderer;
    let body = |kind: CardKind, content: &str, error: &str| {
        let render = renderer
            .render(&RenderInput {
                kind,
                content: content.to_string(),
                error_message: error.to_string(),
                ..RenderInput::default()
            })
            .expect("render");
        let doc: Json = serde_json::from_str(&render.json).expect("valid json");
        doc["elements"][0]["text"]["content"]
            .as_str()
            .expect("body")
            .to_string()
    };
    assert_eq!(body(CardKind::Thinking, "", ""), "Thinking…");
    assert_eq!(body(CardKind::Running, "", ""), "Working on it…");
    assert_eq!(body(CardKind::Final, "answer", ""), "answer");
    assert_eq!(body(CardKind::Final, "", ""), "Done.");
    assert_eq!(body(CardKind::Error, "", "boom"), "Run failed: boom");
    assert_eq!(body(CardKind::Error, "", ""), "Run failed.");
}

/// agent 名字缺失 ⇒ 头部回落 `Multica`；`RenderInput` 的默认变体是 `Thinking`。
#[test]
fn a_missing_agent_name_falls_back_to_the_default_header() {
    let render = DefaultRenderer
        .render(&RenderInput::default())
        .expect("render");
    let doc: Json = serde_json::from_str(&render.json).expect("valid json");
    assert_eq!(
        doc["header"]["title"]["content"],
        json!(DEFAULT_CARD_HEADER)
    );
    assert_eq!(RenderInput::default().kind, CardKind::Thinking);
    assert_eq!(CardKind::default(), CardKind::Thinking);
}

/// 一次性状态卡**不带** `update_multi`（与默认渲染器的生命周期不同，见函数文档）。
#[test]
fn one_shot_notice_cards_are_not_updatable() {
    let card = crate::lark::outbound::render_notice_card("Bot", "offline");
    let doc: Json = serde_json::from_str(&card).expect("valid json");
    assert!(doc["config"].get("update_multi").is_none());
    assert_eq!(doc["header"]["title"]["content"], json!("Bot"));
    assert_eq!(doc["elements"][0]["text"]["content"], json!("offline"));
    assert_eq!(doc["header"]["template"], json!("grey"));
}

/// 卡片词表 → 状态行的映射。
#[test]
fn card_kinds_map_to_statuses() {
    assert_eq!(CardKind::Thinking.status(), CardStatus::Pending);
    assert_eq!(CardKind::Running.status(), CardStatus::Streaming);
    assert_eq!(CardKind::Final.status(), CardStatus::Final);
    assert_eq!(CardKind::Error.status(), CardStatus::Error);
    assert_eq!(CardKind::Error.as_str(), "error");
}

/// 认不出的变体（上游 `unknown card kind %q`，本仓只有模板自己会返回它）。
#[test]
fn render_error_classifies_unknown_kinds() {
    let error = crate::lark::outbound::RenderError::UnknownKind {
        kind: "weird".to_string(),
    };
    assert_eq!(error.to_string(), "lark: unknown card kind weird");
    assert_eq!(
        render_error(&error).to_string(),
        "engine: infrastructure failure: lark: unknown card kind weird"
    );
}

// ---------------------------------------------------------------------
// 卡片 patch 的生命周期（专属验收：`message_edit` 能力位）
// ---------------------------------------------------------------------

/// 开卡 → patch → 收口：每一步都在行上留下状态，且每次 patch 发的是**整卡**。
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn the_card_lifecycle_sends_once_and_patches_the_whole_card_afterwards() {
    let (api, store, _) = fixture("oc_main", ChatType::Group, Some("om_1"), None, json!({})).await;
    let bridged = store.store();
    store.put_legacy_binding(legacy_binding_row(
        Id(Uuid::from_u128(SESSION)),
        Id(Uuid::from_u128(INST)),
        "oc_main",
        ChatType::Group,
    ));
    let patcher = LarkCardPatcher::new(
        Arc::clone(&bridged) as Arc<dyn PatcherQueries>,
        Arc::clone(&api) as Arc<dyn ApiClient>,
    )
    .with_decrypter(decrypter());

    let opened = patcher
        .begin(
            Id(Uuid::from_u128(INST)),
            Id(Uuid::from_u128(SESSION)),
            task(),
            CardKind::Thinking,
            &RenderInput {
                task_id: Some(task()),
                agent_name: "Bot".to_string(),
                ..RenderInput::default()
            },
        )
        .await
        .expect("begin");
    let DeliveryOutcome::Patched { card_id, status } = opened else {
        panic!("expected a patch outcome, got {opened:?}");
    };
    assert_eq!(status, CardStatus::Pending);
    assert_eq!(
        store.card(task()).expect("row").status,
        CardStatus::Pending.as_str()
    );

    let streaming = patcher
        .patch(
            Id(Uuid::from_u128(INST)),
            card_id,
            CardKind::Running,
            &RenderInput {
                task_id: Some(task()),
                content: "working".to_string(),
                ..RenderInput::default()
            },
        )
        .await
        .expect("patch");
    assert_eq!(
        streaming,
        DeliveryOutcome::Patched {
            card_id,
            status: CardStatus::Streaming
        }
    );
    assert_eq!(
        store.card(task()).expect("row").status,
        CardStatus::Streaming.as_str()
    );
    assert!(store.card(task()).expect("row").last_patched_at.is_some());

    let settled = patcher
        .patch(
            Id(Uuid::from_u128(INST)),
            card_id,
            CardKind::Final,
            &RenderInput {
                task_id: Some(task()),
                content: "answer".to_string(),
                ..RenderInput::default()
            },
        )
        .await
        .expect("final");
    assert_eq!(
        settled,
        DeliveryOutcome::Patched {
            card_id,
            status: CardStatus::Final
        }
    );

    // 收口之后再 patch ⇒ 跳过（**不**发，也**不**静默发一半）。
    let after = patcher
        .patch(
            Id(Uuid::from_u128(INST)),
            card_id,
            CardKind::Running,
            &RenderInput {
                task_id: Some(task()),
                ..RenderInput::default()
            },
        )
        .await
        .expect("settled");
    assert_eq!(after, DeliveryOutcome::Skipped(SkipReason::CardSettled));

    // 调用序列：1 张新卡 + 2 次 patch（第三次被状态挡住）。
    let calls = api.log();
    assert_eq!(calls.len(), 3, "{calls:?}");
    assert!(matches!(calls[0], Call::SendCard { .. }));
    assert!(matches!(calls[1], Call::PatchCard { .. }));
    assert!(matches!(calls[2], Call::PatchCard { .. }));
    for call in &calls[1..] {
        let Call::PatchCard { card_json, .. } = call else {
            continue;
        };
        let doc: Json = serde_json::from_str(card_json).expect("valid json");
        assert_eq!(
            doc["config"]["update_multi"],
            json!(true),
            "每次 patch 都必须是**完整**且可 patch 的卡"
        );
    }
}

/// 没有卡片行 ⇒ 跳过（`NoCard`，**不**是"收口"）。
#[tokio::test]
async fn patching_a_task_that_has_no_card_is_skipped() {
    let (api, store, _) = fixture("oc_main", ChatType::Group, Some("om_1"), None, json!({})).await;
    let bridged = store.store();
    let patcher = LarkCardPatcher::new(
        Arc::clone(&bridged) as Arc<dyn PatcherQueries>,
        Arc::clone(&api) as Arc<dyn ApiClient>,
    )
    .with_decrypter(decrypter());

    let outcome = patcher
        .patch(
            Id(Uuid::from_u128(INST)),
            Id(Uuid::from_u128(0x9999)),
            CardKind::Running,
            &RenderInput {
                task_id: Some(task()),
                ..RenderInput::default()
            },
        )
        .await
        .expect("skip");
    assert_eq!(outcome, DeliveryOutcome::Skipped(SkipReason::NoCard));
    assert!(api.log().is_empty());
}

/// 平台 patch 失败 ⇒ `Err`（链路失败），且状态**不**翻。
#[tokio::test]
async fn a_failed_patch_does_not_flip_the_row_status() {
    let (api, store, _) = fixture("oc_main", ChatType::Group, Some("om_1"), None, json!({})).await;
    let bridged = store.store();
    let patcher = LarkCardPatcher::new(
        Arc::clone(&bridged) as Arc<dyn PatcherQueries>,
        Arc::clone(&api) as Arc<dyn ApiClient>,
    )
    .with_decrypter(decrypter());

    let opened = patcher
        .begin(
            Id(Uuid::from_u128(INST)),
            Id(Uuid::from_u128(SESSION)),
            task(),
            CardKind::Thinking,
            &RenderInput {
                task_id: Some(task()),
                ..RenderInput::default()
            },
        )
        .await
        .expect("begin");
    let DeliveryOutcome::Patched { card_id, .. } = opened else {
        panic!("expected a patch outcome");
    };

    api.fail_patch_with(crate::lark::tests::support::transport_error());
    let error = patcher
        .patch(
            Id(Uuid::from_u128(INST)),
            card_id,
            CardKind::Running,
            &RenderInput {
                task_id: Some(task()),
                ..RenderInput::default()
            },
        )
        .await
        .expect_err("must fail");
    assert!(error.to_string().contains("patch card"));
    assert_eq!(
        store.card(task()).expect("row").status,
        CardStatus::Pending.as_str(),
        "失败的 patch 不翻状态"
    );
}

/// 开卡时安装已撤销 ⇒ 跳过（**不**发卡、**不**落行）。
#[tokio::test]
async fn opening_a_card_on_a_revoked_installation_is_skipped() {
    let api = FakeApi::new();
    let store = MemoryStore::new();
    let mut platform = inst();
    platform.status = "revoked".to_string();
    store.put_installation(&platform);
    let bridged = store.store();
    let patcher = LarkCardPatcher::new(
        Arc::clone(&bridged) as Arc<dyn PatcherQueries>,
        Arc::clone(&api) as Arc<dyn ApiClient>,
    )
    .with_decrypter(decrypter());

    let outcome = patcher
        .begin(
            Id(Uuid::from_u128(INST)),
            Id(Uuid::from_u128(SESSION)),
            task(),
            CardKind::Thinking,
            &RenderInput::default(),
        )
        .await
        .expect("skip");
    assert_eq!(
        outcome,
        DeliveryOutcome::Skipped(SkipReason::InstallationRevoked)
    );
    assert!(api.log().is_empty());
    assert!(store.card(task()).is_none());
}

// ---------------------------------------------------------------------
// 诊断面与凭据纪律
// ---------------------------------------------------------------------

/// 两个结构的 `Debug` 都只有端口存在性。
#[test]
fn debug_outputs_are_credential_free() {
    let api = FakeApi::new();
    let store = MemoryStore::new();
    let bridged = store.store();
    let delivery = LarkOutboundDelivery::new(
        Arc::clone(&bridged) as Arc<dyn PatcherQueries>,
        Arc::clone(&api) as Arc<dyn ApiClient>,
    )
    .with_decrypter(decrypter());
    let patcher = LarkCardPatcher::new(
        Arc::clone(&bridged) as Arc<dyn PatcherQueries>,
        Arc::clone(&api) as Arc<dyn ApiClient>,
    )
    .with_decrypter(decrypter());

    for rendered in [format!("{delivery:?}"), format!("{patcher:?}")] {
        assert!(rendered.contains("has_decrypter: true"));
        for forbidden in ["cli_", "app-secret", "app_secret_encrypted"] {
            assert!(
                !rendered.contains(forbidden),
                "出站面的 Debug 不得出现 {forbidden}: {rendered}"
            );
        }
    }
}

/// `DeliveryOutcome` 与 `SkipReason` 的取值稳定（日志 / 看板按它们聚合）。
#[test]
fn outcome_and_skip_reason_literals_are_stable() {
    let literals: Vec<&str> = [
        SkipReason::NoDeliveryRow,
        SkipReason::NotFeishu,
        SkipReason::NotChannelIngested,
        SkipReason::InstallationMissing,
        SkipReason::InstallationRevoked,
        SkipReason::TopicWithoutTrigger,
        SkipReason::EmptyContent,
        SkipReason::CardSettled,
        SkipReason::NoCard,
    ]
    .iter()
    .map(|reason| reason.as_str())
    .collect();
    assert_eq!(
        literals,
        vec![
            "no_delivery_row",
            "not_feishu",
            "not_channel_ingested",
            "installation_missing",
            "installation_revoked",
            "topic_without_trigger",
            "empty_content",
            "card_settled",
            "no_card",
        ]
    );
    assert_eq!(crate::lark::outbound::CHANNEL_TYPE_FEISHU, "feishu");
    assert_eq!(crate::lark::outbound::INSTALLATION_ACTIVE, "active");
}

/// 安装凭据的解析走真 `secretbox`（密文解不开 ⇒ `Infra`，且**不回显**字节）。
#[test]
fn credentials_resolution_uses_the_real_secret_box() {
    use crate::lark::feishu_channel::credentials::Decrypter;
    use crate::lark::outbound::{credentials_error, installation_credentials};

    let platform = inst();
    assert!(installation_credentials(&platform, Some(&decrypter())).is_ok());

    // 无解密器 ⇒ 基础设施失败。
    let missing = installation_credentials(&platform, None).expect_err("must fail");
    assert!(missing.to_string().contains("credentials resolver missing"));

    let wrong = Decrypter::secret_box(
        mc_secrets::secretbox::SecretBox::new(&[0x33; 32]).expect("key size"),
    );
    let error = installation_credentials(&platform, Some(&wrong)).expect_err("must fail");
    let rendered = error.to_string();
    assert!(rendered.contains("decrypt app_secret failed"));
    // **只**报告类别：不带明文、不带密文、不带 `AppSecret` 值。
    assert!(!rendered.contains("app-secret"), "不回显明文: {rendered}");
    assert!(
        !rendered.contains("app_secret_encrypted"),
        "不展开密文列: {rendered}"
    );
    assert!(
        rendered.len() < 120,
        "错误文案不该夹带载荷（长度 {}）: {rendered}",
        rendered.len()
    );

    // 转换器逐个变体（不 panic、只报类别）。
    for variant in [
        crate::lark::feishu_channel::ConfigError::MissingAppId,
        crate::lark::feishu_channel::ConfigError::MissingSecret,
        crate::lark::feishu_channel::ConfigError::SecretNotBase64 { length: 7 },
    ] {
        assert!(!credentials_error(&variant).to_string().contains("c2VjcmV0"));
    }
}

/// `FallbackError` 的分类投影（只带类别，不带平台 `msg`）。
#[test]
fn fallback_error_projects_only_the_class() {
    use crate::lark::outbound::{fallback_error, FallbackError};

    let error = FallbackError::Send {
        op: "send text message",
        original: crate::lark::tests::support::unsupported_reply_target_error(),
        fallback: None,
    };
    let engine = fallback_error(&error);
    let rendered = engine.to_string();
    assert!(rendered.contains("send text message"));
    assert!(rendered.contains("refused"), "{rendered}");
    assert!(!error.fell_back());
    assert_eq!(error.op(), "send text message");
    assert_eq!(
        error.original().class(),
        crate::lark::client::ErrorClass::Refused
    );
}

/// `#` 会话键（本仓的话题隔离形态）⇒ 出站寻址回落到真实 chat id（`config` 那一格）。
#[test]
fn the_composite_session_key_resolves_to_the_real_chat_id() {
    use crate::lark::outbound::{
        binding_config_json, delivery_config_with_sender, outbound_chat_id,
    };
    use crate::lark::store::ChatSessionBinding;
    use crate::lark::tests::support::delivery_row;

    let binding = ChatSessionBinding::from_delivery_row(&delivery_row(
        task(),
        Id(Uuid::from_u128(crate::lark::outbound::tests::BINDING)),
        Id(Uuid::from_u128(INST)),
        "oc_main#t_1",
        ChatType::Group,
        Some("om_1"),
        Some("t_1"),
        json!({"chat_id": "oc_main"}),
    ));
    assert_eq!(outbound_chat_id(&binding).as_str(), "oc_main");
    assert_eq!(
        binding_config_json("oc_main"),
        json!({"chat_id": "oc_main"})
    );
    assert_eq!(
        delivery_config_with_sender(json!({"chat_id": "oc_main"}), "ou_x"),
        json!({"chat_id": "oc_main", "sender_id": "ou_x"})
    );
}
