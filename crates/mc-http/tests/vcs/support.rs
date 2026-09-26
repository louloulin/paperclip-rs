//! `/api/workspaces/{id}/vcs/*` + `POST /api/webhooks/vcs/{connectionId}` 端到端测试的共用件
//! （M8-2 / `LUM-1799`；与 `tests/github/` / `tests/skills/` 同手法）。
//!
//! - **真库**：`workspace` / `member` / `"user"` / `vcs_connection` / `vcs_pull_request` /
//!   `issue_vcs_pull_request` / `vcs_commit_status` 必须已迁移（门 ⑥ 先跑
//!   `mc-migrate run --dir migrations`）。
//! - **离线替身**：Forgejo/GitLab 侧全部由 [`serve_stub`] 起的本地 axum 服务承担。注入点是
//!   **每连接自带的 `instance_url`**（上游 `NormalizeInstanceURL`，`docs/61` §4.2 的 VCS 行）
//!   ⇒ 不需要任何进程级全局注入，用例之间**无串扰**。
//! - `AppState` 用**结构体字面量**构造：`vcs_keys` 要按用例注入「产品边界开/关 × 密钥有/无」
//!   四象限（`AppState::new` 只读进程 env）。字段全 `pub`，不需要改任何锚点文件。
//! - **凭据**：`SECRET_KEY_B64` 是 `docs/59`…`mc-secrets` 单测里那枚**测试专用**密钥
//!   （`0x00..0x1f`），不是生产密钥。

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use mc_core::actor::ActorRegistry;
use mc_db::Db;
use mc_http::state::integrations::VcsKeys;
use mc_http::state::{AdapterRegistry, AppState, ConfigSnapshot, RuntimeHandles};
use mc_realtime::{RealtimeHandle, WsState};
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

pub(crate) const USER_ID_HEADER: &str = "x-multica-user-id";

/// 测试专用 `MULTICA_VCS_SECRET_KEY`（base64 of `0x00..0x1f`，与 `mc-secrets` 单测同一枚）。
const SECRET_KEY_B64: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";

// ---------------------------------------------------------------------------
// 应用状态 / 连接
// ---------------------------------------------------------------------------

/// 按「产品边界 × 密钥」两个独立开关造 `VcsKeys`（`from_env_with` 是唯一的注入点）。
pub(crate) fn vcs_keys(enabled: bool, with_key: bool) -> VcsKeys {
    VcsKeys::from_env_with(|name| {
        if name == VcsKeys::ENABLED_ENV && enabled {
            return Some("true".to_string());
        }
        if name == VcsKeys::SECRET_KEY_ENV && with_key {
            return Some(SECRET_KEY_B64.to_string());
        }
        None
    })
}

/// 生产口径的「可用」组合：边界开 + 密钥在。
pub(crate) fn ready_keys() -> VcsKeys {
    vcs_keys(true, true)
}

/// 解封/封装用的封装盒（测试侧自己造一份，**不**读 state 的私有字段）。
pub(crate) fn secret_box() -> mc_secrets::SecretBox {
    let key = mc_secrets::secretbox::decode_key(SECRET_KEY_B64).expect("32 字节测试密钥");
    mc_secrets::SecretBox::new(&key).expect("AES-256-GCM 盒")
}

/// 把一个明文 secret 封成库里那列的形态（base64 密文）—— 给直插连接的 webhook 用例用。
pub(crate) fn seal(plaintext: &str) -> String {
    use base64::Engine as _;
    let sealed = secret_box().seal(plaintext.as_bytes()).expect("seal");
    base64::engine::general_purpose::STANDARD.encode(sealed)
}

/// 解一个库里的密文列（测试侧直用 `mc-secrets`，**不**依赖路由层的 `pub(crate)` 辅助函数）。
pub(crate) fn open(encrypted_base64: &str) -> String {
    use base64::Engine as _;
    let ciphertext = base64::engine::general_purpose::STANDARD
        .decode(encrypted_base64)
        .expect("base64 ciphertext");
    let plaintext = secret_box().open(&ciphertext).expect("open secretbox");
    String::from_utf8(plaintext).expect("utf8 plaintext")
}

pub(crate) fn app_with(db: Db, vcs: VcsKeys) -> Router {
    let state = build_state(db, vcs);
    mc_http::routes::router(state.clone()).with_state(state)
}

