//! [`super::super`] 的 `Channel` 四个方法 + 工厂 + `union_id` / `region` 的入站可见性。

use std::sync::{Arc, Mutex};

use mc_core::channel::message::{ChatType, OutboundMessage};
use mc_core::channel::ChannelKind;
use serde_json::json;

use super::super::{
    factory, Decrypter, FeishuChannel, FeishuChannelDeps, LarkInboundMessage, LarkInstallConfig,
};
use super::fixtures::*;
use crate::capability::Capability;
use crate::channel::{Channel, ChannelError};
use crate::lark::client::{ApiClient, ApiError, StubApiClient};
use crate::lark::resolvers::TYPE_LARK;
use crate::lark::types::{OpenId, Region};
use crate::lark::ws_connector::{EventConnector, SessionOutcome};
use crate::lark::ws_frame_decoder::{LarkEventMention, LarkSenderId};
use crate::message::SharedInboundHandler;

// =====================================================================
// 四、`Channel` 的四个方法
// =====================================================================

fn channel(connector: Arc<dyn EventConnector>, handler: SharedInboundHandler) -> FeishuChannel {
    FeishuChannel::new(
        installation(Some("on_bot")),
        connector,
        Some(handler),
        Arc::new(StubApiClient::new()),
        Decrypter::login_plaintext(),
    )
}

/// `kind` 与 `capabilities` 逐位对齐上游 `feishuChannel.Capabilities`。
#[test]
fn capabilities_declare_the_seven_upstream_bits() {
    let (_recorder, shared) = handler(false);
    let channel = channel(connector(Vec::new(), None), shared);
    assert_eq!(channel.kind(), ChannelKind::Lark);
    let bits = channel.capabilities();
    for (want, name) in [
        (Capability::TEXT, "text"),
        (Capability::RICH_CARD, "rich_card"),
        (Capability::THREAD_REPLY, "thread_reply"),
        (Capability::QUOTE_REPLY, "quote_reply"),
        (Capability::ATTACHMENT, "attachment"),
        (Capability::TYPING_INDICATOR, "typing_indicator"),
        (Capability::MESSAGE_EDIT, "message_edit"),
    ] {
        assert!(bits.has(want), "缺少能力位 {name}");
    }
    assert!(!bits.has(Capability::VOICE), "上游没有声明语音位");
}

