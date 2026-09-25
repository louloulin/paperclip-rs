//! M5-4：webhook **投递**读面 + replay。
//!
//! - **写者**：M5-4（`docs/44` §3.2）。切片只实现本文件的 `router()`。
//! - **路由**（`router.go` L2118–L2120）：
//!
//! | # | 方法 | 路径 | 上游 handler | span |
//! | ---: | --- | --- | --- | ---: |
//! | 18 | GET | `/api/autopilots/:id/deliveries` | `ListAutopilotDeliveries` | 48 |
//! | 19 | GET | `/api/autopilots/:id/deliveries/:deliveryId` | `GetAutopilotDelivery` | 28 |
//! | 20 | POST | `/api/autopilots/:id/deliveries/:deliveryId/replay` | `ReplayAutopilotDelivery` | 117 |
//!
//! - 三条都是**单形态** ⇒ 不要加尾斜杠别名。
//! - **#18/#19 只上成员闸**；**#20 还上写权闸**（上游 `requireAutopilotWrite`）：replay 会
//!   产生一条新投递 = 一次新的「让 agent 干活」，所以它比读面高一格。三条共用
//!   `loadDeliveryForAutopilot` 的「**跨 autopilot 的 deliveryId 一律 404**」判负
//!   （上游注释逐字：ID 猜中也不能读到、更不能 replay 别人的投递）。
//!
//! # 列表 / 详情的投影差别（上游 `slimDeliveryToResponse` / `deliveryToResponse`）
//!
//! 列表刻意丢掉 `raw_body`（≤256 KiB/行）与 `selected_headers` / `response_body`；
//! 详情才取整行。本地两个行结构（`WebhookDeliverySlimRow` 23 列 / `WebhookDeliveryRow`
//! 28 列）已经把这个差别钉在 SQL 层，本文件只做「瘦行填不满的三列」的取舍。
//!
//! # `WebhookDeliveryResponse` 为什么定义在本文件
//!
//! `../dto.rs` 是 M5-1 的写集（`docs/44` §3.2 的单写者规则）⇒ 本切片的响应契约放在自己的
//! 路由文件里（与 `mc-autopilot::dto::AutopilotRunResponse` 那种跨片共享的形状不同，
//! 这个形状只有本文件的三条路由用）。
//!
//! # replay 的四处**有意偏离**（逐条见 `docs/52`）
//!
//! 1. **不重新归一化事件名**：上游用 `normalizeWebhookPayload` + 事件推断从 `raw_body`
//!    重推 `event`；那对函数属 M5-5 的入站面。本地沿用**原投递的 `event`**（同一 body 的
//!    事件名只可能由同一份归一化逻辑得出），但**保留**「`raw_body` 必须仍是合法 JSON」的
//!    校验，以维持上游 `stored body no longer parses: …` 的 400 契约。
//! 2. **不再登记为缺口（M5-D8 / `LUM-1745` 补上）**：上游最后 `h.WebhookDeliveryWorker.Notify()`
//!    —— 这一处现在与其它三处入站触发点一样调 `webhook_notify_port()`（`mc-http` 的进程级槽，
//!    宿主 `apps/mc-server/src/webhook_worker.rs` 注入）。**只在真正新建 replay 行的那条出口**提示
//!    （上游的幂等命中出口提前 `return` 了，不提示）；即便提示丢掉，worker 的 `1s` ticker 也会
//!    把这一行扫走（`docs/32` §27）。
//! 3. **replay 行的 id 是 v4**：上游 `dbid.NewV7()`，仓库既有行 id 取自 `uuid::Uuid::new_v4()`
//!    （`mc-repos` 全部写面同口径）。
//! 4. **500 折法**：上游三条查询各自一条固定文案（`failed to list deliveries` /
//!    `failed to load delivery` / `failed to create replay delivery` / `failed to inspect
//!    replay request`）。本地 `list`/`get_in_workspace` 走全仓 `repo_err`
//!    （`database_error`，M5-1 读面同口径）；只剩 replay 的两条**写路径**保留上游固定文案
//!    （不把约束名透给成员）。

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use mc_core::Timestamp;
use mc_errors::Error;
use mc_repos::autopilot::delivery::{
    self as delivery_sql, NewReplayDelivery, WebhookDeliveryRow, WebhookDeliverySlimRow,
};
use mc_repos::autopilot::trigger::AutopilotTriggerRepo;
use mc_repos::autopilot::AutopilotRepo;
use mc_repos::RepoError;

use crate::error::ApiResult;
use crate::routes::agents::{bad_request, not_found, parse_uuid, repo_err};
use crate::routes::auth_user::AuthUser;
use crate::routes::autopilots::access::{acting_user_id, load_in_workspace, require_member};
use crate::routes::autopilots::execution::parse_limit_offset;
use crate::routes::inbox::resolve_workspace_id;
use crate::state::AppState;

