//! `POST /api/webhooks/stripe` 1 条（**写者 M9-6** / `LUM-1821` / `docs/62` §4.1 的第 7 行）。
//!
//! 上游：`internal/handler/cloud_billing.go` 的 `L38–L48`（两个常量）+ `L520–L604`
//! （`HandleCloudBillingStripeWebhook`，`router.go:1505` 注册在 `// Public API` 块里）。
//! 出站契约在 [`mc_cloud::webhook`]；本文件只做**入站**那几件事。
//!
//! # 四段本地语义 + 一次出站（**顺序是判据的一部分**，`docs/62` §6.5 的 M9-6 行）
//!
//! | 步 | 判据 | 结果 | 上游出处 |
//! | :-: | --- | :-: | --- |
//! | 1 | `MULTICA_CLOUD_URL` 未配置 | **403** `cloud_runtime_not_configured` | `cloud_billing.go:521–524` |
//! | 2 | per-IP 限流超限 | **429** `rate limit exceeded` | `:530–540` |
//! | 3 | 缺 `Stripe-Signature` | **401** | `:546–550` |
//! | 4 | 体 > 1 MiB | **413** | `:556–560` |
//! | 5 | 成功 | 云侧状态码与体**逐字**回写 | `:598–601` |
//!
//! 🔴 **403 在最前**：上游第一个 `if` 就是 `h.CloudRuntime == nil || !Enabled()`。
//! `docs/62` §9.5 把「限流 / 签名 / 体上限」列成 ①②③，那是**清单**不是顺序。
//!
//! # 限流器用**哪一条**闸（`docs/62` §9.5 末段要求的逐字复核）
//!
//! 上游是 `h.WebhookIPRateLimiter`（`handler.go:492-494` 三条装配之一）——**同一个**字段，
//! autopilot webhook 入站与 stripe 入站共用；上游注释逐字：「we deliberately reuse the same
//! limiter as the autopilot webhook: both are public unauthenticated ingress with the same
//! abuse profile, and budgeting them together gives a single knob to tune」。
//!
//! | 本地闸 | 键 | 何时消费 | stripe 用不用 |
//! | --- | --- | --- | :-: |
//! | [`WEBHOOK_ABSOLUTE_IP_LIMITER`] | IP | **每次请求**（`allow`） | ✅ **就是它** |
//! | `WEBHOOK_IP_LIMITER` | IP | 只记「坏凭据」的**债** | ❌ 那是 autopilot 的凭据债 |
//! | `WEBHOOK_TRIGGER_LIMITER` | `trigger_id` | 每次派发（worker 侧） | ❌ 不在入站 |
//!
//! ⇒ 结论：**复用 [`WEBHOOK_ABSOLUTE_IP_LIMITER`] 这个进程级单例本身**（不是另建一个同参数的
//! limiter）—— 上游就是同一个字段。**禁止**新写限流器。
//!
//! # 拿不到对端地址 ⇒ **跳过**这道闸（逐字跟上游）
//!
//! 上游 `if ip := h.clientIPForRateLimit(r); ip != ""` —— 空 ip **不**限流（不记账、不拒）。
//! 本仓本有两处公开面（`contact_sales.rs`、`webhooks/autopilots.rs`）也是同一立场。
//! `ConnectInfo` 由 `apps/mc-server/src/main.rs` 的 `into_make_service_with_connect_info`
//! 注入 ⇒ **生产永远拿得到**；拿不到的只有「直连 `Router::oneshot`」的测试与 `mc-conformance`
//! 回放器（见「⑨ 的两条 fixture 结构上不可复现」）。
//!
//! # ⑨：`webhooks` 三条 fixture 里**两条结构上不可复现**（已实测，不是猜）
//!
//! 上游三条测试各自**注入**了不同的 handler 状态，而 `mc-conformance` 回放的是**同一个**
//! router、`MULTICA_CLOUD_URL` 未配置：
//!
//! | fixture | 上游测试注入的 | 回放器里 | 结论 |
//! | --- | --- | --- | :-: |
//! | `TestStripeWebhookDisabledReturnsForbidden` | `enabled: false` | `MULTICA_CLOUD_URL` 未配置 | ✅ **可复现** ⇒ 403 pass |
//! | `TestStripeWebhookMissingSignatureRejectedLocally` | `enabled: **true**` + 缺签名 | 未配置 ⇒ 步 1 先给 403 | ❌ 403 ≠ 401 |
//! | `TestStripeWebhookRateLimited` | `enabled: true` + `denyingWebhookIPRateLimiter` | 未配置 ⇒ 403；且回放器**无 `ConnectInfo`** ⇒ 步 2 按上游**跳过** | ❌ 403 ≠ 429 |
//!
//! 后两条与 `docs/62` §6.2 里那 17 条 `member` fixture 属**同一类**：「本地无法构造这次请求」。
//! 要让它们转绿只有两条路，**都不可接受**：① 把步 1 的 403 挪到步 3 之后（**违背上游**）；
//! ② 把无归属流量的配额调成 ≤2/60s 好让单次回放撞线（**为测试而改生产语义**）。
//! ⇒ 本片按上游顺序实现，⑨ 实测 `pass 1 / mismatch 2 / unmounted 0`（另两条的形态从
//! `unmounted` 变成 `mismatch`），口径登记在 `docs/32` §9.13。
//!
//! # 形态与合并点
//!
//! **单形态**：只注册上游字面量 `/api/webhooks/stripe`（`docs/62` §1.4 实测本波只有
//! `/api/notification-preferences` 那 3 条是双形态）⇒ 补尾斜杠形态反而会让
//! `scripts/slash_alias_audit.py` 报 `EXTRA_ALIAS`。合并点 `routes/cloud/mod.rs:51` 已由
//! M9-0 建好（`.merge(webhook::router())`）⇒ 本片**不碰** `mod.rs` / `mount.rs` / `lib.rs`。
//! 同 path+method 重复注册 ⇒ axum 在**启动时 panic**（`docs/15` §9.6.6）。
//!
//! # 这条**不挂**机器凭据闸、**不挂会话
//!
//! 上游在 `// Public API` 块里注册、也**不**注入 `X-User-ID`（`cloud_billing.go:590` 附近注释
//! 逐字：「Intentionally no `UserID` — webhook is unauthenticated by design」）⇒
//! [`mc_cloud::transport::Request::user_id`] 恒 `None`。挂了 `require_human_actor` / `AuthUser`
//! 会把这条公开面变成账号面。

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use mc_autopilot::webhook::ratelimit::{SlidingWindowLimiter, WEBHOOK_ABSOLUTE_IP_LIMITER};
use mc_cloud::webhook as stripe;
use mc_cloud::{CloudError, Response as CloudResponse};
use mc_errors::ErrorResponse;
use serde::Serialize;

