//! M5-4：autopilot **执行面**（手工触发 / runs 读面）。
//!
//! - **写者**：M5-4（`docs/44` §3.2）。切片只实现本文件的 `router()`；deliveries 三条在
//!   `../delivery.rs`（同片）。
//! - **路由**（`router.go` L2115–L2117）：
//!
//! | # | 方法 | 路径 | 上游 handler | span |
//! | ---: | --- | --- | --- | ---: |
//! | 15 | POST | `/api/autopilots/:id/trigger` | `TriggerAutopilot` | 73 (+15) |
//! | 16 | GET | `/api/autopilots/:id/runs` | `ListAutopilotRuns` | 50 |
//! | 17 | GET | `/api/autopilots/:id/runs/:runId` | `GetAutopilotRun` | 44 |
//!
//! - 三条都是单形态 ⇒ **不要**加尾斜杠别名。
//! - **#15 走与调度同一条派发路径**（[`mc_autopilot::dispatch`]），只是计划时间 = 现在 ⇒
//!   不在路由层另写一套「手工触发」逻辑，否则幂等/跳过（`shouldSkipDispatch`）会分叉。
//!
//! # 三个 handler 各自「上什么闸」（上游并不对称，别顺手统一）
//!
//! | # | 成员门槛 | autopilot 加载 | 写权 |
//! | --- | --- | --- | --- |
//! | 15 | ✔ | ✔ | ✔（`requireAutopilotTriggerInvoker`） |
//! | 16 | ✔ | ✔ | ✘ |
//! | 17 | ✔ | ✔ | ✘ |
//!
//! 读面没有写权闸：**任何工作区成员**都能看到 runs（与 M5-1 的读面同口径）。#15 的写权闸是
//! `requireAutopilotTriggerInvoker` —— 上游注明它刻意**不**叠加 `requireAutopilotWrite`
//! （判的是同一个人，要两次授权等于要两个不相干的人），本地因此复用 M5-3 的
//! [`super::trigger::resolve_write_scope`]（成员 → 加载 → 写权），而不是再写一条链。
//!
//! ## #15 的判负顺序是契约
//!
//! 上游把授权放在状态检查**之前**（注释逐字：未授权调用者不该能分辨「未激活」与「已激活」）。
//! 本地逐字保留：`resolve_write_scope`（400/404/403）→ `status != "active"`（400）→
//! `Idempotency-Key` 长度（400）→ 派发。
//!
//! ## #15 的 429 非法响应体
//!
//! 配额拦下时上游写的**不是**本仓标准错误体，而是 `{reason_code, used, reserved, limit,
//! reset_at}` 四数字 + 一个时间戳，外加 `Retry-After` 头（秒，向上取整到 ≥1）。前端按
//! `reason_code == "quota_exceeded"` 分支弹「额度用尽」 ⇒ 这条形状必须逐字保留，
//! 不能折成 `ApiError`。
//!
//! ## 500 的两种折法（有意不对称）
//!
//! - **#15**：上游在这里写死了文案 `failed to trigger autopilot`，并在注释里给了理由
//!   （MUL-6472：任何工作区成员都能走到这里，而 `pgx` 的错误链带约束名/内部 id）⇒ 本地
//!   同样只回固定文案，真实错误进 `tracing::error`。
//! - **#16/#17**：读面沿用 M5-1/全仓的 `repo_err`（`database_error`），与 `list.rs` 同口径。
//!
//! # runs 的响应形状
//!
//! 列表用 `runToResponseSlim`（**丢掉 `trigger_payload`**）：webhook 信封可达 256 KiB、
//! `limit` 默认 20 ⇒ 列表最坏 ~5 MiB 全是随即被 JSON 编码器丢掉的字节。详情才发全量载荷。
//! 两条共用一个投影函数，只有 `trigger_payload` 一处差别（上游 `slim` 就是「全量置 nil」）。

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use uuid::Uuid;

use mc_autopilot::dispatch::{AutopilotDispatcher, DispatchError, DispatchRequest};
use mc_autopilot::dto::AutopilotRunResponse;
use mc_core::Timestamp;
use mc_errors::Error;
use mc_repos::autopilot::run::{self as run_sql, AutopilotRunRow};
use mc_repos::autopilot::AutopilotRepo;

use crate::error::ApiResult;
use crate::routes::agents::{bad_request, not_found, parse_uuid, repo_err};
use crate::routes::auth_user::AuthUser;
use crate::routes::autopilots::access::{acting_user_id, load_in_workspace, require_member};
use crate::routes::inbox::resolve_workspace_id;
use crate::state::AppState;

