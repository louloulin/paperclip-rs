//! `GET /api/integrations/composio/callback`（`router.go:1517`，**公开块**）—— 写者 **M8-6**
//! （`LUM-1803`）。
//!
//! | 注册键 | 方法 | 授权层 | 未配置 | 上游 |
//! | --- | :-: | --- | --- | --- |
//! | `/api/integrations/composio/callback` | GET | **公开块**（凭 `state` HMAC 取身份） | **先按 state 判**（见下表） | `ComposioCallback`（`integrations_composio.go:112`） |
//!
//! # 权限模型（上游 `router.go:1512-1519` 的注释 + `MUL-3843`）
//!
//! Composio 在托管授权流程结束时把用户**浏览器 302** 回来，此时 cookie 会话**经常不在**
//! （会话过期 / `SameSite=Strict` 或 Safari ITP 跨站剥离 / 隐私窗口 / 自建回调落在另一个
//! 子域）。所以这条路由**必须**在 Auth 组**之外**：身份**只**来自 HMAC 签名的 `state`
//! 查询参数。
//!
//! ⚠️ **不得挂会话 middleware，也不得因为缺 cookie 就 401**（`docs/61` §2.7 第 4 条）。
//! 这正是 ⑨ 唯一那条 M8 fixture 的断言语义：
//!
//! ```text
//! integrations/TestComposioCallbackIsPublic_NoCookieNot401@server/cmd/server/composio_callback_public_test.go:25#1
//!   GET /api/integrations/composio/callback（匿名，query = state=bogus&status=success&connected_account_id=ca_x）
//!   status_expected = 401   —— 断言语义：匿名 + 错 state ⇒ **401**（不是 404，也不是「缺 cookie 就 401」）
//! ```
//!
//! ⇒ 本文件**先验 state**（它是唯一身份来源），再谈「未配置」。顺序是承重的：`state` 不合法
//! 时连「本部署配没配 composio」都不该被回显（下游看不到任何配置信息），而**合法 state 才**
//! 需要部署能力。
//!
//! # 四种状态码（逐条可测）
//!
//! | 状态 | 何时 | 上游对应 |
//! | --- | --- | --- |
//! | **401** `composio_state_invalid` | state 篡改 / 过期 / 重放 / 形态非法（四类**不**外传，只进日志） | 上游 `CompleteCallback` 的非 nil error（上游回 302，本仓回 401 —— 见下） |
//! | **403** `composio_not_configured` | state **合法**但本次部署未装配（flag 关 / 缺 key / 缺回调基址） | `writeFeatureDisabled` ⇒ 403 |
//! | **302**（Location = 设置页） | state 合法 + 已装配：成功 `&connected=<slug>`、失败 `&error=composio_connect_failed` | `CallbackRedirect` |
//! | 其它 | —— | —— |
//!
//! ⚠️ **两处登记在 `docs/32` §9.12 的偏离**：
//!
//! 1. **401 而不是 302**：上游把「state 不合法」也折成失败重定向（302）。本仓按 `docs/61`
//!    §1.5 / §6.5 的**本地契约**回 401 —— 这也是 ⑨ 那条夹具要的状态码（夹具读的是上游在
//!    「测试环境没配 `COMPOSIO_API_KEY`」下的行为；本片把**两种环境**的语义都做对：没配 key
//!    ⇒ state 必然验不过 ⇒ 401，配了 key ⇒ 坏 state 也是 401，两种情况**逐字一致**）；
//! 2. **验 state 在「未配置」判定之前**：上游先查 `h.Composio == nil || !flag`（403）、再进
//!    handler。本仓反序（先 state、后配置），因为公开回调的身份来源就是 state，而「未配置」
//!    这一格在下游看来不该泄漏部署状态（`docs/61` §6.5 的 M8-6 行逐字：「**公开回调仍须按
//!    state 判**」）。
//!
//! # 为什么失败也 302（除了上面两格）
//!
//! 上游注释逐字：any failure redirects to the same page with a stable error code so the user
//! is never left on a blank API response。浏览器被 Composio 带回来时，回一个 JSON 错误会把人
//! 卡住 ⇒ 与 M8-1 的 `GitHubSetupCallback` 同一取舍（那次也是「计划书写 400/401，照上游回 302」）。

use std::collections::HashMap;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use mc_composio::service::{CallbackOutcome, ComposioError};

use crate::routes::composio::connect::{
    error_with_code, not_configured, service_from, CODE_STATE_INVALID,
};
use crate::state::AppState;

