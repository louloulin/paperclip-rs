//! M9-2 用例的共用件（写者 M9-2）：云侧**离线替身**、`AppState` 字面量、请求/认领装置。
//!
//! 拆出来是门 ⑩（单文件 800 行硬上限）的要求，先例 = `docs/32` §30 的 **D10**
//! （`routes/channels/lark/tests/{support,db}.rs`）与 M9-1 的
//! `routes/cloud/billing/tests.rs`（新建）。
//!
//! ## 替身纪律（`docs/62` §4.2 的替身纪律 ①）
//!
//! 替身是**本地 axum 服务**（一个进程一个 base，行为按 `X-Request-ID` 的变体前缀分派），
//! 只替**平台 wire** —— 不替业务路径、不替云侧的授权判定、不替 Stripe。
//! 每个用例用自己那枚唯一的 request id 认领自己的出站记录（`CALLS` 是进程级静态而用例并行
//! 跑 ⇒ 按「最后一条」读会拿到别人的，见 `docs/32` §36.1/§36.3 的实测教训）。

use super::*;

// -----------------------------------------------------------------------
// 路由表 / 体（运行期那一半；门 ⑦ 负责「本地 ↔ 上游路由表」）
// -----------------------------------------------------------------------

/// 7 条（method, 本地字面量, 云侧路径）。五条写的云侧路径多一个 `{workspace_id}` 段 ⇒ 占位替换。
pub(super) const SEVEN: [(&str, &str, &str); 7] = [
    (
        "GET",
        "/api/cloud-subscriptions/summary",
        "/api/v1/subscriptions/{ws}/summary",
    ),
    (
        "GET",
        "/api/cloud-subscriptions/prices",
        "/api/v1/subscriptions/{ws}/prices",
    ),
    (
        "POST",
        "/api/cloud-subscriptions/checkout-sessions",
        "/api/v1/subscriptions/checkout-sessions",
    ),
    (
        "POST",
        "/api/cloud-subscriptions/seats/purchase-preview",
        "/api/v1/subscriptions/{ws}/seats/purchase-preview",
    ),
    (
        "POST",
        "/api/cloud-subscriptions/seats/purchases",
        "/api/v1/subscriptions/{ws}/seats/purchases",
    ),
    (
        "POST",
        "/api/cloud-subscriptions/seats/reconcile",
        "/api/v1/subscriptions/{ws}/seats/reconcile",
    ),
    (
        "POST",
        "/api/cloud-subscriptions/portal-sessions",
        "/api/v1/subscriptions/{ws}/portal-sessions",
    ),
];

pub(super) const SUMMARY: &str = "/api/cloud-subscriptions/summary";
pub(super) const PRICES: &str = "/api/cloud-subscriptions/prices";
pub(super) const CHECKOUT: &str = "/api/cloud-subscriptions/checkout-sessions";
pub(super) const PREVIEW: &str = "/api/cloud-subscriptions/seats/purchase-preview";
pub(super) const PURCHASES: &str = "/api/cloud-subscriptions/seats/purchases";
pub(super) const RECONCILE: &str = "/api/cloud-subscriptions/seats/reconcile";
pub(super) const PORTAL: &str = "/api/cloud-subscriptions/portal-sessions";

/// 5 条写（顺序与 [`SEVEN`] 里一致）。
pub(super) const FIVE_WRITES: [(&str, &str, &str); 5] = [
    ("POST", CHECKOUT, "/api/v1/subscriptions/checkout-sessions"),
    (
        "POST",
        PREVIEW,
        "/api/v1/subscriptions/{ws}/seats/purchase-preview",
    ),
    (
        "POST",
        PURCHASES,
        "/api/v1/subscriptions/{ws}/seats/purchases",
    ),
    (
        "POST",
        RECONCILE,
        "/api/v1/subscriptions/{ws}/seats/reconcile",
    ),
    ("POST", PORTAL, "/api/v1/subscriptions/{ws}/portal-sessions"),
];

