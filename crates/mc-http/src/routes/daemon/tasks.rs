//! task 面（M3-7 / LUM-1438）。
//!
//! 覆盖 12 条路由：
//!
//! | 路由 | upstream |
//! |---|---|
//! | `GET …/tasks/:taskId/status` | `GetTaskStatus` `daemon.go:4894` |
//! | `POST …/tasks/:taskId/start` | `StartTask` `daemon.go:4071` |
//! | `POST …/tasks/:taskId/wait-local-directory` | `MarkTaskWaitingLocalDirectory` `daemon.go:4105` |
//! | `POST …/tasks/:taskId/progress` | `ReportTaskProgress` `daemon.go:4138` |
//! | `POST …/tasks/:taskId/complete` | `CompleteTask` `daemon.go:4215` |
//! | `POST …/tasks/:taskId/fail` | `FailTask` `daemon.go:4946` |
//! | `POST …/tasks/:taskId/usage` | `ReportTaskUsage` `daemon.go:4817` |
//! | `GET/POST …/tasks/:taskId/messages` | `ListTaskMessages` 5352 / `ReportTaskMessages` 5037 |
//! | `POST …/tasks/:taskId/cancel-ack` | `AckTaskCancelled` `daemon.go:5203` |
//! | `POST …/tasks/:taskId/session` | `PinTaskSession` `task_lifecycle.go:70` |
//! | `POST …/tasks/:id/plugin-hooks` | `InvokeAgentPluginHook` `plugin_agent_hook.go` |
//! | `GET …/tasks/:id/plugin-mcp/:contributionId/credential` | `ResolvePluginMCPCredential` |
//!
//! ## 校验顺序是接口契约的一部分
//!
//! 上游各 handler 的「先解码还是先鉴权」并不一致，而它决定了**同一次坏请求**
//! 拿到 400 还是 404。本模块逐个照抄：
//!
//! - 先鉴权后解码：`complete` / `fail` / `wait-local-directory` / `session` /
//!   `cancel-ack` / `usage` / `messages(GET)`；
//! - 先解码后鉴权：`progress` / `messages(POST)`；
//! - `cancel-ack` 的 body 是**尽力而为**的（老 daemon 发 `{}`，解码失败也不拦），
//!   而 `wait-local-directory` 只在 body 非空时才解码（空 body 合法）。
//!
//! ## `cancel-ack` 为什么 CAS 失配也回 200
//!
//! daemon 对**每一个**观测到的终态都会 ack，包括已被别的路径终结的行。三条写入
//! 都带 `status='cancelled'` CAS 且用 `COALESCE` 永不覆盖，所以一次迟到的 ack
//! 是**有意的 no-op**：没有可重试的东西，回 200 正是让 daemon 别再重试。反过来，
//! 落库**报错**必须 500 且文案指名失败面 —— 这几个字段是「已取消任务的产出在哪」
//! 的唯一指针，一次 DB 抖动不能变成永久丢分支。
//!
//! ## 本切片不做的部分（逐条登记在 `docs/32` 偏离表）
//!
//! - **事件总线**（`task:progress` / `task:running` / `task:failed` /
//!   `task:finished` / `agent:status`）：本地无 `events.Bus` 等价物。
//!   其中 `progress` 上游**只**发事件、不写库 —— 所以本切片把它做成
//!   「解码 + 鉴权 + 200」，这是刻意的空实现而非漏写。
//! - `agent` 状态回填（`ReconcileAgentStatus`）、issue 回滚、评论对账
//!   （`reconcileCommentsOnCompletion`）、auto-retry、`issue.first_executed_at`。
//! - `complete` 的「上下文耗尽改判失败」兜底（GH #6402）：现行 daemon 在
//!   `/complete` 之前就已自行改判，本切片不复制该启发式。
//! - `pin-session` 的 chat 会话续跑指针推进（`AdvanceCancelledChatSessionPointer`）。
//! - 插件面恒禁用：两条 plugin 路由在鉴权**之前**就回 403（忠实降级，见 §7.3）。

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use mc_core::Id;
use mc_repos::daemon::{DaemonRepo, TaskUsageUpsert};

