//! `POST /api/webhooks/github` + `GET /api/issues/:id/pull-requests` 端到端测试的共用件
//! （M8-4 / `LUM-1801`；与 `tests/github/support.rs`（M8-1）同手法，独立一份是因为本片是
//! 另一个测试二进制 —— 两片不共写同一个 `main.rs`）。
//!
//! - **真库**：`workspace` / `member` / `"user"` / `issue` / `github_installation` /
//!   `github_pull_request` / `issue_pull_request` 必须已迁移（门 ⑥ 先跑
//!   `mc-migrate run --dir migrations`）。
//! - **快照端口**：`PR_REFRESH_SLOT` 是**进程全局**的（`routes/github/webhook.rs`）⇒ 依赖它
//!   的用例必须串行（[`PORT_LOCK`]，与 `tests/skills/import.rs` 的 `MOCK_LOCK` 同款）。
//! - `AppState` 用**结构体字面量**构造：`github_keys` 要按用例注入「配了 / 没配」。

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use mc_core::actor::ActorRegistry;
use mc_db::Db;
use mc_http::state::integrations::GithubKeys;
use mc_http::state::{AdapterRegistry, AppState, ConfigSnapshot, RuntimeHandles};
use mc_realtime::{RealtimeHandle, WsState};
use mc_vcs_github::port::{PrRefreshPort, PrRefreshRequest, SharedPrRefresh};
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

pub(crate) const USER_ID_HEADER: &str = "x-multica-user-id";
pub(crate) const WORKSPACE_ID_HEADER: &str = "x-workspace-id";
pub(crate) const WEBHOOK_SECRET: &str = "itest-m84-webhook-secret";

/// 注入端口是进程全局的 ⇒ 用它的用例串行（`tokio::sync::Mutex` 不会因某个用例 panic 而毒化）。
pub(crate) static PORT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 取得本次用例的
/// **串行许可**（等价于 `PORT_LOCK.lock().await`，见 [`PORT_LOCK`]）。
///
/// ⚠️ 这个 guard 是**故意**活得比若干次 `await` 长的（用例体内从头到尾），那正是它存在的
/// 理由：`PR_REFRESH_SLOT` 是进程级单例，只有「整条用例独占”才能让「本用例注入的端口就是
/// 被测 handler 拿到的那个端口」成立。它用的是**异步** `tokio::sync::Mutex`（不是
/// `std::sync::Mutex`）⇒ 跨 `await` 持锁是安全的、不会自死锁（本模块内没有任何地方会重复
/// 取同一把锁）。同一个造型在 `tests/skills/import.rs` 的 `MOCK_LOCK`、`tests/github/support.rs`
/// 的 `STUB_LOCK` 里已经跑了两轮（两个波次的既有判例）。
pub(crate) async fn serial() -> tokio::sync::MutexGuard<'static, ()> {
    PORT_LOCK.lock().await
}

// ---------------------------------------------------------------------------
// 应用状态 / 连接
// ---------------------------------------------------------------------------

/// 用显式 `github_keys` 造 `AppState`（字段字面量，与 `tests/github/support.rs` 同款）。
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

pub(crate) fn app_with(db: Db, github_keys: GithubKeys) -> (Router, Arc<AppState>) {
    let state = build_state(db, github_keys);
    (
        mc_http::routes::router(state.clone()).with_state(state.clone()),
        state,
    )
}

/// webhook 面的键：只配 `GITHUB_WEBHOOK_SECRET`（`is_webhook_configured`）。
pub(crate) fn webhook_keys(secret: Option<&str>) -> GithubKeys {
    GithubKeys {
        app_id: None,
        private_key_pem: None,
        webhook_secret: secret.map(str::to_string),
        app_slug: None,
    }
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

/// workspace + 一个 `role` 角色的成员。slug 形如 `m84test-…` ⇒ issue 前缀是 `M84TEST`。
pub(crate) async fn seed_workspace(pool: &PgPool, role: &str) -> (Uuid, Uuid, String) {
    let slug = format!("m84test-{}", Uuid::new_v4().simple());
    seed_workspace_with_slug(pool, role, &slug).await
}

/// 指定 slug 的变体：`issue_prefix_from_slug` 只取**前 8 个字母数字**并大写 ⇒ 两个 slug 只要
/// 前 8 个字母数字相同，前缀就相同（造「同一标识符在两个 workspace 里都能解析」的歧义必须这样）。
pub(crate) async fn seed_workspace_with_slug(
    pool: &PgPool,
    role: &str,
    slug: &str,
) -> (Uuid, Uuid, String) {
    let workspace_id: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-m84-http', $1) RETURNING id",
    )
    .bind(slug)
    .fetch_one(pool)
    .await
    .expect("insert workspace");
    let user_id: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-m84-user', $1) RETURNING id"#,
    )
    .bind(format!("m84-{}@example.com", Uuid::new_v4()))
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
    // `issue_prefix_from_slug` 只取字母数字并大写 ⇒ `m84test` 前缀恒为 `M84TEST`。
    let prefix: String = slug
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect::<String>()
        .to_ascii_uppercase();
    (workspace_id, user_id, prefix)
}

