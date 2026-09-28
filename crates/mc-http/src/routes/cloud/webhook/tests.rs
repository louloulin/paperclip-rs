//! M9-6 的证据面（`docs/62` §4.2 的「离线替身」+ §6.5 的 M9-6 行专属判据）。
//!
//! 覆盖：四段顺序（**403 → 429 → 401 → 413** → 逐字回写）、原始体**字节级**直通、
//! `X-User-ID` **不注入**、限流器**复用** M5-5 的那一个（不新写）。

use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use axum::body::Body as AxumBody;
use axum::http::Request as HttpRequest;
use http_body_util::BodyExt as _;
use mc_autopilot::webhook::ratelimit::{
    default_webhook_absolute_ip_rate_limit, SlidingWindowRateLimit, WEBHOOK_ABSOLUTE_IP_LIMITER,
};
use mc_core::actor::ActorRegistry;
use mc_db::Db;
use mc_feature_flags::FeatureFlagCatalog;
use mc_realtime::{RealtimeHandle, WsState};
use serde_json::json;
use tower::ServiceExt as _;

use super::*;
use crate::state::cloud::{CloudConfig, EntitlementConfig};
use crate::state::integrations::{ComposioKeys, GithubKeys, VcsKeys};
use crate::state::{
    AdapterRegistry, ChannelKeys, ConfigSnapshot, GoogleOAuthConfig, RuntimeHandles,
};

/// 出站路径的**字面量**（`mc_core::cloud::STRIPE_WEBHOOK_UPSTREAM_PATH` 的独立复核）。
const UPSTREAM_PATH: &str = "/api/v1/webhooks/stripe";
/// 本地路径的**字面量**（与 `router()` 里的注册逐字相同；⑦ 门负责路由表那一半）。
const LOCAL_PATH: &str = "/api/webhooks/stripe";

/// 一个**不是**合法 JSON、带首尾空白与 NUL 的体 —— 逐字转发时它必须**一个字节不差**地出去。
const RAW_BODY: &[u8] = b"  {\"id\" : \"evt\"}\n\x00  ";

// -----------------------------------------------------------------------
// 替身（§4.2：只替平台 wire）
// -----------------------------------------------------------------------

#[derive(Debug, Clone)]
struct StubCall {
    method: String,
    path: String,
    raw_body: Vec<u8>,
    signatures: Vec<String>,
    user_id: Option<String>,
    request_id: Option<String>,
    content_types: Vec<String>,
}

static CALLS: Mutex<Vec<StubCall>> = Mutex::new(Vec::new());
static STUB_BASE: OnceLock<String> = OnceLock::new();