use super::dto::{
    decode_body, opt, sanitize, AckTaskCancelledRequest, DaemonTaskResponse, PinSessionRequest,
    ProgressRequest, TaskCompleteRequest, TaskFailRequest, TaskUsageRequest,
    WaitLocalDirectoryRequest,
};
use super::scope::{
    db_err, internal, normalize_provider, not_found, require_task_access, validation, DaemonAuth,
};
use crate::error::ApiResult;
use crate::state::AppState;

/// `prepare lease` 续租窗口秒数（`MarkTaskWaitingLocalDirectory` 会顺手续租）。
const PREPARE_LEASE_SECS: i64 = 90;

// ---------------------------------------------------------------------------
// status / start / wait-local-directory
// ---------------------------------------------------------------------------

/// `GET /api/daemon/tasks/:taskId/status` —— **只有** `{"status": "..."}`。
///
/// 这是每个在飞任务都在轮询的「中断信号」：行不存在 = 硬中断（404），
/// 而瞬时 DB 故障必须与它区分开（500），否则一次 DB 抖动会杀掉健康的工作。
pub(crate) async fn task_status(
    State(state): State<Arc<AppState>>,
    auth: DaemonAuth,
    Path(task_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let (task, _workspace) = require_task_access(&state, &auth, &task_id, "task not found").await?;
    Ok(Json(json!({ "status": task.status })))
}

/// `POST /api/daemon/tasks/:taskId/start`（upstream 不读请求体）。
pub(crate) async fn start_task(
    State(state): State<Arc<AppState>>,
    auth: DaemonAuth,
    Path(task_id): Path<String>,
) -> ApiResult<Json<DaemonTaskResponse>> {
    let repo = DaemonRepo::new(&state.db);
    let (existing, workspace) =
        require_task_access(&state, &auth, &task_id, "task not found").await?;
    let row = repo.start_task(existing.id()).await.map_err(db_err)?;
    // CAS 只认 `dispatched` / `waiting_local_directory` 且未开工的行；不命中即 400
    // （上游把 SQL 的报错原文直接当 400 文案）。
    let Some(row) = row else {
        return Err(validation("start task: task is not in a startable state"));
    };
    Ok(Json(DaemonTaskResponse::from_row(
        &row,
        &workspace.to_string(),
    )))
}

/// `POST /api/daemon/tasks/:taskId/wait-local-directory`。
///
/// 空 body 合法（早期 daemon 不带 reason）；`local_directory <绝对路径>` 这种
/// 老格式会被 [`sanitize_wait_reason`] 丢弃 —— 那是旧 daemon 把宿主路径写进了
/// 用户可见的等待原因。
pub(crate) async fn wait_local_directory(
    State(state): State<Arc<AppState>>,
    auth: DaemonAuth,
    Path(task_id): Path<String>,
    body: Bytes,
) -> ApiResult<Json<DaemonTaskResponse>> {
    let repo = DaemonRepo::new(&state.db);
    let (existing, workspace) =
        require_task_access(&state, &auth, &task_id, "task not found").await?;
    let req: WaitLocalDirectoryRequest = if body.is_empty() {
        WaitLocalDirectoryRequest::default()
    } else {
        decode_body(&body)?
    };
    let reason = sanitize_wait_reason(&req.reason);
    let row = repo
        .mark_waiting_local_directory(
            existing.id(),
            (!reason.is_empty()).then(|| reason.clone()),
            PREPARE_LEASE_SECS,
        )
        .await
        .map_err(db_err)?;
    let Some(row) = row else {
        return Err(validation(
            "mark task waiting_local_directory: task is not dispatched",
        ));
    };
    Ok(Json(DaemonTaskResponse::from_row(
        &row,
        &workspace.to_string(),
    )))
}

/// upstream `legacyLocalDirectoryWaitPrefix`。
const LEGACY_LOCAL_DIRECTORY_WAIT_PREFIX: &str = "local_directory ";

/// upstream `sanitizeWaitReason`：trim，并把「旧 daemon 写进来的绝对路径」清成空。
fn sanitize_wait_reason(reason: &str) -> String {
    let reason = reason.trim();
    if let Some(rest) = reason.strip_prefix(LEGACY_LOCAL_DIRECTORY_WAIT_PREFIX) {
        if starts_with_absolute_path(rest) {
            return String::new();
        }
    }
    reason.to_string()
}

/// upstream `startsWithAbsolutePath`：POSIX `/…`、UNC `\\host\share`、盘符 `C:\`/`C:/`。
///
/// 刻意**不**匹配 `local_directory (held by task abc12345)`：目录本身真叫
/// `local_directory` 时那是用户自己的标签，不是路径。
fn starts_with_absolute_path(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    if s.starts_with('/') || s.starts_with(r"\\") {
        return true;
    }
    let bytes = s.as_bytes();
    if bytes.len() >= 3 && bytes[1] == b':' && (bytes[2] == b'\\' || bytes[2] == b'/') {
        let c = bytes[0];
        return c.is_ascii_alphabetic();
    }
    false
}

// ---------------------------------------------------------------------------
// progress
// ---------------------------------------------------------------------------

/// `POST /api/daemon/tasks/:taskId/progress`。
///
/// 上游 `ReportProgress` 只发 `task:progress` 事件、**不写库**；本地无事件总线，
/// 因此本 handler 的语义边界就是「校验并确认」—— 200 表示「daemon 可以不用重试」。
/// 这是登记在 `docs/32` 的刻意降级，不是漏掉的写入。
pub(crate) async fn report_progress(
    State(state): State<Arc<AppState>>,
    auth: DaemonAuth,
    Path(task_id): Path<String>,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    // 上游先解码后鉴权：坏 body 对不存在的任务也是 400。
    let req: ProgressRequest = decode_body(&body)?;
    let (_task, _workspace) =
        require_task_access(&state, &auth, &task_id, "task not found").await?;
    // 上游 `ReportTaskProgress` 只有一件事：往事件总线推 `task.progress`
    // （`EventTaskProgress`，payload = summary/step/total），**不写任何列**。
    // 本仓 daemon 面还没有面向用户的进度事件通道（与 `agent:status` 同一条缺口，
    // 登记在 `docs/32` 偏离表），所以这里保持与上游一致的 `200 {"status":"ok"}`，
    // 并落一条 debug 日志，让「daemon 报了什么进度」在排查时可见。
    tracing::debug!(
        task_id = %task_id,
        summary = %req.summary,
        step = req.step,
        total = req.total,
        "task progress reported"
    );
    Ok(Json(json!({ "status": "ok" })))
}

// ---------------------------------------------------------------------------
// complete / fail
// ---------------------------------------------------------------------------

/// `POST /api/daemon/tasks/:taskId/complete`。
pub(crate) async fn complete_task(
    State(state): State<Arc<AppState>>,
    auth: DaemonAuth,
    Path(task_id): Path<String>,
    body: Bytes,
) -> ApiResult<Json<DaemonTaskResponse>> {
    let repo = DaemonRepo::new(&state.db);
    let (existing, workspace) =
        require_task_access(&state, &auth, &task_id, "task not found").await?;
    let req: TaskCompleteRequest = decode_body(&body)?;

    // 所有字符串列（`error` / `work_dir` / `durable_work_dir` / `branch_name` /
    // `session_id`）都是 TEXT，`result` 是 JSONB —— 都不接受 U+0000。不洗净会让
    // 整个完成事务回滚，任务永远停在 `running`（GH #7098）。
    let req = SanitizedComplete::from(req);

    // 上游把**整个**请求 marshal 进 `result` JSONB（`json.Marshal(req)`）：4 个无
    // `omitempty` 的字段恒在（即使为空串），4 个带 `omitempty` 的字段空则省略。
    let result = json!(CompleteResultPayload {
        pr_url: &req.pr_url,
        output: &req.output,
        session_id: &req.session_id,
        work_dir: &req.work_dir,
        durable_work_dir: &req.durable_work_dir,
        branch_name: &req.branch_name,
        session_rollout_missing: req.session_rollout_missing,
        retired_session_id: &req.retired_session_id,
    });
    let row = repo
        .complete_task(
            existing.id(),
            &result,
            opt(req.session_id),
            opt(req.work_dir),
            opt(req.branch_name),
            req.session_rollout_missing,
            opt(req.retired_session_id),
            opt(req.durable_work_dir),
        )
        .await
        .map_err(db_err)?;

    // CAS 未命中（行已被别的路径终结 / 已不处于 `running`）不是错误：上游此时
    // 重读当前行并按 200 返回，完成回调是幂等的。
    let row = match row {
        Some(row) => row,
        None => repo
            .task_by_id(existing.id())
            .await
            .map_err(db_err)?
            .ok_or_else(|| not_found("task not found"))?,
    };

    // 立刻作废 claim 时签的 `mat_` token：24h 过期与级联删除是持久兜底，
    // 但急着删能让「任务结束后被攻陷的 agent 进程还能调 API」的窗口尽量小。
    if let Err(e) = repo.delete_task_tokens_by_task(row.id()).await {
        tracing::warn!(task_id = %row.id(), error = %e, "complete task: failed to revoke task tokens");
    }
    Ok(Json(DaemonTaskResponse::from_row(
        &row,
        &workspace.to_string(),
    )))
}

/// `POST /api/daemon/tasks/:taskId/fail`。
pub(crate) async fn fail_task(
    State(state): State<Arc<AppState>>,
    auth: DaemonAuth,
    Path(task_id): Path<String>,
    body: Bytes,
) -> ApiResult<Json<DaemonTaskResponse>> {
    let repo = DaemonRepo::new(&state.db);
    let (existing, workspace) =
        require_task_access(&state, &auth, &task_id, "task not found").await?;
    let req: TaskFailRequest = decode_body(&body)?;
    let req = SanitizedFail::from(req);

    let row = repo
        .fail_task(
            existing.id(),
            opt(req.error),
            opt(req.failure_reason),
            opt(req.session_id),
            opt(req.work_dir),
            opt(req.durable_work_dir),
            opt(req.branch_name),
            req.session_rollout_missing,
            opt(req.retired_session_id),
        )
        .await
        .map_err(|e| {
            // 终态事务失败是**基础设施**故障，不是坏请求：daemon 的终态回调把 400
            // 当永久失败直接放弃，回 5xx 才能让它重试到「失败恰好落账一次」。
            tracing::warn!(task_id = %task_id, error = %e, "fail task failed");
            internal(e.to_string())
        })?;

    let row = match row {
        Some(row) => row,
        None => repo
            .task_by_id(existing.id())
            .await
            .map_err(db_err)?
            .ok_or_else(|| not_found("task not found"))?,
    };
    if let Err(e) = repo.delete_task_tokens_by_task(row.id()).await {
        tracing::warn!(task_id = %row.id(), error = %e, "fail task: failed to revoke task tokens");
    }
    Ok(Json(DaemonTaskResponse::from_row(
        &row,
        &workspace.to_string(),
    )))
}

/// upstream `json.Marshal(req)` 的形状（`TaskCompleteRequest`）：无 `omitempty` 的
/// 字段恒在，其余空则省略。
#[derive(serde::Serialize)]
struct CompleteResultPayload<'a> {
    pr_url: &'a str,
    output: &'a str,
    session_id: &'a str,
    work_dir: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    durable_work_dir: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    branch_name: &'a str,
    #[serde(skip_serializing_if = "is_false")]
    session_rollout_missing: bool,
    #[serde(skip_serializing_if = "str::is_empty")]
    retired_session_id: &'a str,
}

