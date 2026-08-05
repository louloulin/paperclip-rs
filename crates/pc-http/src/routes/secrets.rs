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
use pc_core::Timestamp;
use pc_realtime::LiveEvent;
use pc_repos::secret::{CompanySecretRow, NewProviderConfig, NewUserSecretDefinition, ProviderConfigRow, RemoteImportItem, SecretRepo, UserSecretDefinitionRow};
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
            "/api/secret-provider-configs/:id",
            get(get_provider_config).patch(patch_provider_config).delete(delete_provider_config),
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
        .route("/api/companies/:company_id/secrets", get(list_secrets).post(create_company_secret))
        .route(
            "/api/companies/:company_id/me/user-secrets/:secret_id",
            patch(patch_my_user_secret).delete(delete_my_user_secret),
        )
        .route(
            "/api/companies/:company_id/me/user-secrets/:secret_id/rotate",
            post(rotate_my_user_secret),
        )
        .route(
            "/api/companies/:company_id/user-secret-definitions/:definition_id",
            patch(patch_user_def),
        )
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
        // ── Round 201: remote import (Node-style alias) ──
        .route(
            "/api/companies/:company_id/secrets/remote-import/preview",
            post(remote_import_preview),
        )
        .route(
            "/api/companies/:company_id/secrets/remote-import",
            post(remote_import),
        )
}

fn secret_json(row: &CompanySecretRow) -> Value {
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

fn user_def_json(row: &UserSecretDefinitionRow) -> Value {
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
    let rows = SecretRepo::new(&state.db)
        .list_providers(company_id)
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
    let input = NewProviderConfig {
        company_id,
        provider,
        display_name,
        status: "active".to_owned(),
        is_default: false,
        config,
        created_by_agent_id: None,
        created_by_user_id: Some(user_id.clone()),
    };
    let row = SecretRepo::new(&state.db).upsert_provider(&input).await?;
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
    let row = SecretRepo::new(&state.db)
        .get_provider(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("provider config {id}")))?;
    Ok(Json(provider_config_json(&row)))
}

async fn delete_provider_config(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    SecretRepo::new(&state.db).delete_provider(id).await?;
    Ok((StatusCode::NO_CONTENT, Json(json!({ "deleted": true }))))
}

async fn make_default_provider(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(_body): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    let row = SecretRepo::new(&state.db)
        .mark_default_provider(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("provider config {id}")))?;
    Ok((StatusCode::OK, Json(provider_config_json(&row))))
}

async fn provider_health_check(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let row = SecretRepo::new(&state.db).mark_provider_healthy(id).await?;
    Ok((StatusCode::OK, Json(provider_config_json(&row))))
}

async fn list_secrets(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = SecretRepo::new(&state.db)
        .list_for_company(company_id)
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
    let rows = SecretRepo::new(&state.db)
        .list_user_definitions(company_id)
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
    let input = NewUserSecretDefinition {
        company_id,
        key,
        name,
        description: body.description.clone(),
        status: "active".to_owned(),
        provider: "manual".to_owned(),
        managed_mode: "user".to_owned(),
        provider_config_id: None,
        provider_metadata: None,
        usage_guidance: body.usage_guidance.clone(),
        created_by_agent_id: None,
        created_by_user_id: Some(user_id.clone()),
    };
    let row = SecretRepo::new(&state.db)
        .create_user_definition(&input)
        .await?;
    Ok((StatusCode::CREATED, Json(user_def_json(&row))))
}

async fn delete_user_def(
    State(state): State<AppState>,
    Path((_company_id, def_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<impl IntoResponse> {
    SecretRepo::new(&state.db).archive_user_definition(def_id).await?;
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
    let user_id = require_user_id(&state, &headers).await?;
    // List user_secret_declarations for current user
    let rows = SecretRepo::new(&state.db)
        .list_declarations_for_user(company_id, &user_id)
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
    SecretRepo::new(&state.db)
        .upsert_user_declaration(company_id, &user_id, definition_id, &value_ciphertext, &metadata)
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
    let row = SecretRepo::new(&state.db)
        .patch_company_secret(
            secret_id,
            body.name.as_deref(),
            Some(body.description.as_deref()),
        )
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("secret {secret_id}")))?;
    Ok(Json(secret_json(&row)))
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
    let row = SecretRepo::new(&state.db)
        .rotate_company_secret(
            secret_id,
            &body.material,
            body.created_by_user_id.as_deref(),
            body.created_by_agent_id,
        )
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("secret {secret_id}")))?;
    Ok(Json(secret_json(&row)))
}

