//! hook **引擎**（出站）+ **job 粘合**（0 路由）：Multica 唯一一处**主动调出去**的代码。
//!
//! - **写者**：M6-8（`docs/57` §3.2）。
//! - **上游**：`internal/service/plugin_hook.go`（616，入站签名校验 + 调用编排 + 出站签名）、
//!   `internal/handler/plugin_hook.go` 的 **invoke 段**（≈175）、`internal/service/plugin_schedule.go`
//!   （日程投影的对齐）、`internal/scheduler/jobs_plugin_hook.go`（353，job —— 本体在
//!   `mc-scheduler/src/jobs/plugin_hook.rs`，本文件出**它调的那个数据面函数**）。
//!
//! # 本文件的两个角色
//!
//! 1. **引擎**：`InvokeHook` 的本地形态（[`invoke_hook`]）—— 安装/触发器/传输/限流四道前置、
//!    目的地校验（`net:` scope + 公网地址）、HMAC 签名、**四个出站头逐字**、响应处理、
//!    调用记录。路由与 job 都走它，所以「限流、熔断、`net:` 目的地检查、调用记录」只有一份实现。
//! 2. **job 粘合**：`plugin_hook_schedule` 的一格投递（[`dispatch_scheduled_hook`]）—— 生产
//!    scheduler job 的三个端口方法（`list_enabled_schedules` / `load_schedule` / `dispatch_schedule` /
//!    `advance_next_run`）中**唯一有逻辑**的那个。本文件**没有路由**：M6-8 的**唯一**那条注册键是
//!    `POST /api/plugin-bridge/v1/hooks/:key`（`router.go:1598`），按**路径前缀归位**落在
//!    `routes/plugin_bridge/hooks.rs`；本文件按前缀归位规则保留为 job 落点（`docs/32` §9.2）。
//!
//! # 出站 wire 契约（跨实现契约，逐字对齐 —— 外部插件服务器按它自行校验）
//!
//! ```text
//! POST <hook URL>      Content-Type: application/json
//! X-Multica-Timestamp: <unix 秒>          X-Multica-Signature: v1=<hex>
//! X-Multica-Plugin-Installation: <uuid>   User-Agent: Multica-Hooks/1
//! 签名 = HMAC-SHA256(派生密钥, timestamp ‖ "." ‖ body)    容差 = ±5 分钟
//! ```
//!
//! 头名与算法在 `mc-plugin-host::credentials` 里各只有一份（[`super::super::…`] 见
//! [`build_hook_headers`]），本文件**不**重新拼字面量。
//!
//! # 三条「不做」
//!
//! - **不做 `transport.type="mcp"` 的调用**：上游 `InvokeHook` 对非 http 传输答
//!   `PluginErrorIncompatible`（`hook transport %q is not supported yet`）—— 本地逐字照抄。
//!   MCP 侧的传输段（上游 `plugin_mcp_transport.go` ≈150 行）不在本片写集内，登记见 `docs/32` §9.10。
//! - **不做 `event` 触发的分发器**（上游 `plugin_event_dispatch.go` / `plugin_event_bridge.go`）：
//!   它要求挂到事件总线上，而总线的落点不在本片写集内；登记见 `docs/32` §9.10。
//! - **不做 `agent` 触发的路由**（`POST /api/daemon/tasks/:id/plugin-hooks`）：那条键归 M3 的
//!   daemon 面（本仓至今恒 403 `plugin_disabled`）；登记见 `docs/32` §9.10。
//!
//! **状态：M6-8 已落地**。
//!
//! 行预算（门 ⑩）：本文件 ≤700 行。

