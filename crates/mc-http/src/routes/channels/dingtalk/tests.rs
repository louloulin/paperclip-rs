//! `routes::channels::dingtalk` 的**不依赖库**用例（写者 M7-9）。
//!
//! 七条路由的**真库**行为在 `crates/mc-http/tests/channels/dingtalk.rs`（门 ⑥，`#[ignore]`）；
//! 这里钉的是不需要库的那一半：
//!
//! 1. **路由形态**：七条注册键逐字存在，`group-routes`（已退役）**保持 404**；
//! 2. **未配置语义逐端点不同**（R-M7-3）+ agent 级那道门**先于** nil 判断；
//! 3. **wire 形状**：DTO 的字段名与"绝不带 config"的凭据纪律；
//! 4. 错误映射（400 / 403 / 409 / 500）。
//!
//! 装置用 `AppState` 的**结构体字面量**（`channel_keys` 要按用例注入"配了 / 没配"；
//! `AppState::new` 只会读进程 env —— 与 `tests/channels/support.rs` 的构造点同款）。

use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt as _;
use serde_json::json;
use tower::ServiceExt as _;

use super::*;
use crate::state::ChannelKeys;
use mc_channel::dingtalk::install::InstallRecord;

/// 不可达端口的懒连接池：不拨号、不建库（本文件只跑不需要库的那一半）。
const LAZY_URL: &str = "postgres://dingtalk:dingtalk@127.0.0.1:1/dingtalk";

/// `DingTalk` 的落库密钥（`MULTICA_DINGTALK_SECRET_KEY` 的形态：base64 的 32 字节）。
const SECRET_KEY_BASE64: &str = "CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=";

