//! `/api/workspaces/{id}/slack/*` 与 `POST /api/slack/binding/redeem` 端到端测试的共用件
//! （M7-4 / `LUM-1769`；与 `tests/github/` / `tests/plugins/` 同手法）。
//!
//! - **真库**：`workspace` / `member` / `"user"` / `agent_runtime` / `agent` /
//!   `channel_installation` / `channel_binding_token` / `channel_user_binding` 必须已迁移
//!   （门 ⑥ 先跑 `mc-migrate run --dir migrations`）。
//! - **离线替身**：Slack 侧全部由 [`serve_stub`] 起的本地 axum 服务承担；注入点是
//!   [`mc_channel::slack::outbound::set_api_base`]（与 M8-1 的 `set_github_api_base` 同款）。
//!   它是**进程全局**的 ⇒ 依赖它的用例必须串行（[`STUB_LOCK`]）。
//! - `AppState` 用**结构体字面量**构造（`channel_keys` 要按用例注入"配了 / 没配"；
//!   `AppState::new` 只会读进程 env —— 与 `tests/github/support.rs` 的构造点同款）。
//!   字段全 `pub`，不需要改锚点文件。

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use mc_core::actor::ActorRegistry;
use mc_db::Db;
use mc_http::state::integrations::{ComposioKeys, GithubKeys, VcsKeys};
use mc_http::state::{AdapterRegistry, AppState, ChannelKeys, ConfigSnapshot, RuntimeHandles};
use mc_realtime::{RealtimeHandle, WsState};
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

pub(crate) const USER_ID_HEADER: &str = "x-multica-user-id";

/// Slack 的落库密钥（`MULTICA_SLACK_SECRET_KEY` 的形态：base64 的 32 字节）。
pub(crate) const SECRET_KEY_BASE64: &str = "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc=";

/// 注入 base 这件事是进程全局的 ⇒ 所有用替身的用例串行。
pub(crate) static STUB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

// ---------------------------------------------------------------------------
// 应用状态 / 连接
// ---------------------------------------------------------------------------

