//! `/api/cloud-runtime/*` 11 条（节点池管理面）—— **写者 M9-11** / `LUM-2116`。
//!
//! 上游：`internal/handler/cloud_runtime.go`（208 行，11 条 handler）+ `router.go:2299-2311`。
//! 出站契约（11 条路径 / 方法 / `withUserID` / `withQuery` / `withBody`）在
//! [`mc_cloud::runtime`] —— 本地**不落任何表**（`docs/62` §9.4：双侧都是 0 张节点池表，
//! 节点在 `multica-cloud` 自己的库里）。
//!
//! # 授权链（**三层**，顺序与上游逐字一致）
//!
//! | 层 | 本地实现 | 判据 |
//! | --- | --- | --- |
//! | ① workspace 解析（11 条） | [`resolve_workspace`]（本仓既有入口） | 四个来源都缺 ⇒ **400** |
//! | ② 成员校验（11 条） | [`require_workspace_member`] | 非成员 ⇒ **404** `workspace` |
//! | ③ 会话（11 条） | `AuthUser` 提取器 | 缺 `X-Multica-User-Id` ⇒ **401** |
//!
//! 上游 ①② 是 `router.go:1948` 那个 `r.Group` 上的 `r.Use(middleware.
//! RequireWorkspaceMember(queries))` —— 本片起手**逐字复核**过：该组在 `2299` 行处**仍然打开**
//! （`r.Route("/api/cloud-runtime")` 在它内部），且组内**没有**别的 `r.Use`。
//!
//! # 🔴 本片**不挂**机器凭据闸（与 M9-1 / M9-2 相反，登记 `docs/32` §49）
//!
//! M9-1（billing 8 条）与 M9-2（subscriptions 7 条）都在路由组上挂了
//! `require_human_actor`（上游 `router.go:1911` / `:1930` 的 `r.Use(handler.RequireHumanActor)`）。
//! **本簇上游没有**：`/api/cloud-runtime` 所在的组（`router.go:1948`）只有
//! `RequireWorkspaceMember`，`cloud_runtime.go` 全文也没有任何 actor 判定
//! ⇒ 挂上去会把节点池面**收窄**成上游允许的面之外的面（`mat_` / `mcn_` 凭据本可以调它）。
//! 「不挂」是逐字对齐，**不是**漏写；用例钉住「机器凭据**不被**本片拦」。
//!
//! # 两条反向验收（`DoD` 原文，仍然成立）
//!
//! 1. `GET /api/cloud-runtime/healthz` 与 `.../readyz` **不是**服务探针 `/healthz` / `/readyz`
//!    （那两个在 `router.go:1400-1401`，属 M10）—— 本文件**不**注册它们、**不**混实现；
//! 2. 11 条全部在 workspace **member** 组 ⇒ 无 anonymous 面、无 fixture（`docs/62` §4.2）。
//!
//! # 错误映射（`docs/62` §2.6，四行）
//!
//! `Disabled` ⇒ 403 / `InvalidBaseUrl` ⇒ 500 / `Timeout` ⇒ **504** / 其余（连接拒、体超限）
//! ⇒ **502**。**云侧的 4xx/5xx 不是错误**：[`mc_cloud::transport::Client::send`] 对任何
//! HTTP 状态都返回 `Ok(Response)`（上游 `doInner` 的语义），本文件把它**原样**写回客户端。
//!
//! **错误路径不回显云侧响应体**（`docs/62` §2.4 判据 ③）：本文件自己的错误体一律是静态
//! message，既不带出站 URL、也不带云侧体。
//!
//! # 形态（`docs/62` §1.4：全波 `dual-form required: 0`）
//!
//! 11 条**只按上游字面量**注册那一形态。⚠️ 其中 `GET /api/cloud-runtime/` **带尾斜杠**
//! （上游 `r.Route("/api/cloud-runtime")` + 组内 `r.Get("/")` 的拼接结果，`docs/fixtures/
//! upstream-routes.tsv` 逐字如此）—— **不**额外注册不带斜杠那一形态。
//! 本片**零路径参数**（节点 id 走体），所以没有 `{…}` / `:…}` 的取舍问题。

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Query, RawQuery, Request, State};
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use mc_cloud::runtime;
use mc_cloud::{CloudError, Request as CloudRequest, Response as CloudResponse};
use mc_core::cloud::MAX_CLOUD_RUNTIME_REQUEST_BODY_SIZE;
use mc_core::Id;
use mc_errors::ErrorResponse;
use serde::Serialize;

