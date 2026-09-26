//! `GET /health/realtime`（M10-3）的可判定用例 —— **零数据库**。
//!
//! 上游判据来源（三件）：`server/cmd/server/health_realtime.go`（106 行，`f41fae6b08fb`）
//! 的四态访问门；`internal/realtime/metrics.go::Snapshot()`（13 个顶层键）；
//! `internal/daemonws/metrics.go::Snapshot()`（14 个键，挂在 `daemonws` 子对象下）。
//! ⑨ 门在这条路径上**没有** fixture（`docs/64` §6.2 ⇒ 本片不动 `report.json`）⇒
//! 字段级判据的唯一一处就是本文件 + `realtime.rs` 的模块文档。
//!
//! ## 四态访问门各一条（`docs/64` §2.3 的判据表）
//!
//! | # | 情形 | 判据 |
//! | :-: | --- | --- |
//! | 1 | token 已设 + 正确 `Authorization: Bearer <token>` | **200** + `application/json` + `Cache-Control: no-store` |
//! | 2 | token 已设 + 缺失 / 错误 / 空白 token | **401** + `WWW-Authenticate: Bearer realm="metrics"` + 纯文本 `unauthorized` |
//! | 3 | token 未设 + 直连 loopback 且无转发头 | **200** |
//! | 4 | token 未设 + 非 loopback **或任一**转发头 | **404**（"不向远程扫描器宣告它的存在"） |
//!
//! 第 3/4 条靠**注入** `ConnectInfo` 伪造 `RemoteAddr`（本仓既有手法：
//! `crates/mc-http/tests/autopilots/webhook_support.rs:95` 的 `extensions_mut().insert(ConnectInfo(..))`），
//! token 靠 [`super::router_with_token`] 注入 —— **不读进程 env**（env 是进程级的，
//! 并发用例之间会互相干扰）。
//!
//! ## 为什么全部用例都不需要真库
//!
//! handler 不挂 `State<AppState>`（只挂 token 的 `Extension`）⇒ 类型上就拿不到 `Db`。
//! 本文件仍按 `live/tests.rs` 的手势把库装成"真 URL 但不可达"（`connect_lazy`），
//! 任何一次真查询都会在 5s acquire 超时后失败 ⇒ 200 只可能来自"根本没碰库"。

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::{normalize_token, router_with_token, token_from_env};
use crate::state::{AdapterRegistry, AppState, ConfigSnapshot, RuntimeHandles};

/// 真 URL、**不可达**地址 —— `mc-conformance/src/harness.rs::STATELESS_URL` 的同款形状。
const UNREACHABLE_DB: &str = "postgres://realtime-probe:realtime-probe@127.0.0.1:1/realtime_probe";

/// 用例用的 token：**不**经进程 env（见模块头）。
const TOKEN: &str = "s3cret-metrics-token";

/// 上游 `realtime/metrics.go::Snapshot()` 的 **13 个顶层键**（逐字，`docs/64` §2.3）。
const REALTIME_KEYS: [&str; 13] = [
    "connects_total",
    "disconnects_total",
    "active_connections",
    "slow_evictions_total",
    "messages_sent_total",
    "messages_dropped_total",
    "inbound_too_large_total",
    "events_sent_by_type",
    "subscribes_total",
    "unsubscribes_total",
    "subscribe_denied_total",
    "active_scope_rooms",
    "redis",
];

/// 上游 `daemonws/metrics.go::Snapshot()` 的 **14 个键**（逐字），整体挂在 `daemonws` 下。
const DAEMONWS_KEYS: [&str; 14] = [
    "connects_total",
    "disconnects_total",
    "active_connections",
    "slow_evictions_total",
    "wakeup_published_total",
    "wakeup_publish_errors",
    "wakeup_received_total",
    "wakeup_delivered_hit_total",
    "wakeup_delivered_miss_total",
    "runtime_gone_delivered_hit_total",
    "runtime_gone_delivered_miss_total",
    "runtime_gone_published_total",
    "runtime_gone_publish_errors",
    "runtime_gone_received_total",
];

