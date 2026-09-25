//! webhook 入口面（**唯一无认证入口**，R5）。
//!
//! - **写者**：M5-5（`webhook/**` 整组）。
//! - **上游**：`handler/autopilot_webhook.go` 全 1,010（`HandleAutopilotWebhook`298 /
//!   `persistInboundDelivery`52 / `finalise*`65 / 签名 36 / 事件过滤 129 / 限流 40 / provider 适配 145）
//!   以及 `service/autopilot.go` 的 `AdmitAutopilotWebhookDelivery`68 /
//!   `recoverConcurrentWebhookAdmission`26 / `DispatchAutopilotForWebhookDelivery`43 /
//!   `ensureWebhookCreateIssueTask`61 / `repairAutopilotRunTaskLink`56。
//! - **路由**：`POST /api/webhooks/autopilots/{token}`（本波唯一无认证路由，`docs/44` §1.1 #21）。
//! - **`webhook_delivery.status` 语义**（按 `093_webhook_deliveries.up.sql` 的注释）：
//!   `{queued,dispatched,rejected,ignored,failed}`；被 admission 跳过的 run 仍算 `dispatched`，
//!   「跳过」记在 `autopilot_run.status` 上。
//! - **两个计数器不要混**：`attempt_count` 是**入站去重命中计数**，`dispatch_attempts` 才是 worker
//!   的派发尝试次数（`176_webhook_delivery_worker`）。
//!
//! # 本组文件的职责切分
//!
//! | 文件 | 装什么 |
//! | --- | --- |
//! | 本文件 | 常量 / 信封 / 出口与错误类型 / [`WebhookIngress`]（**只有构造**） |
//! | [`signature`] | `sha256=<hex>` HMAC 校验（复用 `mc_core::hash`，**不加依赖**） |
//! | [`ratelimit`] | 三条滑动窗口限流（**进程级全局**，见下） |
//! | [`provider`] | 请求头子集、信封归一化、provider 去重键、事件作用域过滤 |
//! | [`admission`] | 编排：入站 12 步 + worker 认领/派发/收口 |
//!
//! # 为什么限流状态在 `ratelimit.rs` 里是进程级全局
//!
//! `crates/mc-http/src/state.rs` 的 `AppState` 是**共享锚点**（本片写集不含它）⇒ 不能加字段。
//! 上游把三条 limiter 挂在 `Handler` 结构体上（`handler.go:492-494`，进程级单例，内存实现）；
//! 本地等价物就是 `ratelimit.rs` 里的 `LazyLock<SlidingWindowLimiter>`。**代价**（已登记
//! `docs/54`）：多副本部署时限额是**每副本**的，不是全局的 —— 上游没配 Redis 时同样是每进程内存
//! 实现（`cmd/server/router.go` 只在 Redis 可用时才换成 Redis limiter），所以这个偏差是**等价的**。
//!
//! # 入站编排为什么在 `admission.rs` 而不是本文件
//!
//! `mod.rs` 是组内最大的文件（常量 + 类型 + 文档），R7 的 800 行硬上限要求把 12 步编排挪出去。
//! `impl WebhookIngress` 因此跨 `mod.rs`（`new` / `pool` / `with_events`）与 `admission.rs`
//! （两个 `async fn`）—— 同 crate 内同类型多文件 `impl` 是合法的。
//!
//! # 与 M5-4 的边界（**不可越过**）
//!
//! **本文件不调用 `AutopilotDispatcher::dispatch()`**。本地 `dispatch()` 在准入闸/建 run 之后
//! **继续跑完整副作用**（`dispatch_run`），而上游 `AdmitAutopilotWebhookDelivery` 只做准入、
//! 把副作用留给 worker。所以 M5-5 走显式两段：
//!
//! - **A 段（入站，同步）**：`should_skip_dispatch` → `record_skipped`；否则 `initial_status` +
//!   `create_run_with_quota`。
//! - **B 段（worker）**：`dispatch_run(autopilot, Some(trigger_id), RunSource::Webhook, &run, None)`。
//!
//! A 段里被复用的 M5-4 API 一律 `pub(crate)`（`create_run_with_quota` / `should_skip_dispatch` /
//! `initial_status`）—— 本 crate 内可达，且**不改** `dispatch/**` 一个字节。

