//! `routes::channels::wecom` 的用例（写者 M7-15）。
//!
//! 分两半，**判据不同**：
//!
//! - **不依赖库的那一半**（门 ⑤）：路由形态、未配置语义**逐端点**、401、wire 形状与
//!   凭据纪律、两条错误矩阵。装置用 `AppState` 的**结构体字面量**
//!   （`channel_keys` 要按用例注入"配了 / 没配"；`AppState::new` 只会读进程 env）；
//! - **真库的那一半**（门 ⑥，`#[ignore]`，`MULTICA_TEST_DATABASE_URL`）：四条路由的
//!   端到端行为 + [`store`] 那两个端口实现的 SQL。未设置变量 ⇒ 打印跳过并 `return`；
//!   **已设置但连不上 / 没建表 ⇒ panic**（不许静默假装绿）。
//!
//! `docs/60` §6.5 第 5 条：每条路由至少一条用例，且**不用** `health::placeholder`。

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use serde_json::json;
use tower::ServiceExt as _;

use super::*;
use crate::state::ChannelKeys;

/// 不可达端口的懒连接池：不拨号、不建库（本文件只跑不需要库的那一半）。
const LAZY_URL: &str = "postgres://wecom:wecom@127.0.0.1:1/wecom";

/// `WeCom` 的落库密钥（`MULTICA_WECOM_SECRET_KEY` 的形态：base64 的 32 字节，`0x05` × 32）。
const SECRET_KEY_BASE64: &str = "BQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQU=";

/// 一份**明文**哨兵：任何响应的正文或错误文案里出现它就失败（凭据纪律）。
const PLAINTEXT_SENTINEL: &str = "wecom-secret-DO-NOT-LOG";

fn state_with(channel_keys: ChannelKeys) -> Arc<AppState> {
    state_with_db(
        channel_keys,
        mc_db::Db::connect_lazy(LAZY_URL, 1, 0).expect("lazy pool"),
    )
}

fn state_with_db(channel_keys: ChannelKeys, db: mc_db::Db) -> Arc<AppState> {
    let realtime = mc_realtime::RealtimeHandle::start(8);
    let ws = Arc::new(mc_realtime::WsState::new(realtime.clone(), "wecom-tests"));
    Arc::new(AppState {
        db,
        runtime: crate::state::RuntimeHandles {
            actors: mc_core::actor::ActorRegistry::new(),
            adapters: Arc::new(crate::state::AdapterRegistry::default()),
        },
        config: crate::state::ConfigSnapshot {
            host: "127.0.0.1".into(),
            port: 0,
            session_cookie: "multica_session".into(),
            api_key_header: "X-Multica-Api-Key".into(),
            csrf_header: "X-Multica-Csrf".into(),
            invitation_per_workspace_per_hour: Some(50),
            ..Default::default()
        },
        storage: mc_storage::Storage::new(),
        secrets: mc_secrets::Secrets::new(mc_auth::DefaultSecretsBackend::in_memory()),
        feature_flags: Arc::new(mc_feature_flags::FeatureFlagCatalog::new()),
        realtime,
        ws,
        auth: mc_auth::SessionStoreContainer::new(),
        pat: mc_auth::PatStoreContainer::new(),
        verification: mc_auth::VerificationStoreContainer::new(),
        google_oauth: crate::state::GoogleOAuthConfig::default(),
        daemon_hub: Arc::new(mc_ws::hub::Hub::new()),
        daemon_requests: Arc::new(crate::daemon_requests::RequestStore::new()),
        plugin_key: None,
        plugin_surface_origin: None,
        channel_keys,
        github_keys: crate::state::integrations::GithubKeys::default(),
        vcs_keys: crate::state::integrations::VcsKeys::default(),
        composio_keys: crate::state::integrations::ComposioKeys::default(),
        // M9 anchor（LUM-1815）：云面两组字段显式未配置（口径见 `state/cloud.rs`）。AppState 的字面量构造点**全部**在这里补，因为它们不用 `..Default::default()`（见 docs/32 §9.13 的写集扩展登记）。
        cloud: crate::state::cloud::CloudConfig::from_env_with(|_| None),
        entitlement: crate::state::cloud::EntitlementConfig::from_env_with(|_| None),
    })
}

fn mount(state: Arc<AppState>) -> Router {
    let router = crate::routes::router(state.clone());
    router.with_state(state)
}

