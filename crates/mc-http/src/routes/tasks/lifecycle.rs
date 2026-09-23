//! `preview-trigger` / `active-task` / `task-runs`（上游 `issue_trigger.go:146`、
//! `daemon.go:5394`、`daemon.go:5519`）。
//!
//! `preview-trigger` 是 docs/15 §1.6 点名的「取回 source context」前置面：它只做
//! **只读判定**（会不会派单、派给谁），真正的入队仍在 M3-7 的 daemon 循环里。

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderName};
use axum::response::{IntoResponse, Response};
use axum::Json;
use mc_core::Id;
use uuid::Uuid;

use crate::error::ApiResult;

use super::dto::{
    ActiveRunSummaryDto, ActiveTasksResponse, IssueTriggerPreviewItem, IssueTriggerPreviewRequest,
    IssueTriggerPreviewResponse, TaskDto,
};
use super::{bad_request, non_empty_query, repo_err, TaskScope};

/// 单次 preview 的 issue 上限（上游 `maxPreviewTriggerIssues`）。
const MAX_PREVIEW_TRIGGER_ISSUES: usize = 500;
/// `scope=family` 的响应预算（上游 `familyActiveRunCap`）。
const FAMILY_ACTIVE_RUN_CAP: usize = 20;
/// 截断信号 header（上游 `HeaderActiveRunsTruncated`）。
const ACTIVE_RUNS_TRUNCATED: HeaderName = HeaderName::from_static("x-active-runs-truncated");

// ---------------------------------------------------------------------------
// preview-trigger
// ---------------------------------------------------------------------------

/// 一次 prospect 写入的「after」形态（上游 `service.IssueTriggerInput` 的载体）。
struct PreviewCandidate {
    /// 已存在 issue 的 id；`is_create` 时为 `None`（尚无实体）。
    issue_id: Option<Uuid>,
    /// 目标 issue 的（新）状态 key。
    status: String,
    /// 目标 assignee（`(type, id)`）。
    assignee: Option<(String, Uuid)>,
    /// issue 处于 triage（triage 比 backlog 更严：直接拒绝派单）。
    triage: bool,
}

