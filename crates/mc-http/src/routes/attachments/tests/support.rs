//! M10-B1 用例的共用件（写者 M10-B1）：`AppState` 字面量 + 请求装置 + 真库连接。
//!
//! 拆出来是门 ⑩（单文件 800 行硬上限）的要求，先例 = `routes/onboarding/tests/support.rs`
//! （M9-3）与 `routes/cloud/subscriptions/tests/support.rs`（M9-2）。
//!
//! **本片零出站**（6 条全是本地路由：3 条要 workspace 头、2 条要成员资格、1 条只认签名）
//! —— 与 M9-1/M9-2 相反，这份 support 只提供「应用装配 + 发请求 + 连真库」三件套。
//!
//! ⚠️ `AppState` 用真 URL、**不可达**地址的 `connect_lazy` 装
//! （`mc-conformance/src/harness.rs` 的 `STATELESS_URL`、`probes/live/tests.rs` 同款）：
//! 不碰库的那一半一旦有人真去查库就会拿到连接错误，而不是静默通过。

use std::sync::Arc;

use axum::body::Body as AxumBody;
use axum::http::Request as HttpRequest;
use http_body_util::BodyExt as _;
use mc_core::actor::ActorRegistry;
use mc_db::Db;
use mc_feature_flags::FeatureFlagCatalog;
use mc_realtime::{RealtimeHandle, WsState};
use uuid::Uuid;

use crate::routes::auth_user::USER_ID_HEADER;
use crate::state::cloud::{CloudConfig, EntitlementConfig};
use crate::state::integrations::{ComposioKeys, GithubKeys, VcsKeys};
use crate::state::{
    AdapterRegistry, AppState, ChannelKeys, ConfigSnapshot, GoogleOAuthConfig, RuntimeHandles,
};

/// 真 URL、**不可达**地址（`mc-conformance/src/harness.rs::STATELESS_URL` 的同款形状）。
pub const UNREACHABLE_DB: &str = "postgres://attachment:attachment@127.0.0.1:1/attachment_probe";

/// 懒连接池（`connect_lazy` 不拨号）：不碰库的那一半一次都不连。
pub fn lazy_db() -> Db {
    Db::connect_lazy(UNREACHABLE_DB, 1, 0).expect("lazy db")
}

/// 真库那一半的连接（设了变量连不上 ⇒ **panic**，不许静默假装绿）。
pub async fn pool() -> Option<Db> {
    let url = std::env::var("MULTICA_TEST_DATABASE_URL").ok()?;
    Some(
        Db::connect(&url, 4, 1)
            .await
            .unwrap_or_else(|e| panic!("MULTICA_TEST_DATABASE_URL is set but connect failed: {e}")),
    )
}

/// `AppState` 字面量（`AppState::new` 只读进程 env，而用例要注入**没配云 / 没配存储**这个态）。
pub fn test_app(db: Db) -> axum::Router {
    assemble(build_state(db, mc_storage::Storage::new()))
}

/// `AppState` 字面量，**并**把 `storage` 指向一个已注册的本地 provider。
///
/// `/content` 与 `/download` 都要真的把对象字节取出来 ⇒ 不给它们一个可路由的
/// bucket 的话，这两条键在真库那一半只能钉「403 storage not configured」。
/// `root` 是 `LocalDiskStorage` 的根目录，`bucket` 是**已路由**的桶名。
pub fn test_app_with_objects(db: Db, root: &std::path::Path, bucket: &str) -> axum::Router {
    assemble(build_state(db, build_storage(root, bucket)))
}

fn assemble(state: Arc<AppState>) -> axum::Router {
    crate::apply_default_middleware(crate::routes::router(state.clone())).with_state(state)
}

/// 一个已注册 + 已路由的本地磁盘存储。
pub fn build_storage(root: &std::path::Path, bucket: &str) -> mc_storage::Storage {
    let storage = mc_storage::Storage::new();
    storage
        .register(Arc::new(mc_storage::LocalDiskStorage::new(root)))
        .expect("register local_disk");
    storage
        .route_bucket(bucket, "local_disk")
        .expect("route bucket");
    storage
}

/// 把对象写进本地磁盘（`bucket/key`），返回可直接存进 `attachment.url` 的那串。
pub fn seed_object(root: &std::path::Path, bucket: &str, key: &str, body: &[u8]) -> String {
    let dir = root.join(bucket);
    std::fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join(key);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir nested");
    }
    std::fs::write(&path, body).expect("write object");
    format!("{bucket}/{key}")
}

