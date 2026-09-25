//! `wecom_channel` 的**装配面**用例：注册、工厂校验、凭据脱敏、`Channel` 的五个方法。
//!
//! 端到端收发回路在 [`round_trip`]；本文件只管"接上 / 接不上"这一层。

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use mc_core::channel::message::OutboundMessage;
use mc_core::channel::ChannelKind;
use mc_core::id::Id;
use mc_secrets::secretbox::SecretBox;
use serde_json::json;

use super::{
    factory, fail_closed_factory, kind, origin_type, register, register_with, NoDialer,
    WeComChannel, WeComDeps, DEFAULT_WS_URL, SEND_NOT_SUPPORTED, SUBSCRIBE_TIMEOUT,
};
use crate::capability::Capability;
use crate::channel::{Channel, ChannelConfig};
use crate::engine::resolvers::{
    ChannelIssueOutcome, ChannelIssueParams, ChatRunParams, IssueCreator, RunTriggerer,
    SessionReader,
};
use crate::engine::{
    AcquireLeaseParams, ChannelDeps, EngineResult, Installation as SupervisedInstallation,
    InstallationStore, LeaseStore, ReleaseLeaseParams, Router, RouterConfig, NO_RESOLVER_SET,
};
use crate::registry::Registry;
use crate::wecom::credentials::{PlaintextSecret, SecretboxCredentialsResolver};
use crate::wecom::types::{encode_ciphertext, CHANNEL_TYPE, KIND};

mod doubles;
mod harness;
mod round_trip;

use harness::RecordingHandler;

/// 固定部署密钥（与 `mc-secrets` / M7-15 用例里的同一把：`0x00..0x1f`）。
const KEY_BYTES: [u8; 32] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
    26, 27, 28, 29, 30, 31,
];

/// 一枚封好的长连接密钥 + 一条 config blob。
fn sealed_config(bot_id: &str, secret: &str) -> (serde_json::Value, SecretBox) {
    let key = SecretBox::new(&KEY_BYTES).expect("secretbox");
    let sealed = key.seal(secret.as_bytes()).expect("seal");
    (
        json!({
            "app_id": bot_id,
            "bot_id": bot_id,
            "secret_encrypted": encode_ciphertext(&sealed),
            "bot_display_name": "Multica Bot",
        }),
        key,
    )
}

/// 一个接好线的装配袋 + 它的 config blob（密钥是 `secret`）。
fn deps(bot_id: &str, secret: &str) -> (Arc<WeComDeps>, serde_json::Value) {
    let (config, key) = sealed_config(bot_id, secret);
    let deps = Arc::new(
        WeComDeps::new(Arc::new(SecretboxCredentialsResolver::new(key)))
            .with_ws_url("wss://fake.example/aibot"),
    );
    (deps, config)
}

/// `Registry::build` 的 `Ok` 侧是 `Arc<dyn Channel>`（**没有** `Debug`）⇒ 用一个显式的
/// `match` 取代 `expect`（`expect` 要求 `Ok: Debug`）。
fn build(
    registry: &Registry,
    config: ChannelConfig,
) -> Result<Arc<dyn Channel>, crate::channel::ChannelError> {
    registry.build(config)
}

fn config(raw: serde_json::Value, handler: bool) -> ChannelConfig {
    ChannelConfig {
        kind: KIND,
        raw,
        installation_id: Some(Id::new()),
        handler: handler.then(|| harness::shared(Arc::new(RecordingHandler::default()))),
    }
}

