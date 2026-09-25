//! `/api/workspaces/{id}/github/*` + `GET /api/github/setup` 端到端测试的共用件
//! （M8-1 / LUM-1798；与 `tests/skills/` / `tests/plugins/` 同手法）。
//!
//! - **真库**：`workspace` / `member` / `"user"` / `github_installation` /
//!   `github_pending_installation` 必须已迁移（门 ⑥ 先跑 `mc-migrate run --dir migrations`）。
//! - **离线替身**：GitHub 侧全部由 [`serve_stub`] 起的本地 axum 服务承担；注入点是
//!   [`set_github_api_base`]（= 上游 `var githubAPIBase` 的可写包级变量，
//!   `docs/61` §4.2）。它是**进程全局**的 ⇒ 依赖它的用例必须串行（[`STUB_LOCK`]，
//!   与 `tests/skills/import.rs` 的 `MOCK_LOCK` 同款）。
//! - `AppState` 用**结构体字面量**构造（`github_keys` 要按用例注入「配了 / 没配」；
//!   `AppState::new` 只会读进程 env —— 与 `crates/mc-http/src/routes/auth.rs` 的测试
//!   构造点同款）。字段全 `pub`，不需要改锚点文件。

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use mc_core::actor::ActorRegistry;
use mc_db::Db;
use mc_http::state::integrations::GithubKeys;
use mc_http::state::{AdapterRegistry, AppState, ConfigSnapshot, RuntimeHandles};
use mc_realtime::{RealtimeHandle, WsState};
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

pub(crate) const USER_ID_HEADER: &str = "x-multica-user-id";

/// 「注入 base」这件事是进程全局的 ⇒ 所有用替身的用例串行（`tokio::sync::Mutex` 不会因
/// 某个用例 panic 而毒化）。
pub(crate) static STUB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

// ---------------------------------------------------------------------------
// 应用状态 / 连接
// ---------------------------------------------------------------------------

/// 用显式 `github_keys` 造 `AppState`（字段字面量，与 `routes/auth.rs` 的测试点同款）。
pub(crate) fn build_state(db: Db, github_keys: GithubKeys) -> Arc<AppState> {
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
        github_keys,
        vcs_keys: mc_http::state::integrations::VcsKeys::default(),
        composio_keys: mc_http::state::integrations::ComposioKeys::default(),
    })
}

