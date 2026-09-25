//! `replier.rs` 的用例（M7-13）：判决 → 文案 / 卡片，绑定卡与私聊不可达回落，深链与消毒。

use std::sync::Arc;

use mc_core::id::Id;
use uuid::Uuid;

use super::*;
use crate::lark::client::CODE_NO_AVAILABILITY;
use crate::lark::tests::support::{
    decrypter, dispatch, inbound_message, installation, Call, FakeApi, FakeMinter, MemoryStore,
};
use crate::lark::types::ChatType;

const INST: u128 = 0x3000;

fn inst() -> LarkInstallation {
    installation(INST, "cli_a")
}

/// 装一个**接线完整**的回复器（agent 名字查得到）。
fn replier(api: &Arc<FakeApi>, minter: &Arc<FakeMinter>, app_url: &str) -> LarkOutcomeReplier {
    let store = MemoryStore::new();
    let platform = inst();
    store.put_agent_name(platform.agent_id, "Bot");
    let bridged = store.store();
    LarkOutcomeReplier::new(
        Arc::clone(api) as Arc<dyn ApiClient>,
        Arc::clone(minter) as Arc<dyn BindingTokenMinter>,
        decrypter(),
        bridged as Arc<dyn OutcomeReplierQueries>,
        app_url,
        "",
    )
}

/// 卡片体里的正文（卡片 JSON 会把换行转义 ⇒ 直接比字符串会假红）。
fn card_body(card_json: &str) -> String {
    let doc: serde_json::Value = serde_json::from_str(card_json).expect("valid card json");
    doc["elements"][0]["text"]["content"]
        .as_str()
        .expect("card body")
        .to_string()
}

fn group_message() -> mc_core::channel::message::InboundMessage {
    inbound_message("oc_main", ChatType::Group, "om_1", "", "")
}

// ---------------------------------------------------------------------
// 装配（上游 `NewLarkOutcomeReplier` 的降级判据）
// ---------------------------------------------------------------------

/// 依赖缺失 ⇒ noop（**响亮**地降级，不是静默）。
#[tokio::test]
async fn build_downgrades_to_noop_when_wiring_is_incomplete() {
    let api = FakeApi::new();
    let minter = FakeMinter::new();
    let complete = OutcomeReplierConfig {
        client: Some(Arc::clone(&api) as Arc<dyn ApiClient>),
        binding: Some(Arc::clone(&minter) as Arc<dyn BindingTokenMinter>),
        decrypt: Some(decrypter()),
        queries: Some({
            let store = MemoryStore::new();
            store.store() as Arc<dyn OutcomeReplierQueries>
        }),
        app_url: "https://app.example".to_string(),
        binding_path: String::new(),
    };
    assert_eq!(choose(&complete), ReplierChoice::Lark);

    // 逐格删掉一个依赖：每一格都必须降级。
    for missing in 0..4 {
        let mut cfg = complete.clone();
        match missing {
            0 => cfg.client = None,
            1 => cfg.binding = None,
            2 => cfg.decrypt = None,
            _ => cfg.queries = None,
        }
        assert_eq!(
            choose(&cfg),
            ReplierChoice::Noop,
            "缺第 {missing} 个依赖时必须降级"
        );
    }
}

/// 客户端"没配" ⇒ noop（上游 `APIClient.IsConfigured() == false` 那一支）。
#[test]
fn build_downgrades_when_the_client_reports_not_configured() {
    let api = FakeApi::unconfigured();
    let minter = FakeMinter::new();
    let store = MemoryStore::new();
    let cfg = OutcomeReplierConfig {
        client: Some(Arc::clone(&api) as Arc<dyn ApiClient>),
        binding: Some(Arc::clone(&minter) as Arc<dyn BindingTokenMinter>),
        decrypt: Some(decrypter()),
        queries: Some(store.store() as Arc<dyn OutcomeReplierQueries>),
        app_url: String::new(),
        binding_path: String::new(),
    };
    assert_eq!(choose(&cfg), ReplierChoice::Noop);
    // 真的装的是 noop：它**不**发任何东西。
    let noop = build(cfg);
    let resolved = crate::lark::tests::support::resolved(&inst());
    noop.reply(
        &resolved,
        &group_message(),
        &crate::engine::resolvers::RouteResult {
            outcome: crate::engine::resolvers::Outcome::NeedsBinding,
            ..crate::engine::resolvers::RouteResult::default()
        },
    );
    assert!(api.log().is_empty());
}