/// `skip_serializing_if` 判据：Go 的 `omitempty` 对 `false` 同样省略。
///
/// 签名由 serde 定死（它只传 `&T`）。
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(value: &bool) -> bool {
    !*value
}

/// 洗净后的 `complete` 入参（字段与 [`TaskCompleteRequest`] 一一对应）。
struct SanitizedComplete {
    pr_url: String,
    output: String,
    session_id: String,
    work_dir: String,
    durable_work_dir: String,
    branch_name: String,
    session_rollout_missing: bool,
    retired_session_id: String,
}

impl From<TaskCompleteRequest> for SanitizedComplete {
    fn from(req: TaskCompleteRequest) -> Self {
        Self {
            pr_url: sanitize(&req.pr_url),
            output: sanitize(&req.output),
            session_id: sanitize(&req.session_id),
            work_dir: sanitize(&req.work_dir),
            durable_work_dir: sanitize(&req.durable_work_dir),
            branch_name: sanitize(&req.branch_name),
            session_rollout_missing: req.session_rollout_missing,
            retired_session_id: sanitize(&req.retired_session_id),
        }
    }
}

/// 洗净后的 `fail` 入参。
struct SanitizedFail {
    error: String,
    session_id: String,
    work_dir: String,
    durable_work_dir: String,
    failure_reason: String,
    branch_name: String,
    session_rollout_missing: bool,
    retired_session_id: String,
}