/// 上游 `realtime/metrics.go` 的 `redis` 子树（**20** 个键，实测）。
///
/// 🔴 `docs/64` §2.3 与切片描述写的"16 个键 + `last_error:null`"是**计划期估算**；
/// 上游 `Snapshot()` 实测是 20 个键（含 `streams{}` / `last_error`）。`last_error` 是 Go
/// `string` ⇒ 零值是 `""` 而**不是** `null`。更正登记在 `docs/32` §42 与
/// `crates/mc-ws/src/hub/metrics.rs::redis_snapshot` 的文档上。
const REDIS_KEYS: [&str; 20] = [
    "connected",
    "node_id",
    "xadd_total",
    "xadd_errors",
    "xread_total",
    "xread_errors",
    "ack_total",
    "last_xadd_lag_micros",
    "mirror_primary_errors",
    "mirror_secondary_errors",
    "mirror_divergence_total",
    "stream_trimmed_total",
    "stream_missing_total",
    "retention_errors",
    "streams_without_ttl",
    "used_memory_bytes",
    "max_memory_bytes",
    "evicted_keys",
    "streams",
    "last_error",
];

/// 上游 `hasForwardingHeader` 的五个头（逐字）。
const FORWARDING_HEADERS: [&str; 5] = [
    "X-Forwarded-For",
    "X-Forwarded-Host",
    "X-Forwarded-Proto",
    "X-Real-Ip",
    "Forwarded",
];

/// 装一个 `AppState`：库**不可达**，其余按 `mc-conformance` 的 stateless 层同款。
fn state() -> Arc<AppState> {
    let db = mc_db::Db::connect_lazy(UNREACHABLE_DB, 1, 0).expect("lazy pool");
    let realtime = mc_realtime::RealtimeHandle::start(8);
    let ws = Arc::new(mc_realtime::WsState::new(realtime.clone(), "lum-2105"));
    Arc::new(AppState::new(
        db,
        RuntimeHandles {
            actors: mc_core::actor::ActorRegistry::new(),
            adapters: Arc::new(AdapterRegistry::default()),
        },
        ConfigSnapshot {
            host: "127.0.0.1".into(),
            port: 0,
            session_cookie: "multica_session".into(),
            api_key_header: "X-Multica-Api-Key".into(),
            csrf_header: "X-Multica-Csrf".into(),
            ..Default::default()
        },
        realtime,
        ws,
    ))
}

/// 只装**本切片**的 router（token 由用例给定）—— 四态门的单元级判据。
fn probe_router(token: Option<&str>) -> Router {
    let state = state();
    router_with_token(state.clone(), token.map(str::to_owned)).with_state(state)
}

/// 装**全量** router（与 `apps/mc-server/src/main.rs` 同款装配）——
/// 判据是"`mount_slice_probes()` 的 `.merge` 真的把这条键挂到了**根路径**上"。
fn full_router() -> Router {
    let state = state();
    crate::routes::router(state.clone()).with_state(state)
}

/// 一次响应（状态码 + 头 + 原始 body + 解析出的 JSON）。
struct Res {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
    json: Value,
}

impl Res {
    fn header(&self, name: header::HeaderName) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    }

    /// 原始 body 文本（`unauthorized\n` / `404 page not found\n` 这类**非 JSON** 判据用）。
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// 顶层键（排序后）。
    fn keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self
            .json
            .as_object()
            .map(|object| object.keys().cloned().collect())
            .unwrap_or_default();
        keys.sort_unstable();
        keys
    }

    /// 某个子对象的键（排序后）；不是对象 ⇒ panic。
    fn keys_of(&self, path: &str) -> Vec<String> {
        let value = self
            .json
            .get(path)
            .unwrap_or_else(|| panic!("响应里没有 `{path}` 键：{}", self.json));
        let mut keys: Vec<String> = value
            .as_object()
            .unwrap_or_else(|| panic!("`{path}` 不是对象：{value}"))
            .keys()
            .cloned()
            .collect();
        keys.sort_unstable();
        keys
    }
}

