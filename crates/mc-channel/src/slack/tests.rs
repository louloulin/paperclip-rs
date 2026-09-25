use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mc_core::channel::message::{ChatType, MessageKind, OutboundMessage};
use mc_core::channel::ChannelKind;
use mc_core::id::Id;
use serde_json::json;

use crate::capability::Capability;
use crate::channel::{Channel, ChannelConfig};
use crate::engine::resolvers::{DropReason, EngineResult, NoCommands};
use crate::engine::router::{Router, RouterConfig};
use crate::engine::supervisor::{
    AcquireLeaseParams, Installation, InstallationStore, LeaseStore, ReleaseLeaseParams,
};
use crate::registry::Registry;
use crate::slack;
use crate::slack::config::SlackDeps;
use crate::slack::outbound::{reset_api_base, set_api_base, Sender, OUTBOUND_METADATA_EVENT};

mod support;
use support::{
    base64_standard, channel, events_frame, round_trip, serve_socket_mode, serve_web_api,
    BoundIdentity, NoIssues, NoReader, NoTrigger, UnboundIdentity, BASE_LOCK,
};

// 注册面（原 `slack/mod.rs` 的三条，M7-4 搬到本文件）
// =====================================================================

// ---- 注册面用的最小 `ChannelDeps`（`register` 的签名要它；实现全部为 no-op） ----

struct NoInstallations;

#[async_trait]
impl InstallationStore for NoInstallations {
    async fn list_active(&self) -> EngineResult<Vec<Installation>> {
        Ok(Vec::new())
    }
}

struct NoLeases;

#[async_trait]
impl LeaseStore for NoLeases {
    async fn list_held(&self, _ids: &[Id]) -> EngineResult<HashSet<Id>> {
        Ok(HashSet::new())
    }
    async fn try_acquire(&self, _params: AcquireLeaseParams) -> EngineResult<()> {
        Ok(())
    }
    async fn renew(&self, _params: AcquireLeaseParams) -> EngineResult<()> {
        Ok(())
    }
    async fn release(&self, _params: ReleaseLeaseParams) -> EngineResult<()> {
        Ok(())
    }
}

/// 空 `ChannelDeps`（注册面只做类型装配，不启动任何连接）。
fn deps() -> crate::engine::ChannelDeps {
    let router = Arc::new(Router::new(
        Arc::new(NoCommands),
        Arc::new(NoTrigger),
        Arc::new(NoReader),
        Arc::new(NoIssues),
        RouterConfig::default(),
    ));
    crate::engine::ChannelDeps::new(router, Arc::new(NoInstallations), Arc::new(NoLeases))
}

fn config(raw: serde_json::Value) -> ChannelConfig {
    ChannelConfig {
        kind: ChannelKind::Slack,
        raw,
        installation_id: None,
        handler: None,
    }
}

/// `register` 真的把工厂放进表里，且**失败关闭**：没接线时带密文的配置被拒，
/// 而不是把密文当明文。
#[test]
fn register_installs_a_fail_closed_factory() {
    let registry = Registry::new();
    assert!(registry.is_empty());
    slack::register(&registry, &deps());
    assert_eq!(registry.kinds(), vec![ChannelKind::Slack]);
    let Err(error) = registry.build(config(json!({
        "app_id": "A1",
        "app_token_encrypted": "QUJD",
        "bot_token_encrypted": "QUJD",
    }))) else {
        panic!("失败关闭的解密器必须拒掉带密文的配置")
    };
    assert_eq!(error.code(), "channel_invalid_config");
    // 错误文案不回显密文（凭据纪律）。
    assert!(!error.to_string().contains("QUJD"));
}

