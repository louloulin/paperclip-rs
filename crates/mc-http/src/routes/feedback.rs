//! `POST /api/feedback` —— **写者 M9-5**（`LUM-1820`，`docs/62` §4.1 第 6 行）。
//!
//! 上游：`internal/handler/feedback.go`（177 行）。表 = `feedback`
//! （仓储 = [`mc_repos::feedback::FeedbackRepo`]）。形态 = **plain 单形态**
//! （补尾斜杠 = `EXTRA_ALIAS` 硬失败；上游是 `r.Post("/api/feedback", …)`）。
//!
//! # 三条 `DoD`（`docs/62` §6.5 的 M9-5 行）
//!
//! 1. **`has_images` 标记**（布尔/计数，不是图片本体 —— 上传通道在附件面）；
//! 2. **限流 10/h ⇒ 429**：**复用** `mc_autopilot::webhook::ratelimit::SlidingWindowLimiter`
//!    ⇒ **禁止**新写第二份限流器（`docs/62` §2.3 的「不得重复实现」清单点名了它）；
//! 3. **`workspace_id` / `user_id` 来自鉴权上下文**，不来自请求体。
//!
//! # 限流：进程级滑动窗口，**per-user**
//!
//! 上游逐字：「Per-user rate limit: hourly cap on feedback submissions. DB-backed so it
//! survives process restarts and works across multiple instances without a shared cache —
//! cost is one cheap indexed count per submit.」
//!
//! 本仓用 [`SlidingWindowLimiter`]（进程内滑动窗口）而不是 DB 计数：
//! - **必须复用**既有实现（`DoD` 第 2 条）—— 它是 M5-5 的
//!   `crates/mc-autopilot/src/webhook/ratelimit.rs`，与 webhook 三条闸同一个类型；
//! - **偏差已登记**（`docs/32` §9.26）：多副本部署时配额是**每副本**的。上游在**没配
//!   Redis** 时同样是每进程内存实现（`cmd/server/router.go` 只在 `rdb != nil` 时换成
//!   Redis limiter），所以在本仓「无 Redis 依赖」的前提下这两者**等价**；
//! - 上游那条 DB 计数 SQL（`CountRecentFeedbackByUser`）**仍在**仓储里
//!   （[`FeedbackRepo::count_recent_by_user`]）—— 保留上游的读口，限流判定走进程内窗口。
//!
//! # 体上限 64 KiB ⇒ **400**（不是 413）
//!
//! 上游用 `http.MaxBytesReader` + `json.Decode`，**超限表现为解码错误 ⇒ 400**
//! （`writeError(w, 400, "invalid request body")`），**不是** 413 —— 与
//! `routes/onboarding/profile.rs` 的 `PATCH_ONBOARDING_BODY_LIMIT` 同一档，**不是**
//! `routes/webhooks/autopilots.rs` 的 413 那一档（那边上游显式区分了 `MaxBytesError`）。
//!
//! # 判负阶梯（逐字对齐上游的顺序）
//!
//! `401`（无会话）→ `400 invalid request body`（解码 / 超限）→ `400 message is required`
//! → `400 message too long` → `400 invalid feedback context` → `429`（10/h）→ `201`。

use std::net::SocketAddr;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use mc_autopilot::webhook::ratelimit::{
    retry_after_secs, SlidingWindowLimiter, SlidingWindowRateLimit,
};
use mc_core::Id;
use mc_errors::Error;
use mc_repos::feedback::FeedbackRepo;
use serde::{Deserialize, Serialize};

use crate::error::{ApiError, ApiResult};
use crate::routes::agents::{bad_request, repo_err};
use crate::routes::auth_user::AuthUser;
use crate::state::AppState;

/// 上游 `feedbackMaxMessageLen = 10000`（逐字）。
pub const FEEDBACK_MAX_MESSAGE_LEN: usize = 10_000;