/// `GET <uri>`，可注入伪造的 peer 地址与请求头。
///
/// `peer = None` ⇒ **不注入** `ConnectInfo`（生产装配一定注入；测试直连 router 时不注入）
/// —— 那正是上游"`net.ParseIP` 拿不到地址"的同款情形，判据必须是 **fail closed**。
async fn get(router: &Router, uri: &str, peer: Option<&str>, headers: &[(&str, &str)]) -> Res {
    let mut builder = Request::builder().method("GET").uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let mut request = builder.body(Body::empty()).expect("build request");
    if let Some(peer) = peer {
        let addr: SocketAddr = peer
            .parse()
            .unwrap_or_else(|err| panic!("测试 peer 地址 `{peer}` 解析失败：{err}"));
        request.extensions_mut().insert(ConnectInfo(addr));
    }
    let resp = router.clone().oneshot(request).await.expect("dispatch");
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Res {
        status,
        headers,
        body: bytes.to_vec(),
        json,
    }
}

/// 带 **Bearer 头**的一次 `GET`：`Authorization` 头的 `String` 生命周期留在本函数内
/// （[`get`] 只借 `&str`，所以带 token 的调用都走这个入口）。
///
/// `extra` 是额外要注入的头（转发头等），排在 bearer 之后。
async fn get_with_bearer(
    router: &Router,
    uri: &str,
    peer: Option<&str>,
    token: &str,
    extra: &[(&str, &str)],
) -> Res {
    let auth = format!("Bearer {token}");
    let mut headers: Vec<(&str, &str)> = vec![("Authorization", auth.as_str())];
    headers.extend_from_slice(extra);
    get(router, uri, peer, &headers).await
}

/// 排序后的期望键（`[&str; N]` → `Vec<String>`）。
fn sorted(keys: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = keys.iter().map(|key| (*key).to_owned()).collect();
    out.sort_unstable();
    out
}

// ---- 状态 #1：token 已设 + 正确 Bearer ⇒ 200 -----------------------------------------

/// 四态之一：**200** + `application/json` + `Cache-Control: no-store` +
/// 14 个顶层键（13 realtime + `daemonws`）+ `daemonws` 子对象 14 个键。
#[tokio::test]
async fn state_token_and_valid_bearer_is_200_with_snapshot() {
    let res = get_with_bearer(
        &probe_router(Some(TOKEN)),
        "/health/realtime",
        Some("127.0.0.1:40000"),
        TOKEN,
        &[],
    )
    .await;

    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(
        res.header(header::CONTENT_TYPE).as_deref(),
        Some("application/json"),
        "上游显式 `w.Header().Set(\"Content-Type\", \"application/json\")`"
    );
    assert_eq!(
        res.header(header::CACHE_CONTROL).as_deref(),
        Some("no-store"),
        "上游显式 `Cache-Control: no-store`"
    );

    // 14 个顶层键 = realtime 的 13 个 + `daemonws`（上游 `snapshot[\"daemonws\"] = …` 那一行）。
    let mut expected = sorted(&REALTIME_KEYS);
    expected.push("daemonws".to_owned());
    expected.sort_unstable();
    assert_eq!(res.keys(), expected, "顶层键集合必须逐字等于上游");

    assert_eq!(
        res.keys_of("daemonws"),
        sorted(&DAEMONWS_KEYS),
        "`daemonws` 子对象是上游 `daemonws.Metrics.Snapshot()` 的 14 个键"
    );
    assert_eq!(
        res.keys_of("redis"),
        sorted(&REDIS_KEYS),
        "`redis` 子树必须**照发**（本仓无 Redis ⇒ 单副本缺省值，但一个键都不能删）"
    );
}