/// `connect` 跑一条会话并把事件投递给 engine；对端**正常**关闭 ⇒ `Ok(())`。
#[tokio::test]
async fn connect_runs_one_session_and_delivers_events() {
    let (recorder, shared) = handler(false);
    let scripted = connector(
        vec![event("om-1", "text", r#"{"text":"hi"}"#, ChatType::P2p)],
        Some(SessionOutcome::Closed),
    );
    let channel = channel(Arc::clone(&scripted) as Arc<dyn EventConnector>, shared);

    channel.connect().await.expect("正常关闭应当返回 Ok");
    assert_eq!(*scripted.runs.lock().expect("lock"), 1);
    assert_eq!(recorder.messages.lock().expect("lock").len(), 1);
    // 凭据面：连接器拿到的是**解密后**的凭据（region 从安装行透传）。
    assert_eq!(
        scripted.credentials_seen.lock().expect("lock").as_slice(),
        [Region::Feishu]
    );
}

/// 停机信号置位 ⇒ `Ok(SessionOutcome::Cancelled)` ⇒ `connect` 返回 `Ok(())`（**不是**错误）。
#[tokio::test]
async fn cancellation_is_not_an_error() {
    let (_recorder, shared) = handler(false);
    let scripted = connector(Vec::new(), Some(SessionOutcome::Cancelled));
    let channel = channel(Arc::clone(&scripted) as Arc<dyn EventConnector>, shared);
    assert!(channel.connect().await.is_ok());
}

/// 连接器报错 ⇒ `connect` 报错（supervisor 按"这次尝试失败"退避重连）。
#[tokio::test]
async fn connector_failure_propagates() {
    let (_recorder, shared) = handler(false);
    let scripted = Arc::new(ScriptedConnector {
        events: Vec::new(),
        outcome: None,
        error: Some(ChannelError::Transport {
            message: "lark: ws session failed".to_string(),
        }),
        runs: Mutex::new(0),
        credentials_seen: Mutex::new(Vec::new()),
    });
    let channel = channel(Arc::clone(&scripted) as Arc<dyn EventConnector>, shared);
    let error = channel.connect().await.expect_err("应当报错");
    assert_eq!(error.code(), "channel_transport_error");
    assert!(error.is_retryable());
}

/// 没有入站入口 ⇒ 拒（**不**静默地跑一条什么都不投递的会话）。
#[tokio::test]
async fn connect_without_a_handler_is_refused() {
    let channel = FeishuChannel::new(
        installation(None),
        connector(Vec::new(), None),
        None,
        Arc::new(StubApiClient::new()),
        Decrypter::login_plaintext(),
    );
    let error = channel.connect().await.expect_err("应当拒");
    assert_eq!(error.code(), "channel_invalid_config");
}

/// `instantiation has no app_id` ⇒ 拒。
#[tokio::test]
async fn connect_without_an_app_id_is_refused() {
    let (_recorder, shared) = handler(false);
    let mut installed = installation(None);
    installed.app_id = String::new();
    let channel = FeishuChannel::new(
        installed,
        connector(Vec::new(), None),
        Some(shared),
        Arc::new(StubApiClient::new()),
        Decrypter::login_plaintext(),
    );
    let error = channel.connect().await.expect_err("应当拒");
    assert_eq!(error.code(), "channel_invalid_config");
    assert!(error.to_string().contains("no app_id"), "{error}");
}

/// 解密器失败 ⇒ 拒装配（**不外泄**明文/密文）。
#[tokio::test]
async fn connect_with_an_unwired_decrypter_is_refused() {
    let (_recorder, shared) = handler(false);
    let channel = FeishuChannel::new(
        installation(None),
        connector(Vec::new(), None),
        Some(shared),
        Arc::new(StubApiClient::new()),
        Decrypter::fail_closed(),
    );
    let error = channel.connect().await.expect_err("应当拒");
    assert_eq!(error.code(), "channel_invalid_config");
    let rendered = error.to_string();
    assert!(!rendered.contains("plain-secret"), "回显了密文：{rendered}");
    assert!(!rendered.contains("not-wired") || rendered.contains("app_secret unavailable"));
}

/// `send`：委托给客户端，引用坐标映射成 `ReplyTarget`（带线程标志）。
#[tokio::test]
async fn send_delegates_to_the_client_with_a_reply_target() {
    let api = Arc::new(RecordingApi::default());
    let channel = FeishuChannel::new(
        installation(None),
        connector(Vec::new(), None),
        Some(handler(false).1),
        Arc::clone(&api) as Arc<dyn ApiClient>,
        Decrypter::login_plaintext(),
    );
    let result = channel
        .send(OutboundMessage {
            chat_id: "oc-1".to_string(),
            text: "hi".to_string(),
            thread_id: "omt-1".to_string(),
            reply_to: "om-parent".to_string(),
        })
        .await
        .expect("应当发出");
    assert_eq!(result.message_id, "om-sent");

    let sent = api.sent.lock().expect("lock");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].chat_id.as_str(), "oc-1");
    assert_eq!(sent[0].text, "hi");
    assert_eq!(sent[0].reply_target.message_id, "om-parent");
    assert!(sent[0].reply_target.in_thread, "线程标志从 thread_id 来");
    assert_eq!(sent[0].credentials.app_id, "cli_test");
}

/// `send` 没有引用坐标 ⇒ 会话层发送（`ReplyTarget` 是零值）。
#[tokio::test]
async fn send_without_a_reply_coordinate_goes_to_the_chat() {
    let api = Arc::new(RecordingApi::default());
    let channel = FeishuChannel::new(
        installation(None),
        connector(Vec::new(), None),
        Some(handler(false).1),
        Arc::clone(&api) as Arc<dyn ApiClient>,
        Decrypter::login_plaintext(),
    );
    channel
        .send(OutboundMessage {
            chat_id: "oc-1".to_string(),
            text: "hi".to_string(),
            thread_id: String::new(),
            reply_to: String::new(),
        })
        .await
        .expect("应当发出");
    let sent = api.sent.lock().expect("lock");
    assert!(!sent[0].reply_target.is_set());
}

/// `send` 的传输失败映射成渠道错误（凭据失效 ⇒ `Auth`）。
#[tokio::test]
async fn send_maps_credential_failures_to_auth() {
    let api = Arc::new(RecordingApi {
        sent: Mutex::new(Vec::new()),
        error: Some(ApiError::Refused {
            op: "send_text_message",
            status: None,
            code: 99_991_663,
        }),
    });
    let channel = FeishuChannel::new(
        installation(None),
        connector(Vec::new(), None),
        Some(handler(false).1),
        Arc::clone(&api) as Arc<dyn ApiClient>,
        Decrypter::login_plaintext(),
    );
    let error = channel
        .send(OutboundMessage {
            chat_id: "oc-1".to_string(),
            text: "hi".to_string(),
            thread_id: String::new(),
            reply_to: String::new(),
        })
        .await
        .expect_err("应当报错");
    assert_eq!(error.code(), "channel_auth_error");
}