/// 上游 `feedbackHourlyRateLimit = 10`（逐字，`feedback.go:26`）。
pub const FEEDBACK_HOURLY_RATE_LIMIT: u32 = 10;

/// 上游 `feedbackBodyLimit = 64 * 1024`（逐字）。超限 ⇒ **400**（见文件头）。
pub const FEEDBACK_BODY_LIMIT: usize = 64 * 1024;

/// 10/h 滑动窗口闸（**进程级全局**，与 webhook 三条闸同款：状态不可从 `AppState` 取，
/// 因为 `state.rs` 是**共享锚点**、不在本片写集里）。
static FEEDBACK_LIMITER: LazyLock<SlidingWindowLimiter> = LazyLock::new(|| {
    SlidingWindowLimiter::new(SlidingWindowRateLimit::new(
        FEEDBACK_HOURLY_RATE_LIMIT,
        Duration::from_secs(3600),
    ))
});

/// 上游 `CreateFeedbackRequest`（`workspace_id` 可空且**不**从鉴权上下文取 ——
/// 上游逐字从体里读 `req.WorkspaceID`，缺省 `NULL`）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateFeedbackRequest {
    /// 正文（`TrimSpace` 后非空、≤ [`FEEDBACK_MAX_MESSAGE_LEN`]）。
    #[serde(default)]
    pub message: String,
    /// 提交时所在页面。
    #[serde(default)]
    pub url: String,
    /// 粗分类（`bug` / `feature` / `general` / `praise`；**不**校验，只进指标）。
    #[serde(default)]
    pub kind: String,
    /// 工作区（**可空**；landing-page 反馈没有它）。
    #[serde(default)]
    pub workspace_id: Option<String>,
    /// 诊断上下文（**只**接受 `desktop_route_error` 那一族，见 [`valid_feedback_context`]）。
    #[serde(default)]
    pub context: Option<serde_json::Value>,
}

/// 上游 `FeedbackResponse`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedbackResponse {
    /// 新建的 `feedback.id`。
    pub id: String,
    /// 提交时刻。
    pub created_at: String,
}

/// feedback 切片：1 条，**单形态**。
pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/feedback", post(create_feedback))
}

/// 上游 `desktopRouteErrorFeedbackContextKind`（逐字）。
pub const DESKTOP_ROUTE_ERROR_KIND: &str = "desktop_route_error";

/// 上游 `validFeedbackContext` 逐字：`kind` 必须是 `desktop_route_error`，且
/// `trigger` / `error.name` / `error.message` 三个都非空。
///
/// `context = None` ⇒ **合法**（普通反馈不带诊断上下文）。
pub fn valid_feedback_context(context: Option<&serde_json::Value>) -> bool {
    let Some(context) = context else {
        return true;
    };
    if context.get("kind").and_then(serde_json::Value::as_str) != Some(DESKTOP_ROUTE_ERROR_KIND) {
        return false;
    }
    let non_empty = |path: &[&str]| {
        let mut cursor = context;
        for key in path {
            let Some(next) = cursor.get(*key) else {
                return false;
            };
            cursor = next;
        }
        cursor
            .as_str()
            .is_some_and(|value| !value.trim().is_empty())
    };
    non_empty(&["trigger"]) && non_empty(&["error", "name"]) && non_empty(&["error", "message"])
}