/// `POST /api/issues/preview-trigger`（上游 `PreviewIssueTrigger`）。
///
/// 逐字沿用上游的判定分支（`service.WillEnqueueRun`）；**不做** squad 分支
/// （本仓没有 squad 仓储）与 agent-actor 分支（本仓一律按 member 处理），
/// 两条差异见 `docs/41-M3-6-TASK-QUEUE.md` §5。
#[allow(clippy::too_many_lines)]
pub(crate) async fn preview_trigger(
    State(state): State<Arc<crate::state::AppState>>,
    user: crate::routes::auth_user::AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> ApiResult<Json<IssueTriggerPreviewResponse>> {
    let scope = TaskScope::resolve(&state, user, &headers, &query).await?;
    let req: IssueTriggerPreviewRequest =
        serde_json::from_slice(&body).map_err(|_| bad_request("invalid request body"))?;
    if req.issue_ids.len() > MAX_PREVIEW_TRIGGER_ISSUES {
        return Err(bad_request("too many issue_ids"));
    }

    // 目标 assignee 只解析一次：畸形 id 是确定性的 400，绝不静默漏计。
    let mut new_assignee: Option<(String, Uuid)> = None;
    if let (Some(assignee_type), Some(assignee_id)) =
        (req.assignee_type.as_deref(), req.assignee_id.as_deref())
    {
        if !assignee_type.is_empty() && !assignee_id.is_empty() {
            new_assignee = Some((
                assignee_type.to_owned(),
                super::parse_uuid(assignee_id, "assignee_id")?,
            ));
        }
    }

    let mut triggers: Vec<IssueTriggerPreviewItem> = Vec::new();

    if req.is_create {
        let status = req
            .status
            .as_deref()
            .filter(|v| !v.is_empty())
            .unwrap_or("todo")
            .to_owned();
        let candidate = PreviewCandidate {
            issue_id: None,
            status,
            assignee: new_assignee,
            triage: false,
        };
        if let Some((agent_id, source)) =
            will_enqueue(&scope, &candidate, None, true, false, false).await?
        {
            // create 没有落库的 issue id：上游此时 `trigger.IssueID` 是零值 UUID。
            triggers.push(IssueTriggerPreviewItem {
                issue_id: Uuid::nil().to_string(),
                agent_id: agent_id.to_string(),
                source: source.to_owned(),
            });
        }
        return Ok(Json(IssueTriggerPreviewResponse {
            total_count: triggers.len(),
            triggers,
        }));
    }

    for raw_id in &req.issue_ids {
        // 畸形 id 与跨 workspace / 不存在的 id 都「不贡献 trigger」（确定性）。
        let Ok(issue_uuid) = Uuid::parse_str(raw_id.trim()) else {
            continue;
        };
        let Ok(Some(issue)) = scope
            .repo
            .issue_for_workspace(Id::from(issue_uuid), scope.workspace_id())
            .await
        else {
            continue;
        };

        let mut assignee_changed = false;
        let post_assignee = if let Some(new) = new_assignee.clone() {
            assignee_changed = issue.assignee_type.as_deref() != Some(new.0.as_str())
                || issue.assignee_id != Some(new.1);
            Some(new)
        } else {
            issue.assignee_type.clone().zip(issue.assignee_id)
        };

        let status_changed = req
            .status
            .as_deref()
            .is_some_and(|s| !s.is_empty() && issue.status != s);
        let post_status = req
            .status
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(&issue.status)
            .to_owned();

        let candidate = PreviewCandidate {
            issue_id: Some(issue.id),
            status: post_status,
            assignee: post_assignee,
            triage: issue.triage_state.is_some(),
        };
        if let Some((agent_id, source)) = will_enqueue(
            &scope,
            &candidate,
            Some(&issue.status),
            false,
            assignee_changed,
            status_changed,
        )
        .await?
        {
            triggers.push(IssueTriggerPreviewItem {
                issue_id: issue.id.to_string(),
                agent_id: agent_id.to_string(),
                source: source.to_owned(),
            });
        }
    }

    Ok(Json(IssueTriggerPreviewResponse {
        total_count: triggers.len(),
        triggers,
    }))
}

/// 上游 `service.WillEnqueueRun`（`service/issue_trigger.go:97`）的逐条镜像。
///
/// 返回「会派单」的目标 agent 与来源（`assign` / `status`）。
async fn will_enqueue(
    scope: &TaskScope,
    candidate: &PreviewCandidate,
    prev_status: Option<&str>,
    is_create: bool,
    assignee_changed: bool,
    status_changed: bool,
) -> ApiResult<Option<(Uuid, &'static str)>> {
    let Some((assignee_type, assignee_id)) = candidate.assignee.clone() else {
        return Ok(None);
    };
    if candidate.triage {
        return Ok(None);
    }

    let workspace_id = scope.workspace_id();
    let current = scope
        .repo
        .effective_status(workspace_id, &candidate.status)
        .await
        .map_err(|e| repo_err(e, "issue"))?;
    let prev = match prev_status {
        Some(raw) => scope
            .repo
            .effective_status(workspace_id, raw)
            .await
            .map_err(|e| repo_err(e, "issue"))?,
        None => String::new(),
    };

    let source = if is_create || assignee_changed {
        // backlog 是停放区：指派进 backlog 永远不会起跑。
        if current == "backlog" {
            return Ok(None);
        }
        "assign"
    } else if status_changed
        && prev == "backlog"
        && current != "backlog"
        && current != "done"
        && current != "cancelled"
    {
        // 自环判定只对 agent actor 有意义；本仓一律按 member 处理 ⇒ 非自环。
        "status"
    } else {
        return Ok(None);
    };

    // squad 分支缺席（本仓无 squad 仓储，见 docs/41 §5）。
    if assignee_type != "agent" {
        return Ok(None);
    }

    let Ok(Some(brief)) = scope.repo.agent_brief(Id::from(assignee_id)).await else {
        return Ok(None);
    };
    if brief.runtime_id.is_none() || brief.archived_at.is_some() {
        return Ok(None);
    }
    // 私有 agent 的就绪度不得泄露给看不见它的成员（上游 preview 用真门槛）。
    let Ok(Some(agent)) = scope.agent.agent_opt(Id::from(assignee_id)).await else {
        return Ok(None);
    };
    let Ok(targets) = scope.agent.targets_of(Id::from(assignee_id)).await else {
        return Ok(None);
    };
    if !scope.agent.can_invoke(&agent, &targets) {
        return Ok(None);
    }

    if source == "status" {
        // status 源可能对已有 pending 任务的 assignee 重复起火；(issue, agent) 的
        // 部分唯一索引会把那次插入合并掉 ⇒ preview 不能承诺它。
        let Some(issue_id) = candidate.issue_id else {
            return Ok(None);
        };
        let pending = scope
            .repo
            .has_pending_task_for_issue_agent(Id::from(issue_id), Id::from(assignee_id))
            .await
            .map_err(|e| repo_err(e, "task"))?;
        if pending {
            return Ok(None);
        }
    }
    Ok(Some((assignee_id, source)))
}

// ---------------------------------------------------------------------------
// active-task
// ---------------------------------------------------------------------------

/// `GET /api/issues/:id/active-task`（上游 `GetActiveTaskForIssue`）。
///
/// 上游**吞掉**查询错误并回 `{"tasks": []}`（协调查询宁少不错），这里逐字保留。
pub(crate) async fn get_active_task(
    State(state): State<Arc<crate::state::AppState>>,
    user: crate::routes::auth_user::AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(issue_id): Path<String>,
) -> ApiResult<Json<ActiveTasksResponse>> {
    let scope = TaskScope::resolve(&state, user, &headers, &query).await?;
    let issue = scope.issue(&issue_id).await?;
    let workspace_id = scope.workspace_id().0;
    let rows = scope
        .repo
        .list_active_tasks_by_issue(Id::from(issue.id))
        .await
        .unwrap_or_default();
    let tasks = rows
        .iter()
        .filter(|row| visible_in_history(row))
        .map(|row| TaskDto::from_row(row, workspace_id))
        .collect();
    Ok(Json(ActiveTasksResponse { tasks }))
}

// ---------------------------------------------------------------------------
// task-runs
// ---------------------------------------------------------------------------

/// `GET /api/issues/:id/task-runs`（上游 `ListTasksByIssue`）。
///
/// `scope=family` 时回 `ActiveRunSummary[]` 并可能带 `X-Active-Runs-Truncated`；
/// 否则回完整 `AgentTaskResponse[]`（本仓为收窄后的 [`TaskDto`]）。
pub(crate) async fn task_runs(
    State(state): State<Arc<crate::state::AppState>>,
    user: crate::routes::auth_user::AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(issue_id): Path<String>,
) -> ApiResult<Response> {
    let scope = TaskScope::resolve(&state, user, &headers, &query).await?;
    let issue = scope.issue(&issue_id).await?;
    let workspace_id = scope.workspace_id().0;

    let scope_param = non_empty_query(&query, "scope");
    if let Some(value) = scope_param.as_deref() {
        if value != "issue" && value != "family" {
            return Err(bad_request("scope must be 'issue' or 'family'"));
        }
    }
    let active_only = match non_empty_query(&query, "active").as_deref() {
        None | Some("false") => false,
        Some("true") => true,
        Some(_) => return Err(bad_request("invalid active parameter; expected boolean")),
    };

    if scope_param.as_deref() == Some("family") {
        // 族根 = 有父 issue 就取父（子 issue 能看到兄弟），否则取自己。
        let root = issue.parent_issue_id.unwrap_or(issue.id);
        let limit = i64::try_from(FAMILY_ACTIVE_RUN_CAP).unwrap_or(i64::MAX) + 1;
        let mut rows = scope
            .repo
            .list_active_tasks_by_issue_family(scope.workspace_id(), Id::from(root), limit)
            .await
            .map_err(|e| repo_err(e, "task"))?;
        let truncated = rows.len() > FAMILY_ACTIVE_RUN_CAP;
        rows.truncate(FAMILY_ACTIVE_RUN_CAP);
        let summaries: Vec<ActiveRunSummaryDto> = rows
            .iter()
            .map(|row| ActiveRunSummaryDto {
                task_id: row.task_id.to_string(),
                issue_id: row.issue_id.to_string(),
                issue_identifier: issue_identifier(&row.issue_prefix, row.issue_number),
                issue_title: row.issue_title.clone(),
                agent_id: row.agent_id.to_string(),
                status: row.status.clone(),
                created_at: row.created_at.to_rfc3339(),
                started_at: row.started_at.map(|t| t.to_rfc3339()),
            })
            .collect();
        let mut response = Json(summaries).into_response();
        if truncated {
            response.headers_mut().insert(
                ACTIVE_RUNS_TRUNCATED,
                header::HeaderValue::from_static("true"),
            );
        }
        return Ok(response);
    }

    let rows = if active_only {
        scope
            .repo
            .list_active_tasks_by_issue(Id::from(issue.id))
            .await
            .map_err(|e| repo_err(e, "task"))?
    } else {
        scope
            .repo
            .list_tasks_by_issue(Id::from(issue.id))
            .await
            .map_err(|e| repo_err(e, "task"))?
    };
    let tasks: Vec<TaskDto> = rows
        .iter()
        .filter(|row| visible_in_history(row))
        .map(|row| TaskDto::from_row(row, workspace_id))
        .collect();
    Ok(Json(tasks).into_response())
}

/// 上游 `visibleTaskHistory`：未开始的升级占位行不进执行日志。
fn visible_in_history(row: &mc_repos::task::TaskRow) -> bool {
    !(row.escalation_for_task_id.is_some()
        && row.started_at.is_none()
        && (row.status == "deferred" || row.status == "cancelled"))
}

/// 上游 `service.IssueIdentifier`。
fn issue_identifier(prefix: &str, number: i32) -> String {
    if prefix.is_empty() {
        format!("#{number}")
    } else {
        format!("{prefix}-{number}")
    }
}