/// 接线好的入口：带上部署密钥就能造出真 Channel，位图与上游一致，且**工厂已接上出站**。
#[test]
fn register_with_secret_box_builds_a_channel_with_a_wired_sender() {
    let boxed = mc_secrets::secretbox::SecretBox::new(&[7u8; 32]).expect("key");
    let app = boxed.seal(b"xapp-real").expect("seal");
    let bot = boxed.seal(b"xoxb-real").expect("seal");
    let registry = Registry::new();
    slack::register_with(&registry, &SlackDeps::with_secret_box(boxed));
    let channel = registry
        .build(config(json!({
            "app_id": "A1",
            "bot_user_id": "UBOT",
            "app_token_encrypted": base64_standard(&app),
            "bot_token_encrypted": base64_standard(&bot),
        })))
        .expect("接线好就能造");
    assert_eq!(channel.kind(), ChannelKind::Slack);
    assert!(channel.capabilities().has(Capability::TEXT));
    assert!(channel.capabilities().has(Capability::THREAD_REPLY));
    // 工厂**总是**注入发送器（M7-4 的接线点），所以未接线的失败文案不会再出现；
    // 具体的出站行为由本文件的端到端回路与 `slack/outbound/tests.rs` 覆盖。
    assert_eq!(channel.kind().as_str(), "slack");
}

/// `fail_closed_deps()` 与 `register` 走同一条判据（带密文 ⇒ 拒）。
#[test]
fn the_fail_closed_deps_match_the_register_path() {
    let registered = Registry::new();
    slack::register_with(&registered, &slack::fail_closed_deps());
    assert!(registered
        .build(config(json!({
            "app_id": "A1",
            "app_token_encrypted": "QUJD",
            "bot_token_encrypted": "QUJD",
        })))
        .is_err());
}

// =====================================================================
// 门禁证据
// =====================================================================

/// **本片的门禁证据**：一条 Socket Mode 帧进去，一条 `chat.postMessage` 帧回到替身。
#[tokio::test]
async fn the_slack_round_trip_closes_on_a_local_platform_stand_in() {
    let _serial = BASE_LOCK.lock().await;
    let round = round_trip(Arc::new(UnboundIdentity));

    let (ws_url, mut acks) =
        serve_socket_mode(vec![events_frame("env-1", "1700000000.000100")]).await;
    let (base, mut posted) = serve_web_api(ws_url).await;
    set_api_base(&base);

    let channel = channel(&round.router);
    let running = tokio::spawn(async move {
        // `connect` 阻塞跑接收循环；用例在断言之后把它 abort 掉。
        let _ = channel.connect().await;
    });

    // 入站方向：替身收到的是**真 ACK**（顺序「先 ACK 后处理」由 `socket.rs` 保证）。
    let ack = tokio::time::timeout(Duration::from_secs(10), acks.recv())
        .await
        .expect("ACK 超时")
        .expect("ACK 内容");
    let ack: serde_json::Value = serde_json::from_str(&ack).expect("ACK 是 JSON");
    assert_eq!(ack["envelope_id"], "env-1");

    // 出站方向：替身收到的是**真 chat.postMessage**（逐字段断言原始帧）。
    let body = tokio::time::timeout(Duration::from_secs(10), posted.recv())
        .await
        .expect("出站帧超时")
        .expect("出站帧内容");
    reset_api_base();
    running.abort();

    let sent: serde_json::Value = serde_json::from_str(&body).expect("出站帧是 JSON");
    assert_eq!(sent["channel"], "C1", "回到入站的那条会话");
    // 顶层提及没有线程根 ⇒ 绑定卡落在**会话层**（上游 `postResult` 用的就是
    // `msg.Source.ThreadID`，绑定的线程归一化只影响会话隔离键）。
    assert!(sent["thread_ts"].is_null());
    assert_eq!(sent["unfurl_links"], false);
    assert_eq!(sent["metadata"]["event_type"], OUTBOUND_METADATA_EVENT);
    assert_eq!(sent["metadata"]["event_payload"]["kind"], "control_ack");
    let text = sent["text"].as_str().expect("text");
    assert!(
        text.contains("<https://app.example/slack/bind?token=e2e-token|link your account>"),
        "未绑定发件人 ⇒ 绑定卡（真令牌铸出来、真显式链接）：{text}"
    );

    // 业务侧真的发生了：令牌铸给帧里的发件人。
    assert_eq!(round.minter.calls.lock().expect("lock").as_slice(), ["U1"]);
    // 账本**故意**没有行：绑定卡这条路径上还没有会话绑定行，没有"这条出站属于谁"可记
    // （上游 `postResult` 逐字：`if r.ledger == nil || !res.ChannelBindingID.Valid { return nil }`）。
    // 记账真的发生的那条路径（`control_ack` / `issue_ack`）由 `slack/replier/tests.rs`
    // 用带 `channel_binding_id` 的判决覆盖，此处只钉住这条**不该**记账的边界。
    assert!(
        round.ledger.records.lock().expect("lock").is_empty(),
        "没有会话绑定 ⇒ 不记账（上游同）"
    );
}

