//! `GET /health/realtime` —— realtime/daemonws 进程级计数器快照（**M10-3 原地填充**，
//! `docs/64` §2.3 / §4.1 第 4 行）。
//!
//! 上游：`server/cmd/server/health_realtime.go`（106 行）+ `internal/realtime/metrics.go`
//! 的 `Snapshot()`（13 个顶层键）+ `internal/daemonws/metrics.go`（14 个键，挂在
//! `snapshot["daemonws"]` 之下）。
//!
//! ```go
//! // health_realtime.go L31-54（空格缩进：clippy::tabs_in_doc_comments 是 pedantic 档、
//! // 门 ③ 把它当错误 —— 与语义无关，只是文档排版；见 docs/32 §41 的同类说明）
//! func realtimeMetricsHandler(token string) http.HandlerFunc {
//!     token = strings.TrimSpace(token)
//!     return func(w http.ResponseWriter, r *http.Request) {
//!         if token != "" {
//!             if !hasBearerToken(r, token) {
//!                 w.Header().Set("WWW-Authenticate", `Bearer realm="metrics"`)
//!                 http.Error(w, "unauthorized", http.StatusUnauthorized)
//!                 return
//!             }
//!         } else if !isDirectLoopbackRequest(r) {
//!             http.NotFound(w, r)
//!             return
//!         }
//!         w.Header().Set("Content-Type", "application/json")
//!         w.Header().Set("Cache-Control", "no-store")
//!         snapshot := realtime.M.Snapshot()
//!         snapshot["daemonws"] = daemonws.M.Snapshot()
//!         _ = json.NewEncoder(w).Encode(snapshot)
//!     }
//! }
//! ```
//!
//! ## 四态访问门（上游注释逐字给的理由，用来钉住判据）
//!
//! | # | 情形 | 状态码 | 头 / body |
//! | :-: | --- | :-: | --- |
//! | 1 | `REALTIME_METRICS_TOKEN` 已设 + 正确 `Authorization: Bearer <token>` | **200** | `application/json` + `Cache-Control: no-store`；14 个顶层键（13 realtime + `daemonws`） |
//! | 2 | 已设 token + 缺失/错误/空白 token | **401** | `WWW-Authenticate: Bearer realm="metrics"` + 纯文本 `unauthorized` |
//! | 3 | 未设 token + **直连 loopback** 且无转发头 | **200** | 同 #1（"本地开发工作流不用配置就能跑"） |
//! | 4 | 未设 token + 非 loopback，**或任一** `X-Forwarded-*`/`Forwarded` 存在 | **404** | `http.NotFound`（"不向远程扫描器宣告它的存在"） |
//!
//! 🔴 #4 为什么连"127.0.0.1 但带 `X-Forwarded-For`"也拒：上游注释逐字 —— 服务坐在
//! Caddy/Nginx 后面、由代理在 localhost 上终结 TLS 时，**所有**请求看起来都是 loopback，
//! 于是公开的扫描器会拿到这份运维面。转发头在这里**不是**用来识别真实客户端的（上游明说
//! 不信任它们），只被当成"这是转发来的 ⇒ fail closed"的信号。
//!
//! ## 为什么 `RemoteAddr` 用 `Option<ConnectInfo<SocketAddr>>`
//!
//! 生产装配（`apps/mc-server/src/main.rs:233-235`）用
//! `into_make_service_with_connect_info::<SocketAddr>()` 注入 peer 地址；而**测试直连 router**
//! 时不会注入。用严格版 `ConnectInfo` 会让"没注入"变成 axum 自带的 500 拒绝体；`Option` 则让
//! 本文件明确回答那个问题 —— 而且答案必须是 **fail closed**（拿不到地址 = 与上游 `net.ParseIP`
//! 失败同款 ⇒ 走 #4 的 404），先例见 `routes/webhooks/autopilots.rs:58-72`。
//!
//! ## 快照从哪来
//!
//! `mc_ws::hub::metrics`（本片**唯一**写者）—— 进程级单例，`Hub::new()` 用的就是它 ⇒ 读到的
//! 是真正在跑的那条 ws hub 的计数。本文件**不**挂 `State<Arc<AppState>>` 的任何字段：
//! `AppState` 里没有 daemon hub，而上游 handler 也只用包级单例（不要为了"看起来有点像"
//! 去改 `state.rs` —— 那不在本片写集）。
//!
//! ## 形态：只注册**无尾斜杠**那一形态
//!
//! 上游是 plain `r.Get("/health/realtime", …)`（`router.go:1412`）⇒ 只注册 `/health/realtime`；
//! 补 `/health/realtime/` 就是 `EXTRA_ALIAS` 硬失败（本波 `slash-alias-allowlist.tsv` 是
//! **0 数据行**、没有豁免退路）。
//!
//! ## 与上游的**已知差异**（登记在 `docs/32-M3-DAEMON-FACE.md` §42）
//!
//! | # | 上游 | 本地 | 影响 |
//! | :-: | --- | --- | --- |
//! | 1 | `os.Getenv("REALTIME_METRICS_TOKEN")` 在 `router.go:1412`（装配期）读 | [`router`] 在构建期读同一个 env（`routes/mod.rs::mount_slice_probes` ⇒ `probes::router`） | 判据相同（改 env 只影响新进程；本仓没有热重载 router） |
//! | 2 | `token` 只 `TrimSpace` 一次 | 同（空白 env 值 ⇒ 视同未设） | 无 |
//! | 3 | `subtle.ConstantTimeCompare` | 本地逐字节常量时间比较（长度不等直接 false） | 无（同样的失败侧） |
//! | 4 | `json.NewEncoder` 在 body 末尾补 `'\n'`、显式写 `Content-Length` | axum `Json`（无尾换行，length 自动） | `Content-Type` 一致；该路径**无** ⑨ fixture ⇒ 无机器判据 |
//! | 5 | `r.RemoteAddr` 由 Go 恒填 | `Option<ConnectInfo<SocketAddr>>`，缺 ⇒ fail closed | 见上（测试直连 router 时走 #4） |
//! | 6 | `http.Error` 的 body 是 Go 的 `unauthorized\n`；`http.NotFound` 是 `404 page not found\n` | 逐字复刻这两个 body（本地刻意**不**用空 body） | 让"挂上了但被门拒"与"路径没挂"可区分（见 `tests.rs` 的 `mounted_vs_unmounted_*`） |

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Extension};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};