impl From<TaskFailRequest> for SanitizedFail {
    fn from(req: TaskFailRequest) -> Self {
        Self {
            error: sanitize(&req.error),
            session_id: sanitize(&req.session_id),
            work_dir: sanitize(&req.work_dir),
            durable_work_dir: sanitize(&req.durable_work_dir),
            failure_reason: sanitize(&req.failure_reason),
            branch_name: sanitize(&req.branch_name),
            session_rollout_missing: req.session_rollout_missing,
            retired_session_id: sanitize(&req.retired_session_id),
        }
    }
}

// ---------------------------------------------------------------------------
// usage
// ---------------------------------------------------------------------------

/// `POST /api/daemon/tasks/:taskId/usage` —— 一律 200 `{"status":"ok"}`。
///
/// 单条 upsert 失败只 `warn` + `continue`：用量是旁路观测，不该让一条模型行的
/// 写失败变成任务回调的 5xx（daemon 会重试整批，但它永远修不好那条行）。
pub(crate) async fn report_usage(
    State(state): State<Arc<AppState>>,
    auth: DaemonAuth,
    Path(task_id): Path<String>,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    let repo = DaemonRepo::new(&state.db);
    let (task, _workspace) = require_task_access(&state, &auth, &task_id, "task not found").await?;
    let req: TaskUsageRequest = decode_body(&body)?;

    // provider 统一小写：客户端按 provider 做定价匹配，大小写漂移会让它落到 $0。
    // 空 provider（老 daemon 不带该字段）从 task 的 runtime 回填，这样 `auto`
    // 这类通用模型名仍能解析出 provider，而不是落成 ''。
    let mut runtime_provider: Option<String> = None;
    let mut runtime_provider_loaded = false;

    for u in &req.usage {
        let mut provider = normalize_provider(&u.provider);
        if provider.is_empty() {
            if !runtime_provider_loaded {
                runtime_provider = match task.runtime_id {
                    Some(runtime_id) => repo
                        .runtime_by_id(Id::from(runtime_id))
                        .await
                        .ok()
                        .flatten()
                        .map(|rt| normalize_provider(&rt.provider))
                        .filter(|p| !p.is_empty()),
                    None => None,
                };
                runtime_provider_loaded = true;
            }
            provider = runtime_provider.clone().unwrap_or_default();
        }
        let upsert = TaskUsageUpsert {
            task_id: task.id(),
            provider,
            model: u.model.clone(),
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cache_read_tokens: u.cache_read_tokens,
            cache_write_tokens: u.cache_write_tokens,
            // 只有正数是权威值：0 是「不知道」，落库就会声称一笔真实的 $0 花费
            // 并压掉费率表估算；负数是畸形上报。
            cost_usd_ticks: (u.cost_usd_ticks > 0).then_some(u.cost_usd_ticks),
        };
        if let Err(e) = repo.upsert_task_usage(&upsert).await {
            tracing::warn!(task_id = %task.id(), model = %u.model, error = %e, "upsert task usage failed");
        }
    }
    Ok(Json(json!({ "status": "ok" })))
}