fn build_state(db: Db, storage: mc_storage::Storage) -> Arc<AppState> {
    let realtime = RealtimeHandle::start(8);
    let ws = Arc::new(WsState::new(realtime.clone(), "attachment-test"));
    Arc::new(AppState {
        db,
        runtime: RuntimeHandles {
            actors: ActorRegistry::new(),
            adapters: Arc::new(AdapterRegistry::default()),
        },
        config: ConfigSnapshot::default(),
        storage,
        secrets: mc_secrets::Secrets::new(mc_auth::DefaultSecretsBackend::in_memory()),
        feature_flags: Arc::new(FeatureFlagCatalog::new()),
        realtime,
        ws,
        auth: mc_auth::SessionStoreContainer::new(),
        pat: mc_auth::PatStoreContainer::new(),
        verification: mc_auth::VerificationStoreContainer::new(),
        google_oauth: GoogleOAuthConfig::default(),
        daemon_hub: Arc::new(mc_ws::hub::Hub::new()),
        daemon_requests: Arc::new(crate::daemon_requests::RequestStore::new()),
        plugin_key: None,
        plugin_surface_origin: None,
        channel_keys: ChannelKeys::default(),
        github_keys: GithubKeys::default(),
        vcs_keys: VcsKeys::default(),
        composio_keys: ComposioKeys::default(),
        // 本片**零出站** ⇒ 云配没配都一样（用未配置那个态，顺带证明它不被读）。
        cloud: CloudConfig::from_env_with(|_| None),
        entitlement: EntitlementConfig::from_env_with(|_| None),
    })
}

/// 一条待发请求。
pub struct Call {
    pub method: &'static str,
    pub uri: String,
    pub user: Option<Uuid>,
    /// `x-workspace-id`（`/content` 与 `/api/issues/:id/attachments` 两条 workspace 路由要）。
    pub workspace: Option<Uuid>,
}

impl Call {
    /// 带会话 + workspace 头的一条请求。
    pub fn new(method: &'static str, uri: impl Into<String>, user: Uuid, workspace: Uuid) -> Self {
        Self {
            method,
            uri: uri.into(),
            user: Some(user),
            workspace: Some(workspace),
        }
    }

    /// 只带会话（`/download` 与 `/signed-download` **不要** workspace 头 —— 那正是
    /// 它们与另两条的差别；带上就等于没测出那条差别）。
    pub fn authed(method: &'static str, uri: impl Into<String>, user: Uuid) -> Self {
        Self {
            method,
            uri: uri.into(),
            user: Some(user),
            workspace: None,
        }
    }

    /// 匿名（用来钉 401）。
    pub fn anon(method: &'static str, uri: impl Into<String>) -> Self {
        Self {
            method,
            uri: uri.into(),
            user: None,
            workspace: None,
        }
    }

    async fn send(self, app: &axum::Router) -> (u16, axum::http::HeaderMap, String) {
        let (status, headers, bytes) = self.send_bytes(app).await;
        (
            status,
            headers,
            String::from_utf8_lossy(&bytes).into_owned(),
        )
    }

    /// 字节精确的那一半（**不经 `from_utf8_lossy`** —— 它会把 0x89 这类字节换成
    /// U+FFFD，于是「原样取回」这条判据根本测不出来）。
    async fn send_bytes(self, app: &axum::Router) -> (u16, axum::http::HeaderMap, Vec<u8>) {
        let mut req = HttpRequest::builder().method(self.method).uri(&self.uri);
        if let Some(u) = self.user {
            req = req.header(USER_ID_HEADER.as_str(), u.to_string());
        }
        if let Some(w) = self.workspace {
            req = req.header("x-workspace-id", w.to_string());
        }
        let resp = app
            .clone()
            .oneshot(req.body(AxumBody::empty()).expect("build request"))
            .await
            .expect("router responded");
        let status = resp.status().as_u16();
        let headers = resp.headers().clone();
        let bytes = resp
            .into_body()
            .collect()
            .await
            .expect("read body")
            .to_bytes();
        (status, headers, bytes.to_vec())
    }
}
use tower::ServiceExt as _;

/// 发一条请求，返回 `(状态码, 响应头, 正文)`。
pub async fn call(app: &axum::Router, c: Call) -> (u16, axum::http::HeaderMap, String) {
    c.send(app).await
}

/// 发一条请求并**原样取回字节**（`/download` 那一族用它验「字节逐字不变」）。
pub async fn call_bytes(app: &axum::Router, c: Call) -> (u16, axum::http::HeaderMap, Vec<u8>) {
    c.send_bytes(app).await
}

/// 一个会自己清掉的临时目录（`LocalDiskStorage` 的根）。
///
/// **不用 `tempfile`**：它是 `mc-storage` 的 dev-dep，**不是** `mc-http` 的
/// ⇒ 用它要改 `mc-http/Cargo.toml`（本片写集之外）。判据：本仓惯例是
/// 「写集外的共享 manifest 一律不动，零新 package 也要靠既有依赖撑住」。
pub struct TempDir(std::path::PathBuf);

impl TempDir {
    #[must_use]
    pub fn new(tag: &str) -> Self {
        let p =
            std::env::temp_dir().join(format!("multica-m10b1-{tag}-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&p).expect("mkdir tempdir");
        Self(p)
    }

    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 造一个 uuid（用例只拿它当**不同的**标识符用，不作任何确定性断言）。
///
/// 命名避开 `uuid` 这个 crate 本身，免得 `db.rs` 里 `use uuid::Uuid` 撞名。
pub fn new_uuid() -> Uuid {
    Uuid::new_v4()
}