pub mod admission;
pub mod provider;
pub mod ratelimit;
pub mod signature;
mod worker;

use std::sync::Arc;

use mc_realtime::RealtimeHandle;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use crate::dispatch::AutopilotDispatcher;
use provider::WebhookHeaders;

/// 上游 `maxWebhookBodyBytes`：入站 body 上限 256 KiB。
///
/// 由 HTTP 层（`routes/webhooks/autopilots.rs`）在**读流**阶段执行（`to_bytes(body, max+1)`）：
/// 超限时连 body 都不读完就 413，避免把几百 MB 的 JSON 缓冲进内存再判大小。
pub const MAX_WEBHOOK_BODY_BYTES: usize = 256 * 1024;

/// 上游 `webhookTokenPrefix`：token 形态 `awt_` + RawURLEncoding(32B) = 47 字符。
///
/// **铸造**归 M5-3（`mc_repos::autopilot::trigger`），本文件只用它的形态做文档与长度断言。
pub const WEBHOOK_TOKEN_PREFIX: &str = "awt_";

/// 签名状态闭集（`webhook_delivery.signature_status` 的 CHECK）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SigStatus {
    /// 未配置 `signing_secret`：只验 bearer token。
    NotRequired,
    /// 配了 secret 且 HMAC 匹配。
    Valid,
    /// 配了 secret、带了签名头，但 HMAC 不匹配。
    Invalid,
    /// 配了 secret 但没带签名头。
    Missing,
}

impl SigStatus {
    /// 落库字符串。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotRequired => "not_required",
            Self::Valid => "valid",
            Self::Invalid => "invalid",
            Self::Missing => "missing",
        }
    }
}

/// 入站终态的交付状态（`webhook_delivery.status` 的 CHECK）。
///
/// `queued` / `dispatched` 由 worker 收口写入；`rejected` / `ignored` 在入站就地写。
pub const DELIVERY_STATUS_QUEUED: &str = "queued";
/// worker 已把投递交给 autopilot（**包含** admission 跳过的 run，见模块头）。
pub const DELIVERY_STATUS_DISPATCHED: &str = "dispatched";
/// 签名不合法 / 缺失。
pub const DELIVERY_STATUS_REJECTED: &str = "rejected";
/// trigger 停用 / autopilot 非 active / 事件被 scope 过滤 / 配额拦下。
pub const DELIVERY_STATUS_IGNORED: &str = "ignored";
/// worker 派发失败（含重试次数用尽）。
pub const DELIVERY_STATUS_FAILED: &str = "failed";

/// `ignored` 的原因：既是响应体里的 `reason`，也是 `error` 列的值（上游同款）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IgnoredReason {
    /// `!trigger.enabled`。
    TriggerDisabled,
    /// `autopilot.status == 'archived'`。
    AutopilotArchived,
    /// `autopilot.status` 既不是 `active` 也不是 `archived`（暂停等）。
    AutopilotPaused,
    /// 事件不在 trigger 的 `event_filters` 作用域内。
    EventFiltered,
}

impl IgnoredReason {
    /// 落库 / 响应字符串。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TriggerDisabled => "trigger_disabled",
            Self::AutopilotArchived => "autopilot_archived",
            Self::AutopilotPaused => "autopilot_paused",
            Self::EventFiltered => "event_filtered",
        }
    }
}

/// `rejected` 的原因（响应体 `reason` + `error` 列）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectedReason {
    /// 签名头在但 HMAC 不匹配。
    InvalidSignature,
    /// 配了 secret 但没带签名头。
    MissingSignature,
}

impl RejectedReason {
    /// 落库 / 响应字符串。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidSignature => "invalid_signature",
            Self::MissingSignature => "missing_signature",
        }
    }
}

/// 配额拦下时写进 `webhook_delivery.reason_code` 的固定码。
pub const REASON_CODE_QUOTA_EXCEEDED: &str = "quota_exceeded";