pub(crate) fn app_with(db: Db, github_keys: GithubKeys) -> Router {
    let state = build_state(db, github_keys);
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

/// `github_keys` 的「能连接」组合：`GITHUB_APP_SLUG` + `GITHUB_WEBHOOK_SECRET`。
pub(crate) fn connectable_keys(secret: &str, slug: &str) -> GithubKeys {
    GithubKeys {
        app_id: None,
        private_key_pem: None,
        webhook_secret: Some(secret.into()),
        app_slug: Some(slug.into()),
    }
}

/// 「能浏览仓库」也要的那两条（`GITHUB_APP_ID` + `GITHUB_APP_PRIVATE_KEY`）。
///
/// 私钥是 `mc-vcs-github` 的 `app.rs` 单测里那枚**测试专用** PKCS#8 密钥（不是生产密钥）。
pub(crate) fn browseable_keys(secret: &str, slug: &str) -> GithubKeys {
    GithubKeys {
        app_id: Some("123456".into()),
        private_key_pem: Some(TEST_KEY_PEM.to_string()),
        webhook_secret: Some(secret.into()),
        app_slug: Some(slug.into()),
    }
}

pub(crate) const TEST_KEY_PEM: &str = "\
-----BEGIN PRIVATE KEY-----
MIIEvwIBADANBgkqhkiG9w0BAQEFAASCBKkwggSlAgEAAoIBAQC8Lko4B+10Dj7o
dZedrDE3ZbHRBAsSKACHGOsI00EzEEUh8di1MVB1O/dYC2ZwlsteuwkZFF+aQDuT
3gZTzQVkq5menpNmJ5BVKfQr6jy7mQAhhgUEG69Afu2QNC3hFe3S+lcjdt9agM3r
rbuK35OrPrhaeW33bO+1ip0r5gnnw0j6IaW+Tnndzusv1H7tBJj9R6IA7E7tPGIr
UbdWADlP2XDPC/g5R37XhY5H5sg9B3dJptrVBD532eHG0nK7UTQybq7QMHOygYn5
KfbgZZLnYwpV40SE56DhR9jzxcIdo8okGMeoIlbwebC09spzLXwi66bPW6jE7eKY
cYwfvixlAgMBAAECggEACpr7QNQljERfRD+YV2EEdxBKqLJ3I0NQ4ExFtr4dLxEM
LGESawfH9ot2Iaam09KTzJdy6FBvIOTc1rUNGzzzQFyxcDCUsw2owzv1kGIHoTT6
vmjssHIU+ugMYHOoYEaZnCnSrmN9K/8VW+JzLtzx2BVVU3gDfA3OJqeUuwwgY8jT
bSrNe8LKiS6sj5IF2hIpBIXFTuE6SKU/64T3kiKG2AbnLLtF34LNl1sbw5TTjEAs
hGnDvNJqHgCtDqGDXaAiNbTrPWeWMJW4RhfLDzF1fpZXSw3f5z+zVMzgg6JSBZCj
Pf5FAsU4db9cGW+yLjb5NqNrBX/c24OxZ2yNcPRSkQKBgQDsraU02Mf2uFj22T0r
HBp3q11IcpuA7YU2lBP6o4aKlc6cCyj8U75yS7hu/kUoEtAUnxyyCXOpM7lhGbmz
dtIqM6r74FlIr8mPi2TLMloUGmtY42sUNtciQgd+Ds4RjWNC2/1FRe5XMFm1bP/f
6jMZEr0k0d0Cz7au5sZg4Jwg6QKBgQDLixe4xIOvQ7wvKpKngKbStW0MIAoYnvSB
hIj3xxasXc6V/qp9ANTnG28yk9tsCajUMQSfEX0uIx1YSAB0MbaIrAhsqvUaknDx
vkUtaeBWE8e0H3U+9KVnVWLZoPNHFIVeGhWMXJKpbQiZiDreELaAeltgIzU5JfAP
VRJ8eoOiHQKBgQCopqQOoFr9aCec3vhDe+cwVyBFu8UrfhVq6uHBvDznDBEKCLnP
9CzFbUejb/T/tUgpKahdBXcxnvX+R0KYq5bfE6pHiXqV3Q2YCBBu6xZdNOZBlOx8
nwd2Fe8Y2JvmzgVpYzF653YLEx0Ztu4uNMjsmPnG/vSqSDE5OKEr72HR4QKBgQDF
jWKgulr1KNDlFnTwjjVcHSqRsicabmzxqCkoE9s1wHZZrqraWIxLIp1ygX9eBKIQ
EONjYB4XQY2huYB3Rijbzdz/W445FBj7CKkrwq8x3FDfygiJ6fj/qigfAdAdFRW8
l6SCbvcJ6gGGwmogTihT2m4FiSaHKQMuXmtq1Z4dIQKBgQCBdv0m4+OX6Rxjx3cd
cpr1nFZBZqPqOdDLakWXRko+K0eNFOKCF4Smf3OUVZz5yoAAGG9HcXEwUO09ZzCD
6FZCMLwGRY4IXmRObxEfD5k/ZlzL9+/rNIKtQjObwppciqPW0NoPt5qJvMJXq7Dw
C2CZCWlMaEGLpAGiVraQnRQlPw==
-----END PRIVATE KEY-----
";

// ---------------------------------------------------------------------------
// 种子
// ---------------------------------------------------------------------------

/// workspace + 一个 `role` 角色的成员，返回 `(workspace_id, user_id)`。
pub(crate) async fn seed_workspace(pool: &PgPool, role: &str) -> (Uuid, Uuid) {
    let workspace_id: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-m81-http', $1) RETURNING id",
    )
    .bind(format!("itest-m81-http-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert workspace");
    let user_id = seed_user(pool, workspace_id, role).await;
    (workspace_id, user_id)
}

pub(crate) async fn seed_user(pool: &PgPool, workspace_id: Uuid, role: &str) -> Uuid {
    let user_id: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-m81-user', $1) RETURNING id"#,
    )
    .bind(format!("m81-{}@example.com", Uuid::new_v4()))
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

/// 直插一行 `github_installation`（不经 repo，避免测试依赖被测代码的写路径）。
pub(crate) async fn seed_installation(
    pool: &PgPool,
    workspace_id: Uuid,
    installation_id: i64,
    login: &str,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO github_installation(workspace_id, installation_id, account_login, account_type) \
         VALUES ($1, $2, $3, 'Organization') RETURNING id",
    )
    .bind(workspace_id)
    .bind(installation_id)
    .bind(login)
    .fetch_one(pool)
    .await
    .expect("insert github_installation")
}

pub(crate) async fn seed_pending_installation(pool: &PgPool, installation_id: i64, login: &str) {
    sqlx::query(
        "INSERT INTO github_pending_installation(installation_id, account_login, account_type) \
         VALUES ($1, $2, 'User') ON CONFLICT (installation_id) DO NOTHING",
    )
    .bind(installation_id)
    .bind(login)
    .execute(pool)
    .await
    .expect("insert github_pending_installation");
}

/// 清场：workspace 级联删 `github_installation`；user 单独删（成员行级联）。
pub(crate) async fn cleanup(pool: &PgPool, workspace_id: Uuid, user_ids: &[Uuid]) {
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

/// 发一次请求，返回 `(status, json, location)`。
/// `json` 在 body 不是 JSON（204 / 空）时是 `Value::Null`；`location` 取 `Location` 头。
pub(crate) async fn call_full(
    app: &Router,
    method: &str,
    uri: &str,
    user_id: Option<Uuid>,
) -> (StatusCode, Value, Option<String>) {
    let response = app
        .clone()
        .oneshot(req(method, uri, user_id))
        .await
        .expect("router call");
    let status = response.status();
    let location = response
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
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
    (status, json, location)
}

pub(crate) async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    user_id: Option<Uuid>,
) -> (StatusCode, Value) {
    let (status, json, _) = call_full(app, method, uri, user_id).await;
    (status, json)
}

// ---------------------------------------------------------------------------
// 离线替身（GitHub 侧）
// ---------------------------------------------------------------------------

/// 起一个只服务本用例的 GitHub 替身（端口由内核分配），返回 `http://127.0.0.1:{port}`。
///
/// handler 用 axum 的 `Router` 写（与 `tests/skills/support.rs` 的 `serve_mock` 同款）；
/// 用例里的路径就是 GitHub 的真实路径（`/app/installations/{id}`、
/// `/app/installations/{id}/access_tokens`、`/installation/repositories`、
/// `/installation/token`），**中间零 mock**。
pub(crate) async fn serve_stub(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind github stub");
    let port = listener.local_addr().expect("stub addr").port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://127.0.0.1:{port}")
}
