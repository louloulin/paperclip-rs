//! composio 面端到端测试的共用件（M8-6 / `LUM-1803`；与 `tests/vcs/` / `tests/github/` 同手法）。
//!
//! - **真库**：`"user"` / `user_composio_connection` 必须已迁移（门 ⑥ 先跑
//!   `mc-migrate run --dir migrations`）。
//! - **离线替身**：Composio 侧全部由 [`stub_base`] 起的**一个**本地 axum 服务承担。注入点是
//!   `set_composio_api_base`（生产代码里那个进程级槽），**行为按请求的 `x-api-key` 分派**
//!   ⇒ 一个 base 服务整个测试二进制，用例之间**没有**「谁把 base 改成什么」的竞态
//!   （每个用例给自己的 `AppState` 一枚不同的 key，见 [`keys`]）。
//! - `AppState` 用**结构体字面量**构造：`composio_keys` 与 `feature_flags` 要按用例注入
//!   「四条件 × flag」的每个象限（`AppState::new` 只读进程 env）。字段全 `pub`，
//!   不需要改任何锚点文件。
//! - **凭据**：`STATE_SECRET` 是测试专用值；库里的行**不含** bearer（连接表本来就只存外部标识）。

use std::sync::Arc;
use std::sync::Mutex;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::{Json, Router};
use http_body_util::BodyExt;
use mc_core::actor::ActorRegistry;
use mc_db::Db;
use mc_feature_flags::{FeatureFlagCatalog, FeatureKey};
use mc_http::routes::composio::connect::{set_composio_api_base, COMPOSIO_MCP_APPS_FLAG};
use mc_http::state::integrations::ComposioKeys;
use mc_http::state::{AdapterRegistry, AppState, ConfigSnapshot, RuntimeHandles};
use mc_realtime::{RealtimeHandle, WsState};
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

pub(crate) const USER_ID_HEADER: &str = "x-multica-user-id";

/// 测试专用的 state secret（**不是**生产密钥）。
pub(crate) const STATE_SECRET: &str = "composio-test-state-secret";

/// 生产口径的回调基址（`begin_connect` 用它拼回调；本测试里**没有**服务端在听它）。
pub(crate) const CALLBACK_BASE: &str = "https://api.example.test";

// ---------------------------------------------------------------------------
// 替身的「行为开关」：按 x-api-key 分派
// ---------------------------------------------------------------------------

/// 一切正常：目录里有 notion（有 auth config）+ github（没有）。
pub(crate) const VARIANT_OK: &str = "ok";
/// 项目里**没有**任何启用的 auth config ⇒ 目录空、connect ⇒ 400 toolkit not supported。
pub(crate) const VARIANT_NO_CONFIGS: &str = "noconf";
/// `/auth_configs` 回 500 ⇒ 目录 / toolkits 走 502。
pub(crate) const VARIANT_BOOM: &str = "boom";
/// 账号归属复核失败（账号属于别人）⇒ callback 走失败重定向且**不落库**；
/// `/connected_accounts/{id}` 也回 404（考客户端把 404 当成功）。
pub(crate) const VARIANT_FOREIGN: &str = "foreign";

/// 本用例的 API key 形态：`ak_<variant>_<uuid>`。
///
/// ⚠️ 它**同时**承担两件事：① 告诉替身「按哪个行为答」；② 把**调用者是谁**带过去
/// —— 替身要回一个 `user_id` 给账号归属复核，而回调的身份来自 signed state（不是会话），
/// 所以这个 id 只能随 key 走。这样每个用例都有自己的 key + 自己的 user ⇒ **零进程级状态**，
/// 并行跑也互不干扰。
pub(crate) fn api_key_for(variant: &str, user_id: Uuid) -> String {
    format!("ak_{variant}_{user_id}")
}