/// runs / deliveries 两条读面共用的分页解析（上游两处逐字相同）。
///
/// 三态逐字对照 Go 的 `strconv.Atoi`：
///
/// - 缺省 / 解析失败 / `<= 0` ⇒ **保持默认**（`limit=20`、`offset=0`）；
/// - `limit > 100` ⇒ 夹到 100（**不是** 400）；
/// - `offset` 只接受 `>= 0`。
///
/// 刻意**不 trim**：Go 的 `Atoi(" 12")` 失败 ⇒ 回默认值，本地 `str::parse` 同样失败。
pub(super) fn parse_limit_offset(query: &HashMap<String, String>) -> (i64, i64) {
    let mut limit: i64 = 20;
    if let Some(raw) = query.get("limit") {
        if let Ok(value) = raw.parse::<i32>() {
            if value > 0 {
                limit = i64::from(value);
            }
        }
    }
    if limit > 100 {
        limit = 100;
    }
    let mut offset: i64 = 0;
    if let Some(raw) = query.get("offset") {
        if let Ok(value) = raw.parse::<i32>() {
            if value >= 0 {
                offset = i64::from(value);
            }
        }
    }
    (limit, offset)
}

/// execution 面 router（3 条路由 / 3 个注册键，全部单形态）。
pub fn router() -> Router<Arc<AppState>> {
    // 路径参数一律 `:id` / `:runId`（matchit 0.7；`{id}` 会被当字面量段 ⇒ 恒 404）。
    Router::new()
        .route("/api/autopilots/:id/trigger", post(trigger_autopilot))
        .route("/api/autopilots/:id/runs", get(list_autopilot_runs))
        .route("/api/autopilots/:id/runs/:runId", get(get_autopilot_run))
}

/// `runToResponse`（`handler/autopilot.go:319`）：run 行 → wire 形状。
///
/// `trigger_payload` / `result` 是 `jsonb`：上游对**非 NULL** 的裸字节做一次
/// `json.Unmarshal` 再回发（等于原样透出）。本地行结构已经把这两列解成
/// `Option<serde_json::Value>` ⇒ 直接搬运，NULL 保持 `null`。
#[must_use]
pub(super) fn run_to_response(run: &AutopilotRunRow) -> AutopilotRunResponse {
    AutopilotRunResponse {
        id: run.id,
        autopilot_id: run.autopilot_id,
        trigger_id: run.trigger_id,
        source: run.source.clone(),
        status: run.status.clone(),
        issue_id: run.issue_id,
        task_id: run.task_id,
        triggered_at: Timestamp::from(run.triggered_at),
        completed_at: run.completed_at.map(Timestamp::from),
        failure_reason: run.failure_reason.clone(),
        reason_code: run.reason_code.clone(),
        trigger_payload: run.trigger_payload.clone(),
        result: run.result.clone(),
        created_at: Timestamp::from(run.created_at),
    }
}

/// `runToResponseSlim`（`autopilot.go:351`）：全量投影 + `trigger_payload = null`。
#[must_use]
pub(super) fn run_to_response_slim(run: &AutopilotRunRow) -> AutopilotRunResponse {
    let mut resp = run_to_response(run);
    resp.trigger_payload = None;
    resp
}

/// 列表信封：`{"runs":[...],"total":N}`（`total` = **本页长度**，不是全表计数）。
#[derive(Debug, Serialize)]
struct RunListEnvelope {
    runs: Vec<AutopilotRunResponse>,
    total: usize,
}

/// 配额拦下时的非法响应体（见模块文档）。
///
/// 五个字段**都没有** `skip_serializing_if`：上游这里恒发全部字段（`reset_at` 一定是
/// RFC3339，不是 `null`）。
#[derive(Debug, Serialize)]
struct QuotaExceededBody {
    reason_code: &'static str,
    used: i64,
    reserved: i64,
    limit: i64,
    reset_at: String,
}

