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
use uuid::Uuid;

use crate::AppState;

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
            get(get_provider_config)
                .patch(patch_provider_config)
                .delete(delete_provider_config),
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

async fn agent_secrets_list(State(_s): State<AppState>) -> Json<Value> {
    Json(json!({ "items": [] }))
}

async fn agent_secret_set(
    State(_s): State<AppState>,
    Path(key): Path<String>,
    Json(_body): Json<Value>,
) -> impl IntoResponse {
    let _ = key;
    (StatusCode::OK, Json(json!({ "key": key, "stored": true })))
}

async fn list_providers(State(_s): State<AppState>, Path(company_id): Path<Uuid>) -> Json<Value> {
    let _ = company_id;
    Json(json!({ "items": [] }))
}

async fn providers_health(State(_s): State<AppState>, Path(company_id): Path<Uuid>) -> Json<Value> {
    let _ = company_id;
    Json(json!({ "items": [] }))
}

async fn list_provider_configs(
    State(_s): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> Json<Value> {
    let _ = company_id;
    Json(json!({ "items": [] }))
}

#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)]
struct ProviderConfigBody {
    provider: Option<String>,
    config: Option<Value>,
}

async fn create_provider_config(
    State(_s): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<ProviderConfigBody>,
) -> impl IntoResponse {
    let _ = company_id;
    let _ = body;
    (
        StatusCode::CREATED,
        Json(json!({ "id": "spc_new", "provider": body.provider })),
    )
}

async fn discovery_preview(
    State(_s): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(_body): Json<Value>,
) -> Json<Value> {
    let _ = company_id;
    Json(json!({ "preview": [] }))
}

async fn get_provider_config(State(_s): State<AppState>, Path(id): Path<String>) -> Json<Value> {
    Json(json!({ "id": id }))
}

async fn patch_provider_config(
    State(_s): State<AppState>,
    Path(id): Path<String>,
    Json(_body): Json<Value>,
) -> impl IntoResponse {
    (StatusCode::OK, Json(json!({ "id": id, "updated": true })))
}

async fn delete_provider_config(
    State(_s): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    (
        StatusCode::NO_CONTENT,
        Json(json!({ "id": id, "deleted": true })),
    )
}

async fn make_default_provider(
    State(_s): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    (StatusCode::OK, Json(json!({ "id": id, "isDefault": true })))
}

async fn provider_health_check(
    State(_s): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(json!({ "id": id, "status": "healthy" })),
    )
}

async fn list_secrets(State(_s): State<AppState>, Path(company_id): Path<Uuid>) -> Json<Value> {
    Json(json!({ "companyId": company_id, "items": [] }))
}

#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)]
struct UserSecretDefBody {
    name: Option<String>,
    description: Option<String>,
}

async fn list_user_defs(State(_s): State<AppState>, Path(company_id): Path<Uuid>) -> Json<Value> {
    Json(json!({ "companyId": company_id, "items": [] }))
}

async fn create_user_def(
    State(_s): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(_body): Json<UserSecretDefBody>,
) -> impl IntoResponse {
    (
        StatusCode::CREATED,
        Json(json!({ "companyId": company_id, "id": "usd_new" })),
    )
}

async fn delete_user_def(
    State(_s): State<AppState>,
    Path((company_id, def_id)): Path<(Uuid, String)>,
) -> impl IntoResponse {
    (
        StatusCode::NO_CONTENT,
        Json(json!({ "companyId": company_id, "id": def_id, "deleted": true })),
    )
}

async fn definition_coverage(
    State(_s): State<AppState>,
    Path((company_id, def_id)): Path<(Uuid, String)>,
) -> Json<Value> {
    Json(json!({
        "companyId": company_id,
        "id": def_id,
        "coveredAgents": [],
        "missingAgents": []
    }))
}

async fn my_user_secrets(State(_s): State<AppState>, Path(company_id): Path<Uuid>) -> Json<Value> {
    Json(json!({ "companyId": company_id, "items": [] }))
}

async fn upsert_my_user_secret(
    State(_s): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(_body): Json<Value>,
) -> impl IntoResponse {
    let _ = company_id;
    (StatusCode::OK, Json(json!({ "stored": true })))
}