/// 每一条在本片用例里的请求体（`GET` / 无体路由是 `None`）。
pub(super) fn body_for(local: &str) -> Option<Vec<u8>> {
    match local {
        CHECKOUT => Some(
            json!({"interval": "month", "idempotency_key": "checkout-1"})
                .to_string()
                .into_bytes(),
        ),
        PREVIEW => Some(json!({"additional_seats": 2}).to_string().into_bytes()),
        PURCHASES => Some(seat_purchase("seat-1").into_bytes()),
        _ => None,
    }
}

/// 一个合法的座位购买体（`DoD` 第 4 条的三件套；币种故意用大写 —— 上游会小写化）。
pub(super) fn seat_purchase(idempotency_key: &str) -> String {
    json!({
        "additional_seats": 2,
        "expected_current_seats": 5,
        "expected_purchase_version": 41,
        "accepted_proration_amount": 425,
        "currency": "USD",
        "idempotency_key": idempotency_key,
    })
    .to_string()
}

// -----------------------------------------------------------------------
// 云侧替身
// -----------------------------------------------------------------------

/// 替身回的两条错误体 + 超限体的标记（三者都必须**不**出现在本地错误体里）。
pub(super) const PAYLOAD_402: &str = r#"{"error":"payment_required"}"#;
pub(super) const PAYLOAD_500: &str = r#"{"error":"cloud exploded"}"#;
pub(super) const HUGE_MARKER: &str = "cloud-body-marker";

#[derive(Debug, Clone)]
pub(super) struct StubCall {
    pub(super) method: String,
    pub(super) target: String,
    pub(super) user_id: Option<String>,
    pub(super) request_id: Option<String>,
    pub(super) idempotency_key: Option<String>,
    pub(super) body: String,
    pub(super) content_type: Option<String>,
}

static CALLS: Mutex<Vec<StubCall>> = Mutex::new(Vec::new());
static STUB_BASE: OnceLock<String> = OnceLock::new();

/// 起（或复用）云侧替身：跑在**自己的 OS 线程 + 自己的 runtime** 上
/// （`#[tokio::test]` 的 runtime 会随用例结束而死）。
pub(super) fn stub_base() -> String {
    STUB_BASE
        .get_or_init(|| {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind stub");
            listener.set_nonblocking(true).expect("non-blocking");
            let port = listener.local_addr().expect("stub addr").port();
            std::thread::Builder::new()
                .name("cloud-subscriptions-stub".to_string())
                .spawn(move || {
                    let runtime = tokio::runtime::Builder::new_multi_thread()
                        .enable_all()
                        .build()
                        .expect("stub runtime");
                    runtime.block_on(async move {
                        let listener =
                            tokio::net::TcpListener::from_std(listener).expect("tokio listener");
                        let _ = axum::serve(listener, Router::new().fallback(stub)).await;
                    });
                })
                .expect("spawn stub thread");
            format!("http://127.0.0.1:{port}")
        })
        .clone()
}

/// 一个**没人听**的基址（连接必然被拒）—— 端到端触发 `docs/62` §2.6 的 502 那一行。
pub(super) fn dead_base() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind dead");
    let port = listener.local_addr().expect("dead addr").port();
    drop(listener);
    format!("http://127.0.0.1:{port}")
}

