//! `routes::channels::lark` 的用例（写者 M7-14）。
//!
//! 分两半，**判据不同**：
//!
//! - **不依赖库的那一半**（门 ⑤）：路由形态、**七条 golden fixture 的逐条复刻**、wire 形状与
//!   凭据纪律、三条错误矩阵。装置用 `AppState` 的**结构体字面量**（`channel_keys` 要按用例注入
//!   "配了 / 没配"；`AppState::new` 只会读进程 env）；
//! - **真库的那一半**（门 ⑥，`#[ignore]`，`MULTICA_TEST_DATABASE_URL`）：五条路由的配置态
//!   端到端行为 + [`store`] 三个端口实现的 SQL。未设置变量 ⇒ 打印跳过并 `return`；
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
use mc_channel::lark::installation::Installation;
use mc_channel::lark::registration::SessionStatus;
use mc_channel::lark::types::{OpenId, Region};

/// 不可达端口的懒连接池：不拨号、不建库（本文件只跑不需要库的那一半）。
const LAZY_URL: &str = "postgres://lark:lark@127.0.0.1:1/lark";

/// lark 的落库密钥（`MULTICA_LARK_SECRET_KEY` 的形态：base64 的 32 字节，`0x07` × 32）。
const SECRET_KEY_BASE64: &str = "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc=";

/// 一份**明文**哨兵：任何响应的正文或错误文案里出现它就失败（凭据纪律）。
const PLAINTEXT_SENTINEL: &str = "lark-app-secret-DO-NOT-LOG";

fn state_with(channel_keys: ChannelKeys) -> Arc<AppState> {
    state_with_db(
        channel_keys,
        mc_db::Db::connect_lazy(LAZY_URL, 1, 0).expect("lazy pool"),
    )
}