use crate::state::AppState;

/// 「未配置」（上游 `writeFeatureDisabled(w, "cloud_runtime_not_configured", …)`，`:522`）。
const CODE_NOT_CONFIGURED: &str = "cloud_runtime_not_configured";
/// 「配了但非法」（`docs/62` §2.6 的 500 行；上游把这两态合成一条 `Enabled()` 检查）。
const CODE_MISCONFIGURED: &str = "cloud_runtime_misconfigured";
const CODE_TIMEOUT: &str = "cloud_runtime_timeout";
const CODE_FAILED: &str = "upstream_error";
const CODE_PAYLOAD_TOO_LARGE: &str = "payload_too_large";

const MSG_NOT_CONFIGURED: &str = "cloud runtime is not configured";
const MSG_MISCONFIGURED: &str = "cloud runtime is misconfigured";
const MSG_TIMEOUT: &str = "cloud runtime request timed out";
const MSG_FAILED: &str = "cloud runtime request failed";
/// 上游 `writeError(w, 429, "rate limit exceeded")` 逐字（`:537`）。
const MSG_RATE_LIMITED: &str = "rate limit exceeded";
/// 上游 `writeError(w, 401, "missing Stripe-Signature header")` 逐字（`:549`）。
const MSG_MISSING_SIGNATURE: &str = "missing Stripe-Signature header";
/// 上游 `writeError(w, 413, "request body is too large")` 逐字（`:561`）。
const MSG_BODY_TOO_LARGE: &str = "request body is too large";