/// noop 的"本来会回哪几种判决"表（`Ingested` 无 issue 与 `Dropped` 不在内）。
#[test]
fn noop_reports_exactly_the_reply_producing_outcomes() {
    for outcome in [
        Outcome::NeedsBinding,
        Outcome::AgentOffline,
        Outcome::AgentArchived,
        Outcome::FreshPending,
        Outcome::ChatStarted,
        Outcome::IssueUsage,
    ] {
        assert!(NoopOutcomeReplier::would_reply(outcome), "{outcome:?}");
    }
    for outcome in [Outcome::Ingested, Outcome::Dropped] {
        assert!(!NoopOutcomeReplier::would_reply(outcome), "{outcome:?}");
    }
}

/// 绑定路径归一化：空 ⇒ 默认；不带前导斜杠 ⇒ 补上。
#[test]
fn binding_path_is_normalized() {
    assert_eq!(normalize_binding_path(""), DEFAULT_BINDING_PATH);
    assert_eq!(normalize_binding_path("lark/bind"), "/lark/bind");
    assert_eq!(normalize_binding_path("/lark/bind"), "/lark/bind");
    assert_eq!(DEFAULT_BINDING_PATH, "/lark/bind");
    assert_eq!(DEFAULT_NOTICE_HEADER, "Multica");
}

// ---------------------------------------------------------------------
// 状态告知（上游 `sendChatNotice`）
// ---------------------------------------------------------------------

/// 五条"状态卡"判决各自发一张灰头卡，正文**逐字**。
#[tokio::test]
async fn status_outcomes_send_a_notice_card_with_the_upstream_copy() {
    let cases = [
        (Outcome::AgentOffline, AGENT_OFFLINE_COPY),
        (Outcome::AgentArchived, AGENT_ARCHIVED_COPY),
        (Outcome::FreshPending, FRESH_PENDING_COPY),
        (Outcome::ChatStarted, CHAT_STARTED_COPY),
        (Outcome::IssueUsage, ISSUE_USAGE_COPY),
    ];
    for (outcome, copy) in cases {
        let api = FakeApi::new();
        let minter = FakeMinter::new();
        let replier = replier(&api, &minter, "https://app.example");
        let message = group_message();
        let outcome_value = replier
            .reply_now(&inst(), &message, &dispatch(outcome))
            .await;
        assert!(
            matches!(outcome_value, DeliveryOutcome::MarkdownCard { .. }),
            "{outcome:?} 应当发卡"
        );
        match &api.log()[0] {
            Call::SendCard {
                card_json,
                chat_id,
                reply_message_id,
                ..
            } => {
                assert_eq!(chat_id, "oc_main");
                assert_eq!(card_body(card_json), copy, "文案逐字");
                assert!(card_json.contains("Bot"), "头部是 agent 名字");
                assert_eq!(reply_message_id, "om_1", "普通群回复触发那条消息");
            }
            other => panic!("expected SendCard, got {other:?}"),
        }
    }
}

/// `/issue` 缺标题**且带媒体** ⇒ 另一条文案。
#[tokio::test]
async fn issue_usage_with_media_uses_the_other_copy() {
    let api = FakeApi::new();
    let minter = FakeMinter::new();
    let replier = replier(&api, &minter, "https://app.example");
    let message = group_message();
    let mut result = dispatch(Outcome::IssueUsage);
    result.issue_usage_had_media = true;
    replier.reply_now(&inst(), &message, &result).await;

    match &api.log()[0] {
        Call::SendCard { card_json, .. } => {
            assert_eq!(card_body(card_json), ISSUE_USAGE_WITH_MEDIA_COPY);
            assert_ne!(
                card_body(card_json),
                ISSUE_USAGE_COPY,
                "带媒体时用另一条文案"
            );
        }
        other => panic!("expected SendCard, got {other:?}"),
    }
}