fn header_of(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

async fn stub(request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let raw = axum::body::to_bytes(body, 8 << 20)
        .await
        .map(|bytes| bytes.to_vec())
        .unwrap_or_default();
    let method = parts.method.to_string();
    let target = parts.uri.to_string();
    let path = parts.uri.path().to_string();
    let request_id = header_of(&parts.headers, "x-request-id");
    CALLS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(StubCall {
            method: method.clone(),
            target,
            user_id: header_of(&parts.headers, "x-user-id"),
            request_id: request_id.clone(),
            idempotency_key: header_of(&parts.headers, IDEMPOTENCY_KEY_HEADER),
            body: String::from_utf8_lossy(&raw).to_string(),
            content_type: header_of(&parts.headers, "content-type"),
        });
    let variant = request_id
        .as_deref()
        .and_then(|value| value.split('-').next())
        .unwrap_or_default()
        .to_string();
    let echoed = request_id.map_or_else(|| "none".to_string(), |id| format!("echoed-{id}"));
    let (status, payload) = match variant.as_str() {
        "status402" => (StatusCode::PAYMENT_REQUIRED, PAYLOAD_402.to_string()),
        "status500" => (StatusCode::INTERNAL_SERVER_ERROR, PAYLOAD_500.to_string()),
        "notjson" => (StatusCode::OK, format!("not-json-marker:{path}")),
        "blank" => (StatusCode::OK, "  \n".to_string()),
        "huge" => (
            StatusCode::OK,
            format!("{HUGE_MARKER}{}", "x".repeat(2 << 20)),
        ),
        _ => (
            StatusCode::OK,
            json!({"ok": path, "method": method, "body_len": raw.len()}).to_string(),
        ),
    };
    (status, [("x-request-id", echoed)], payload).into_response()
}

// -----------------------------------------------------------------------
// 应用装配
// -----------------------------------------------------------------------

pub(super) fn cloud_at(base: &str) -> CloudConfig {
    CloudConfig::from_env_with(|name| (name == mc_cloud::CLOUD_URL_ENV).then(|| base.to_string()))
}

pub(super) fn cloud_disabled() -> CloudConfig {
    CloudConfig::from_env_with(|_| None)
}

/// 懒连接池（`connect_lazy` 不拨号）：授权链的前三层一次都不碰库。
pub(super) fn lazy_db() -> Db {
    Db::connect_lazy("postgres://cloud:cloud@127.0.0.1:1/none", 1, 0).expect("lazy db")
}

pub(super) fn test_app(db: Db, cloud: CloudConfig, flag_on: bool) -> Router {
    let state = build_state(db, cloud, flag_on);
    crate::apply_default_middleware(crate::routes::router(state.clone())).with_state(state)
}

/// `AppState` 字面量构造（`AppState::new` 只读进程 env，而测试要注入云基址与 flag 的两个态）。
fn build_state(db: Db, cloud: CloudConfig, flag_on: bool) -> Arc<AppState> {
    let realtime = RealtimeHandle::start(8);
    let ws = Arc::new(WsState::new(realtime.clone(), "cloud-subscriptions-test"));
    let feature_flags = FeatureFlagCatalog::new();
    // flag 的两个态都在这里构造（生产由 `/api/config` 的目录负责 —— M10-4 面）。
    feature_flags.register(&FeatureKey::new(SUBSCRIPTIONS_FLAG), flag_on, None);
    Arc::new(AppState {
        db,
        runtime: RuntimeHandles {
            actors: ActorRegistry::new(),
            adapters: Arc::new(AdapterRegistry::default()),
        },
        config: ConfigSnapshot::default(),
        storage: mc_storage::Storage::new(),
        secrets: mc_secrets::Secrets::new(mc_auth::DefaultSecretsBackend::in_memory()),
        feature_flags: Arc::new(feature_flags),
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
        cloud,
        entitlement: EntitlementConfig::from_env_with(|_| None),
    })
}

/// 真库那一半的连接（设了变量连不上 ⇒ **panic**，不许静默假装绿）。
pub(super) async fn pool() -> Option<Db> {
    let url = std::env::var("MULTICA_TEST_DATABASE_URL").ok()?;
    Some(
        Db::connect(&url, 4, 1)
            .await
            .unwrap_or_else(|e| panic!("MULTICA_TEST_DATABASE_URL is set but connect failed: {e}")),
    )
}

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
// 跨模块可见（`db.rs` 用 `use super::support::*;` 取它）：`macro_rules!` 的文本作用域
// 不跨模块，所以要把名字再导出一次（与 `lark/tests/support.rs` 同款）。
pub(super) use fixture;

// -----------------------------------------------------------------------
// 请求 / 认领装置
// -----------------------------------------------------------------------

/// 一条待发的请求（用它的 `request_id` 认领替身收到的那一笔出站）。
pub(super) struct Call {
    method: &'static str,
    uri: String,
    user: Option<Uuid>,
    workspace: Option<Uuid>,
    pub(super) request_id: String,
    pub(super) body: Option<Vec<u8>>,
    actor_source: Option<&'static str>,
    pub(super) idempotency_key: Option<String>,
}

impl Call {
    pub(super) fn new(method: &'static str, uri: &str, user: Uuid, workspace: Uuid) -> Self {
        Self {
            method,
            uri: uri.to_string(),
            user: Some(user),
            workspace: Some(workspace),
            request_id: rid_of("ok"),
            body: None,
            actor_source: None,
            idempotency_key: None,
        }
    }

    pub(super) fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = Some(body.into());
        self
    }

    /// 可选体（有的路由本来就带体、有的不带；`None` 与"不带体"同义）。
    pub(super) fn maybe_body(mut self, body: Option<Vec<u8>>) -> Self {
        self.body = body;
        self
    }

    pub(super) fn query(mut self, raw: &str) -> Self {
        self.uri = format!("{}?{raw}", self.uri);
        self
    }

    pub(super) fn variant(mut self, variant: &str) -> Self {
        self.request_id = rid_of(variant);
        self
    }

    pub(super) fn machine(mut self, actor: &'static str) -> Self {
        self.actor_source = Some(actor);
        self
    }

    pub(super) fn without_session(mut self) -> Self {
        self.user = None;
        self
    }

    pub(super) fn without_workspace(mut self) -> Self {
        self.workspace = None;
        self
    }

    pub(super) fn key(mut self, key: impl Into<String>) -> Self {
        self.idempotency_key = Some(key.into());
        self
    }
}