/// delivery 面 router（3 条路由 / 3 个注册键，全部单形态）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/autopilots/:id/deliveries",
            get(list_autopilot_deliveries),
        )
        .route(
            "/api/autopilots/:id/deliveries/:deliveryId",
            get(get_autopilot_delivery),
        )
        .route(
            "/api/autopilots/:id/deliveries/:deliveryId/replay",
            post(replay_autopilot_delivery),
        )
}

/// 上游 `WebhookDeliveryResponse`（`handler/webhook_delivery.go:26`）。
///
/// 三个**详情专属**字段带 `omitempty`（列表形态整个字段缺席，不是 `null`）：这是上游
/// `json.RawMessage` / `*string` 的 `omitempty` 语义，本地用 `skip_serializing_if`。
/// 其余字段**不带** `omitempty` ⇒ 值缺失时显式 `null`。
#[derive(Debug, Serialize)]
pub(super) struct WebhookDeliveryResponse {
    id: Uuid,
    workspace_id: Uuid,
    autopilot_id: Uuid,
    trigger_id: Uuid,
    provider: String,
    event: String,
    dedupe_key: Option<String>,
    dedupe_source: Option<String>,
    signature_status: String,
    status: String,
    attempt_count: i32,
    dispatch_attempts: i32,
    available_at: Timestamp,
    content_type: Option<String>,
    response_status: Option<i32>,
    autopilot_run_id: Option<Uuid>,
    replayed_from_delivery_id: Option<Uuid>,
    error: Option<String>,
    reason_code: Option<String>,
    replay_idempotency_key: Option<String>,
    received_at: Timestamp,
    last_attempt_at: Timestamp,
    created_at: Timestamp,
    /// `selected_headers`（**仅详情**；列表形态缺席）。
    #[serde(skip_serializing_if = "Option::is_none")]
    selected_headers: Option<Value>,
    /// `raw_body`（**仅详情**；UTF-8 化的原文）。
    #[serde(skip_serializing_if = "Option::is_none")]
    raw_body: Option<String>,
    /// `response_body`（**仅详情**）。
    #[serde(skip_serializing_if = "Option::is_none")]
    response_body: Option<String>,
}

impl WebhookDeliveryResponse {
    /// `slimDeliveryToResponse`（`handler/webhook_delivery.go:61`）：列表行（23 列）→ wire 形状。
    ///
    /// 三个详情专属字段保持缺席（`None` + `skip_serializing_if` ⇒ 字段整个不出现，
    /// 与上游 `omitempty` 一致）。
    #[must_use]
    fn from_slim(row: &WebhookDeliverySlimRow) -> Self {
        Self {
            id: row.id,
            workspace_id: row.workspace_id,
            autopilot_id: row.autopilot_id,
            trigger_id: row.trigger_id,
            provider: row.provider.clone(),
            event: row.event.clone(),
            dedupe_key: row.dedupe_key.clone(),
            dedupe_source: row.dedupe_source.clone(),
            signature_status: row.signature_status.clone(),
            status: row.status.clone(),
            attempt_count: row.attempt_count,
            dispatch_attempts: row.dispatch_attempts,
            available_at: Timestamp::from(row.available_at),
            content_type: row.content_type.clone(),
            response_status: row.response_status,
            autopilot_run_id: row.autopilot_run_id,
            replayed_from_delivery_id: row.replayed_from_delivery_id,
            error: row.error.clone(),
            reason_code: row.reason_code.clone(),
            replay_idempotency_key: row.replay_idempotency_key.clone(),
            received_at: Timestamp::from(row.received_at),
            last_attempt_at: Timestamp::from(row.last_attempt_at),
            created_at: Timestamp::from(row.created_at),
            selected_headers: None,
            raw_body: None,
            response_body: None,
        }
    }