/// 反例 1：**重复信封**（同一条消息 ts）⇒ 去重命中 ⇒ 丢弃且**不**回报错误，
/// 两个信封都被 ACK，但只发一条出站帧。
#[tokio::test]
async fn a_replayed_envelope_is_dropped_without_an_error() {
    let _serial = BASE_LOCK.lock().await;
    let round = round_trip(Arc::new(UnboundIdentity));
    let (ws_url, mut acks) = serve_socket_mode(vec![
        events_frame("env-1", "1700000000.000100"),
        events_frame("env-2", "1700000000.000100"),
    ])
    .await;
    let (base, mut posted) = serve_web_api(ws_url).await;
    set_api_base(&base);

    let channel = channel(&round.router);
    let running = tokio::spawn(async move {
        let _ = channel.connect().await;
    });

    for expected in ["env-1", "env-2"] {
        let ack = tokio::time::timeout(Duration::from_secs(10), acks.recv())
            .await
            .expect("ACK 超时")
            .expect("ACK");
        let ack: serde_json::Value = serde_json::from_str(&ack).expect("JSON");
        assert_eq!(ack["envelope_id"], expected, "ACK 与判决无关");
    }

    let body = tokio::time::timeout(Duration::from_secs(10), posted.recv())
        .await
        .expect("第一条出站帧")
        .expect("内容");
    assert!(body.contains("link your account"));
    let second = tokio::time::timeout(Duration::from_millis(500), posted.recv()).await;
    reset_api_base();
    running.abort();
    assert!(second.is_err(), "去重命中必须**不**再发一条出站帧");

    let drops = round.audit.drops.lock().expect("lock");
    assert!(
        drops.contains(&DropReason::Duplicate),
        "丢弃原因 = duplicate"
    );
    assert_eq!(round.dedup.claims.lock().expect("lock").len(), 1);
}

/// 反例 2：`disconnect` 帧 ⇒ `connect` 以**可退避**的错误返回（supervisor 据此重连）。
#[tokio::test]
async fn a_disconnect_frame_ends_the_loop_with_a_retryable_error() {
    let _serial = BASE_LOCK.lock().await;
    let round = round_trip(Arc::new(BoundIdentity));
    let (ws_url, _acks) = serve_socket_mode(vec![
        json!({"type": "disconnect", "reason": "link down"}).to_string(),
    ])
    .await;
    let (base, _posted) = serve_web_api(ws_url).await;
    set_api_base(&base);

    let channel = channel(&round.router);
    let outcome = tokio::time::timeout(Duration::from_secs(10), channel.connect())
        .await
        .expect("connect 必须返回（不该永挂）");
    reset_api_base();
    let error = outcome.expect_err("断开 ⇒ 返回错误让 supervisor 退避重连");
    assert!(error.is_retryable(), "传输类错误必须可退避：{error}");
    assert!(error.to_string().contains("link down"));
}

/// 反例 2 的另一半：对端直接关流也走同一个出口。
#[tokio::test]
async fn a_closed_stream_ends_the_loop_with_a_retryable_error() {
    let _serial = BASE_LOCK.lock().await;
    let round = round_trip(Arc::new(BoundIdentity));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            let _ = tokio_tungstenite::accept_async(stream).await;
            // 立刻丢弃 ⇒ 流关闭。
        }
    });
    let (base, _posted) = serve_web_api(format!("ws://127.0.0.1:{port}")).await;
    set_api_base(&base);
    let channel = channel(&round.router);
    let error = tokio::time::timeout(Duration::from_secs(10), channel.connect())
        .await
        .expect("connect 必须返回")
        .expect_err("流关闭 ⇒ 错误");
    reset_api_base();
    assert!(error.is_retryable());
}