/// 未配置的整站装置（`channel_keys` 全空）。
fn unconfigured() -> Router {
    mount(state_with(ChannelKeys::default()))
}

/// 配好 `WeCom` 落库密钥的整站装置。
fn configured() -> Router {
    mount(configured_state())
}

fn configured_state() -> Arc<AppState> {
    state_with(ChannelKeys::from_env_with(|name| {
        if name == "MULTICA_WECOM_SECRET_KEY" {
            Some(SECRET_KEY_BASE64.to_string())
        } else {
            None
        }
    }))
}

/// 发一次请求，返回 `(status, json)`。
async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    user: Option<uuid::Uuid>,
) -> (StatusCode, serde_json::Value) {
    call_with_body(app, method, uri, user, "{}").await
}

async fn call_with_body(
    app: &Router,
    method: &str,
    uri: &str,
    user: Option<uuid::Uuid>,
    body: &str,
) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(user) = user {
        builder = builder.header("x-multica-user-id", user.to_string());
    }
    // 写路由带一个 JSON 体（否则 axum 的 `Json` 提取器先回 415，测不到未配置分支）。
    let body = if method == "POST" {
        builder = builder.header("content-type", "application/json");
        Body::from(body.to_string())
    } else {
        Body::empty()
    };
    let response = app
        .clone()
        .oneshot(builder.body(body).expect("request"))
        .await
        .expect("dispatch");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let json = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    };
    (status, json)
}

fn workspace_uri(suffix: &str) -> String {
    format!("/api/workspaces/{}/{suffix}", uuid::Uuid::new_v4())
}

/// 四条路由的 `(method, uri)`（未配置与未授权两族用例共用同一张表）。
fn the_four_routes(
    workspace_id: uuid::Uuid,
    installation_id: uuid::Uuid,
) -> Vec<(&'static str, String)> {
    let ws = workspace_id.to_string();
    vec![
        ("GET", format!("/api/workspaces/{ws}/wecom/installations")),
        (
            "DELETE",
            format!("/api/workspaces/{ws}/wecom/installations/{installation_id}"),
        ),
        (
            "POST",
            format!(
                "/api/workspaces/{ws}/wecom/install/byo?agent_id={}",
                uuid::Uuid::new_v4()
            ),
        ),
        ("POST", "/api/wecom/binding/redeem".to_string()),
    ]
}

// =====================================================================
// ① 路由形态
// =====================================================================

#[test]
fn the_router_registers_the_four_upstream_paths_without_panicking() {
    // axum 0.7 同 path+method 重复注册会在 build 期 panic ⇒ 这一行就是"四条互不冲突"的判据。
    let _ = router();
}

// =====================================================================
// ② 未配置语义（逐端点不同）
// =====================================================================

#[tokio::test]
async fn the_unconfigured_semantics_are_per_endpoint() {
    let app = unconfigured();
    let user = uuid::Uuid::new_v4();
    let workspace_id = uuid::Uuid::new_v4();
    let installation_id = uuid::Uuid::new_v4();
    let routes = the_four_routes(workspace_id, installation_id);

    // 列表：200 空 + 两个 `false`（**不**查库、**不**读身份 —— 匿名也给 200）。
    let (status, body) = call(&app, "GET", &routes[0].1, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["installations"], json!([]));
    assert_eq!(body["configured"], json!(false));
    assert_eq!(body["install_supported"], json!(false));

    // 其余三条：403 `wecom_not_configured`（**不是** 503 —— 见模块头的口径更正）。
    for (method, uri) in &routes[1..] {
        let (status, body) = call(&app, method, uri, Some(user)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}: {body}");
        assert_eq!(
            body["error"]["code"],
            json!(dto::CODE_WECOM_NOT_CONFIGURED),
            "{method} {uri}"
        );
    }
}

