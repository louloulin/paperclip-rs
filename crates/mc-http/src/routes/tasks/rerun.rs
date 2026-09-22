//! 两条「再来一遍」路由：issue 级 rerun（上游 `task_lifecycle.go:165 RerunIssue`）
//! 与 quick-create 的 source-context 人工重试（上游 `task_lifecycle.go:237`）。
//!
//! 两者的共同点是**先过 invoke 门再动数据**：上游在 service 里把
//! `canInvoke` 回调放在清 pending slot / 建新任务**之前**，被拒时「不取消任何
//! 旧任务、不创建任何新任务」（fail-closed）。本模块保留这个顺序。
//!
//! 两条 403 都回上游 `dispatchBlockedResponse` 的**原始体**
//! （`{"error": ..., "reason_code": ...}`），不套本仓的错误信封 —— 前端按
//! `reason_code` 分支，套信封会让它拿不到 code。

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use mc_core::Id;
use mc_errors::Error;
use mc_repos::task::RerunTaskSpec;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::ApiResult;

use super::dto::{RerunIssueRequest, TaskDto};
use super::{bad_request, parse_uuid, repo_err, TaskScope};

/// 上游 `dispatch.ReasonInvocationNotAllowed`。
pub(crate) const REASON_INVOCATION_NOT_ALLOWED: &str = "invocation_not_allowed";
/// 上游 `dispatch.ReasonIssueInTriage`。
pub(crate) const REASON_ISSUE_IN_TRIAGE: &str = "issue_in_triage";

/// 上游 `writeDispatchBlocked` —— 原始 JSON 体（`admission.go:105`）。
pub(crate) fn dispatch_blocked(reason_code: &str) -> Response {
    let error = match reason_code {
        REASON_INVOCATION_NOT_ALLOWED => "you don't have permission to use this target",
        REASON_ISSUE_IN_TRIAGE => {
            "the issue is in Triage and has no owner to run yet; accept it out of Triage first"
        }
        _ => "the run was blocked",
    };
    (
        StatusCode::FORBIDDEN,
        Json(json!({ "error": error, "reason_code": reason_code })),
    )
        .into_response()
}

/// 上游 `ErrSourceContextRetryUnavailable` 的 409 原始体（`task_lifecycle.go:271`）。
fn retry_unavailable() -> Response {
    (
        StatusCode::CONFLICT,
        Json(json!({
            "code": "source_context_retry_unavailable",
            "error": "This context can no longer be retried. Start again from the branch point.",
        })),
    )
        .into_response()
}

/// `POST /api/issues/:id/rerun`（上游 `RerunIssue`）。
///
/// 202 + 新任务。请求体可选：`{}` / 空体走「把 assignee 再跑一遍」的派生语义，
/// `{"task_id": "..."}`（或 `null`）走具名语义（重复某一次历史运行）。
#[allow(clippy::too_many_lines)]
pub(crate) async fn rerun_issue(
    State(state): State<Arc<crate::state::AppState>>,
    user: crate::routes::auth_user::AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(issue_id): Path<String>,
    body: Bytes,
) -> ApiResult<Response> {
    let scope = TaskScope::resolve(&state, user, &headers, &query).await?;
    let issue = scope.issue(&issue_id).await?;
    let workspace_id = scope.workspace_id();

    let req: RerunIssueRequest = if body.is_empty() {
        RerunIssueRequest::default()
    } else {
        serde_json::from_slice::<Option<RerunIssueRequest>>(&body)
            .map_err(|_| bad_request("invalid request body"))?
            .unwrap_or_default()
    };
    let named_source = req
        .task_id
        .as_deref()
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
        .map(str::to_owned);

    // 具名来源可以是「重复一次讨论」，所以 triage 里的具名 rerun 合法；只有派生
    // rerun 需要执行者，而 triage 恰好没有执行者。
    if named_source.is_none() && issue.triage_state.is_some() {
        return Ok(dispatch_blocked(REASON_ISSUE_IN_TRIAGE));
    }

    let source = match named_source {
        Some(raw) => {
            let id = Id::from(parse_uuid(&raw, "task_id")?);
            let Some(row) = scope
                .repo
                .task_in_workspace(id, workspace_id)
                .await
                .map_err(|e| repo_err(e, "task"))?
            else {
                return Err(bad_request("load source task: task not found"));
            };
            if row.issue_id != Some(issue.id) {
                return Err(bad_request("source task does not belong to this issue"));
            }
            if is_triage_task(&row) {
                return Err(bad_request(
                    "the source task is a triage run, which is redone by re-triaging the issue \
                     rather than by rerunning it",
                ));
            }
            Some(row)
        }
        None => None,
    };

    // 目标 agent：具名来源用历史 agent（可能是已被改派掉的私有 agent），派生来源
    // 用当前 assignee ⇒ 具名 rerun 的门必须打在历史 agent 上。
    let (agent_id, is_leader, trigger_comment_id, rerun_of) = match &source {
        Some(row) => (
            row.agent_id,
            row.is_leader_task,
            row.trigger_comment_id,
            Some(row.id),
        ),
        None => match (issue.assignee_type.as_deref(), issue.assignee_id) {
            (Some("agent"), Some(id)) => (id, false, None, None),
            // 本仓没有 squad 仓储：squad 指派解析不出 leader（上游 GetSquad 失败）。
            (Some("squad"), Some(_)) => {
                return Err(bad_request(
                    "issue is assigned to a squad but squad not found",
                ));
            }
            _ => return Err(bad_request("issue is not assigned to an agent or squad")),
        },
    };

    // 先过门再动数据：被拒时旧任务原样保留（MUL-4525）。
    let Some(agent) = scope.invoke_gate_opt(Id::from(agent_id)).await? else {
        return Ok(dispatch_blocked(REASON_INVOCATION_NOT_ALLOWED));
    };
    let Some(runtime_id) = agent.runtime_id else {
        return Err(bad_request("load target agent: agent has no runtime"));
    };

    // 清掉还没起跑的同 (issue, agent) pending 行：唯一索引会把新行挡在门外。
    // running / waiting_local_directory 故意不动（正在执行的 agent 不该被悄悄杀掉）。
    let clear = || async {
        scope
            .repo
            .cancel_pending_tasks_in_thread(
                Id::from(issue.id),
                Id::from(agent_id),
                trigger_comment_id.map(Id::from),
            )
            .await
    };
    // 上游在清 slot 失败时只 warn 并继续（`task.go:5731`）。
    _ = clear().await;

    let spec = RerunTaskSpec {
        id: Id::from(Uuid::now_v7()),
        agent_id: Id::from(agent_id),
        issue_id: Id::from(issue.id),
        runtime_id: Id::from(runtime_id),
        // IssueBrief 没有 priority 列（上游读 issue.Priority）：取队列默认优先级 0。
        priority: 0,
        trigger_comment_id: trigger_comment_id.map(Id::from),
        is_leader_task: is_leader,
        actor_user_id: scope.user_id(),
        rerun_of_task_id: rerun_of.map(Id::from),
    };
    let task = match scope.repo.enqueue_rerun_task(&spec).await {
        Ok(task) => task,
        Err(first) => {
            // 清 slot 与入队是两个事务：并发的系统重试可能在这中间占走 pending slot。
            // 上游只多试一次（第二次再撞说明有人在循环入队，该暴露而不是自旋）。
            tracing::info!(
                ?first,
                "issue rerun: pending slot taken concurrently, reclaiming"
            );
            _ = clear().await;
            scope
                .repo
                .enqueue_rerun_task(&spec)
                .await
                .map_err(|e| repo_err(e, "task"))?
        }
    };
    Ok((
        StatusCode::ACCEPTED,
        Json(TaskDto::from_row(&task, workspace_id.0)),
    )
        .into_response())
}