/// agent 名字查不到 ⇒ 头部回落默认值（**不**让回复失败）。
#[tokio::test]
async fn a_missing_agent_name_falls_back_to_the_default_header() {
    let api = FakeApi::new();
    let minter = FakeMinter::new();
    let store = MemoryStore::new();
    let replier = LarkOutcomeReplier::new(
        Arc::clone(&api) as Arc<dyn ApiClient>,
        Arc::clone(&minter) as Arc<dyn BindingTokenMinter>,
        decrypter(),
        store.store() as Arc<dyn OutcomeReplierQueries>,
        "https://app.example",
        "",
    );
    replier
        .reply_now(&inst(), &group_message(), &dispatch(Outcome::AgentOffline))
        .await;

    match &api.log()[0] {
        Call::SendCard { card_json, .. } => {
            assert!(card_json.contains(DEFAULT_NOTICE_HEADER));
        }
        other => panic!("expected SendCard, got {other:?}"),
    }
}

/// `Dropped` ⇒ **沉默**（一条调用都不发）。
#[tokio::test]
async fn a_dropped_verdict_is_silent() {
    let api = FakeApi::new();
    let minter = FakeMinter::new();
    let replier = replier(&api, &minter, "https://app.example");
    replier
        .reply_now(&inst(), &group_message(), &dispatch(Outcome::Dropped))
        .await;
    assert!(api.log().is_empty());
}

/// `Ingested` **没有** issue ⇒ 沉默（agent 自己的回复走 patcher）。
#[tokio::test]
async fn an_ingested_verdict_without_an_issue_is_silent() {
    let api = FakeApi::new();
    let minter = FakeMinter::new();
    let replier = replier(&api, &minter, "https://app.example");
    replier
        .reply_now(&inst(), &group_message(), &dispatch(Outcome::Ingested))
        .await;
    assert!(api.log().is_empty());
}

/// 会话 id 空 ⇒ 不发（上游 `missing chat_id`），且**不**panic。
#[tokio::test]
async fn a_notice_without_a_chat_id_is_not_sent() {
    let api = FakeApi::new();
    let minter = FakeMinter::new();
    let replier = replier(&api, &minter, "https://app.example");
    let mut message = group_message();
    message.source.chat_id = String::new();
    replier
        .reply_now(&inst(), &message, &dispatch(Outcome::AgentOffline))
        .await;
    assert!(api.log().is_empty());
}

// ---------------------------------------------------------------------
// 绑定卡（上游 `sendBindingPrompt`）
// ---------------------------------------------------------------------

/// 铸令牌 + 拼 URL + 私聊发给发件人的 `open_id`。
#[tokio::test]
async fn the_binding_prompt_mints_a_token_and_targets_the_sender() {
    let api = FakeApi::new();
    let minter = FakeMinter::new();
    let replier = replier(&api, &minter, "https://app.example/");
    let message = group_message();
    let mut result = dispatch(Outcome::NeedsBinding);
    result.sender_open_id = "ou_sender".to_string();

    let outcome = replier.reply_now(&inst(), &message, &result).await;
    assert!(matches!(outcome, DeliveryOutcome::Text { .. }));

    assert_eq!(
        minter.requests(),
        vec![(
            Id(Uuid::from_u128(0x9000)),
            Id(Uuid::from_u128(INST)),
            "ou_sender".to_string()
        )]
    );
    match &api.log()[0] {
        Call::SendBindingPrompt { open_id, bind_url } => {
            assert_eq!(open_id, "ou_sender");
            // 尾斜杠被剥掉 + 默认路径 + 令牌在后面。
            assert_eq!(
                bind_url,
                "https://app.example/lark/bind?token=raw-binding-token"
            );
        }
        other => panic!("expected SendBindingPrompt, got {other:?}"),
    }
}