use crate::error::ApiError;
use crate::routes::auth_user::AuthUser;
use crate::routes::invitations::require_workspace_member;
use crate::routes::issues::{resolve_workspace, WorkspaceQuery};
use crate::state::AppState;

/// 「未配置」（上游 `writeFeatureDisabled(w, "cloud_runtime_not_configured", …)` 逐字）。
const CODE_NOT_CONFIGURED: &str = "cloud_runtime_not_configured";
/// 「配了但非法」（`docs/62` §2.6 的 500 行）。
const CODE_MISCONFIGURED: &str = "cloud_runtime_misconfigured";
/// 超时（上游这一行**没有** code，本地按本仓信封补一个；见 `docs/32` §49）。
const CODE_TIMEOUT: &str = "cloud_runtime_timeout";
/// 其余传输失败（与 `routes/cloud/{billing,subscriptions}` 的 502 同一个 code）。
const CODE_FAILED: &str = "upstream_error";
/// 校验类 400（全仓同款）。
const CODE_VALIDATION: &str = "validation_error";
/// 体超限 413（全仓同款）。
const CODE_TOO_LARGE: &str = "payload_too_large";

const MSG_NOT_CONFIGURED: &str = "cloud runtime is not configured";
const MSG_MISCONFIGURED: &str = "cloud runtime is misconfigured";
/// 上游 `writeError(w, 504, …)` 逐字。
const MSG_TIMEOUT: &str = "cloud runtime request timed out";
/// 上游 `writeError(w, 502, …)` 逐字。
const MSG_FAILED: &str = "cloud runtime request failed";
/// 上游 `readCloudRuntimeJSONBody` 的两条 400 + 一条 413 文本（逐字）。
const MSG_BODY_REQUIRED: &str = "request body is required";
const MSG_BODY_INVALID: &str = "invalid request body";
const MSG_BODY_TOO_LARGE: &str = "request body is too large";

/// 11 条 workspace 级节点池代理。
///
/// ⚠️ **零注册点改动**：anchor（M9-0）已把 `pub mod cloud_runtime;` 与
/// `mount_slice_cloud_runtime()` 预声明好（`routes/{mod,mount}.rs` 是**冻结**文件）—— 本片
/// 只填这两个文件的**函数体**。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        // ⚠️ `GET /api/cloud-runtime` 与 `GET /api/cloud-runtime/` **两个都要注册**。
        // 上游是 chi 的 `r.Route("/api/cloud-runtime")` + 组内 `r.Get("/")`（`router.go:2299-2300`）
        // —— Mount + child `/` 让**两种写法都命中同一个 handler**；axum/matchit 没有 Mount
        // 概念，只注册带斜杠那一形态会让不带斜杠的客户端拿到 **404**。
        // 先例 = `routes/runtimes.rs:83-84`（同一形态的既有做法），判据 = 门 ⑦b
        // （`MISSING_ALIAS` 是 defect、`EXTRA_ALIAS` 只是 warning）。
        // ⑦ 会把这一对**折叠**比较（"same route with and without trailing slash (legal;
        // folded in comparison)"）⇒ 不影响 `implemented` / `known_gap` 的口径。
        .route("/api/cloud-runtime", get(get_service))
        .route("/api/cloud-runtime/", get(get_service))
        .route("/api/cloud-runtime/healthz", get(get_health))
        .route("/api/cloud-runtime/readyz", get(get_ready))
        .route(
            "/api/cloud-runtime/nodes",
            get(list_nodes).post(create_node).delete(delete_node),
        )
        .route(
            "/api/cloud-runtime/nodes/start",
            axum::routing::post(start_node),
        )
        .route(
            "/api/cloud-runtime/nodes/stop",
            axum::routing::post(stop_node),
        )
        .route(
            "/api/cloud-runtime/nodes/reboot",
            axum::routing::post(reboot_node),
        )
        .route(
            "/api/cloud-runtime/nodes/status",
            axum::routing::post(node_status),
        )
        .route(
            "/api/cloud-runtime/nodes/exec",
            axum::routing::post(exec_node),
        )
}

// ---------------------------------------------------------------------------
// 作用域解析（① workspace + ② 成员）
// ---------------------------------------------------------------------------