/// stripe 转发切片（**单路由**）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/webhooks/stripe", post(handle_stripe_webhook))
}

// ---------------------------------------------------------------------------
// 1 个 handler
// ---------------------------------------------------------------------------

/// `POST /api/webhooks/stripe`。
///
/// 抽取器只用 `FromRequestParts` 的三个（`State` / `Option<ConnectInfo>` / `HeaderMap`）——
/// **刻意不取 body**：body 要在限流与签名判定**之后**才读（上游注释逐字：「Two pre-checks
/// happen BEFORE we read the body」）。
async fn handle_stripe_webhook(
    State(state): State<Arc<AppState>>,
    peer: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    request: Request,
) -> Response {
    // ---- 步 1：cloud 未配置 ⇒ 403（**先于**限流与签名，上游第一个 if）----------
    let Some(client) = state.cloud.client() else {
        return if state.cloud.is_misconfigured() {
            error_with_code(
                StatusCode::INTERNAL_SERVER_ERROR,
                CODE_MISCONFIGURED,
                MSG_MISCONFIGURED,
            )
        } else {
            error_with_code(
                StatusCode::FORBIDDEN,
                CODE_NOT_CONFIGURED,
                MSG_NOT_CONFIGURED,
            )
        };
    };

    // ---- 步 2：per-IP 限流（**抢在读 body 之前**；无 ip ⇒ 跳过，逐字跟上游）-------
    if let Some(limited) = rate_limited(
        peer.map(|ConnectInfo(addr)| addr.ip().to_string())
            .as_deref(),
    ) {
        return limited;
    }

    // ---- 步 3：`Stripe-Signature` 存在性 ---------------------------------------
    let signatures = header_values(&headers, stripe::STRIPE_SIGNATURE_HEADER);
    if !stripe::has_stripe_signature(&signatures) {
        return error_with_code(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            MSG_MISSING_SIGNATURE,
        );
    }

    // ---- 步 4：1 MiB 体上限（上游 `http.MaxBytesReader`）-----------------------
    let limit = stripe::MAX_STRIPE_WEBHOOK_BODY_SIZE;
    let body = match axum::body::to_bytes(request.into_body(), limit + 1).await {
        Ok(body) => body,
        Err(error) => {
            // 上游把这一支拆成两个状态：`*http.MaxBytesError` ⇒ 413「too large」，
            // 其它（连接中断 / 畸形 chunked）⇒ 400「invalid request body」。
            // `axum::body::to_bytes` 的错误是**不透明**的 `axum_core::Error`（没有
            // `BytesRejection` 那样的可匹配枚举）⇒ 本仓与 `routes/cloud/billing.rs`
            // 走同款归并：一律 413（客户端已断开时这个响应也无人读），
            // 登记在 `docs/32` §9.13 的 D-1。
            tracing::debug!(error = %error, "stripe webhook: failed to read request body");
            return error_with_code(
                StatusCode::PAYLOAD_TOO_LARGE,
                CODE_PAYLOAD_TOO_LARGE,
                MSG_BODY_TOO_LARGE,
            );
        }
    };
    if body.len() > limit {
        return error_with_code(
            StatusCode::PAYLOAD_TOO_LARGE,
            CODE_PAYLOAD_TOO_LARGE,
            MSG_BODY_TOO_LARGE,
        );
    }

    // ---- 步 5：出站（500 / 504 / 502 / 逐字回写）-------------------------------
    let content_types = header_values(&headers, "content-type");
    let out = stripe::stripe_webhook_request(
        &body,
        &signatures,
        &content_types,
        first_header(&headers, "x-request-id"),
    );
    match client.send(out).await {
        Ok(response) => cloud_response(response),
        Err(error) => transport_error(&error),
    }
}