/// 令牌里的 `-` / `_` 是 unreserved ⇒ 不被百分号编码（上游 `url.QueryEscape` 的形态）。
#[tokio::test]
async fn the_binding_token_is_escaped_like_go_query_escape() {
    let api = FakeApi::new();
    let minter = FakeMinter::new();
    let replier = replier(&api, &minter, "https://app.example");
    let message = group_message();
    let mut result = dispatch(Outcome::NeedsBinding);
    result.sender_open_id = "ou/with space".to_string();
    replier.reply_now(&inst(), &message, &result).await;

    match &api.log()[0] {
        Call::SendBindingPrompt { bind_url, .. } => {
            assert!(bind_url.contains("token=raw-binding-token"));
            assert!(!bind_url.contains(' '));
        }
        other => panic!("expected SendBindingPrompt, got {other:?}"),
    }
    assert_eq!(url_encode("a-b_c.d~e"), "a-b_c.d~e");
    assert_eq!(url_encode("a b"), "a+b");
    assert_eq!(url_encode("a/b"), "a%2Fb");
}

/// 没配 app url ⇒ **不**铸令牌、不发卡（上游 `app_url not configured`）。
#[tokio::test]
async fn a_binding_prompt_without_an_app_url_is_not_sent() {
    let api = FakeApi::new();
    let minter = FakeMinter::new();
    let replier = replier(&api, &minter, "");
    replier
        .reply_now(&inst(), &group_message(), &dispatch(Outcome::NeedsBinding))
        .await;
    assert!(api.log().is_empty());
    assert!(minter.requests().is_empty(), "前置不满足 ⇒ 不铸令牌");
}

/// 没有发件人 `open_id` ⇒ 不发（上游 `missing sender open_id`）。
#[tokio::test]
async fn a_binding_prompt_without_a_sender_is_not_sent() {
    let api = FakeApi::new();
    let minter = FakeMinter::new();
    let replier = replier(&api, &minter, "https://app.example");
    let mut result = dispatch(Outcome::NeedsBinding);
    result.sender_open_id = String::new();
    replier.reply_now(&inst(), &group_message(), &result).await;
    assert!(api.log().is_empty());
}

/// 铸令牌失败 ⇒ 不发（**不**把空令牌塞进 URL）。
#[tokio::test]
async fn a_failed_mint_leaves_the_user_unmessaged() {
    let api = FakeApi::new();
    let minter = FakeMinter::new();
    *minter.failure.lock().expect("poisoned") = Some("no token service".to_string());
    let replier = replier(&api, &minter, "https://app.example");
    replier
        .reply_now(&inst(), &group_message(), &dispatch(Outcome::NeedsBinding))
        .await;
    assert!(api.log().is_empty());
}

/// **群聊里私聊不可达** ⇒ 回落一条会话内文案（上游逐字：*after private prompt unavailable*）。
#[tokio::test]
async fn an_unreachable_private_chat_falls_back_to_an_in_chat_notice() {
    let api = FakeApi::new();
    let minter = FakeMinter::new();
    let replier = replier(&api, &minter, "https://app.example");
    api.fail_binding_with(ApiError::Refused {
        op: "binding prompt",
        status: None,
        code: CODE_NO_AVAILABILITY,
    });

    let outcome = replier
        .reply_now(&inst(), &group_message(), &dispatch(Outcome::NeedsBinding))
        .await;
    assert!(matches!(outcome, DeliveryOutcome::MarkdownCard { .. }));
    let calls = api.log();
    assert_eq!(calls.len(), 2, "先试私聊、再回落会话内: {calls:?}");
    match &calls[1] {
        Call::SendCard { card_json, .. } => {
            assert_eq!(card_body(card_json), BINDING_PROMPT_UNAVAILABLE_COPY);
        }
        other => panic!("expected a notice card, got {other:?}"),
    }
}

/// **p2p** 里同样的失败**不**回落（没有"会话内"这个概念 —— 那条路本来就是会话）。
#[tokio::test]
async fn an_unreachable_private_chat_in_p2p_does_not_fall_back() {
    let api = FakeApi::new();
    let minter = FakeMinter::new();
    let replier = replier(&api, &minter, "https://app.example");
    api.fail_binding_with(ApiError::Refused {
        op: "binding prompt",
        status: None,
        code: CODE_NO_AVAILABILITY,
    });

    replier
        .reply_now(
            &inst(),
            &inbound_message("oc_dm", ChatType::P2p, "om_1", "", ""),
            &dispatch(Outcome::NeedsBinding),
        )
        .await;
    assert_eq!(api.log().len(), 1, "p2p 只有那一次尝试");
}

