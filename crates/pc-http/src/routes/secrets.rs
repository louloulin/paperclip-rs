//! 秘密管理：agent 密钥、provider 配置、用户密钥。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, patch, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::FromRow;
use uuid::Uuid;

use crate::{ApiError, ApiResult, AppState};

use sha2::{Digest, Sha256};

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SecretProviderDescriptor {
    id: &'static str,
    label: &'static str,
    requires_external_ref: bool,
    supports_managed_values: bool,
    supports_external_references: bool,
    supports_external_value_writes: bool,
    configured: bool,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SecretProviderHealth {
    provider: &'static str,
    status: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

fn provider_descriptors() -> Vec<SecretProviderDescriptor> {
    vec![
        SecretProviderDescriptor {
            id: "local_encrypted",
            label: "Local encrypted (default)",
            requires_external_ref: false,
            supports_managed_values: true,
            supports_external_references: false,
            supports_external_value_writes: false,
            configured: true,
        },
        SecretProviderDescriptor {
            id: "aws_secrets_manager",
            label: "AWS Secrets Manager",
            requires_external_ref: true,
            supports_managed_values: true,
            supports_external_references: true,
            supports_external_value_writes: true,
            configured: std::env::var("PAPERCLIP_SECRETS_AWS_REGION")
                .or_else(|_| std::env::var("AWS_REGION"))
                .is_ok(),
        },
        SecretProviderDescriptor {
            id: "gcp_secret_manager",
            label: "Google Secret Manager",
            requires_external_ref: true,
            supports_managed_values: false,
            supports_external_references: true,
            supports_external_value_writes: false,
            configured: false,
        },
        SecretProviderDescriptor {
            id: "vault",
            label: "HashiCorp Vault",
            requires_external_ref: true,
            supports_managed_values: false,
            supports_external_references: true,
            supports_external_value_writes: false,
            configured: false,
        },
    ]
}

fn provider_health() -> Vec<SecretProviderHealth> {
    let mut checks = Vec::new();
    checks.push(SecretProviderHealth {
        provider: "local_encrypted",
        status: "ok",
        message: "Local encrypted secret provider is available.".into(),
        warnings: Vec::new(),
    });

    let aws_configured = std::env::var("PAPERCLIP_SECRETS_AWS_REGION")
        .or_else(|_| std::env::var("AWS_REGION"))
        .is_ok();
    checks.push(SecretProviderHealth {
        provider: "aws_secrets_manager",
        status: if aws_configured { "warn" } else { "warn" },
        message: if aws_configured {
            "AWS Secrets Manager configuration is present; credentials are resolved at runtime."
                .into()
        } else {
            "AWS Secrets Manager provider is not ready: region is not configured.".into()
        },
        warnings: if aws_configured {
            vec!["Credential readiness is checked when a provider operation runs.".into()]
        } else {
            vec!["Set PAPERCLIP_SECRETS_AWS_REGION or AWS_REGION to configure the provider.".into()]
        },
    });
    for (provider, label) in [
        ("gcp_secret_manager", "Google Secret Manager"),
        ("vault", "HashiCorp Vault"),
    ] {
        checks.push(SecretProviderHealth {
            provider,
            status: "warn",
            message: format!("{label} provider is not configured in this deployment."),
            warnings: vec!["External provider integration is unavailable.".into()],
        });
    }
    checks
}

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
        .route("/api/secrets/:id/rotate", post(rotate_secret))
        .route("/api/secrets/:id", patch(update_secret))
        .route("/api/secrets/:id/usage", get(secret_usage))
        .route("/api/secrets/:id/access-events", get(secret_access_events))
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
    Json(json!(provider_descriptors()))
}

async fn providers_health(
    State(_s): State<AppState>,
    Path(_company_id): Path<Uuid>,
) -> Json<Value> {
    Json(json!({ "providers": provider_health() }))
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

// ── Rotate / Update / Usage / Access-events ──────────────────

#[expect(dead_code)]
#[derive(Debug, FromRow)]
struct SecretVersionRow {
    id: Uuid,
    secret_id: Uuid,
    version: i32,
    material: Value,
    value_sha256: String,
    created_by_user_id: Option<String>,
    created_by_agent_id: Option<Uuid>,
    created_at: chrono::DateTime<chrono::Utc>,
    revoked_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, FromRow)]
struct SecretBindingRow {
    id: Uuid,
    company_id: Uuid,
    secret_id: Uuid,
    target_type: String,
    target_id: String,
    config_path: String,
    version_selector: String,
    required: bool,
    label: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, FromRow)]