use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use mc_core::Id;
use mc_db::Db;
use mc_feature_flags::{FeatureFlagCatalog, FeatureKey};
use mc_mcp::client::resolve_endpoint;
use mc_mcp::devorigin::{EndpointPolicy, DEV_CA_ENV, DEV_ORIGINS_ENV};
use mc_plugin_host::capabilities::{HookTransport, HookTrigger};
use mc_plugin_host::credentials::{
    hook_signing_key, sign_hook_payload, CredentialError, DeploymentKey, HOOK_SIGNATURE_HEADER,
    HOOK_SIGNATURE_VERSION, HOOK_TIMESTAMP_HEADER,
};
use mc_plugin_host::manifest::{Hook, Manifest, CONFIG_SECRET};
use mc_plugin_host::scope::net_domains;
use mc_plugin_host::token::{CallbackRequest, HookActor};
use mc_repos::plugin::hook::{HookScheduleRepo, HookScheduleRow, NewInvocation};
use mc_repos::plugin::installation::{InstallationRepo, InstallationRow};
use mc_repos::RepoError;

use super::install::{
    decode, installation_manifest, parse_installation_manifest, PluginError, PluginResult,
};
use crate::routes::v1::issues::plugin_issue_for_caller;
use crate::routes::v1::policy;
use crate::state::{AppState, PluginSecretKey};

/// 回调客户端标识（上游 `User-Agent: Multica-Hooks/1`）。
pub const HOOK_USER_AGENT: &str = "Multica-Hooks/1";

/// 头名（上游 `X-Multica-Plugin-Installation`；另外两个在 `mc-plugin-host::credentials`）。
pub const HOOK_INSTALLATION_HEADER: &str = "X-Multica-Plugin-Installation";

/// `Content-Type`（上游就这一个值）。
pub const HOOK_CONTENT_TYPE: &str = "application/json";

/// 单次调用超时缺省（manifest 的 `timeout_ms` 缺省时用它）。
pub const HOOK_DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// 响应体上限（上游 `hookMaxResponseBytes = 1 << 20`）：一个回 1GB 的端点应当失败，
/// 而不是把宿主吃光。
pub const HOOK_MAX_RESPONSE_BYTES: usize = 1 << 20;

/// 每 hook 每窗口的调用上限（上游 `hookRateLimit = 120` / `hookRateWindow = time.Minute`）。
pub const HOOK_RATE_LIMIT: i64 = 120;
/// 限流窗口。
pub const HOOK_RATE_WINDOW: Duration = Duration::from_secs(60);

/// 熔断阈值（上游 `hookBreakerThreshold = 5` / `hookBreakerWindow = 5 * time.Minute`）：
/// 一个已经坏了一分钟的端点不需要每个事件/每个计划格子各来一发才发现。
pub const HOOK_BREAKER_THRESHOLD: i64 = 5;
/// 熔断窗口。
pub const HOOK_BREAKER_WINDOW: Duration = Duration::from_secs(300);

/// 调用记录的保留期（上游 `invocationRetention = 7 * 24 * time.Hour`）。
pub const HOOK_INVOCATION_RETENTION: Duration = Duration::from_hours(168);

/// 失败描述上限（上游 `truncate(message, 500)`）。
const ERROR_LIMIT: usize = 500;

// ---------------------------------------------------------------------------
// 一次调用
// ---------------------------------------------------------------------------

/// 一次 hook 调用（上游 `HookInvocation`）。
#[derive(Debug, Clone)]
pub struct HookInvocation {
    /// 读**安装行**的 manifest（不是今天源地址吐出来的那份）：管理员同意的是具体一批端点。
    pub installation: InstallationRow,
    pub hook: Hook,
    pub trigger: HookTrigger,
    /// 仅 `event` 触发时有值。
    pub event_type: Option<String>,
    /// 写归属（上游 `HookActor`）：`ui`/`manual` 是那个人，`event`/`schedule` 是安装本身。
    pub actor: HookActor,
    /// 这次调用关于哪个 issue（有则收窄回调令牌）。
    pub issue_id: Option<Id>,
    /// 原样透传的入参（本地只做 JSON 值转发，不解析它的语义）。
    pub input: Option<Value>,
    /// 同一次计划投递重试间稳定。
    pub delivery_id: Option<String>,
    /// cron 的**计划发生时刻**。
    pub planned_at: Option<DateTime<Utc>>,
    /// 第几次尝试（`1..10`）。
    pub attempt: i32,
}