    /// `deliveryToResponse(d, detail)`（`webhook_delivery.go:102`）：整行（28 列）→ wire 形状。
    ///
    /// 与上面逐字段相同的部分**故意重复一遍**（不抽 23 参数的公共构造器）：上游就是两个
    /// 函数，重复的只是搬运；抽公共参数列反而会把「两处必须一致」变成「一个 23 参调用点」，
    /// 那既过不了 `clippy::too_many_arguments`，也让 diff 里看不出哪一列漏了。
    #[must_use]
    fn from_full(row: &WebhookDeliveryRow, detail: bool) -> Self {
        let raw_body = if detail {
            // `raw_body` 是 `bytea`：上游 `string(d.RawBody)` 直接当 UTF-8 发（空数组 = 缺席）。
            row.raw_body
                .as_deref()
                .filter(|raw| !raw.is_empty())
                .map(|raw| String::from_utf8_lossy(raw).into_owned())
        } else {
            None
        };
        Self {
            id: row.id,
            workspace_id: row.workspace_id,
            autopilot_id: row.autopilot_id,
            trigger_id: row.trigger_id,
            provider: row.provider.clone(),
            event: row.event.clone(),
            dedupe_key: row.dedupe_key.clone(),
            dedupe_source: row.dedupe_source.clone(),
            signature_status: row.signature_status.clone(),
            status: row.status.clone(),
            attempt_count: row.attempt_count,
            dispatch_attempts: row.dispatch_attempts,
            available_at: Timestamp::from(row.available_at),
            content_type: row.content_type.clone(),
            response_status: row.response_status,
            autopilot_run_id: row.autopilot_run_id,
            replayed_from_delivery_id: row.replayed_from_delivery_id,
            error: row.error.clone(),
            reason_code: row.reason_code.clone(),
            replay_idempotency_key: row.replay_idempotency_key.clone(),
            received_at: Timestamp::from(row.received_at),
            last_attempt_at: Timestamp::from(row.last_attempt_at),
            created_at: Timestamp::from(row.created_at),
            // 上游判的是裸字节长度（`len(d.SelectedHeaders) > 0`）：只有真正空的 jsonb 才缺席。
            selected_headers: if detail && row.selected_headers != Value::Null {
                Some(row.selected_headers.clone())
            } else {
                None
            },
            raw_body,
            response_body: if detail {
                row.response_body.clone()
            } else {
                None
            },
        }
    }
}

/// 列表信封：`{"deliveries":[…],"total":N}`。
#[derive(Debug, Serialize)]
struct DeliveryListEnvelope {
    deliveries: Vec<WebhookDeliveryResponse>,
    total: usize,
}

/// 列表 / 详情的加载链：成员闸 → autopilot 加载 → delivery 加载（跨 autopilot → 404）。
async fn load_delivery_for_autopilot(
    state: &AppState,
    autopilot_id: Uuid,
    workspace_id: Uuid,
    raw_delivery_id: &str,
) -> Result<WebhookDeliveryRow, Error> {
    let delivery_uuid = parse_uuid(raw_delivery_id, "delivery id")?;
    match delivery_sql::get_in_workspace(state.db.pool(), delivery_uuid, workspace_id).await {
        Ok(row) if row.autopilot_id == autopilot_id => Ok(row),
        // 不属于本 autopilot 与「不存在」不可区分（上游注释：防御 ID 猜测）。
        Ok(_) | Err(RepoError::NotFound) => Err(not_found("delivery")),
        Err(err) => Err(repo_err(err, "webhook delivery")),
    }
}

/// `GET /api/autopilots/:id/deliveries`（上游 `ListAutopilotDeliveries` 48 行）。
async fn list_autopilot_deliveries(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(id): Path<String>,
) -> ApiResult<Json<DeliveryListEnvelope>> {
    let workspace_id = resolve_workspace_id(&headers, &query)?;
    let user_id = acting_user_id(user);
    require_member(&state, workspace_id, user_id).await?;
    let repo = AutopilotRepo::new(state.db.clone());
    let autopilot = load_in_workspace(&repo, &id, workspace_id).await?;

    let (limit, offset) = parse_limit_offset(&query);
    let rows = delivery_sql::list(state.db.pool(), autopilot.id, workspace_id.0, limit, offset)
        .await
        .map_err(|err| repo_err(err, "webhook delivery"))?;

    let deliveries = rows
        .iter()
        .map(WebhookDeliveryResponse::from_slim)
        .collect::<Vec<_>>();
    Ok(Json(DeliveryListEnvelope {
        total: deliveries.len(),
        deliveries,
    }))
}

/// `GET /api/autopilots/:id/deliveries/:deliveryId`（上游 `GetAutopilotDelivery` 28 行）。
async fn get_autopilot_delivery(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path((id, delivery_id)): Path<(String, String)>,
) -> ApiResult<Json<WebhookDeliveryResponse>> {
    let workspace_id = resolve_workspace_id(&headers, &query)?;
    let user_id = acting_user_id(user);
    require_member(&state, workspace_id, user_id).await?;
    let repo = AutopilotRepo::new(state.db.clone());
    let autopilot = load_in_workspace(&repo, &id, workspace_id).await?;

    let row =
        load_delivery_for_autopilot(&state, autopilot.id, workspace_id.0, &delivery_id).await?;
    Ok(Json(WebhookDeliveryResponse::from_full(&row, true)))
}

