//! M9-1 的证据面（`docs/62` §4.2 的「离线替身」与 §6.5 的通用 `DoD`）。
//!
//! 替身按 `X-Request-ID` 的**变体前缀**分派；每个用例用自己那枚唯一的 request id 认领自己的
//! 出站记录（`CALLS` 是进程级静态而用例并行跑 —— 按「最后一条」读会拿到别人的，见
//! `tests/composio/support.rs` 与 `docs/32` §36.1/§36.3 的实测教训）。

use std::sync::{Mutex, OnceLock};

use axum::body::Body as AxumBody;
use axum::http::Request as HttpRequest;
use http_body_util::BodyExt as _;
use mc_core::actor::ActorRegistry;
use mc_core::cloud::{BILLING_UPSTREAM_PREFIX, CLOUD_BILLING_PREFIX};
use mc_db::Db;
use mc_feature_flags::FeatureFlagCatalog;
use mc_realtime::{RealtimeHandle, WsState};
use serde_json::{json, Value};
use tower::ServiceExt as _;
use uuid::Uuid;

use super::*;
use crate::actor_guard::HUMAN_ACTOR_REQUIRED_MESSAGE;
use crate::routes::auth_user::USER_ID_HEADER;
use crate::state::cloud::{CloudConfig, EntitlementConfig};
use crate::state::integrations::{ComposioKeys, GithubKeys, VcsKeys};
use crate::state::{
    AdapterRegistry, ChannelKeys, ConfigSnapshot, GoogleOAuthConfig, RuntimeHandles,
};

/// 8 条（method, 本地字面量, 云侧路径, 体）：本地那一列与 `router()` 的字面量逐字相同
/// （⑦ 门负责「本地 ↔ 上游路由表」，本表负责运行期的那一半）。
const CHECKOUT_BODY: &[u8] = b"{\"tier_id\":\"t1\"}";
const EIGHT: [(&str, &str, &str, Option<&[u8]>); 8] = [
    (
        "GET",
        "/api/cloud-billing/balance",
        "/api/v1/billing/balance",
        None,
    ),
    (
        "GET",
        "/api/cloud-billing/transactions",
        "/api/v1/billing/transactions",
        None,
    ),
    (
        "GET",
        "/api/cloud-billing/batches",
        "/api/v1/billing/batches",
        None,
    ),
    (
        "GET",
        "/api/cloud-billing/topups",
        "/api/v1/billing/topups",
        None,
    ),
    (
        "GET",
        "/api/cloud-billing/price-tiers",
        "/api/v1/billing/price-tiers",
        None,
    ),
    (
        "POST",
        "/api/cloud-billing/checkout-sessions",
        "/api/v1/billing/checkout-sessions",
        Some(CHECKOUT_BODY),
    ),
    (
        "GET",
        "/api/cloud-billing/checkout-sessions/cs_test_abc",
        "/api/v1/billing/checkout-sessions/cs_test_abc",
        None,
    ),
    (
        "POST",
        "/api/cloud-billing/portal-sessions",
        "/api/v1/billing/portal-sessions",
        None,
    ),
];

/// `huge` 变体的标记：它必须**出现**在云侧体里、**不出现**在本地错误体里。
const HUGE_MARKER: &str = "cloud-body-marker";
/// 替身替云侧回的两条错误体（与 `cloud_side_statuses_…` 用例共享）。
const PAYLOAD_402: &str = r#"{"error":"payment_required"}"#;
const PAYLOAD_500: &str = r#"{"error":"cloud exploded"}"#;

// -----------------------------------------------------------------------
// 应用装配 / 请求
// -----------------------------------------------------------------------

#[derive(Debug, Clone)]
struct StubCall {
    method: String,
    target: String,
    user_id: Option<String>,
    request_id: Option<String>,
    body: String,
    content_type: Option<String>,
}

static CALLS: Mutex<Vec<StubCall>> = Mutex::new(Vec::new());
static STUB_BASE: OnceLock<String> = OnceLock::new();