/// 一次调用的结果（上游 `HookResult`，字段名逐字）。
#[derive(Debug, Clone)]
pub struct HookCallResult {
    pub status: &'static str,
    pub output: Option<Value>,
    pub error: Option<String>,
    pub latency_ms: i32,
    pub hook_key: String,
    pub trigger: &'static str,
    pub attempts: i32,
}

impl HookCallResult {
    /// 上游 `hookResultPayload`：`output` / `error` 为空时**不出现**（不是 `null`）。
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut out = Map::new();
        out.insert("status".into(), json!(self.status));
        if let Some(output) = &self.output {
            out.insert("output".into(), output.clone());
        }
        if let Some(error) = &self.error {
            out.insert("error".into(), json!(error));
        }
        out.insert("latency_ms".into(), json!(self.latency_ms));
        out.insert("hook_key".into(), json!(self.hook_key));
        out.insert("trigger".into(), json!(self.trigger));
        out.insert("attempts".into(), json!(self.attempts));
        Value::Object(out)
    }
}

// ---------------------------------------------------------------------------
// 错误
// ---------------------------------------------------------------------------

/// hook 面的错误：状态码 + 稳定码 + 上游文案 + `plugin_invocation.status`。
///
/// 为什么不是直接用 `routes/plugins/install.rs` 的 `PluginError`：那个类型是 `pub(super)`
/// （`routes::plugins`），而本片的**跨 crate** 出口（`apps/mc-server/src/scheduler` 的端口实现
/// 调 [`dispatch_scheduled_hook`]）需要一个 `pub` 的错误类型。两者之间的折法就在本文件：
/// `impl From<HookError> for PluginError` —— 状态码表仍然只有一张（`install.rs` 是所有者）。
#[derive(Debug, Clone)]
pub struct HookError {
    status: u16,
    code: &'static str,
    message: String,
    invocation_status: &'static str,
}

impl HookError {
    fn new(status: u16, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            invocation_status: if matches!(status, 403 | 422 | 429 | 507) {
                "refused"
            } else {
                "failed"
            },
        }
    }

    /// 上游 `PluginErrorInvalid`。
    fn invalid(message: impl Into<String>) -> Self {
        Self::new(400, "validation_error", message)
    }

    /// 上游 `PluginErrorForbidden`。
    fn forbidden(message: impl Into<String>) -> Self {
        Self::new(403, "forbidden", message)
    }

    /// 上游 `PluginErrorIncompatible`。
    fn incompatible(message: impl Into<String>) -> Self {
        Self::new(422, "unprocessable", message)
    }

    /// 上游 `PluginErrorQuota`（hook 每分钟上限 ⇒ 507，与 M6-5 的 `PluginError::quota` 同码）。
    fn quota(message: impl Into<String>) -> Self {
        Self::new(507, "insufficient_storage", message)
    }

    /// 上游 `PluginErrorUnavailable` ⇒ 502。
    fn unavailable(message: impl Into<String>) -> Self {
        Self::new(502, "plugin_unavailable", message)
    }

    /// **本地口径**（`docs/32` §9.8 的 `M6D-1`，四处一致）：部署密钥未配置 ⇒ 503
    /// `plugin_disabled`。上游走 `PluginErrorUnavailable`（502），差异只有状态码这一位。
    pub fn disabled() -> Self {
        // 状态码是本地口径（503，见 `docs/32` §9.8 的 `M6D-1`），但 `plugin_invocation.status`
        // 仍按上游分类：签名密钥缺失是 `PluginErrorUnavailable` ⇒ **`failed`**，不是 `refused`。
        Self::new(
            503,
            "plugin_disabled",
            CredentialError::HooksDisabled.to_string(),
        )
    }

    /// 超时单独一态（上游 `hookFailureStatus` 把 `context.DeadlineExceeded` 折成 `timeout`）。
    fn timed_out() -> Self {
        let mut error = Self::new(502, "plugin_unavailable", "hook endpoint timed out");
        error.invocation_status = "timeout";
        error
    }

    /// HTTP 状态码。
    #[must_use]
    pub const fn status(&self) -> u16 {
        self.status
    }

    /// 稳定错误码。
    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }

    /// 上游文案。
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// 落 `plugin_invocation.status` 的取值。
    #[must_use]
    pub const fn invocation_status(&self) -> &'static str {
        self.invocation_status
    }
}

