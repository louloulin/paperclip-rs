//! `/api/cloud-billing/*` 8 条（**写者 M9-1** / `LUM-1816` / `docs/62` §4.1 的第 2 行）。
//!
//! 上游：`internal/handler/cloud_billing.go` 的 `L356–L503`（8 条 billing handler，全部是
//! `proxyCloudRuntime` 的**纯透传**）。出站契约（8 条路径 / 方法 / query / body）与两条出站
//! 前判定在 [`mc_cloud::billing`] 的模块头 —— 本地**不落任何表**（`docs/62` §9.4）。
//!
//! # 三层语义（`docs/62` §2.5 的 A 行，**逐条可测**）
//!
//! | 情形 | 本地 | code / message |
//! | --- | :-: | --- |
//! | `MULTICA_CLOUD_URL` 缺 / 空 | **403** | `cloud_runtime_not_configured` |
//! | 配了但非法（userinfo / query / fragment / 非绝对） | **500** | `cloud_runtime_misconfigured` |
//! | 无会话（缺 `X-Multica-User-Id`） | **401** | `unauthorized`（`AuthUser` 提取器） |
//! | `mat_` / `mcn_` 机器凭据（`X-Actor-Source: task_token｜cloud_pat`） | **403** | 上游逐字文本，**且不产生出站请求** |
//!
//! 链的顺序是**路由组中间件 → handler 提取器 → handler 体**：机器凭据闸挂在
//! [`router`] 的 `route_layer` 上（上游 `r.Use(handler.RequireHumanActor)` 的等价物，
//! R-M9-2），会话由 `AuthUser` 提取器给 401，未配置/非法在 handler 体里判。
//!
//! # 错误映射（`docs/62` §2.6）
//!
//! `Disabled` ⇒ 403 / `InvalidBaseUrl` ⇒ 500 / `Timeout` ⇒ **504** / 其余（连接拒、体超限）
//! ⇒ **502**。**云侧的 4xx/5xx 不是错误**：`transport::Client::send` 对任何 HTTP 状态都返回
//! `Ok(Response)`（上游 `doInner` 的语义），本文件把它**原样**写回客户端。
//!
//! # 响应：逐字透传（+ 一处有意的偏离）
//!
//! `status` 与体**逐字**回写；**不**解析、**不**重编码、**不** trim。上游
//! `writeCloudRuntimeResponse` 在两处与这里不同，都登记在 `docs/32` §46：
//! ① 它对体做 `bytes.TrimSpace`（本文件不 trim —— 本片 `DoD` 第 4 条要求逐字）；
//! ② 体不是合法 JSON 时它把体**包进** `{"error": "<云侧体>"}`（本文件原样转发字节 ——
//! 那一步会把任意云侧内容塞进**错误信封**，与 §2.4 判据 ③ 的取向相反）。
//!
//! **错误路径不回显云侧响应体**（§2.4 判据 ③）：本文件自己的错误体一律是
//! 静态 message（[`error_with_code`]），既不带出站 URL、也不带云侧体。
//!
//! # 机器凭据闸为什么是**中间件**而不是 handler 提取器
//!
//! 与上游逐字对齐：`router.go:1910` 是 `r.Route("/api/cloud-billing", …)` 内部
//! `r.Use(handler.RequireHumanActor)` —— **只有**中间件，没有 handler 级 backstop
//! （handler 级 backstop 是 upstream **subscriptions** 面的做法，归 M9-2）。
//!
//! # 形态（`docs/62` §1.4 实测 `declared 34 / dual-form required: 0`）
//!
//! 8 条**只按上游字面量注册**那一形态；路径参数写 `:sessionId`（matchit 0.7 把 `{…}` 当
//! 字面量 ⇒ 编译通过且恒 404）。同 path+method 重复注册 ⇒ axum 在**启动时 panic**。

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, RawQuery, Request, State};
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use mc_cloud::billing;
use mc_cloud::{CloudError, Request as CloudRequest, Response as CloudResponse};
use mc_core::cloud::MAX_CLOUD_RUNTIME_REQUEST_BODY_SIZE;
use mc_errors::ErrorResponse;
use serde::Serialize;

use crate::actor_guard::require_human_actor;
use crate::routes::auth_user::AuthUser;
use crate::state::AppState;

/// 「未配置」（上游 `writeFeatureDisabled(w, "cloud_runtime_not_configured", …)`）。
const CODE_NOT_CONFIGURED: &str = "cloud_runtime_not_configured";
/// 「配了但非法」（`docs/62` §2.6 的 500 行）。
const CODE_MISCONFIGURED: &str = "cloud_runtime_misconfigured";
/// 超时（上游这一行**没有** code，本地按本仓信封补一个；见 `docs/32` §46）。
const CODE_TIMEOUT: &str = "cloud_runtime_timeout";
/// 其余传输失败（与 `routes/composio` / `routes/auth` 的 502 同一个 code）。
const CODE_FAILED: &str = "upstream_error";

