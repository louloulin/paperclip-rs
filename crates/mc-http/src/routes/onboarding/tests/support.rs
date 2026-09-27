//! M9-3 用例的共用件（写者 M9-3）：`AppState` 字面量 + 请求装置 + 真库连接。
//!
//! 拆出来是门 ⑩（单文件 800 行硬上限）的要求，先例 = `routes/cloud/subscriptions/tests/support.rs`
//! （M9-2）与 `routes/channels/lark/tests/support.rs`。
//!
//! **本片没有云侧替身**（5 条全部是本地 user-scoped 路由，不出站）—— 与 M9-1/M9-2 相反，
//! 这份 support 只提供「应用装配 + 发请求 + 连真库」三件套。

use axum::body::Body as AxumBody;
use axum::http::Request as HttpRequest;
use http_body_util::BodyExt as _;
use mc_core::actor::ActorRegistry;
use mc_db::Db;
use mc_feature_flags::FeatureFlagCatalog;
use mc_realtime::{RealtimeHandle, WsState};
use uuid::Uuid;

use crate::routes::auth_user::USER_ID_HEADER;

/// `middleware::authn` 那套守卫认的会话头（`require_user` 走的是它，**不是** `USER_ID_HEADER`）。
///
/// ⚠️ 本片 5 条里**只有** `complete` 在 `require_user` 之下（它挂在 `workspaces.rs` 的
/// user-scoped 组里）；其余 4 条用 [`USER_ID_HEADER`] 那个 dev-mode 提取器。
/// 本测试装置**两个头都带**（值都是同一个 user id）⇒ 5 条的认证路径在用例里是等价的。
pub const SESSION_HEADER: &str = "x-multica-session";
use crate::state::cloud::{CloudConfig, EntitlementConfig};
use crate::state::integrations::{ComposioKeys, GithubKeys, VcsKeys};
use crate::state::{
    AdapterRegistry, AppState, ChannelKeys, ConfigSnapshot, GoogleOAuthConfig, RuntimeHandles,
};
use std::sync::Arc;

/// 懒连接池（`connect_lazy` 不拨号）：不碰库的那一半一次都不连。
pub fn lazy_db() -> Db {
    Db::connect_lazy("postgres://onboarding:onboarding@127.0.0.1:1/none", 1, 0).expect("lazy db")
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

/// 跳过的宏（`db.rs` 用 `use super::support::*;` 取它；`macro_rules!` 的文本作用域不跨模块）。
macro_rules! fixture {
    () => {
        match pool().await {
            Some(db) => db,
            None => {
                eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
                return;
            }
        }
    };
}
pub(super) use fixture;

/// `AppState` 字面量（`AppState::new` 只读进程 env，而用例要注入**没配云**这个态）。
pub fn test_app(db: Db) -> axum::Router {
    let state = build_state(db);
    crate::apply_default_middleware(crate::routes::router(state.clone())).with_state(state)
}

fn build_state(db: Db) -> Arc<AppState> {
    let realtime = RealtimeHandle::start(8);
    let ws = Arc::new(WsState::new(realtime.clone(), "onboarding-test"));
    Arc::new(AppState {
        db,
        runtime: RuntimeHandles {
            actors: ActorRegistry::new(),
            adapters: Arc::new(AdapterRegistry::default()),
        },
        config: ConfigSnapshot::default(),
        storage: mc_storage::Storage::new(),
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
        // onboarding 面**不出站** ⇒ 云配没配都一样（用未配置那个态，顺带证明它不被读）。
        cloud: CloudConfig::from_env_with(|_| None),
        entitlement: EntitlementConfig::from_env_with(|_| None),
    })
}

/// 一条待发请求。
pub struct Call {
    pub method: &'static str,
    pub uri: &'static str,
    pub user: Option<Uuid>,
    pub body: Option<Vec<u8>>,
}

impl Call {
    /// 带会话的一条请求。
    pub fn new(method: &'static str, uri: &'static str, user: Uuid) -> Self {
        Self {
            method,
            uri,
            user: Some(user),
            body: None,
        }
    }

    /// 无会话（⇒ 401 那一格）。
    pub fn anonymous(method: &'static str, uri: &'static str) -> Self {
        Self {
            method,
            uri,
            user: None,
            body: None,
        }
    }

    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = Some(body.into());
        self
    }

    /// 显式**不带**体（`complete` 的空体是合法 legacy 调用，要与「体是 `{}`」区分开）。
    pub fn no_body(mut self) -> Self {
        self.body = Some(Vec::new());
        self
    }
}

fn request_of(call: &Call) -> HttpRequest<AxumBody> {
    let mut builder = HttpRequest::builder().method(call.method).uri(call.uri);
    if let Some(user) = call.user {
        builder = builder
            .header(USER_ID_HEADER, user.to_string())
            .header(SESSION_HEADER, user.to_string());
    }
    builder
        .body(
            call.body
                .clone()
                .map_or_else(AxumBody::empty, AxumBody::from),
        )
        .expect("request")
}

/// 发一条请求，回 `(status, body 字节)`。
pub async fn send(app: &axum::Router, call: &Call) -> (axum::http::StatusCode, Vec<u8>) {
    let response = app
        .clone()
        .oneshot(request_of(call))
        .await
        .expect("router call");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes()
        .to_vec();
    (status, bytes)
}

/// 需要 `tower::ServiceExt` 的 `oneshot`。
use tower::ServiceExt as _;