impl std::fmt::Display for HookError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl From<HookError> for PluginError {
    fn from(error: HookError) -> Self {
        Self::new(
            StatusCode::from_u16(error.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            error.code,
            error.message,
        )
    }
}

impl IntoResponse for HookError {
    fn into_response(self) -> Response {
        PluginError::from(self).into_response()
    }
}

// ---------------------------------------------------------------------------
// 引擎的运行期上下文（不是 `AppState`）
// ---------------------------------------------------------------------------

/// 引擎需要的三样东西，**故意不是 `AppState`**。
///
/// 原因在装配点：`apps/mc-server/src/main.rs`（M5-9 的接线，本片**一行不改**）调
/// `scheduler::start(&db, daemon_hub)` —— 调度循环的端口实现只能拿到 `Db` + `Hub`，
/// 拿不到 `Arc<AppState>`。把引擎的依赖面收窄成这三样，路由侧（`HookRuntime::from_state`）
/// 与 job 侧（`apps/mc-server/src/scheduler/hook_port.rs` 自己拼）就都能构造它。
#[derive(Clone)]
pub struct HookRuntime {
    /// 仓储的数据面。
    pub db: Db,
    /// 部署密钥（`MULTICA_PLUGIN_SECRET_KEY`）；`None` = hook 面整体降级（503 `plugin_disabled`）。
    pub plugin_key: Option<PluginSecretKey>,
    /// 特征开关目录（`plugins_v1` 的判定）。
    pub feature_flags: Arc<FeatureFlagCatalog>,
}

impl std::fmt::Debug for HookRuntime {
    /// 手写脱敏实现：`PluginSecretKey` 已自带一个，这里只是不把 `Db` 的细节铺开。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HookRuntime")
            .field("plugin_key", &self.plugin_key)
            .finish_non_exhaustive()
    }
}

impl HookRuntime {
    /// 路由侧的构造点（从 HTTP 状态里取三样）。
    #[must_use]
    pub fn from_state(state: &AppState) -> Self {
        Self {
            db: state.db.clone(),
            plugin_key: state.plugin_key.clone(),
            feature_flags: state.feature_flags.clone(),
        }
    }

    /// 生产装配点的构造点（调度循环只有 `Db`）。
    ///
    /// 部署密钥读进程 env（与 `AppState::new` 同一入口）；开关目录**空** ⇒ 按本仓口径
    /// 「未登记 = 开启」（`docs/32` §9.6 的 M6-5-D1）。
    #[must_use]
    pub fn standalone(db: Db) -> Self {
        Self {
            db,
            plugin_key: PluginSecretKey::from_env(),
            feature_flags: Arc::new(FeatureFlagCatalog::new()),
        }
    }

    /// 上游 `pluginsV1Enabled`（与 M6-5 / M6-7 同一口径：显式登记为 `false` 才关）。
    #[must_use]
    pub fn plugins_v1_enabled(&self) -> bool {
        self.feature_flags
            .get(&FeatureKey::new(policy::PLUGINS_V1))
            .is_none_or(|flag| flag.enabled)
    }

    /// 部署密钥的**唯一**转写点（未配置 ⇒ `None`，调用方按缺失处理）。
    #[must_use]
    pub fn deployment_key(&self) -> Option<DeploymentKey> {
        self.plugin_key
            .as_ref()
            .and_then(|key| DeploymentKey::new(key.as_bytes()))
    }
}

