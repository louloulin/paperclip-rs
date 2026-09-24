//! `/v1` 的**中间件层**：凭据校验入口 + 限流档位。
//!
//! - **写者**：M6-7（`docs/57` §3.2）。
//! - **上游**：`internal/middleware/plugin_auth.go`（47 行）与 `plugin_ratelimit.go`（40 行），
//!   加上 `pkg/publicapi/v1/problem.go` 的 `WriteProblem`；凭据解析的口径在
//!   `internal/handler/plugin_action.go` 的 `pluginCaller` / `pluginTokenCaller` /
//!   `pluginSessionCaller`（本文件把它们收在一个地方，两侧挂载点共用同一份实现）。
//!
//! ## 为什么是一个 `apply`，而不是每个子文件自己挂层
//!
//! `router()` 拿不到 `Arc<AppState>`（state 由 `main.rs` 的 `with_state` 一次性注入），
//! 而 `tower_governor` 的限流桶**按 router 实例分片**：三个子 router 各挂一层 = 3 倍配额。
//! 所以层必须由 `v1/mod.rs` 在**合并点之后**加一次，签名保持**与状态无关的泛型**。
//!
//! 层的顺序：`.layer(限流).layer(凭据)` —— axum 的 `Router::layer` 每调一次就把已有 service
//! **再包一层**，所以**后加的在外层**：先出 401（凭据），再进限流桶。凭据不合格的请求不消耗配额，
//! 与上游「`PluginBearerOnly` 在 `PluginRateLimit` 之前」的路由组顺序一致。
//!
//! ## 凭据口径（4 种）
//!
//! | 凭据 | 取值 | 谁产生 | 落在哪 |
//! | --- | --- | --- | --- |
//! | 安装令牌 | `Authorization: Bearer mpi_…` | 插件 | `plugin_installation.token_hash`（只存哈希） |
//! | 回调令牌 | `Authorization: Bearer mpc_…` | 宿主（hook 派发时） | **进程内**（[`callback_tokens`]），5 分钟可重复调用 |
//! | 用户会话 | `X-Multica-User-Id` + `X-Multica-Plugin-Installation` | 浏览器 | 无 |
//! | 无凭据 | —— | surface 页面 | 无（`routes/surfaces.rs`，路径 token 即凭据） |
//!
//! 校验逻辑**只有一份**（`mc_plugin_host::token`）；本层负责「取出来、判一判、放进 caller」。
//! `/v1` 面的 9 条请求由 [`require_plugin_bearer`] 保证**必须**是插件凭据（`mpi_`/`mpc_`），
//! 桥面（`routes/plugin_bridge/*`）不套这一层 ⇒ 同一条 handler 在两个前缀下按**呈现出来的
//! 凭据**决定 actor（上游 `pluginCaller` 的同一个分支）。
//!
//! ## 本片登记在 `docs/32` §9 的偏离（三条）
//!
//! 1. **`plugins_v1` 的本地口径**：与 M6-5 同一处口径 —— **未登记 = 开启**，显式登记为
//!    `false` 才 403 `plugin_api_disabled`（同码同体）。理由同 M6-5：本仓
//!    `FeatureFlagCatalog` 没有持久化后端（全仓唯一的 `register` 调用点在测试里），照抄上游
//!    的默认值 `false` 会让 `/v1` 面**永远** 403 —— 而它正是本片的交付物。
//!    `contracts/golden/context/001`（`TestPluginActionRequiresTheFeatureFlag`）在上游是靠
//!    **测试里显式关掉开关**拿到 403 的（`withPluginsV1Flag(t, testHandler, false)`）⇒ 本仓
//!    以「显式登记 false ⇒ 403」逐字满足它，见 `tests/public_api/guard.rs` 的同名用例。
//! 2. **回调令牌表落在本文件**：`AppState`（M6-0 anchor 冻结）没有 `CallbackTokens` 字段，
//!    而两片都需要同一张进程内表（M6-8 的 hook 派发**签发**、本片**解析**）⇒ 唯一实现点
//!    由本文件提供 [`callback_tokens`]，M6-8 **必须**用它（不要再 new 一张）。
//! 3. **安装令牌的 `token_hash` 读口落在本文件**：`mc-repos` 的 `installation.rs` 是 M6-5 的
//!    写集（无此读口，且本片不得编辑它）⇒ 这里用 `installation::COLUMNS`（那个文件公开的列
//!    投影常量）直查一次 `token_hash`，列清单仍只有一份。
//!
//! 行预算（门 ⑩）：本文件 ≤520 行。

