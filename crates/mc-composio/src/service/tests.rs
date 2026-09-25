//! `service` 的单元测试（从 `service.rs` 拆出：门 ⑩ 的单文件 800 行硬上限）。

use crate::service::*;
use crate::state::DEFAULT_STATE_TTL_SECS;

const USER: &str = "0b2f5a52-0b1a-4f4e-9d2f-7a1c6f0d1e2a";

fn configured(api_base: &str) -> ComposioConfig {
    ComposioConfig {
        api_key: Some("ak_test".into()),
        state_secret: Some("state-secret".into()),
        callback_base_url: Some("https://api.example.test/".into()),
        feature_enabled: true,
        api_base: Some(api_base.into()),
        ..ComposioConfig::default()
    }
}

#[test]
fn config_requires_all_four_conditions() {
    let mut config = ComposioConfig::default();
    assert!(!config.is_configured());
    assert_eq!(config.missing().len(), 4);

    config.feature_enabled = true;
    config.api_key = Some("k".into());
    config.state_secret = Some("s".into());
    assert!(!config.is_configured(), "callback base is still missing");
    config.callback_base_url = Some("https://example.test".into());
    assert!(config.is_configured());
    assert!(config.missing().is_empty());

    // Debug 不回显密钥值。
    assert!(!format!("{config:?}").contains("\"k\""));
}

#[test]
fn missing_names_every_unconfigured_condition() {
    let missing = ComposioConfig::default().missing();
    assert_eq!(
        missing,
        vec![
            "feature_flag",
            "COMPOSIO_API_KEY",
            "COMPOSIO_STATE_SECRET|JWT_SECRET",
            "COMPOSIO_CALLBACK_BASE_URL|MULTICA_PUBLIC_URL",
        ]
    );
}

#[test]
fn service_assembles_without_panicking_when_unconfigured() {
    let service = ComposioService::new(ComposioConfig::default());
    assert!(!service.enabled(), "未配置 ⇒ 空服务，而非 panic");
    assert!(!service.client().enabled());
    assert_eq!(service.signer().ttl_secs(), DEFAULT_STATE_TTL_SECS);
    assert_eq!(
        service.callback_url(),
        Err(ComposioError::NotConfigured),
        "缺回调基址 ⇒ 连回调地址都造不出来"
    );
    assert!(format!("{service:?}").contains("store"));
}

#[test]
fn callback_url_is_the_api_base_plus_the_frozen_path() {
    let service = ComposioService::new(configured("http://127.0.0.1:9"));
    assert_eq!(
        service.callback_url().expect("callback url"),
        format!("https://api.example.test{CALLBACK_PATH}")
    );
    assert_eq!(CALLBACK_PATH, "/api/integrations/composio/callback");
    assert!(service.enabled());
}

#[tokio::test]
async fn begin_connect_needs_a_callback_base_and_a_live_directory() {
    // 缺回调基址 ⇒ NotConfigured（**不**先打上游）。
    let mut config = configured("http://127.0.0.1:9");
    config.callback_base_url = None;
    let service = ComposioService::new(config);
    assert_eq!(
        service
            .begin_connect(Id::parse(USER).expect("uuid"), "notion")
            .await,
        Err(ComposioError::NotConfigured)
    );
}

#[tokio::test]
async fn unconfigured_service_rejects_every_state() {
    // 空 secret：任何 state（含格式合法的）都验不过。
    let service = ComposioService::new(ComposioConfig::default());
    assert_eq!(
        service.complete_callback("bogus", "success", "ca_x").await,
        CallbackOutcome::StateRejected(StateError::Malformed)
    );
    let unsigned = ComposioService::new(configured("http://127.0.0.1:9"));
    let state = StateClaims::new(USER, "notion", "ac_1", now_unix(), 300);
    let token = unsigned
        .signer()
        .sign(&state)
        .expect("另一个 secret 签出的 state 也是格式合法的");
    let empty_secret = ComposioService::new(ComposioConfig::default());
    assert_eq!(
        empty_secret
            .complete_callback(&token, "success", "ca_x")
            .await,
        CallbackOutcome::StateRejected(StateError::Tampered)
    );
}