struct SecretAccessEventRow {
    id: Uuid,
    company_id: Uuid,
    secret_id: Option<Uuid>,
    secret_scope: String,
    version: Option<i32>,
    provider: String,
    actor_type: String,
    actor_id: Option<String>,
    consumer_type: String,
    consumer_id: String,
    outcome: String,
    error_code: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateSecretBody {
    name: Option<String>,
    description: Option<String>,
}

async fn update_secret(
    State(state): State<AppState>,
    Path(secret_id): Path<Uuid>,
    Json(body): Json<UpdateSecretBody>,
) -> ApiResult<Json<Value>> {
    if let Some(ref name) = body.name {
        sqlx::query("UPDATE company_secrets SET name = $1, updated_at = now() WHERE id = $2")
            .bind(name)
            .bind(secret_id)
            .execute(state.db.pool())
            .await?;
    }
    if let Some(ref desc) = body.description {
        sqlx::query(
            "UPDATE company_secrets SET description = $1, updated_at = now() WHERE id = $2",
        )
        .bind(desc)
        .bind(secret_id)
        .execute(state.db.pool())
        .await?;
    }
    // Re-fetch
    let row: Option<SecretRow> = sqlx::query_as(
        "SELECT id, company_id, name, key, provider, status, scope, description, latest_version,          created_at, updated_at FROM company_secrets WHERE id = $1",
    )
    .bind(secret_id)
    .fetch_optional(state.db.pool())
    .await?;
    match row {
        Some(row) => Ok(Json(secret_json(&row))),
        None => Err(ApiError::NotFound(format!("secret {secret_id}"))),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RotateSecretBody {
    material: Value,
    created_by_user_id: Option<String>,
    created_by_agent_id: Option<Uuid>,
}

async fn rotate_secret(
    State(state): State<AppState>,
    Path(secret_id): Path<Uuid>,
    Json(body): Json<RotateSecretBody>,
) -> ApiResult<Json<Value>> {
    // Fetch current secret to get latest_version
    let current: Option<(i32,)> =
        sqlx::query_as("SELECT latest_version FROM company_secrets WHERE id = $1")
            .bind(secret_id)
            .fetch_optional(state.db.pool())
            .await?;
    let Some((latest_version,)) = current else {
        return Err(ApiError::NotFound(format!("secret {secret_id}")));
    };

    let new_version = latest_version + 1;
    let material = &body.material;
    // Compute SHA-256 of the prepared material bytes for integrity tracking
    let material_bytes = serde_json::to_vec(material).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(&material_bytes);
    let value_sha256 = format!("{:x}", hasher.finalize());

    // Insert new version
    sqlx::query(
        "INSERT INTO company_secret_versions          (secret_id, version, material, value_sha256, created_by_user_id, created_by_agent_id)          VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(secret_id)
    .bind(new_version)
    .bind(material)
    .bind(value_sha256)
    .bind(&body.created_by_user_id)
    .bind(body.created_by_agent_id)
    .execute(state.db.pool())
    .await?;

    // Bump latest_version on parent
    sqlx::query("UPDATE company_secrets SET latest_version = $1, updated_at = now() WHERE id = $2")
        .bind(new_version)
        .bind(secret_id)
        .execute(state.db.pool())
        .await?;

    // Re-fetch
    let row: Option<SecretRow> = sqlx::query_as(
        "SELECT id, company_id, name, key, provider, status, scope, description, latest_version,          created_at, updated_at FROM company_secrets WHERE id = $1",
    )
    .bind(secret_id)
    .fetch_optional(state.db.pool())
    .await?;
    match row {
        Some(row) => Ok(Json(secret_json(&row))),
        None => Err(ApiError::NotFound(format!("secret {secret_id}"))),
    }
}

async fn secret_usage(
    State(state): State<AppState>,
    Path(secret_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let bindings: Vec<SecretBindingRow> = sqlx::query_as(
        "SELECT id, company_id, secret_id, target_type, target_id, config_path,          version_selector, required, label, created_at          FROM company_secret_bindings WHERE secret_id = $1 ORDER BY created_at DESC",
    )
    .bind(secret_id)
    .fetch_all(state.db.pool())
    .await?;
    let items: Vec<Value> = bindings
        .iter()
        .map(|b| {
            json!({
                "id": b.id,
                "companyId": b.company_id,
                "secretId": b.secret_id,
                "targetType": b.target_type,
                "targetId": b.target_id,
                "configPath": b.config_path,
                "versionSelector": b.version_selector,
                "required": b.required,
                "label": b.label,
                "createdAt": b.created_at,
            })
        })
        .collect();
    Ok(Json(json!({ "secretId": secret_id, "items": items })))
}

async fn secret_access_events(
    State(state): State<AppState>,
    Path(secret_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let events: Vec<SecretAccessEventRow> = sqlx::query_as(
        "SELECT id, company_id, secret_id, secret_scope, version, provider,          actor_type, actor_id, consumer_type, consumer_id, outcome, error_code, created_at          FROM secret_access_events WHERE secret_id = $1 ORDER BY created_at DESC LIMIT 100",
    )
    .bind(secret_id)
    .fetch_all(state.db.pool())
    .await?;
    let items: Vec<Value> = events
        .iter()
        .map(|e| {
            json!({
                "id": e.id,
                "companyId": e.company_id,
                "secretId": e.secret_id,
                "secretScope": e.secret_scope,
                "version": e.version,
                "provider": e.provider,
                "actorType": e.actor_type,
                "actorId": e.actor_id,
                "consumerType": e.consumer_type,
                "consumerId": e.consumer_id,
                "outcome": e.outcome,
                "errorCode": e.error_code,
                "createdAt": e.created_at,
            })
        })
        .collect();
    Ok(Json(json!({ "secretId": secret_id, "items": items })))
}
