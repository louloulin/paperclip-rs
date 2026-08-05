//! 公司级 skill 策略。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use pc_repos::company_skill_policy::{CompanySkillPolicyRepo, PolicyRow};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/api/companies/:company_id/skill-policy",
        get(get_skill_policy)
            .put(put_skill_policy)
            .delete(delete_skill_policy),
    )
}

#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)]
struct PolicyBody {
    #[serde(default)]
    default_effect: Option<String>,
    #[serde(default)]
    rules: Option<Value>,
    #[serde(default)]
    revision: Option<i32>,
}

fn policy_json(row: &PolicyRow) -> Value {
    json!({
        "companyId": row.company_id,
        "schemaVersion": row.schema_version,
        "revision": row.revision,
        "defaultEffect": row.default_effect,
        "rules": row.rules,
        "updatedAt": row.updated_at,
    })
}

fn default_policy(company_id: Uuid) -> Value {
    json!({
        "companyId": company_id,
        "schemaVersion": 1,
        "revision": 0,
        "defaultEffect": "allow",
        "rules": [],
        "updatedAt": null
    })
}

async fn read(state: &AppState, company_id: Uuid) -> ApiResult<Value> {
    match CompanySkillPolicyRepo::new(&state.db).fetch(company_id).await? {
        Some(row) => Ok(policy_json(&row)),
        None => Ok(default_policy(company_id)),
    }
}

async fn write(state: &AppState, company_id: Uuid, body: &PolicyBody) -> ApiResult<Value> {
    let default_effect = body
        .default_effect
        .clone()
        .unwrap_or_else(|| "allow".to_owned());
    let rules = body.rules.clone().unwrap_or_else(|| json!([]));
    let new_revision = body.revision.unwrap_or(0) + 1;
    CompanySkillPolicyRepo::new(&state.db)
        .upsert(company_id, new_revision, &default_effect, &rules)
        .await?;
    read(state, company_id).await
}

async fn get_skill_policy(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    Ok(Json(read(&state, company_id).await?))
}

async fn put_skill_policy(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<PolicyBody>,
) -> ApiResult<impl IntoResponse> {
    Ok((
        StatusCode::OK,
        Json(write(&state, company_id, &body).await?),
    ))
}

async fn delete_skill_policy(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    CompanySkillPolicyRepo::new(&state.db).delete(company_id).await?;
    Ok((
        StatusCode::OK,
        Json(json!({ "deleted": true, "companyId": company_id })),
    ))
}
