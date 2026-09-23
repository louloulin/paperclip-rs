//! M4-4（LUM-1475）：聊天**队列**五条路由。
//!
//! | 路由 | 上游 | 本文件 |
//! | --- | --- | --- |
//! | `GET /api/chat/sessions/:id/pending-task` | `GetPendingChatTask`（`chat.go:1609`） | [`get_pending_chat_task`] |
//! | `DELETE /api/chat/sessions/:id/queued-tasks` | `ClearQueuedChatTasks`（`chat.go:1758`） | [`clear_queued_chat_tasks`] |
//! | `POST /api/chat/sessions/:id/queued-tasks/:taskId/prioritize` | `PrioritizeQueuedChatTask`（`chat.go:1650`） | [`prioritize_queued_chat_task`] |
//! | `GET /api/chat/pending-tasks` | `ListPendingChatTasks`（`chat.go:1477`） | [`list_pending_chat_tasks`] |
//! | `GET /api/chat/pending-tasks/has-any` | `HasPendingChatTasks`（`chat.go:1565`） | [`has_pending_chat_tasks`] |
//!
//! 三个「可见性」约定（上游注释把理由写在各自 handler 上，本文件只标行号）：
//! 1. FAB 的两条（`list` / `has-any`）先取调用者**可见**的 agent 集合，空集合直接短路
//!    （少一次往返，且不会把「看不见」误报成「没有」）；`list` 再按集合过滤行，
//!    `has-any` 把集合烘进 `EXISTS`。
//! 2. `pending-task` 走公开会话门，返回**可见头** + 只含 `status == queued` 的后续行。
//! 3. `prioritize` 与 `clear` 都在会话锁下与 daemon 的 claim 串行化。
//!
//! **有意偏离**（登记在 `docs/45`）：`prioritize` 成功后上游再 `GetAgentTask` 回读一次
//!（其失败分支是 500 `"failed to load prioritized task"`），本仓的 CAS 直接 `RETURNING`
//! 出该行 ⇒ 那条 500 不可达（少一次往返，状态码面不变）。`BroadcastTaskQueued`
//!（`chat.go:1743`）属 LUM-1506，本片不发。

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

use mc_chat::task as chat_task;
use mc_errors::Error;
use mc_repos::chat_task::{ChatTaskRepo, PriorityOutcome};

use crate::error::ApiResult;
use crate::state::AppState;

use super::support::{internal, parse_uuid_field, ts, ChatScope};

// ---------------------------------------------------------------------------
// DTO
// ---------------------------------------------------------------------------

/// 上游 `PendingChatTasksResponse`（`chat.go:1467`）：`tasks` **永不为 null**。
#[derive(Debug, Serialize)]
pub(super) struct PendingChatTasksResponse {
    tasks: Vec<PendingChatTaskItemDto>,
}

/// 上游 `PendingChatTaskItem`（`chat.go:1471`）。
#[derive(Debug, Serialize)]
pub(super) struct PendingChatTaskItemDto {
    task_id: String,
    status: String,
    chat_session_id: String,
}

/// 上游 `HasPendingChatTasksResponse`（`chat.go:1550`）。
#[derive(Debug, Serialize)]
pub(super) struct HasPendingChatTasksResponse {
    has_pending: bool,
}

/// 上游 `PendingChatTaskResponse`（`chat.go:1283`）。
///
/// 四个 `omitempty` 字段在「没有 pending 任务」时全部消失 ⇒ 空响应恰好是
/// `{"supports_queue":true}`（`supports_queue` 是唯一无 `omitempty` 的）。
#[derive(Debug, Serialize)]
pub(super) struct PendingChatTaskResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    created_at: Option<String>,
    /// 只有 `waiting_local_directory` 才下发（见 `wait_reason_for_status`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    wait_reason: Option<String>,
    supports_queue: bool,
    /// Go 的 `omitempty` 对**空切片**也成立 ⇒ 没有后续行时整个键消失。
    #[serde(skip_serializing_if = "Option::is_none")]
    queued_tasks: Option<Vec<QueuedChatTaskResponse>>,
}

/// 上游 `QueuedChatTaskResponse`（`chat.go:1309`）。
#[derive(Debug, Serialize)]
pub(super) struct QueuedChatTaskResponse {
    task_id: String,
    status: String,
    created_at: String,
    /// `uuidToString` 对无效 UUID 回 `""` ⇒ `omitempty` 抹掉（无输入批次时）。
    #[serde(skip_serializing_if = "Option::is_none")]
    message_id: Option<String>,
    /// `COALESCE(content, '')` 为空串时同样被 `omitempty` 抹掉。
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
}