/// `disconnect` 幂等且安全：没有在飞的一代 ⇒ 直接 `Ok`。
#[tokio::test]
async fn disconnect_is_idempotent_and_safe() {
    let (_recorder, shared) = handler(false);
    let channel = channel(connector(Vec::new(), None), shared);
    assert!(channel.disconnect().await.is_ok());
    assert!(channel.disconnect().await.is_ok());
}

/// `FeishuChannel` 的 `Debug` 全脱敏（密文只报长度）。
#[test]
fn channel_debug_redacts_the_ciphertext() {
    let (_recorder, shared) = handler(false);
    let channel = channel(connector(Vec::new(), None), shared);
    let rendered = format!("{channel:?}");
    assert!(!rendered.contains("plain-secret"), "{rendered}");
    assert!(rendered.contains("len"), "{rendered}");
    assert!(rendered.contains("<dyn EventConnector>"), "{rendered}");
}

// =====================================================================
// 五、工厂的**拒装配**四条（外加一条正路）
// =====================================================================

fn good_config() -> serde_json::Value {
    json!({
        "app_id": "cli_test",
        "app_secret_encrypted": "cGxhaW4tc2VjcmV0",
        "tenant_key": "tenant-1",
        "bot_open_id": "ou_bot",
        "bot_union_id": "on_bot",
        "region": "lark"
    })
}

fn deps() -> FeishuChannelDeps {
    FeishuChannelDeps::new(connector(Vec::new(), None), Decrypter::login_plaintext())
}

/// 正路：配置合法 ⇒ 交出一条 `Lark` 的 channel，region 从 config blob 透传。
#[test]
fn factory_builds_a_lark_channel_from_a_valid_config() {
    let built = factory(deps())(factory_config(good_config())).expect("应当装配成功");
    assert_eq!(built.kind(), ChannelKind::Lark);
    let debug = format!("{:?}", built.kind());
    assert_eq!(debug, "Lark");
}

/// 拒装配四条：配置形状错 / 缺 `app_id` / 缺密文 / 坏 base64 / 缺连接器。
#[test]
fn factory_refuses_every_incomplete_configuration() {
    // ① 配置形状错（别的平台的 blob）。
    let error = factory(deps())(factory_config(json!({"not": "lark"})))
        .err()
        .expect("应当拒");
    assert_eq!(error.code(), "channel_invalid_config");

    // ② 缺 `app_id`。
    let mut raw = good_config();
    raw["app_id"] = json!("");
    let error = factory(deps())(factory_config(raw)).err().expect("应当拒");
    assert!(error.to_string().contains("no app_id"), "{error}");

    // ③ 缺密文列。
    let mut raw = good_config();
    raw["app_secret_encrypted"] = json!("");
    let error = factory(deps())(factory_config(raw)).err().expect("应当拒");
    assert!(
        error.to_string().contains("no app_secret_encrypted"),
        "{error}"
    );

    // ④ 密文不是合法 base64 ⇒ 只报长度（不回显内容）。
    let mut raw = good_config();
    raw["app_secret_encrypted"] = json!("not base64 !!!");
    let error = factory(deps())(factory_config(raw)).err().expect("应当拒");
    let rendered = error.to_string();
    assert!(rendered.contains("not valid base64"), "{rendered}");
    assert!(!rendered.contains("base64 !!!"), "回显了密文：{rendered}");

    // ⑤ 连接器缺失（"接线未完成"要响亮）。
    let mut no_connector =
        FeishuChannelDeps::new(connector(Vec::new(), None), Decrypter::login_plaintext());
    no_connector.connector = None;
    let error = factory(no_connector)(factory_config(good_config()))
        .err()
        .expect("应当拒");
    assert!(
        error.to_string().contains("ws connector is not wired"),
        "{error}"
    );
}

/// 工厂产出的 channel 可以 `connect`：配置里的 region / bot 标识真的进了装配。
#[tokio::test]
async fn factory_built_channel_connects() {
    let built = factory(deps())(factory_config(good_config())).expect("应当装配成功");
    // 工厂路径没注入 handler ⇒ `connect` 拒（**不**静默跑空会话）。
    let error = built.connect().await.expect_err("没有 handler 应当拒");
    assert_eq!(error.code(), "channel_invalid_config");
}