/// 配额拦下时写进 `webhook_delivery.error` 的固定文案 —— 上游 `AutopilotQuotaExceededError.Error()`
/// 逐字（`server/internal/service/autopilot_quota.go`）。
pub const QUOTA_EXCEEDED_MESSAGE: &str = "autopilot run quota exceeded";

/// 上游 `WebhookEnvelope`：归一化后的载荷，存进 `autopilot_run.trigger_payload`。
///
/// **偏差（`docs/54` D6）**：上游 `EventPayload` 是 `json.RawMessage`（原样字节），本地是
/// `serde_json::Value`。语义等价（下游读的都是 JSON 结构），差别只在键序 —— 而 `serde_json`
/// 默认 `Map` 是 `BTreeMap`（键有序），与 Go `map[string]any` 的 marshal 行为**同为有序**。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WebhookEnvelope {
    /// 事件名。自由文本（`webhook_delivery.event` 是开放集）。
    pub event: String,
    /// 原始载荷（或调用方自带的 `eventPayload`）。
    #[serde(rename = "eventPayload")]
    pub event_payload: Value,
    /// 请求元信息。
    pub request: WebhookRequest,
}

/// 上游 `WebhookRequest`：只留「收到时刻 + content type」两个可复现字段。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WebhookRequest {
    /// RFC3339（UTC）。worker 恢复时会用 `webhook_delivery.received_at` 覆盖它。
    #[serde(rename = "receivedAt")]
    pub received_at: String,
    /// 分号前的 content type；空则省略（上游 `omitempty`）。
    #[serde(
        rename = "contentType",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub content_type: Option<String>,
}

/// 入站结果（**纯数据**，HTTP 状态码映射在 `mc-http`）。
///
/// 五种形态与上游 200 体一一对应：`accepted` / `skipped` / `ignored` / `duplicate`，
/// 外加配额拦下（上游也是 200 的 `ignored` + `reason_code`，本地单独一个变体以便日志区分）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboundOutcome {
    /// 同步建成（或复用）run：`{"status":"accepted",…}`。
    Accepted {
        /// 交付 id。
        delivery_id: Uuid,
        /// 已准入的 run。
        run_id: Uuid,
        /// autopilot id。
        autopilot_id: Uuid,
        /// trigger id。
        trigger_id: Uuid,
    },
    /// 准入闸跳过：`{"status":"skipped",…}`（run 落 `skipped`，delivery 仍走 `dispatched`）。
    Skipped {
        /// 交付 id。
        delivery_id: Uuid,
        /// 跳过态的 run。
        run_id: Uuid,
        /// 跳过原因（`autopilot_run.failure_reason`）。
        reason: Option<String>,
    },
    /// 事件被作用域过滤：`{"status":"ignored","reason":"event_filtered","event":…}`。
    EventFiltered {
        /// 交付 id。
        delivery_id: Uuid,
        /// 归一化后的事件名（响应体回显，便于排障）。
        event: String,
    },
    /// trigger / autopilot 状态导致的 `ignored`。
    Ignored {
        /// 交付 id。
        delivery_id: Uuid,
        /// 原因。
        reason: IgnoredReason,
    },
    /// 配额拦下：`{"status":"ignored","delivery_id","reason_code":"quota_exceeded"}`。
    QuotaExceeded {
        /// 交付 id。
        delivery_id: Uuid,
    },
    /// 去重命中（同 `dedupe_key` 已有投递）：返回**既有** delivery（+ 已知的 run）。
    Duplicate {
        /// 既有交付 id。
        delivery_id: Uuid,
        /// 既有投递最终链上的 run（worker 还没跑完时可能为 `None`）。
        run_id: Option<Uuid>,
    },
    /// 签名不合法 / 缺失：`{"status":"rejected",…}`。
    Rejected {
        /// 交付 id。
        delivery_id: Uuid,
        /// 原因。
        reason: RejectedReason,
    },
}