/// engine 的端口袋 —— 只为 `register()` 那条**失败关闭**的路径服务，所以三条端口都是空替身。
fn engine_deps() -> ChannelDeps {
    struct EmptyInstalls;
    #[async_trait]
    impl InstallationStore for EmptyInstalls {
        async fn list_active(&self) -> EngineResult<Vec<SupervisedInstallation>> {
            Ok(Vec::new())
        }
    }
    struct EmptyLeases;
    #[async_trait]
    impl LeaseStore for EmptyLeases {
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
    struct NoTrigger;
    #[async_trait]
    impl RunTriggerer for NoTrigger {
        async fn schedule_chat_run(&self, _params: ChatRunParams) -> EngineResult<()> {
            Ok(())
        }
        async fn drain(&self) -> EngineResult<()> {
            Ok(())
        }
    }
    struct NoReader;
    #[async_trait]
    impl SessionReader for NoReader {
        async fn workspace_identity(
            &self,
            _workspace_id: Id,
        ) -> EngineResult<crate::engine::WorkspaceIdentity> {
            Ok(crate::engine::WorkspaceIdentity::default())
        }
    }
    struct NoIssues;
    #[async_trait]
    impl IssueCreator for NoIssues {
        async fn create_issue(
            &self,
            _params: ChannelIssueParams,
        ) -> EngineResult<ChannelIssueOutcome> {
            Ok(ChannelIssueOutcome {
                issue: crate::engine::ChannelIssue {
                    id: Id::new(),
                    number: 1,
                    title: "t".into(),
                },
                duplicate: false,
                assigned_task_id: None,
            })
        }
    }
    let router = Arc::new(Router::new(
        Arc::new(crate::engine::ChannelCommandClassifier),
        Arc::new(NoTrigger),
        Arc::new(NoReader),
        Arc::new(NoIssues),
        RouterConfig::default(),
    ));
    let _ = NO_RESOLVER_SET;
    ChannelDeps::new(router, Arc::new(EmptyInstalls), Arc::new(EmptyLeases))
}

// =====================================================================
// 注册面
// =====================================================================

/// `register`（宿主那条**没有部署密钥**的路径）是**失败关闭**的：工厂在，但造不出任何 Channel，
/// 且错误里没有半个凭据字节。
#[test]
fn register_is_fail_closed() {
    let registry = Registry::new();
    assert!(registry.is_empty());
    register(&registry, &engine_deps());
    assert_eq!(registry.kinds(), vec![ChannelKind::WeCom]);

    let Err(error) = build(&registry, config(json!({"bot_id": "bot_1"}), true)) else {
        panic!("失败关闭：这次 build 必须失败");
    };
    assert_eq!(error.code(), "channel_invalid_config");
    assert!(error.to_string().contains("fail closed"), "{error}");
    let Err(explicit) = fail_closed_factory()(config(json!({}), true)) else {
        panic!("失败关闭");
    };
    assert_eq!(explicit.code(), error.code());
}

/// `register_with` → 工厂能从一条真 config 造出 Channel（**凭据解出来了**）。
#[test]
fn register_with_builds_a_channel_out_of_a_sealed_config() {
    let (deps, raw) = deps("bot_5f1c9a", "super-secret-value");
    let registry = Registry::new();
    register_with(&registry, deps);
    let channel = build(&registry, config(raw, true)).expect("built");
    assert_eq!(channel.kind(), ChannelKind::WeCom);
    assert_eq!(
        channel.capabilities(),
        Capability::TEXT.union(Capability::ATTACHMENT),
        "上游逐字：入站附件成立，出站媒体不声称"
    );
}

/// 凭据面：`WeComChannel` 的 `Debug` 只报"有没有密钥"（明文、密文、都不到任何 `{:?}`）。
#[test]
fn debug_never_echoes_the_secret() {
    let (deps, _) = deps("bot_5f1c9a", "s");
    let mut channel = WeComChannel {
        installation_id: Some(Id::new()),
        bot_id: "bot_5f1c9a".to_string(),
        secret: PlaintextSecret::new("super-secret-value"),
        bot_display_name: "Multica Bot".to_string(),
        handler: None,
        dialer: Arc::clone(&deps.dialer),
        ws_url: "wss://fake.example/aibot".to_string(),
        senders: None,
        metrics: None,
    };
    let rendered = format!("{channel:?}");
    assert!(rendered.contains("<redacted>"), "{rendered}");
    assert!(!rendered.contains("super-secret-value"), "{rendered}");
    assert!(rendered.contains("bot_5f1c9a"), "{rendered}");
    // 密钥缺席只报 `<empty>`。
    channel.secret = PlaintextSecret::new("");
    assert!(format!("{channel:?}").contains("<empty>"));
}

/// 工厂的校验：没有 `bot_id`、config 不是对象、密文解不开 —— 每一条都是 `channel_invalid_config`，
/// 而且**错误路径不回显凭据**。
#[test]
fn the_factory_validates_the_configuration() {
    let (deps, _) = deps("bot_5f1c9a", "s");
    let built = factory(Arc::clone(&deps));

    let Err(error) = built(config(json!({"bot_display_name": "x"}), true)) else {
        panic!("缺 bot_id 必须被拒");
    };
    assert_eq!(error.code(), "channel_invalid_config");
    assert!(error.to_string().contains("missing bot_id"), "{error}");

    let Err(error) = built(config(serde_json::Value::Null, true)) else {
        panic!("没有 blob");
    };
    assert!(error.to_string().contains("no config blob"), "{error}");

    let Err(error) = built(config(json!([1, 2, 3]), true)) else {
        panic!("不是对象");
    };
    assert!(error.to_string().contains("not a JSON object"), "{error}");

    // 别的部署密钥封的密文 ⇒ 解不开。错误里既没有密文、也没有明文。
    let other_key = SecretBox::new(&[7_u8; 32]).expect("secretbox");
    let foreign = other_key.seal(b"another-secret").expect("seal");
    let Err(error) = built(config(
        json!({"bot_id": "bot_1", "secret_encrypted": encode_ciphertext(&foreign)}),
        true,
    )) else {
        panic!("解不开的密文必须被拒");
    };
    let rendered = error.to_string();
    assert!(!rendered.contains("another-secret"), "{rendered}");
    assert!(
        !rendered.contains(&encode_ciphertext(&foreign)),
        "{rendered}"
    );

    // 老行只有 `app_id`（没有 `bot_id`）：仍然认得出来（M7-15 的读侧回落）。
    let (_, key) = sealed_config("bot_legacy", "s");
    let sealed = key.seal(b"s").expect("seal");
    let channel = match built(config(
        json!({"app_id": "bot_legacy", "secret_encrypted": encode_ciphertext(&sealed)}),
        true,
    )) {
        Ok(channel) => channel,
        Err(error) => panic!("老行必须解得出来: {error}"),
    };
    assert_eq!(channel.kind(), ChannelKind::WeCom);
}

/// `Channel::send` **不支持**（上游 `ErrSendNotSupported`），而 `disconnect` 是 no-op。
#[tokio::test]
async fn send_is_unsupported_and_disconnect_is_a_noop() {
    let (deps, raw) = deps("bot_5f1c9a", "s");
    let channel = match factory(deps)(config(raw, true)) {
        Ok(channel) => channel,
        Err(error) => panic!("build: {error}"),
    };
    let error = channel
        .send(OutboundMessage {
            chat_id: "c".to_string(),
            text: "hi".to_string(),
            thread_id: String::new(),
            reply_to: String::new(),
        })
        .await
        .expect_err("not supported");
    assert_eq!(error.code(), "channel_invalid_config");
    assert!(error.to_string().contains("OutboundReplier"), "{error}");
    assert!(SEND_NOT_SUPPORTED.contains("OutboundReplier"));
    channel.disconnect().await.expect("noop");
}

/// `connect` 的本地前置在**拨号之前**拒掉（否则一次误配会变成一次对生产端点的无意义连接）。
#[tokio::test]
async fn connect_refuses_before_dialing() {
    let (deps, raw) = deps("bot_5f1c9a", "s");
    let built = factory(Arc::clone(&deps));

    // 缺 handler。
    let channel = match built(config(raw.clone(), false)) {
        Ok(channel) => channel,
        Err(error) => panic!("build: {error}"),
    };
    let error = channel.connect().await.expect_err("no handler");
    assert!(error.to_string().contains("inbound handler"), "{error}");

    // URL 形态不对。
    let (_, key) = sealed_config("bot_5f1c9a", "s");
    let bad_url = Arc::new(
        WeComDeps::new(Arc::new(SecretboxCredentialsResolver::new(key)))
            .with_ws_url("https://not-a-ws-endpoint"),
    );
    let channel = match factory(bad_url)(config(raw, true)) {
        Ok(channel) => channel,
        Err(error) => panic!("build: {error}"),
    };
    let error = channel.connect().await.expect_err("bad url");
    assert!(error.to_string().contains("ws:// or wss://"), "{error}");
}

/// `WeComDeps` 的 `Debug`：端口只报存在性、URL 报出来（它是公开端点）。
#[test]
fn deps_debug_reports_port_presence() {
    let (deps, _) = deps("bot_1", "s");
    let rendered = format!("{deps:?}");
    assert!(rendered.contains("<dyn CredentialsResolver>"), "{rendered}");
    assert!(rendered.contains("wss://fake.example/aibot"), "{rendered}");
    assert!(rendered.contains("senders: false"), "{rendered}");
    assert!(rendered.contains("metrics: false"), "{rendered}");

    let with_senders = WeComDeps::new(Arc::clone(&deps.credentials))
        .with_senders(harness::TestSenders::new())
        .with_dialer(Arc::new(NoDialer));
    assert!(format!("{with_senders:?}").contains("senders: true"));
    assert!(format!("{with_senders:?}").contains("<dyn WsDialer>"));
}

/// 常量面：判别式 / `channel_type` / origin / 端点 / 两个时间预算。
#[test]
fn the_public_surface_is_stable() {
    assert_eq!(kind(), ChannelKind::WeCom);
    assert_eq!(kind().storage_str(), CHANNEL_TYPE);
    assert_eq!(origin_type(), "wecom_chat");
    assert_eq!(DEFAULT_WS_URL, "wss://openws.work.weixin.qq.com");
    assert_eq!(SUBSCRIBE_TIMEOUT.as_secs(), 10);
    assert_eq!(super::r#loop::CALLBACK_QUEUE_DEPTH, 64);
    assert_eq!(super::r#loop::READ_DEADLINE.as_secs(), 90);
    // 读窗口必须比心跳宽出一截，否则一次稍晚的 pong 会造成误判重连。
    assert!(
        super::r#loop::READ_DEADLINE > crate::wecom::ws_frame::PING_INTERVAL * 2,
        "读截止必须远宽于心跳间隔"
    );
}

/// `WeComChannel` 的私有字段在**子模块**里可见，且 `Debug` 只报"有没有密钥"。
#[test]
fn the_channel_exposes_the_non_secret_identity() {
    let (deps, _) = deps("bot_5f1c9a", "s");
    let channel = WeComChannel {
        installation_id: Some(Id::new()),
        bot_id: "bot_5f1c9a".to_string(),
        secret: PlaintextSecret::new("THE-SECRET"),
        bot_display_name: "Multica Bot".to_string(),
        handler: None,
        dialer: Arc::clone(&deps.dialer),
        ws_url: "wss://fake.example/aibot".to_string(),
        senders: None,
        metrics: None,
    };
    assert_eq!(channel.bot_id(), "bot_5f1c9a");
    assert_eq!(channel.bot_display_name(), "Multica Bot");
    assert!(channel.installation_id().is_some());
    let rendered = format!("{channel:?}");
    assert!(rendered.contains("<redacted>"), "{rendered}");
    assert!(!rendered.contains("THE-SECRET"), "{rendered}");
    // 没配汇 ⇒ 拿到的是 no-op（永远可调用，不阻塞）。
    channel.metrics().record_connect_failure();
}
