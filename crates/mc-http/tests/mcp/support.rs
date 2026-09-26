//! MCP 面端到端测试的共用件（M8-3 / `LUM-1800`；与 `tests/vcs/` / `tests/skills/` 同手法）。
//!
//! - **真库**：`workspace` / `member` / `"user"` / `agent` / `workspace_mcp_server` /
//!   `agent_mcp_server` 必须已迁移（门 ⑥ 先跑 `mc-migrate run --dir migrations`）。
//! - **无替身**：这一面全是本地库语义，**不需要**任何 HTTP 替身（`docs/61` §4.2 的 MCP 行）。
//! - `AppState` 用**结构体字面量**构造（与 `tests/vcs/support.rs` 同款）：MCP 面不读任何部署
//!   密钥 ⇒ 三组密钥全部走 `Default`，但字段仍要逐一点名（字段全 `pub`，不必改锚点文件）。

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
/// agent 面**不带** workspace 在路径里（`/api/agents/{id}/...`）⇒ workspace 走这个头
/// （本仓 `resolve_workspace_id` 的取值顺序：头 → `?workspace_id=`）。
pub(crate) const WORKSPACE_HEADER: &str = "x-workspace-id";

// ---------------------------------------------------------------------------
// 应用状态 / 连接
// ---------------------------------------------------------------------------

/// 无密钥的 `AppState`（MCP 面不读部署密钥 ⇒ 三组 `Default`）。
pub(crate) fn build_state(db: Db) -> Arc<AppState> {
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
        channel_keys: ChannelKeys::default(),
        github_keys: GithubKeys::default(),
        vcs_keys: VcsKeys::default(),
        composio_keys: ComposioKeys::default(),
        // M9 anchor（LUM-1815）：云面两组字段显式未配置（口径见 `state/cloud.rs`）。AppState 的字面量构造点**全部**在这里补，因为它们不用 `..Default::default()`（见 docs/32 §9.13 的写集扩展登记）。
        cloud: mc_http::state::cloud::CloudConfig::from_env_with(|_| None),
        entitlement: mc_http::state::cloud::EntitlementConfig::from_env_with(|_| None),
    })
}

pub(crate) fn app_with(db: Db) -> Router {
    let state = build_state(db);
    mc_http::routes::router(state.clone()).with_state(state)
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

/// workspace + 一个 `role` 角色的成员，返回 `(workspace_id, user_id)`。
pub(crate) async fn seed_workspace(pool: &PgPool, role: &str) -> (Uuid, Uuid) {
    let workspace_id: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-m83-http', $1) RETURNING id",
    )
    .bind(format!("itest-m83-http-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert workspace");
    let user_id = seed_user(pool, workspace_id, role).await;
    (workspace_id, user_id)
}

pub(crate) async fn seed_user(pool: &PgPool, workspace_id: Uuid, role: &str) -> Uuid {
    let user_id: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-m83-user', $1) RETURNING id"#,
    )
    .bind(format!("m83-{}@example.com", Uuid::new_v4()))
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

/// workspace 外的用户（不是任何 workspace 的成员）。
pub(crate) async fn seed_outsider(pool: &PgPool) -> Uuid {
    sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-m83-out', $1) RETURNING id"#,
    )
    .bind(format!("m83-out-{}@example.com", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert outsider")
}

/// 一个 `kind='user'` 的 agent（`loadAgentForUser` 只认这一种）。
async fn seed_agent_row(pool: &PgPool, workspace_id: Uuid, owner_id: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO agent(workspace_id, name, runtime_mode, kind, owner_id) \
         VALUES ($1, $2, 'local', 'user', $3) RETURNING id",
    )
    .bind(workspace_id)
    .bind(format!("itest-m83-agent-{}", Uuid::new_v4()))
    .bind(owner_id)
    .fetch_one(pool)
    .await
    .expect("insert agent")
}

/// 直插一行库条目（不经 route，让 agent 面的用例能自己造 server）。
pub(crate) async fn seed_server(
    pool: &PgPool,
    workspace_id: Uuid,
    name: &str,
    config: &str,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO workspace_mcp_server(workspace_id, name, config) \
         VALUES ($1, $2, $3::jsonb) RETURNING id",
    )
    .bind(workspace_id)
    .bind(name)
    .bind(config)
    .fetch_one(pool)
    .await
    .expect("insert workspace_mcp_server")
}

/// 一行绑定的原始状态（**绕过 route** —— 断言落库结果时用）。
pub(crate) async fn binding_enabled(
    pool: &PgPool,
    agent_id: Uuid,
    server_id: Uuid,
) -> Option<bool> {
    sqlx::query_scalar(
        "SELECT enabled FROM agent_mcp_server WHERE agent_id = $1 AND server_id = $2",
    )
    .bind(agent_id)
    .bind(server_id)
    .fetch_optional(pool)
    .await
    .expect("select binding")
}

/// 绑定行数（幂等断言用）。
pub(crate) async fn count_bindings(pool: &PgPool, server_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM agent_mcp_server WHERE server_id = $1")
        .bind(server_id)
        .fetch_one(pool)
        .await
        .expect("count bindings")
}