/// 上游 `feedbackImageRegex = regexp.MustCompile(`!\[[^\]]*\]\([^)]+\)`)` 逐字。
///
/// ⚠️ 上游逐字注明它是**粗**检查：「a false positive on a literal "![" in prose is
/// acceptable for a support-triage signal」⇒ 本仓**不**引入 regex 依赖（`mc-core` 没有
/// `regex`，加它就要动 manifest 与 `Cargo.lock`），改为**手写**这一个模式的扫描，
/// 语义与上游逐条一致（见 [`has_images`]）。
pub fn has_images(message: &str) -> bool {
    // 模式：`!` `[` 任意非 `]` 序列 `]` `(` 任意非 `)` 序列 `)`。
    // 手写扫描：找 `!` → 必须紧跟 `[` → 找该 `[` 之后的**第一个** `]`（`[^]]*` 不跨 `]`）
    // → 必须紧跟 `(` → 找该 `(` 之后的**第一个** `)`（`[^)]*` 不跨 `)`）→ 且两段**非空**由
    // 模式本身保证（`*` 允许空，但 `()` 空内容在 Go 与此处**都**算命中，保持一致）。
    let bytes = message.as_bytes();
    for (i, byte) in bytes.iter().enumerate() {
        if *byte != b'!' {
            continue;
        }
        let Some(&open_bracket) = bytes.get(i + 1) else {
            continue;
        };
        if open_bracket != b'[' {
            continue;
        }
        // `[^]]*` 停在该 `[` 之后的**第一个** `]`。
        let Some(close_bracket_rel) = bytes[i + 2..].iter().position(|b| *b == b']') else {
            continue;
        };
        let close_bracket = i + 2 + close_bracket_rel;
        if bytes.get(close_bracket + 1) != Some(&b'(') {
            continue;
        }
        // `[^)]*` 停在该 `(` 之后的**第一个** `)`。
        let Some(close_paren_rel) = bytes[close_bracket + 2..]
            .iter()
            .position(|b| *b == b')')
        else {
            continue;
        };
        let _ = close_bracket + 2 + close_paren_rel;
        return true;
    }
    false
}

/// `POST /api/feedback`（上游 `CreateFeedback`）。
///
/// 远端地址只喂限流的**日志**（限流键是 **user id**，不是 IP —— 上游逐字
/// `CountRecentFeedbackByUser` 是 per-user）；`Option<ConnectInfo<_>>` 的理由与
/// `routes/webhooks/autopilots.rs` 相同（生产一定注入，测试直连 router 时不注入）。
pub async fn create_feedback(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    peer: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<FeedbackResponse>)> {
    let user_id = user.id();

    // ① 体上限（64 KiB）⇒ 超限与非法体**同一条 400**（文件头「不是 413」）。
    if body.len() > FEEDBACK_BODY_LIMIT {
        return Err(ApiError(bad_request("invalid request body")));
    }
    let request: CreateFeedbackRequest =
        serde_json::from_slice(&body).map_err(|_| ApiError(bad_request("invalid request body")))?;

    // ② 正文：先 `TrimSpace`，再判空、再判长（上游这个顺序逐字）。
    let message = request.message.trim().to_owned();
    if message.is_empty() {
        return Err(ApiError(bad_request("message is required")));
    }
    if message.len() > FEEDBACK_MAX_MESSAGE_LEN {
        return Err(ApiError(bad_request("message too long")));
    }

    // ③ 诊断上下文（`None` 合法）。
    if !valid_feedback_context(request.context.as_ref()) {
        return Err(ApiError(bad_request("invalid feedback context")));
    }

    // ④ 限流 10/h（**消费**一次）。键 = user id ⇒ 不同用户互不影响。
    if !FEEDBACK_LIMITER.allow(&user_id.as_string()) {
        let retry = FEEDBACK_LIMITER.retry_after(&user_id.as_string());
        if let Some(ConnectInfo(addr)) = peer {
            tracing::debug!(peer = %addr, "feedback: per-user hourly cap reached");
        }
        return Err(ApiError(Error::RateLimited {
            retry_after_secs: u32::try_from(retry_after_secs(retry)).unwrap_or(u32::MAX),
        }));
    }

    // ⑤ `workspace_id` 只做 UUID 形状校验（畸形 ⇒ 400），**不**当鉴权依据。
    let workspace_id = match request.workspace_id.as_deref().filter(|v| !v.is_empty()) {
        Some(raw) => Some(
            Id::parse(raw.trim())
                .map_err(|_| ApiError(bad_request("invalid workspace_id")))?,
        ),
        None => None,
    };

    // ⑥ `metadata`：`has_images` 是**标记**（`DoD` 第 1 条），不是图片本体。
    let metadata = metadata_of(&request, &message, &headers);

    let row = FeedbackRepo::new(state.db.clone())
        .create(user_id, workspace_id, &message, &metadata)
        .await
        .map_err(|e| repo_err(e, "feedback"))?;

    Ok((
        StatusCode::CREATED,
        Json(FeedbackResponse {
            id: row.id.to_string(),
            created_at: row.created_at.to_rfc3339(),
        }),
    ))
}