/// `POST /api/autopilots/:id/trigger`（上游 `TriggerAutopilot`，成功 **200**）。
///
/// # Errors
///
/// 400（非 UUID / `autopilot is not active` / `Idempotency-Key` 超长）、403（无写权）、
/// 404（跨工作区 / 不存在）、**429**（配额）、500（派发失败，固定文案）。
async fn trigger_autopilot(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    // 成员 → 加载 → 写权（顺序即契约，见模块文档）。
    let scope = super::trigger::resolve_write_scope(&state, user, &headers, &query, &id).await?;
    let autopilot = scope.autopilot;

    // 授权**先于**状态检查：未授权者不该能分辨未激活与已激活。
    if autopilot.status != "active" {
        return Err(bad_request("autopilot is not active").into());
    }

    let idempotency_key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .trim()
        .to_string();
    if idempotency_key.len() > 255 {
        return Err(bad_request("Idempotency-Key is too long").into());
    }
    let idempotency_key = if idempotency_key.is_empty() {
        // 上游 `service.NewRequestIdempotencyKey()`：本地等价物就是一枚新 UUID。
        format!("req-{}", Uuid::new_v4())
    } else {
        idempotency_key
    };

    let dispatcher =
        AutopilotDispatcher::new(state.db.pool().clone()).with_events(state.realtime.clone());
    let req = DispatchRequest::manual_with_key(
        &autopilot,
        None,
        None,
        Some(scope.user_id.0),
        &idempotency_key,
    );
    let outcome = match dispatcher.dispatch(req).await {
        Ok(outcome) => outcome,
        Err(DispatchError::QuotaExceeded {
            used,
            reserved,
            limit,
            reset_at,
        }) => {
            let retry_after = (reset_at - chrono::Utc::now()).num_seconds().max(1);
            let mut response = Json(QuotaExceededBody {
                reason_code: "quota_exceeded",
                used,
                reserved,
                limit,
                // 上游 `ResetAt.UTC().Format(time.RFC3339)`：秒精度 + `Z`。
                reset_at: reset_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            })
            .into_response();
            *response.status_mut() = StatusCode::TOO_MANY_REQUESTS;
            if let Ok(value) = HeaderValue::from_str(&retry_after.to_string()) {
                response.headers_mut().insert(header::RETRY_AFTER, value);
            }
            return Ok(response);
        }
        Err(err) => {
            // 固定文案 + 真实错误只进日志（MUL-6472，见模块文档）。
            tracing::error!(
                error = %err,
                autopilot_id = %autopilot.id,
                "trigger autopilot failed"
            );
            return Err(Error::Internal("failed to trigger autopilot".to_string()).into());
        }
    };

    // 准入原因码（源头决定的那个）直接进响应：UI 按 `status` + `reason_code` 分支弹 toast。
    let mut resp = run_to_response(&outcome.run);
    if let Some(code) = outcome.reason_code {
        resp.reason_code = Some(code.as_str().to_string());
    }
    Ok((StatusCode::OK, Json(resp)).into_response())
}

/// `GET /api/autopilots/:id/runs`（上游 `ListAutopilotRuns` 50 行）。
async fn list_autopilot_runs(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(id): Path<String>,
) -> ApiResult<Json<RunListEnvelope>> {
    let workspace_id = resolve_workspace_id(&headers, &query)?;
    let user_id = acting_user_id(user);
    require_member(&state, workspace_id, user_id).await?;
    let repo = AutopilotRepo::new(state.db.clone());
    let autopilot = load_in_workspace(&repo, &id, workspace_id).await?;

    let (limit, offset) = parse_limit_offset(&query);
    let runs = run_sql::list(state.db.pool(), autopilot.id, limit, offset)
        .await
        .map_err(|err| repo_err(err, "autopilot run"))?;

    let resp = runs.iter().map(run_to_response_slim).collect::<Vec<_>>();
    Ok(Json(RunListEnvelope {
        total: resp.len(),
        runs: resp,
    }))
}

/// `GET /api/autopilots/:id/runs/:runId`（上游 `GetAutopilotRun` 44 行）。
///
/// run 的 workspace 归属**经 autopilot 重查一遍**（上游注释逐字）：只靠 URL 里的
/// autopilot id 不够，否则别的 autopilot 下的 runId 猜中了就能读到。
async fn get_autopilot_run(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path((id, run_id)): Path<(String, String)>,
) -> ApiResult<Json<AutopilotRunResponse>> {
    let workspace_id = resolve_workspace_id(&headers, &query)?;
    let user_id = acting_user_id(user);
    require_member(&state, workspace_id, user_id).await?;
    let repo = AutopilotRepo::new(state.db.clone());
    let autopilot = load_in_workspace(&repo, &id, workspace_id).await?;

    let run_uuid = parse_uuid(&run_id, "run id")?;
    // 上游把「不存在」与「属于别的 autopilot」折成同一个 404（不泄露存在性）。
    let run = match run_sql::get(state.db.pool(), run_uuid).await {
        Ok(run) if run.autopilot_id == autopilot.id => run,
        Ok(_) | Err(mc_repos::RepoError::NotFound) => return Err(not_found("run").into()),
        Err(err) => return Err(repo_err(err, "autopilot run").into()),
    };
    Ok(Json(run_to_response(&run)))
}