/// 上游 `appURLFromEnv()` 的缺省（空 ⇒ 站内相对路径）。
pub const APP_URL_ENV: &str = "MULTICA_APP_URL";
/// 上游 `appURLFromEnv()` 的回落变量。
pub const FRONTEND_ORIGIN_ENV: &str = "FRONTEND_ORIGIN";

/// 本文件的路由切片。
pub fn router() -> Router<std::sync::Arc<AppState>> {
    Router::new().route("/api/integrations/composio/callback", get(callback))
}

/// 上游 `ComposioCallback`（`integrations_composio.go:112`）。
///
/// **公开**：签名里**没有** `AuthUser` 提取器（缺会话也必须能走完 —— 上游把这条路由放在
/// Auth 组之外就是为了这个）。
async fn callback(
    State(state): State<std::sync::Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let service = service_from(&state);
    let state_param = query.get("state").map_or("", String::as_str);
    let status = query.get("status").map_or("", String::as_str);
    let connected_account_id = query.get("connected_account_id").map_or("", String::as_str);

    let frontend = app_url_from_env();
    match service
        .complete_callback(state_param, status, connected_account_id)
        .await
    {
        CallbackOutcome::StateRejected(reason) => {
            // 四类原因**不**外传（只进日志）：上游注释逐字 —— we never tell the browser which
            // check failed。对外的 body 只有稳定 code。
            tracing::info!(%reason, "composio: callback state rejected");
            error_with_code(
                StatusCode::UNAUTHORIZED,
                CODE_STATE_INVALID,
                "invalid composio state",
            )
        }
        CallbackOutcome::NotConfigured { toolkit_slug } => {
            // state 合法 ⇒ 身份已确认，这时「本部署没打开这个功能」回 403 是诚实的
            // （上游 `writeFeatureDisabled` 的语义），且复用 4 条会话路由的同一格。
            tracing::warn!(%toolkit_slug, "composio: callback on an unconfigured deployment");
            not_configured()
        }
        CallbackOutcome::Rejected {
            toolkit_slug,
            reason,
        } => {
            // 上游故障（不可达 / 401 / 5xx）单独记一条，别的失败只记 info。
            if is_upstream_failure(&reason) {
                tracing::warn!(%reason, "composio: callback failed upstream");
            } else {
                tracing::info!(%reason, "composio: callback rejected");
            }
            redirect(&callback_redirect_location(&frontend, &toolkit_slug, false))
        }
        CallbackOutcome::Connected { toolkit_slug } => {
            redirect(&callback_redirect_location(&frontend, &toolkit_slug, true))
        }
    }
}

/// 「上游不可达 / 拒了我们」这一类（用来挑日志级别；**不**改对外状态码）。
fn is_upstream_failure(reason: &ComposioError) -> bool {
    matches!(
        reason,
        ComposioError::Upstream { .. }
            | ComposioError::Transport(_)
            | ComposioError::Unauthorized
            | ComposioError::Store(_)
    )
}

/// 上游 `Service.CallbackRedirect` + `appURLFromEnv()`。
///
/// 上游逐字：成功 ⇒ `/settings?tab=integrations&connected=<slug>`；失败 ⇒
/// `/settings?tab=integrations&error=composio_connect_failed`；基址为空 ⇒ **站内相对路径**
/// （上游 `s.frontendURL + path` 在空 base 下就是这个形态）。
///
/// ⚠️ 上游用的是 slug-less 的 `/settings?tab=integrations`（注释逐字：the web proxy's
/// legacy-route redirect prepends the user's last workspace slug）——本仓没有那层 web 代理，
/// 照上游的**字面量**给出同一条 path。
fn callback_redirect_location(frontend: &str, toolkit_slug: &str, success: bool) -> String {
    let path = if success {
        format!(
            "/settings?tab=integrations&connected={}",
            percent_encode(toolkit_slug)
        )
    } else {
        "/settings?tab=integrations&error=composio_connect_failed".to_string()
    };
    format!("{}{path}", frontend.trim_end_matches('/'))
}

/// 上游 `appURLFromEnv()`：`MULTICA_APP_URL` → `FRONTEND_ORIGIN` → **空**（⇒ 相对路径）。
///
/// ⚠️ 这是本文件**唯一**读 env 的地方，且**不是密钥**（与 M8-1 的 `frontend_origin_from_env`
/// 同一处置：部署密钥的唯一读取口是 `AppState`）。
pub fn app_url_from_env() -> String {
    app_url_from(|name| std::env::var(name).ok())
}

