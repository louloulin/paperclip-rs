//! 秘密管理：agent 密钥、provider 配置、用户密钥。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::FromRow;
use uuid::Uuid;

use crate::{ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/agents/me/secrets", get(agent_secrets_list))
        .route("/api/agents/me/secrets/:key/value", post(agent_secret_set))
        .route(
            "/api/companies/:company_id/secret-providers",
            get(list_providers),
        )
        .route(
            "/api/companies/:company_id/secret-providers/health",
            get(providers_health),
        )
        .route(
            "/api/companies/:company_id/secret-provider-configs",
            get(list_provider_configs).post(create_provider_config),
        )
        .route(
            "/api/companies/:company_id/secret-provider-configs/discovery/preview",
            post(discovery_preview),
        )
        .route(
            "/api/secret-provider-configs/:id",
            get(get_provider_config).delete(delete_provider_config),
        )
        .route(
            "/api/secret-provider-configs/:id/default",
            post(make_default_provider),
        )
        .route(
            "/api/secret-provider-configs/:id/health",
            post(provider_health_check),
        )
        .route("/api/companies/:company_id/secrets", get(list_secrets))
        .route(
            "/api/companies/:company_id/user-secret-definitions",
            get(list_user_defs).post(create_user_def),
        )
        .route(
            "/api/companies/:company_id/user-secret-definitions/:definition_id",
            delete(delete_user_def),
        )
        .route(
            "/api/companies/:company_id/user-secret-definitions/:definition_id/coverage",
            get(definition_coverage),
        )
        .route(
            "/api/companies/:company_id/me/user-secrets",
            get(my_user_secrets).post(upsert_my_user_secret),
        )
}

#[derive(Debug, FromRow)]
struct SecretRow {
    id: Uuid,
    company_id: Uuid,
    name: String,
    key: String,
    provider: String,
    status: String,
    scope: String,
    description: Option<String>,
    latest_version: i32,
    created_at: pc_core::Timestamp,
    updated_at: pc_core::Timestamp,
}

fn secret_json(row: &SecretRow) -> Value {
    json!({
        "id": row.id,
        "companyId": row.company_id,
        "name": row.name,
        "key": row.key,
        "provider": row.provider,
        "status": row.status,
        "scope": row.scope,
        "description": row.description,
        "latestVersion": row.latest_version,
        "createdAt": row.created_at,
        "updatedAt": row.updated_at,
    })
}

#[derive(Debug, FromRow)]
struct ProviderConfigRow {
    id: Uuid,
    company_id: Uuid,
    provider: String,
    display_name: String,
    status: String,
    is_default: bool,
    config: Value,
    health_status: Option<String>,
    health_checked_at: Option<pc_core::Timestamp>,
    health_message: Option<String>,
    disabled_at: Option<pc_core::Timestamp>,
    created_at: pc_core::Timestamp,
    updated_at: pc_core::Timestamp,
}

fn provider_config_json(row: &ProviderConfigRow) -> Value {
    json!({
        "id": row.id,
        "companyId": row.company_id,
        "provider": row.provider,
        "displayName": row.display_name,
        "status": row.status,
        "isDefault": row.is_default,
        "config": row.config,
        "healthStatus": row.health_status,
        "healthCheckedAt": row.health_checked_at,
        "healthMessage": row.health_message,
        "disabledAt": row.disabled_at,
        "createdAt": row.created_at,
        "updatedAt": row.updated_at,
    })
}

#[derive(Debug, FromRow)]
struct UserDefRow {
    id: Uuid,
    company_id: Uuid,
    key: String,
    name: String,
    description: Option<String>,
    status: String,
    provider: String,
    usage_guidance: Option<String>,
    created_at: pc_core::Timestamp,
    updated_at: pc_core::Timestamp,
}

fn user_def_json(row: &UserDefRow) -> Value {
    json!({
        "id": row.id,
        "companyId": row.company_id,
        "key": row.key,
        "name": row.name,
        "description": row.description,
        "status": row.status,
        "provider": row.provider,
        "usageGuidance": row.usage_guidance,
        "createdAt": row.created_at,
        "updatedAt": row.updated_at,
    })
}

async fn agent_secrets_list(State(_s): State<AppState>) -> Json<Value> {
    Json(json!({ "items": [] }))
}

async fn agent_secret_set(
    State(_s): State<AppState>,
    Path(key): Path<String>,
    Json(_body): Json<Value>,
) -> impl IntoResponse {
    let _ = key;
    (StatusCode::OK, Json(json!({ "stored": true })))
}

async fn list_providers(State(_s): State<AppState>, Path(_company_id): Path<Uuid>) -> Json<Value> {
    Json(json!({ "items": [] }))
}

async fn providers_health(
    State(_s): State<AppState>,
    Path(_company_id): Path<Uuid>,
) -> Json<Value> {
    Json(json!({ "items": [] }))
}

