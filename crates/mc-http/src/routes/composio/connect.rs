//! `POST /api/integrations/composio/connect/init`（`router.go:1866`）—— 写者 **M8-6**
//! （`LUM-1803`）。
//!
//! | 注册键 | 方法 | 授权层 | 未配置 | 上游 |
//! | --- | :-: | --- | --- | --- |
//! | `/api/integrations/composio/connect/init` | POST | Auth 组内（匿名 ⇒ **401**） | **403** `composio_not_configured` | `ComposioConnectInit`（`integrations_composio.go:67`） |
//!
//! # 本文件是 composio 面的**公共件**之家
//!
//! `routes/composio/mod.rs` 是 anchor 冻结的（M8-0），所以三个写者文件（本文件 / `catalog.rs` /
//! `callback.rs`）共用的四件东西落在**本文件**（都是本片的写集）：
//!
//! | 件 | 为什么共用 |
//! | --- | --- |
//! | [`service_from`] | 5 条路由都要装配同一个服务（四条件 + feature flag + `api_base` 注入面） |
//! | [`error_with_code`] | 本仓「各切片各自持有本地副本」的约定（见 `routes/agents.rs` 的注释） |
//! | [`not_configured`] | 「未配置」的**唯一**出口（403 + 稳定错误码） |
//! | [`set_composio_api_base`] | 离线替身的注入点（测试专用，形状照 M8-1 的 `set_github_api_base`） |
//!
//! # 顺序：先**鉴权**（401）再**未配置**（403）
//!
//! 上游把 4 条会话路由挂在 Auth 组**内**（middleware 先跑）⇒ 匿名请求根本到不了 handler，
//! 永远不会看到「未配置」那一格。本仓没有 per-route 的会话 middleware（M8-1 / M8-2 同款），
//! 所以本文件用 [`AuthUser`] 提取器把 middleware 的那一步**放在 handler 签名里**：
//! 提取器失败 ⇒ 401（`ApiError`），成功后才轮到 403 / 400 / 502。
//!
//! # ⚠️ 计划文档写 503，上游写 403 —— 本片照**上游**（登记在 `docs/32` §9.12）
//!
//! `docs/61` §2.5 的 composio 行写「四种「未配置」⇒ 503」。上游的**实现**是
//! `writeFeatureDisabled(...)`，其实现逐字是 `writeErrorCode(w, http.StatusForbidden, …)`
//! —— 上游 `integrations_composio.go` 的文件头也逐字写着：「A missing deployment capability
//! is a **non-retryable 403**, not a transient 503.」（`router.go` 的注释里残留的 503 是
//! 过期文案）。M8-1 的 `repositories` 与 M8-2 的 VCS 写面遇上同一个分歧、做了同一个选择
//! ⇒ 本片与它们一致，避免同一波里出现两种「未配置」状态码。
//!
//! ⚠️ 但**公开回调**不一样：它不看这一格（`docs/61` §6.5 的 M8-6 行：「公开回调仍须按 state
//! 判」）—— 见 `callback.rs`。

use std::sync::{Arc, Mutex, PoisonError};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use mc_composio::service::{ComposioConfig, ComposioError, ComposioService};
use mc_errors::{ErrorBody, ErrorResponse};
use mc_feature_flags::FeatureKey;
use mc_repos::composio::connection::ComposioConnectionRepo;
use serde::{Deserialize, Serialize};

use crate::routes::agents::bad_request;
use crate::routes::auth_user::AuthUser;
use crate::state::AppState;

/// `composio_mcp_apps` feature flag 的键（上游 `featureflags.ComposioMCPApps` 逐字）。
pub const COMPOSIO_MCP_APPS_FLAG: &str = "composio_mcp_apps";

/// 「未配置」的稳定错误码（上游 `writeFeatureDisabled(w, "composio_not_configured", …)` 逐字）。
pub const CODE_NOT_CONFIGURED: &str = "composio_not_configured";

/// 上游失败的稳定错误码（本仓 `upstream_error` 家族）。
pub const CODE_UPSTREAM: &str = "upstream_error";

/// state 不合法的稳定错误码（公开回调用；**不**区分四类原因，见 `callback.rs`）。
pub const CODE_STATE_INVALID: &str = "composio_state_invalid";

// ---------------------------------------------------------------------------
// 离线替身的注入点（`docs/61` §4.2：`pkg/composio` 的 API base 可注入）
// ---------------------------------------------------------------------------

