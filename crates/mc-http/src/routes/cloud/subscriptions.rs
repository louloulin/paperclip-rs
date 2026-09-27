//! `/api/cloud-subscriptions/*` 7 条（**写者 M9-2** / `LUM-1817` / `docs/62` §4.1 的第 3 行）。
//!
//! 上游：`internal/handler/cloud_billing.go` 的 `L54–L355`（7 条 subscription handler +
//! `requireCloudSubscriptionWorkspace` + `proxyCloudSubscription`）+ `router.go:1925–1945` 的
//! 两段 route group。出站契约（7 条路径 / 方法 / 注入体 / 幂等键两档）在
//! [`mc_cloud::subscriptions`] 的模块头 —— 本地**不落任何表**（`docs/62` §9.4）。
//!
//! # 授权链（`docs/62` §1.5 的 B 行 —— 四层，**顺序**与上游逐字一致）
//!
//! | 层 | 本地实现 | 判据 |
//! | --- | --- | --- |
//! | ① 机器凭据闸（7 条全部，**两层**） | 路由组 `.route_layer(require_human_actor)` + 每个 handler 的 [`HumanActor`] 提取器 | `X-Actor-Source ∈ {task_token, cloud_pat}` ⇒ **403**，且**不产生出站请求** |
//! | ② rollout flag（**只有写 5 条**） | [`manager_scope`] 的第一格 | `billing_workspace_subscriptions` 关 ⇒ **403** `workspace_subscriptions_disabled` |
//! | ③ workspace 解析（7 条） | [`resolve_workspace`]（本仓既有入口） | 四个来源都缺 ⇒ **400** |
//! | ④ 成员 / 角色（7 条） | [`require_workspace_member`]（读 2 条）/ [`require_workspace_admin`]（写 5 条） | 非成员 ⇒ **404** `workspace`；成员但非 `owner\|admin` ⇒ **403** |
//!
//! ① 是**双层**（路由组 layer = 上游 `r.Use(handler.RequireHumanActor)`；handler 提取器 = 上游
//! backstop 里的 `isMachineCredentialActor(r)` 那一段 —— M9-1 的 8 条**只**有前者，所以那个文件
//! 的模块头把「handler 级 backstop」写成"归 M9-2"）；②③④ 都在 handler 体里
//! —— 上游的 flag 检查**本来**就在 handler 级 backstop `requireCloudSubscriptionWorkspace`
//! 里（`cloud_billing.go:78`），router 上**没有**对应 layer，本片照抄这个层级
//! （② 在 [`manager_scope`] 的第一格，③④ 紧随其后）。
//!
//! # 三层语义（`docs/62` §2.5 的 B 行，**逐条可测**）
//!
//! | 情形 | 本地 | code / message |
//! | --- | :-: | --- |
//! | `MULTICA_CLOUD_URL` 缺 / 空 | **403** | `cloud_runtime_not_configured` |
//! | 配了但非法 | **500** | `cloud_runtime_misconfigured` |
//! | 无会话（缺 `X-Multica-User-Id`） | **401** | `unauthorized` |
//! | 机器凭据 | **403** | `this endpoint is only available to human actors` |
//! | 非成员 | **404** | `workspace`（隐藏资源存在性，与全仓 member 口径一致） |
//! | 成员但非 `owner\|admin`（写 5 条） | **403** | `insufficient permissions`（本地副本文本） |
//! | flag 关（写 5 条） | **403** | `workspace_subscriptions_disabled` |
//!
//! # 错误映射（`docs/62` §2.6）
//!
//! `Disabled` ⇒ 403 / `InvalidBaseUrl` ⇒ 500 / `Timeout` ⇒ **504** / 其余 ⇒ **502**。
//! **云侧的 4xx/5xx 不是错误**：`transport::Client::send` 对任何 HTTP 状态都返回
//! `Ok(Response)`，本文件把它**原样**写回客户端。云侧响应与错误路径的**逐字**口径与
//! `routes/cloud/billing.rs`（M9-1）**同形**——本仓既有约定是「各切片各自持有本地副本」，
//! 而不去改别的切片的写集文件。
//!
//! **错误路径不回显云侧响应体**（`docs/62` §2.4 判据 ③）：本文件自己的错误体一律是静态
//! message（[`error_with_code`]），既不带出站 URL、也不带云侧体。
//!
//! # 三条只属于本片的契约（`docs/62` §2.7 / §4.2，逐条落在代码上）
//!
//! 1. 🔴 **`workspace_id` 由服务端解析后注入请求体**：客户端在 JSON 里走私的
//!    `workspace_id` / `customer_email` **在反序列化那一步**就被丢掉（本地请求 DTO
//!    `CloudSubscriptionCheckoutRequest` 与注入体 DTO `…UpstreamRequest` 是两个类型），
//!    注入体由 [`mc_cloud::subscriptions::checkout_body`] 现场拼 ⇒ 覆盖是**结构上**的；
//! 2. **`Idempotency-Key` 两档上限**（255 / 座位购买 200；超限与缺失都是 **400**）；
//! 3. **座位购买三个并发字段逐字透传**（`expected_current_seats` /
//!    `expected_purchase_version` / `accepted_proration_amount` —— 本地**不**解释它们）。
//!
//! # 与上游的两处**有意**偏离（登记 `docs/32` §47）
//!
//! - **读 2 条不 gate rollout flag**：上游 handler 的 backstop 对 7 条**一律**检查 flag
//!   （`cloud_billing.go:78` 在 `requireCloudSubscriptionWorkspace` 里，读 handler 也走它），
//!   上游测试 `TestCloudWorkspaceSubscriptionsDisabledByDefault` 甚至对读面断言 403；
//!   而本片 `DoD` 第 1 条、`docs/62` §2.5 的 B 行、anchor 的桩文档三处一致写「读 2 条不受
//!   flag 影响」⇒ 按本仓口径落地（读面**只**是 member 可读）。翻回去只需把
//!   [`member_scope`] 也照 [`manager_scope`] 的第一格加一行（一处）。
//! - **payer email 只从库里读**：上游从 `user.email` 取；本仓 `routes/invitations.rs` 有一个
//!   `X-Multica-User-Email` dev-mode 覆盖口，本片**有意不接**它 —— checkout 的付款人身份
//!   必须是服务端解析的（上游逐字：「A caller cannot smuggle … Stripe payer identity」）。
//!
//! # 形态（`docs/62` §1.4 实测 `dual-form required: 0`）
//!
//! 7 条**只按上游字面量注册**那一形态；本片无路径参数。同 path+method 重复注册 ⇒ axum 在
//! **启动时 panic**（anchor 已把合并点与子文件分开，本片**零注册点改动**）。

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Query, Request, State};
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use mc_cloud::subscriptions;
use mc_cloud::{CloudError, Request as CloudRequest, Response as CloudResponse};
use mc_core::cloud::{
    is_valid_billing_interval, CloudSubscriptionCheckoutRequest,
    CloudSubscriptionSeatPurchaseRequest, IDEMPOTENCY_KEY_HEADER,
    MAX_CLOUD_RUNTIME_REQUEST_BODY_SIZE, MAX_IDEMPOTENCY_KEY_LENGTH,
    MAX_SEAT_PURCHASE_IDEMPOTENCY_KEY_LENGTH,
};
use mc_core::Id;
use mc_errors::ErrorResponse;
use mc_feature_flags::FeatureKey;
use serde::Serialize;