use std::sync::{Arc, OnceLock};

use axum::body::Body;
use axum::extract::Request;
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use sha2::{Digest, Sha256};
use tower_governor::governor::GovernorConfigBuilder;
use tower_governor::key_extractor::KeyExtractor;
use tower_governor::{GovernorError, GovernorLayer};

use mc_core::plugin::PluginTokenKind;
use mc_plugin_host::token::CallbackTokens;

/// Bearer 协议头的前缀（上游 `middleware.BearerToken` 的字面量；比较时大小写不敏感）。
const BEARER_PREFIX: &str = "Bearer ";

/// 上游 `featureflags.PluginsV1`。
pub(crate) const PLUGINS_V1: &str = "plugins_v1";

/// 上游 `RATE_LIMIT_PLUGIN_API` 的默认值（每分钟、每个凭据一个固定窗口）。
pub(crate) const RATE_LIMIT_PER_MINUTE: u32 = 120;

/// 限流窗口（秒）—— 也是 429 响应里 `Retry-After` 的取值（上游 `PluginRateLimit` 逐字）。
pub(crate) const RATE_LIMIT_WINDOW_SECS: u64 = 60;

/// 插件面回调令牌的进程内唯一实例（见文件头偏离 2）。
static CALLBACK_TOKENS: OnceLock<CallbackTokens> = OnceLock::new();

/// 进程内的回调令牌表：**M6-8 的 hook 派发用它签发**，本片用它解析。
///
/// 跨实例/重启后令牌提前失效（403、可重试）是**设计**（`docs/57` §2.4 第 10 条）。
pub(crate) fn callback_tokens() -> &'static CallbackTokens {
    CALLBACK_TOKENS.get_or_init(CallbackTokens::new)
}

/// 测试用的回调令牌表出口（`mc-http/test-util` 门控）。
///
/// 存在的理由：签发侧在 M6-8 的 hook 派发里，而本片的 e2e 必须能构造一枚**真的** `mpc_`
/// 令牌（否则「回调令牌可重复调用」这条 `DoD` 只能靠 mock，而 mock 掉的正是被测的那张表）。
/// 它返回的就是生产路径用的那**同一个** `&'static CallbackTokens`（见 [`callback_tokens`]）。
#[cfg(feature = "test-util")]
pub fn callback_tokens_for_test() -> &'static CallbackTokens {
    callback_tokens()
}

/// 插件面错误：状态码 + 稳定码 + 上游文案（上游 `service.PluginError` 的 `(Kind, Message)`）。
#[derive(Debug, Clone)]
pub(crate) struct ActionError {
    pub(crate) status: StatusCode,
    /// 稳定码（空串 ⇒ 按 `mc_openapi::v1::code_for_status` 推导）。
    pub(crate) code: &'static str,
    pub(crate) detail: String,
}

pub(crate) type ActionResult<T> = Result<T, ActionError>;

impl ActionError {
    pub(crate) fn new(status: StatusCode, code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            status,
            code,
            detail: detail.into(),
        }
    }

    /// 上游 `PluginErrorInvalid` ⇒ 400 `invalid_request`。
    pub(crate) fn invalid(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_request", detail)
    }

    /// 上游 `PluginErrorNotFound` ⇒ 404 `not_found`。
    pub(crate) fn not_found(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", detail)
    }

    /// 上游 `PluginErrorForbidden` ⇒ 403 `forbidden`。
    pub(crate) fn forbidden(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", detail)
    }

    /// 上游 `PluginErrorConflict` ⇒ 409 `conflict`。
    pub(crate) fn conflict(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "conflict", detail)
    }

    /// 上游 `PluginErrorQuota` ⇒ 507 `quota_exceeded`。
    pub(crate) fn quota(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::INSUFFICIENT_STORAGE, "quota_exceeded", detail)
    }

    /// 上游 `PluginErrorUnavailable` ⇒ 502 `upstream_unavailable`。
    pub(crate) fn unavailable(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_GATEWAY, "upstream_unavailable", detail)
    }

    /// 用**请求的** request id 渲染（handler 的统一出口）。
    pub(crate) fn into_response_for(self, request_id: &str) -> Response {
        problem_response(
            self.status,
            self.code,
            &self.detail,
            request_id,
            HeaderMap::new(),
        )
    }
}