// ---------------------------------------------------------------------------
// 引擎（租户无关的判定 + 出站）
// ---------------------------------------------------------------------------

/// 上游 `FindHook`：从**安装行的** manifest 里取指定 key 的 hook。
///
/// # Errors
///
/// manifest 不可读 ⇒ [`PluginError::invalid`]（400）；没有这个 key ⇒ 404。
fn find_hook(installation: &InstallationRow, hook_key: &str) -> PluginResult<Hook> {
    let manifest = installation_manifest(installation)?;
    find_hook_in(&manifest, hook_key)
}

fn find_hook_in(manifest: &Manifest, hook_key: &str) -> PluginResult<Hook> {
    manifest
        .contributes
        .hooks
        .iter()
        .find(|hook| hook.key == hook_key)
        .cloned()
        .ok_or_else(|| {
            PluginError::not_found(format!("this Plugin has no hook named {hook_key:?}"))
        })
}

/// 上游 `HookAllowsTrigger`：manifest 没声明的触发器不是宿主可以自行发明的调用点。
#[must_use]
pub fn hook_allows_trigger(hook: &Hook, trigger: HookTrigger) -> bool {
    hook.triggers
        .iter()
        .any(|declared| declared == trigger.as_str())
}

/// 上游 `checkHookRate`：每 hook 每分钟 120 次（按**尝试**计）。
///
/// 读失败 ⇒ **不限流**（上游：遥测读不该把功能一起带下来）。
async fn check_rate(
    runtime: &HookRuntime,
    installation_id: Id,
    hook_key: &str,
) -> Result<(), HookError> {
    let repo = HookScheduleRepo::new(runtime.db.clone());
    let since = Utc::now() - chrono::Duration::from_std(HOOK_RATE_WINDOW).unwrap_or_default();
    let count = repo
        .count_recent(installation_id, hook_key, since, false)
        .await
        .unwrap_or(0);
    if count >= HOOK_RATE_LIMIT {
        return Err(HookError::quota(format!(
            "hook {hook_key:?} exceeded {HOOK_RATE_LIMIT} calls per minute"
        )));
    }
    Ok(())
}

/// 上游 `HookBreakerOpen`：窗口内失败次数 ≥ 阈值 ⇒ 后台投递暂停。
///
/// 读失败 ⇒ **不熔断**（同上游）。
pub async fn hook_breaker_open(runtime: &HookRuntime, installation_id: Id, hook_key: &str) -> bool {
    let repo = HookScheduleRepo::new(runtime.db.clone());
    let since = Utc::now() - chrono::Duration::from_std(HOOK_BREAKER_WINDOW).unwrap_or_default();
    repo.count_recent(installation_id, hook_key, since, true)
        .await
        .unwrap_or(0)
        >= HOOK_BREAKER_THRESHOLD
}