use crate::actor_guard::{require_human_actor, HumanActor};
use crate::error::ApiError;
use crate::routes::auth_user::AuthUser;
use crate::routes::invitations::{require_workspace_admin, require_workspace_member};
use crate::routes::issues::{resolve_workspace, WorkspaceQuery};
use crate::state::AppState;

/// 本片的 rollout flag（上游 `featureflags.BillingWorkspaceSubscriptions`，逐字）。
pub const SUBSCRIPTIONS_FLAG: &str = "billing_workspace_subscriptions";

/// 「未配置」（上游 `writeFeatureDisabled(w, "cloud_runtime_not_configured", …)`）。
const CODE_NOT_CONFIGURED: &str = "cloud_runtime_not_configured";
/// 「配了但非法」（`docs/62` §2.6 的 500 行）。
const CODE_MISCONFIGURED: &str = "cloud_runtime_misconfigured";
/// 超时（本地按本仓信封补的 code；见 `docs/32` §46 的 D-7/§47）。
const CODE_TIMEOUT: &str = "cloud_runtime_timeout";
/// 其余传输失败（与 `routes/{composio,billing}` 的 502 同一个 code）。
const CODE_FAILED: &str = "upstream_error";
/// flag 关（上游 `writeFeatureDisabled(w, "workspace_subscriptions_disabled", …)`）。
const CODE_SUBSCRIPTIONS_DISABLED: &str = "workspace_subscriptions_disabled";
/// 校验类 400（全仓同款）。
const CODE_VALIDATION: &str = "validation_error";
/// 体超限 413（全仓同款）。
const CODE_TOO_LARGE: &str = "payload_too_large";
/// 本仓 `Error::Internal` 的 code（payer 解析失败的两条 500）。
const CODE_INTERNAL: &str = "internal_error";