/// 本用例唯一的出站请求 id（替身按前缀分派行为，用例按全串认领记录）。
pub(super) fn rid_of(variant: &str) -> String {
    format!("{variant}-{}", Uuid::new_v4())
}

fn request_of(call: &Call) -> HttpRequest<AxumBody> {
    let mut builder = HttpRequest::builder()
        .method(call.method)
        .uri(&call.uri)
        .header("x-request-id", &call.request_id);
    if let Some(user) = call.user {
        builder = builder.header(USER_ID_HEADER, user.to_string());
    }
    if let Some(workspace) = call.workspace {
        builder = builder.header("x-workspace-id", workspace.to_string());
    }
    if let Some(actor) = call.actor_source {
        builder = builder.header("x-actor-source", actor);
    }
    if let Some(key) = &call.idempotency_key {
        builder = builder.header(IDEMPOTENCY_KEY_HEADER, key);
    }
    builder
        .body(
            call.body
                .clone()
                .map_or_else(AxumBody::empty, AxumBody::from),
        )
        .expect("request")
}

pub(super) async fn send(app: &Router, call: &Call) -> (StatusCode, HeaderMap, Vec<u8>) {
    let response = app
        .clone()
        .oneshot(request_of(call))
        .await
        .expect("router call");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes()
        .to_vec();
    (status, headers, bytes)
}

/// 本用例**自己**那几笔出站调用（按自己那枚唯一的 request id 认领）。
pub(super) fn calls(request_id: &str) -> Vec<StubCall> {
    CALLS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .filter(|call| call.request_id.as_deref() == Some(request_id))
        .cloned()
        .collect()
}

/// 响应体里的 `error` 对象（本仓的嵌套错误信封）。
pub(super) fn error_of(bytes: &[u8]) -> Value {
    serde_json::from_slice::<Value>(bytes).unwrap_or(Value::Null)["error"].clone()
}

/// 体上限常量（`mc-core` 的 pin；别名只为让用例读起来短一点）。
pub(super) const MAX_CLOUD_REQUEST_BODY: usize =
    mc_core::cloud::MAX_CLOUD_RUNTIME_REQUEST_BODY_SIZE;
