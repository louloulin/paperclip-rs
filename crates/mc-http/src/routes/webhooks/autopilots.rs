//! M5-5：`POST /api/webhooks/autopilots/:token`（autopilot webhook 入口）—— **本波唯一无认证路由**。
//!
//! - **写者**：M5-5（`docs/44` §3.2）。切片只实现本文件的 `router()`，不改
//!   `webhooks/mod.rs` / `mount.rs` / `routes/mod.rs`。
//! - **路由**（`router.go:1487`）：
//!
//! | # | 方法 | 路径 | 上游 handler | span |
//! | ---: | --- | --- | --- | ---: |
//! | 21 | POST | `/api/webhooks/autopilots/:token` | `HandleAutopilotWebhook` | 298 |
//!
//! - **单形态**（plain 路由）⇒ 不要加尾斜杠别名（会被门 ⑦ 判 `EXTRA_ALIAS`）；路径参数写
//!   `:token`（matchit 0.7 把 `{token}` 当字面量段：编译过、恒 404）。
//! - **与 M5-3 的 token 形态对齐**：路径形态由 `webhookPathForToken` 定（trigger 写面铸造
//!   token 的地方），两边**逐字一致**；token → trigger/workspace 的反查在
//!   `mc_repos::autopilot::ingress`。
//! - **上游体量**：`handler/autopilot_webhook.go` 1,010 + admission 段 ⇒ 真值在
//!   `mc_autopilot::webhook/**`（签名 / 限流 / admission / provider 四个文件），本文件只做
//!   「取 token → 读 body → 调 service → 映射状态码」。
//! - **⑨ 现状**：本路由**零 fixture**（`contracts/golden/autopilots/` 8 条只碰 2 条路由）
//!   ⇒ 等价证据靠本地 e2e（`crates/mc-http/tests/autopilots/webhook*.rs`，`docs/44` §6.2 的补救口径）。
//!
//! # 无认证面（R5）的三条硬约束
//!
//! 1. **不放任何工作区中间件**：token 就是唯一凭证；workspace 作用域由 token 反查 trigger 后
//!    **从 DB 取**（`ingress.autopilot_workspace_id`），**绝不**读客户端给的 `X-Workspace-ID`。
//!    本文件因此连 `AuthUser` 都不出现在签名里 —— 有它反而会被误当成授权入口。
//! 2. **不回显内部细节**：所有 5xx/4xx 都是固定文案（`{"error":"internal error"}` 等），
//!    真实原因只进 `tracing`。唯一的例外是 body 解析错误 —— 那是**调用方自己的 body**，
//!    上游也原样回显（`invalid json: …`）。
//! 3. **不泄漏存在性**：未知 token / 空 token / 父 autopilot 行缺失 / workspace 交叉校验失败
//!    全部折成同一个 404 `{"error":"webhook not found"}`（上游逐字）。
//!
//! # 投递 worker 的唤醒口（M5-D8 / `LUM-1745`，登记 `docs/32` §27）
//!
//! 入站面三处「投递留在 `queued`」的时刻要提示投递 worker（上游 `autopilot_webhook.go` 的
//! `h.WebhookDeliveryWorker.Notify()`）：构造 `WebhookIngress` 时就地挂上
//! [`webhook_notify_port()`]，实现由宿主 `apps/mc-server/src/webhook_worker.rs` 注入。
//! 拿不到端口 ⇒ [`DisabledNotify`]（**诚实退化**：不假装 worker 被唤醒，投递仍由 worker 自己的
//! `1s` ticker 消费）。槽的形状与纪律逐条复刻 M8-2 的 `routes/github/webhook.rs::PR_REFRESH_SLOT`。
//!
//! # 响应形状：**扁平** `{"error":"…"}` + 尾随换行（不是本仓标准错误体）
//!
//! 本仓标准错误体是嵌套的 `{"error":{"code":…,"message":…}}`（`crate::error`），但上游 webhook
//! 面无认证入口用的是 `writeError` ⇒ `json.Marshal(map[string]string{"error": msg})`。
//! provider（GitHub/GitLab 的投递 UI）按扁平体解析，所以这里**手写**形状，不复用 `ApiError`。
//! 上游 `writeJSON` 显式补了一个尾随 `\n`（注释逐字："Match the trailing newline that
//! json.Encoder.Encode historically appended"）⇒ 本地也补，字节形态一致。
//!
//! # 413 为什么靠 `DefaultBodyLimit` 而不是 `to_bytes(.., max+1)`
//!
//! 上游用 `http.MaxBytesReader` + `errors.As(err, &mbe)` 把「读流超限」（413）与「其它读错」
//! （400）分开。`axum::body::to_bytes` 的 `axum::Error` **不暴露内部类型**，而
//! `http_body_util::LengthLimitError` 在本 crate 只是 dev-dependency（`src/` 拿不到）。
//! 等价且类型安全的路子是 `Bytes` 抽取器 + [`DefaultBodyLimit`]：超限时它给
//! `BytesRejection::FailedToBufferBody(FailedToBufferBody::LengthLimitError)`，正是上游
//! `MaxBytesError` 的同位物，于是 413/400 两分支**逐字**对上（见 `read_body`）。
//!
//! # `ConnectInfo` 为什么是 `Option`
//!
//! 远端 IP 只喂给限流（`clientIPForRateLimit`）。生产链路一定带
//! `into_make_service_with_connect_info::<SocketAddr>()`（`apps/mc-server/src/main.rs`），
//! 但**测试直连 router** 时不会注入它。用严格版 `ConnectInfo` 抽取器会让「没注入」变成
//! axum 自带的 500 拒绝体（形状不对且信息量为零）；`Option<ConnectInfo<_>>` 则让本文件
//! 继续按契约回 `{"error":"internal error"}`，同时把缺失记进日志。**没有**可信代理支持
//! （上游 `MULTICA_TRUSTED_PROXIES`）：转发头一律不读，见 `docs/54` D2。

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};