const MSG_NOT_CONFIGURED: &str = "cloud runtime is not configured";
const MSG_MISCONFIGURED: &str = "cloud runtime is misconfigured";
/// 上游 `writeError(w, 504, …)` 逐字。
const MSG_TIMEOUT: &str = "cloud runtime request timed out";
/// 上游 `writeError(w, 502, …)` 逐字。
const MSG_FAILED: &str = "cloud runtime request failed";
/// 上游 `writeFeatureDisabled` 的文本逐字。
const MSG_SUBSCRIPTIONS_DISABLED: &str = "workspace subscriptions are not enabled";
/// 上游 `readCloudRuntimeJSONBody` 的两条 400 + 一条 413 文本（逐字）。
const MSG_BODY_REQUIRED: &str = "request body is required";
const MSG_BODY_INVALID: &str = "invalid request body";
const MSG_BODY_TOO_LARGE: &str = "request body is too large";
/// 上游三条本地校验文本（逐字）。
const MSG_INTERVAL_INVALID: &str = "interval must be month or year";
const MSG_KEY_REQUIRED: &str = "Idempotency-Key or idempotency_key is required";
const MSG_KEY_TOO_LONG: &str = "idempotency key must be at most 255 bytes";
const MSG_SEAT_KEY_TOO_LONG: &str = "seat purchase idempotency key must be at most 200 bytes";
const MSG_SEATS_NOT_POSITIVE: &str = "additional_seats must be positive";
const MSG_SEAT_PURCHASE_INVALID: &str = "invalid seat purchase confirmation";
/// 上游 checkout 付款人解析的两条 500 文本（逐字）。
const MSG_PAYER_FAILED: &str = "failed to resolve checkout payer";
const MSG_PAYER_EMAIL_UNAVAILABLE: &str = "checkout payer email is unavailable";

/// 7 条 workspace 级代理。
///
/// 🔴 七条**全部**挂机器凭据闸（`docs/62` §1.5 的 B 行）：workspace 订阅同样能**搬钱**
/// （checkout 会话 / 座位购买 / Billing Portal），而 `mat_` / `mcn_` 凭据以**属主**身份行事
/// （R-M9-2：「没有这条闸就不许合并 M9-1/M9-2」）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .merge(member_routes())
        .merge(manager_routes())
        // 上游 `router.go:1930` 的 `r.Use(handler.RequireHumanActor)`：整个路由组一条闸。
        .route_layer(axum::middleware::from_fn(require_human_actor))
}

/// 读面（2 条）：**member 可读**，**不** gate rollout flag（见模块头的偏离登记）。
fn member_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/cloud-subscriptions/summary",
            get(get_subscription_summary),
        )
        .route(
            "/api/cloud-subscriptions/prices",
            get(get_subscription_prices),
        )
}

/// 写面（5 条）：`owner|admin` + rollout flag。
///
/// ⚠️ flag 的判定点在 [`manager_scope`] 里（**不是**中间件）：上游的 flag 检查本来就在
/// handler 级 backstop `requireCloudSubscriptionWorkspace` 里（`cloud_billing.go:78`），
/// router 上**没有**对应的 layer ⇒ 本片照抄这个层级。
fn manager_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/cloud-subscriptions/checkout-sessions",
            post(create_checkout_session),
        )
        .route(
            "/api/cloud-subscriptions/seats/purchase-preview",
            post(preview_seat_purchase),
        )
        .route(
            "/api/cloud-subscriptions/seats/purchases",
            post(purchase_seats),
        )
        .route(
            "/api/cloud-subscriptions/seats/reconcile",
            post(reconcile_seats),
        )
        .route(
            "/api/cloud-subscriptions/portal-sessions",
            post(create_portal_session),
        )
}

