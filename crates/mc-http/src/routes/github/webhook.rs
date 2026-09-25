//! `POST /api/webhooks/github`（`router.go:1490`，**公开块**）—— 写者 **M8-4**。
//!
//! 上游对应物是 `internal/handler/github.go` 的 L964–L1997（`HandleGitHubWebhook` +
//! `handleInstallationEvent` + `handlePullRequestEvent` + `resolveCloseIntentPolicy` +
//! `mirrorPullRequestForWorkspace` + `triggerPRRefreshFromCIEvent`）。
//!
//! - **凭据**：`GITHUB_WEBHOOK_SECRET` + `X-Hub-Signature-256`（HMAC-SHA256）。
//! - **三族事件**：`installation` / `pull_request` / `check_suite`（M8-4 的 `DoD`）。
//! - **幂等**：同一 webhook 重投 2 次只插 1 行 PR（靠 `github_pull_request` 的唯一键与
//!   `issue_pull_request` 的 `ON CONFLICT` upsert，本文件不额外去重）。
//! - ⚠️ 公开路由**不得**挂会话 middleware，也**不得**因为缺会话就返回 401
//!   （`docs/61` §2.7 第 4 条）：本文件不取 `AuthUser`，本子 router 也不套任何鉴权层。
//!
//! # 失败语义（逐条对齐上游，**不统一**）
//!
//! | 情形 | 状态码 | body |
//! | --- | :-: | --- |
//! | `GITHUB_WEBHOOK_SECRET` 缺失 / 为空 | **404** | `not found`（宁可整体拒收，也不把未配置当成「所有签名都有效」） |
//! | 验签失败 | **401** | `invalid signature` |
//! | body 读失败 | 400 | `read body failed` |
//! | body 超限 | **413** | `payload too large`（与上游 `LimitReader` 的静默截断刻意不同，登记 `docs/32` §18.2 的 D1，与 M8-2 的 `vcs/webhook.rs` 同判） |
//! | `ping` | **200** | `{"ok":"pong"}` |
//! | 其余（含未建模事件） | **202** | 空 body |
//!
//! 错误体是**扁平**的 `{"error":"…"}` + 尾随换行（上游这一族用 `writeError`，与
//! `routes/webhooks/autopilots.rs` / `routes/vcs/webhook.rs` 同一判断，不复用本仓标准的嵌套
//! envelope）。载荷解码失败**只**打 warn + 继续走 202（上游三个 handler 的第一段都是
//! `slog.Warn` + `return`，绝不把坏载荷变成 4xx —— 否则 GitHub 会把端点标成 failing 并停投）。
//!
//! # 文件布局（门 ⑩ 单文件 800 行硬上限）
//!
//! 本文件是**入口与装配**；三个事件族的处理各自一个子模块（`webhook.rs` 声明 `mod`，
//! 子文件落在 `routes/github/webhook/`，与 `routes/channels/slack.rs` + `channels/slack/store.rs`
//! 同一手法）：
//!
//! - `webhook/installations.rs`：`installation` 事件（卸载掉绑定 / 刷新账号元数据 / pending）
//! - `webhook/mirror.rs`：`pull_request` 事件（扇出 + 投递级关闭裁决 + 镜像 + 自动推进）
//! - `webhook/ci.rs`：三族 CI 事件（纯触发器 → 快照刷新入队）
//!
//! # 快照刷新端口：一个 anchor 缺口的**本地处置**（登记 `docs/32` §18.2 的 D2）
//!
//! 上游 `h.PRRefresh` 是 Handler 的字段；本仓 `AppState` **没有**对应字段
//! （`docs/61` §5 的 `state.rs` 行只加了 vcs/github/composio 三组密钥），而 `state.rs` 与
//! `mount.rs` 是 anchor 冻结文件 ⇒ 请求面拿不到端口。本片因此在**自己的**文件里落一个
//! 进程级注入槽 [`set_pr_refresh_port`]，默认值是 anchor 的
//! [`DisabledPrRefresh`]（**诚实退化**：未接线时 `enabled() == false`、入队是 no-op，绝不
//! 假装刷新已接上）。写法与纪律逐条复刻 M8-1 的 `GITHUB_API_BASE`（`routes/github/install.rs`）：
//! 包级可变状态、生产装配点将来改成 `AppState` 字段、测试用它注入**记录型**端口。
//!
//! # 广播留在 HTTP 层（与 M8-1 同判例）
//!
//! `mc-vcs-github` 的依赖边里没有 `mc-realtime`（anchor 冻结）⇒ 事件发布在这里做，形状与
//! M8-1 的 `installation_created_envelope` 同款。

use std::sync::{Arc, Mutex, PoisonError};

use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, FailedToBufferBody};
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use mc_vcs_github::payload::{GithubEventKind, InstallationEventPayload, PullRequestEventPayload};
use mc_vcs_github::port::{DisabledPrRefresh, SharedPrRefresh};
use mc_vcs_github::webhook::{event_kind_for_request, verify_webhook_signature};
use serde_json::json;

use crate::state::AppState;

mod ci;
mod installations;
mod mirror;

/// body 上限（上游 `io.ReadAll(io.LimitReader(r.Body, 10<<20))` 的 `10 MiB`）。
pub const GITHUB_WEBHOOK_MAX_BODY_BYTES: usize = 10 * 1024 * 1024;