/// `Bearer` 前缀比较**不分大小写**（上游 `strings.EqualFold`）⇒ 小写 `bearer` 也放行；
/// 前缀之后的 token 还要 `TrimSpace`（上游 `strings.TrimSpace(auth[len(prefix):])`）。
#[tokio::test]
async fn bearer_prefix_is_case_insensitive_like_upstream() {
    for value in [
        format!("bearer {TOKEN}"),
        format!("BEARER {TOKEN}"),
        format!("BeArEr   {TOKEN}  "),
    ] {
        let headers = [("Authorization", value.as_str())];
        let res = get(
            &probe_router(Some(TOKEN)),
            "/health/realtime",
            Some("127.0.0.1:40000"),
            &headers,
        )
        .await;
        assert_eq!(
            res.status,
            StatusCode::OK,
            "上游 `strings.EqualFold(auth[:len(prefix)], \"Bearer \")` ⇒ `{value}` 必须放行"
        );
    }
}

/// token 分支**先于** loopback 判定（上游 `if token != "" { … } else if …`）⇒
/// 带 token 的请求即使来自非 loopback、带着转发头，也只看 Bearer。
#[tokio::test]
async fn state_token_branch_ignores_peer_and_forwarding_headers() {
    let res = get_with_bearer(
        &probe_router(Some(TOKEN)),
        "/health/realtime",
        Some("203.0.113.7:40000"),
        TOKEN,
        &[
            ("X-Forwarded-For", "10.0.0.1"),
            ("Forwarded", "for=10.0.0.1"),
        ],
    )
    .await;
    assert_eq!(res.status, StatusCode::OK);
}

// ---- 状态 #2：token 已设 + 缺失/错误/空白 ⇒ 401 ---------------------------------------

/// 四态之二：**401** + `WWW-Authenticate: Bearer realm="metrics"` + Go `http.Error` 的
/// 纯文本 body（`unauthorized\n`，`text/plain; charset=utf-8` + `nosniff`）。
#[tokio::test]
async fn state_token_with_missing_or_wrong_bearer_is_401_with_challenge() {
    let cases: [(&str, Vec<(&str, String)>); 6] = [
        ("头完全没有", vec![]),
        ("空值", vec![("Authorization", String::new())]),
        (
            "错误的 token",
            vec![("Authorization", "Bearer not-the-token".to_owned())],
        ),
        (
            "只差一个字符",
            vec![(
                "Authorization",
                format!("Bearer {}", &TOKEN[..TOKEN.len() - 1]),
            )],
        ),
        ("只有前缀没有值", vec![("Authorization", "Bearer  ".into())]),
        (
            "另一种方案",
            vec![("Authorization", format!("Basic {TOKEN}"))],
        ),
    ];

    for (what, headers) in cases {
        let headers: Vec<(&str, &str)> = headers
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();
        let res = get(
            &probe_router(Some(TOKEN)),
            "/health/realtime",
            Some("127.0.0.1:40000"),
            &headers,
        )
        .await;

        assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{what} ⇒ 401");
        assert_eq!(
            res.header(header::WWW_AUTHENTICATE).as_deref(),
            Some("Bearer realm=\"metrics\""),
            "{what} ⇒ 上游逐字的挑战头"
        );
        assert_eq!(
            res.text(),
            "unauthorized\n",
            "{what} ⇒ Go http.Error 的 body"
        );
        assert_eq!(
            res.header(header::CONTENT_TYPE).as_deref(),
            Some("text/plain; charset=utf-8"),
            "{what} ⇒ Go http.Error 的 Content-Type"
        );
        assert_eq!(
            res.header(header::X_CONTENT_TYPE_OPTIONS).as_deref(),
            Some("nosniff"),
            "{what} ⇒ Go http.Error 的 nosniff"
        );
        // 被门拒的请求**不读**计数器（上游在 `realtime.M.Snapshot()` 之前 return）⇒ 无 JSON。
        assert_eq!(res.json, Value::Null, "{what} ⇒ 不得产生快照");
    }
}