/// rollout flag 是否打开（唯一判定点；用例经它构造两个态）。
#[must_use]
pub fn subscriptions_enabled(state: &AppState) -> bool {
    state
        .feature_flags
        .is_enabled(&FeatureKey::new(SUBSCRIPTIONS_FLAG))
}

/// flag 关 ⇒ **403** `workspace_subscriptions_disabled`（上游 `writeFeatureDisabled`）。
fn subscriptions_disabled() -> Response {
    error_with_code(
        StatusCode::FORBIDDEN,
        CODE_SUBSCRIPTIONS_DISABLED,
        MSG_SUBSCRIPTIONS_DISABLED,
    )
}

// ---------------------------------------------------------------------------
// 作用域解析（③ workspace + ④ 成员 / 角色）
// ---------------------------------------------------------------------------

/// 读面作用域：workspace 解析（400）→ **成员**校验（非成员 404）。
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

/// 写面作用域：**flag**（403）→ workspace 解析（400）→ 成员（404）→ **`owner|admin`**（403）。
///
/// 顺序与上游逐字一致（handler backstop 先查 flag，再查 workspace 与成员角色）。
/// 后两格复用 [`require_workspace_admin`] 自己的两步查询（非成员 ⇒ 404，成员但角色不足 ⇒ 403），
/// 免得同一条 SQL 在一处写两遍。
#[allow(clippy::result_large_err)]
async fn manager_scope(
    state: &AppState,
    headers: &HeaderMap,
    query: &WorkspaceQuery,
    user: AuthUser,
) -> Result<Id, Response> {
    if !subscriptions_enabled(state) {
        return Err(subscriptions_disabled());
    }
    let workspace_id = resolve_workspace(state, headers, query)
        .await
        .map_err(|error| ApiError(error).into_response())?;
    require_workspace_admin(state, workspace_id, user.id())
        .await
        .map_err(|error| ApiError(error).into_response())?;
    Ok(workspace_id)
}

// ---------------------------------------------------------------------------
// 7 个 handler
// ---------------------------------------------------------------------------

/// `GET /api/cloud-subscriptions/summary`（上游 `GetCloudWorkspaceSubscriptionSummary`）。
async fn get_subscription_summary(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    _actor: HumanActor,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
) -> Response {
    let workspace_id = match member_scope(&state, &headers, &query, user).await {
        Ok(workspace_id) => workspace_id,
        Err(response) => return response,
    };
    proxy(
        &state,
        subscriptions::summary_request(workspace_id, user.id(), request_id(&headers).as_deref()),
    )
    .await
}

/// `GET /api/cloud-subscriptions/prices`（上游 `GetCloudWorkspaceSubscriptionPrices`）。
async fn get_subscription_prices(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    _actor: HumanActor,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
) -> Response {
    let workspace_id = match member_scope(&state, &headers, &query, user).await {
        Ok(workspace_id) => workspace_id,
        Err(response) => return response,
    };
    proxy(
        &state,
        subscriptions::prices_request(workspace_id, user.id(), request_id(&headers).as_deref()),
    )
    .await
}

