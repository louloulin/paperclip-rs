//! Issue checkout + wakeup 路径。
//!
//! - checkout：建立 actor 对 issue 的执行锁（写入 checkout_run_id）
//! - wakeup：在 agent_wakeup_requests 表中插入请求并触发心跳

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use serde_json::json;
use uuid::Uuid;

use crate::{state::require_user_id, ApiError, ApiResult, AppState};
use pc_execution_workspace_guards::{
    is_closed_isolated_execution_workspace, ExecutionWorkspaceGuardTarget,
    ExecutionWorkspaceMode as GuardWorkspaceMode, ExecutionWorkspaceStatus as GuardWorkspaceStatus,
};
use pc_repos::execution::ExecutionRepo;
use pc_repos::issue::IssueRepo;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/issues/:issue_id/checkout", post(checkout))
        .route("/api/issues/:issue_id/wakeup", post(wakeup))
}

#[derive(Debug, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct CheckoutBody {
    #[serde(default)]
    actor_type: Option<String>,
    #[serde(default)]
    actor_id: Option<String>,
    #[serde(default)]
    run_id: Option<Uuid>,
    #[serde(default)]
    strategy: Option<String>,
}

/// R566: closed isolated execution workspace guard. Mirrors Node
/// `getClosedIssueExecutionWorkspace` from `server/src/routes/issues.ts`.
/// Returns Some(payload) if the issue is linked to a closed isolated
/// execution workspace; None otherwise. The caller should respond with
/// 409 carrying the payload (under `executionWorkspace`).
async fn r566_closed_workspace_guard(
    db: &pc_db::Db,
    issue_id: Uuid,
) -> ApiResult<Option<serde_json::Value>> {
    let Some(issue_row) = IssueRepo::new(db).get(issue_id).await? else {
        return Ok(None);
    };
    let Some(ws_id) = issue_row.execution_workspace_id else {
        return Ok(None);
    };
    let Some(row) = ExecutionRepo::new(db).get_by_id(ws_id).await? else {
        return Ok(None);
    };
    let Some(mode) = GuardWorkspaceMode::parse(&row.mode) else {
        return Ok(None);
    };
    let Some(status) = GuardWorkspaceStatus::parse(&row.status) else {
        return Ok(None);
    };
    let target = ExecutionWorkspaceGuardTarget {
        closed_at: row.closed_at.map(|ts| ts.as_datetime().to_rfc3339()),
        mode,
        name: row.name.clone(),
        status,
    };
    if !is_closed_isolated_execution_workspace(Some(&target)) {
        return Ok(None);
    }
    Ok(Some(serde_json::json!({
        "id": row.id,
        "companyId": row.company_id,
        "name": row.name,
        "mode": row.mode,
        "status": row.status,
        "closedAt": row.closed_at,
        "cleanupReason": row.cleanup_reason,
    })))
}

async fn checkout(
    State(state): State<AppState>,
    Path(issue_id): Path<Uuid>,
    headers: axum::http::HeaderMap,
    body: Option<Json<CheckoutBody>>,
) -> ApiResult<impl IntoResponse> {
    let actor_id = require_user_id(&state, &headers).await?;
    // R566: closed isolated execution workspace guard.
    if let Some(payload_ws) = r566_closed_workspace_guard(&state.db, issue_id).await? {
        let name = payload_ws
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("workspace")
            .to_string();
        let mode = payload_ws
            .get("mode")
            .and_then(|v| v.as_str())
            .and_then(GuardWorkspaceMode::parse)
            .unwrap_or(GuardWorkspaceMode::Inherit);
        let status = payload_ws
            .get("status")
            .and_then(|v| v.as_str())
            .and_then(GuardWorkspaceStatus::parse)
            .unwrap_or(GuardWorkspaceStatus::Archived);
        let target = ExecutionWorkspaceGuardTarget {
            closed_at: payload_ws
                .get("closedAt")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            mode,
            name,
            status,
        };
        let message =
            pc_execution_workspace_guards::get_closed_isolated_execution_workspace_message(&target);
        return Err(ApiError::ConflictWith {
            message,
            payload: serde_json::json!({ "executionWorkspace": payload_ws }),
        });
    }
    let body = body.map(|Json(b)| b).unwrap_or_default();
    let run_id = body.run_id.unwrap_or_else(Uuid::new_v4);
    let strategy = body.strategy.as_deref().unwrap_or("merge");
    let actor_type = body.actor_type.as_deref().unwrap_or("board");

    let repo = IssueRepo::new(&state.db);
    let snapshot = repo
        .get_checkout_snapshot(issue_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let Some((_, assignee_agent_id, prev_checkout_run_id)) = snapshot else {
        return Err(ApiError::NotFound(format!("issue {issue_id}")));
    };

    let _ = repo
        .set_checkout_run(issue_id, run_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let _ = repo
        .insert_checkout_lock(issue_id, run_id, actor_type, &actor_id, strategy)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let should_wake = should_wake_assignee(
        actor_type,
        &actor_id,
        assignee_agent_id,
        prev_checkout_run_id,
    );
    if should_wake {
        if let Some(agent_id) = assignee_agent_id {
            enqueue_wakeup(
                &state,
                &repo,
                issue_id,
                agent_id,
                "issue_checkout",
                &actor_id,
                actor_type,
            )
            .await;
        }
    }

    Ok((
        StatusCode::OK,
        Json(json!({
            "issueId": issue_id,
            "status": "checked-out",
            "actorId": actor_id,
            "runId": run_id,
            "wakeupQueued": should_wake,
        })),
    ))
}

async fn wakeup(
    State(state): State<AppState>,
    Path(issue_id): Path<Uuid>,
    headers: axum::http::HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor_id = require_user_id(&state, &headers).await?;
    let repo = IssueRepo::new(&state.db);
    let row = repo
        .get_id_and_assignee(issue_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let Some((_, assignee_agent_id)) = row else {
        return Err(ApiError::NotFound(format!("issue {issue_id}")));
    };

    let queued = if let Some(agent_id) = assignee_agent_id {
        enqueue_wakeup(
            &state,
            &repo,
            issue_id,
            agent_id,
            "issue_wakeup",
            &actor_id,
            "user",
        )
        .await;
        true
    } else {
        false
    };

    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "issueId": issue_id,
            "status": if queued { "wakeup-queued" } else { "no-assignee" },
            "actorId": actor_id,
        })),
    ))
}

async fn enqueue_wakeup(
    _state: &AppState,
    repo: &IssueRepo<'_>,
    issue_id: Uuid,
    agent_id: Uuid,
    source: &str,
    actor_id: &str,
    actor_type: &str,
) {
    // Resolve company_id for the agent
    let company_id = match repo.get_agent_company_id(agent_id).await {
        Ok(Some(c)) => c,
        _ => return,
    };
    let payload = json!({ "issueId": issue_id, "actorId": actor_id });
    let _ = repo
        .enqueue_agent_wakeup(
            company_id,
            agent_id,
            source,
            &format!("{source}:{issue_id}"),
            &payload,
            actor_type,
            actor_id,
        )
        .await;
}

fn should_wake_assignee(
    actor_type: &str,
    actor_id: &str,
    assignee_agent_id: Option<Uuid>,
    checkout_run_id: Option<Uuid>,
) -> bool {
    if actor_type != "agent" {
        return true;
    }
    if assignee_agent_id.is_none() {
        return false;
    }
    if checkout_run_id.is_none() {
        return true;
    }
    Uuid::parse_str(actor_id)
        .map(|uid| Some(uid) != assignee_agent_id)
        .unwrap_or(true)
}
