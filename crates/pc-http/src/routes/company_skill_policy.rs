//! 公司级 skill 策略。

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

use crate::{ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/api/companies/:company_id/skill-policy",
        get(get_skill_policy)
            .put(put_skill_policy)
            .delete(delete_skill_policy),
    )
}

#[derive(Debug, FromRow)]
struct PolicyRow {
    company_id: Uuid,
    schema_version: i32,
    revision: i32,
    default_effect: String,
    rules: Value,
    updated_at: pc_core::Timestamp,
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

async fn fetch(state: &AppState, company_id: Uuid) -> ApiResult<Option<PolicyRow>> {
    Ok(sqlx::query_as::<_, PolicyRow>(
        "SELECT company_id, schema_version, revision, default_effect, rules, updated_at \
         FROM company_skill_policies WHERE company_id = $1",
    )
    .bind(company_id)
    .fetch_optional(state.db.pool())
    .await?)
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
    match fetch(state, company_id).await? {
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
    sqlx::query(
        "INSERT INTO company_skill_policies \
            (company_id, schema_version, revision, default_effect, rules, updated_at) \
         VALUES ($1, 1, $2, $3, $4, now()) \
         ON CONFLICT (company_id) DO UPDATE SET \
            revision = company_skill_policies.revision + 1, \
            default_effect = EXCLUDED.default_effect, \
            rules = EXCLUDED.rules, \
            updated_at = now()",
    )
    .bind(company_id)
    .bind(new_revision)
    .bind(&default_effect)
    .bind(&rules)
    .execute(state.db.pool())
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
    sqlx::query("DELETE FROM company_skill_policies WHERE company_id = $1")
        .bind(company_id)
        .execute(state.db.pool())
        .await?;
    Ok((
        StatusCode::OK,
        Json(json!({ "deleted": true, "companyId": company_id })),
    ))
}