use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, FailedToBufferBody};
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use serde_json::{json, Value};

use mc_autopilot::webhook::provider::WebhookHeaders;
use mc_autopilot::webhook::{
    disabled_notify, InboundRequest, SharedWebhookNotify, WebhookError, WebhookIngress,
    MAX_WEBHOOK_BODY_BYTES,
};

use crate::state::AppState;

/// webhook 面 router（1 条路由 / 1 个注册键，单形态）。
///
/// body 上限挂在**本子 router** 上（它只有这一条路由 ⇒ 等价于挂在这条路由上）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/webhooks/autopilots/:token",
            post(handle_autopilot_webhook),
        )
        .layer(DefaultBodyLimit::max(MAX_WEBHOOK_BODY_BYTES))
}

/// `HandleAutopilotWebhook`（上游 `handler/autopilot_webhook.go:347`）。
///
/// 抽取器顺序即执行顺序（axum 保证 `FromRequestParts` 先跑、body 抽取器最后跑）：路径 token →
/// 远端地址 → 请求头 → body。判负阶梯与响应形状全在
/// [`WebhookIngress::handle_inbound`]（`mc_autopilot::webhook::admission`），本函数只负责
/// 「HTTP ↔ 纯数据」的两次转换。
///
/// 返回 `Response` 而不是 `Result<_, ApiError>`：错误体是**扁平**的（见模块文档），
/// 折进 `ApiError` 会变成嵌套体 ⇒ 形状分叉。所以这里显式构造。
async fn handle_autopilot_webhook(
    State(state): State<Arc<AppState>>,
    Path(token): Path<String>,
    peer: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    // ① body：413（超限）/ 400（其它读错）—— 上游 `MaxBytesReader` 的两个分支。
    let body = match read_body(body) {
        Ok(body) => body,
        Err(err) => return write_error(&err),
    };

    // ② 请求头子集：按名字取**第一个**值（上游 `headers.Get`），名单与落库名单是同一份
    //    （`WebhookHeaders::INBOUND_HEADER_NAMES`）⇒ 不会出现「多读了一个头」。
    let mut webhook_headers = WebhookHeaders::new();
    for name in WebhookHeaders::INBOUND_HEADER_NAMES {
        if let Some(value) = headers.get(name).and_then(|value| value.to_str().ok()) {
            webhook_headers.set(name, value);
        }
    }

    // ③ 远端 IP：`clientIPForRateLimit` 只要 host 部分（`127.0.0.1:53421` → `127.0.0.1`）。
    let peer_ip = if let Some(ConnectInfo(addr)) = peer {
        Some(addr.ip().to_string())
    } else {
        tracing::warn!("webhook: no ConnectInfo; per-IP rate limits are disabled for this request");
        None
    };

    let ingress = WebhookIngress::new(state.db.pool().clone())
        .with_events(state.realtime.clone())
        .with_notify(webhook_notify_port());
    let outcome = ingress
        .handle_inbound(&InboundRequest {
            token: &token,
            peer_ip: peer_ip.as_deref(),
            headers: webhook_headers,
            body: &body,
        })
        .await;

    match outcome {
        Ok(outcome) => {
            // 状态码由 service 决定（只有签名被拒是 401，其余 200），响应体与
            // `webhook_delivery.response_body` 是**同一份**数据（`InboundOutcome::body`）。
            write_json(outcome.http_status(), &outcome.body(), None)
        }
        Err(err) => write_error(&err),
    }
}