/// 上游 `service.IsTriageTask`：`context.type == "triage"`。
fn is_triage_task(row: &mc_repos::task::TaskRow) -> bool {
    context_str(row, "type").as_deref() == Some("triage")
}

/// 读 `context` JSONB 的字符串字段。
fn context_str(row: &mc_repos::task::TaskRow, key: &str) -> Option<String> {
    row.context
        .as_ref()
        .and_then(|value| value.get(key))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// `POST /api/tasks/:task_id/retry-source-context`（上游 `RetrySourceContextQuickCreate`）。
///
/// 只允许**原始发起人**重试（`context.requester_id`），否则 409 的
/// `source_context_retry_unavailable` —— 否则任何成员只要猜到 task id 就能重放别人的
/// 私有 prompt。判定顺序与上游一致：先任务 / 上下文，再 agent 就绪度，最后 invoke 门。
pub(crate) async fn retry_source_context(
    State(state): State<Arc<crate::state::AppState>>,
    user: crate::routes::auth_user::AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(task_id): Path<String>,
) -> ApiResult<Response> {
    let scope = TaskScope::resolve(&state, user, &headers, &query).await?;
    let workspace_id = scope.workspace_id();
    let id = Id::from(parse_uuid(&task_id, "task id")?);

    let Some(parent) = scope
        .repo
        .task_in_workspace(id, workspace_id)
        .await
        .map_err(|e| repo_err(e, "task"))?
    else {
        return Ok(retry_unavailable());
    };
    if parent.status != "failed" {
        return Ok(retry_unavailable());
    }
    let source_context_id = context_str(&parent, "source_context_id").unwrap_or_default();
    let requester_id = context_str(&parent, "requester_id").unwrap_or_default();
    if context_str(&parent, "type").as_deref() != Some("quick_create")
        || source_context_id.is_empty()
        || requester_id != scope.user_id().0.to_string()
        || Uuid::parse_str(&source_context_id).is_err()
    {
        return Ok(retry_unavailable());
    }

    let agent_id = Id::from(parent.agent_id);
    let Some(agent) = scope.agent.agent_opt(agent_id).await? else {
        return Ok(retry_unavailable());
    };
    if agent.archived_at.is_some() || agent.runtime_id.is_none() {
        return Ok(retry_unavailable());
    }
    let targets = scope.agent.targets_of(agent_id).await?;
    if !scope.agent.can_invoke(&agent, &targets) {
        return Ok(dispatch_blocked(REASON_INVOCATION_NOT_ALLOWED));
    }

    let child = scope
        .repo
        .create_quick_create_retry(workspace_id, id, scope.user_id())
        .await
        .map_err(|_| Error::Internal("retry source context quick create".to_owned()))?;
    let Some(child) = child else {
        return Ok(retry_unavailable());
    };
    Ok((
        StatusCode::ACCEPTED,
        Json(TaskDto::from_row(&child, workspace_id.0)),
    )
        .into_response())
}