/// 上游 `InvokeHook`：一次调用与它的记录。
///
/// 四道前置（安装启用 / 触发器已声明 / 传输受支持 / 限流）→ 出站 → **无论成败都记一行**。
///
/// # Errors
///
/// 见 [`HookError`] 的各构造点；调用失败时返回的是**出站失败**，且 `plugin_invocation`
/// 已经落了对应的 `failed` / `timeout` / `refused` 行。
pub async fn invoke_hook(
    runtime: &HookRuntime,
    invocation: HookInvocation,
) -> Result<HookCallResult, HookError> {
    let mut invocation = invocation;
    if invocation.attempt < 1 {
        invocation.attempt = 1;
    }
    let hook_key = invocation.hook.key.clone();
    let trigger = invocation.trigger;
    let mut result = HookCallResult {
        status: "ok",
        output: None,
        error: None,
        latency_ms: 0,
        hook_key: hook_key.clone(),
        trigger: trigger.as_str(),
        attempts: invocation.attempt,
    };

    if !invocation.installation.enabled {
        return Err(HookError::forbidden("this Plugin is disabled"));
    }
    if !hook_allows_trigger(&invocation.hook, trigger) {
        return Err(HookError::forbidden(format!(
            "hook {hook_key:?} does not declare the {} trigger",
            trigger.as_str()
        )));
    }
    if invocation.hook.transport.kind != HookTransport::Http.as_str() {
        return Err(HookError::incompatible(format!(
            "hook transport {:?} is not supported yet",
            invocation.hook.transport.kind
        )));
    }
    check_rate(runtime, invocation.installation.id(), &hook_key).await?;

    let started = std::time::Instant::now();
    let called = call_hook_endpoint(runtime, &invocation).await;
    let latency_ms = i32::try_from(started.elapsed().as_millis()).unwrap_or(i32::MAX);
    result.latency_ms = latency_ms;

    let (status, message) = match &called {
        Ok(_) => ("ok", None),
        Err(error) => (error.invocation_status(), Some(error.message().to_owned())),
    };
    match called {
        Ok(output) => result.output = output,
        Err(error) => {
            result.status = error.invocation_status();
            result.error = Some(error.message().to_owned());
            record_invocation(runtime, &invocation, status, latency_ms, message.as_deref()).await;
            return Err(error);
        }
    }
    record_invocation(runtime, &invocation, status, latency_ms, None).await;
    Ok(result)
}

/// 上游 `recordInvocation`：**best effort** —— 描述调用的遥测不得让调用本身失败。
async fn record_invocation(
    runtime: &HookRuntime,
    invocation: &HookInvocation,
    status: &str,
    latency_ms: i32,
    message: Option<&str>,
) {
    let truncated: Option<String> = message.map(|text| truncate(text, ERROR_LIMIT));
    HookScheduleRepo::new(runtime.db.clone())
        .record(&NewInvocation {
            id: Uuid::new_v4(),
            installation_id: invocation.installation.id(),
            workspace_id: invocation.installation.workspace_id(),
            hook_key: &invocation.hook.key,
            trigger: invocation.trigger.as_str(),
            status,
            event_type: invocation.event_type.as_deref(),
            attempt: invocation.attempt.clamp(1, 10),
            latency_ms,
            error: truncated.as_deref(),
            delivery_id: invocation.delivery_id.as_deref(),
            planned_at: invocation.planned_at,
        })
        .await
        .ok();
}

fn truncate(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_owned();
    }
    value.chars().take(limit).collect()
}

// 门 ⑩（单文件 800 行硬上限）逼出来的拆分：`routes/plugins/mod.rs`（anchor 冻结）把
// 本片固定在 `hooks_job` 这一个条目上，所以只能靠**子模块目录**消化 ——
// `hooks_job.rs` 声明的 `mod x;` 解析到 `hooks_job/x.rs`（与 `install.rs` / `install/` 同款）。
mod bridge;
mod outbound;
mod schedule;
mod wire;

use outbound::call_hook_endpoint;

/// 桥面路由的 handler（注册在 `routes/plugin_bridge/hooks.rs`）。
pub(crate) use bridge::invoke_bridge_hook;
/// 出站 wire 契约的构造点（四个头 + 请求体）。
pub use outbound::{build_hook_headers, schedule_delivery_id};
/// job 数据面（`apps/mc-server/src/scheduler/hook_port.rs` 调的就是这四个）。
pub use schedule::{
    advance_schedule_next_run, dispatch_scheduled_hook, list_enabled_schedules, load_schedule,
    ScheduledHookOutcome, ScheduledHookRequest,
};
/// 日程投影的对齐（安装/升级/启停三处的调用点，`docs/32` §9.6 的 M6-5-D2 回填）。
pub(crate) use schedule::{reconcile_schedules_tx, set_schedules_enabled_tx};

/// 本文件**没有路由**（M6-8 的唯一注册键在 `plugin_bridge/hooks.rs`），保留合并点是给将来的
/// 管理面路由用的：`routes/plugins/mod.rs`（anchor 冻结）已经 `.merge(hooks_job::router())`。
pub fn router() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
}

#[cfg(test)]
mod tests;