const MSG_NOT_CONFIGURED: &str = "cloud runtime is not configured";
const MSG_MISCONFIGURED: &str = "cloud runtime is misconfigured";
/// 上游 `writeError(w, 504, …)` 逐字。
const MSG_TIMEOUT: &str = "cloud runtime request timed out";
/// 上游 `writeError(w, 502, …)` 逐字。
const MSG_FAILED: &str = "cloud runtime request failed";
/// 上游 `GetCloudBillingCheckoutSession` 的三条 400 文本（逐字）。
const MSG_SESSION_ID_REQUIRED: &str = "session_id is required";
const MSG_SESSION_ID_INVALID: &str = "invalid session_id";
/// 上游 `readCloudRuntimeJSONBody` 的两条 400 + 一条 413 文本（逐字）。
const MSG_BODY_REQUIRED: &str = "request body is required";
const MSG_BODY_INVALID: &str = "invalid request body";
const MSG_BODY_TOO_LARGE: &str = "request body is too large";

/// 8 条 owner-credit 出站代理。
///
/// 🔴 八条**全部**挂机器凭据闸（`docs/62` §1.5 的 A 行）—— 计费是**账户级**动作，
/// 而 `mat_` / `mcn_` 凭据会以**属主**身份行事（R-M9-2：「没有这条闸就不许合并 M9-1/M9-2」）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/cloud-billing/balance", get(get_balance))
        .route("/api/cloud-billing/transactions", get(list_transactions))
        .route("/api/cloud-billing/batches", get(list_batches))
        .route("/api/cloud-billing/topups", get(list_topups))
        .route("/api/cloud-billing/price-tiers", get(list_price_tiers))
        .route(
            "/api/cloud-billing/checkout-sessions",
            post(create_checkout_session),
        )
        .route(
            "/api/cloud-billing/checkout-sessions/:sessionId",
            get(get_checkout_session),
        )
        .route(
            "/api/cloud-billing/portal-sessions",
            post(create_portal_session),
        )
        // 上游 `router.go:1911` 的 `r.Use(handler.RequireHumanActor)`：整个路由组一条闸。
        .route_layer(axum::middleware::from_fn(require_human_actor))
}

// ---------------------------------------------------------------------------
// 8 个 handler（每个 3–8 行：出站契约在 mc-cloud，语义在 proxy）
// ---------------------------------------------------------------------------

async fn get_balance(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
) -> Response {
    proxy(
        &state,
        billing::balance_request(user.id(), request_id(&headers).as_deref()),
    )
    .await
}

async fn list_transactions(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    RawQuery(raw_query): RawQuery,
) -> Response {
    proxy(
        &state,
        billing::transactions_request(
            user.id(),
            request_id(&headers).as_deref(),
            forward_query(raw_query.as_deref()),
        ),
    )
    .await
}

async fn list_batches(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    RawQuery(raw_query): RawQuery,
) -> Response {
    proxy(
        &state,
        billing::batches_request(
            user.id(),
            request_id(&headers).as_deref(),
            forward_query(raw_query.as_deref()),
        ),
    )
    .await
}

async fn list_topups(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    RawQuery(raw_query): RawQuery,
) -> Response {
    proxy(
        &state,
        billing::topups_request(
            user.id(),
            request_id(&headers).as_deref(),
            forward_query(raw_query.as_deref()),
        ),
    )
    .await
}

async fn list_price_tiers(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
) -> Response {
    proxy(
        &state,
        billing::price_tiers_request(user.id(), request_id(&headers).as_deref()),
    )
    .await
}

async fn create_checkout_session(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    request: Request,
) -> Response {
    let request_id = request_id(request.headers());
    let body = match read_cloud_runtime_json_body(request).await {
        Ok(body) => body,
        Err(response) => return response,
    };
    proxy(
        &state,
        billing::checkout_session_create_request(user.id(), request_id.as_deref(), body),
    )
    .await
}

async fn get_checkout_session(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    Path(session_id): Path<String>,
) -> Response {
    // 上游取 `chi.URLParam(r, "sessionId")`：空 ⇒ 400（`{sessionId}` 段为空时本地拿不到
    // 这条路由 —— matchit 的段不允许为空，见 `docs/32` §46 的 D-4；本分支是上游语义的照抄）。
    if session_id.is_empty() {
        return error_with_code(
            StatusCode::BAD_REQUEST,
            "validation_error",
            MSG_SESSION_ID_REQUIRED,
        );
    }
    // 🔴 出站 URL 的路径段**直接拼**这个值 ⇒ 必须先过 allowlist（`mc_cloud::billing`）。
    if !billing::is_valid_stripe_session_id(&session_id) {
        return error_with_code(
            StatusCode::BAD_REQUEST,
            "validation_error",
            MSG_SESSION_ID_INVALID,
        );
    }
    proxy(
        &state,
        billing::checkout_session_request(&session_id, user.id(), request_id(&headers).as_deref()),
    )
    .await
}