/// 11 条共用的作用域：workspace 解析（400）→ **成员**校验（非成员 404）。
///
/// 与 `routes/cloud/subscriptions.rs` 的 `member_scope` 同款（读面 member，写面 admin）——
/// 本簇 11 条**全是** member，故只需要这一档。
#[allow(clippy::result_large_err)] // `Err` 就是要原样返回的响应（不装一层 Box）。
async fn member_scope(
    state: &AppState,
    headers: &HeaderMap,
    query: &WorkspaceQuery,
    user: AuthUser,
) -> Result<Id, Response> {
    let workspace_id = resolve_workspace(state, headers, query)
        .await
        .map_err(|error| ApiError(error).into_response())?;
    require_workspace_member(state, workspace_id, user.id())
        .await
        .map_err(|error| ApiError(error).into_response())?;
    Ok(workspace_id)
}

// ---------------------------------------------------------------------------
// 11 个 handler（每个 5–14 行：出站契约在 mc-cloud，语义在 proxy）
// ---------------------------------------------------------------------------

/// `GET /api/cloud-runtime/`（上游 `GetCloudRuntimeService`，`withUserID`）。
async fn get_service(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
) -> Response {
    if let Err(response) = member_scope(&state, &headers, &query, user).await {
        return response;
    }
    proxy(
        &state,
        runtime::service_request(user.id(), request_id(&headers).as_deref()),
    )
    .await
}

/// `GET /api/cloud-runtime/healthz`（上游 `GetCloudRuntimeHealth`）。
///
/// ⚠️ **不注入身份**（上游 `withUserID` 关闭）—— 云侧 fleet 服务自己的探针。
/// ⚠️ 但**会话闸仍然生效**：上游这两条在 `RequireWorkspaceMember` 组里，本地照抄。
async fn get_health(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
) -> Response {
    if let Err(response) = member_scope(&state, &headers, &query, user).await {
        return response;
    }
    proxy(
        &state,
        runtime::healthz_request(request_id(&headers).as_deref()),
    )
    .await
}

/// `GET /api/cloud-runtime/readyz`（上游 `GetCloudRuntimeReady`，同样不注入身份）。
async fn get_ready(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
) -> Response {
    if let Err(response) = member_scope(&state, &headers, &query, user).await {
        return response;
    }
    proxy(
        &state,
        runtime::readyz_request(request_id(&headers).as_deref()),
    )
    .await
}

/// `GET /api/cloud-runtime/nodes`（上游 `ListCloudRuntimeNodes`）—— 唯一带 query 的一条。
async fn list_nodes(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
    RawQuery(raw_query): RawQuery,
) -> Response {
    if let Err(response) = member_scope(&state, &headers, &query, user).await {
        return response;
    }
    proxy(
        &state,
        runtime::list_nodes_request(
            user.id(),
            request_id(&headers).as_deref(),
            runtime::parse_query(raw_query.as_deref().unwrap_or_default()),
        ),
    )
    .await
}

/// `POST /api/cloud-runtime/nodes`（上游 `CreateCloudRuntimeNode`）。
///
/// 上游逐字注明的取舍：云侧自己铸造节点级 PAT ⇒ **不再**转发调用方的 `mul_` PAT，只转发体。
async fn create_node(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
    request: Request,
) -> Response {
    if let Err(response) = member_scope(&state, &headers, &query, user).await {
        return response;
    }
    let request_id = request_id(&headers);
    let body = match read_cloud_runtime_json_body(request).await {
        Ok(body) => body,
        Err(response) => return response,
    };
    proxy(
        &state,
        runtime::create_node_request(user.id(), request_id.as_deref(), body),
    )
    .await
}

/// `DELETE /api/cloud-runtime/nodes`（上游 `DeleteCloudRuntimeNode`）—— 节点 id 在**体**里。
async fn delete_node(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
    request: Request,
) -> Response {
    if let Err(response) = member_scope(&state, &headers, &query, user).await {
        return response;
    }
    let request_id = request_id(&headers);
    let body = match read_cloud_runtime_json_body(request).await {
        Ok(body) => body,
        Err(response) => return response,
    };
    proxy(
        &state,
        runtime::delete_node_request(user.id(), request_id.as_deref(), body),
    )
    .await
}

/// `POST /api/cloud-runtime/nodes/start`（上游 `StartCloudRuntimeNode`）。
async fn start_node(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
    request: Request,
) -> Response {
    if let Err(response) = member_scope(&state, &headers, &query, user).await {
        return response;
    }
    node_action(&state, &headers, user, request, runtime::start_node_request).await
}

/// `POST /api/cloud-runtime/nodes/stop`（上游 `StopCloudRuntimeNode`）。
async fn stop_node(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
    request: Request,
) -> Response {
    if let Err(response) = member_scope(&state, &headers, &query, user).await {
        return response;
    }
    node_action(&state, &headers, user, request, runtime::stop_node_request).await
}