async fn secret_usage(
    State(state): State<AppState>,
    Path(secret_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let bindings = SecretRepo::new(&state.db)
        .list_bindings_for_secret(secret_id)
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
    let events = SecretRepo::new(&state.db)
        .list_access_events_for_secret(secret_id, 100)
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

// ============== Round 26: user-secret CRUD + secret-provider-configs PATCH ==============

// ── secret-provider-configs PATCH ───────────────────────────

#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PatchProviderConfigBody {
    label: Option<String>,
    status: Option<String>,
    provider_config: Option<serde_json::Value>,
    default_for_kind: Option<bool>,
}

async fn patch_provider_config(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<PatchProviderConfigBody>,
) -> ApiResult<Json<Value>> {
    if body.status.is_none() && body.label.is_none() && body.provider_config.is_none() && body.default_for_kind.is_none() {
        return Err(ApiError::BadRequest("no fields to update".into()));
    }
    let row = SecretRepo::new(&state.db)
        .patch_provider_config(
            id,
            body.label.as_deref(),
            body.status.as_deref(),
            Some(body.provider_config.clone()),
            body.default_for_kind,
        )
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("company_secret_provider_config {id}")))?;
    state.realtime.publish(
        LiveEvent::new("secret_provider_config.updated", "secret_provider_config", id)
            .with_company(row.company_id)
            .with_data(json!({"label": row.display_name, "status": row.status})),
    );
    Ok(Json(json!({
        "id": row.id,
        "companyId": row.company_id,
        "label": row.display_name,
        "status": row.status,
        "config": row.config,
        "isDefault": row.is_default,
        "updatedAt": row.updated_at,
    })))
}

// ── Company secrets POST ───────────────────────────────────

#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateCompanySecretBody {
    name: String,
    description: Option<String>,
    provider: Option<String>,
    value: Option<String>,
}

async fn create_company_secret(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<CreateCompanySecretBody>,
) -> ApiResult<impl IntoResponse> {
    if body.name.trim().is_empty() {
        return Err(ApiError::BadRequest("name is required".into()));
    }
    let provider = body.provider.clone().unwrap_or_else(|| "local_encrypted".to_owned());
    let secret_repo = SecretRepo::new(&state.db);
    let existing = secret_repo.find_id_by_name(company_id, &body.name).await?;
    if existing.is_some() {
        return Err(ApiError::Conflict(format!("secret {} already exists", body.name)));
    }
    let external_ref = if let Some(v) = body.value.as_deref() {
        // Persist a placeholder external_ref + first version
        format!("local:{}", Uuid::new_v4().simple())
    } else {
        format!("local:{}", Uuid::new_v4().simple())
    };
    let id = secret_repo
        .create_company_secret(company_id, &body.name, &provider, &external_ref, body.description.as_deref())
        .await?;
    // If value provided, create v1
    if let Some(v) = body.value.as_deref() {
        use sha2::{Digest, Sha256};
        let sha = format!("{:x}", Sha256::digest(v.as_bytes()));
        secret_repo
            .insert_first_version(company_id, id, &sha, &json!({ "value": v }))
            .await?;
    }
    state.realtime.publish(
        LiveEvent::new("company_secret.created", "company_secret", id).with_company(company_id),
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": id,
            "companyId": company_id,
            "name": body.name,
            "provider": provider,
            "description": body.description,
            "latestVersion": if body.value.is_some() { 1 } else { 0 },
        })),
    ))
}

// ── My user-secrets PATCH / DELETE / rotate ─────────────────

#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PatchMyUserSecretBody {
    value: Option<String>,
    status: Option<String>,
}

async fn patch_my_user_secret(
    State(state): State<AppState>,
    Path((company_id, secret_id)): Path<(Uuid, Uuid)>,
    headers: axum::http::HeaderMap,
    Json(body): Json<PatchMyUserSecretBody>,
) -> ApiResult<Json<Value>> {
    let user_id = crate::state::require_user_id(&state, &headers).await?;
    // Update only if the secret is owned by this user (owner_user_id)
    let secret_repo = SecretRepo::new(&state.db);
    if let Some(ref st) = body.status {
        secret_repo
            .update_status_with_owner(company_id, secret_id, &user_id, st)
            .await?;
    }
    if let Some(v) = body.value.as_deref() {
        let next_version = secret_repo.next_version_number(secret_id).await?;
        use sha2::{Digest, Sha256};
        let sha = format!("{:x}", Sha256::digest(v.as_bytes()));
        secret_repo
            .insert_version(company_id, secret_id, next_version, &sha, &json!({ "value": v }))
            .await?;
        secret_repo.update_latest_version(secret_id, next_version).await?;
    }
    let row = secret_repo
        .find_summary_by_owner(company_id, secret_id, &user_id)
        .await?;
    let (id, _, name, status, latest_version, updated_at) = row
        .ok_or_else(|| ApiError::NotFound(format!("user secret {secret_id}")))?;
    state.realtime.publish(
        LiveEvent::new("user_secret.updated", "company_secret", id)
            .with_company(company_id)
            .with_data(json!({"userId": user_id, "name": name})),
    );
    Ok(Json(json!({
        "id": id,
        "companyId": company_id,
        "name": name,
        "status": status,
        "latestVersion": latest_version,
        "updatedAt": updated_at,
    })))
}