// ---------------------------------------------------------------------------
// cancel-ack / session
// ---------------------------------------------------------------------------

/// `POST /api/daemon/tasks/:taskId/cancel-ack` —— body 尽力而为，一律 200 `{"status":"ok"}`。
///
/// 四条写入合并成**一条**语句（`COALESCE` 永不覆盖 + `status='cancelled'` CAS），
/// 所以「哪些字段有值」不影响结果，全部为空时也是一次无副作用的 no-op。
/// 落库失败必须指名失败面并回 500：daemon 会重试这次 ack，而这几个字段是已取消
/// 任务产出物的唯一指针 —— 一次 DB 抖动不能变成永久找不到分支。
pub(crate) async fn ack_cancelled(
    State(state): State<Arc<AppState>>,
    auth: DaemonAuth,
    Path(task_id): Path<String>,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    let repo = DaemonRepo::new(&state.db);
    let (task, _workspace) = require_task_access(&state, &auth, &task_id, "task not found").await?;
    // 解码失败不阻断：老 daemon 发 `{}`，而「取消契约」比字段完整更重要。
    let req: AckTaskCancelledRequest = decode_body(&body).unwrap_or_default();

    let branch_name = trimmed(opt(sanitize(&req.branch_name)));
    let durable_work_dir = trimmed(opt(sanitize(&req.durable_work_dir)));
    let error = trimmed(opt(sanitize(&req.error_message)));
    let failure_reason = trimmed(opt(sanitize(&req.failure_reason)));

    if let Err(e) = repo
        .ack_task_cancelled(
            task.id(),
            branch_name,
            durable_work_dir,
            error,
            failure_reason,
        )
        .await
    {
        tracing::error!(task_id = %task.id(), error = %e, "cancel ack: record preserved work failed");
        return Err(internal("failed to record cancelled task fields"));
    }
    Ok(Json(json!({ "status": "ok" })))
}