/// `POST /api/cloud-runtime/nodes/reboot`（上游 `RebootCloudRuntimeNode`）。
async fn reboot_node(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
    request: Request,
) -> Response {
    if let Err(response) = member_scope(&state, &headers, &query, user).await {
        return response;
    }
    node_action(
        &state,
        &headers,
        user,
        request,
        runtime::reboot_node_request,
    )
    .await
}

/// `POST /api/cloud-runtime/nodes/status`（上游 `GetCloudRuntimeNodeStatus`）。
///
/// ⚠️ 上游查状态用 **POST** ⇒ 本地**不**"顺手"改成 GET。
async fn node_status(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
    request: Request,
) -> Response {
    if let Err(response) = member_scope(&state, &headers, &query, user).await {
        return response;
    }
    node_action(
        &state,
        &headers,
        user,
        request,
        runtime::node_status_request,
    )
    .await
}

/// `POST /api/cloud-runtime/nodes/exec`（上游 `ExecCloudRuntimeNode`）。
async fn exec_node(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
    request: Request,
) -> Response {
    if let Err(response) = member_scope(&state, &headers, &query, user).await {
        return response;
    }
    node_action(&state, &headers, user, request, runtime::exec_node_request).await
}

/// 5 条「节点动作」的公共骨架（读体 → 出站）。
///
/// 五条**逐字同款**（上游 5 个 handler 只差 `path` 一个实参）⇒ 差别由**出站请求构造函数**
/// 这个实参承载：少写一个 handler 就少一处可能把 `stop` 写成 `reboot`。
async fn node_action(
    state: &AppState,
    headers: &HeaderMap,
    user: AuthUser,
    request: Request,
    build: fn(Id, Option<&str>, Vec<u8>) -> CloudRequest,
) -> Response {
    let request_id = request_id(headers);
    let body = match read_cloud_runtime_json_body(request).await {
        Ok(body) => body,
        Err(response) => return response,
    };
    proxy(state, build(user.id(), request_id.as_deref(), body)).await
}

// ---------------------------------------------------------------------------
// 出站与响应的公共骨架
// ---------------------------------------------------------------------------

/// 一次出站代理（上游 `proxyCloudRuntime`）。
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

/// 本仓标准的**嵌套**错误信封（`{"error":{"code":…,"message":…}}`；形状与 `mc_errors` 一致）。
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
/// 第二支读 chi 的进程内 request id，本仓没有 request-id 中间件 —— `docs/32` §49 的 D-5）。
fn request_id(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

/// 读并校验转发体（上游 `readCloudRuntimeJSONBody`）：413（>1 MiB）/ 400（空体、非法 JSON）。
///
/// 判定顺序与上游逐字一致：读（超限即 413）→ 全空白 ⇒ 400「体是必需的」→ JSON 语法 ⇒ 400
/// 「体非法」。**体本身不 trim**（转发的是原始字节：云侧要对它签名）。
#[allow(clippy::result_large_err)] // `Err` 变体就是我们要原样返回的那个响应（不装一层 Box）。
async fn read_cloud_runtime_json_body(request: Request) -> Result<Vec<u8>, Response> {
    let limit = MAX_CLOUD_RUNTIME_REQUEST_BODY_SIZE;
    let Ok(bytes) = axum::body::to_bytes(request.into_body(), limit + 1).await else {
        // 读失败只有两种来源：超限（axum 的 `LengthLimitError`）与连接中断。
        // 两者都落在上游「体太大 / 体读不出来」的同一侧，而客户端断开时响应没人看 ——
        // 取上限那一支（`docs/32` §49 的 D-6 登记了这一处归并）。
        return Err(error_with_code(
            StatusCode::PAYLOAD_TOO_LARGE,
            CODE_TOO_LARGE,
            MSG_BODY_TOO_LARGE,
        ));
    };
    if bytes.len() > limit {
        return Err(error_with_code(
            StatusCode::PAYLOAD_TOO_LARGE,
            CODE_TOO_LARGE,
            MSG_BODY_TOO_LARGE,
        ));
    }
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Err(error_with_code(
            StatusCode::BAD_REQUEST,
            CODE_VALIDATION,
            MSG_BODY_REQUIRED,
        ));
    }
    if serde_json::from_slice::<serde_json::Value>(&bytes).is_err() {
        return Err(error_with_code(
            StatusCode::BAD_REQUEST,
            CODE_VALIDATION,
            MSG_BODY_INVALID,
        ));
    }
    Ok(bytes.to_vec())
}

#[cfg(test)]
mod tests;