// ---- 状态 #3：token 未设 + 直连 loopback ⇒ 200 ----------------------------------------

/// 四态之三：未设 token（"本地开发工作流不用配置就能跑"）+ 直连 loopback + 无转发头 ⇒ 200。
#[tokio::test]
async fn state_no_token_direct_loopback_is_200() {
    // `127.0.0.2` 也在 127/8 回环网段里（`IpAddr::is_loopback` 对整段为真，与 Go 的
    // `net.IP.IsLoopback` 同款）。
    for peer in ["127.0.0.1:40000", "127.0.0.2:40000", "[::1]:40000"] {
        let res = get(&probe_router(None), "/health/realtime", Some(peer), &[]).await;
        assert_eq!(res.status, StatusCode::OK, "loopback {peer} ⇒ 200");
        assert_eq!(
            res.header(header::CACHE_CONTROL).as_deref(),
            Some("no-store")
        );
        assert!(
            res.json.get("connects_total").is_some(),
            "loopback 分支必须给出完整快照：{}",
            res.json
        );
    }
}

/// env 里的 token 是空白（`REALTIME_METRICS_TOKEN="   "`）时必须与"未设"走**同一条**分支
/// （上游 `strings.TrimSpace(token)` 之后判 `token != ""`）⇒ loopback 走 #3、非 loopback 走 #4。
#[tokio::test]
async fn blank_token_env_behaves_exactly_like_unset() {
    assert_eq!(normalize_token(Some("   ".into())), None);
    assert_eq!(normalize_token(Some("\t\n".into())), None);
    assert_eq!(normalize_token(Some(String::new())), None);
    assert_eq!(normalize_token(None), None);
    assert_eq!(
        normalize_token(Some("  tok  ".into())),
        Some("tok".to_owned()),
        "非空值必须 TrimSpace 后保留"
    );

    let res = get(
        &probe_router(None),
        "/health/realtime",
        Some("127.0.0.1:1"),
        &[],
    )
    .await;
    assert_eq!(res.status, StatusCode::OK);
}

// ---- 状态 #4：token 未设 + 非 loopback / 任一转发头 ⇒ 404 ------------------------------

/// 四态之四：**404** + Go `http.NotFound` 的逐字 body ⇒ 远程扫描器看不到这份运维面。
#[tokio::test]
async fn state_no_token_non_loopback_is_404() {
    for peer in ["203.0.113.7:40000", "10.0.0.1:40000", "[2001:db8::1]:40000"] {
        let res = get(&probe_router(None), "/health/realtime", Some(peer), &[]).await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{peer} ⇒ 404");
        assert_eq!(res.text(), "404 page not found\n");
    }
}

/// 五个转发头**逐个**都能把 loopback 打回 404（哪怕来源确实是 127.0.0.1）——
/// 上游注释逐字：服务坐在 Caddy/Nginx 后面、代理在 localhost 终结 TLS 时，所有请求看起来都是
/// loopback，转发头在这里被当成"这是转发来的 ⇒ fail closed"的**信号**（不是可信客户端标识）。
#[tokio::test]
async fn state_no_token_any_forwarding_header_is_404_even_from_loopback() {
    for name in FORWARDING_HEADERS {
        let res = get(
            &probe_router(None),
            "/health/realtime",
            Some("127.0.0.1:40000"),
            &[(name, "evidence")],
        )
        .await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "`{name}` ⇒ 404");
        assert_eq!(res.text(), "404 page not found\n");

        // 全空白的转发头等于不存在（上游 `strings.TrimSpace(Header.Get(h)) != ""`）。
        let blank = get(
            &probe_router(None),
            "/health/realtime",
            Some("127.0.0.1:40000"),
            &[(name, "   ")],
        )
        .await;
        assert_eq!(
            blank.status,
            StatusCode::OK,
            "全空白的 `{name}` 不算「存在」（上游对取值 TrimSpace 后判空）"
        );
    }
}