impl From<mc_errors::Error> for ActionError {
    fn from(error: mc_errors::Error) -> Self {
        Self {
            status: StatusCode::from_u16(error.http_status())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            code: match error.http_status() {
                400 => "invalid_request",
                401 => "unauthorized",
                403 => "forbidden",
                404 => "not_found",
                409 => "conflict",
                422 => "incompatible",
                _ => "internal_error",
            },
            detail: error.message(),
        }
    }
}

// ---------------------------------------------------------------------------
// 问题响应（上游 `publicapiv1.WriteProblem`）
// ---------------------------------------------------------------------------

/// 请求关联 id：优先回显进来的 `X-Request-Id`，否则新生成一个（上游 `requestID` 的兜底分支）。
pub(crate) fn request_id(headers: &HeaderMap) -> String {
    headers
        .get(mc_openapi::v1::HEADER_REQUEST_ID)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map_or_else(|| uuid::Uuid::new_v4().to_string(), str::to_string)
}

/// RFC 9457 风格的问题响应：体是 `mc_openapi::v1::ProblemDetail`，落 `application/problem+json`
/// 与 `X-Request-Id`（上游 `WriteProblem` 逐字）。
pub(crate) fn problem_response(
    status: StatusCode,
    code: &str,
    detail: &str,
    request_id: &str,
    extra_headers: HeaderMap,
) -> Response {
    let body = mc_openapi::v1::problem_detail(status.as_u16(), code, detail, request_id);
    let mut response = (status, Json(body)).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(mc_openapi::v1::PROBLEM_CONTENT_TYPE),
    );
    if let Ok(value) = HeaderValue::from_str(request_id) {
        headers.insert(HeaderName::from_static("x-request-id"), value);
    }
    for (name, value) in extra_headers {
        if let Some(name) = name {
            headers.insert(name, value);
        }
    }
    response
}

// ---------------------------------------------------------------------------
// 凭据：`PluginBearerOnly`
// ---------------------------------------------------------------------------

/// Authorization 头里的裸凭据（上游 `BearerToken`：`Bearer ` 大小写不敏感、去空白）。
pub(crate) fn bearer_token(headers: &HeaderMap) -> String {
    let Some(header) = headers.get(header::AUTHORIZATION) else {
        return String::new();
    };
    let Ok(header) = header.to_str() else {
        return String::new();
    };
    let header = header.trim();
    if header.len() <= BEARER_PREFIX.len()
        || !header[..BEARER_PREFIX.len()].eq_ignore_ascii_case(BEARER_PREFIX)
    {
        return String::new();
    }
    header[BEARER_PREFIX.len()..].trim().to_string()
}

/// 凭据是否属于**插件**令牌族（`mpi_` / `mpc_`）。
///
/// **只按前缀判**（上游注释逐字）：这只是「哪条代码路径看这枚令牌」，真正的校验在
/// [`resolve_caller`] 里 —— 但按前缀分流可以避免「插件令牌去 PAT 表里试一遍」。
pub(crate) fn is_plugin_bearer_token(token: &str) -> bool {
    let token = token.trim_start();
    token.starts_with(PluginTokenKind::Install.prefix())
        || token.starts_with(PluginTokenKind::Callback.prefix())
}

/// `plugin bearer token required` 的 401（上游 `PluginBearerOnly` 的响应）。
fn plugin_bearer_required(request_id: &str) -> Response {
    problem_response(
        StatusCode::UNAUTHORIZED,
        "plugin_bearer_required",
        "plugin bearer token required",
        request_id,
        HeaderMap::new(),
    )
}