/// 显式 `vcs_keys` 的 `AppState`（字段字面量，与 `tests/github/support.rs` 同款）。
pub(crate) fn build_state(db: Db, vcs_keys: VcsKeys) -> Arc<AppState> {
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
        channel_keys: mc_http::state::ChannelKeys::default(),
        github_keys: mc_http::state::integrations::GithubKeys::default(),
        vcs_keys,
        composio_keys: mc_http::state::integrations::ComposioKeys::default(),
        // M9 anchor（LUM-1815）：云面两组字段显式未配置（口径见 `state/cloud.rs`）。AppState 的字面量构造点**全部**在这里补，因为它们不用 `..Default::default()`（见 docs/32 §9.13 的写集扩展登记）。
        cloud: mc_http::state::cloud::CloudConfig::from_env_with(|_| None),
        entitlement: mc_http::state::cloud::EntitlementConfig::from_env_with(|_| None),
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

/// workspace + 一个 `role` 角色的成员，返回 `(workspace_id, user_id)`。
pub(crate) async fn seed_workspace(pool: &PgPool, role: &str) -> (Uuid, Uuid) {
    let workspace_id: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-m82-http', $1) RETURNING id",
    )
    .bind(format!("itest-m82-http-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert workspace");
    let user_id = seed_user(pool, workspace_id, role).await;
    (workspace_id, user_id)
}

pub(crate) async fn seed_user(pool: &PgPool, workspace_id: Uuid, role: &str) -> Uuid {
    let user_id: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-m82-user', $1) RETURNING id"#,
    )
    .bind(format!("m82-{}@example.com", Uuid::new_v4()))
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
        r#"INSERT INTO "user"(name, email) VALUES ('itest-m82-out', $1) RETURNING id"#,
    )
    .bind(format!("m82-out-{}@example.com", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert outsider")
}

/// 直插一行 `vcs_connection`（不经 route，让 webhook 用例能自己造连接）。
///
/// 两个 `*_encrypted` 列传的是**任意密文**：webhook 用例只关心「解封失败 ⇒ 500」与
/// 「解封成功 ⇒ 验签」两条路径，而 connect 用例走真实封装。
pub(crate) async fn seed_connection(
    pool: &PgPool,
    workspace_id: Uuid,
    provider: &str,
    instance_url: &str,
    secret_encrypted: &str,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO vcs_connection(workspace_id, provider, instance_url, account_login, \
         access_token_encrypted, webhook_secret_encrypted) \
         VALUES ($1, $2, $3, 'seed-bot', 'unused-ciphertext', $4) RETURNING id",
    )
    .bind(workspace_id)
    .bind(provider)
    .bind(instance_url)
    .bind(secret_encrypted)
    .fetch_one(pool)
    .await
    .expect("insert vcs_connection")
}

/// 清场：这 4 张表**没有 FK**（迁移 `216`），所以子行必须显式删；workspace / user 也删。
pub(crate) async fn cleanup(pool: &PgPool, workspace_id: Uuid, user_ids: &[Uuid]) {
    for sql in [
        "DELETE FROM issue_vcs_pull_request WHERE pull_request_id IN \
         (SELECT id FROM vcs_pull_request WHERE workspace_id = $1)",
        "DELETE FROM vcs_commit_status WHERE connection_id IN \
         (SELECT id FROM vcs_connection WHERE workspace_id = $1)",
        "DELETE FROM vcs_pull_request WHERE workspace_id = $1",
        "DELETE FROM vcs_connection WHERE workspace_id = $1",
    ] {
        let _ = sqlx::query(sql).bind(workspace_id).execute(pool).await;
    }
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

/// POST + JSON body。
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

/// **公开** webhook 帧：任意头 + 原始 body（**不**带会话头 —— 这条路径本来就没有会话）。
pub(crate) fn req_raw(uri: &str, headers: &[(&str, &str)], body: &[u8]) -> Request<Body> {
    let mut builder = Request::builder().method("POST").uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    builder.body(Body::from(body.to_vec())).expect("request")
}

/// 发一次请求，返回 `(status, json)`；body 不是 JSON（204 / 空）时 `json` 是 `Value::Null`。
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

// ---------------------------------------------------------------------------
// 离线替身（Forgejo / GitLab）
// ---------------------------------------------------------------------------

/// 起一个只服务本用例的替身（端口由内核分配），返回 `http://127.0.0.1:{port}`。
///
/// 用例里的路径就是上游的**真实路径**（Forgejo `/api/v1/user`、GitLab `/api/v4/user`）
/// —— 替身是「假实例」，不是「假 service」（`docs/61` §4.2 的替身纪律 ①）。
pub(crate) async fn serve_stub(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind vcs stub");
    let port = listener.local_addr().expect("stub addr").port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://127.0.0.1:{port}")
}