/// 别的失败码 ⇒ **不**回落（上游只认 `codeNoAvailability`）。
#[tokio::test]
async fn an_ordinary_binding_failure_does_not_fall_back() {
    let api = FakeApi::new();
    let minter = FakeMinter::new();
    let replier = replier(&api, &minter, "https://app.example");
    api.fail_binding_with(ApiError::Transport {
        op: "binding prompt",
    });

    replier
        .reply_now(&inst(), &group_message(), &dispatch(Outcome::NeedsBinding))
        .await;
    assert_eq!(api.log().len(), 1);
    assert!(!is_binding_prompt_unavailable(&ApiError::Transport {
        op: "binding prompt"
    }));
    assert!(is_binding_prompt_unavailable(&ApiError::Refused {
        op: "binding prompt",
        status: None,
        code: CODE_NO_AVAILABILITY,
    }));
}

// ---------------------------------------------------------------------
// 分类过的回落（与出站面共用）
// ---------------------------------------------------------------------

/// 通知卡也会走那条**分类过的**会话层回落。
#[tokio::test]
async fn notice_cards_share_the_classified_chat_level_fallback() {
    let api = FakeApi::new();
    let minter = FakeMinter::new();
    let replier = replier(&api, &minter, "https://app.example");
    api.fail_send_with(crate::lark::tests::support::unsupported_reply_target_error());

    let message = inbound_message("oc_main", ChatType::Group, "om_1", "t_1", "");
    replier
        .reply_now(&inst(), &message, &dispatch(Outcome::AgentOffline))
        .await;

    let calls = api.log();
    assert_eq!(calls.len(), 2, "话题回复不可用 ⇒ 回落一次: {calls:?}");
    assert!(matches!(
        calls[0],
        Call::SendCard {
            in_thread: true,
            ..
        }
    ));
    assert!(matches!(
        calls[1],
        Call::SendCard {
            in_thread: false,
            ref reply_message_id,
            ..
        } if reply_message_id.is_empty()
    ));
}

// ---------------------------------------------------------------------
// 诊断面与 engine 端口
// ---------------------------------------------------------------------

/// 回复器的 `Debug` 只有存在性与 URL（**没有**令牌字段）。
#[tokio::test]
async fn the_replier_debug_never_shows_a_token() {
    let api = FakeApi::new();
    let minter = FakeMinter::new();
    let replier = replier(&api, &minter, "https://app.example");
    let rendered = format!("{replier:?}");
    assert!(rendered.contains("LarkOutcomeReplier"));
    assert!(rendered.contains("app_url: \"https://app.example\""));
    for forbidden in ["raw-binding-token", "app-secret", "app_secret_encrypted"] {
        assert!(
            !rendered.contains(forbidden),
            "回复器的 Debug 不得出现 {forbidden}: {rendered}"
        );
    }
}

/// `build` 出来的接线形态真的会回复（端到端一小段）。
#[tokio::test]
async fn a_built_replier_actually_replies() {
    let api = FakeApi::new();
    let minter = FakeMinter::new();
    let store = MemoryStore::new();
    let platform = inst();
    store.put_agent_name(platform.agent_id, "Bot");
    let port = build(OutcomeReplierConfig {
        client: Some(Arc::clone(&api) as Arc<dyn ApiClient>),
        binding: Some(Arc::clone(&minter) as Arc<dyn BindingTokenMinter>),
        decrypt: Some(decrypter()),
        queries: Some(store.store() as Arc<dyn OutcomeReplierQueries>),
        app_url: "https://app.example".to_string(),
        binding_path: "lark/bind".to_string(),
    });

    // engine 的同步接缝：装一条 `ResolvedInstallation`（含本 adapter 的投影）。
    let resolved = crate::lark::tests::support::resolved(&platform);
    let message = group_message();
    let mut result = crate::engine::resolvers::RouteResult {
        outcome: crate::engine::resolvers::Outcome::NeedsBinding,
        sender: "ou_sender".to_string(),
        ..crate::engine::resolvers::RouteResult::default()
    };
    result.installation_id = Some(platform.id);
    port.reply(&resolved, &message, &result);

    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    assert_eq!(api.log().len(), 1);
    assert!(matches!(api.log()[0], Call::SendBindingPrompt { .. }));
}