/// body 读取：把 `Bytes` 抽取器的拒绝折成上游的两种错误。
///
/// - `FailedToBufferBody(LengthLimitError)` ⇒ 413 `payload too large`
///   （上游 `http.MaxBytesError`；由 [`DefaultBodyLimit`] 或全局 `RequestBodyLimitLayer` 触发）。
/// - 其它（连接中断 / 畸形 chunked）⇒ 400 `failed to read request body`（上游逐字）。
fn read_body(body: Result<Bytes, BytesRejection>) -> Result<Bytes, WebhookError> {
    match body {
        Ok(body) => Ok(body),
        Err(BytesRejection::FailedToBufferBody(FailedToBufferBody::LengthLimitError(_))) => {
            Err(WebhookError::PayloadTooLarge)
        }
        Err(rejection) => {
            tracing::debug!(error = %rejection, "webhook: failed to read request body");
            Err(WebhookError::Invalid {
                message: "failed to read request body".to_owned(),
            })
        }
    }
}

/// 上游 `writeError`：扁平 `{"error": msg}` + 尾随换行（见模块文档），外加限流的
/// `Retry-After`（只有 429 有）。
///
/// 状态码与文案都在 [`WebhookError`] 上（`http_status()` / `Display`），本函数不做任何判断
/// —— 「哪句话配哪个码」只有一处真值。
fn write_error(err: &WebhookError) -> Response {
    write_json(
        err.http_status(),
        &json!({ "error": err.to_string() }),
        err.retry_after_secs(),
    )
}

/// 上游 `writeJSON`：`application/json` + 尾随 `\n`。
///
/// `Content-Length` 交给 axum（body 是 String ⇒ 定长，hyper 自己会写对；
/// 上游显式写它只是为了避开 chunked）。
fn write_json(status: u16, body: &Value, retry_after_secs: Option<u64>) -> Response {
    let mut payload = body.to_string();
    payload.push('\n');

    let mut out = HeaderMap::new();
    out.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    if let Some(secs) = retry_after_secs {
        // 上游 `strconv.Itoa(int(math.Ceil(seconds)))`，且至少 1 秒（`writeWebhookRateLimit`
        // 里那两处 `max(1)`）—— 到点即 0 秒会让客户端立刻重试，反而放大压力。
        let value = HeaderValue::from_str(&secs.to_string())
            .unwrap_or_else(|_| HeaderValue::from_static("1"));
        out.insert(header::RETRY_AFTER, value);
    }

    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, out, payload).into_response()
}

// ---------------------------------------------------------------------------
// 投递 worker 的唤醒端口（M5-D8 / `LUM-1745`，登记 `docs/32` §27）
// ---------------------------------------------------------------------------

/// 进程级注入槽：`None` ⇒ [`disabled_notify`]（**诚实退化**：不假装 worker 被唤醒，投递仍由
/// worker 自己的 `1s` ticker 消费）。
///
/// # 为什么不挂 `AppState`（与 M8-2 同一条处置）
///
/// 上游把投递 worker 挂在 `Handler` 上（`handler.go`），本仓 `AppState` **没有**对应字段
/// （`crates/mc-http/src/state.rs` 是各波的共享锚点）。写法与纪律逐条复刻 M8-2 的
/// `routes/github/webhook.rs::PR_REFRESH_SLOT`：包级槽 + 「读用缺省 / 写是生产装配点 / 清是
/// 测试收尾」三件；生产装配点在 `apps/mc-server/src/webhook_worker.rs::start()` 里。
static WEBHOOK_NOTIFY_SLOT: Mutex<Option<SharedWebhookNotify>> = Mutex::new(None);

/// 当前生效的唤醒端口（未注入 ⇒ 未配置的空实现）。
///
/// 每个入站请求都会读一次（[`handle_autopilot_webhook`] 在构造 `WebhookIngress` 时）⇒
/// 装配顺序无关紧要：`main.rs` 先建 router、后起 worker 也照样生效。
pub fn webhook_notify_port() -> SharedWebhookNotify {
    WEBHOOK_NOTIFY_SLOT
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
        .unwrap_or_else(disabled_notify)
}

/// 注入端口（**生产装配点**：`apps/mc-server` 的 `webhook_worker::start()`）。
pub fn set_webhook_notify_port(port: SharedWebhookNotify) {
    *WEBHOOK_NOTIFY_SLOT
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(port);
}

/// 清掉注入（测试收尾）。
pub fn reset_webhook_notify_port() {
    *WEBHOOK_NOTIFY_SLOT
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = None;
}

/// 叫醒投递 worker（**入站面之外**的调用点：`routes/autopilots/delivery.rs` 的 replay
/// 新建了一条 `queued` 行 ⇒ 上游 `webhook_delivery.go:344` 的 `Notify()`）。
///
/// 把 trait 关在本文件里：调用方只需一行，不必为了 `.notify()` 把
/// `mc_autopilot::webhook::WebhookNotify` 导进自己的 `use` 表。
pub(crate) fn notify_webhook_worker() {
    webhook_notify_port().notify();
}