/// 凭据层的中间件：不是 `mpi_`/`mpc_` ⇒ 401（上游 `PluginBearerOnly`）。
///
/// 只在 `/v1` 的合并点套一次（见文件头的「层顺序」段）。
async fn require_plugin_bearer(request: Request, next: Next) -> Response {
    if is_plugin_bearer_token(&bearer_token(request.headers())) {
        next.run(request).await
    } else {
        plugin_bearer_required(&request_id(request.headers()))
    }
}

// ---------------------------------------------------------------------------
// 限流：`PluginRateLimit`
// ---------------------------------------------------------------------------

/// 限流键 = `sha256(凭据)` 的 hex（上游 `PluginRateLimit` 逐字：密钥绝不进日志/运维数据）。
///
/// 顺带把「不是插件凭据」的请求挡在桶外（返回 401），这样即使误调 `apply` 的层顺序，
/// 行为也与 [`require_plugin_bearer`] 一致。
#[derive(Debug, Clone, Copy)]
pub(crate) struct PluginTokenKeyExtractor;

impl KeyExtractor for PluginTokenKeyExtractor {
    type Key = String;

    fn extract<T>(&self, request: &axum::http::Request<T>) -> Result<Self::Key, GovernorError> {
        let token = bearer_token(request.headers());
        if !is_plugin_bearer_token(&token) {
            return Err(GovernorError::Other {
                code: StatusCode::UNAUTHORIZED,
                msg: None,
                headers: None,
            });
        }
        let digest = Sha256::digest(token.as_bytes());
        Ok(hex::encode(digest))
    }
}

// 限流器配置（每分钟 `RATE_LIMIT_PER_MINUTE` 个单元、允许同量突发）内联在 `apply` 里，不抽成
// 函数：`tower_governor` 不公开它 `use` 进来的 `governor::middleware::NoOpMiddleware`，而
// `GovernorConfig` 的类型参数里有它 —— 抽成函数就必须命名一个本 crate 看不到的类型。
// GCRA 与上游的固定窗口不是同一种算法（本地没有 Redis，`docs/57` §2.5 把 `tower_governor`
// 定为唯一实现点）：持续速率与突发上限一致，差别只在窗口边界。

/// `tower_governor` 的错误 → 本契约的问题体（429 `rate_limited` / 401 `plugin_bearer_required`）。
///
/// 取引用而不是值：`error_handler` 只要求 `Fn(GovernorError) -> Response<Body>`，而这里只读字段
/// ⇒ 调用点用闭包转一下，免得把用不到的所有权做进签名（clippy `needless_pass_by_value`）。
fn governor_error_response(error: &GovernorError) -> Response<Body> {
    let request_id = uuid::Uuid::new_v4().to_string();
    match error {
        GovernorError::TooManyRequests { .. } => {
            let mut headers = HeaderMap::new();
            if let Ok(value) = HeaderValue::from_str(&RATE_LIMIT_WINDOW_SECS.to_string()) {
                headers.insert(header::RETRY_AFTER, value);
            }
            problem_response(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                "Plugin API rate limit exceeded",
                &request_id,
                headers,
            )
        }
        GovernorError::Other { code, .. } => {
            if *code == StatusCode::UNAUTHORIZED {
                plugin_bearer_required(&request_id)
            } else {
                problem_response(
                    *code,
                    "",
                    "plugin request refused",
                    &request_id,
                    HeaderMap::new(),
                )
            }
        }
        GovernorError::UnableToExtractKey => plugin_bearer_required(&request_id),
    }
}