impl InboundOutcome {
    /// HTTP 状态码：只有签名被拒是 `401`，其余都是 `200`（上游逐个 `writeJSON(200)`，只有 reject 走 401）。
    #[must_use]
    pub fn http_status(&self) -> u16 {
        match self {
            Self::Rejected { .. } => 401,
            Self::Accepted { .. }
            | Self::Skipped { .. }
            | Self::EventFiltered { .. }
            | Self::Ignored { .. }
            | Self::QuotaExceeded { .. }
            | Self::Duplicate { .. } => 200,
        }
    }

    /// 响应体（也是 `webhook_delivery.response_body` 里存的那份 —— 两者**必须**一致）。
    ///
    /// 上游用 `map[string]any` + `json.Marshal`（键有序）；本地 `serde_json` 默认 `BTreeMap`
    /// 同样按键排序 ⇒ 字节形态一致。
    #[must_use]
    pub fn body(&self) -> Value {
        match self {
            Self::Accepted {
                delivery_id,
                run_id,
                autopilot_id,
                trigger_id,
            } => json!({
                "status": "accepted",
                "delivery_id": delivery_id.to_string(),
                "run_id": run_id.to_string(),
                "autopilot_id": autopilot_id.to_string(),
                "trigger_id": trigger_id.to_string(),
            }),
            Self::Skipped {
                delivery_id,
                run_id,
                reason,
            } => {
                let mut body = json!({
                    "status": "skipped",
                    "delivery_id": delivery_id.to_string(),
                    "run_id": run_id.to_string(),
                });
                if let Some(reason) = reason {
                    body["reason"] = Value::String(reason.clone());
                }
                body
            }
            Self::EventFiltered { delivery_id, event } => json!({
                "status": "ignored",
                "delivery_id": delivery_id.to_string(),
                "reason": IgnoredReason::EventFiltered.as_str(),
                "event": event,
            }),
            Self::Ignored {
                delivery_id,
                reason,
            } => json!({
                "status": "ignored",
                "delivery_id": delivery_id.to_string(),
                "reason": reason.as_str(),
            }),
            Self::QuotaExceeded { delivery_id } => json!({
                "status": "ignored",
                "delivery_id": delivery_id.to_string(),
                "reason_code": REASON_CODE_QUOTA_EXCEEDED,
            }),
            Self::Duplicate {
                delivery_id,
                run_id,
            } => {
                let mut body = json!({
                    "status": "duplicate",
                    "delivery_id": delivery_id.to_string(),
                });
                if let Some(run_id) = run_id {
                    body["run_id"] = Value::String(run_id.to_string());
                }
                body
            }
            Self::Rejected {
                delivery_id,
                reason,
            } => json!({
                "status": "rejected",
                "delivery_id": delivery_id.to_string(),
                "reason": reason.as_str(),
            }),
        }
    }
}

/// 入站 / worker 的失败面。**路由私有**（不追加到 `error.rs`：那是跨切片热点，见 `docs/54`）。
///
/// `Display` 的字面量就是上游 `writeError` 的 body —— `mc-http` 直接照抄，不要再加工。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WebhookError {
    /// 未知 token / 空 token / autopilot 行缺失 / workspace 交叉校验失败（**统一形态不泄漏存在性**）。
    #[error("webhook not found")]
    NotFound,
    /// body 不是合法 JSON 对象/数组（上游 `normalizeWebhookPayload` 的错误文案，原样透出）。
    #[error("{message}")]
    Invalid {
        /// 上游错误文案（`invalid json: …` / `body must be a JSON object or array` / `empty body`）。
        message: String,
    },
    /// body 超过 [`MAX_WEBHOOK_BODY_BYTES`]。
    #[error("payload too large")]
    PayloadTooLarge,
    /// 限流拦下；`retry_after_secs` 直接进 `Retry-After` 头。
    #[error("rate limit exceeded")]
    RateLimited {
        /// `Retry-After`（秒，向上取整且至少 1）。
        retry_after_secs: u64,
    },
    /// 库错 / 内部不一致。**真实原因只进 `tracing` 日志**（无认证入口不回显内部细节）。
    #[error("internal error")]
    Internal,
    /// 准入闸报错（上游 `AdmitAutopilotWebhookDelivery` 的非配额失败面）。
    ///
    /// 与 [`Self::Internal`] 分开是因为上游回的是另一句文案，而且**投递行留在 `queued`** ——
    /// worker 稍后会重试准入（本地无轮询循环时等下一次 sweep，见 `docs/54` D8）。
    #[error("failed to admit autopilot")]
    AdmitFailed,
    /// worker / 守护进程侧的库错。**`mc-http` 永远拿不到这个变体**（只有 B 段返回它），
    /// 所以它的 `Display` 可以带上上下文给日志用。
    #[error("{message}")]
    Worker {
        /// 带上下文的原因。
        message: String,
    },
}