/// `POST /api/cloud-subscriptions/checkout-sessions`
/// （上游 `CreateCloudWorkspaceSubscriptionCheckout`）。
///
/// 上游判定顺序逐字：体（413/400）→ 反序列化 → `interval` → 幂等键 → 付款人 id → 付款人邮箱
/// → 拼注入体 → 出站。
async fn create_checkout_session(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    _actor: HumanActor,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
    request: Request,
) -> Response {
    let workspace_id = match manager_scope(&state, &headers, &query, user).await {
        Ok(workspace_id) => workspace_id,
        Err(response) => return response,
    };
    let request_id = request_id(&headers);
    let body = match read_cloud_runtime_json_body(request).await {
        Ok(body) => body,
        Err(response) => return response,
    };
    let Ok(input) = serde_json::from_slice::<CloudSubscriptionCheckoutRequest>(&body) else {
        return validation(MSG_BODY_INVALID);
    };
    if !is_valid_billing_interval(&input.interval) {
        return validation(MSG_INTERVAL_INVALID);
    }
    let key = match subscriptions::resolve_idempotency_key(
        input.idempotency_key.as_deref(),
        header_key(&headers).as_deref(),
        MAX_IDEMPOTENCY_KEY_LENGTH,
    ) {
        Ok(key) => key,
        Err(subscriptions::IdempotencyKeyError::Missing) => return validation(MSG_KEY_REQUIRED),
        Err(subscriptions::IdempotencyKeyError::TooLong { .. }) => {
            return validation(MSG_KEY_TOO_LONG)
        }
    };
    let email = match payer_email(&state, user.id()).await {
        Ok(email) => email,
        Err(response) => return response,
    };
    // ⚠️ 转发的头**只**看请求头（上游 `cloudSubscriptionIdempotencyHeaders`）：客户端只给了
    // 体键时，云侧拿到的 `idempotency_key` 在**体**里、头是空的 —— 两条都由用例钉住。
    let forwarded = subscriptions::forwarded_idempotency_key(header_key(&headers).as_deref());
    proxy(
        &state,
        subscriptions::checkout_session_request(
            user.id(),
            request_id.as_deref(),
            subscriptions::checkout_body(workspace_id, &input.interval, Some(&key), &email),
            forwarded.as_deref(),
        ),
    )
    .await
}

/// `POST /api/cloud-subscriptions/seats/purchase-preview`
/// （上游 `PreviewCloudWorkspaceSubscriptionSeatPurchase`）：**只**接受增量座位数。
async fn preview_seat_purchase(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    _actor: HumanActor,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
    request: Request,
) -> Response {
    let workspace_id = match manager_scope(&state, &headers, &query, user).await {
        Ok(workspace_id) => workspace_id,
        Err(response) => return response,
    };
    let request_id = request_id(&headers);
    let body = match read_cloud_runtime_json_body(request).await {
        Ok(body) => body,
        Err(response) => return response,
    };
    let Ok(input) = serde_json::from_slice::<
        mc_core::cloud::CloudSubscriptionSeatPurchasePreviewRequest,
    >(&body) else {
        return validation(MSG_BODY_INVALID);
    };
    if input.additional_seats < 1 {
        return validation(MSG_SEATS_NOT_POSITIVE);
    }
    proxy(
        &state,
        subscriptions::seat_purchase_preview_request(
            workspace_id,
            user.id(),
            request_id.as_deref(),
            subscriptions::seat_purchase_preview_body(input.additional_seats),
        ),
    )
    .await
}

/// `POST /api/cloud-subscriptions/seats/purchases`
/// （上游 `PurchaseCloudWorkspaceSubscriptionSeats`）：转发用户确认过的报价。
async fn purchase_seats(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    _actor: HumanActor,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
    request: Request,
) -> Response {
    let workspace_id = match manager_scope(&state, &headers, &query, user).await {
        Ok(workspace_id) => workspace_id,
        Err(response) => return response,
    };
    let request_id = request_id(&headers);
    let body = match read_cloud_runtime_json_body(request).await {
        Ok(body) => body,
        Err(response) => return response,
    };
    let Ok(mut input) = serde_json::from_slice::<CloudSubscriptionSeatPurchaseRequest>(&body)
    else {
        return validation(MSG_BODY_INVALID);
    };
    if !is_valid_seat_purchase(&input) {
        return validation(MSG_SEAT_PURCHASE_INVALID);
    }
    let key = match subscriptions::resolve_idempotency_key(
        input.idempotency_key.as_deref(),
        header_key(&headers).as_deref(),
        MAX_IDEMPOTENCY_KEY_LENGTH,
    ) {
        Ok(key) => key,
        Err(subscriptions::IdempotencyKeyError::Missing) => return validation(MSG_KEY_REQUIRED),
        Err(subscriptions::IdempotencyKeyError::TooLong { .. }) => {
            return validation(MSG_KEY_TOO_LONG)
        }
    };
    // 🔴 第二档上限：**同一个**键在 255 下合法、在 200 下超限（上游刻意更短）。
    if key.len() > MAX_SEAT_PURCHASE_IDEMPOTENCY_KEY_LENGTH {
        return validation(MSG_SEAT_KEY_TOO_LONG);
    }
    // 上游逐字：币种**小写化**、幂等键换成解析出来的那个，再序列化。
    input.currency = input.currency.to_lowercase();
    input.idempotency_key = Some(key.clone());
    proxy(
        &state,
        subscriptions::seat_purchase_request(
            workspace_id,
            user.id(),
            request_id.as_deref(),
            subscriptions::seat_purchase_body(&input),
            &key,
        ),
    )
    .await
}