/// trim 后仍为空 → `None`（上游 `strings.TrimSpace(x) != ""` 的判据）。
fn trimmed(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// `POST /api/daemon/tasks/:taskId/session` —— **204 No Content**。
pub(crate) async fn pin_session(
    State(state): State<Arc<AppState>>,
    auth: DaemonAuth,
    Path(task_id): Path<String>,
    body: Bytes,
) -> ApiResult<StatusCode> {
    let repo = DaemonRepo::new(&state.db);
    let (task, _workspace) = require_task_access(&state, &auth, &task_id, "task not found").await?;
    let req: PinSessionRequest = decode_body(&body)?;
    if req.session_id.is_empty() && req.work_dir.is_empty() {
        return Err(validation("session_id or work_dir required"));
    }
    // 只填空槽、绝不覆盖（`UpdateAgentTaskSession` 的 COALESCE）。0 行受影响不是
    // 错误：pin 是异步的，任务可能已被取消或早已终结 —— 上游同样静默 204。
    repo.pin_task_session(
        task.id(),
        opt(sanitize(&req.session_id)),
        opt(sanitize(&req.work_dir)),
    )
    .await
    .map_err(db_err)?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// 插件面（恒禁用）
// ---------------------------------------------------------------------------

/// upstream `requirePluginsV1` 的降级形态：插件面恒关闭 ⇒ 403。
///
/// 与上游同码（`plugin_api_disabled`），只是套进本仓的嵌套错误信封
/// （`{"error":{"code","message"}}`，登记在 `docs/32`）。
fn plugin_api_disabled() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({
            "error": {
                "code": "plugin_api_disabled",
                "message": "Plugin management is not enabled",
            }
        })),
    )
        .into_response()
}

/// `POST /api/daemon/tasks/:taskId/plugin-hooks`。
///
/// `requirePluginsV1` 在**任务鉴权之前**执行，所以本仓的响应与上游完全一致：
/// 403，且不泄露该 task 是否存在。
pub(crate) async fn plugin_hooks(
    State(_state): State<Arc<AppState>>,
    _auth: DaemonAuth,
    Path(_task_id): Path<String>,
    _body: Bytes,
) -> Response {
    plugin_api_disabled()
}

/// `GET /api/daemon/tasks/:taskId/plugin-mcp/:contributionId/credential`。
pub(crate) async fn plugin_mcp_credential(
    State(_state): State<Arc<AppState>>,
    _auth: DaemonAuth,
    Path((_task_id, _contribution_id)): Path<(String, String)>,
) -> Response {
    plugin_api_disabled()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_absolute_path_wait_reason_is_dropped() {
        assert_eq!(sanitize_wait_reason("local_directory /Users/x/proj"), "");
        assert_eq!(sanitize_wait_reason(r"local_directory \\host\share"), "");
        assert_eq!(sanitize_wait_reason(r"local_directory C:\proj"), "");
        assert_eq!(sanitize_wait_reason("local_directory C:/proj"), "");
    }

    #[test]
    fn genuine_directory_name_and_holder_clause_survive() {
        // 目录真叫 `local_directory`，或被占位者是另一台任务：这是用户自己的标签。
        assert_eq!(sanitize_wait_reason("local_directory"), "local_directory");
        assert_eq!(
            sanitize_wait_reason("local_directory (held by task abc12345)"),
            "local_directory (held by task abc12345)"
        );
        // 非 `local_directory` 前缀的路径是合法原因（例如纯文本提示）。
        assert_eq!(sanitize_wait_reason(" /tmp/x "), "/tmp/x");
    }
}