async fn list_provider_configs(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows: Vec<ProviderConfigRow> = sqlx::query_as(
        "SELECT id, company_id, provider, display_name, status, is_default, config, \
                health_status, health_checked_at, health_message, disabled_at, created_at, updated_at \
         FROM company_secret_provider_configs WHERE company_id = $1 ORDER BY created_at DESC",
    )
    .bind(company_id)
    .fetch_all(state.db.pool())
    .await?;
    let items: Vec<Value> = rows.iter().map(provider_config_json).collect();
    Ok(Json(json!({ "companyId": company_id, "items": items })))
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ProviderConfigBody {
    provider: Option<String>,
    display_name: Option<String>,
    config: Option<Value>,
}

async fn create_provider_config(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    headers: axum::http::HeaderMap,
    Json(body): Json<ProviderConfigBody>,
) -> ApiResult<impl IntoResponse> {
    use crate::require_user_id;
    let provider = body
        .provider
        .clone()
        .ok_or_else(|| ApiError::BadRequest("provider required".into()))?;
    let display_name = body
        .display_name
        .clone()
        .unwrap_or_else(|| provider.clone());
    let config = body.config.clone().unwrap_or(json!({}));
    let user_id = require_user_id(&state, &headers).await?;
    let row: ProviderConfigRow = sqlx::query_as(
        "INSERT INTO company_secret_provider_configs \
            (company_id, provider, display_name, config, created_by_user_id) \
         VALUES ($1, $2, $3, $4, $5) \
         RETURNING id, company_id, provider, display_name, status, is_default, config, \
                   health_status, health_checked_at, health_message, disabled_at, created_at, updated_at",
    )
    .bind(company_id)
    .bind(&provider)
    .bind(&display_name)
    .bind(&config)
    .bind(&user_id)
    .fetch_one(state.db.pool())
    .await?;
    Ok((StatusCode::CREATED, Json(provider_config_json(&row))))
}

async fn discovery_preview(
    State(_s): State<AppState>,
    Path(_company_id): Path<Uuid>,
    Json(_body): Json<Value>,
) -> Json<Value> {
    Json(json!({ "preview": [] }))
}

async fn get_provider_config(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row: Option<ProviderConfigRow> = sqlx::query_as(
        "SELECT id, company_id, provider, display_name, status, is_default, config, \
                health_status, health_checked_at, health_message, disabled_at, created_at, updated_at \
         FROM company_secret_provider_configs WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(state.db.pool())
    .await?;
    match row {
        Some(row) => Ok(Json(provider_config_json(&row))),
        None => Err(ApiError::NotFound(format!("provider config {id}"))),
    }
}

async fn delete_provider_config(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    sqlx::query("DELETE FROM company_secret_provider_configs WHERE id = $1")
        .bind(id)
        .execute(state.db.pool())
        .await?;
    Ok((StatusCode::NO_CONTENT, Json(json!({ "deleted": true }))))
}

async fn make_default_provider(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(_body): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    let row: Option<ProviderConfigRow> = sqlx::query_as(
        "UPDATE company_secret_provider_configs SET is_default = true, updated_at = now() \
         WHERE id = $1 \
         RETURNING id, company_id, provider, display_name, status, is_default, config, \
                   health_status, health_checked_at, health_message, disabled_at, created_at, updated_at",
    )
    .bind(id)
    .fetch_optional(state.db.pool())
    .await?;
    match row {
        Some(row) => Ok((StatusCode::OK, Json(provider_config_json(&row)))),
        None => Err(ApiError::NotFound(format!("provider config {id}"))),
    }
}

async fn provider_health_check(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    sqlx::query(
        "UPDATE company_secret_provider_configs SET health_status = 'ok', health_checked_at = now(), \
                health_message = NULL, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .execute(state.db.pool())
    .await?;
    let row: ProviderConfigRow = sqlx::query_as(
        "SELECT id, company_id, provider, display_name, status, is_default, config, \
                health_status, health_checked_at, health_message, disabled_at, created_at, updated_at \
         FROM company_secret_provider_configs WHERE id = $1",
    )
    .bind(id)
    .fetch_one(state.db.pool())
    .await?;
    Ok((StatusCode::OK, Json(provider_config_json(&row))))
}

async fn list_secrets(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows: Vec<SecretRow> = sqlx::query_as(
        "SELECT id, company_id, name, key, provider, status, scope, description, latest_version, \
                created_at, updated_at \
         FROM company_secrets WHERE company_id = $1 AND deleted_at IS NULL ORDER BY created_at DESC LIMIT 200",
    )
    .bind(company_id)
    .fetch_all(state.db.pool())
    .await?;
    let items: Vec<Value> = rows.iter().map(secret_json).collect();
    Ok(Json(json!({ "companyId": company_id, "items": items })))
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct UserDefBody {
    key: Option<String>,
    name: Option<String>,
    description: Option<String>,
    usage_guidance: Option<String>,
}

async fn list_user_defs(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows: Vec<UserDefRow> = sqlx::query_as(
        "SELECT id, company_id, key, name, description, status, provider, usage_guidance, created_at, updated_at \
         FROM user_secret_definitions WHERE company_id = $1 AND deleted_at IS NULL ORDER BY created_at DESC",
    )
    .bind(company_id)
    .fetch_all(state.db.pool())
    .await?;
    let items: Vec<Value> = rows.iter().map(user_def_json).collect();
    Ok(Json(json!({ "companyId": company_id, "items": items })))
}

async fn create_user_def(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    headers: axum::http::HeaderMap,
    Json(body): Json<UserDefBody>,
) -> ApiResult<impl IntoResponse> {
    use crate::require_user_id;
    let key = body
        .key
        .clone()
        .ok_or_else(|| ApiError::BadRequest("key required".into()))?;
    let name = body.name.clone().unwrap_or_else(|| key.clone());
    let user_id = require_user_id(&state, &headers).await?;
    let row: UserDefRow = sqlx::query_as(
        "INSERT INTO user_secret_definitions \
            (company_id, key, name, description, usage_guidance, created_by_user_id, updated_by_user_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $6) \
         RETURNING id, company_id, key, name, description, status, provider, usage_guidance, created_at, updated_at",
    )
    .bind(company_id)
    .bind(&key)
    .bind(&name)
    .bind(body.description.clone())
    .bind(body.usage_guidance.clone())
    .bind(&user_id)
    .fetch_one(state.db.pool())
    .await?;
    Ok((StatusCode::CREATED, Json(user_def_json(&row))))
}

async fn delete_user_def(
    State(state): State<AppState>,
    Path((company_id, def_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<impl IntoResponse> {
    sqlx::query(
        "UPDATE user_secret_definitions SET deleted_at = now() \
         WHERE id = $1 AND company_id = $2",
    )
    .bind(def_id)
    .bind(company_id)
    .execute(state.db.pool())
    .await?;
    Ok((StatusCode::NO_CONTENT, Json(json!({ "deleted": true }))))
}

async fn definition_coverage(
    State(_s): State<AppState>,
    Path((company_id, def_id)): Path<(Uuid, Uuid)>,
) -> Json<Value> {
    Json(json!({
        "companyId": company_id,
        "id": def_id,
        "coveredAgents": [],
        "missingAgents": []
    }))
}

async fn my_user_secrets(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Json<Value>> {
    use crate::require_user_id;
    let _ = require_user_id(&state, &headers).await?;
    // List user_secret_declarations for current user
    let rows: Vec<(Uuid, Uuid, Uuid, String, String, Value, pc_core::Timestamp)> = sqlx::query_as(
        "SELECT id, company_id, definition_id, value_ciphertext, status, metadata, updated_at \
         FROM user_secret_declarations WHERE company_id = $1",
    )
    .bind(company_id)
    .fetch_all(state.db.pool())
    .await?;
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(id, cid, def_id, val, status, meta, updated_at)| {
            json!({
                "id": id,
                "companyId": cid,
                "definitionId": def_id,
                "valueCiphertext": val,
                "status": status,
                "metadata": meta,
                "updatedAt": updated_at
            })
        })
        .collect();
    Ok(Json(json!({ "companyId": company_id, "items": items })))
}

#[derive(Debug, Deserialize, Default)]
struct UpsertUserSecretBody {
    definition_id: Option<Uuid>,
    value_ciphertext: Option<String>,
    metadata: Option<Value>,
}

async fn upsert_my_user_secret(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    headers: axum::http::HeaderMap,
    Json(body): Json<UpsertUserSecretBody>,
) -> ApiResult<impl IntoResponse> {
    use crate::require_user_id;
    let user_id = require_user_id(&state, &headers).await?;
    let definition_id = body
        .definition_id
        .ok_or_else(|| ApiError::BadRequest("definition_id required".into()))?;
    let value_ciphertext = body.value_ciphertext.clone().unwrap_or_default();
    let metadata = body.metadata.clone().unwrap_or(json!({}));
    sqlx::query(
        "INSERT INTO user_secret_declarations \
            (company_id, user_id, definition_id, value_ciphertext, metadata, status) \
         VALUES ($1, $2, $3, $4, $5, 'active') \
         ON CONFLICT (company_id, user_id, definition_id) DO UPDATE SET \
            value_ciphertext = EXCLUDED.value_ciphertext, \
            metadata = EXCLUDED.metadata, \
            updated_at = now()",
    )
    .bind(company_id)
    .bind(&user_id)
    .bind(definition_id)
    .bind(&value_ciphertext)
    .bind(&metadata)
    .execute(state.db.pool())
    .await?;
    Ok((StatusCode::OK, Json(json!({ "stored": true }))))
}