use crate::state::AppState;

/// 访问门配置 —— **在 router 构建期定值**（上游同款：`router.go:1412` 在装配期读 env）。
#[derive(Clone, Debug)]
struct MetricsToken(Option<String>);

/// 上游 `router.go:1412` 读的那个 env。
///
/// 空的 / 全是空白的取值一律映射成 `None`：上游 `strings.TrimSpace` 之后用 `token != ""`
/// 判定，所以 `REALTIME_METRICS_TOKEN="   "` 与"没设"在上游是同一条分支（#3/#4）。
///
/// ⚠️ 本 env **不**登记进 `mc-config`：上游也是在 `cmd/server` 的路由装配处直接
/// `os.Getenv` 的（它属于"这一条路由的访问门"，不是进程级配置段），登记见 `docs/32` §42。
pub(crate) fn token_from_env() -> Option<String> {
    // `env::var` 对**非 UTF-8** 取值报错 ⇒ 落到 `None`（视同未设）。上游 Go 拿的是原始字节，
    // 差别只出现在"token 不是 UTF-8"这个不可能由正常部署产生的情形上，且方向是 fail closed。
    normalize_token(std::env::var("REALTIME_METRICS_TOKEN").ok())
}

/// `strings.TrimSpace` + `token != ""` 的**纯函数形态**（用例不必动进程 env）。
pub(crate) fn normalize_token(raw: Option<String>) -> Option<String> {
    raw.map(|value| value.trim().to_owned())
        .filter(|token| !token.is_empty())
}

/// `/health/realtime` 切片（生产装配走它：token 取自 env）。
pub fn router(state: Arc<AppState>) -> Router<Arc<AppState>> {
    router_with_token(state, token_from_env())
}

/// 同 [`router`]，但 token 由调用方给定 —— 让四态访问门的用例**不**依赖进程 env
/// （env 是进程级的，并发用例之间会互相干扰；本仓既有手法见
/// `routes/webhooks/autopilots.rs` 的测试注入）。
pub fn router_with_token(_state: Arc<AppState>, token: Option<String>) -> Router<Arc<AppState>> {
    // `_state` 保留在签名里是 anchor（M10-0）定下的切片形状，四个子 router 一致；
    // handler 与上游一样**不读**任何 server 状态。
    Router::new()
        .route("/health/realtime", get(realtime_metrics))
        .layer(Extension(MetricsToken(token)))
}