/// 每个用例一个**唯一**的 GitHub installation id。
///
/// ⚠️ 不能用固定值：`github_installation` 的 fan-out / 删除都按 `installation_id` **全局**定位
/// （那正是上游的语义），于是共享一个数字的用例会互相看到对方的绑定与 PR 行 —— 尤其是某个
/// 用例断言失败而 panic（cleanup 不会跑）之后，遗留行会污染后续用例。
pub(crate) fn unique_installation_id() -> i64 {
    // 取 UUID 前 8 字节当一个 `i64`，再把它映射到 `[10^12, 2×10^12)` —— 固定高位段保证
    // 非零（`installation.id == 0` 是被短路的值）。用 `try_from` 而不是 `as`：`tail` 恒
    // `< 10^12`，所以转换不可能失败，而让这一点在类型层面可读（clippy 的
    // `cast_possible_wrap` / `cast_possible_truncation` 正是在这两处画线）。
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&Uuid::new_v4().as_bytes()[..8]);
    let raw = i64::from_le_bytes(bytes);
    let tail = i64::try_from(raw.unsigned_abs() % 1_000_000_000_000).unwrap_or_default();
    1_000_000_000_000 + tail
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

/// 直插一行 issue（显式给 `number` / `identifier`，让被测代码的自动关联能按号命中）。
pub(crate) async fn seed_issue(
    pool: &PgPool,
    workspace_id: Uuid,
    user_id: Uuid,
    number: i32,
    identifier: &str,
    status: &str,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO issue(workspace_id, number, identifier, title, status, creator_type, creator_id) \
         VALUES ($1, $2, $3, 'itest-m84 issue', $4, 'member', $5) RETURNING id",
    )
    .bind(workspace_id)
    .bind(number)
    .bind(identifier)
    .bind(status)
    .bind(user_id)
    .fetch_one(pool)
    .await
    .expect("insert issue")
}

/// workspace 的自动关联开关（`settings` 是 JSONB）——写库是为了让「关掉 ⇒ 不建关联账」可测。
pub(crate) async fn set_workspace_settings(pool: &PgPool, workspace_id: Uuid, settings: Value) {
    sqlx::query("UPDATE workspace SET settings = $2 WHERE id = $1")
        .bind(workspace_id)
        .bind(settings)
        .execute(pool)
        .await
        .expect("update workspace settings");
}

/// 清场：workspace 级联删 installation / issue / PR；user 单独删（成员行级联）。
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
// 真实帧（离线替身：发帧的那一侧）
// ---------------------------------------------------------------------------

/// 一帧带有**真实 HMAC-SHA256 头**的 webhook（`docs/61` §4.2 的替身纪律第 ② 条）。
pub(crate) fn signed_webhook_request(
    secret: Option<&str>,
    event: &str,
    body: &Value,
) -> Request<Body> {
    let body = serde_json::to_vec(body).expect("serialize frame");
    let signature = secret.map(|secret| mc_vcs_github::webhook::sign_webhook_body(secret, &body));
    let mut builder = Request::builder()
        .method("POST")
        .uri("/api/webhooks/github")
        .header("content-type", "application/json")
        .header("x-github-event", event);
    if let Some(signature) = signature {
        builder = builder.header("x-hub-signature-256", signature);
    }
    builder.body(Body::from(body)).expect("request")
}

pub(crate) fn get_with_workspace(
    uri: &str,
    user_id: Option<Uuid>,
    workspace_id: Option<Uuid>,
) -> Request<Body> {
    let mut builder = Request::builder().method("GET").uri(uri);
    if let Some(user_id) = user_id {
        builder = builder.header(USER_ID_HEADER, user_id.to_string());
    }
    if let Some(workspace_id) = workspace_id {
        builder = builder.header(WORKSPACE_ID_HEADER, workspace_id.to_string());
    }
    builder.body(Body::empty()).expect("request")
}

/// 发一次请求，返回 `(status, json)`（body 不是 JSON 时是 `Value::Null`）。
pub(crate) async fn call(app: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = app.clone().oneshot(request).await.expect("router call");
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
// 记录型快照端口（DoD 的「快照入队」断言点）
// ---------------------------------------------------------------------------

/// 记录每一次入队并保持「已配置」的端口 —— 断言链的最后一环在这里落地。
pub(crate) struct RecordingPort {
    pub(crate) enabled: bool,
    pub(crate) enqueued: Mutex<Vec<PrRefreshRequest>>,
    pub(crate) view_enqueued: Mutex<Vec<PrRefreshRequest>>,
}

impl RecordingPort {
    pub(crate) fn new(enabled: bool) -> Arc<Self> {
        Arc::new(Self {
            enabled,
            enqueued: Mutex::new(Vec::new()),
            view_enqueued: Mutex::new(Vec::new()),
        })
    }

    pub(crate) fn as_port(self: &Arc<Self>) -> SharedPrRefresh {
        Arc::new(RecordingHandle(Arc::clone(self)))
    }

    /// 已入队次数（按 `reason` 过滤由调用方做）。
    pub(crate) fn enqueued(&self) -> Vec<PrRefreshRequest> {
        self.enqueued
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn view_enqueued(&self) -> Vec<PrRefreshRequest> {
        self.view_enqueued
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

/// `SharedPrRefresh` 要求 `Arc<dyn PrRefreshPort>` ⇒ 加一层薄转发（不能直接 coerce 一个
/// 需要 `Arc<Self>` 的自定义类型）。
struct RecordingHandle(Arc<RecordingPort>);

impl PrRefreshPort for RecordingHandle {
    fn enabled(&self) -> bool {
        self.0.enabled
    }

    fn enqueue(&self, request: PrRefreshRequest) {
        self.0
            .enqueued
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(request);
    }

    fn maybe_enqueue_on_view(&self, request: PrRefreshRequest) -> bool {
        self.0
            .view_enqueued
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(request);
        true
    }
}