/// 进程级的 API base 覆盖（**测试/替身唯一入口**；形状照 M8-1 的 `GITHUB_API_BASE`）。
///
/// 为什么是 `Mutex<Option<String>>` 而不是 `OnceLock`：一个测试二进制里既要跑「默认 base」
/// 的用例又要跑「注入 base」的用例，而 `OnceLock` 只允许写一次。
static COMPOSIO_API_BASE: Mutex<Option<String>> = Mutex::new(None);

/// 当前生效的 API base（没注入过 ⇒ `None` ⇒ [`mc_composio::client::DEFAULT_API_BASE`]）。
pub fn composio_api_base() -> Option<String> {
    COMPOSIO_API_BASE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// 测试注入 base（返回被替换掉的旧值）。生产代码**不得**调用。
pub fn set_composio_api_base(base: impl Into<String>) -> Option<String> {
    COMPOSIO_API_BASE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .replace(base.into())
}

/// 清掉注入（测试收尾）。
pub fn reset_composio_api_base() {
    *COMPOSIO_API_BASE
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = None;
}

// ---------------------------------------------------------------------------
// 装配
// ---------------------------------------------------------------------------

/// 按 `AppState` 的部署密钥 + feature flag 装配 composio 服务（**每次调用一件**）。
///
/// 服务**按请求**构造（与 M8-1 的 `GithubClient::new(github_api_base())` 同款）：
/// 它只持有「配置 + HTTP 连接池 + 签发器 + auth-config 目录」，没有跨请求的语义状态
/// （重放台账是 `mc-composio` 里的**进程级**静态 —— 服务按请求构造也能挡住重放，
/// 见 `mc_composio::state` 的文件头；代价是 auth-config 目录的缓存只在一次请求内生效，
/// 登记在 `docs/32` §9.12）。
pub(crate) fn service_from(state: &AppState) -> ComposioService {
    let keys = &state.composio_keys;
    let config = ComposioConfig {
        api_key: keys.api_key.clone(),
        state_secret: keys.state_secret.clone(),
        callback_base_url: keys.callback_base_url.clone(),
        feature_enabled: state
            .feature_flags
            .is_enabled(&FeatureKey::new(COMPOSIO_MCP_APPS_FLAG)),
        api_base: composio_api_base(),
        state_ttl_secs: None,
    };
    ComposioService::new(config).with_store(ComposioConnectionRepo::new(state.db.clone()))
}

// ---------------------------------------------------------------------------
// 响应 helper
// ---------------------------------------------------------------------------

/// 本仓标准的**嵌套**错误信封 + 上游的稳定 `code`
/// （`{"error":{"code":…,"message":…}}`；形状与 `mc_errors` 一致，`code` 用上游字面量）。
///
/// 与 `routes/{github,vcs}::error_with_code` 同形 —— 本仓既有约定是「各切片各自持有本地副本」，
/// 所以这里再写一份而不是去改别人的冻结文件。
pub(crate) fn error_with_code(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(ErrorBody {
            error: ErrorResponse::new(code, message),
        }),
    )
        .into_response()
}

/// 「未配置」：**403** + `composio_not_configured`（上游 `writeFeatureDisabled` 逐字）。
///
/// 四种条件（缺 `COMPOSIO_API_KEY` / flag 关 / 缺 state secret / 缺回调基址）**共用**这一条
/// 响应 —— 上游也是共用的（`h.Composio == nil || !flag` 一个分支），诊断信息只在服务侧
/// （`ComposioConfig::missing()`）与日志里。**四种语义逐条可测**由测试逐条件断言。
pub(crate) fn not_configured() -> Response {
    error_with_code(
        StatusCode::FORBIDDEN,
        CODE_NOT_CONFIGURED,
        "composio integration not configured",
    )
}

/// 本仓标准错误 → 响应（`mc_errors::Error` 只经 [`crate::error::ApiError`] 才会变成 HTTP 响应）。
pub(crate) fn error_response(error: mc_errors::Error) -> Response {
    crate::error::ApiError(error).into_response()
}

/// 上游失败：**502** + `upstream_error`（上游 `writeError(w, http.StatusBadGateway, …)`）。
pub(crate) fn upstream_error(message: &str) -> Response {
    error_with_code(StatusCode::BAD_GATEWAY, CODE_UPSTREAM, message)
}

// ---------------------------------------------------------------------------
// 路由
// ---------------------------------------------------------------------------