/// 拿不到 peer 地址 ⇒ **fail closed**（上游 `net.ParseIP` 失败 或 `host == ""` ⇒ false）。
#[tokio::test]
async fn state_no_token_without_peer_address_fails_closed() {
    let res = get(&probe_router(None), "/health/realtime", None, &[]).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert_eq!(res.text(), "404 page not found\n");
}

// ---- 接线判据：根路径上真的挂上了（`mount_slice_probes` 的 merge） ---------------------

/// 全量装配下（`crate::routes::router`）：`/health/realtime` 在**根路径**、不在 `/api` 下，
/// 且 loopback 直连可读（未设 token 的部署形态）。
#[tokio::test]
async fn realtime_probe_is_mounted_at_the_root_path() {
    assert!(
        token_from_env().is_none(),
        "本用例的前提是进程 env **没有**设置 REALTIME_METRICS_TOKEN；请在不设它的环境里跑（门 ⑤ 正是如此）"
    );

    let res = get(
        &full_router(),
        "/health/realtime",
        Some("127.0.0.1:40000"),
        &[],
    )
    .await;
    assert_eq!(
        res.status,
        StatusCode::OK,
        "根路径上必须能读到快照（`mount_slice_probes()` 的 merge）：{}",
        res.text()
    );
    assert_eq!(res.keys().len(), 14, "13 + `daemonws`");
}

/// "**挂上了**但被门拒"与"这条路径**根本没注册**"必须可区分 —— 两者都是 404，
/// 唯一可观测的差别就是 Go `http.NotFound` 的 body（axum 默认 404 是空 body）。
///
/// 这条用例同时把形态钉住：上游是 plain `r.Get("/health/realtime", …)` ⇒ 只服务**无**尾斜杠
/// 那一形态（`docs/64` §1.4 实测 `dual-form required: 0`；补尾斜杠 = `EXTRA_ALIAS` 硬失败）。
#[tokio::test]
async fn mounted_and_unmounted_404_are_distinguishable() {
    // token 未设 + 非 loopback ⇒ 门拒（**挂上了**）。
    let gated = get(
        &probe_router(None),
        "/health/realtime",
        Some("203.0.113.7:40000"),
        &[],
    )
    .await;
    assert_eq!(gated.status, StatusCode::NOT_FOUND);
    assert_eq!(
        gated.text(),
        "404 page not found\n",
        "门拒的 404 必须带 Go http.NotFound 的 body"
    );

    // 带尾斜杠 ⇒ 本切片**没有**注册这条键（axum 默认 404，空 body）。
    let unmounted = get_with_bearer(
        &probe_router(Some(TOKEN)),
        "/health/realtime/",
        Some("127.0.0.1:40000"),
        TOKEN,
        &[],
    )
    .await;
    assert_eq!(unmounted.status, StatusCode::NOT_FOUND);
    assert!(
        unmounted.body.is_empty(),
        "带尾斜杠的形态**不得**被本切片服务（上游 plain 注册只服务一种形态）：{:?}",
        unmounted.text()
    );
}

/// 全量装配下的形态判据（与上面的切片级判据互为对照）。
#[tokio::test]
async fn only_the_plain_form_is_served_by_the_full_router() {
    let router = full_router();
    let plain = get(&router, "/health/realtime", Some("127.0.0.1:40000"), &[]).await;
    assert_eq!(plain.status, StatusCode::OK, "无尾斜杠形态必须服务");

    let slashed = get(&router, "/health/realtime/", Some("127.0.0.1:40000"), &[]).await;
    assert_ne!(
        slashed.status,
        StatusCode::OK,
        "带尾斜杠形态不得被服务（`EXTRA_ALIAS` 是硬失败）"
    );
}

// ---- `redis` 子树的值形态（不许删键；本仓无 Redis ⇒ 单副本缺省） -----------------------