/// 只有 `MULTICA_WECOM_SECRET_KEY` 能打开这四条路由：别的渠道的密钥配了不算数
/// （`ChannelKind::WeCom` 是键，不是"任意一个渠道配了"）。
#[tokio::test]
async fn only_the_wecom_deployment_key_enables_the_routes() {
    let state = state_with(ChannelKeys::from_env_with(|name| match name {
        "MULTICA_DINGTALK_SECRET_KEY"
        | "MULTICA_SLACK_SECRET_KEY"
        | "MULTICA_LARK_SECRET_KEY"
        | "MULTICA_TELEGRAM_SECRET_KEY" => Some(SECRET_KEY_BASE64.to_string()),
        _ => None,
    }));
    assert!(!dto::is_configured(&state), "别的渠道配了不等于 wecom 配了");
    let app = mount(state);
    let (status, body) = call(&app, "GET", &workspace_uri("wecom/installations"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["configured"], json!(false));
    let (status, body) = call(&app, "POST", "/api/wecom/binding/redeem", None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

// =====================================================================
// ③ 配置好了但没身份 ⇒ 401（四条都先要会话）
// =====================================================================

#[tokio::test]
async fn every_route_requires_a_session_once_configured() {
    let app = configured();
    let workspace_id = uuid::Uuid::new_v4();
    let installation_id = uuid::Uuid::new_v4();
    for (method, uri) in the_four_routes(workspace_id, installation_id) {
        let (status, body) = call(&app, method, &uri, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}: {body}");
    }
}

// =====================================================================
// ④ wire 形状与凭据纪律
// =====================================================================

/// 一条安装的装置（密文列刻意用**不可能**被序列化出去的字面量）。
fn installation() -> mc_channel::wecom::types::Installation {
    let now = chrono::Utc::now();
    mc_channel::wecom::types::Installation {
        id: Id::new(),
        workspace_id: Id::new(),
        agent_id: Id::new(),
        installer_user_id: Id::new(),
        status: mc_core::channel::InstallationStatus::Active,
        bot_id: "bot_5f1c9a".to_string(),
        secret_encrypted: PLAINTEXT_SENTINEL.as_bytes().to_vec(),
        bot_display_name: "Multica Bot".to_string(),
        config: json!({
            "app_id": "bot_5f1c9a",
            "bot_id": "bot_5f1c9a",
            "secret_encrypted": "CIPHERTEXT-DO-NOT-LOG",
        }),
        installed_at: now,
        created_at: now,
        updated_at: now,
    }
}

#[test]
fn the_installation_response_never_carries_the_config() {
    let response = WecomInstallationResponse::from_installation(&installation());
    let text = serde_json::to_string(&response).expect("serialize");
    for forbidden in ["CIPHERTEXT", "encrypted", "config", PLAINTEXT_SENTINEL] {
        assert!(!text.contains(forbidden), "回显了 {forbidden}：{text}");
    }
    let object: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&text).expect("object");
    for want in [
        "id",
        "workspace_id",
        "agent_id",
        "bot_id",
        "installer_user_id",
        "status",
        "installed_at",
        "created_at",
        "updated_at",
    ] {
        assert!(object.contains_key(want), "缺字段 {want}：{text}");
    }
    // `bot_id` **不是**秘密（管理后台可见）⇒ 它在响应里，且不在 config 里也能拿到。
    assert_eq!(response.bot_id, "bot_5f1c9a");
    assert_eq!(response.status, "active");
}

#[test]
fn the_list_envelopes_are_the_upstream_three_cells() {
    let value = serde_json::to_value(WecomInstallationsResponse::not_configured()).expect("json");
    assert_eq!(value["installations"], json!([]));
    assert_eq!(value["configured"], json!(false));
    assert_eq!(value["install_supported"], json!(false));

    let ready = WecomInstallationsResponse::configured_with(vec![
        WecomInstallationResponse::from_installation(&installation()),
    ]);
    assert!(ready.configured && ready.install_supported);
    assert_eq!(ready.installations.len(), 1);
}

#[test]
fn the_binding_url_and_encoder_match_the_upstream_shape() {
    assert_eq!(
        dto::binding_url("https://app.multica.ai/", "a b/c"),
        "https://app.multica.ai/wecom/bind?token=a+b%2Fc"
    );
    assert_eq!(dto::url_encode("AZaz09-_.~"), "AZaz09-_.~");
    assert_eq!(dto::url_encode("+/="), "%2B%2F%3D");
    // 空格 → `+`（Go 的 `url.QueryEscape`，与其它三个渠道副本同一份实现）。
    assert_eq!(dto::url_encode("a b"), "a+b");
    assert_eq!(dto::BINDING_PATH, "/wecom/bind");
}

// =====================================================================
// ⑤ 两条错误矩阵
// =====================================================================

#[test]
fn the_install_error_mapping_matches_the_upstream_switch() {
    let cases = [
        (InstallError::NotFound, 404, "wecom_installation_not_found"),
        (
            InstallError::InvalidParams { field: "bot_id" },
            400,
            "wecom_install_rejected",
        ),
        (
            InstallError::CredentialsRejected { errcode: 40001 },
            400,
            "wecom_credentials_rejected",
        ),
        (
            InstallError::CredentialsUnverifiable { errcode: 45009 },
            503,
            "wecom_credentials_unverifiable",
        ),
        (
            InstallError::OwnedBySameWorkspace,
            409,
            "wecom_bot_owned_by_same_workspace",
        ),
        (
            InstallError::OwnedByArchivedAgent,
            409,
            "wecom_bot_owned_by_archived_agent",
        ),
        (
            InstallError::OwnedByAnotherWorkspace,
            409,
            "wecom_bot_owned_by_another_workspace",
        ),
        (InstallError::Seal, 500, "wecom_install_failed"),
        (InstallError::Encode, 500, "wecom_install_failed"),
        (
            InstallError::Store {
                message: "connection refused".into(),
            },
            500,
            "wecom_install_failed",
        ),
    ];
    for (error, expected, code) in cases {
        assert_eq!(error.http_status(), expected, "{error:?}");
        assert_eq!(error.code(), code, "{error:?}");
        let response = install_error(&error);
        assert_eq!(response.status().as_u16(), expected, "{error:?}");
        let text = format!("{error:?}{error}");
        assert!(
            !text.contains(PLAINTEXT_SENTINEL),
            "错误文案回显了密钥：{text}"
        );
    }
}

#[test]
fn the_binding_error_mapping_matches_the_upstream_switch() {
    let cases = [
        (
            BindingError::TokenInvalid,
            410,
            "wecom_binding_token_invalid",
        ),
        (
            BindingError::AlreadyAssigned,
            409,
            "wecom_binding_already_assigned",
        ),
        (BindingError::NotMember, 403, "wecom_binding_not_member"),
        (
            BindingError::Store {
                message: "boom".into(),
            },
            500,
            "wecom_binding_store_error",
        ),
    ];
    for (error, expected, code) in cases {
        assert_eq!(error.http_status(), expected, "{error:?}");
        assert_eq!(error.code(), code, "{error:?}");
        let response = binding_error(&error);
        assert_eq!(response.status().as_u16(), expected, "{error:?}");
    }
    // 判决 → 错误映射：`Bound` 不是错误，其余三条各自有码。
    assert!(BindingError::from_redeem(&RedeemOutcome::Bound(
        mc_channel::wecom::binding::RedeemedBinding {
            workspace_id: Id::new(),
            installation_id: Id::new(),
            channel_user_id: "T-1".into(),
        }
    ))
    .is_none());
    for (outcome, expected) in [
        (RedeemOutcome::TokenInvalid, 410),
        (RedeemOutcome::AlreadyAssigned, 409),
        (RedeemOutcome::NotMember, 403),
    ] {
        let error = BindingError::from_redeem(&outcome).expect("错误");
        assert_eq!(binding_error(&error).status().as_u16(), expected);
    }
}

/// BYO 的请求体：三个键都可缺（上游 `RegisterWecomBYORequest` 只有 `bot_id` / `secret`
/// 两个必填语义，`bot_name` 是 `omitempty`），且**不派生 `Debug`** ——
/// 它装的是**明文**密钥，默认派生会把它写进任何 `{:?}` 插值（`docs/60` §2.3 第 1 条）。
/// 这条纪律在 adapter 侧有对应用例（`InstallationParams` 的手写 `Debug`）。
#[test]
fn the_byo_request_body_defaults_every_field() {
    let body: RegisterWecomByoRequest =
        serde_json::from_value(json!({ "bot_id": "bot_1" })).expect("parse");
    assert_eq!(body.bot_id, "bot_1");
    assert_eq!(body.secret, "");
    assert_eq!(body.bot_name, "");
    let empty: RegisterWecomByoRequest = serde_json::from_value(json!({})).expect("parse empty");
    assert!(empty.bot_id.is_empty());
    // 兑换请求体同理（空体不 panic，交给 handler 的 400）。
    let redeem: RedeemWecomBindingTokenRequest =
        serde_json::from_value(json!({})).expect("parse redeem");
    assert!(redeem.token.is_empty());
}

// =====================================================================
// ⑥ 真库（门 ⑥，`MULTICA_TEST_DATABASE_URL`）
// =====================================================================

// 真库那一半（门 ⑥）：`wecom/tests/{support,db}.rs`。
mod db;
mod support;