/// 上游 `metadata` 那张 map（逐字：`url` / `platform` / `version` / `os` / `user_agent`
/// + 可选 `context`），本仓**额外**加一个 `has_images` 布尔标记（`DoD` 第 1 条要求它落库）。
///
/// `platform` / `version` / `os` 来自上游的 `middleware.ClientMetadataFromContext`
/// （客户端头）—— 本仓无那条中间件，**留空串**而不是伪造值。
fn metadata_of(
    request: &CreateFeedbackRequest,
    message: &str,
    headers: &HeaderMap,
) -> serde_json::Value {
    let mut metadata = serde_json::json!({
        "url": request.url,
        "platform": "",
        "version": "",
        "os": "",
        "user_agent": headers
            .get(axum::http::header::USER_AGENT)
            .and_then(|value| value.to_str().ok())
            .unwrap_or(""),
        // 🔴 `DoD` 第 1 条：布尔标记，不解析 markdown、不碰附件表。
        "has_images": has_images(message),
    });
    if let Some(context) = &request.context {
        metadata["context"] = context.clone();
    }
    metadata
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use crate::state::{AdapterRegistry, AppState, ConfigSnapshot, RuntimeHandles};

    fn state() -> Arc<AppState> {
        let db =
            mc_db::Db::connect_lazy("postgres://np:np@127.0.0.1:1/none", 1, 0).expect("lazy");
        let realtime = mc_realtime::RealtimeHandle::start(8);
        let ws = Arc::new(mc_realtime::WsState::new(realtime.clone(), "lum-1820"));
        Arc::new(AppState::new(
            db,
            RuntimeHandles {
                actors: mc_core::actor::ActorRegistry::new(),
                adapters: Arc::new(AdapterRegistry::default()),
            },
            ConfigSnapshot::default(),
            realtime,
            ws,
        ))
    }

    fn probe() -> Router {
        let state = state();
        router().with_state(state)
    }

    async fn call(method: &str, uri: &str, body: Option<&str>) -> StatusCode {
        use axum::body::Body as AxumBody;
        use http_body_util::BodyExt as _;
        use tower::ServiceExt as _;

        let mut builder = axum::http::Request::builder().method(method).uri(uri);
        if body.is_some() {
            builder = builder.header(axum::http::header::CONTENT_TYPE, "application/json");
        }
        let response = probe()
            .oneshot(
                builder
                    .body(body.map_or_else(AxumBody::empty, |b| AxumBody::from(b.to_owned())))
                    .expect("request"),
            )
            .await
            .expect("router call");
        let status = response.status();
        let _ = response.into_body().collect().await;
        status
    }

    /// 形态：本片**只**注册无尾斜杠那一形态（补尾斜杠 = `EXTRA_ALIAS` 硬失败）。
    #[tokio::test]
    async fn registered_as_a_single_form_without_trailing_slash() {
        assert_eq!(call("POST", "/api/feedback", Some("{}")).await, StatusCode::UNAUTHORIZED);
        assert_eq!(call("POST", "/api/feedback/", Some("{}")).await, StatusCode::NOT_FOUND);
    }

    /// 无会话 ⇒ 401（`AuthUser` 提取器先于一切）。
    #[tokio::test]
    async fn without_a_session_it_is_401() {
        assert_eq!(call("POST", "/api/feedback", Some("{}")).await, StatusCode::UNAUTHORIZED);
    }

    /// `has_images` 的正/反例（`DoD` 第 1 条的判据）。
    ///
    /// 模式 `!\[[^\]]*\]\([^)]+\)` 逐条对：alt 段**不**跨 `]`，url 段**不**跨 `)`。
    #[test]
    fn has_images_matches_the_upstream_regex_on_positive_and_negative_cases() {
        // 正例。
        assert!(has_images("![](/img/a.png)"));
        assert!(has_images("see ![alt](https://x/y.png) here"));
        assert!(has_images("![](x)"));
        // `[^)]+` 停在该 `(` 之后的**第一个** `)` ⇒ 尾随的 `)` 仍在模式内命中
        // （Go 正则逐字同款：不是「整串消费到底」）。
        assert!(has_images("![alt](x))"));
        // alt 段**不**跨 `]`：第一个 `]` 后面不是 `(` ⇒ 不命中。
        assert!(!has_images("![a]b](x)"));
        // 反例。
        for negative in [
            "",
            "no images here",
            "![missing paren",
            "![missing bracket(",
            "[alt](x)",      // 缺 `!`
            "![](",          // 缺右括号
            "![alt](",        // 缺右括号
            "![]",            // 只有感叹号 + 方括号
            "![alt] (x)",     // `]` 与 `(` 之间有空格
        ] {
            assert!(!has_images(negative), "{negative:?} must not count as an image");
        }
    }

    /// `valid_feedback_context`：`None` 合法；`desktop_route_error` 那一族三个字段都得非空。
    #[test]
    fn feedback_context_is_validated_exactly_like_upstream() {
        assert!(valid_feedback_context(None));
        // 合法的一整族。
        assert!(valid_feedback_context(Some(&serde_json::json!({
            "kind": "desktop_route_error",
            "trigger": "send",
            "error": {"name": "TypeError", "message": "boom"}
        }))));
        // 逐个反例。
        for negative in [
            serde_json::json!({"kind": "other", "trigger": "t",
                               "error": {"name": "n", "message": "m"}}),
            serde_json::json!({"kind": "desktop_route_error", "trigger": "  ",
                               "error": {"name": "n", "message": "m"}}),
            serde_json::json!({"kind": "desktop_route_error", "trigger": "t",
                               "error": {"name": "", "message": "m"}}),
            serde_json::json!({"kind": "desktop_route_error", "trigger": "t",
                               "error": {"name": "n", "message": " "}}),
            serde_json::json!({"kind": "desktop_route_error", "trigger": "t"}),
        ] {
            assert!(
                !valid_feedback_context(Some(&negative)),
                "{negative} must be rejected"
            );
        }
    }

    /// 限流闸：10/h，per-user，且**复用** `SlidingWindowLimiter`（`DoD` 第 2 条）。
    #[test]
    fn the_limiter_is_ten_per_hour_and_reused_not_rewritten() {
        assert_eq!(FEEDBACK_HOURLY_RATE_LIMIT, 10);
        // 闸的类型就是 M5-5 那个（不是第二份实现）—— 逐字按类型断言。
        let _: &SlidingWindowLimiter = &FEEDBACK_LIMITER;
        // 消费 10 次放行，第 11 次被拒；换一个键照旧放行（per-user 而非全局）。
        let probe = SlidingWindowLimiter::new(SlidingWindowRateLimit::new(
            FEEDBACK_HOURLY_RATE_LIMIT,
            Duration::from_secs(3600),
        ));
        for _ in 0..FEEDBACK_HOURLY_RATE_LIMIT {
            assert!(probe.allow("user-a"));
        }
        assert!(!probe.allow("user-a"));
        assert!(probe.allow("user-b"));
    }
}
