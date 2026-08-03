//! `/api/workflows/*` 路由：暴露 pc-workflow 引擎。
//!
//! - `GET  /api/workflows` 列出已注册的 routines + pipelines
//! - `POST /api/workflows/:key/run` 触发一个 workflow run
//! - `GET  /api/workflows/active` 列出当前活跃的 runs

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use pc_workflow::{TriggerSpec, WorkflowDefinition, WorkflowRunState};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/workflows", get(list_workflows))
        .route("/api/workflows/active", get(list_active_runs))
        .route("/api/workflows/:key/run", post(run_workflow))
}

async fn list_workflows(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let defs: Vec<WorkflowDefinition> = state.workflow_registry.list();
    let items: Vec<Value> = defs
        .into_iter()
        .map(|d| match d {
            WorkflowDefinition::Routine(r) => json!({
                "kind": "routine",
                "key": r.key,
                "label": r.label,
                "id": r.id,
            }),
            WorkflowDefinition::Pipeline(p) => json!({
                "kind": "pipeline",
                "key": p.key,
                "label": p.label,
                "id": p.id,
                "stepCount": p.steps.len(),
            }),
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

async fn list_active_runs(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let runs: Vec<Value> = state
        .workflow_engine
        .active_runs()
        .await
        .into_iter()
        .map(|id| {
            json!({
                "id": id,
                "state": "pending",
                "note": "engine-level tracking; full state hosted on AppState",
            })
        })
        .collect();
    Ok(Json(json!({ "runs": runs })))
}

async fn run_workflow(
    State(state): State<AppState>,
    Path(key): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let trigger = match body.get("trigger").cloned().unwrap_or(json!("manual")) {
        Value::String(s) if s == "manual" => TriggerSpec::manual(
            body.get("actor")
                .and_then(Value::as_str)
                .unwrap_or("api")
                .to_string(),
        ),
        other => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": "unsupported trigger (only manual supported in this skeleton)",
                    "got": other,
                })),
            )
                .into_response();
        }
    };
    match state.workflow_engine.run(&key, trigger).await {
        Ok(handle) => {
            let id: Uuid = handle.run_id.0;
            let body = Json(json!({
                "id": id,
                "key": key,
                "state": match handle.current_state().await {
                    WorkflowRunState::Pending => "pending",
                    WorkflowRunState::Queued => "queued",
                    WorkflowRunState::Running => "running",
                    WorkflowRunState::Succeeded => "succeeded",
                    WorkflowRunState::Failed => "failed",
                    WorkflowRunState::Cancelled => "cancelled",
                },
            }));
            (StatusCode::ACCEPTED, body).into_response()
        }
        Err(e) => {
            let status = match e {
                pc_workflow::engine::EngineError::WorkflowNotFound(_) => StatusCode::NOT_FOUND,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (status, Json(json!({"error": e.to_string()}))).into_response()
        }
    }
}