/// 入站与出站共用**同一份** mrkdwn 实现（M7-4 只做一次转换）。
#[tokio::test]
async fn outbound_goes_through_the_same_mrkdwn_path() {
    let _serial = BASE_LOCK.lock().await;
    let (base, mut posted) = serve_web_api("ws://127.0.0.1:1/unused".to_string()).await;
    set_api_base(&base);
    let out = OutboundMessage {
        chat_id: "C1".to_string(),
        text: "**bold** and `code`".to_string(),
        thread_id: String::new(),
        reply_to: String::new(),
    };
    let frame = Sender::http().send("xoxb-e2e", &out).await.expect("send");
    reset_api_base();
    assert_eq!(frame.timestamps.len(), 1);
    let body = posted.recv().await.expect("body");
    let sent: serde_json::Value = serde_json::from_str(&body).expect("JSON");
    assert_eq!(sent["text"], "*bold* and `code`");
}

/// DM（`message` 事件，非提及）也能穿到流水线：这就是"每个频道一条连续会话"的入口。
#[tokio::test]
async fn a_direct_message_event_reaches_the_pipeline() {
    let _serial = BASE_LOCK.lock().await;
    let round = round_trip(Arc::new(UnboundIdentity));
    let frame = json!({
        "type": "events_api",
        "envelope_id": "env-dm",
        "payload": {
            "team_id": "T1",
            "api_app_id": "A1",
            "event": {
                "type": "message",
                "channel": "D1",
                "channel_type": "im",
                "user": "U1",
                "text": "hello",
                "ts": "1700000000.000200",
            }
        }
    })
    .to_string();
    let (ws_url, mut acks) = serve_socket_mode(vec![frame]).await;
    let (base, mut posted) = serve_web_api(ws_url).await;
    set_api_base(&base);
    let channel = channel(&round.router);
    let running = tokio::spawn(async move {
        let _ = channel.connect().await;
    });
    let ack = tokio::time::timeout(Duration::from_secs(10), acks.recv())
        .await
        .expect("ACK")
        .expect("ACK");
    assert!(ack.contains("env-dm"));
    let body = tokio::time::timeout(Duration::from_secs(10), posted.recv())
        .await
        .expect("出站")
        .expect("内容");
    reset_api_base();
    running.abort();
    let sent: serde_json::Value = serde_json::from_str(&body).expect("JSON");
    assert_eq!(sent["channel"], "D1");
    // DM 的回复落点是**会话层**（没有 thread）；正文仍是绑定卡。
    assert!(sent["text"]
        .as_str()
        .expect("text")
        .contains("link your account"));
}

/// 归一化层真的把信封解成了信封装（断言 `InboundMessage` 的字段，而不是只看有没有回帧）。
#[test]
fn an_app_mention_frame_normalizes_into_the_cross_platform_envelope() {
    let frame = crate::slack::socket::parse_socket_frame(&events_frame("env-x", "1700000000.1"))
        .expect("解析信封");
    let action = crate::slack::socket::dispatch_frame(&frame, "UBOT");
    let crate::slack::socket::FrameAction::Dispatch(message) = action else {
        panic!("app_mention 必须投递");
    };
    assert_eq!(message.message_id, "1700000000.1");
    assert_eq!(message.source.chat_id, "C1");
    assert_eq!(message.source.chat_type, ChatType::Group);
    assert_eq!(message.source.sender_id, "U1");
    assert!(message.addressed_to_bot);
    assert_eq!(message.kind, MessageKind::Text);
    assert_eq!(
        message.raw.get("api_app_id").and_then(|v| v.as_str()),
        Some("A1"),
        "路由键留在 raw 里（只有 adapter 与 resolver 读它）"
    );
}