/// 本文件的路由切片（**1 个新增注册键**：`POST /api/webhooks/github`，单形态）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/webhooks/github", post(handle_github_webhook))
        .layer(DefaultBodyLimit::max(GITHUB_WEBHOOK_MAX_BODY_BYTES))
}

// ---------------------------------------------------------------------------
// 快照刷新端口的装配点（见模块头「anchor 缺口的本地处置」）
// ---------------------------------------------------------------------------

/// 进程级注入槽：`None` ⇒ 用 [`DisabledPrRefresh`]（诚实退化）。
static PR_REFRESH_SLOT: Mutex<Option<SharedPrRefresh>> = Mutex::new(None);

/// 当前生效的快照刷新端口（未注入 ⇒ 未配置的空实现）。
pub fn pr_refresh_port() -> SharedPrRefresh {
    PR_REFRESH_SLOT
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
        .unwrap_or_else(|| Arc::new(DisabledPrRefresh))
}

/// 注入端口（**生产装配点**：`apps/mc-server` 的 `IntegrationHandles`；M8-5 接线后由
/// `main.rs` 在 `router()` 之前调用。当前只有测试调用它）。
pub fn set_pr_refresh_port(port: SharedPrRefresh) {
    *PR_REFRESH_SLOT
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(port);
}

/// 清掉注入（测试收尾）。
pub fn reset_pr_refresh_port() {
    *PR_REFRESH_SLOT
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = None;
}

// ---------------------------------------------------------------------------
// 入口
// ---------------------------------------------------------------------------

/// 上游 `HandleGitHubWebhook`（`github.go:1056`）。
async fn handle_github_webhook(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let body = match body {
        Ok(body) => body,
        Err(BytesRejection::FailedToBufferBody(FailedToBufferBody::LengthLimitError(_))) => {
            return flat_error(StatusCode::PAYLOAD_TOO_LARGE, "payload too large");
        }
        Err(rejection) => {
            tracing::debug!(error = %rejection, "github: failed to read webhook body");
            return flat_error(StatusCode::BAD_REQUEST, "read body failed");
        }
    };

    // ① 密钥缺失 ⇒ 404（上游的第一段：未配置的部署整体拒收，而不是「所有签名都有效」）。
    let Some(secret) = state
        .github_keys
        .webhook_secret
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    else {
        return flat_error(StatusCode::NOT_FOUND, "not found");
    };
    // ② 验签失败 ⇒ 401。
    if !verify_webhook_signature(secret, &headers, &body) {
        return flat_error(StatusCode::UNAUTHORIZED, "invalid signature");
    }

    match event_kind_for_request(&headers) {
        GithubEventKind::Ping => {
            return (StatusCode::OK, Json(json!({ "ok": "pong" }))).into_response();
        }
        GithubEventKind::Installation => {
            if let Ok(payload) = serde_json::from_slice::<InstallationEventPayload>(&body) {
                installations::handle_installation_event(&state, &payload).await;
            } else {
                tracing::warn!("github: bad installation payload");
            }
        }
        GithubEventKind::PullRequest => {
            if let Ok(payload) = serde_json::from_slice::<PullRequestEventPayload>(&body) {
                mirror::handle_pull_request_event(&state, &payload).await;
            } else {
                tracing::warn!("github: bad pull_request payload");
            }
        }
        // 三族 CI 事件是**纯触发器**（Plan C）：载荷从不用于展示，只用来定位要刷新的 PR。
        GithubEventKind::CheckSuite => ci::trigger_pr_refresh_from_ci_event(&state, &body).await,
        // 上游 `default:`：确认每一个事件（GitHub 才不会把端点标成 failing），但不动作。
        GithubEventKind::Other => {}
    }
    StatusCode::ACCEPTED.into_response()
}

/// 上游这一族的错误体：扁平 `{"error":"…"}` + 尾随换行（`writeError` 的形态）。
fn flat_error(status: StatusCode, message: &str) -> Response {
    let mut payload = json!({ "error": message }).to_string();
    payload.push('\n');
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    (status, headers, payload).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mc_vcs_github::port::{PrRefreshPort, PrRefreshRequest};

    /// 两个方法都不做事的端口：只用来验证注入槽的往返（`items_after_statements` 要求
    /// 类型项落在模块层，不能在测试函数体里）。
    struct Recording;

    impl PrRefreshPort for Recording {
        fn enabled(&self) -> bool {
            true
        }

        fn enqueue(&self, _request: PrRefreshRequest) {}

        fn maybe_enqueue_on_view(&self, _request: PrRefreshRequest) -> bool {
            true
        }
    }

    #[test]
    fn refresh_port_defaults_to_the_honest_disabled_implementation() {
        reset_pr_refresh_port();
        let port = pr_refresh_port();
        assert!(!port.enabled(), "未注入 ⇒ 未配置（绝不假装已接上）");

        set_pr_refresh_port(Arc::new(Recording));
        assert!(pr_refresh_port().enabled());
        reset_pr_refresh_port();
        assert!(!pr_refresh_port().enabled());
    }

    #[test]
    fn flat_error_shape_matches_write_error() {
        let response = flat_error(StatusCode::NOT_FOUND, "not found");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
    }

    #[test]
    fn body_limit_constant_matches_upstream_ten_mib() {
        assert_eq!(GITHUB_WEBHOOK_MAX_BODY_BYTES, 10 * 1024 * 1024);
    }
}