async fn create_portal_session(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
) -> Response {
    // ⚠️ 与 `create_checkout_session` 的**唯一**差别：这里**不读体**（上游 `withBody` 没打开，
    // 见 `mc_cloud::billing::portal_session_request` 的文档）。
    proxy(
        &state,
        billing::portal_session_request(user.id(), request_id(&headers).as_deref()),
    )
    .await
}

// ---------------------------------------------------------------------------
// 出站与响应的公共骨架
// ---------------------------------------------------------------------------

/// 一次出站代理（上游 `proxyCloudRuntime` 的 billing 分支）。
async fn proxy(state: &AppState, request: CloudRequest) -> Response {
    let Some(client) = state.cloud.client() else {
        // 上游把「没配」与「配了但非法」分成两条（403 / 500）—— 顺序：先 500。
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
    match client.send(request).await {
        Ok(response) => cloud_response(response),
        Err(error) => transport_error(&error),
    }
}

/// 云侧响应逐字写回（上游 `writeCloudRuntimeResponse` 的本地形态）。
///
/// - 状态码**原样**（含 4xx / 5xx —— 云侧是最终授权方）；
/// - 体**原样**（空 / 全空白 ⇒ 无体，与上游 `len(body) == 0` 那一支同款）；
/// - 体是合法 JSON ⇒ 补 `Content-Type: application/json`（上游 `json.Valid` 那一支）；
/// - 云侧给了 `X-Request-ID` ⇒ 回写（上游逐字）。
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
        return (status, headers, Body::empty()).into_response();
    }
    if serde_json::from_slice::<serde_json::Value>(&response.body).is_ok() {
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    }
    (status, headers, Body::from(response.body)).into_response()
}

/// 传输层失败 → 本地错误（`docs/62` §2.6 的四行，**静态** message）。
///
/// # 为什么这是一个可单测的纯函数
///
/// 四条里有两条（`Timeout` / `ResponseTooLarge`）**无法**在离线替身上端到端触发
/// （前者要 35s，后者要 >1 MiB 的响应体；后者可以，见测试），把映射提出来就能逐条钉住。
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

/// 本仓标准的**嵌套**错误信封（`{"error":{"code":…,"message":…}}`；形状与 `mc_errors` 一致，
/// `code` 用上游字面量）。与 `routes/{vcs,github,composio,channels}` 各自持有的本地副本同形
/// —— 本仓既有约定是「各切片各自持有本地副本」，不去改别人的冻结文件。
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

/// 出站 `X-Request-ID`：只透传调用方给的那个（上游 `cloudRuntimeRequestID` 的第一支；
/// 第二支读 chi 的进程内 request id，本仓没有 request-id 中间件 —— `docs/32` §46 的 D-5）。
fn request_id(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

/// 透传查询串（上游 `withQuery` 那一支：`query = r.URL.Query()`）。
fn forward_query(raw_query: Option<&str>) -> Vec<(String, String)> {
    billing::parse_query(raw_query.unwrap_or_default())
}

/// 读并校验转发体（上游 `readCloudRuntimeJSONBody`）：413（>1 MiB）/ 400（空体、非法 JSON）。
///
/// 判定顺序与上游逐字一致：读（超限即 413）→ `TrimSpace` 后为空 ⇒ 400「体是必需的」→
/// JSON 语法 ⇒ 400「体非法」。**体本身不 trim**（转发的是原始字节）。
#[allow(clippy::result_large_err)] // `Err` 变体就是我们要原样返回的那个响应（不装一层 Box）。
async fn read_cloud_runtime_json_body(request: Request) -> Result<Vec<u8>, Response> {
    let limit = MAX_CLOUD_RUNTIME_REQUEST_BODY_SIZE;
    let Ok(bytes) = axum::body::to_bytes(request.into_body(), limit + 1).await else {
        // 读失败只有两种来源：超限（axum 的 `LengthLimitError`）与连接中断。
        // 两者都落在上游「体太大 / 体读不出来」的同一侧，而客户端断开时响应没人看 ——
        // 取上限那一支（`docs/32` §46 的 D-6 登记了这一处归并）。
        return Err(error_with_code(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            MSG_BODY_TOO_LARGE,
        ));
    };
    if bytes.len() > limit {
        return Err(error_with_code(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            MSG_BODY_TOO_LARGE,
        ));
    }
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Err(error_with_code(
            StatusCode::BAD_REQUEST,
            "validation_error",
            MSG_BODY_REQUIRED,
        ));
    }
    if serde_json::from_slice::<serde_json::Value>(&bytes).is_err() {
        return Err(error_with_code(
            StatusCode::BAD_REQUEST,
            "validation_error",
            MSG_BODY_INVALID,
        ));
    }
    Ok(bytes.to_vec())
}

#[cfg(test)]
mod tests;