#[tokio::test]
async fn expired_state_is_rejected_before_anything_else() {
    let mut config = configured("http://127.0.0.1:9");
    config.state_ttl_secs = Some(-1);
    let service = ComposioService::new(config);
    let state = StateClaims::new(USER, "notion", "ac_1", now_unix() - 5, -1);
    let token = service.signer().sign(&state).expect("sign");
    assert_eq!(
        service.complete_callback(&token, "success", "ca_x").await,
        CallbackOutcome::StateRejected(StateError::Expired)
    );
}

#[tokio::test]
async fn a_valid_state_on_an_unconfigured_deployment_is_not_configured() {
    // secret 在（能签），但 flag / key / 回调基址缺 ⇒ **合法 state** 也会落到 403 那一格。
    let signer_service = ComposioService::new(ComposioConfig {
        state_secret: Some("state-secret".into()),
        ..ComposioConfig::default()
    });
    let state = StateClaims::new(USER, "notion", "ac_1", now_unix(), 300);
    let token = signer_service.signer().sign(&state).expect("sign");
    assert_eq!(
        signer_service
            .complete_callback(&token, "success", "ca_x")
            .await,
        CallbackOutcome::NotConfigured {
            toolkit_slug: "notion".into()
        }
    );
}

#[tokio::test]
async fn a_valid_state_with_a_non_success_status_never_writes() {
    let service = ComposioService::new(configured("http://127.0.0.1:9"));
    let state = StateClaims::new(USER, "notion", "ac_1", now_unix(), 300);
    let token = service.signer().sign(&state).expect("sign");
    assert_eq!(
        service.complete_callback(&token, "failed", "ca_x").await,
        CallbackOutcome::Rejected {
            toolkit_slug: "notion".into(),
            reason: ComposioError::ConnectNotSuccessful,
        }
    );
    // 同一个 state 的第二次到达 ⇒ 已经消费掉了（重放）。
    assert_eq!(
        service.complete_callback(&token, "success", "ca_x").await,
        CallbackOutcome::StateRejected(StateError::Replayed)
    );
}

#[tokio::test]
async fn a_valid_state_without_a_connected_account_is_rejected() {
    let service = ComposioService::new(configured("http://127.0.0.1:9"));
    let state = StateClaims::new(USER, "notion", "ac_1", now_unix(), 300);
    let token = service.signer().sign(&state).expect("sign");
    let outcome = service.complete_callback(&token, "SUCCESS ", "  ").await;
    assert!(
        matches!(
            outcome,
            CallbackOutcome::Rejected {
                reason: ComposioError::Malformed(_),
                ..
            }
        ),
        "缺 connected_account_id ⇒ 拒绝：{outcome:?}"
    );
}

#[tokio::test]
async fn state_with_a_non_uuid_user_is_rejected() {
    let service = ComposioService::new(configured("http://127.0.0.1:9"));
    let state = StateClaims::new("not-a-uuid", "notion", "ac_1", now_unix(), 300);
    let token = service.signer().sign(&state).expect("sign");
    let outcome = service.complete_callback(&token, "success", "ca_x").await;
    assert!(
        matches!(
            outcome,
            CallbackOutcome::Rejected {
                reason: ComposioError::Malformed(_),
                ..
            }
        ),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn store_backed_methods_report_a_missing_store() {
    let service = ComposioService::new(configured("http://127.0.0.1:9"));
    let user = Id::parse(USER).expect("uuid");
    let row = Id::parse("11111111-1111-1111-1111-111111111111").expect("uuid");
    assert_eq!(
        service.list_connections(user).await,
        Err(ComposioError::StoreMissing)
    );
    assert_eq!(
        service.disconnect(user, row).await,
        Err(ComposioError::StoreMissing)
    );
    assert_eq!(
        service.create_mcp_session(user).await,
        Err(ComposioError::StoreMissing)
    );
}

#[test]
fn errors_never_echo_credentials() {
    let rendered = [
        ComposioError::Transport("connect error".into()).to_string(),
        ComposioError::Upstream {
            status: 500,
            context: "list toolkits".into(),
        }
        .to_string(),
        ComposioError::Store("db down".into()).to_string(),
    ]
    .join("|");
    assert!(!rendered.contains("ak_test"));
    assert!(!rendered.contains("state-secret"));
    // 服务与配置的 Debug 都不回显密钥。
    let service = ComposioService::new(configured("http://127.0.0.1:9"));
    let debug = format!("{service:?}");
    assert!(!debug.contains("ak_test"));
    assert!(!debug.contains("state-secret"));
}