/// 从 API key 里解出 `(variant, user_id)`（形状不对 ⇒ `("ok", "")`）。
pub(crate) fn parse_api_key(key: &str) -> (String, String) {
    let Some(rest) = key.strip_prefix("ak_") else {
        return (VARIANT_OK.to_string(), String::new());
    };
    match rest.split_once('_') {
        Some((variant, user)) => (variant.to_string(), user.to_string()),
        None => (rest.to_string(), String::new()),
    }
}

/// 替身侧的调用记录（断言出站 wire 用）。
#[derive(Debug, Clone)]
pub(crate) struct StubCall {
    pub method: String,
    pub path_and_query: String,
    pub api_key: Option<String>,
    pub body: String,
}

static CALLS: Mutex<Vec<StubCall>> = Mutex::new(Vec::new());

/// 读回替身侧的调用记录（拷贝）。
pub(crate) fn calls() -> Vec<StubCall> {
    CALLS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// 本用例的 key 对应的那几笔调用（路径子串匹配，便于按阶段断言）。
pub(crate) fn calls_for(key: &str, path_contains: &str) -> Vec<StubCall> {
    calls()
        .into_iter()
        .filter(|call| {
            call.api_key.as_deref() == Some(key) && call.path_and_query.contains(path_contains)
        })
        .collect()
}

/// **本用例自己**的那几笔调用 —— 按 key 里带的 user id 认领，**不看**进程级的先后顺序。
///
/// ⚠️ 为什么不能按「全局最后一条」读：`CALLS` 是**进程级**静态，而同一个测试二进制里的
/// 用例是**并行**跑的（一条 `#[tokio::test]` 一个 runtime，但共享进程）⇒ `calls()` 里
/// 混着别的用例的调用，「最后一条」可能属于别人（`docs/32` §36.1/§36.3 的实测现场）。
/// 每个用例用自己 `seed_user` 出来的 uuid 造 key（`ak_<variant>_<uuid>`），key 的 user 段
/// 因此是全进程唯一的判据 ⇒ 按它过滤是零竞态的，也**不需要**任何清场（曾经的
/// `reset_calls()` 反而是害：它按进程级粒度清，会连带清掉并行用例已发出、还没读回的记录）。
pub(crate) fn calls_for_user(user: Uuid, path_contains: &str) -> Vec<StubCall> {
    let owner = user.to_string();
    calls()
        .into_iter()
        .filter(|call| {
            call.api_key
                .as_deref()
                .is_some_and(|key| parse_api_key(key).1 == owner)
                && call.path_and_query.contains(path_contains)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 替身服务端（一个进程一个 base）
// ---------------------------------------------------------------------------

static STUB_BASE: tokio::sync::OnceCell<String> = tokio::sync::OnceCell::const_new();

/// 起（或复用）替身，并把 base 注入 [`set_composio_api_base`]。
///
/// ⚠️ 注入**只做一次**（`OnceCell`）⇒ 整个二进制共享一个 base，用例之间没有竞态。
///
/// ⚠️ 替身跑在**自己的 OS 线程 + 自己的 runtime** 上（不是 `tokio::spawn` 到用例的运行时）：
/// `#[tokio::test]` **每个用例**建一个 runtime，用例一结束 runtime 就被丢掉 ⇒ 挂在它上面的
/// 服务任务会跟着死，后面的用例就变成「连接被拒」。线程里的 runtime 活到进程结束。
pub(crate) async fn stub_base() -> String {
    STUB_BASE
        .get_or_init(|| async {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind composio stub");
            listener
                .set_nonblocking(true)
                .expect("stub listener is non-blocking");
            let port = listener.local_addr().expect("stub addr").port();
            std::thread::Builder::new()
                .name("composio-stub".to_string())
                .spawn(move || {
                    let runtime = tokio::runtime::Builder::new_multi_thread()
                        .enable_all()
                        .build()
                        .expect("stub runtime");
                    runtime.block_on(async move {
                        let listener =
                            tokio::net::TcpListener::from_std(listener).expect("tokio listener");
                        let _ = axum::serve(listener, stub_router()).await;
                    });
                })
                .expect("spawn stub thread");
            let base = format!("http://127.0.0.1:{port}");
            set_composio_api_base(&base);
            base
        })
        .await
        .clone()
}

/// 替身路由：**只**替平台 wire（`docs/61` §4.2 的替身纪律 ①），行为按 `x-api-key` 分派。
fn stub_router() -> Router {
    Router::new()
        .route("/toolkits", axum::routing::get(stub_toolkits))
        .route("/auth_configs", axum::routing::get(stub_auth_configs))
        .route(
            "/connected_accounts/link",
            axum::routing::post(stub_create_link),
        )
        .route(
            "/connected_accounts",
            axum::routing::get(stub_list_accounts),
        )
        .route(
            "/connected_accounts/:id/revoke",
            axum::routing::post(stub_revoke),
        )
        .route(
            "/connected_accounts/:id",
            axum::routing::delete(stub_delete_account),
        )
        .route(
            "/tool_router/session",
            axum::routing::post(stub_create_session),
        )
}

/// 记录一笔调用（`State<Arc<…>>` 用不上：`CALLS` 是进程级静态）。
fn record(method: &str, uri: &str, headers: &axum::http::HeaderMap, body: &str) -> Option<String> {
    let api_key = headers
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    CALLS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(StubCall {
            method: method.to_string(),
            path_and_query: uri.to_string(),
            api_key: api_key.clone(),
            body: body.to_string(),
        });
    api_key
}

async fn body_text(body: Body) -> String {
    body.collect()
        .await
        .map(|collected| String::from_utf8_lossy(&collected.to_bytes()).to_string())
        .unwrap_or_default()
}

async fn stub_toolkits(req: Request<Body>) -> (StatusCode, Json<Value>) {
    record("GET", &uri_of(&req), req.headers(), "");
    let base = stub_base_cached();
    (
        StatusCode::OK,
        Json(json!({
            "items": [
                {"slug": "notion", "name": "Notion", "logo": format!("{base}/logos/notion"),
                 "categories": ["productivity"]},
                {"slug": "github", "name": "GitHub", "categories": []},
                {"slug": "", "name": "no slug"},
            ],
            "next_cursor": "",
        })),
    )
}

async fn stub_auth_configs(req: Request<Body>) -> (StatusCode, Json<Value>) {
    let key = record("GET", &uri_of(&req), req.headers(), "");
    let variant = key
        .as_deref()
        .map_or(String::new(), |key| parse_api_key(key).0);
    match variant.as_str() {
        VARIANT_BOOM => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": {"message": "boom", "slug": "INTERNAL"}})),
        ),
        VARIANT_NO_CONFIGS => (
            StatusCode::OK,
            Json(json!({"items": [], "next_cursor": ""})),
        ),
        // 一页里两条 notion 的 auth config（自定义 + 托管）⇒ 归约要挑出自定义那条。
        _ => (
            StatusCode::OK,
            Json(json!({
                "items": [
                    {"id": "ac_notion_managed", "toolkit": {"slug": "notion"},
                     "is_composio_managed": true, "status": "ENABLED",
                     "last_updated_at": "2026-09-01T00:00:00Z"},
                    {"id": "ac_notion_custom", "toolkit": {"slug": "notion"},
                     "is_composio_managed": false, "status": "ENABLED",
                     "last_updated_at": "2026-01-01T00:00:00Z"},
                    {"id": "ac_disabled", "toolkit": {"slug": "github"},
                     "is_composio_managed": false, "status": "DISABLED",
                     "last_updated_at": "2026-09-09T00:00:00Z"},
                ],
                "next_cursor": "",
            })),
        ),
    }
}

async fn stub_create_link(req: Request<Body>) -> (StatusCode, Json<Value>) {
    let (parts, body) = req.into_parts();
    let text = body_text(body).await;
    record(
        "POST",
        &parts.uri.to_string(),
        &parts.headers,
        text.as_str(),
    );
    let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let user = parsed["user_id"].as_str().unwrap_or_default().to_string();
    (
        StatusCode::OK,
        Json(json!({
            "redirect_url": format!("https://composio.example/link/{user}"),
            "link_token": "lt_1",
            "connected_account_id": "ca_pending",
            "expires_at": "2026-09-25T11:00:00Z",
        })),
    )
}

async fn stub_list_accounts(req: Request<Body>) -> (StatusCode, Json<Value>) {
    let key = record("GET", &uri_of(&req), req.headers(), "");
    let requested = query_param(&uri_of(&req), "connected_account_ids").unwrap_or_default();
    let (variant, user) = key
        .as_deref()
        .map_or((String::new(), String::new()), parse_api_key);
    let owner = if variant == VARIANT_FOREIGN {
        "someone-else".to_string()
    } else {
        user
    };
    (
        StatusCode::OK,
        Json(json!({
            "items": [{
                "id": requested,
                "user_id": owner,
                "auth_config_id": "",
                "auth_config": {"id": "ac_notion_custom"},
                "toolkit": {"slug": "notion"},
                "status": "ACTIVE",
            }],
        })),
    )
}

async fn stub_revoke(req: Request<Body>) -> StatusCode {
    record("POST", &uri_of(&req), req.headers(), "");
    StatusCode::OK
}

async fn stub_delete_account(req: Request<Body>) -> StatusCode {
    let key = record("DELETE", &uri_of(&req), req.headers(), "");
    // 404 路径：`VARIANT_FOREIGN` 的那枚 key 让它回 404（客户端必须把它当成功）。
    if key
        .as_deref()
        .is_some_and(|key| parse_api_key(key).0 == VARIANT_FOREIGN)
    {
        return StatusCode::NOT_FOUND;
    }
    StatusCode::NO_CONTENT
}

async fn stub_create_session(req: Request<Body>) -> (StatusCode, Json<Value>) {
    let (parts, body) = req.into_parts();
    let text = body_text(body).await;
    record(
        "POST",
        &parts.uri.to_string(),
        &parts.headers,
        text.as_str(),
    );
    (
        StatusCode::OK,
        Json(json!({
            "session_id": "sess_1",
            "mcp": {"type": "http", "url": "https://mcp.example/s/sess_1"},
        })),
    )
}

fn uri_of(req: &Request<Body>) -> String {
    req.uri().to_string()
}

/// 从 `uri` 的 query 里取一个参数（替身自己解析，省掉一个 `Query` 提取器 —— 与
/// `Request<Body>` 不能共存：后者必须是最后一个提取器，而且我们已经用它拿原始 body）。
fn query_param(uri: &str, name: &str) -> Option<String> {
    let query = uri.split_once('?')?.1;
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=')?;
        if key == name {
            return Some(value.to_string());
        }
    }
    None
}

/// 替身自己的 base（给 logo URL 之类的绝对地址用；未起 ⇒ 空串）。
fn stub_base_cached() -> String {
    STUB_BASE.get().cloned().unwrap_or_default()
}

// ---------------------------------------------------------------------------
// 应用状态 / 连接
// ---------------------------------------------------------------------------

/// 按 `AppState` 的四条件造 `ComposioKeys`（`from_env_with` 是唯一的注入点）。
///
/// ⚠️ 它是 `async` 的**唯一**原因：先 [`stub_base`] 确保替身起好并注入 `api_base`
/// （注入只发生一次；不先起替身的话，客户端会去撞真的 `backend.composio.dev`）。
pub(crate) async fn keys(
    api_key: Option<&str>,
    with_secret: bool,
    with_base: bool,
) -> ComposioKeys {
    stub_base().await;
    ComposioKeys::from_env_with(|name| match name {
        "COMPOSIO_API_KEY" => api_key.map(str::to_string),
        "COMPOSIO_STATE_SECRET" => with_secret.then(|| STATE_SECRET.to_string()),
        "COMPOSIO_CALLBACK_BASE_URL" => with_base.then(|| CALLBACK_BASE.to_string()),
        _ => None,
    })
}

/// 四条件齐的生产口径组合（flag 另外传）：`ak_<variant>_<user>` + secret + 回调基址。
pub(crate) async fn configured_keys(variant: &str, user_id: Uuid) -> ComposioKeys {
    keys(Some(api_key_for(variant, user_id).as_str()), true, true).await
}

pub(crate) fn app_with(db: Db, keys: ComposioKeys, flag_on: bool) -> Router {
    let state = build_state(db, keys, flag_on);
    mc_http::routes::router(state.clone()).with_state(state)
}

/// 显式 `composio_keys` + feature flag 的 `AppState`（字段字面量，与 `tests/vcs/support.rs` 同款）。
pub(crate) fn build_state(db: Db, composio_keys: ComposioKeys, flag_on: bool) -> Arc<AppState> {
    let realtime = RealtimeHandle::start(8);
    let ws = Arc::new(WsState::new(realtime.clone(), "multica-rs-test"));
    let feature_flags = FeatureFlagCatalog::new();
    feature_flags.register(&FeatureKey::new(COMPOSIO_MCP_APPS_FLAG), flag_on, None);
    Arc::new(AppState {
        db,
        runtime: RuntimeHandles {
            actors: ActorRegistry::new(),
            adapters: Arc::new(AdapterRegistry::default()),
        },
        config: ConfigSnapshot {
            host: "127.0.0.1".into(),
            port: 0,
            session_cookie: "multica_session".into(),
            api_key_header: "X-Multica-Api-Key".into(),
            csrf_header: "X-Multica-Csrf".into(),
            invitation_per_workspace_per_hour: Some(50),
            ..ConfigSnapshot::default()
        },
        storage: mc_storage::Storage::new(),
        secrets: mc_secrets::Secrets::new(mc_auth::DefaultSecretsBackend::in_memory()),
        feature_flags: Arc::new(feature_flags),
        realtime,
        ws,
        auth: mc_auth::SessionStoreContainer::new(),
        pat: mc_auth::PatStoreContainer::new(),
        verification: mc_auth::VerificationStoreContainer::new(),
        google_oauth: mc_http::state::GoogleOAuthConfig::default(),
        daemon_hub: Arc::new(mc_ws::hub::Hub::new()),
        daemon_requests: Arc::new(mc_http::daemon_requests::RequestStore::new()),
        plugin_key: None,
        plugin_surface_origin: None,
        channel_keys: mc_http::state::ChannelKeys::default(),
        github_keys: mc_http::state::integrations::GithubKeys::default(),
        vcs_keys: mc_http::state::integrations::VcsKeys::default(),
        composio_keys,
    })
}

/// `MULTICA_TEST_DATABASE_URL` 缺失 → `None`（用例打印跳过并 return）；
/// **设了却连不上 → panic**（库坏了必须红，不能静默跳过假装绿）。
pub(crate) async fn connect() -> Option<(PgPool, Db)> {
    let url = std::env::var("MULTICA_TEST_DATABASE_URL").ok()?;
    let pool = sqlx::PgPool::connect(&url)
        .await
        .unwrap_or_else(|e| panic!("MULTICA_TEST_DATABASE_URL is set but connect failed: {e}"));
    let db = Db::from_pool(pool.clone());
    Some((pool, db))
}

// ---------------------------------------------------------------------------
// 种子 / 清场
// ---------------------------------------------------------------------------

/// 一个用户（composio 的连接**属于用户**，不需要 workspace）。
pub(crate) async fn seed_user(pool: &PgPool) -> Uuid {
    sqlx::query_scalar(r#"INSERT INTO "user"(name, email) VALUES ('itest-m86', $1) RETURNING id"#)
        .bind(format!("m86-{}@example.com", Uuid::new_v4()))
        .fetch_one(pool)
        .await
        .expect("insert user")
}

/// 库里的连接行（直接查，绕开路由）。
pub(crate) async fn connection_rows(pool: &PgPool, user_id: Uuid) -> Vec<Value> {
    let rows: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT toolkit_slug, auth_config_id, connected_account_id, status \
         FROM user_composio_connection WHERE user_id = $1 ORDER BY created_at ASC",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .expect("select connections");
    rows.into_iter()
        .map(|(slug, auth, account, status)| {
            json!({"toolkit_slug": slug, "auth_config_id": auth,
                   "connected_account_id": account, "status": status})
        })
        .collect()
}

/// 清场：这 1 张表**没有 FK**（迁移 `127`），所以子行必须显式删；`"user"` 也删。
pub(crate) async fn cleanup(pool: &PgPool, user_id: Uuid) {
    let _ = sqlx::query("DELETE FROM user_composio_connection WHERE user_id = $1")
        .bind(user_id)
        .execute(pool)
        .await;
    let _ = sqlx::query(r#"DELETE FROM "user" WHERE id = $1"#)
        .bind(user_id)
        .execute(pool)
        .await;
}

// ---------------------------------------------------------------------------
// 请求 / 响应
// ---------------------------------------------------------------------------

pub(crate) fn req(method: &str, uri: &str, user_id: Option<Uuid>) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(user_id) = user_id {
        builder = builder.header(USER_ID_HEADER, user_id.to_string());
    }
    builder.body(Body::empty()).expect("request")
}

pub(crate) fn req_json(
    method: &str,
    uri: &str,
    user_id: Option<Uuid>,
    body: &Value,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(user_id) = user_id {
        builder = builder.header(USER_ID_HEADER, user_id.to_string());
    }
    builder.body(Body::from(body.to_string())).expect("request")
}

/// 发一次请求，返回 `(status, json, location, raw)`。
pub(crate) async fn send(
    app: &Router,
    request: Request<Body>,
) -> (StatusCode, Value, Option<String>, String) {
    let response = app.clone().oneshot(request).await.expect("router call");
    let status = response.status();
    let location = response
        .headers()
        .get(axum::http::header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let raw = String::from_utf8_lossy(&bytes).to_string();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, json, location, raw)
}

/// 不带 body 的调用（GET / DELETE）。
pub(crate) async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    user_id: Option<Uuid>,
) -> (StatusCode, Value, Option<String>, String) {
    send(app, req(method, uri, user_id)).await
}

/// 走一遍 `connect/init` 并把**真流程产出**的 signed state 交回调用者
/// （e2e 用例里要拿它去回调，避免测试自己签 state）。
///
/// ⚠️ 取调用的方式按 [`calls_for_user`]：**按自己的 user id 认领**，不是
/// 「`api_keys.last()`」。曾经的 `last()` 在并行下会拿到**别的用例**的 key ⇒ 拿到别人那份
/// state ⇒ 回调把行写进别人的用户名下（本用例看到 0 行），而自己的 state 被偷走后又被
/// 对方消费 ⇒ 后来者落 `401 composio_state_invalid`（`docs/32` §36.1/§36.3）。
pub(crate) async fn flow_state(app: &Router, user: Uuid) -> String {
    let (status, _, _, raw) = send(
        app,
        req_json(
            "POST",
            "/api/integrations/composio/connect/init",
            Some(user),
            &json!({"toolkit_slug": "notion"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "connect init failed: {raw}");
    let link = calls_for_user(user, "/connected_accounts/link")
        .pop()
        .expect("a link call");
    let body: Value = serde_json::from_str(&link.body).expect("link body");
    body["callback_url"]
        .as_str()
        .expect("callback_url")
        .split("state=")
        .nth(1)
        .expect("state in callback url")
        .to_string()
}

/// 错误信封里的稳定 `code`（响应体不是信封形状时回 `<none>`）。
pub(crate) fn error_code(body: &Value) -> String {
    body["error"]["code"]
        .as_str()
        .unwrap_or("<none>")
        .to_string()
}
