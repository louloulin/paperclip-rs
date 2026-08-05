//! 用户收件箱 AI 代理策略。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use pc_repos::inbox_agent_policy::{
    InboxAgentPolicy, InboxAgentPolicyMode, InboxAgentPolicyRepo, UpdateInboxAgentPolicyInput,
};
use serde::Deserialize;
use serde_json::{json, Value};
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

#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)]
struct PolicyBody {
    mode: Option<String>,
    allowed_agent_ids: Option<Vec<String>>,
}

fn policy_json(p: &InboxAgentPolicy) -> Value {
    json!({
        "companyId": p.company_id,
        "userId": p.user_id,
        "mode": p.mode.as_str(),
        "allowedAgentIds": p.allowed_agent_ids,
        "materialized": p.materialized,
        "updatedAt": p.updated_at,
    })
}

async fn read_policy(state: &AppState, company_id: Uuid, user_id: &str) -> ApiResult<Value> {
    let p = InboxAgentPolicyRepo::new(&state.db)
        .get(company_id, user_id)
        .await?;
    Ok(policy_json(&p))
}

async fn write_policy(
    state: &AppState,
    company_id: Uuid,
    user_id: &str,
    body: &PolicyBody,
) -> ApiResult<Value> {
    let mode = InboxAgentPolicyMode::parse(body.mode.as_deref().unwrap_or("open"))
        .unwrap_or(InboxAgentPolicyMode::Open);
    let allowed: Vec<Uuid> = body
        .allowed_agent_ids
        .clone()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|s| Uuid::parse_str(&s).ok())
        .collect();
    let p = InboxAgentPolicyRepo::new(&state.db)
        .update(
            company_id,
            user_id,
            UpdateInboxAgentPolicyInput {
                mode,
                allowed_agent_ids: allowed,
            },
        )
        .await
        .map_err(|e| crate::ApiError::Internal(e.to_string()))?;
    Ok(policy_json(&p))
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