/// 起（或复用）云侧替身：只替平台 wire（`docs/62` §4.2 的替身纪律 ①）。跑在**自己的
/// OS 线程 + 自己的 runtime** 上（`#[tokio::test]` 的 runtime 会随用例结束而死）。
fn stub_base() -> String {
    STUB_BASE
        .get_or_init(|| {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind stub");
            listener.set_nonblocking(true).expect("non-blocking");
            let port = listener.local_addr().expect("stub addr").port();
            std::thread::Builder::new()
                .name("cloud-billing-stub".to_string())
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
            format!("http://127.0.0.1:{port}")
        })
        .clone()
}

/// 一个**没人听**的基址（连接必然被拒）—— 端到端触发 §2.6 的 502 那一行。
fn dead_base() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind dead");
    let port = listener.local_addr().expect("dead addr").port();
    drop(listener);
    format!("http://127.0.0.1:{port}")
}

fn stub_router() -> Router {
    Router::new().fallback(stub)
}

/// 替身侧读一个头（缺失 / 非 ASCII ⇒ `None`）。
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
// 应用装配 / 请求
// -----------------------------------------------------------------------

fn cloud_at(base: &str) -> CloudConfig {
    CloudConfig::from_env_with(|name| (name == mc_cloud::CLOUD_URL_ENV).then(|| base.to_string()))
}

fn cloud_disabled() -> CloudConfig {
    CloudConfig::from_env_with(|_| None)
}

/// 懒连接池（`connect_lazy` 不拨号）：8 条路由一次都不碰库 ⇒ 离线用例可以放心用它。
fn test_app(cloud: CloudConfig) -> Router {
    let db = Db::connect_lazy("postgres://cloud:cloud@127.0.0.1:1/none", 1, 0).expect("lazy db");
    test_app_with(db, cloud)
}

fn test_app_with(db: Db, cloud: CloudConfig) -> Router {
    let state = build_state(db, cloud);
    crate::apply_default_middleware(crate::routes::router(state.clone())).with_state(state)
}

/// `AppState` 字面量构造（`AppState::new` 只读进程 env，而测试要注入云基址）。
fn build_state(db: Db, cloud: CloudConfig) -> Arc<AppState> {
    let realtime = RealtimeHandle::start(8);
    let ws = Arc::new(WsState::new(realtime.clone(), "cloud-billing-test"));
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
        cloud,
        entitlement: EntitlementConfig::from_env_with(|_| None),
    })
}

/// 本用例唯一的出站请求 id（替身按前缀分派行为，用例按全串认领记录）。
fn rid_of(variant: &str) -> String {
    format!("{variant}-{}", Uuid::new_v4())
}

fn wire(
    method: &str,
    uri: &str,
    caller: Option<Uuid>,
    request_id: &str,
    body: Option<&[u8]>,
) -> HttpRequest<AxumBody> {
    let mut builder = HttpRequest::builder()
        .method(method)
        .uri(uri)
        .header("x-request-id", request_id);
    if let Some(caller) = caller {
        builder = builder.header(USER_ID_HEADER, caller.to_string());
    }
    builder
        .body(body.map_or_else(AxumBody::empty, |bytes| AxumBody::from(bytes.to_vec())))
        .expect("request")
}

fn as_machine(mut request: HttpRequest<AxumBody>, actor: &str) -> HttpRequest<AxumBody> {
    request.headers_mut().insert(
        "x-actor-source",
        HeaderValue::from_str(actor).expect("actor source"),
    );
    request
}