/// 给 `/v1` 的合并 router 加一层（凭据 + 限流）。
///
/// 层顺序见文件头：凭据在外、限流在内。
pub fn apply<S>(router: Router<S>) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    // ⚠️ `per_millisecond` / `burst_size` 取 `&mut self` 并返回借用 ⇒ 链式调用会造就一个
    // 活不过语句的临时值（`E0716`）。分两步：先建 builder，再逐项设置。
    let mut builder = GovernorConfigBuilder::default().key_extractor(PluginTokenKeyExtractor);
    builder
        .per_millisecond(RATE_LIMIT_WINDOW_SECS * 1000 / u64::from(RATE_LIMIT_PER_MINUTE))
        .burst_size(RATE_LIMIT_PER_MINUTE);
    builder.error_handler(|error| governor_error_response(&error));
    let config = Arc::new(
        builder
            .finish()
            .expect("plugin rate limit config is never zero (period/burst are consts)"),
    );
    // ⚠️ 必须是 `route_layer` 而不是 `layer`：`Router::layer` 会**连 fallback 一起包**，
    // 于是「没匹配到路由」的请求（尾斜杠别名、纯 404）会被本层提前变成 401 —— 实测把
    // `tests/autopilots/*` 的三条「单形态路由必须 404」用例打红。`route_layer` 只包已声明的
    // 路由，未匹配的请求仍旧走 404（axum 文档对这个区别有专门一段）。
    router
        .route_layer(GovernorLayer { config })
        .route_layer(axum::middleware::from_fn(require_plugin_bearer))
}

/// 桥面（`/api/plugin-bridge/v1/*`）的信任边界层：**不认插件令牌**。
///
/// 上游把桥面挂在 `middleware.Auth(...)` 的组里（`router.go:1593-1601`），会话中继面与公开面
/// 因此是**两个互不越界的信任面**（`docs/57` §2.3）：即使两个主机名路由到同一个进程，会话也
/// 跨不过去，反之亦然。本仓的会话就是 `X-Multica-User-Id`（M1 dev-mode 的既有形态）。
///
/// 为什么这层必要：桥面的 handler 与 `/v1` **是同一批函数**，而 `resolve_caller` 按「呈现出来
/// 的凭据」分流 —— 没有这层，一个 `mpi_` 令牌打到 `/api/plugin-bridge/v1/...` 也会被接受，
/// 上游不会（它被 `PluginBearerOnly` 之外的会话中间件挡在任何 handler 之前）。
///
/// ⚠️ 本层**不加限流**：桥面的配额档与公开面不同（`docs/57` §2.3 的「会话中继面」一行），
/// 而 `tower_governor` 的桶按 router 实例分片 ⇒ 桥面的层必须落在桥面的合并点上。
/// 本仓 `routes/plugin_bridge/mod.rs`（M6-0 冻结）没有合并层的位置，且它损坏的判决是
/// 「M6-7 自己挂」⇒ 由 `routes/plugin_bridge/{context,issues,storage}.rs` 各自 `apply_bridge`；
/// 这三处每处各一个桶是**可接受**的（同一前缀下每个面各自计数，不会互相稀释配额），
/// 但桥面与公开面**不共享**配额桶 —— 登记在 `docs/32` §9。
fn bridge_layer<S>(router: Router<S>) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    // 同 [`apply`]：`route_layer`（而不是 `layer`）—— 桥面上未匹配的路径必须仍是 404。
    router.route_layer(axum::middleware::from_fn(refuse_plugin_tokens_on_bridge))
}

/// 桥面中间件：带插件令牌的请求一律 401（会话面只认会话）。
async fn refuse_plugin_tokens_on_bridge(request: Request, next: Next) -> Response {
    if is_plugin_bearer_token(&bearer_token(request.headers())) {
        problem_response(
            StatusCode::UNAUTHORIZED,
            "session_required",
            "the plugin bridge requires a signed-in session; plugin tokens belong to the public /v1 API",
            &request_id(request.headers()),
            HeaderMap::new(),
        )
    } else {
        next.run(request).await
    }
}

/// 给**桥面**的合并 router 加一层（会话信任边界）。见 [`bridge_layer`]。
pub fn apply_bridge<S>(router: Router<S>) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    bridge_layer(router)
}

mod caller;

