//! Smoke Lab — OAuth / 集成冒烟测试。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::FromRow;
use uuid::Uuid;

use crate::{ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/companies/:company_id/smoke-lab/oauth/authorize",
            get(oauth_authorize).post(oauth_authorize_post),
        )
        .route(
            "/api/companies/:company_id/smoke-lab/oauth/token",
            post(oauth_token),
        )
        .route(
            "/api/companies/:company_id/smoke-lab/oauth/userinfo",
            get(oauth_userinfo),
        )
        .route(
            "/api/companies/:company_id/smoke-lab/oauth/revoke",
            post(oauth_revoke),
        )
        .route(
            "/api/companies/:company_id/smoke-lab/services",
            get(services_list),
        )
        .route(
            "/api/companies/:company_id/smoke-lab/services/start",
            post(service_start),
        )
        .route(
            "/api/companies/:company_id/smoke-lab/services/stop",
            post(service_stop),
        )
        .route(
            "/api/companies/:company_id/smoke-lab/install-fixtures",
            post(install_fixtures),
        )
        .route(
            "/api/companies/:company_id/smoke-lab/runs",
            get(runs_list).post(runs_create),
        )
        .route(
            "/api/companies/:company_id/smoke-lab/runs/:run_id",
            get(runs_get),
        )
        .route(
            "/api/companies/:company_id/smoke-lab/runs/:run_id/steps",
            post(runs_steps),
        )
        .route(
            "/api/companies/:company_id/smoke-lab/reset",
            post(smoke_reset),
        )
}

#[derive(Debug, FromRow)]
struct RunRow {
    id: Uuid,
    company_id: Uuid,
    trigger: String,
    status: String,
    started_at: pc_core::Timestamp,
    finished_at: Option<pc_core::Timestamp>,
    summary: Value,
    created_at: pc_core::Timestamp,
    updated_at: pc_core::Timestamp,
}

fn run_json(row: &RunRow) -> Value {
    json!({
        "id": row.id,
        "companyId": row.company_id,
        "trigger": row.trigger,
        "status": row.status,
        "startedAt": row.started_at,
        "finishedAt": row.finished_at,
        "summary": row.summary,
        "createdAt": row.created_at,
        "updatedAt": row.updated_at,
    })
}

#[derive(Debug, FromRow)]
struct StepRow {
    id: Uuid,
    company_id: Uuid,
    run_id: Uuid,
    path: String,
    scenario_step: String,
    status: String,
    detail: Option<String>,
    screenshot_artifact_ref: Option<Value>,
    duration_ms: Option<i32>,
    created_at: pc_core::Timestamp,
    updated_at: pc_core::Timestamp,
}