// ---------------------------------------------------------------------------
// 四段本地判定的公共骨架
// ---------------------------------------------------------------------------

/// 步 2：per-IP 绝对天花板。`Some(response)` = 已经 429。**无 ip ⇒ 跳过**（逐字跟上游
/// `if ip := h.clientIPForRateLimit(r); ip != ""`）。
///
/// 内核 [`gate_by_ip`] 把限流器做成实参：生产传进程级单例，测试传一枚小配额替身
/// （600 / 60s 的真实闸没法用穷举打满，而 429 这一支必须被**判**到）。
fn rate_limited(peer_ip: Option<&str>) -> Option<Response> {
    gate_by_ip(&WEBHOOK_ABSOLUTE_IP_LIMITER, peer_ip)
}

fn gate_by_ip(limiter: &SlidingWindowLimiter, peer_ip: Option<&str>) -> Option<Response> {
    let ip = peer_ip?;
    if limiter.allow(ip) {
        return None;
    }
    Some(error_with_code(
        StatusCode::TOO_MANY_REQUESTS,
        "rate_limited",
        MSG_RATE_LIMITED,
    ))
}

/// 取**第一个**同名头的值（Go `http.Header.Get` 语义；头名大小写不敏感）。
fn first_header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

/// 取**全部**同名头的值（Go `http.Header.Values` 语义；非 ASCII 的值被丢掉 —— 上游那边
/// 它们在 wire 上就是字节，本地无法逐字复刻一个非法头值）。
fn header_values<'a>(headers: &'a HeaderMap, name: &str) -> Vec<&'a str> {
    headers
        .get_all(name)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .collect()
}

/// 云侧响应逐字写回（与 `routes/cloud/billing.rs` 的 `cloud_response` 同款：状态码原样、
/// 体原样、**不** trim、**不** 把非 JSON 体包进错误信封）。
fn cloud_response(response: CloudResponse) -> Response {
    let status = StatusCode::from_u16(response.status).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut headers = HeaderMap::new();
    if let Some(value) = response
        .header("X-Request-ID")
        .and_then(|value| HeaderValue::from_str(value).ok())
    {
        headers.insert("x-request-id", value);
    }
    if response.is_body_blank() {
        return (status, headers, Bytes::new()).into_response();
    }
    if serde_json::from_slice::<serde_json::Value>(&response.body).is_ok() {
        headers.insert(
            axum::http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
    }
    (status, headers, response.body).into_response()
}

/// 传输层失败 → 本地错误（`docs/62` §2.6 的四行，**静态** message，绝不回显云侧体）。
fn transport_error(error: &CloudError) -> Response {
    if error.is_timeout() {
        error_with_code(StatusCode::GATEWAY_TIMEOUT, CODE_TIMEOUT, MSG_TIMEOUT)
    } else if error.is_misconfigured() {
        error_with_code(
            StatusCode::INTERNAL_SERVER_ERROR,
            CODE_MISCONFIGURED,
            MSG_MISCONFIGURED,
        )
    } else if error.is_disabled() {
        error_with_code(
            StatusCode::FORBIDDEN,
            CODE_NOT_CONFIGURED,
            MSG_NOT_CONFIGURED,
        )
    } else {
        error_with_code(StatusCode::BAD_GATEWAY, CODE_FAILED, MSG_FAILED)
    }
}

/// 本仓标准的**嵌套**错误信封（形状与 `routes/cloud/billing.rs` 逐字同款）。
fn error_with_code(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(ErrorBody {
            error: ErrorResponse::new(code, message),
        }),
    )
        .into_response()
}

#[derive(Serialize)]
struct ErrorBody {
    error: ErrorResponse,
}

#[cfg(test)]
mod tests;