// 只有这两样被两侧的 handler 共用（`require_plugins_v1` 在 `resolve_caller` 内部用，不外露）。
pub(crate) use caller::{resolve_caller, ActionActor, ActionCaller};

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    #[test]
    fn bearer_extraction_matches_upstream() {
        assert_eq!(
            bearer_token(&headers(&[("authorization", "Bearer mpi_abc")])),
            "mpi_abc"
        );
        // scheme 大小写不敏感。
        assert_eq!(
            bearer_token(&headers(&[("authorization", "bEaReR mpc_x")])),
            "mpc_x"
        );
        // 前后空白被裁掉。
        assert_eq!(
            bearer_token(&headers(&[("authorization", "  Bearer   mpi_x  ")])),
            "mpi_x"
        );
        // 没有头 / 空头 / 只有 scheme / 别的 scheme ⇒ 空串。
        assert!(bearer_token(&HeaderMap::new()).is_empty());
        assert!(bearer_token(&headers(&[("authorization", "")])).is_empty());
        assert!(bearer_token(&headers(&[("authorization", "Bearer ")])).is_empty());
        assert!(bearer_token(&headers(&[("authorization", "Token mpi_abc")])).is_empty());
        // 前缀判族只认两族（大小写敏感：令牌前缀是字面量）。
        assert!(is_plugin_bearer_token("mpi_x"));
        assert!(is_plugin_bearer_token("mpc_x"));
        assert!(!is_plugin_bearer_token("MPI_x"));
        assert!(!is_plugin_bearer_token("pat_x"));
        assert!(!is_plugin_bearer_token(""));
    }

    #[test]
    fn problem_response_is_the_v1_envelope() {
        let response = problem_response(
            StatusCode::FORBIDDEN,
            "plugin_api_disabled",
            "Plugin management is not enabled",
            "req-1",
            HeaderMap::new(),
        );
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            mc_openapi::v1::PROBLEM_CONTENT_TYPE
        );
        assert_eq!(response.headers().get("x-request-id").unwrap(), "req-1");
        // `Retry-After` 只在限流分支出现（上游测试显式断言 feature gate 不带它）。
        assert!(response.headers().get(header::RETRY_AFTER).is_none());
    }

    #[test]
    fn rate_limit_error_maps_to_429_with_retry_after() {
        let response = governor_error_response(&GovernorError::TooManyRequests {
            wait_time: 1,
            headers: None,
        });
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers().get(header::RETRY_AFTER).unwrap(), "60");
    }

    #[test]
    fn key_extractor_hashes_the_token_and_refuses_strangers() {
        let request = axum::http::Request::builder()
            .header(header::AUTHORIZATION, "Bearer mpi_secret-value")
            .body(())
            .unwrap();
        let key = PluginTokenKeyExtractor.extract(&request).unwrap();
        assert_eq!(key.len(), 64, "sha256 hex");
        assert!(!key.contains("secret-value"), "凭据明文绝不进限流键");

        let stranger = axum::http::Request::builder()
            .header(header::AUTHORIZATION, "Bearer pat_1")
            .body(())
            .unwrap();
        assert!(matches!(
            PluginTokenKeyExtractor.extract(&stranger),
            Err(GovernorError::Other {
                code: StatusCode::UNAUTHORIZED,
                ..
            })
        ));
        let anonymous = axum::http::Request::builder().body(()).unwrap();
        assert!(PluginTokenKeyExtractor.extract(&anonymous).is_err());
    }

    #[test]
    fn request_id_prefers_the_incoming_header() {
        assert_eq!(
            request_id(&headers(&[("x-request-id", "incoming")])),
            "incoming"
        );
        let generated = request_id(&headers(&[("x-request-id", "  ")]));
        assert_eq!(generated.len(), 36, "空/空白头 ⇒ 退化成新 uuid");
    }

    #[test]
    fn key_extractor_is_the_only_place_the_token_is_hashed_for_limiting() {
        // 同一枚令牌两次提取 ⇒ 同一个键（限流桶按凭据分片，不按请求）。
        let build = || {
            axum::http::Request::builder()
                .header(header::AUTHORIZATION, "Bearer mpi_same")
                .body(())
                .unwrap()
        };
        assert_eq!(
            PluginTokenKeyExtractor.extract(&build()).unwrap(),
            PluginTokenKeyExtractor.extract(&build()).unwrap()
        );
    }

    #[test]
    fn callback_tokens_are_a_single_process_wide_table() {
        // 偏离 2 的回归：两处调用拿到的是同一张表（M6-8 签发 / 本片解析）。
        assert!(std::ptr::eq(callback_tokens(), callback_tokens()));
    }
}
