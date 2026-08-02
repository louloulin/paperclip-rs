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
use uuid::Uuid;

use crate::AppState;

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
            get(runs_get).patch(runs_patch),
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

#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)]
struct EmptyBody {}

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
    Path(company_id): Path<Uuid>,
) -> impl IntoResponse {
    let _ = company_id;
    (
        StatusCode::OK,
        Json(json!({"status": "redirected", "to": "smoke-lab://authorize"})),
    )
}

#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)]
struct TokenBody {
    grant_type: Option<String>,
    code: Option<String>,
}

async fn oauth_token(
    State(_state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<TokenBody>,
) -> Json<Value> {
    Json(json!({
        "companyId": company_id,
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
    Json(json!({
        "companyId": company_id,
        "sub": "smoke-lab-user",
        "email": "smoke@example.com"
    }))
}

async fn oauth_revoke(
    State(_state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(_body): Json<EmptyBody>,
) -> impl IntoResponse {
    let _ = company_id;
    (StatusCode::OK, Json(json!({"revoked": true})))
}

async fn services_list(
    State(_state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> Json<Value> {
    Json(json!({
        "companyId": company_id,
        "services": []
    }))
}

async fn service_start(
    State(_state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(_body): Json<EmptyBody>,
) -> impl IntoResponse {
    let _ = company_id;
    (StatusCode::ACCEPTED, Json(json!({"status": "starting"})))
}

async fn service_stop(
    State(_state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(_body): Json<EmptyBody>,
) -> impl IntoResponse {
    let _ = company_id;
    (StatusCode::ACCEPTED, Json(json!({"status": "stopping"})))
}

async fn install_fixtures(
    State(_state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> impl IntoResponse {
    let _ = company_id;
    (
        StatusCode::ACCEPTED,
        Json(json!({"status": "fixtures-installing"})),
    )
}

#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)]
struct CreateRunBody {
    suite: Option<String>,
}

async fn runs_list(State(_state): State<AppState>, Path(company_id): Path<Uuid>) -> Json<Value> {
    Json(json!({
        "companyId": company_id,
        "items": []
    }))
}

async fn runs_create(
    State(_state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<CreateRunBody>,
) -> impl IntoResponse {
    let _ = company_id;
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "companyId": company_id,
            "suite": body.suite.unwrap_or_default(),
            "status": "queued",
            "createdAt": chrono::Utc::now()
        })),
    )
}

async fn runs_get(
    State(_state): State<AppState>,
    Path((company_id, run_id)): Path<(Uuid, String)>,
) -> Json<Value> {
    Json(json!({
        "companyId": company_id,
        "runId": run_id,
        "status": "queued"
    }))
}

#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)]
struct PatchRunBody {
    status: Option<String>,
    note: Option<String>,
}

async fn runs_patch(
    State(_state): State<AppState>,
    Path((company_id, run_id)): Path<(Uuid, String)>,
    Json(body): Json<PatchRunBody>,
) -> impl IntoResponse {
    let _ = company_id;
    let _ = run_id;
    (
        StatusCode::OK,
        Json(json!({"status": body.status.unwrap_or_else(|| "updated".to_owned())})),
    )
}

#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)]
struct StepsBody {
    step: Option<String>,
    result: Option<String>,
}

async fn runs_steps(
    State(_state): State<AppState>,
    Path((company_id, run_id)): Path<(Uuid, String)>,
    Json(body): Json<StepsBody>,
) -> impl IntoResponse {
    let _ = company_id;
    let _ = run_id;
    let _ = body;
    (
        StatusCode::CREATED,
        Json(json!({"recorded": true, "at": chrono::Utc::now()})),
    )
}

async fn smoke_reset(
    State(_state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> impl IntoResponse {
    let _ = company_id;
    (
        StatusCode::ACCEPTED,
        Json(json!({"status": "reset-queued"})),
    )
}