/// `POST /api/cloud-subscriptions/seats/reconcile`
/// （上游 `ReconcileCloudWorkspaceSubscriptionSeats`）：**只是提示**，本地不给座位数。
async fn reconcile_seats(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    _actor: HumanActor,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
) -> Response {
    let workspace_id = match manager_scope(&state, &headers, &query, user).await {
        Ok(workspace_id) => workspace_id,
        Err(response) => return response,
    };
    // 上游这一条**不读体、不要求幂等键、也不转发它**（`cloud_billing.go:254` 传 `nil`）。
    proxy(
        &state,
        subscriptions::seats_reconcile_request(
            workspace_id,
            user.id(),
            request_id(&headers).as_deref(),
        ),
    )
    .await
}

/// `POST /api/cloud-subscriptions/portal-sessions`
/// （上游 `CreateCloudWorkspaceSubscriptionPortal`）：幂等键**必需**，体不转发。
async fn create_portal_session(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    _actor: HumanActor,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
) -> Response {
    let workspace_id = match manager_scope(&state, &headers, &query, user).await {
        Ok(workspace_id) => workspace_id,
        Err(response) => return response,
    };
    // 上游这一条不读体（`withBody` 没打开）⇒ 体键无从谈起，只有请求头那一路。
    // `requireCloudSubscriptionIdempotencyKey(w, r, "")` 在这里**只做校验** —— 转发的那一头
    // 由 `cloudSubscriptionIdempotencyHeaders(r)` 单独产出（两者对头那一路同值，见配对的
    // 单元用例 `the_two_key_helpers_agree_on_the_header_path`）。
    let header_key = header_key(&headers);
    if let Err(error) = subscriptions::resolve_idempotency_key(
        None,
        header_key.as_deref(),
        MAX_IDEMPOTENCY_KEY_LENGTH,
    ) {
        return match error {
            subscriptions::IdempotencyKeyError::Missing => validation(MSG_KEY_REQUIRED),
            subscriptions::IdempotencyKeyError::TooLong { .. } => validation(MSG_KEY_TOO_LONG),
        };
    }
    let forwarded = subscriptions::forwarded_idempotency_key(header_key.as_deref());
    proxy(
        &state,
        subscriptions::portal_session_request(
            workspace_id,
            user.id(),
            request_id(&headers).as_deref(),
            forwarded.as_deref(),
        ),
    )
    .await
}

// ---------------------------------------------------------------------------
// 两条纯函数（座位购买的校验面）
// ---------------------------------------------------------------------------

/// 上游座位购买的那一串校验（`in.AdditionalSeats < 1 || in.ExpectedCurrentSeats < 1 ||
/// in.ExpectedPurchaseVersion < 1 || in.AcceptedProrationAmount < 0 || !isASCIICurrency(...)`）。
///
/// 提成纯函数是为了让"三件套各自为 0/负数"的每一格都能被单独钉住（`DoD` 第 4 条）。
#[must_use]
pub fn is_valid_seat_purchase(input: &CloudSubscriptionSeatPurchaseRequest) -> bool {
    input.additional_seats >= 1
        && input.expected_current_seats >= 1
        && input.expected_purchase_version >= 1
        && input.accepted_proration_amount >= 0
        && is_ascii_currency(&input.currency)
}

/// 上游 `isASCIICurrency`：**恰好 3 个** ASCII 字母（大小写都算；`usd` / `USD` 都合法，
/// 小写化发生在校验**之后**）。
#[must_use]
pub fn is_ascii_currency(value: &str) -> bool {
    value.len() == 3 && value.bytes().all(|byte| byte.is_ascii_alphabetic())
}