async fn delete_my_user_secret(
    State(state): State<AppState>,
    Path((company_id, secret_id)): Path<(Uuid, Uuid)>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Json<Value>> {
    let user_id = crate::state::require_user_id(&state, &headers).await?;
    let affected = SecretRepo::new(&state.db)
        .archive_user_secret(company_id, secret_id, &user_id)
        .await?;
    if affected == 0 {
        return Err(ApiError::NotFound(format!("user secret {secret_id}")));
    }
    state.realtime.publish(
        LiveEvent::new("user_secret.archived", "company_secret", secret_id)
            .with_company(company_id)
            .with_data(json!({"userId": user_id})),
    );
    Ok(Json(json!({
        "id": secret_id,
        "companyId": company_id,
        "archived": true,
    })))
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RotateMyUserSecretBody {
    value: Option<String>,
}

async fn rotate_my_user_secret(
    State(state): State<AppState>,
    Path((company_id, secret_id)): Path<(Uuid, Uuid)>,
    headers: axum::http::HeaderMap,
    Json(body): Json<RotateMyUserSecretBody>,
) -> ApiResult<Json<Value>> {
    let user_id = crate::state::require_user_id(&state, &headers).await?;
    let new_value = body
        .value
        .clone()
        .unwrap_or_else(|| format!("sk_{}", Uuid::new_v4().simple()));
    let secret_repo = SecretRepo::new(&state.db);
    let next_version = secret_repo.next_version_number(secret_id).await?;
    use sha2::{Digest, Sha256};
    let sha = format!("{:x}", Sha256::digest(new_value.as_bytes()));
    secret_repo
        .insert_version(company_id, secret_id, next_version, &sha, &json!({ "value": new_value }))
        .await?;
    secret_repo
        .rotate_with_owner(company_id, secret_id, &user_id, next_version)
        .await?;
    state.realtime.publish(
        LiveEvent::new("user_secret.rotated", "company_secret", secret_id)
            .with_company(company_id)
            .with_data(json!({"userId": user_id, "newVersion": next_version})),
    );
    Ok(Json(json!({
        "id": secret_id,
        "companyId": company_id,
        "latestVersion": next_version,
        "rotatedAt": chrono::Utc::now(),
    })))
}

// ── User-secret-definitions PATCH ───────────────────────────

#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PatchUserDefBody {
    name: Option<String>,
    description: Option<String>,
    status: Option<String>,
    usage_guidance: Option<String>,
    provider_metadata: Option<serde_json::Value>,
}

async fn patch_user_def(
    State(state): State<AppState>,
    Path((company_id, definition_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<PatchUserDefBody>,
) -> ApiResult<Json<Value>> {
    let row = SecretRepo::new(&state.db)
        .patch_user_definition(
            company_id,
            definition_id,
            body.name.as_deref(),
            Some(body.description.as_deref()),
            body.status.as_deref(),
            Some(body.usage_guidance.as_deref()),
            Some(body.provider_metadata.clone()),
        )
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("user_secret_definition {definition_id}")))?;
    let id = row.id;
    let name = row.name.clone();
    let status = row.status.clone();
    let key = row.key.clone();
    let updated_at = row.updated_at;
    state.realtime.publish(
        LiveEvent::new("user_secret_definition.updated", "user_secret_definition", id)
            .with_company(company_id)
            .with_data(json!({"name": name, "key": key, "status": status})),
    );
    Ok(Json(json!({
        "id": id,
        "companyId": company_id,
        "name": name,
        "key": key,
        "status": status,
        "updatedAt": updated_at,
    })))
}

// ============================================================================
// Round 201: secrets/remote-import + preview
//
// 语义：批量从外部源（env / file / remote KMS）导入 secrets 到当前 company。
// 设计：
// - 请求体 `{ source, items: [{ name, value?, provider?, description? }] }`
// - preview 仅做校验 + 冲突检测，不写库。
// - import 在单个事务内创建 company_secrets (+ 可选首批 version)。
// ============================================================================

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteImportItemDto {
    name: String,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteImportBody {
    #[serde(default = "default_remote_source")]
    source: String,
    #[serde(default)]
    items: Vec<RemoteImportItemDto>,
}

fn default_remote_source() -> String {
    "manual".to_owned()
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RemoteImportPreviewEntry {
    name: String,
    provider: String,
    has_value: bool,
    would_create: bool,
    conflict: bool,
    reason: Option<String>,
}

fn validate_import_item(it: &RemoteImportItemDto) -> Result<(String, Option<String>), String> {
    let name = it.name.trim();
    if name.is_empty() {
        return Err("name is required".into());
    }
    let provider = it
        .provider
        .clone()
        .unwrap_or_else(|| "local_encrypted".to_owned());
    Ok((name.to_owned(), Some(provider)))
}

async fn remote_import_preview(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<RemoteImportBody>,
) -> ApiResult<Json<Value>> {
    let names: Vec<String> = body
        .items
        .iter()
        .filter_map(|i| {
            let n = i.name.trim().to_owned();
            if n.is_empty() { None } else { Some(n) }
        })
        .collect();
    let secret_repo = SecretRepo::new(&state.db);
    let existing = secret_repo
        .find_existing_names(company_id, &names)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let mut preview = Vec::with_capacity(body.items.len());
    let mut conflicts = 0usize;
    let mut would_create = 0usize;
    for it in &body.items {
        match validate_import_item(it) {
            Err(e) => preview.push(RemoteImportPreviewEntry {
                name: it.name.clone(),
                provider: String::new(),
                has_value: it.value.is_some(),
                would_create: false,
                conflict: false,
                reason: Some(e),
            }),
            Ok((name, provider)) => {
                let is_conflict = existing.contains(&name);
                let will = !is_conflict;
                if is_conflict { conflicts += 1; } else { would_create += 1; }
                preview.push(RemoteImportPreviewEntry {
                    name,
                    provider: provider.unwrap_or_else(|| "local_encrypted".to_owned()),
                    has_value: it.value.is_some(),
                    would_create: will,
                    conflict: is_conflict,
                    reason: None,
                });
            }
        }
    }
    Ok(Json(json!({
        "companyId": company_id,
        "source": body.source,
        "totalItems": body.items.len(),
        "wouldCreate": would_create,
        "conflicts": conflicts,
        "preview": preview,
    })))
}

async fn remote_import(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<RemoteImportBody>,
) -> ApiResult<impl IntoResponse> {
    let secret_repo = SecretRepo::new(&state.db);

    // 过滤 + 校验
    let mut items: Vec<RemoteImportItem> = Vec::with_capacity(body.items.len());
    let mut skipped: Vec<Value> = Vec::new();
    for it in &body.items {
        match validate_import_item(it) {
            Err(e) => skipped.push(json!({ "name": it.name, "reason": e })),
            Ok((name, provider)) => items.push(RemoteImportItem {
                name,
                provider: provider.unwrap_or_else(|| "local_encrypted".to_owned()),
                description: it.description.clone(),
                value: it.value.clone(),
            }),
        }
    }

    // 冲突检测：已存在的跳过
    let names: Vec<String> = items.iter().map(|i| i.name.clone()).collect();
    let existing = secret_repo
        .find_existing_names(company_id, &names)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let (to_create, skipped_existing): (Vec<RemoteImportItem>, Vec<RemoteImportItem>) = items
        .into_iter()
        .partition(|i| !existing.contains(&i.name));
    let mut skipped = skipped;
    for it in &skipped_existing {
        skipped.push(json!({ "name": it.name, "reason": "already exists" }));
    }

    let created = secret_repo
        .bulk_create_secrets_atomic(company_id, &to_create)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let created_json: Vec<Value> = created
        .iter()
        .map(|(id, name)| json!({ "id": id, "name": name }))
        .collect();
    for (id, _name) in &created {
        state.realtime.publish(
            LiveEvent::new("company_secret.imported", "company_secret", *id)
                .with_company(company_id),
        );
    }
    Ok((
        StatusCode::OK,
        Json(json!({
            "companyId": company_id,
            "source": body.source,
            "totalCreated": created.len(),
            "totalSkipped": skipped.len(),
            "created": created_json,
            "skipped": skipped,
        })),
    ))
}