/// 上游 `PrioritizeQueuedChatTaskResponse`（`chat.go:1317`）。
#[derive(Debug, Serialize)]
pub(super) struct PrioritizeQueuedChatTaskResponse {
    task_id: String,
    /// CAS 时没有可见的活跃任务 ⇒ `""` ⇒ `omitempty` 抹掉。
    #[serde(skip_serializing_if = "Option::is_none")]
    active_task_id: Option<String>,
}

// ---------------------------------------------------------------------------
// handler
// ---------------------------------------------------------------------------

/// `GET /api/chat/sessions/:id/pending-task`（上游 `GetPendingChatTask`）。
pub(super) async fn get_pending_chat_task(
    State(state): State<Arc<AppState>>,
    auth: crate::routes::auth_user::AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(session_id): Path<String>,
) -> ApiResult<Json<PendingChatTaskResponse>> {
    let scope = ChatScope::resolve(&state, auth, &headers, &query).await?;
    let tasks = ChatTaskRepo::new(state.db.clone());
    let session = scope.gate_public_session_for_user(&session_id).await?;

    // 上游任何读失败都是 500（没有 404/409 分支）。
    let rows = tasks
        .pending_tasks_for_session(session.id)
        .await
        .map_err(|_| internal("failed to list pending chat tasks"))?;

    // 上游 `len(tasks)==0` ⇒ 只回 `supports_queue`。
    let rows = rows
        .into_iter()
        .map(|row| chat_task::PendingTaskRow {
            task_id: row.task_id,
            status: row.status,
            created_at: row.created_at,
            wait_reason: row.wait_reason,
            message_id: row.message_id,
            content: Some(row.content),
        })
        .collect();
    let projection = chat_task::project_pending_tasks(rows);
    let Some(head) = projection.head else {
        return Ok(Json(PendingChatTaskResponse {
            task_id: None,
            status: None,
            created_at: None,
            wait_reason: None,
            supports_queue: projection.supports_queue,
            queued_tasks: None,
        }));
    };

    // 空 Vec ⇒ `None`（复现 Go 的 `omitempty` 抹空切片）。
    let queued: Vec<QueuedChatTaskResponse> = projection
        .queued
        .iter()
        .map(|row| QueuedChatTaskResponse {
            task_id: row.task_id.to_string(),
            status: row.status.clone(),
            created_at: ts(row.created_at),
            message_id: row.message_id.map(|id| id.to_string()),
            content: row.content.clone().filter(|c| !c.is_empty()),
        })
        .collect();

    Ok(Json(PendingChatTaskResponse {
        task_id: Some(head.task_id.to_string()),
        status: Some(head.status.clone()),
        created_at: Some(ts(head.created_at)),
        wait_reason: {
            let reason =
                chat_task::wait_reason_for_status(&head.status, head.wait_reason.as_deref());
            (!reason.is_empty()).then_some(reason)
        },
        supports_queue: projection.supports_queue,
        queued_tasks: (!queued.is_empty()).then_some(queued),
    }))
}