fn state_with_db(channel_keys: ChannelKeys, db: mc_db::Db) -> Arc<AppState> {
    let realtime = mc_realtime::RealtimeHandle::start(8);
    let ws = Arc::new(mc_realtime::WsState::new(realtime.clone(), "lark-tests"));
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

/// 配好 lark 落库密钥的整站装置。
fn configured() -> Router {
    mount(configured_state())
}

fn configured_state() -> Arc<AppState> {
    state_with(ChannelKeys::from_env_with(|name| {
        if name == "MULTICA_LARK_SECRET_KEY" {
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

/// 五条路由的 `(method, uri)`（未配置与未授权两族用例共用同一张表）。
fn the_five_routes(
    workspace_id: uuid::Uuid,
    installation_id: uuid::Uuid,
) -> Vec<(&'static str, String)> {
    let ws = workspace_id.to_string();
    vec![
        ("GET", format!("/api/workspaces/{ws}/lark/installations")),
        (
            "DELETE",
            format!("/api/workspaces/{ws}/lark/installations/{installation_id}"),
        ),
        (
            "POST",
            format!(
                "/api/workspaces/{ws}/lark/install/begin?agent_id={}",
                uuid::Uuid::new_v4()
            ),
        ),
        (
            "GET",
            format!("/api/workspaces/{ws}/lark/install/sess_abc/status"),
        ),
        ("POST", "/api/lark/binding/redeem".to_string()),
    ]
}

// =====================================================================
// ① 路由形态
// =====================================================================

#[test]
fn the_router_registers_the_five_upstream_paths_without_panicking() {
    // axum 0.7 同 path+method 重复注册会在 build 期 panic ⇒ 这一行就是"五条互不冲突"的判据。
    let _ = router();
}

// =====================================================================
// ② 未配置语义（逐端点不同）—— 这就是 ⑨ 的**七条 golden fixture**
// =====================================================================

/// 七条 fixture 的逐条复刻（`server/internal/handler/lark_test.go` 的 `status_expected`）。
///
/// `(method, path, expected)` 与 `crates/mc-conformance/report.json` 里路径含 `lark` 的
/// **恰好七条** 一一对应 —— 这条用例就是"本片把 ⑨ 从 `unmounted` 翻成 `pass`"的**可复现判据**
/// （未配置整站 + 匿名请求 ⇒ 逐条命中）。
#[tokio::test]
async fn the_seven_golden_fixtures_resolve_exactly_as_expected() {
    let app = unconfigured();
    let fixtures: [(&str, &str, StatusCode); 7] = [
        ("POST", "/api/lark/binding/redeem", StatusCode::FORBIDDEN),
        (
            "POST",
            "/api/workspaces/x/lark/install/begin?agent_id=y",
            StatusCode::FORBIDDEN,
        ),
        (
            "GET",
            "/api/workspaces/x/lark/install/sess_y/status",
            StatusCode::FORBIDDEN,
        ),
        (
            "GET",
            "/api/workspaces/x/lark/installations",
            StatusCode::OK,
        ),
        (
            "GET",
            "/api/workspaces/x/lark/installations",
            StatusCode::OK,
        ),
        (
            "GET",
            "/api/workspaces/x/lark/installations",
            StatusCode::OK,
        ),
        (
            "DELETE",
            "/api/workspaces/x/lark/installations/y",
            StatusCode::FORBIDDEN,
        ),
    ];
    for (method, path, expected) in fixtures {
        let (status, body) = call(&app, method, path, None).await;
        assert_eq!(status, expected, "{method} {path}: {body}");
        // 未配置分支**只**回那一个稳定码（列表那三条不查库、不读身份）。
        if status == StatusCode::FORBIDDEN {
            assert_eq!(
                body["error"]["code"],
                json!(dto::CODE_LARK_NOT_CONFIGURED),
                "{path}"
            );
        }
    }
}

#[tokio::test]
async fn the_unconfigured_semantics_are_per_endpoint() {
    let app = unconfigured();
    let user = uuid::Uuid::new_v4();
    let routes = the_five_routes(uuid::Uuid::new_v4(), uuid::Uuid::new_v4());

    // 列表：200 空 + 两个 `false`（**不**查库、**不**读身份 —— 匿名也给 200）。
    let (status, body) = call(&app, "GET", &routes[0].1, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["installations"], json!([]));
    assert_eq!(body["configured"], json!(false));
    assert_eq!(body["install_supported"], json!(false));

    // 其余四条：403 `lark_not_configured`（**不是**统一 503 —— 见模块头的 D11 口径更正）。
    for (method, uri) in &routes[1..] {
        let (status, body) = call(&app, method, uri, Some(user)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}: {body}");
        assert_eq!(
            body["error"]["code"],
            json!(dto::CODE_LARK_NOT_CONFIGURED),
            "{method} {uri}"
        );
    }
}

/// 只有 `MULTICA_LARK_SECRET_KEY` 能打开这五条路由：别的渠道的密钥配了不算数。
#[tokio::test]
async fn only_the_lark_deployment_key_enables_the_routes() {
    let state = state_with(ChannelKeys::from_env_with(|name| match name {
        "MULTICA_DINGTALK_SECRET_KEY"
        | "MULTICA_SLACK_SECRET_KEY"
        | "MULTICA_WECOM_SECRET_KEY"
        | "MULTICA_TELEGRAM_SECRET_KEY" => Some(SECRET_KEY_BASE64.to_string()),
        _ => None,
    }));
    assert!(!dto::is_configured(&state), "别的渠道配了不等于 lark 配了");
    let app = mount(state);
    let (status, body) = call(&app, "GET", "/api/workspaces/x/lark/installations", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["configured"], json!(false));
    let (status, body) = call(&app, "POST", "/api/lark/binding/redeem", None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

// =====================================================================
// ③ 配置好了但没身份 ⇒ 401（五条都先要会话）
// =====================================================================

#[tokio::test]
async fn every_route_requires_a_session_once_configured() {
    let app = configured();
    let routes = the_five_routes(uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
    for (method, uri) in routes {
        let (status, body) = call(&app, method, &uri, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}: {body}");
    }
}

// =====================================================================
// ④ wire 形状与凭据纪律
// =====================================================================

/// 一条安装的装置（密文列刻意用**不可能**被序列化出去的字面量）。
fn installation() -> Installation {
    Installation {
        id: Id(uuid::Uuid::new_v4()),
        workspace_id: Id(uuid::Uuid::new_v4()),
        agent_id: Id(uuid::Uuid::new_v4()),
        app_id: "cli_itest".to_string(),
        app_secret_encrypted: PLAINTEXT_SENTINEL.as_bytes().to_vec(),
        tenant_key: Some("tk_1".to_string()),
        bot_open_id: OpenId::new("ou_bot"),
        bot_union_id: None,
        region: Region::Lark,
        installer_user_id: Id(uuid::Uuid::new_v4()),
        status: "active".to_string(),
        installed_at: chrono::Utc::now(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

#[test]
fn the_installation_response_never_carries_the_ciphertext() {
    let row = installation();
    let rendered = serde_json::to_string(&LarkInstallationResponse::from_installation(&row))
        .expect("serialize");
    assert!(
        !rendered.contains(PLAINTEXT_SENTINEL),
        "响应里出现了密文: {rendered}"
    );
    assert!(!rendered.contains("app_secret"), "{rendered}");
    assert!(!rendered.contains("ws_lease"), "{rendered}");
    // `app_id` 与 `region` **在**：前者是管理台可见的标识，后者是 UI 的徽标依据。
    assert!(rendered.contains("cli_itest"), "{rendered}");
    assert!(rendered.contains("\"region\":\"lark\""), "{rendered}");
    assert!(
        rendered.contains("\"bot_open_id\":\"ou_bot\""),
        "{rendered}"
    );
    assert!(rendered.contains("\"tenant_key\":\"tk_1\""), "{rendered}");
}

#[test]
fn the_installation_response_omits_an_absent_tenant_key() {
    let mut row = installation();
    row.tenant_key = None;
    let rendered = serde_json::to_string(&LarkInstallationResponse::from_installation(&row))
        .expect("serialize");
    assert!(!rendered.contains("tenant_key"), "{rendered}");
}

#[test]
fn the_list_envelopes_are_the_upstream_three_cells() {
    // 未配置：一格不少、一格不多（`install_supported` 与 `configured` **同生同死**）。
    let blank = serde_json::to_value(LarkInstallationsResponse::not_configured()).expect("json");
    assert_eq!(
        blank,
        json!({
            "installations": [],
            "configured": false,
            "install_supported": false
        })
    );
    // 配置好但只有替身客户端 ⇒ `install_supported:false`（上游那条 StubClient 判据）。
    let stub = serde_json::to_value(LarkInstallationsResponse::configured_with(
        Vec::new(),
        false,
    ))
    .expect("json");
    assert_eq!(stub["configured"], json!(true));
    assert_eq!(stub["install_supported"], json!(false));
}

#[test]
fn the_status_response_carries_the_documented_keys_per_state() {
    let pending =
        LarkInstallStatusResponse::from_session(&session(SessionStatus::Pending, None, "", ""));
    assert_eq!(
        serde_json::to_value(&pending).expect("json"),
        json!({ "status": "pending" })
    );

    let success = LarkInstallStatusResponse::from_session(&session(
        SessionStatus::Success,
        Some(Id(uuid::Uuid::from_bytes([3; 16]))),
        "",
        "",
    ));
    let value = serde_json::to_value(&success).expect("json");
    assert_eq!(value["status"], json!("success"));
    assert_eq!(
        value["installation_id"],
        json!(uuid::Uuid::from_bytes([3; 16]).to_string())
    );
    assert!(value.get("error_reason").is_none(), "{value}");

    let failed = LarkInstallStatusResponse::from_session(&session(
        SessionStatus::Error,
        None,
        mc_channel::lark::registration::reason::ACCESS_DENIED,
        "registration: access_denied",
    ));
    let value = serde_json::to_value(&failed).expect("json");
    assert_eq!(value["status"], json!("error"));
    assert_eq!(value["error_reason"], json!("access_denied"));
    assert!(value.get("installation_id").is_none(), "{value}");
}

fn session(
    status: SessionStatus,
    installation_id: Option<Id>,
    reason_code: &str,
    message: &str,
) -> mc_channel::lark::registration::InstallSessionState {
    mc_channel::lark::registration::InstallSessionState {
        id: "sess_1".to_string(),
        workspace_id: Id(uuid::Uuid::new_v4()),
        initiator_id: Id(uuid::Uuid::new_v4()),
        status,
        installation_id,
        error_reason: reason_code.to_string(),
        error_message: message.to_string(),
        expires_at: chrono::Utc::now(),
    }
}

// =====================================================================
// ⑤ 三条错误矩阵
// =====================================================================

#[tokio::test]
async fn the_install_error_mapping_matches_the_upstream_matrix() {
    async fn body_of(error: InstallError) -> (StatusCode, serde_json::Value) {
        let response = install_error(&error);
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }
    for (error, expected) in [
        (InstallError::NotFound, StatusCode::NOT_FOUND),
        (
            InstallError::InvalidParams { field: "app_id" },
            StatusCode::BAD_REQUEST,
        ),
        (InstallError::OwnedByAnotherWorkspace, StatusCode::CONFLICT),
        (InstallError::OwnedByArchivedAgent, StatusCode::CONFLICT),
        (InstallError::OwnedBySameWorkspace, StatusCode::CONFLICT),
        (InstallError::ConflictUnclassified, StatusCode::CONFLICT),
        (InstallError::AlreadyAssigned, StatusCode::CONFLICT),
        (InstallError::NotWorkspaceMember, StatusCode::FORBIDDEN),
        (InstallError::Seal, StatusCode::INTERNAL_SERVER_ERROR),
        (
            InstallError::Store {
                message: "sqlstate 08006".to_string(),
            },
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    ] {
        let (status, body) = body_of(error.clone()).await;
        assert_eq!(status, expected, "{error}");
        // ⚠️ `NotFound` 那一格**故意**走本仓的标准 `Error::NotFound` 信封（wire 码是
        // `not_found`，全仓一致），而不是 adapter 自己那个更细的码 —— 其余九格逐字是
        // adapter 的稳定码（`docs/32` §30 的 D12）。
        let expected_code = if matches!(error, InstallError::NotFound) {
            "not_found"
        } else {
            error.code()
        };
        assert_eq!(body["error"]["code"], json!(expected_code), "{error}");
        // 错误正文里**没有**任何凭据/密文片段。
        let rendered = body.to_string();
        assert!(!rendered.contains(PLAINTEXT_SENTINEL), "{rendered}");
    }
}

#[tokio::test]
async fn the_binding_error_mapping_matches_the_upstream_switch() {
    async fn body_of(error: BindingError) -> (StatusCode, serde_json::Value) {
        let response = binding_error(&error);
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }
    for (error, expected, code) in [
        (
            BindingError::TokenInvalid,
            StatusCode::GONE,
            "lark_binding_token_invalid",
        ),
        (
            BindingError::AlreadyAssigned,
            StatusCode::CONFLICT,
            "lark_binding_already_assigned",
        ),
        (
            BindingError::NotWorkspaceMember,
            StatusCode::FORBIDDEN,
            "lark_binding_not_workspace_member",
        ),
        (
            BindingError::Store {
                message: "sqlstate 08006".to_string(),
            },
            StatusCode::INTERNAL_SERVER_ERROR,
            "lark_binding_failed",
        ),
    ] {
        let (status, body) = body_of(error.clone()).await;
        assert_eq!(status, expected, "{error}");
        assert_eq!(body["error"]["code"], json!(code), "{error}");
    }
}

// =====================================================================
// ⑥ 入参门（region / body / token）
// =====================================================================

#[test]
fn the_begin_region_gate_accepts_exactly_three_spellings() {
    for acceptable in ["", "  ", "feishu", "Lark", " LARK "] {
        let query = BeginInstallQuery {
            agent_id: "x".to_string(),
            region: acceptable.to_string(),
        };
        assert!(query.region_is_acceptable(), "{acceptable:?}");
    }
    for rejected in ["slack", "feishulark", "lark1"] {
        let query = BeginInstallQuery {
            agent_id: "x".to_string(),
            region: rejected.to_string(),
        };
        assert!(!query.region_is_acceptable(), "{rejected:?}");
    }
}

#[test]
fn the_redeem_body_defaults_an_absent_token() {
    let body: RedeemLarkBindingTokenRequest = serde_json::from_value(json!({})).expect("decode");
    assert_eq!(body.token, "");
    let body: RedeemLarkBindingTokenRequest =
        serde_json::from_value(json!({ "token": "abc" })).expect("decode");
    assert_eq!(body.token, "abc");
}

#[test]
fn the_begin_query_defaults_both_fields() {
    let query: BeginInstallQuery = serde_json::from_value(json!({})).expect("decode");
    assert_eq!(query.agent_id, "");
    assert_eq!(query.region, "");
}

#[test]
fn the_body_limit_is_the_upstream_sixteen_kilobytes() {
    // 两个 JSON 端点都是几个短字段；上限防的是"已认证的调用方 POST 一个几 GB 的 token"。
    assert_eq!(dto::BODY_LIMIT, 16 * 1024);
}

// =====================================================================
// ⑦ 真库的那一半（门 ⑥，`MULTICA_TEST_DATABASE_URL`）
// =====================================================================

// 真库那一半（门 ⑥）：`lark/tests/{support,db}.rs`。
mod db;
mod support;
