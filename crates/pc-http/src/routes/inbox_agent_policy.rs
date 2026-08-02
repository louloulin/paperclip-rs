//! 用户收件箱 AI 代理策略。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::FromRow;
use uuid::Uuid;

use crate::{require_user_id, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/companies/:company_id/users/me/inbox-agent-policy",
            get(get_my_policy).put(put_my_policy),
        )
        .route(
            "/api/companies/:company_id/users/:user_id/inbox-agent-policy",
            get(get_user_policy).put(put_user_policy),
        )
}

#[derive(Debug, FromRow)]
struct PolicyRow {
    company_id: Uuid,
    user_id: String,
    mode: String,
    allowed_agent_ids: Value,
    updated_at: pc_core::Timestamp,
}

#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)]
struct PolicyBody {
    mode: Option<String>,
    allowed_agent_ids: Option<Vec<String>>,
}

fn policy_json(row: &PolicyRow) -> Value {
    json!({
        "companyId": row.company_id,
        "userId": row.user_id,
        "mode": row.mode,
        "allowedAgentIds": row.allowed_agent_ids,
        "updatedAt": row.updated_at,
    })
}

async fn fetch_policy(
    state: &AppState,
    company_id: Uuid,
    user_id: &str,
) -> ApiResult<Option<PolicyRow>> {
    Ok(sqlx::query_as::<_, PolicyRow>(
        "SELECT company_id, user_id, mode, allowed_agent_ids, updated_at \
         FROM user_inbox_agent_policies WHERE company_id = $1 AND user_id = $2",
    )
    .bind(company_id)
    .bind(user_id)
    .fetch_optional(state.db.pool())
    .await?)
}

fn default_policy(company_id: Uuid, user_id: &str) -> Value {
    json!({
        "companyId": company_id,
        "userId": user_id,
        "mode": "open",
        "allowedAgentIds": [],
        "updatedAt": null
    })
}

async fn read_policy(state: &AppState, company_id: Uuid, user_id: &str) -> ApiResult<Value> {
    match fetch_policy(state, company_id, user_id).await? {
        Some(row) => Ok(policy_json(&row)),
        None => Ok(default_policy(company_id, user_id)),
    }
}

async fn write_policy(
    state: &AppState,
    company_id: Uuid,
    user_id: &str,
    body: &PolicyBody,
) -> ApiResult<Value> {
    let mode = body.mode.clone().unwrap_or_else(|| "open".to_owned());
    let allowed = body.allowed_agent_ids.clone().unwrap_or_default();
    let allowed_json = serde_json::to_value(allowed).unwrap_or_else(|_| json!([]));
    sqlx::query(
        "INSERT INTO user_inbox_agent_policies \
            (company_id, user_id, mode, allowed_agent_ids, updated_at) \
         VALUES ($1, $2, $3, $4, now()) \
         ON CONFLICT (company_id, user_id) DO UPDATE SET \
            mode = EXCLUDED.mode, \
            allowed_agent_ids = EXCLUDED.allowed_agent_ids, \
            updated_at = now()",
    )
    .bind(company_id)
    .bind(user_id)
    .bind(&mode)
    .bind(&allowed_json)
    .execute(state.db.pool())
    .await?;
    read_policy(state, company_id, user_id).await
}

async fn get_my_policy(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Json<Value>> {
    let user_id = require_user_id(&state, &headers).await?;
    read_policy(&state, company_id, &user_id).await.map(Json)
}

async fn put_my_policy(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    headers: axum::http::HeaderMap,
    Json(body): Json<PolicyBody>,
) -> ApiResult<impl IntoResponse> {
    let user_id = require_user_id(&state, &headers).await?;
    Ok((
        StatusCode::OK,
        Json(write_policy(&state, company_id, &user_id, &body).await?),
    ))
}

async fn get_user_policy(
    State(state): State<AppState>,
    Path((company_id, user_id)): Path<(Uuid, String)>,
) -> ApiResult<Json<Value>> {
    read_policy(&state, company_id, &user_id).await.map(Json)
}

async fn put_user_policy(
    State(state): State<AppState>,
    Path((company_id, user_id)): Path<(Uuid, String)>,
    Json(body): Json<PolicyBody>,
) -> ApiResult<impl IntoResponse> {
    Ok((
        StatusCode::OK,
        Json(write_policy(&state, company_id, &user_id, &body).await?),
    ))
}