fn state_with(channel_keys: ChannelKeys) -> Arc<AppState> {
    let db = mc_db::pool::Db::connect_lazy(LAZY_URL, 1, 0).expect("lazy pool");
    let realtime = mc_realtime::RealtimeHandle::start(8);
    let ws = Arc::new(mc_realtime::WsState::new(
        realtime.clone(),
        "dingtalk-tests",
    ));
    Arc::new(AppState {
        db,
        runtime: mc_http_runtime_handles(),
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

fn mc_http_runtime_handles() -> crate::state::RuntimeHandles {
    crate::state::RuntimeHandles {
        actors: mc_core::actor::ActorRegistry::new(),
        adapters: Arc::new(crate::state::AdapterRegistry::default()),
    }
}

fn mount(state: Arc<AppState>) -> Router {
    let router = crate::routes::router(state.clone());
    router.with_state(state)
}

/// 未配置的整站装置（`channel_keys` 全空）。
fn unconfigured() -> Router {
    mount(state_with(ChannelKeys::default()))
}

/// 配好 `DingTalk` 落库密钥的整站装置。
fn configured() -> Router {
    mount(state_with(ChannelKeys::from_env_with(|name| {
        if name == "MULTICA_DINGTALK_SECRET_KEY" {
            Some(SECRET_KEY_BASE64.to_string())
        } else {
            None
        }
    })))
}

/// 发一次请求，返回 `(status, json)`。
async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    user: Option<uuid::Uuid>,
) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(user) = user {
        builder = builder.header("x-multica-user-id", user.to_string());
    }
    // 写路由带一个空 JSON 体（否则 axum 的 `Json` 提取器先回 415，测不到未配置分支）。
    let body = if method == "POST" {
        builder = builder.header("content-type", "application/json");
        Body::from("{}")
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

// =====================================================================
// ① 路由形态
// =====================================================================

#[test]
fn the_router_registers_the_seven_upstream_paths_without_panicking() {
    // axum 0.7 同 path+method 重复注册会在 build 期 panic ⇒ 这一行就是"七条互不冲突"的判据。
    let _ = router();
}

/// **反向验收**（`docs/60` §1.6）：上游已退役 `group-routes` 并在 `integration_test.go:786`
/// 主动断言它 **404**。本仓**不注册**它，也不得为它建读面。
#[tokio::test]
async fn the_retired_group_routes_route_stays_a_404() {
    let app = configured();
    let (status, body) = call(
        &app,
        "GET",
        &workspace_uri("dingtalk/group-routes"),
        Some(uuid::Uuid::new_v4()),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    // 上游断言的是"404 空 body"（axum 的 fallback），不是 403 / 200 空数组。
    assert_eq!(body, serde_json::Value::Null);
}

// =====================================================================
// ② 未配置语义（逐端点不同）
// =====================================================================

#[tokio::test]
async fn the_unconfigured_semantics_are_per_endpoint() {
    let app = unconfigured();
    let user = uuid::Uuid::new_v4();

    // 列表：200 空 + 两个 `false`（**不**查库、**不**读身份 —— 匿名也给 200）。
    let (status, body) = call(&app, "GET", &workspace_uri("dingtalk/installations"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["installations"], json!([]));
    assert_eq!(body["configured"], json!(false));
    assert_eq!(body["install_supported"], json!(false));

    // 群清单：200 + 空清单，且 `group_discovery_supported` 是 **true**
    // （「能发现，只是暂时没数据」—— 与 lark 的 `install_supported:false` 不是一回事）。
    let (status, body) = call(&app, "GET", &workspace_uri("dingtalk/groups"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["groups"], json!([]));
    assert_eq!(body["group_discovery_supported"], json!(true));
    assert_eq!(body["inactive_group_counts"], json!({}));
    assert_eq!(body["bot_identities"], json!({}));
    assert!(body.get("next_offset").is_none(), "没有下一页游标：{body}");

    // 其余四条：403 `dingtalk_not_configured`（**不是** 503）。
    let installation = uuid::Uuid::new_v4();
    let cases = [
        (
            "DELETE",
            workspace_uri(&format!("dingtalk/installations/{installation}")),
        ),
        (
            "DELETE",
            workspace_uri(&format!(
                "dingtalk/installations/{installation}/groups/cid-1"
            )),
        ),
        (
            "POST",
            workspace_uri(&format!(
                "dingtalk/install/byo?agent_id={}",
                uuid::Uuid::new_v4()
            )),
        ),
        ("POST", "/api/dingtalk/binding/redeem".to_string()),
    ];
    for (method, uri) in cases {
        let (status, body) = call(&app, method, &uri, Some(user)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}: {body}");
        assert_eq!(body["error"]["code"], json!(CODE_DINGTALK_NOT_CONFIGURED));
    }
}

/// agent 级那条是**唯一**把 agent 门放在 nil 判断**之前**的（上游逐字）⇒ 未配置也不能把它
/// 变成"200 空"：没身份是 401，有身份但库不可达也**不是** 200。
#[tokio::test]
async fn the_agent_level_gate_runs_before_the_unconfigured_branch() {
    let app = unconfigured();
    let uri = format!("/api/agents/{}/dingtalk/groups", uuid::Uuid::new_v4());
    let (status, _) = call(&app, "GET", &uri, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "先要身份");

    let (status, _) = call(&app, "GET", &uri, Some(uuid::Uuid::new_v4())).await;
    assert_ne!(
        status,
        StatusCode::OK,
        "agent 门先于 nil 判断 ⇒ 未配置**不**回 200 空"
    );

    // 对照：workspace 级的那条同一个未配置装置回 200（顺序确实不同）。
    let (status, _) = call(&app, "GET", &workspace_uri("dingtalk/groups"), None).await;
    assert_eq!(status, StatusCode::OK);
}

// =====================================================================
// ③ wire 形状与凭据纪律
// =====================================================================

/// 一条安装的读面装置（密文列刻意用**不可能**被序列化出去的字面量）。
fn record() -> InstallRecord {
    InstallRecord {
        id: Id::new(),
        workspace_id: Id::new(),
        agent_id: Id::new(),
        installer_user_id: Id::new(),
        status: "active".to_string(),
        config: json!({
            "app_id": "dingkey",
            "robot_code": "dingkey",
            "app_secret_encrypted": "CIPHERTEXT-DO-NOT-LOG",
        }),
        installed_at: chrono::Utc::now(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

#[test]
fn the_installation_response_never_carries_the_config() {
    let response = DingTalkInstallationResponse::from_record(&record());
    let text = serde_json::to_string(&response).expect("serialize");
    assert!(!text.contains("CIPHERTEXT"), "{text}");
    assert!(!text.contains("encrypted"), "{text}");
    assert!(!text.contains("config"), "{text}");
    assert!(response.agent_available);
    assert!(response.bound_dingtalk_user_ids.is_none());
    let object: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&text).expect("object");
    for want in [
        "id",
        "workspace_id",
        "agent_id",
        "installer_user_id",
        "status",
        "installed_at",
        "created_at",
        "updated_at",
        "agent_available",
    ] {
        assert!(object.contains_key(want), "缺字段 {want}：{text}");
    }
    // `AppKey` **不是**秘密 ⇒ 它**不**在响应里，只因为它不在安装行的对外投影里
    // （身份列要经 `decode_public_config`；这里钉住"没有多余的键"）。
    assert!(!object.contains_key("app_id"), "{text}");
    // `bound_dingtalk_user_ids` 非管理员**缺席**（上游 `omitempty`）。
    assert!(!object.contains_key("bound_dingtalk_user_ids"), "{text}");
}

#[test]
fn the_bound_user_ids_column_is_omitted_when_unset_and_present_when_empty() {
    let mut response = DingTalkInstallationResponse::from_record(&record());
    response.bound_dingtalk_user_ids = Some(Vec::new());
    let text = serde_json::to_string(&response).expect("serialize");
    assert!(text.contains("\"bound_dingtalk_user_ids\":[]"), "{text}");
}

#[test]
fn the_not_configured_envelope_is_empty_with_two_false_flags() {
    let response = DingTalkInstallationsResponse::not_configured();
    let value = serde_json::to_value(&response).expect("json");
    assert_eq!(value["installations"], json!([]));
    assert_eq!(value["configured"], json!(false));
    assert_eq!(value["install_supported"], json!(false));
    let ready = DingTalkInstallationsResponse::configured_with(vec![
        DingTalkInstallationResponse::from_record(&record()),
    ]);
    assert!(ready.configured && ready.install_supported);
    assert_eq!(ready.installations.len(), 1);
}

#[test]
fn the_empty_group_inventory_matches_the_upstream_nil_branch() {
    let value = serde_json::to_value(GroupInventory::empty()).expect("json");
    assert_eq!(value["groups"], json!([]));
    assert_eq!(value["group_discovery_supported"], json!(true));
    assert_eq!(value["inactive_group_counts"], json!({}));
    assert_eq!(value["bot_identities"], json!({}));
    assert!(value.get("next_offset").is_none());
}

#[test]
fn the_binding_url_and_encoder_match_the_upstream_shape() {
    assert_eq!(
        binding_url("https://app.multica.ai/", "a b/c"),
        "https://app.multica.ai/dingtalk/bind?token=a+b%2Fc"
    );
    assert_eq!(url_encode("AZaz09-_.~"), "AZaz09-_.~");
    assert_eq!(url_encode("+/="), "%2B%2F%3D");
    // 空格 → `+`（Go 的 `url.QueryEscape`，与 telegram / slack 两侧同一份实现）。
    assert_eq!(url_encode("a b"), "a+b");
}

// =====================================================================
// ④ 错误映射
// =====================================================================

#[tokio::test]
async fn the_feature_disabled_response_is_a_403_with_the_upstream_code() {
    let response = feature_disabled();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .expect("body");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(value["error"]["code"], json!(CODE_DINGTALK_NOT_CONFIGURED));
}

#[test]
fn the_install_error_mapping_matches_the_upstream_switch() {
    use mc_channel::dingtalk::install::InstallError;

    let cases = [
        (InstallError::NotFound, 404),
        (InstallError::InvalidAppKey, 400),
        (InstallError::InvalidAppSecret, 400),
        (InstallError::CredentialValidation, 400),
        (InstallError::OwnedBySameWorkspace, 409),
        (InstallError::OwnedByArchivedAgent, 409),
        (InstallError::OwnedByAnotherWorkspace, 409),
        (InstallError::Seal, 500),
        (InstallError::Encode, 500),
        (
            InstallError::Store {
                message: "boom".to_string(),
            },
            500,
        ),
    ];
    for (error, expected) in cases {
        assert_eq!(error.http_status(), expected, "{error:?}");
        assert_eq!(
            install_error(&error).status().as_u16(),
            expected,
            "{error:?}"
        );
    }
}
