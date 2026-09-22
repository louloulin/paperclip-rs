//! 两条取消路由：issue 作用域（上游 `daemon.go:5421 CancelTask`）与
//! task 作用域（上游 `chat.go:1780 CancelTaskByUser`）。
//!
//! 两者共用同一份领域载荷（[`Cancellation::by_user`]）与同一张
//! `cancelled_by_type/id/name` 列，但**门槛不同**：
//! - issue 作用域先证明 task 属于该 issue，再按「人工取消」写列；
//! - task 作用域走 agent join 的租户判定（对所有来源的 task 都成立），
//!   再叠加 chat 私有性 / 私有 agent 可见性。

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use mc_core::{Id, Timestamp};
use mc_repos::task::Cancellation;
use uuid::Uuid;

use crate::error::ApiResult;

use super::dto::TaskDto;
use super::TaskScope;
use super::{bad_request, forbidden, not_found, parse_uuid, task_error, user_display_name};

/// `POST /api/issues/:id/tasks/:task_id/cancel`（上游 `CancelTask`）。
///
/// 必须同时证明「URL 里的 issue 属于调用者 workspace」与「task 属于该 issue」——
/// 别的 issue（乃至别的 workspace）的 task UUID 不能从这个路由取消。
pub(crate) async fn cancel_issue_task(
    State(state): State<Arc<crate::state::AppState>>,
    user: crate::routes::auth_user::AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path((issue_id, task_id)): Path<(String, String)>,
) -> ApiResult<Json<TaskDto>> {
    let scope = TaskScope::resolve(&state, user, &headers, &query).await?;
    let issue = scope.issue(&issue_id).await?;

    // 上游用 `parseUUID`（不报错的变体）+ `GetAgentTask`：畸形 id 走的是
    // 「查不到」⇒ 404，而不是 400。
    let parsed = Uuid::parse_str(task_id.trim()).map_err(|_| not_found("task"))?;
    let Some(existing) = scope
        .repo
        .task_for_issue(Id::from(parsed), Id::from(issue.id))
        .await
        .map_err(|e| super::repo_err(e, "task"))?
    else {
        return Err(not_found("task"));
    };

    let name = user_display_name(&state, scope.user_id()).await;
    let cancellation = Cancellation::by_user(Some(scope.user_id()), name);
    // 上游把 service 层的任何错误都写成 400 + `err.Error()`。
    scope
        .repo
        .cancel_task(Id::from(existing.id), &cancellation, Timestamp::now())
        .await
        .map_err(|e| bad_request(e.to_string()))?;

    let refreshed = scope
        .repo
        .task_in_workspace(Id::from(existing.id), scope.workspace_id())
        .await
        .map_err(|e| super::repo_err(e, "task"))?
        .unwrap_or(existing);
    Ok(Json(TaskDto::from_row(&refreshed, scope.workspace_id().0)))
}

/// `POST /api/tasks/:task_id/cancel`（上游 `CancelTaskByUser`）。
///
/// 租户判定一律走 task 的拥有 agent（`agent_task_queue.agent_id` 非空且
/// `ON DELETE CASCADE`，agent 才是 workspace 作用域的），因此对 issue / chat /
/// autopilot / quick-create（`issue_id IS NULL`）四种 task 都成立。
///
/// 可选 `expected_status=queued` + `chat_session_id` + `queue_action` 三元组是
/// chat 队列的 CAS：命中后只允许取消仍在 `queued` 的行，否则 409
/// `task is no longer queued`（上游 `ErrTaskNoLongerQueued`）。
pub(crate) async fn cancel_task(
    State(state): State<Arc<crate::state::AppState>>,
    user: crate::routes::auth_user::AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(task_id): Path<String>,
) -> ApiResult<Json<TaskDto>> {
    let scope = TaskScope::resolve(&state, user, &headers, &query).await?;
    let task = scope.task_in_workspace(&task_id).await?;
    let workspace_id = scope.workspace_id();

    let mut queued_only = false;
    if let Some(expected_status) = super::non_empty_query(&query, "expected_status") {
        if expected_status != "queued" {
            return Err(bad_request("expected_status must be queued"));
        }
        let expected_session = parse_uuid(
            query.get("chat_session_id").map_or("", String::as_str),
            "chat_session_id",
        )?;
        if task.chat_session_id != Some(expected_session) {
            return Err(mc_errors::Error::Conflict {
                message: "task does not belong to the expected chat session".to_owned(),
            }
            .into());
        }
        match super::non_empty_query(&query, "queue_action").as_deref() {
            Some("edit" | "remove") => {}
            _ => return Err(bad_request("queue_action must be edit or remove")),
        }
        queued_only = true;
    }

    if let Some(session_id) = task.chat_session_id {
        // chat 私有性：即便是共享 workspace，也只有开启会话的成员能取消。
        let Some(session) = scope
            .repo
            .chat_session_in_workspace(Id::from(session_id), workspace_id)
            .await
            .map_err(|e| super::repo_err(e, "task"))?
        else {
            return Err(not_found("task"));
        };
        if session.creator_id != scope.user_id().0 {
            return Err(forbidden("not your task"));
        }
    } else {
        // issue / autopilot / quick-create task 都会出现在 agent Activity 与
        // workspace 快照上，那两个面按私有 agent 过滤 ⇒ 这里镜像同一门槛。
        let Some(agent) = scope.agent.agent_opt(Id::from(task.agent_id)).await? else {
            return Err(not_found("task"));
        };
        let targets = scope.agent.targets_of(Id::from(task.agent_id)).await?;
        if !scope.agent.can_access_private(&agent, &targets) {
            return Err(forbidden("you do not have access to this agent"));
        }
    }

    if queued_only && task.status != "queued" {
        return Err(mc_errors::Error::Conflict {
            message: "task is no longer queued".to_owned(),
        }
        .into());
    }

    let name = user_display_name(&state, scope.user_id()).await;
    let cancellation = Cancellation::by_user(Some(scope.user_id()), name);
    scope
        .repo
        .cancel_task(Id::from(task.id), &cancellation, Timestamp::now())
        .await
        .map_err(|e| task_error(e, "task"))?;

    let refreshed = scope
        .repo
        .task_in_workspace(Id::from(task.id), workspace_id)
        .await
        .map_err(|e| super::repo_err(e, "task"))?
        .unwrap_or(task);
    Ok(Json(TaskDto::from_row(&refreshed, workspace_id.0)))
}