impl WebhookError {
    /// HTTP 状态码。**与上游一一对应**（上游用 `http.Error` + 固定文案，本地同样只回固定文案）。
    ///
    /// 映射规则放在这里、不放在 `mc-http`，是为了让「哪句话配哪个码」只有一处真值 ——
    /// 本仓既有惯例（`mc_errors::Error::http_status() -> u16`）。
    #[must_use]
    pub fn http_status(&self) -> u16 {
        match self {
            Self::NotFound => 404,
            Self::Invalid { .. } => 400,
            Self::PayloadTooLarge => 413,
            Self::RateLimited { .. } => 429,
            Self::Internal | Self::AdmitFailed | Self::Worker { .. } => 500,
        }
    }

    /// 限流响应的 `Retry-After`（秒）。只有 [`Self::RateLimited`] 有值。
    #[must_use]
    pub fn retry_after_secs(&self) -> Option<u64> {
        match self {
            Self::RateLimited { retry_after_secs } => Some(*retry_after_secs),
            _ => None,
        }
    }
}

/// 一次入站调用的输入（**路由无关**：不含 `HeaderMap` / `SocketAddr` / HTTP 类型）。
#[derive(Debug, Clone)]
pub struct InboundRequest<'a> {
    /// URL 路径里的 bearer token（`awt_…`）。
    pub token: &'a str,
    /// 远端 IP（`RemoteAddr` 的 host 部分）。`None` = 拿不到（如 Unix socket / 测试未注入 `ConnectInfo`）
    /// ⇒ **限流整体不生效**（上游 `ip == ""` 时同理跳过两道闸）。
    pub peer_ip: Option<&'a str>,
    /// 上游 `selectedHeadersJSON` 关心的那几个头。
    pub headers: WebhookHeaders,
    /// 原始 body（**签名是对原始字节算的**，所以这里必须是未解码的 `&[u8]`）。
    pub body: &'a [u8],
}

// ---------------------------------------------------------------------------
// 投递唤醒端口（`webhook_delivery_worker.go` 的 `Notify()` 面）
// ---------------------------------------------------------------------------

/// 「队列里可能又多了一条可认领的投递」的提示口 —— 投递 worker 的唤醒侧。
///
/// # 契约：**可以丢**
///
/// 队列与租约都在 Postgres（上游 `WebhookDeliveryWorker` 的结构体注释逐字：
/// 「the in-memory notification is only a latency hint」）⇒ 实现**允许**在容量满 / 无人等待时
/// 直接丢弃提示，**不得**阻塞、**不得** panic。它只影响「多久被消费」，不影响「会不会被消费」
/// （那由 worker 自己的 `1s` ticker 保证）。
///
/// # 为什么是一个端口而不是直接一个 tokio 句柄
///
/// 本 crate 的依赖表被 anchor 冻结（`docs/44` §5.2 逐字「此后 M5 各切片**不得**再新增三方依赖」），
/// 里面**没有** `tokio` ⇒ 这里只能声明一个 `Send + Sync` 的 trait，由宿主（`apps/mc-server`，
/// 它本来就有 tokio）给出实现。形状与 M8-2 的 `mc_vcs_github::port::PrRefresh` 逐条同款
/// （端口 trait + `Disabled*` 空实现 + 进程级注入槽 + 宿主注入）。
pub trait WebhookNotify: Send + Sync + 'static {
    /// 非阻塞提示。**唯一允许的语义**：尽快，且可以丢。
    fn notify(&self);
}