/// 上游 `realtimeMetricsHandler` 的内层闭包（门 + 快照）。
///
/// 提取器的顺序就是判据的顺序：**先**判访问门（token / loopback），过了才可能产生快照 ——
/// 被拒的请求连计数器都不读（与上游逐字同：`http.Error` / `http.NotFound` 都在
/// `realtime.M.Snapshot()` **之前** return）。
async fn realtime_metrics(
    Extension(MetricsToken(token)): Extension<MetricsToken>,
    headers: HeaderMap,
    peer: Option<ConnectInfo<SocketAddr>>,
) -> Response {
    if let Some(want) = token.as_deref() {
        if !has_bearer_token(&headers, want) {
            return unauthorized();
        }
    } else if !is_direct_loopback(&headers, peer.as_ref()) {
        return not_found();
    }

    let mut response = Json(mc_ws::hub::metrics::snapshot()).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// 上游 `hasBearerToken`。
///
/// 三条与上游逐字对齐的细节：① 前缀比较**不分大小写**（`strings.EqualFold`）；② 前缀之后
/// 还要 `TrimSpace`，空串不算（`Authorization: Bearer    ` 走 401）；③ 比较是**常量时间**的
/// —— 上游用 `subtle.ConstantTimeCompare`，本地逐字节 XOR 累加（长度不等立刻 false，与
/// `subtle` 同侧）。
fn has_bearer_token(headers: &HeaderMap, want: &str) -> bool {
    const PREFIX: &str = "Bearer ";
    let Some(raw) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    // 按**字节**比前缀：Go 的 `auth[:len(prefix)]` 也是字节切片。这里先验完 6 个 ASCII 字节，
    // 后面的 `raw[PREFIX.len()..]` 才必然落在字符边界上（否则对多字节 token 会 panic）。
    if raw.len() <= PREFIX.len()
        || !raw.as_bytes()[..PREFIX.len()].eq_ignore_ascii_case(PREFIX.as_bytes())
    {
        return false;
    }
    let got = raw[PREFIX.len()..].trim();
    if got.is_empty() {
        return false;
    }
    constant_time_eq(got.as_bytes(), want.as_bytes())
}

/// 常量时间字节比较（长度不等 ⇒ `false`）。
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0_u8;
    for (left, right) in a.iter().zip(b) {
        diff |= left ^ right;
    }
    diff == 0
}

/// 上游 `isDirectLoopbackRequest`。
fn is_direct_loopback(headers: &HeaderMap, peer: Option<&ConnectInfo<SocketAddr>>) -> bool {
    if has_forwarding_header(headers) {
        return false;
    }
    // 拿不到 peer 地址（Unix socket / 测试未注入 `ConnectInfo`）⇒ 与上游 `net.ParseIP` 返回
    // nil 同款：**拒绝**。方向是安全的（宁可 404 也不在不知道来源时发这份面）。
    peer.is_some_and(|ConnectInfo(addr)| addr.ip().is_loopback())
}

/// 上游 `hasForwardingHeader` —— 五个头逐个判"非空白即存在"。
///
/// 用字面量而不是 `header::*` 常量：`http` crate 只为 `FORWARDED` 一类通用头提供常量，
/// 其余四条 X- 头没有（名字大小写不敏感，与 Go 的 `Header.Get` 同款）。
fn has_forwarding_header(headers: &HeaderMap) -> bool {
    const FORWARDING_HEADERS: [&str; 5] = [
        "X-Forwarded-For",
        "X-Forwarded-Host",
        "X-Forwarded-Proto",
        "X-Real-Ip",
        "Forwarded",
    ];
    FORWARDING_HEADERS.iter().any(|name| {
        headers
            .get(*name)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| !value.trim().is_empty())
    })
}

/// #2：`WWW-Authenticate: Bearer realm="metrics"` + Go `http.Error` 的纯文本 body。
fn unauthorized() -> Response {
    let mut response = text_response(StatusCode::UNAUTHORIZED, "unauthorized\n");
    response.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static("Bearer realm=\"metrics\""),
    );
    response
}

/// #4：Go `http.NotFound` 逐字（状态码 + `text/plain; charset=utf-8` + `X-Content-Type-Options`）。
///
/// body 取 Go 的 `404 page not found\n` 而不是空 body：它是"**挂上了**但被门拒"与
/// "这条路径根本没注册"（axum 默认 404，空 body）之间**唯一**的可观测差别，`tests.rs` 用它
/// 把"接入点真的挂上了"这条判据钉住。它不含任何本服务的信息 ⇒ 不违反"不宣告存在"。
fn not_found() -> Response {
    text_response(StatusCode::NOT_FOUND, "404 page not found\n")
}

/// 纯文本响应（`Content-Type: text/plain; charset=utf-8` + `nosniff`，与 Go `http.Error` 同款）。
fn text_response(status: StatusCode, body: &'static str) -> Response {
    let mut response = (status, body).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

#[cfg(test)]
mod tests;