/// `appURLFromEnv()` 的纯函数形态（测试注入，不碰进程 env）。
pub fn app_url_from<F>(get: F) -> String
where
    F: Fn(&str) -> Option<String>,
{
    for name in [APP_URL_ENV, FRONTEND_ORIGIN_ENV] {
        let value = get(name)
            .map(|raw| raw.trim().trim_end_matches('/').to_string())
            .filter(|normalized| !normalized.is_empty());
        if let Some(value) = value {
            return value;
        }
    }
    String::new()
}

/// 302（上游 `http.Redirect(..., http.StatusFound)`）。
///
/// ⚠️ 不能用 `axum::response::Redirect::to`：它发的是 **303**（See Other），而上游逐字是
/// **302 Found**（M8-1 的 `setup.rs` 踩过同一个坑）。
fn redirect(location: &str) -> Response {
    (
        StatusCode::FOUND,
        [(axum::http::header::LOCATION, location)],
    )
        .into_response()
}

/// query 值的百分号编码（上游 `url.QueryEscape` 的等价物；只编码必然要编码的字节）。
fn percent_encode(value: &str) -> String {
    use std::fmt::Write as _;

    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRONTEND: &str = "https://app.example.test";

    #[test]
    fn the_success_redirect_carries_the_toolkit_and_the_failure_one_a_stable_code() {
        assert_eq!(
            callback_redirect_location(FRONTEND, "notion", true),
            "https://app.example.test/settings?tab=integrations&connected=notion"
        );
        assert_eq!(
            callback_redirect_location(FRONTEND, "notion", false),
            "https://app.example.test/settings?tab=integrations&error=composio_connect_failed",
            "失败重定向**不**泄漏 slug"
        );
    }

    #[test]
    fn an_unset_frontend_base_yields_a_site_relative_path() {
        let location = callback_redirect_location("", "notion", true);
        assert_eq!(location, "/settings?tab=integrations&connected=notion");
        assert!(!location.contains("//"));
    }

    #[test]
    fn the_slug_is_percent_encoded() {
        assert_eq!(percent_encode("notion"), "notion");
        assert_eq!(percent_encode("a/b c"), "a%2Fb%20c");
        assert_eq!(
            callback_redirect_location(FRONTEND, "a b", true),
            "https://app.example.test/settings?tab=integrations&connected=a%20b"
        );
    }

    #[test]
    fn app_url_prefers_multica_app_url_then_frontend_origin() {
        let both = |name: &str| match name {
            "MULTICA_APP_URL" => Some("  https://app.example.test/  ".to_string()),
            "FRONTEND_ORIGIN" => Some("http://localhost:3000".to_string()),
            _ => None,
        };
        assert_eq!(app_url_from(both), "https://app.example.test");

        let only_frontend = |name: &str| match name {
            "FRONTEND_ORIGIN" => Some("http://localhost:3000/".to_string()),
            _ => None,
        };
        assert_eq!(app_url_from(only_frontend), "http://localhost:3000");

        // 空串 / 纯空白都算没配（上游 `strings.TrimSpace` + `!= ""`）。
        let blank = |_: &str| Some("   ".to_string());
        assert_eq!(app_url_from(blank), "");
        assert_eq!(app_url_from(|_| None), "");
    }

    #[test]
    fn upstream_failures_are_the_ones_we_log_loudly() {
        assert!(is_upstream_failure(&ComposioError::Transport("x".into())));
        assert!(is_upstream_failure(&ComposioError::Unauthorized));
        assert!(is_upstream_failure(&ComposioError::Upstream {
            status: 500,
            context: "c".into()
        }));
        assert!(is_upstream_failure(&ComposioError::Store("db".into())));
        assert!(!is_upstream_failure(&ComposioError::ConnectNotSuccessful));
        assert!(!is_upstream_failure(&ComposioError::AccountVerification));
        assert!(!is_upstream_failure(&ComposioError::Malformed("m".into())));
    }

    #[tokio::test]
    async fn the_redirect_is_a_302_with_a_location_header() {
        let response = redirect("/settings?tab=integrations&connected=notion");
        assert_eq!(
            response.status(),
            StatusCode::FOUND,
            "上游逐字 302（不是 303）"
        );
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::LOCATION)
                .and_then(|value| value.to_str().ok()),
            Some("/settings?tab=integrations&connected=notion")
        );
    }
}