async fn call(app: &Router, request: HttpRequest<AxumBody>) -> (StatusCode, HeaderMap, Vec<u8>) {
    let response = app.clone().oneshot(request).await.expect("router call");
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
fn calls(request_id: &str) -> Vec<StubCall> {
    CALLS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .filter(|call| call.request_id.as_deref() == Some(request_id))
        .cloned()
        .collect()
}

/// 响应体里的 `error` 对象（本仓的嵌套错误信封）。
fn error_of(bytes: &[u8]) -> Value {
    serde_json::from_slice::<Value>(bytes).unwrap_or(Value::Null)["error"].clone()
}

// -----------------------------------------------------------------------
// 用例
// -----------------------------------------------------------------------

/// 本地字面量与云侧路径是**同一个键的前后两半**（`mc-core` 的两个前缀是唯一真相源）。
#[test]
fn local_and_upstream_paths_are_the_two_halves_of_one_key() {
    for (method, local, upstream, _) in EIGHT {
        assert!(local.starts_with(CLOUD_BILLING_PREFIX), "{local}");
        assert!(upstream.starts_with(BILLING_UPSTREAM_PREFIX), "{upstream}");
        assert_eq!(
            local.strip_prefix(CLOUD_BILLING_PREFIX),
            upstream.strip_prefix(BILLING_UPSTREAM_PREFIX),
            "{method} {local}"
        );
    }
}

/// 8 条逐条：路由 → 注入的 `X-User-ID` → 云侧路径 → 响应**逐字**透传（`docs/62` §4.2）。
#[tokio::test]
async fn all_eight_routes_proxy_verbatim_and_stamp_the_identity() {
    let app = test_app(cloud_at(&stub_base()));
    let caller = Uuid::new_v4();
    for (method, local, upstream, body) in EIGHT {
        let rid = rid_of("ok");
        let (status, headers, bytes) =
            call(&app, wire(method, local, Some(caller), &rid, body)).await;
        assert_eq!(status, StatusCode::OK, "{method} {local}");
        let expected = json!({
            "ok": upstream,
            "method": method,
            "body_len": body.map_or(0, <[u8]>::len),
        });
        assert_eq!(
            bytes,
            expected.to_string().as_bytes(),
            "{method} {local} 响应逐字"
        );
        assert_eq!(
            headers
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
        assert_eq!(
            headers
                .get("x-request-id")
                .and_then(|value| value.to_str().ok()),
            Some(format!("echoed-{rid}").as_str()),
            "云侧的 X-Request-ID 回写（上游 writeCloudRuntimeResponse）"
        );
        let outbound = calls(&rid);
        assert_eq!(outbound.len(), 1, "{method} {local}");
        assert_eq!(outbound[0].method, method);
        assert_eq!(outbound[0].target, upstream, "云侧路径逐字");
        assert_eq!(
            outbound[0].user_id.as_deref(),
            Some(caller.to_string().as_str()),
            "注入的 X-User-ID 必须是会话身份"
        );
    }
}

/// `withQuery` 只在上游打开的那 3 条上生效：带 query 的透传、不带的**丢掉**。
#[tokio::test]
async fn query_strings_are_forwarded_only_where_upstream_turns_that_on() {
    let app = test_app(cloud_at(&stub_base()));
    let caller = Uuid::new_v4();
    for (local, upstream, with_query) in [
        (
            "/api/cloud-billing/transactions",
            "/api/v1/billing/transactions",
            true,
        ),
        (
            "/api/cloud-billing/batches",
            "/api/v1/billing/batches",
            true,
        ),
        ("/api/cloud-billing/topups", "/api/v1/billing/topups", true),
        (
            "/api/cloud-billing/balance",
            "/api/v1/billing/balance",
            false,
        ),
    ] {
        let rid = rid_of("ok");
        let uri = format!("{local}?page=2&page_size=20");
        let (status, _, _) = call(&app, wire("GET", &uri, Some(caller), &rid, None)).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        let expected = if with_query {
            format!("{upstream}?page=2&page_size=20")
        } else {
            upstream.to_string()
        };
        assert_eq!(calls(&rid)[0].target, expected, "{local}");
    }
}

/// 路径参数：合法值逐字拼进云侧路径；敌意值在**任何出站之前** 400。
#[tokio::test]
async fn the_session_path_parameter_is_allowlisted_before_any_outbound_work() {
    let app = test_app(cloud_at(&stub_base()));
    let caller = Uuid::new_v4();
    let rid_ok = rid_of("ok");
    let uri = "/api/cloud-billing/checkout-sessions/cs_test_abc";
    let (status, _, _) = call(&app, wire("GET", uri, Some(caller), &rid_ok, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        calls(&rid_ok)[0].target,
        "/api/v1/billing/checkout-sessions/cs_test_abc"
    );
    for hostile in [
        "cs_1%2F..%2Fbalance",
        "%2E%2E%2Fbalance",
        "cs%2Etest",
        "cs%3Fx%3D1",
    ] {
        let rid = rid_of("ok");
        let uri = format!("/api/cloud-billing/checkout-sessions/{hostile}");
        let (status, _, bytes) = call(&app, wire("GET", &uri, Some(caller), &rid, None)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
        assert_eq!(error_of(&bytes)["message"], MSG_SESSION_ID_INVALID, "{uri}");
        assert!(calls(&rid).is_empty(), "{uri} 不得产生出站请求");
    }
}

/// 体逐字转发（含前导空格与尾换行），并按上游的三步顺序判定。
#[tokio::test]
async fn the_checkout_body_is_forwarded_byte_for_byte_and_validated_in_order() {
    let app = test_app(cloud_at(&stub_base()));
    let caller = Uuid::new_v4();
    let raw = b"  {\"tier_id\":\"t1\",\"customer_email\":\"a@b.test\"}\n";
    let rid = rid_of("ok");
    let (status, _, _) = call(
        &app,
        wire(
            "POST",
            "/api/cloud-billing/checkout-sessions",
            Some(caller),
            &rid,
            Some(raw),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let outbound = calls(&rid);
    assert_eq!(outbound[0].body.as_bytes(), raw, "体逐字");
    assert_eq!(
        outbound[0].content_type.as_deref(),
        Some("application/json")
    );

    for (body, expected, message) in [
        (Vec::new(), StatusCode::BAD_REQUEST, MSG_BODY_REQUIRED),
        (b"  \n".to_vec(), StatusCode::BAD_REQUEST, MSG_BODY_REQUIRED),
        (b"{oops".to_vec(), StatusCode::BAD_REQUEST, MSG_BODY_INVALID),
        (
            vec![b'x'; MAX_CLOUD_RUNTIME_REQUEST_BODY_SIZE + 1],
            StatusCode::PAYLOAD_TOO_LARGE,
            MSG_BODY_TOO_LARGE,
        ),
    ] {
        let rid = rid_of("ok");
        let (status, _, bytes) = call(
            &app,
            wire(
                "POST",
                "/api/cloud-billing/checkout-sessions",
                Some(caller),
                &rid,
                Some(body.as_slice()),
            ),
        )
        .await;
        assert_eq!(status, expected, "{message}");
        assert_eq!(error_of(&bytes)["message"], message);
        assert!(calls(&rid).is_empty(), "{message} ⇒ 不得发出站");
    }
}

/// `portal-sessions` 与 checkout 的**唯一**差别：上游 `withBody` 没打开 ⇒ 不读体、不转发体。
#[tokio::test]
async fn portal_sessions_never_forwards_a_client_body() {
    let app = test_app(cloud_at(&stub_base()));
    let rid = rid_of("ok");
    let (status, _, _) = call(
        &app,
        wire(
            "POST",
            "/api/cloud-billing/portal-sessions",
            Some(Uuid::new_v4()),
            &rid,
            Some(b"{\"ignored\":true}"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let outbound = calls(&rid);
    assert!(outbound[0].body.is_empty(), "portal-sessions 不转发体");
    assert_eq!(outbound[0].content_type, None, "没有体 ⇒ 不设 Content-Type");
}

/// 云侧的 4xx/5xx **不是错误**（原样透传）；非 JSON / 全空白体只保留状态码。
#[tokio::test]
async fn cloud_side_statuses_and_bodies_are_passed_through_verbatim() {
    let app = test_app(cloud_at(&stub_base()));
    let caller = Uuid::new_v4();
    for (variant, expected, payload) in [
        ("status402", StatusCode::PAYMENT_REQUIRED, PAYLOAD_402),
        ("status500", StatusCode::INTERNAL_SERVER_ERROR, PAYLOAD_500),
    ] {
        let rid = rid_of(variant);
        let (status, _, bytes) = call(
            &app,
            wire(
                "GET",
                "/api/cloud-billing/balance",
                Some(caller),
                &rid,
                None,
            ),
        )
        .await;
        assert_eq!(status, expected, "{variant}");
        assert_eq!(bytes, payload.as_bytes(), "{variant} 体逐字");
    }
    let rid = rid_of("notjson");
    let (status, headers, bytes) = call(
        &app,
        wire(
            "GET",
            "/api/cloud-billing/balance",
            Some(caller),
            &rid,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bytes, b"not-json-marker:/api/v1/billing/balance");
    assert!(headers.get(CONTENT_TYPE).is_none(), "非 JSON ⇒ 不声明 JSON");
    let rid = rid_of("blank");
    let (status, _, bytes) = call(
        &app,
        wire(
            "GET",
            "/api/cloud-billing/balance",
            Some(caller),
            &rid,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(bytes.is_empty(), "全空白体 ⇒ 无体");
}

/// 三层语义矩阵（`docs/62` §2.5 的 A 行）**逐条 × 8 条路由**，且都不产生出站请求。
#[tokio::test]
async fn the_three_layer_matrix_holds_for_all_eight_routes() {
    let caller = Uuid::new_v4();
    let misconfigured = CloudConfig::from_env_with(|name| {
        (name == mc_cloud::CLOUD_URL_ENV).then(|| "https://u:p@cloud.test".to_string())
    });
    for (label, cloud, who, expected, code) in [
        (
            "未配置",
            cloud_disabled(),
            Some(caller),
            StatusCode::FORBIDDEN,
            CODE_NOT_CONFIGURED,
        ),
        (
            "配了但非法",
            misconfigured,
            Some(caller),
            StatusCode::INTERNAL_SERVER_ERROR,
            CODE_MISCONFIGURED,
        ),
        (
            "无会话",
            cloud_at(&stub_base()),
            None,
            StatusCode::UNAUTHORIZED,
            "unauthorized",
        ),
    ] {
        let app = test_app(cloud);
        for (method, local, _, body) in EIGHT {
            let rid = rid_of("ok");
            let (status, _, bytes) = call(&app, wire(method, local, who, &rid, body)).await;
            assert_eq!(status, expected, "{label}: {method} {local}");
            assert_eq!(error_of(&bytes)["code"], code, "{label}: {local}");
            assert!(calls(&rid).is_empty(), "{label}: {local} 不得产生出站请求");
        }
    }
}

/// 机器凭据闸（R-M9-2）：8 条 × 2 种来源逐条 403，且**不产生出站请求**。
#[tokio::test]
async fn machine_credentials_are_403_and_never_reach_the_wire() {
    let app = test_app(cloud_at(&stub_base()));
    let caller = Uuid::new_v4();
    for actor in ["task_token", "cloud_pat"] {
        for (method, local, _, body) in EIGHT {
            let rid = rid_of("ok");
            let request = as_machine(wire(method, local, Some(caller), &rid, body), actor);
            let (status, _, bytes) = call(&app, request).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{actor}: {method} {local}");
            // 上游写的是 `{"error": "<文本>"}`；本仓是嵌套信封，且 `message` 带
            // `Error::Forbidden` 的 `forbidden: ` 前缀（全仓同款，见 `docs/32` §46）。
            let message = error_of(&bytes)["message"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            assert!(
                message.contains(HUMAN_ACTOR_REQUIRED_MESSAGE),
                "{actor}: {method} {local}: {message}"
            );
            assert!(calls(&rid).is_empty(), "{actor}: {local} 不得产生出站请求");
        }
    }
    // 登记的口径（`docs/32` §46 的 D-1）：闸在链上**先于**会话提取 ⇒「无会话 + 机器凭据」
    // 本地 403（上游 401，chi 的 Auth 在 RequireHumanActor 之前）。这一格是**有意**的偏离。
    let rid = rid_of("ok");
    let request = as_machine(
        wire("GET", "/api/cloud-billing/balance", None, &rid, None),
        "task_token",
    );
    let (status, _, _) = call(&app, request).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// 传输层失败：502（连接被拒 / 体超限），**错误路径不回显**基址与云侧响应体（§2.4 判据 ③）。
#[tokio::test]
async fn transport_failures_are_502_and_never_echo_the_cloud_body() {
    let caller = Uuid::new_v4();
    let dead = dead_base();
    let app = test_app(cloud_at(&dead));
    let rid = rid_of("ok");
    let (status, _, bytes) = call(
        &app,
        wire(
            "GET",
            "/api/cloud-billing/balance",
            Some(caller),
            &rid,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(error_of(&bytes)["code"], CODE_FAILED);
    assert!(
        !String::from_utf8_lossy(&bytes).contains(&dead),
        "错误路径不得回显基址"
    );

    let app = test_app(cloud_at(&stub_base()));
    let rid = rid_of("huge");
    let (status, _, bytes) = call(
        &app,
        wire(
            "GET",
            "/api/cloud-billing/balance",
            Some(caller),
            &rid,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(
        !String::from_utf8_lossy(&bytes).contains(HUGE_MARKER),
        "错误路径不得回显云侧响应体"
    );
}

/// `docs/62` §2.6 的四行映射逐行（含离线替身端到端**打不到**的两个变体）。
#[tokio::test]
async fn the_transport_error_table_is_total() {
    for (error, expected, code) in [
        (
            CloudError::Disabled,
            StatusCode::FORBIDDEN,
            CODE_NOT_CONFIGURED,
        ),
        (CloudError::Transport, StatusCode::BAD_GATEWAY, CODE_FAILED),
        (
            CloudError::InvalidPath,
            StatusCode::BAD_GATEWAY,
            CODE_FAILED,
        ),
        (
            CloudError::InvalidJson,
            StatusCode::BAD_GATEWAY,
            CODE_FAILED,
        ),
        (
            CloudError::InvalidBaseUrl,
            StatusCode::INTERNAL_SERVER_ERROR,
            CODE_MISCONFIGURED,
        ),
        (
            CloudError::Timeout,
            StatusCode::GATEWAY_TIMEOUT,
            CODE_TIMEOUT,
        ),
        (
            CloudError::ResponseTooLarge { limit: 4 },
            StatusCode::BAD_GATEWAY,
            CODE_FAILED,
        ),
    ] {
        let response = transport_error(&error);
        assert_eq!(response.status(), expected, "{error}");
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        assert_eq!(error_of(&bytes)["code"], code, "{error}");
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("billing/"), "错误体不得回显云侧路径");
    }
}

/// 真库那一半（门 ⑥，`MULTICA_TEST_DATABASE_URL`）：真库在场时 8 条仍然只做代理。
///
/// 「纯代理」的**结构性**证据 = 本面在真库里**没有**任何可写的表（`docs/62` §9.4 双侧实测）。
/// 为什么不比行数差：门 ⑥ 用 `--ignored` 跑整个 mc-http 的 DB 套件，别的用例会在同一张
/// `activity_log` 上并发写 ⇒ 行数差**不是**确定判据（`docs/32` §46 的 D-7 登记了这一处）。
#[tokio::test]
#[ignore = "needs PostgreSQL via MULTICA_TEST_DATABASE_URL (gate ⑥)"]
async fn a_real_database_holds_no_billing_table_for_the_eight_proxy_routes() {
    let Some(url) = std::env::var("MULTICA_TEST_DATABASE_URL").ok() else {
        eprintln!("MULTICA_TEST_DATABASE_URL not set; skipping");
        return;
    };
    let db = Db::connect(&url, 4, 0).await.expect("connect");
    assert!(db.pool().size() >= 1, "真库连接必须真的建立");
    let app = test_app_with(db.clone(), cloud_at(&stub_base()));
    let caller = Uuid::new_v4();
    for (method, local, upstream, body) in EIGHT {
        let rid = rid_of("ok");
        let (status, _, bytes) = call(&app, wire(method, local, Some(caller), &rid, body)).await;
        assert_eq!(status, StatusCode::OK, "{method} {local}");
        assert!(
            String::from_utf8_lossy(&bytes).contains(upstream),
            "{local} 的响应必须是云侧的逐字体"
        );
    }
    let billing_tables: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.tables WHERE table_schema = 'public' \
         AND (table_name LIKE 'cloud_billing%' OR table_name LIKE 'cloud_subscription%' \
         OR table_name LIKE 'checkout_session%' OR table_name LIKE 'topup%' \
         OR table_name LIKE 'stripe_event%')",
    )
    .fetch_one(db.pool())
    .await
    .expect("information_schema");
    assert_eq!(
        billing_tables, 0,
        "billing / subscriptions / stripe 面本地 0 张表"
    );
}