/// checkout 的付款人邮箱（上游 `h.Queries.GetUser` → `user.email` 的 `TrimSpace`）。
///
/// **没有** `X-Multica-User-Email` 覆盖口（与 `routes/invitations.rs` 的那份**有意**不同）：
/// 付款人身份必须是服务端解析的（上游逐字：「A caller cannot smuggle … Stripe payer
/// identity through JSON」）—— 见模块头的偏离登记。
#[allow(clippy::result_large_err)] // `Err` 就是要原样返回的响应（同上面两个 scope）。
async fn payer_email(state: &AppState, user_id: Id) -> Result<String, Response> {
    let row: Result<Option<(String,)>, sqlx::Error> =
        sqlx::query_as(r#"SELECT email FROM "user" WHERE id = $1"#)
            .bind(user_id.0)
            .fetch_optional(state.db.pool())
            .await;
    match row {
        // 查得到但（trim 后）为空 ⇒ 上游的 "checkout payer email is unavailable"。
        Ok(Some((email,))) => {
            let trimmed = email.trim();
            if trimmed.is_empty() {
                return Err(error_with_code(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    CODE_INTERNAL,
                    MSG_PAYER_EMAIL_UNAVAILABLE,
                ));
            }
            Ok(trimmed.to_string())
        }
        // 没有这一行 / 查不动 ⇒ 上游同一条 "failed to resolve checkout payer"
        //（上游把 `sql.ErrNoRows` 与查询错误合并在那一支）。
        Ok(None) | Err(_) => Err(error_with_code(
            StatusCode::INTERNAL_SERVER_ERROR,
            CODE_INTERNAL,
            MSG_PAYER_FAILED,
        )),
    }
}

// ---------------------------------------------------------------------------
// 出站与响应的公共骨架（与 `routes/cloud/billing.rs` 同形的本地副本）
// ---------------------------------------------------------------------------

/// 一次出站代理（上游 `proxyCloudSubscription`）。
async fn proxy(state: &AppState, request: CloudRequest) -> Response {
    let Some(client) = state.cloud.client() else {
        // 上游把「没配」与「配了但非法」分成两条（403 / 500）。
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

/// 云侧响应逐字写回（上游 `writeCloudRuntimeResponse` 的本地形态，与 M9-1 的逐字同款）。
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

/// 传输层失败 → 本地错误（`docs/62` §2.6 的五支，**静态** message）。
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

/// 本仓标准的**嵌套**错误信封（`{"error":{"code":…,"message":…}}`）。
fn error_with_code(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(ErrorBody {
            error: ErrorResponse::new(code, message),
        }),
    )
        .into_response()
}

/// 400 `validation_error`（本文件所有本地校验的**唯一**出口，文本由调用方给）。
fn validation(message: &str) -> Response {
    error_with_code(StatusCode::BAD_REQUEST, CODE_VALIDATION, message)
}

#[derive(Serialize)]
struct ErrorBody {
    error: ErrorResponse,
}

/// 出站 `X-Request-ID`：只透传调用方给的那个（上游 `cloudRuntimeRequestID` 的第一支）。
fn request_id(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

/// 请求头里的 `Idempotency-Key`（缺失 / 非 ASCII ⇒ `None` ⇒ 按"没给"处理）。
fn header_key(headers: &HeaderMap) -> Option<String> {
    headers
        .get(IDEMPOTENCY_KEY_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

/// 读并校验转发体（上游 `readCloudRuntimeJSONBody`）：413（>1 MiB）/ 400（空体、非法 JSON）。
#[allow(clippy::result_large_err)] // 同上：`Err` 就是要原样返回的响应。
async fn read_cloud_runtime_json_body(request: Request) -> Result<Vec<u8>, Response> {
    let limit = MAX_CLOUD_RUNTIME_REQUEST_BODY_SIZE;
    let Ok(bytes) = axum::body::to_bytes(request.into_body(), limit + 1).await else {
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
        return Err(validation(MSG_BODY_REQUIRED));
    }
    if serde_json::from_slice::<serde_json::Value>(&bytes).is_err() {
        return Err(validation(MSG_BODY_INVALID));
    }
    Ok(bytes.to_vec())
}

#[cfg(test)]
mod tests;