fn step_json(row: &StepRow) -> Value {
    json!({
        "id": row.id,
        "companyId": row.company_id,
        "runId": row.run_id,
        "path": row.path,
        "scenarioStep": row.scenario_step,
        "status": row.status,
        "detail": row.detail,
        "screenshotArtifactRef": row.screenshot_artifact_ref,
        "durationMs": row.duration_ms,
        "createdAt": row.created_at,
        "updatedAt": row.updated_at,
    })
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct TokenBody {
    grant_type: Option<String>,
    code: Option<String>,
}

async fn oauth_authorize(
    State(_state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> Json<Value> {
    Json(json!({
        "companyId": company_id,
        "authorizationUrl": null,
        "state": "smoke-lab-state"
    }))
}

async fn oauth_authorize_post(
    State(_state): State<AppState>,
    Path(_company_id): Path<Uuid>,
) -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(json!({"status": "redirected", "to": "smoke-lab://authorize"})),
    )
}

async fn oauth_token(
    State(_state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<TokenBody>,
) -> Json<Value> {
    let _ = company_id;
    Json(json!({
        "access_token": "smoke-lab-access-token",
        "token_type": "bearer",
        "expires_in": 3600,
        "grant_type": body.grant_type
    }))
}

async fn oauth_userinfo(
    State(_state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> Json<Value> {
    let _ = company_id;
    Json(json!({
        "sub": "smoke-lab-user",
        "email": "smoke@example.com"
    }))
}

async fn oauth_revoke(
    State(_state): State<AppState>,
    Path(_company_id): Path<Uuid>,
    Json(_body): Json<Value>,
) -> impl IntoResponse {
    (StatusCode::OK, Json(json!({"revoked": true})))
}

async fn services_list(
    State(_state): State<AppState>,
    Path(_company_id): Path<Uuid>,
) -> Json<Value> {
    Json(json!({ "services": [] }))
}

async fn service_start(
    State(_state): State<AppState>,
    Path(_company_id): Path<Uuid>,
    Json(_body): Json<Value>,
) -> impl IntoResponse {
    (StatusCode::ACCEPTED, Json(json!({"status": "starting"})))
}

async fn service_stop(
    State(_state): State<AppState>,
    Path(_company_id): Path<Uuid>,
    Json(_body): Json<Value>,
) -> impl IntoResponse {
    (StatusCode::ACCEPTED, Json(json!({"status": "stopping"})))
}

async fn install_fixtures(
    State(_state): State<AppState>,
    Path(_company_id): Path<Uuid>,
) -> impl IntoResponse {
    (
        StatusCode::ACCEPTED,
        Json(json!({"status": "fixtures-installing"})),
    )
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct CreateRunBody {
    trigger: Option<String>,
    suite: Option<String>,
}

async fn runs_list(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows: Vec<RunRow> = sqlx::query_as(
        "SELECT id, company_id, trigger, status, started_at, finished_at, summary, created_at, updated_at \
         FROM smoke_runs WHERE company_id = $1 ORDER BY started_at DESC LIMIT 50",
    )
    .bind(company_id)
    .fetch_all(state.db.pool())
    .await?;
    let items: Vec<Value> = rows.iter().map(run_json).collect();
    Ok(Json(json!({ "companyId": company_id, "items": items })))
}

async fn runs_create(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<CreateRunBody>,
) -> ApiResult<impl IntoResponse> {
    let trigger = body
        .trigger
        .clone()
        .or_else(|| body.suite.clone())
        .unwrap_or_else(|| "manual".to_owned());
    let row: RunRow = sqlx::query_as(
        "INSERT INTO smoke_runs (company_id, trigger, status) \
         VALUES ($1, $2, 'running') \
         RETURNING id, company_id, trigger, status, started_at, finished_at, summary, created_at, updated_at",
    )
    .bind(company_id)
    .bind(&trigger)
    .fetch_one(state.db.pool())
    .await?;
    Ok((StatusCode::ACCEPTED, Json(run_json(&row))))
}

async fn runs_get(
    State(state): State<AppState>,
    Path((company_id, run_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let row: Option<RunRow> = sqlx::query_as(
        "SELECT id, company_id, trigger, status, started_at, finished_at, summary, created_at, updated_at \
         FROM smoke_runs WHERE id = $1 AND company_id = $2",
    )
    .bind(run_id)
    .bind(company_id)
    .fetch_optional(state.db.pool())
    .await?;
    match row {
        Some(row) => {
            let steps: Vec<StepRow> = sqlx::query_as(
                "SELECT id, company_id, run_id, path, scenario_step, status, detail, screenshot_artifact_ref, duration_ms, created_at, updated_at \
                 FROM smoke_run_steps WHERE run_id = $1 ORDER BY created_at ASC",
            )
            .bind(run_id)
            .fetch_all(state.db.pool())
            .await?;
            let step_items: Vec<Value> = steps.iter().map(step_json).collect();
            Ok(Json(json!({
                "run": run_json(&row),
                "steps": step_items
            })))
        }
        None => Err(ApiError::NotFound(format!("smoke run {run_id}"))),
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct StepBody {
    path: Option<String>,
    scenario_step: Option<String>,
    status: Option<String>,
    detail: Option<String>,
    duration_ms: Option<i32>,
    screenshot_artifact_ref: Option<Value>,
}

async fn runs_steps(
    State(state): State<AppState>,
    Path((company_id, run_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<StepBody>,
) -> ApiResult<impl IntoResponse> {
    let path = body.path.clone().unwrap_or_else(|| "/".to_owned());
    let scenario_step = body
        .scenario_step
        .clone()
        .unwrap_or_else(|| "step".to_owned());
    let status = body.status.clone().unwrap_or_else(|| "passed".to_owned());
    let row: StepRow = sqlx::query_as(
        "INSERT INTO smoke_run_steps (company_id, run_id, path, scenario_step, status, detail, screenshot_artifact_ref, duration_ms) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
         RETURNING id, company_id, run_id, path, scenario_step, status, detail, screenshot_artifact_ref, duration_ms, created_at, updated_at",
    )
    .bind(company_id)
    .bind(run_id)
    .bind(&path)
    .bind(&scenario_step)
    .bind(&status)
    .bind(body.detail.clone())
    .bind(body.screenshot_artifact_ref.clone())
    .bind(body.duration_ms)
    .fetch_one(state.db.pool())
    .await?;
    Ok((StatusCode::CREATED, Json(step_json(&row))))
}

async fn smoke_reset(
    State(_state): State<AppState>,
    Path(_company_id): Path<Uuid>,
) -> impl IntoResponse {
    (
        StatusCode::ACCEPTED,
        Json(json!({"status": "reset-queued"})),
    )
}