/// `POST /api/autopilots/:id/deliveries/:deliveryId/replay`（上游 `ReplayAutopilotDelivery`）。
///
/// 成功与幂等命中都是 **202**（`Accepted`，不是 201）：上游把它当成「请求已受理，durable
/// worker 负责真正的派发」。
///
/// # Errors
///
/// 400（签名/gate 类判负）、403（无写权）、404（跨 autopilot / 不存在 / trigger 没了）、
/// 500（库错，固定文案）。
async fn replay_autopilot_delivery(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path((id, delivery_id)): Path<(String, String)>,
) -> ApiResult<Response> {
    // replay 是本面唯一的写路径 ⇒ 与 #15 一样走「成员 → 加载 → 写权」。
    let scope = super::trigger::resolve_write_scope(&state, user, &headers, &query, &id).await?;
    let autopilot = scope.autopilot;
    let workspace_id = autopilot.workspace_id;

    let original =
        load_delivery_for_autopilot(&state, autopilot.id, workspace_id, &delivery_id).await?;

    // 判负顺序逐字对照上游（见模块文档的偏离第 1 条）。
    if original.signature_failed() {
        return Err(
            bad_request("cannot replay a delivery that failed signature verification").into(),
        );
    }
    let Some(raw_body) = original.raw_body.clone().filter(|raw| !raw.is_empty()) else {
        return Err(bad_request("original delivery has no raw body to replay").into());
    };
    if autopilot.status != "active" {
        return Err(bad_request("autopilot is not active").into());
    }

    let trigger_repo = AutopilotTriggerRepo::new(state.db.clone());
    let trigger = match trigger_repo.get_by_id(original.trigger_id).await {
        Ok(trigger) => trigger,
        Err(RepoError::NotFound) => return Err(not_found("trigger").into()),
        Err(err) => return Err(repo_err(err, "trigger").into()),
    };
    if !trigger.enabled {
        return Err(bad_request("trigger is disabled").into());
    }

    // `raw_body` 必须仍是合法 JSON（上游逐字文案；**不**重新归一化事件名，见偏离第 1 条）。
    if let Err(err) = serde_json::from_slice::<Value>(&raw_body) {
        return Err(bad_request(format!("stored body no longer parses: {err}")).into());
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
        format!("replay-{}", Uuid::new_v4())
    } else {
        idempotency_key
    };

    // 幂等：同一 (原投递, Idempotency-Key) 已有 replay ⇒ 直接把它回出去（不新建第二行）。
    if let Some(existing) =
        delivery_sql::find_replay(state.db.pool(), original.id, &idempotency_key)
            .await
            .map_err(|err| repo_err(err, "webhook delivery"))?
    {
        return Ok(accepted(&existing));
    }

    let content_type = original.content_type.clone().filter(|raw| !raw.is_empty());
    let new = NewReplayDelivery {
        id: Uuid::new_v4(),
        workspace_id,
        autopilot_id: autopilot.id,
        trigger_id: original.trigger_id,
        provider: original.provider.clone(),
        event: original.event.clone(),
        selected_headers: original.selected_headers.clone(),
        content_type,
        raw_body,
        replayed_from_delivery_id: original.id,
        replay_idempotency_key: idempotency_key.clone(),
    };
    match delivery_sql::create_replay(state.db.pool(), &new).await {
        Ok(replay) => {
            // 上游 `webhook_delivery.go:344` 的 `h.WebhookDeliveryWorker.Notify()`：新行已落
            // `queued`，叫 worker 来收。幂等命中 / 并发冲突回读那两条出口**不**提示（上游同款：
            // 那两条在 `Notify()` 之前就 `return` 了）。
            crate::routes::webhooks::autopilots::notify_webhook_worker();
            Ok(accepted(&replay))
        }
        // 并发重放（唯一索引抢先）⇒ 读回那一行，语义与幂等命中相同。
        Err(RepoError::Conflict) => {
            match delivery_sql::find_replay(state.db.pool(), original.id, &idempotency_key).await {
                Ok(Some(existing)) => Ok(accepted(&existing)),
                Ok(None) | Err(_) => {
                    Err(Error::Internal("failed to create replay delivery".to_string()).into())
                }
            }
        }
        Err(err) => {
            // 固定文案 + 真实错误只进日志（与 #15 同口径）。
            tracing::error!(
                error = %err,
                original_delivery_id = %original.id,
                "replay: insert delivery failed"
            );
            Err(Error::Internal("failed to create replay delivery".to_string()).into())
        }
    }
}

/// 202 + 详情形态（上游 replay 两条出口都是 `deliveryToResponse(replay, true)`）。
fn accepted(row: &WebhookDeliveryRow) -> Response {
    (
        StatusCode::ACCEPTED,
        Json(WebhookDeliveryResponse::from_full(row, true)),
    )
        .into_response()
}