/// 清场：这 2 张表**没有 FK**（迁移 `315`），所以子行必须显式删；agent / workspace / user 也删。
pub(crate) async fn cleanup(pool: &PgPool, workspace_id: Uuid, user_ids: &[Uuid]) {
    let _ = sqlx::query(
        "DELETE FROM agent_mcp_server WHERE server_id IN \
         (SELECT id FROM workspace_mcp_server WHERE workspace_id = $1)",
    )
    .bind(workspace_id)
    .execute(pool)
    .await;
    let _ = sqlx::query("DELETE FROM workspace_mcp_server WHERE workspace_id = $1")
        .bind(workspace_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM agent WHERE workspace_id = $1")
        .bind(workspace_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
        .bind(workspace_id)
        .execute(pool)
        .await;
    for user_id in user_ids {
        let _ = sqlx::query(r#"DELETE FROM "user" WHERE id = $1"#)
            .bind(user_id)
            .execute(pool)
            .await;
    }
}

// ---------------------------------------------------------------------------
// 请求
// ---------------------------------------------------------------------------

pub(crate) fn req(method: &str, uri: &str, user_id: Option<Uuid>) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(user_id) = user_id {
        builder = builder.header(USER_ID_HEADER, user_id.to_string());
    }
    builder.body(Body::empty()).expect("request")
}

/// 带 JSON body 的请求（`None` ⇒ 不带 `content-type`，用于测空 body）。
pub(crate) fn req_json(
    method: &str,
    uri: &str,
    user_id: Option<Uuid>,
    body: &str,
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

/// 发一次请求，返回 `(status, json, 原始 body)`；body 不是 JSON（204 / 空）时 `json` 是 `Null`。
pub(crate) async fn send(app: &Router, request: Request<Body>) -> (StatusCode, Value, String) {
    let response = app.clone().oneshot(request).await.expect("router call");
    let status = response.status();
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
    (status, json, raw)
}

pub(crate) async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    user_id: Option<Uuid>,
) -> (StatusCode, Value, String) {
    send(app, req(method, uri, user_id)).await
}

/// 带头/带 body 的请求（agent 面用它注入 workspace）。
pub(crate) fn req_scoped(
    method: &str,
    uri: &str,
    user_id: Option<Uuid>,
    workspace_id: Option<Uuid>,
    body: Option<&str>,
) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(user_id) = user_id {
        builder = builder.header(USER_ID_HEADER, user_id.to_string());
    }
    if let Some(workspace_id) = workspace_id {
        builder = builder.header(WORKSPACE_HEADER, workspace_id.to_string());
    }
    let body = match body {
        Some(body) => {
            builder = builder.header("content-type", "application/json");
            Body::from(body.to_string())
        }
        None => Body::empty(),
    };
    builder.body(body).expect("request")
}

/// 发一次**带 workspace 头**的请求（agent 面的专用入口）。
pub(crate) async fn call_scoped(
    app: &Router,
    method: &str,
    uri: &str,
    user_id: Option<Uuid>,
    workspace_id: Option<Uuid>,
    body: Option<&str>,
) -> (StatusCode, Value, String) {
    send(app, req_scoped(method, uri, user_id, workspace_id, body)).await
}

/// agent 面集合 URL（agent id 在路径里，workspace 走头 ⇒ 不依赖 `Fx`）。
pub(crate) fn agent_uri(agent_id: Uuid) -> String {
    format!("/api/agents/{agent_id}/mcp-servers")
}

/// agent 面开关 URL。
pub(crate) fn agent_enabled_uri(agent_id: Uuid, server_id: Uuid) -> String {
    format!("/api/agents/{agent_id}/mcp-servers/{server_id}/enabled")
}

/// agent 面单条 URL。
pub(crate) fn agent_item_uri(agent_id: Uuid, server_id: Uuid) -> String {
    format!("/api/agents/{agent_id}/mcp-servers/{server_id}")
}

/// 用例跳过时打印一句（`connect()` 返回 `None`）。
pub(crate) fn skipped() {
    eprintln!("skip: MULTICA_TEST_DATABASE_URL 未设置");
}

// ---------------------------------------------------------------------------
// 固定装置
// ---------------------------------------------------------------------------

/// 一次用例的固定装置：workspace + 四个身份的调用者 + 已建的 router。
///
/// `Fx::new()` 返回 `None` ⇒ 用例 `skipped(); return;`（与 `tests/vcs/` 同款）。
pub(crate) struct Fx {
    pub(crate) pool: PgPool,
    pub(crate) app: Router,
    pub(crate) ws: Uuid,
    pub(crate) admin: Uuid,
    pub(crate) member: Uuid,
    pub(crate) guest: Uuid,
    pub(crate) outsider: Uuid,
}

impl Fx {
    pub(crate) async fn new() -> Option<Self> {
        let (pool, db) = connect().await?;
        let (ws, admin) = seed_workspace(&pool, "admin").await;
        let member = seed_user(&pool, ws, "member").await;
        let guest = seed_user(&pool, ws, "guest").await;
        let outsider = seed_outsider(&pool).await;
        let app = app_with(db);
        Some(Self {
            pool,
            app,
            ws,
            admin,
            member,
            guest,
            outsider,
        })
    }

    /// 库面集合 URL。
    pub(crate) fn library_uri(&self) -> String {
        format!("/api/workspaces/{}/mcp-servers", self.ws)
    }

    /// 库面单条 URL。
    pub(crate) fn library_item_uri(&self, server_id: Uuid) -> String {
        format!("/api/workspaces/{}/mcp-servers/{server_id}", self.ws)
    }

    /// 直插一个 `kind='user'` 的 agent（`loadAgentForUser` 只认这一种）。
    pub(crate) async fn seed_agent(&self, owner_id: Uuid) -> Uuid {
        seed_agent_row(&self.pool, self.ws, owner_id).await
    }

    /// 直插一行库条目。
    pub(crate) async fn seed_server(&self, name: &str, config: &str) -> Uuid {
        seed_server(&self.pool, self.ws, name, config).await
    }

    /// 清场（见 [`cleanup`]）。
    pub(crate) async fn teardown(&self) {
        cleanup(
            &self.pool,
            self.ws,
            &[self.admin, self.member, self.guest, self.outsider],
        )
        .await;
    }
}