/// 本文件的路由切片。
pub fn router() -> Router<Arc<AppState>> {
    Router::new().route(
        "/api/integrations/composio/connect/init",
        post(connect_init),
    )
}

/// `POST /connect/init` 的 body（上游 `ComposioConnectInitRequest` 逐字）。
#[derive(Debug, Deserialize)]
struct ConnectInitRequest {
    toolkit_slug: Option<String>,
}

/// `POST /connect/init` 的响应（上游 `ComposioConnectInitResponse` 逐字）。
#[derive(Debug, Serialize)]
struct ConnectInitResponse {
    redirect_url: String,
}

/// 上游 `ComposioConnectInit`（`integrations_composio.go:67`）。
///
/// 顺序：**鉴权 401**（提取器）→ 未配置 403 → body/参数 400 → 上游 502。
async fn connect_init(State(state): State<Arc<AppState>>, user: AuthUser, body: Bytes) -> Response {
    let service = service_from(&state);
    if !service.enabled() {
        return not_configured();
    }
    let request: ConnectInitRequest = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(_) => return error_response(bad_request("invalid request body")),
    };
    let slug = request.toolkit_slug.unwrap_or_default();
    if slug.trim().is_empty() {
        return error_response(bad_request("toolkit_slug is required"));
    }

    match service.begin_connect(user.id(), &slug).await {
        Ok(redirect_url) => Json(ConnectInitResponse { redirect_url }).into_response(),
        Err(ComposioError::ToolkitNotSupported) => {
            error_response(bad_request("toolkit not supported"))
        }
        Err(error) => {
            // 错误值**不含**凭据 / 响应体（`ComposioError` 的每个变体只带原因或状态码）。
            tracing::warn!(%error, "composio: connect init failed");
            upstream_error("failed to start composio connect")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_flag_key_and_error_codes_match_upstream_literals() {
        assert_eq!(COMPOSIO_MCP_APPS_FLAG, "composio_mcp_apps");
        assert_eq!(CODE_NOT_CONFIGURED, "composio_not_configured");
        assert_eq!(CODE_UPSTREAM, "upstream_error");
        assert_eq!(CODE_STATE_INVALID, "composio_state_invalid");
    }

    #[test]
    fn the_api_base_slot_round_trips_and_defaults_to_none() {
        reset_composio_api_base();
        assert_eq!(composio_api_base(), None);
        assert_eq!(set_composio_api_base("http://127.0.0.1:9"), None);
        assert_eq!(composio_api_base().as_deref(), Some("http://127.0.0.1:9"));
        assert_eq!(
            set_composio_api_base("http://127.0.0.1:10").as_deref(),
            Some("http://127.0.0.1:9"),
            "替换时把旧值交回调用者"
        );
        reset_composio_api_base();
        assert_eq!(composio_api_base(), None);
    }

    #[test]
    fn connect_init_request_tolerates_a_missing_toolkit() {
        let request: ConnectInitRequest = serde_json::from_str("{}").expect("empty body");
        assert_eq!(request.toolkit_slug, None);
        let request: ConnectInitRequest =
            serde_json::from_str(r#"{"toolkit_slug":"notion"}"#).expect("body");
        assert_eq!(request.toolkit_slug.as_deref(), Some("notion"));
        assert!(serde_json::from_str::<ConnectInitRequest>("not json").is_err());
    }

    #[tokio::test]
    async fn the_error_envelope_is_nested_and_carries_the_stable_code() {
        for (response, want_status, want_code) in [
            (not_configured(), StatusCode::FORBIDDEN, CODE_NOT_CONFIGURED),
            (
                upstream_error("failed to start composio connect"),
                StatusCode::BAD_GATEWAY,
                CODE_UPSTREAM,
            ),
            (
                error_with_code(
                    StatusCode::UNAUTHORIZED,
                    CODE_STATE_INVALID,
                    "invalid composio state",
                ),
                StatusCode::UNAUTHORIZED,
                CODE_STATE_INVALID,
            ),
        ] {
            assert_eq!(response.status(), want_status);
            let bytes = axum::body::to_bytes(response.into_body(), 65536)
                .await
                .expect("body");
            let value: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
            assert_eq!(value["error"]["code"], serde_json::json!(want_code));
            assert!(value["error"]["message"].is_string());
            assert_eq!(
                value
                    .as_object()
                    .expect("object")
                    .keys()
                    .collect::<Vec<_>>(),
                vec!["error"],
                "信封只有一层顶层键"
            );
        }
    }
}