/// 本仓没有 Redis 依赖 ⇒ `redis` 子树**照发**单副本缺省：`connected:false` / `node_id:""` /
/// 计数 0 / `streams:{}` / `last_error:""`（Go `string` 的零值，**不是** `null`）。
///
/// 客户端可能按 key 的**存在性**解析（`redis.connected` 判多副本、`redis.streams` 判 relay），
/// 所以既不许删键、也不许改值形态。
#[tokio::test]
async fn redis_subtree_is_emitted_with_single_replica_defaults() {
    let res = get_with_bearer(
        &probe_router(Some(TOKEN)),
        "/health/realtime",
        Some("127.0.0.1:40000"),
        TOKEN,
        &[],
    )
    .await;
    assert_eq!(res.status, StatusCode::OK);

    let redis = res.json.get("redis").expect("`redis` 键必须在");
    assert_eq!(redis["connected"], json!(false));
    assert_eq!(redis["node_id"], json!(""));
    assert_eq!(redis["streams"], json!({}));
    assert_eq!(
        redis["last_error"],
        json!(""),
        "上游 `redisLastErr` 是 Go `string` ⇒ 零值是空串（`docs/64` §2.3 写的 `null` 是计划期估算）"
    );
    for key in REDIS_KEYS {
        let value = redis
            .get(key)
            .unwrap_or_else(|| panic!("`redis.{key}` 必须存在（不许删键）"));
        if key == "connected" || key == "node_id" || key == "streams" || key == "last_error" {
            continue;
        }
        assert_eq!(value, &json!(0), "`redis.{key}` 单副本缺省是 0");
    }
}

/// 没有写入者的四个映射与三个未被接线的计数器**照发零值**（键存在性是契约）。
///
/// `inbound_too_large_total` 的接线点（`crate::pump::read_pump`）不在本片写集 ⇒
/// 结构性为 0；`subscribes_total` / `unsubscribes_total` / `subscribe_denied_total` /
/// `active_scope_rooms` 在本仓**没有对应的协议面**（身份在升级时一次解析，没有运行期订阅
/// 动作）⇒ 结构性为空映射。两者都登记在 `docs/32` §42。
#[tokio::test]
async fn unwired_counters_are_emitted_as_zero_not_omitted() {
    let res = get_with_bearer(
        &probe_router(Some(TOKEN)),
        "/health/realtime",
        Some("127.0.0.1:40000"),
        TOKEN,
        &[],
    )
    .await;
    assert_eq!(res.status, StatusCode::OK);

    assert_eq!(res.json["inbound_too_large_total"], json!(0));
    for key in [
        "events_sent_by_type",
        "subscribes_total",
        "unsubscribes_total",
        "subscribe_denied_total",
        "active_scope_rooms",
    ] {
        assert_eq!(res.json[key], json!({}), "`{key}` 必须存在且为空映射");
    }
    for key in [
        "wakeup_published_total",
        "wakeup_publish_errors",
        "wakeup_received_total",
        "runtime_gone_published_total",
        "runtime_gone_publish_errors",
        "runtime_gone_received_total",
    ] {
        assert_eq!(
            res.json["daemonws"][key],
            json!(0),
            "`daemonws.{key}` 必须存在（本仓无 Redis relay ⇒ 结构性 0）"
        );
    }
}

/// 每个计数器都必须是 JSON 整数（不是字符串、不是浮点）—— 客户端按数值解析。
#[tokio::test]
async fn every_counter_is_a_json_integer() {
    let res = get_with_bearer(
        &probe_router(Some(TOKEN)),
        "/health/realtime",
        Some("127.0.0.1:40000"),
        TOKEN,
        &[],
    )
    .await;
    assert_eq!(res.status, StatusCode::OK);

    for key in REALTIME_KEYS {
        let value = &res.json[key];
        if value.is_object() {
            continue; // 五个映射 + `redis` 子树
        }
        assert!(value.is_i64(), "`{key}` 必须是整数，实际 {value}");
    }
    for key in DAEMONWS_KEYS {
        let value = &res.json["daemonws"][key];
        assert!(value.is_i64(), "`daemonws.{key}` 必须是整数，实际 {value}");
    }
}
