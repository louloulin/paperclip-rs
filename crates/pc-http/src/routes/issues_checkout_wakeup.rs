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

async fn checkout(
    State(state): State<AppState>,
    Path(issue_id): Path<Uuid>,
    headers: axum::http::HeaderMap,
    body: Option<Json<CheckoutBody>>,
) -> ApiResult<impl IntoResponse> {
    let actor_id = require_user_id(&state, &headers).await?;
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