/// `FeishuChannelDeps` 的 `Debug` 手写脱敏。
#[test]
fn deps_debug_is_redacted() {
    let rendered = format!("{:?}", deps());
    assert!(rendered.contains("has_connector: true"), "{rendered}");
    assert!(rendered.contains("<dyn ApiClient>"), "{rendered}");
    assert!(rendered.contains("plaintext(test)"), "{rendered}");
}

// =====================================================================
// 六、union_id / region 在**入站路径**上的可观测面（不依赖真实回填）
// =====================================================================

/// `union_id` 的回填状态在入站路径上**两处**可观测（`content_flatten` 的判据之外）：
///
/// 1. `addressed_to_bot` 的判据取 `bot_union_id`（已知）/ `bot_open_id`（回填之前）；
/// 2. 归一化信封的 `source.sender_stable_id` 就是**发送者**的 `union_id`
///    —— 绑定与去重之外，跨安装的身份归并也只有它可用。
#[test]
fn union_id_state_is_observable_on_the_inbound_path() {
    // 回填之后：只按 union_id 判（open_id 撞上也不认）。
    let installed = installation(Some("on_bot"));
    let mut raw = event(
        "om-1",
        "text",
        r#"{"text":"@_user_1 总结一下"}"#,
        ChatType::Group,
    );
    raw.mentions = vec![bot_mention()];
    let normalized = LarkInboundMessage::from_event(raw, &installed);
    assert!(normalized.addressed_to_bot);
    assert_eq!(normalized.body, "总结一下");

    // 回填之前：`bot_union_id` 缺席 ⇒ 按 open_id 回落，安装**仍然可用**。
    let mut legacy = installation(None);
    legacy.bot_open_id = OpenId::new("ou_bot");
    let mut raw = event(
        "om-2",
        "text",
        r#"{"text":"@_user_1 总结一下"}"#,
        ChatType::Group,
    );
    raw.mentions = vec![LarkEventMention {
        key: "@_user_1".to_string(),
        id: LarkSenderId {
            open_id: "ou_bot".to_string(),
            union_id: "on_unknown".to_string(),
            user_id: String::new(),
        },
        name: "Bot".to_string(),
    }];
    let normalized = LarkInboundMessage::from_event(raw, &legacy);
    assert!(normalized.addressed_to_bot, "回填之前仍要可用");
    assert_eq!(normalized.body, "总结一下");

    // 发送者的 `union_id` 逐字进稳定身份那一格（跨安装归并的输入）。
    let normalized = LarkInboundMessage::from_event(
        event("om-3", "text", r#"{"text":"hi"}"#, ChatType::P2p),
        &installation(None),
    );
    assert_eq!(
        normalized
            .to_inbound_message()
            .expect("编码")
            .source
            .sender_stable_id,
        "on_bob"
    );
}

/// `region` 的回填状态在入站路径上的可观测面 = **交给连接器的凭据**：
/// `region` 从安装行（`lark_installation.region`）经解密器一路进
/// [`InstallationCredentials`]，由连接器观测 —— 因此"这条安装走哪个云"可以
/// **不依赖 `region_backfill.go`**（M7-14 的写集）就钉住。
#[tokio::test]
async fn region_state_is_observable_through_the_connector_credentials() {
    for (raw_region, expected) in [
        ("feishu", Region::Feishu),
        ("lark", Region::Lark),
        ("", Region::Feishu),
    ] {
        let mut installed = installation(None);
        installed.region = Region::or_default(raw_region);
        let (_recorder, shared) = handler(false);
        let scripted = connector(Vec::new(), Some(SessionOutcome::Cancelled));
        let channel = FeishuChannel::new(
            installed,
            Arc::clone(&scripted) as Arc<dyn EventConnector>,
            Some(shared),
            Arc::new(StubApiClient::new()),
            Decrypter::login_plaintext(),
        );
        channel.connect().await.expect("取消不是错误");
        assert_eq!(
            scripted.credentials_seen.lock().expect("lock").as_slice(),
            [expected],
            "region={raw_region:?}"
        );
    }
}

/// `credentials` 子模块的 `Debug` 出口在本文件也成立（装配袋里的解密器）。
#[test]
fn credentials_decrypter_label_is_visible_for_ops() {
    assert!(format!("{:?}", Decrypter::login_plaintext()).contains("plaintext(test)"));
    assert!(format!("{:?}", Decrypter::fail_closed()).contains("fail-closed"));
    assert!(format!("{:?}", LarkInstallConfig::default()).contains("<empty>"));
    let _ = TYPE_LARK;
}