/// `GET /api/chat/pending-tasks`（上游 `ListPendingChatTasks`）。
pub(super) async fn list_pending_chat_tasks(
    State(state): State<Arc<AppState>>,
    auth: crate::routes::auth_user::AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<PendingChatTasksResponse>> {
    let scope = ChatScope::resolve(&state, auth, &headers, &query).await?;
    let tasks = ChatTaskRepo::new(state.db.clone());
    let allowed = scope.accessible_agent_ids().await?;

    // 空集合 ⇒ 每一行都会被滤掉，跳过这次往返（与 `has-any` 同款）。
    if chat_task::skip_pending_query(allowed.len()) {
        return Ok(Json(PendingChatTasksResponse { tasks: Vec::new() }));
    }

    let rows = tasks
        .pending_tasks_by_creator(scope.workspace_id().0, scope.user_id().0)
        .await
        .map_err(|_| internal("failed to list pending chat tasks"))?;

    // 丢私有 agent 的行（查询已带回 `agent_id`，不必再扫一次会话表 —— MUL-4159）。
    // `tasks` 由 `Vec` 序列化 ⇒ 空集是 `[]`，不会变 `null`。
    let tasks: Vec<PendingChatTaskItemDto> = rows
        .into_iter()
        .filter(|row| allowed.contains(&row.agent_id))
        .filter_map(|row| {
            let item = chat_task::PendingChatTaskItem {
                task_id: row.task_id,
                status: row.status,
                chat_session_id: row.chat_session_id?,
            };
            Some(PendingChatTaskItemDto {
                task_id: item.task_id.to_string(),
                status: item.status,
                chat_session_id: item.chat_session_id.to_string(),
            })
        })
        .collect();

    Ok(Json(PendingChatTasksResponse { tasks }))
}

/// `GET /api/chat/pending-tasks/has-any`（上游 `HasPendingChatTasks`）。
pub(super) async fn has_pending_chat_tasks(
    State(state): State<Arc<AppState>>,
    auth: crate::routes::auth_user::AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<HasPendingChatTasksResponse>> {
    let scope = ChatScope::resolve(&state, auth, &headers, &query).await?;
    let tasks = ChatTaskRepo::new(state.db.clone());
    let allowed = scope.accessible_agent_ids().await?;

    // 拿不到任何可见 agent ⇒ 调用者能看到的东西不可能在飞。
    if chat_task::skip_pending_query(allowed.len()) {
        return Ok(Json(HasPendingChatTasksResponse { has_pending: false }));
    }

    let agent_ids: Vec<uuid::Uuid> = allowed.into_iter().collect();
    let has_pending = tasks
        .has_pending_tasks_by_creator(scope.workspace_id().0, scope.user_id().0, &agent_ids)
        .await
        .map_err(|_| internal("failed to check pending chat tasks"))?;

    Ok(Json(HasPendingChatTasksResponse { has_pending }))
}

/// `POST /api/chat/sessions/:id/queued-tasks/:taskId/prioritize`（上游 `PrioritizeQueuedChatTask`）。
///
/// 判定顺序逐字：`task id` 解析（400）→ 公开会话门 → 事务（500 起手）→
/// 锁 agent（500）→ CAS → 两种 409 → 500 各阶段 → 200。
pub(super) async fn prioritize_queued_chat_task(
    State(state): State<Arc<AppState>>,
    auth: crate::routes::auth_user::AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path((session_id, task_id)): Path<(String, String)>,
) -> ApiResult<(StatusCode, Json<PrioritizeQueuedChatTaskResponse>)> {
    let scope = ChatScope::resolve(&state, auth, &headers, &query).await?;
    let tasks = ChatTaskRepo::new(state.db.clone());
    // 路径参数先解析（上游把它放在会话门**之前**）。
    let task_id = parse_uuid_field(&task_id, "task id")?;

    let session = scope.gate_public_session_for_user(&session_id).await?;

    // `PriorityError` 的 `Display` 就是上游那四句 500 文案（DB 细节留在 source 里）。
    let outcome = tasks
        .prioritize_queued_task(session.id, session.agent_id, task_id)
        .await
        .map_err(|e| internal(e.to_string()))?;

    match outcome {
        PriorityOutcome::Prioritized(row) => Ok((
            StatusCode::OK,
            Json(PrioritizeQueuedChatTaskResponse {
                task_id: row.task_id.to_string(),
                active_task_id: row.active_task_id.map(|id| id.to_string()),
            }),
        )),
        // 目标还在队列里，但可见头尚未被认领 —— 没有活跃回复可替换（上游回读区分）。
        PriorityOutcome::NoActiveReply => Err(Error::Conflict {
            message: "there is no active reply to replace".into(),
        }
        .into()),
        // 目标已不在队列（被 daemon 提升 / 取消 / 换了会话）。
        PriorityOutcome::NotQueued => Err(Error::Conflict {
            message: "task is no longer queued".into(),
        }
        .into()),
    }
}

/// `DELETE /api/chat/sessions/:id/queued-tasks`（上游 `ClearQueuedChatTasks`）。
///
/// 取消本会话除可见头以外的全部 queued 追问，**即使该头自己还没被认领也保住它**。
pub(super) async fn clear_queued_chat_tasks(
    State(state): State<Arc<AppState>>,
    auth: crate::routes::auth_user::AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(session_id): Path<String>,
) -> ApiResult<Response> {
    let scope = ChatScope::resolve(&state, auth, &headers, &query).await?;
    let tasks = ChatTaskRepo::new(state.db.clone());
    let session = scope.gate_public_session_for_user(&session_id).await?;

    // 上游提交后的四步副作用（埋点 / agent 状态汇总 / 两条广播）不在本片写集，见模块头。
    tasks
        .clear_queued_tasks(session.id, session.agent_id)
        .await
        .map_err(|_| internal("failed to clear queued tasks"))?;

    Ok(StatusCode::NO_CONTENT.into_response())
}