/// 共享端口句柄（`Arc<dyn …>`）：入站侧**每次请求**构造一个 [`WebhookIngress`]，
/// 句柄必须廉价可克隆。
pub type SharedWebhookNotify = Arc<dyn WebhookNotify>;

/// 未接线的空实现（**诚实退化**）：不假装 worker 被唤醒 —— 投递仍由 worker 自己的 `1s`
/// ticker 消费（这正是 `DisabledPrRefresh` 的同判例）。
#[derive(Debug, Clone, Copy, Default)]
pub struct DisabledNotify;

impl WebhookNotify for DisabledNotify {
    fn notify(&self) {}
}

/// 缺省端口（未注入 ⇒ 空实现）。
#[must_use]
pub fn disabled_notify() -> SharedWebhookNotify {
    Arc::new(DisabledNotify)
}

/// webhook 入站面 + worker 面的服务对象。
///
/// 只持有 [`AutopilotDispatcher`]：pool 从它取（`dispatcher.pool()`），realtime 出口也在它里面。
/// 这样「入站建 run」与「worker 派发」共用同一套事件发布路径，不会出现一边发事件一边不发。
///
/// 另持一个[投递唤醒端口][WebhookNotify]：入站侧三处「投递留在 `queued`」的时刻提示 worker
/// （上游 `handler/autopilot_webhook.go` 的 4 处 `h.WebhookDeliveryWorker.Notify()` 里的 3 处；
/// 第 4 处在 replay 路由，读的是 `mc-http` 的进程级槽）。**B 段（worker 自己）不用它。**
///
/// 不派 `Debug` / `Clone`：[`AutopilotDispatcher`] 都没有（它持 realtime 出口），入站侧每次调用
/// 就地构造一个即可，没有跨层传递需求。
pub struct WebhookIngress {
    dispatcher: AutopilotDispatcher,
    /// 缺省 [`DisabledNotify`]（未接线，诚实退化）。
    notify: SharedWebhookNotify,
}

impl WebhookIngress {
    /// 不带 realtime 出口（单测 / worker-only 场景）。
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self {
            dispatcher: AutopilotDispatcher::new(pool),
            notify: disabled_notify(),
        }
    }

    /// 带 realtime 出口（`mc-http` 传 `AppState.realtime`，与 M5-4 的 `execution.rs` 同手法）。
    #[must_use]
    pub fn with_events(mut self, events: RealtimeHandle) -> Self {
        self.dispatcher = self.dispatcher.with_events(events);
        self
    }

    /// 带投递唤醒端口（`mc-http` 传 `webhook_notify_port()`，实现由 `apps/mc-server` 的
    /// `webhook_worker` 注入）。**additive builder**：[`Self::new`] 的单参签名被
    /// `crates/mc-http/tests/autopilots/webhook_worker.rs` 多处直接调用，不能改成必填参数。
    #[must_use]
    pub fn with_notify(mut self, notify: SharedWebhookNotify) -> Self {
        self.notify = notify;
        self
    }

    /// 库池（`ingress` 的 SQL 全走它）。
    #[must_use]
    pub fn pool(&self) -> &PgPool {
        self.dispatcher.pool()
    }

    /// 派发器（A 段准入 / B 段派发都从这里走）。
    pub(crate) fn dispatcher(&self) -> &AutopilotDispatcher {
        &self.dispatcher
    }

    /// 提示 worker「队列里可能又多了一条」。
    ///
    /// 上游三处的本地对应物（见 [`WebhookIngress`] 的文档）：去重命中（
    /// `autopilot_webhook.go:499`，**仅当该行仍是 `queued`**）、同步准入失败（`:592`，投递原地
    /// 留在队列里等下一位认领者）、已接受/跳过（`:627`，`acknowledge` 之后 `status` 仍是
    /// `queued`）。三处共同的前提都是「把这一条留给 worker」，所以提示本身**无返回值、无失败**
    /// ——丢掉一枚提示最多多等一拍 ticker（见 [`WebhookNotify`] 的契约）。
    fn notify_worker(&self) {
        self.notify.notify();
    }
}