fn stub_base() -> String {
    STUB_BASE
        .get_or_init(|| {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind stub");
            listener.set_nonblocking(true).expect("non-blocking");
            let port = listener.local_addr().expect("stub addr").port();
            std::thread::Builder::new()
                .name("cloud-webhook-stub".to_string())
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

/// 一个**没人听**的基址（连接必然被拒）—— 端到端触发 502 那一支。
fn dead_base() -> String {
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

fn all_of(headers: &HeaderMap, name: &str) -> Vec<String> {
    headers
        .get_all(name)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .map(str::to_string)
        .collect()
}

async fn stub(request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let raw = axum::body::to_bytes(body, 8 << 20)
        .await
        .map(|bytes| bytes.to_vec())
        .unwrap_or_default();
    let path = parts.uri.path().to_string();
    let marker = String::from_utf8_lossy(&raw).to_string();
    CALLS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(StubCall {
            method: parts.method.to_string(),
            path: path.clone(),
            user_id: header_of(&parts.headers, "x-user-id"),
            request_id: header_of(&parts.headers, "x-request-id"),
            signatures: all_of(&parts.headers, "stripe-signature"),
            content_types: all_of(&parts.headers, "content-type"),
            raw_body: raw,
        });
    // 替身按**体里的标记**分派：云侧自己的 4xx/5xx 必须被本地原样透传。
    if marker.contains("stub-402") {
        return (
            StatusCode::PAYMENT_REQUIRED,
            json!({"error": "payment_required"}).to_string(),
        )
            .into_response();
    }
    if marker.contains("stub-500") {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "cloud-side-exploded".to_string(),
        )
            .into_response();
    }
    (StatusCode::OK, json!({"ok": path}).to_string()).into_response()
}

/// 本文件自己的那几笔出站调用（`CALLS` 是进程级静态而用例并行跑 ⇒ 按**唯一签名**认领，
/// 见 `cloud/billing/tests.rs` 的同款纪律）。
fn calls_for(signature: &str) -> Vec<StubCall> {
    CALLS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .filter(|call| call.signatures.first().map(String::as_str) == Some(signature))
        .cloned()
        .collect()
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

fn test_app(cloud: CloudConfig) -> Router {
    // 懒连接池（不拨号）：这条路由一次都不碰库。
    let db = Db::connect_lazy("postgres://cloud:cloud@127.0.0.1:1/none", 1, 0).expect("lazy db");
    let realtime = RealtimeHandle::start(8);
    let ws = Arc::new(WsState::new(realtime.clone(), "cloud-webhook-test"));
    let state = Arc::new(AppState {
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
    });
    crate::apply_default_middleware(crate::routes::router(state.clone())).with_state(state)
}

/// 本用例唯一的一枚签名（`t=1,v1=<uuid>`）—— 替身按它认领，用例按它取回自己的那笔。
fn signature() -> String {
    format!("t=1,v1={}", uuid::Uuid::new_v4())
}

fn post(signature: Option<&str>, body: &[u8]) -> HttpRequest<AxumBody> {
    let mut builder = HttpRequest::builder()
        .method("POST")
        .uri(LOCAL_PATH)
        .header("content-type", "application/json");
    if let Some(signature) = signature {
        builder = builder.header("stripe-signature", signature);
    }
    builder
        .body(AxumBody::from(body.to_vec()))
        .expect("request")
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

fn code_of(body: &[u8]) -> String {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value["error"]["code"]
                .as_str()
                .map(str::to_string)
                .or_else(|| value["error"].as_str().map(str::to_string))
        })
        .unwrap_or_default()
}

// -----------------------------------------------------------------------
// 判据 1：四段**顺序**（`docs/62` §6.5 的 M9-6 行第 2 条）
// -----------------------------------------------------------------------

/// 步 1 优先于其余三段：云侧未配置 ⇒ **403**，即使请求同时「缺签名 + 超大体 + 被限流」。
///
/// 这条是上游第一个 `if`（`cloud_billing.go:521–524`）的本地形态，也是
/// `docs/62` §9.5 那份 ①②③ 清单**不是**顺序的证据。
#[tokio::test]
async fn unconfigured_cloud_is_403_before_everything_else() {
    let app = test_app(cloud_disabled());
    let oversized = vec![b'x'; stripe::MAX_STRIPE_WEBHOOK_BODY_SIZE + 1];
    // 缺签名
    let (status, _, body) = call(&app, post(None, b"{\"id\":\"evt\"}")).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "{}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(code_of(&body), "cloud_runtime_not_configured");
    // 有签名但超大体 ⇒ 仍是 403（不是 413）
    let (status, _, body) = call(&app, post(Some(&signature()), &oversized)).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "{}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(code_of(&body), "cloud_runtime_not_configured");
}

/// 步 1 的另一侧：配了但非法 ⇒ **500** `cloud_runtime_misconfigured`（403 只归「未配置」）。
#[tokio::test]
async fn misconfigured_cloud_is_500() {
    // 带 userinfo 的基址：非法。
    let cloud = CloudConfig::from_env_with(|name| {
        (name == mc_cloud::CLOUD_URL_ENV).then(|| "http://user:pw@cloud.invalid".to_string())
    });
    let app = test_app(cloud);
    let (status, _, body) = call(&app, post(Some(&signature()), b"{\"id\":\"evt\"}")).await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "{}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(code_of(&body), "cloud_runtime_misconfigured");
}

/// 步 2 优先于步 3/4：小配额替身 + **缺签名 + 超大体** ⇒ 429（不是 401 / 413）。
#[tokio::test]
async fn rate_limit_precedes_signature_and_body_checks() {
    let limiter =
        SlidingWindowLimiter::new(SlidingWindowRateLimit::new(1, Duration::from_secs(60)));
    let ip = Some("203.0.113.7".to_string());
    assert!(gate_by_ip(&limiter, ip.as_deref()).is_none(), "第一次放行");
    let response = gate_by_ip(&limiter, ip.as_deref()).expect("第二次 429");
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
}

/// 限流键是 **per-IP**：换一个 IP 就是一份新配额。
#[tokio::test]
async fn the_rate_limit_key_is_the_peer_ip() {
    let limiter =
        SlidingWindowLimiter::new(SlidingWindowRateLimit::new(1, Duration::from_secs(60)));
    assert!(gate_by_ip(&limiter, Some("203.0.113.1")).is_none());
    assert!(
        gate_by_ip(&limiter, Some("203.0.113.2")).is_none(),
        "不同 IP 互不影响"
    );
    assert!(gate_by_ip(&limiter, Some("203.0.113.1")).is_some());
}

/// **无 ip ⇒ 跳过**（逐字跟上游 `if ip := h.clientIPForRateLimit(r); ip != ""`）。
///
/// 这一条决定了 ⑨ 的 `TestStripeWebhookRateLimited` 为什么在本仓**结构上不可复现**：
/// `mc-conformance` 直连 `Router::oneshot`，**没有 `ConnectInfo`** ⇒ 这道闸按上游语义跳过。
#[tokio::test]
async fn missing_peer_address_skips_the_gate_verbatim() {
    let limiter =
        SlidingWindowLimiter::new(SlidingWindowRateLimit::new(1, Duration::from_secs(60)));
    for _ in 0..5 {
        assert!(
            gate_by_ip(&limiter, None).is_none(),
            "拿不到对端地址 ⇒ 不记账、不拒（上游逐字）"
        );
    }
}

/// 步 3 优先于步 4：缺签名 + 超大体 ⇒ **401**（体压根没被读）。
#[tokio::test]
async fn signature_check_precedes_the_body_cap() {
    let app = test_app(cloud_at(&stub_base()));
    let oversized = vec![b'x'; stripe::MAX_STRIPE_WEBHOOK_BODY_SIZE + 1];
    let (status, _, body) = call(&app, post(None, &oversized)).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "{}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(code_of(&body), "unauthorized");
}

/// 步 3：显式 `Stripe-Signature: `（空串 / 纯空白）**也算缺失**。
///
/// 上游的**注释**明写「a header explicitly set to `""` still counts as missing」，
/// 但它的**代码**是 `len(Values(...)) == 0`（`Set(k, "")` 之后长度是 1）⇒ 两者不一致。
/// 本仓跟注释（`docs/62` §6.5 的 M9-6 行），偏离登记在 `docs/32` §9.13 的 D-2。
#[tokio::test]
async fn explicitly_empty_signature_is_401() {
    let app = test_app(cloud_at(&stub_base()));
    for value in ["", "   "] {
        let (status, _, body) = call(&app, post(Some(value), b"{\"id\":\"evt\"}")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "value={value:?}");
        let parsed: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(parsed["error"]["message"], MSG_MISSING_SIGNATURE);
    }
}

/// 步 4：有签名 + 超 1 MiB ⇒ **413**，且**零出站**（上游 `TestStripeWebhookRejectsLargeBody`）。
#[tokio::test]
async fn body_over_one_mib_is_413_without_any_upstream_call() {
    let app = test_app(cloud_at(&stub_base()));
    let marker = signature();
    let oversized = vec![b'x'; stripe::MAX_STRIPE_WEBHOOK_BODY_SIZE + 1];
    let (status, _, body) = call(&app, post(Some(&marker), &oversized)).await;
    assert_eq!(
        status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "{}",
        String::from_utf8_lossy(&body)
    );
    assert!(calls_for(&marker).is_empty(), "413 ⇒ 不得有任何出站请求");
}

/// 边界：恰好 1 MiB **放行**（`>` 而不是 `>=`）。
#[tokio::test]
async fn exactly_one_mib_is_forwarded() {
    let app = test_app(cloud_at(&stub_base()));
    let marker = signature();
    let exact = vec![b'x'; stripe::MAX_STRIPE_WEBHOOK_BODY_SIZE];
    let (status, _, body) = call(&app, post(Some(&marker), &exact)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let calls = calls_for(&marker);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].raw_body.len(), exact.len());
}

/// **空体照发**（上游 `TestStripeWebhookForwardsEmptyBody`：Stripe 的 tester 会发空 ping，
/// 本地不预判空体，判定权在云侧）。
#[tokio::test]
async fn empty_body_is_forwarded_not_rejected() {
    let app = test_app(cloud_at(&stub_base()));
    let marker = signature();
    let (status, _, body) = call(&app, post(Some(&marker), b"")).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let calls = calls_for(&marker);
    assert_eq!(calls.len(), 1);
    assert!(calls[0].raw_body.is_empty());
}

// -----------------------------------------------------------------------
// 判据 2：原始体**字节级**直通 / `X-User-ID` 不注入 / 出站头
// -----------------------------------------------------------------------

/// 原始体逐字到云侧：一个字节都不改（不 trim、不重编码、不解析）。
#[tokio::test]
async fn raw_body_reaches_the_cloud_byte_for_byte() {
    let app = test_app(cloud_at(&stub_base()));
    let marker = signature();
    let (status, _, body) = call(&app, post(Some(&marker), RAW_BODY)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let calls = calls_for(&marker);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].raw_body, RAW_BODY);
    assert_eq!(calls[0].path, UPSTREAM_PATH);
    assert_eq!(calls[0].method, "POST");
}

/// 出站的 `Stripe-Signature` / `Content-Type` 都**原样**带走（不 trim、不加前缀）。
#[tokio::test]
async fn signature_and_content_type_are_forwarded_verbatim() {
    let app = test_app(cloud_at(&stub_base()));
    let marker = signature();
    let (status, _, _) = call(&app, post(Some(&marker), RAW_BODY)).await;
    assert_eq!(status, StatusCode::OK);
    let calls = calls_for(&marker);
    assert_eq!(calls[0].signatures, vec![marker.clone()]);
    assert_eq!(calls[0].content_types, vec!["application/json".to_string()]);
    assert_eq!(
        calls[0].request_id, None,
        "客户端没给 X-Request-ID ⇒ 不盖章"
    );
}

/// 同名**多值**头一个不少地转过去（上游 `headers[k] = sigs` 是 map 赋值，不是 `Get`）。
#[tokio::test]
async fn multi_valued_signature_header_is_all_forwarded() {
    let app = test_app(cloud_at(&stub_base()));
    let marker = signature();
    let mut request = post(Some(&marker), RAW_BODY);
    request.headers_mut().append(
        "stripe-signature",
        HeaderValue::from_static("t=2,v1=second"),
    );
    let (status, _, _) = call(&app, request).await;
    assert_eq!(status, StatusCode::OK);
    let calls = calls_for(&marker);
    assert_eq!(
        calls[0].signatures,
        vec![marker.clone(), "t=2,v1=second".to_string()]
    );
}

/// 客户端给了 `X-Request-ID` ⇒ 出站带上它。
#[tokio::test]
async fn request_id_is_stamped_from_the_caller() {
    let app = test_app(cloud_at(&stub_base()));
    let marker = signature();
    let mut request = post(Some(&marker), RAW_BODY);
    request
        .headers_mut()
        .insert("x-request-id", HeaderValue::from_static("req-m9-6"));
    let (status, _, _) = call(&app, request).await;
    assert_eq!(status, StatusCode::OK);
    let calls = calls_for(&marker);
    assert_eq!(calls[0].request_id.as_deref(), Some("req-m9-6"));
}

/// 🔴 `X-User-ID` **不注入**（上游注释逐字：这条**没有**人类身份）。
#[tokio::test]
async fn no_user_id_is_injected() {
    let app = test_app(cloud_at(&stub_base()));
    let marker = signature();
    // 客户端**自己**带会话身份 + 机器凭据头：也**不许**变成出站的 `X-User-ID`。
    let mut request = post(Some(&marker), RAW_BODY);
    {
        let headers = request.headers_mut();
        headers.insert(
            "x-multica-session",
            HeaderValue::from_static("00000000-0000-0000-0000-000000000001"),
        );
        headers.insert(
            "x-multica-user-id",
            HeaderValue::from_static("00000000-0000-0000-0000-000000000001"),
        );
        headers.insert("x-actor-source", HeaderValue::from_static("task_token"));
    }
    let (status, _, body) = call(&app, request).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let calls = calls_for(&marker);
    assert_eq!(calls[0].user_id, None, "X-User-ID 绝不注入");
}

/// 幂等：**同一请求重投 ⇒ 转发两次、本地不落任何状态**（`docs/62` §9.5 的第二条「有意等价」）。
#[tokio::test]
async fn replaying_the_same_delivery_forwards_twice() {
    let app = test_app(cloud_at(&stub_base()));
    let marker = signature();
    for _ in 0..2 {
        let (status, _, body) = call(&app, post(Some(&marker), RAW_BODY)).await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    }
    let calls = calls_for(&marker);
    assert_eq!(calls.len(), 2, "本地不做事件去重（幂等由云侧负责）");
    assert_eq!(calls[0].raw_body, calls[1].raw_body);
}

/// 云侧 4xx/5xx **原样**透传（云侧是最终授权方；`transport::Client::send` 对任何状态都
/// 返回 `Ok(Response)`）—— 402 与 500 各一条。
#[tokio::test]
async fn cloud_side_errors_are_passed_through_verbatim() {
    let app = test_app(cloud_at(&stub_base()));
    for (marker_body, expected) in [
        (
            br#"{"id":"evt","stub-402":true}"#.as_slice(),
            StatusCode::PAYMENT_REQUIRED,
        ),
        (
            br#"{"id":"evt","stub-500":true}"#.as_slice(),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    ] {
        let marker = signature();
        let (status, _, body) = call(&app, post(Some(&marker), marker_body)).await;
        assert_eq!(status, expected, "{}", String::from_utf8_lossy(&body));
        assert_eq!(calls_for(&marker).len(), 1, "云侧的错也**不许**重试出站");
    }
}

/// 传输失败（连接被拒）⇒ **502**，且**不回显**云侧体 / 出站 URL。
#[tokio::test]
async fn transport_failure_is_502_without_leaking_the_url() {
    let app = test_app(cloud_at(&dead_base()));
    let (status, _, body) = call(&app, post(Some(&signature()), RAW_BODY)).await;
    assert_eq!(
        status,
        StatusCode::BAD_GATEWAY,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let text = String::from_utf8_lossy(&body);
    assert!(!text.contains("127.0.0.1"), "{text}");
    assert_eq!(code_of(&body), "upstream_error");
}

/// 错误信封是**嵌套**的（`{"error":{"code":…,"message":…}}`）且 message 是静态串。
#[tokio::test]
async fn error_envelope_is_the_nested_repo_shape() {
    let app = test_app(cloud_disabled());
    let (_, _, body) = call(&app, post(Some(&signature()), RAW_BODY)).await;
    let parsed: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert!(parsed["error"]["code"].is_string());
    assert!(parsed["error"]["message"].is_string());
    assert!(parsed.get("code").is_none(), "不许扁平信封：{parsed}");
}

// -----------------------------------------------------------------------
// 判据 3：限流器是**复用**的（`docs/62` §9.5 末段）
// -----------------------------------------------------------------------

/// 生产路径用的就是 M5-5 的那个进程级单例，且它的配额就是上游那条绝对 IP 天花板。
#[test]
fn the_production_gate_is_the_shared_absolute_ip_limiter() {
    let limit = default_webhook_absolute_ip_rate_limit();
    assert_eq!(
        limit.limit, 600,
        "上游 DefaultWebhookAbsoluteIPRateLimit 逐字"
    );
    assert_eq!(limit.window, Duration::from_secs(60));
    // `rate_limited` 传的就是这个单例（签名上收 `&SlidingWindowLimiter`，
    // 真实闸 600/60s 打不满 ⇒ 429 那一支由 `gate_by_ip` 的小配额替身判）。
    let _ = &WEBHOOK_ABSOLUTE_IP_LIMITER;
}