/// 用显式 `channel_keys` 造 `AppState`（字段字面量，与 `tests/github/support.rs` 同款）。
pub(crate) fn build_state(db: Db, channel_keys: ChannelKeys) -> Arc<AppState> {
    let realtime = RealtimeHandle::start(8);
    let ws = Arc::new(WsState::new(realtime.clone(), "multica-rs-test"));
    let actors = ActorRegistry::new();
    let adapters = Arc::new(AdapterRegistry::default());
    Arc::new(AppState {
        db,
        runtime: RuntimeHandles { actors, adapters },
        config: ConfigSnapshot {
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
        google_oauth: mc_http::state::GoogleOAuthConfig::default(),
        daemon_hub: Arc::new(mc_ws::hub::Hub::new()),
        daemon_requests: Arc::new(mc_http::daemon_requests::RequestStore::new()),
        plugin_key: None,
        plugin_surface_origin: None,
        channel_keys,
        github_keys: GithubKeys::default(),
        vcs_keys: VcsKeys::default(),
        composio_keys: ComposioKeys::default(),
    })
}

pub(crate) fn app_with(db: Db, channel_keys: ChannelKeys) -> Router {
    let state = build_state(db, channel_keys);
    mc_http::routes::router(state.clone()).with_state(state)
}

/// 配好 Slack 落库密钥的那一份（`MULTICA_SLACK_SECRET_KEY` 是唯一判据）。
pub(crate) fn configured_keys() -> ChannelKeys {
    ChannelKeys::from_env_with(|name| {
        if name == "MULTICA_SLACK_SECRET_KEY" {
            Some(SECRET_KEY_BASE64.to_string())
        } else {
            None
        }
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
// 种子
// ---------------------------------------------------------------------------

/// `(workspace_id, agent_id, admin_user_id, member_user_id, guest_user_id, outsider_user_id)`
pub(crate) struct Seed {
    pub workspace_id: Uuid,
    pub agent_id: Uuid,
    pub admin: Uuid,
    pub member: Uuid,
    pub guest: Uuid,
    pub outsider: Uuid,
}

/// 造一个 workspace + 三个成员（owner/admin、member、guest）+ 一个 outsider + 一个 agent。
pub(crate) async fn seed(pool: &PgPool) -> Seed {
    let workspace_id: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-m74-slack', $1) RETURNING id",
    )
    .bind(format!("itest-m74-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert workspace");
    let admin = seed_user(pool, workspace_id, "admin").await;
    let member = seed_user(pool, workspace_id, "member").await;
    let guest = seed_user(pool, workspace_id, "guest").await;
    let outsider: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-m74-outsider', $1) RETURNING id"#,
    )
    .bind(format!("m74-out-{}@example.com", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert outsider");
    let runtime_id: Uuid = sqlx::query_scalar(
        "INSERT INTO agent_runtime(workspace_id, name, runtime_mode, provider, status, owner_id) \
         VALUES ($1, $2, 'local', 'claude_code', 'online', $3) RETURNING id",
    )
    .bind(workspace_id)
    .bind(format!("itest-m74-rt-{}", Uuid::new_v4()))
    .bind(admin)
    .fetch_one(pool)
    .await
    .expect("insert agent_runtime");
    let agent_id: Uuid = sqlx::query_scalar(
        "INSERT INTO agent(workspace_id, name, runtime_mode, runtime_id, owner_id, kind) \
         VALUES ($1, $2, 'local', $3, $4, 'user') RETURNING id",
    )
    .bind(workspace_id)
    .bind(format!("itest-m74-agent-{}", Uuid::new_v4()))
    .bind(runtime_id)
    .bind(admin)
    .fetch_one(pool)
    .await
    .expect("insert agent");
    Seed {
        workspace_id,
        agent_id,
        admin,
        member,
        guest,
        outsider,
    }
}

pub(crate) async fn seed_user(pool: &PgPool, workspace_id: Uuid, role: &str) -> Uuid {
    let user_id: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-m74-user', $1) RETURNING id"#,
    )
    .bind(format!("m74-{}@example.com", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert user");
    sqlx::query("INSERT INTO member(workspace_id, user_id, role) VALUES ($1, $2, $3)")
        .bind(workspace_id)
        .bind(user_id)
        .bind(role)
        .execute(pool)
        .await
        .expect("insert member");
    user_id
}

/// 清场：**显式**删三张渠道表再删 workspace（渠道表的 FK 没有级联 ⇒ 不删就会留在库里，
/// 让下一次运行的 `(slack, app_id)` 路由槽被"上一个 workspace 的死主"占着）。
pub(crate) async fn cleanup(pool: &PgPool, seed: &Seed) {
    for sql in [
        "DELETE FROM channel_user_binding WHERE installation_id IN \
         (SELECT id FROM channel_installation WHERE workspace_id = $1)",
        "DELETE FROM channel_binding_token WHERE workspace_id = $1",
        "DELETE FROM channel_installation WHERE workspace_id = $1",
    ] {
        let _ = sqlx::query(sql).bind(seed.workspace_id).execute(pool).await;
    }
    let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
        .bind(seed.workspace_id)
        .execute(pool)
        .await;
    for user_id in [seed.admin, seed.member, seed.guest, seed.outsider] {
        let _ = sqlx::query(r#"DELETE FROM "user" WHERE id = $1"#)
            .bind(user_id)
            .execute(pool)
            .await;
    }
}

// ---------------------------------------------------------------------------
// 请求
// ---------------------------------------------------------------------------

pub(crate) fn req(
    method: &str,
    uri: &str,
    user_id: Option<Uuid>,
    body: Option<Value>,
) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(user_id) = user_id {
        builder = builder.header(USER_ID_HEADER, user_id.to_string());
    }
    match body {
        Some(body) => builder
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("request"),
        None => builder.body(Body::empty()).expect("request"),
    }
}

/// 发一次请求，返回 `(status, json)`。
pub(crate) async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    user_id: Option<Uuid>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(req(method, uri, user_id, body))
        .await
        .expect("router call");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, json)
}

// ---------------------------------------------------------------------------
// 离线替身（Slack 侧）
// ---------------------------------------------------------------------------

/// 起一个只服务本用例的 Slack 替身（端口由内核分配），返回 `http://127.0.0.1:{port}`。
///
/// 路由就是 Slack 的真实方法名（`/auth.test` / `/bots.info` / `/apps.connections.open`），
/// **中间零 mock**：`tests/channels/slack.rs` 里断言的就是我们发出去的请求体。
pub(crate) async fn serve_stub(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind slack stub");
    let port = listener.local_addr().expect("stub addr").port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://127.0.0.1:{port}")
}

/// 一个"全都通过"的 Slack 替身：`auth.test` → `(T1, UBOT, B1)`，`bots.info` → app id，
/// `apps.connections.open` → 一个不可达但**形状合法**的 `wss://` URL（我们只验证 ok）。
pub(crate) fn consistent_stub(app_id: String) -> Router {
    Router::new()
        .route(
            "/auth.test",
            axum::routing::post(|| async {
                axum::Json(serde_json::json!({
                    "ok": true,
                    "team_id": "T1",
                    "user_id": "UBOT",
                    "bot_id": "B1",
                }))
            }),
        )
        .route(
            "/bots.info",
            axum::routing::post(move || {
                let app_id = app_id.clone();
                async move {
                    axum::Json(serde_json::json!({
                        "ok": true,
                        "bot": { "id": "B1", "app_id": app_id },
                    }))
                }
            }),
        )
        .route(
            "/apps.connections.open",
            axum::routing::post(|| async {
                axum::Json(serde_json::json!({
                    "ok": true,
                    "url": "wss://stub.invalid/link/?ticket=never-used",
                }))
            }),
        )
}