/// `ResolvedInstallation` 里没有本 adapter 的安装投影 ⇒ 打 warn 后返回（**不**猜）。
#[tokio::test]
async fn the_sync_port_skips_a_foreign_installation_envelope() {
    let api = FakeApi::new();
    let minter = FakeMinter::new();
    let port = build(OutcomeReplierConfig {
        client: Some(Arc::clone(&api) as Arc<dyn ApiClient>),
        binding: Some(Arc::clone(&minter) as Arc<dyn BindingTokenMinter>),
        decrypt: Some(decrypter()),
        queries: Some(MemoryStore::new().store() as Arc<dyn OutcomeReplierQueries>),
        app_url: "https://app.example".to_string(),
        binding_path: String::new(),
    });
    let foreign = crate::engine::resolvers::ResolvedInstallation::new(
        Id(Uuid::from_u128(INST)),
        Id(Uuid::from_u128(0x9000)),
        Id(Uuid::from_u128(0x9100)),
        Id(Uuid::from_u128(0x9200)),
        crate::lark::resolvers::TYPE_LARK,
        true,
    );
    port.reply(
        &foreign,
        &group_message(),
        &crate::engine::resolvers::RouteResult {
            outcome: crate::engine::resolvers::Outcome::NeedsBinding,
            ..crate::engine::resolvers::RouteResult::default()
        },
    );
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    assert!(api.log().is_empty());
}

/// `ApiFailure` 的归类（warn 用的就是它）。
#[test]
fn api_failure_carries_only_the_class() {
    let missing = ApiFailure::Missing("missing chat_id");
    assert_eq!(missing.to_string(), "lark: missing chat_id");
    assert!(matches!(missing, ApiFailure::Missing(_)));

    let mint = ApiFailure::Mint("no token service".to_string());
    assert!(mint.to_string().contains("no token service"));

    let api_error = ApiFailure::Api(ApiError::Transport { op: "send" });
    assert!(api_error.to_string().contains("transport failed"));

    let engine = ApiFailure::Engine(crate::engine::resolvers::EngineError::infra("boom"));
    assert!(ApiFailure::into_engine(engine).to_string().contains("boom"));
}

/// `FallbackError` → `ApiFailure` 保留**原始**失败（回落详情不进用户可见面）。
#[test]
fn a_fallback_error_degrades_to_the_original_api_error() {
    let error = FallbackError::Send {
        op: "send notice card",
        original: ApiError::Transport { op: "send" },
        fallback: None,
    };
    match ApiFailure::from(error) {
        ApiFailure::Api(original) => {
            assert_eq!(original.class(), crate::lark::client::ErrorClass::Transport);
        }
        other => panic!("expected Api, got {other:?}"),
    }
}

/// 文案常量**逐字**（它们就是产品面；改动会被这里挡住）。
#[test]
fn the_user_visible_copies_are_pinned_verbatim() {
    assert!(AGENT_OFFLINE_COPY.starts_with("Agent 当前离线"));
    assert!(AGENT_ARCHIVED_COPY.starts_with("这个 Agent 已被归档"));
    assert!(FRESH_PENDING_COPY.starts_with("✅ 已准备从空上下文运行"));
    assert!(CHAT_STARTED_COPY.starts_with("✅ 已新建 Multica 对话"));
    assert!(ISSUE_USAGE_COPY.contains("/issue <标题>"));
    assert!(ISSUE_USAGE_WITH_MEDIA_COPY.contains("图片或视频"));
    assert!(BINDING_PROMPT_UNAVAILABLE_COPY.contains("绑定卡片未能发送到你的私聊"));
}

mod text;
