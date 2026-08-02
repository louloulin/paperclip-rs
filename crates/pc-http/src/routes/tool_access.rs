//! 工具访问：connections、tool gallery、connections OAuth。

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
            "/api/agents/me/connections/:connection_id/start-authorization",
            post(start_connection_authz),
        )
        .route(
            "/api/agents/me/connections/:connection_id/token",
            post(connection_token),
        )
        .route(
            "/api/companies/:company_id/tools/gallery",
            get(tool_gallery),
        )
        .route(
            "/api/companies/:company_id/tools/apps/connect",
            post(connect_tool_app),
        )
        .route(
            "/api/companies/:company_id/tools/connections/:connection_id/start-authorization",
            post(start_company_connection_authz),
        )
        .route("/api/tools/oauth/:connection_id/start", post(oauth_start))
        .route("/api/tools/oauth/callback", get(oauth_callback))
        .route(
            "/api/companies/:company_id/tools/apps/:connection_id/finish",
            post(finish_oauth),
        )
        .route(
            "/api/companies/:company_id/tools/connections",
            get(list_connections).post(create_connection),
        )
        .route(
            "/api/companies/:company_id/tools/connections/:connection_id",
            get(get_connection).delete(delete_connection),
        )
        .route(
            "/api/companies/:company_id/tools/categories",
            get(tool_categories),
        )
        .route("/api/companies/:company_id/tools/lookup", post(tool_lookup))
        .route(
            "/api/companies/:company_id/tools/:tool_id",
            get(get_tool).delete(delete_tool),
        )
        .route(
            "/api/companies/:company_id/tools/:tool_id/invoke",
            post(invoke_tool),
        )
        .route(
            "/api/companies/:company_id/tools/invocations",
            get(list_invocations),
        )
}

#[derive(Debug, FromRow)]
struct ConnectionRow {
    id: Uuid,
    company_id: Uuid,
    application_id: Uuid,
    name: String,
    transport: String,
    status: String,
    enabled: bool,
    config: Value,
    credential_refs: Value,
    health_status: String,
    health_message: Option<String>,
    last_health_at: Option<pc_core::Timestamp>,
    last_catalog_refresh_at: Option<pc_core::Timestamp>,
    created_at: pc_core::Timestamp,
    updated_at: pc_core::Timestamp,
}

fn connection_json(row: &ConnectionRow) -> Value {
    json!({
        "id": row.id,
        "companyId": row.company_id,
        "applicationId": row.application_id,
        "name": row.name,
        "transport": row.transport,
        "status": row.status,
        "enabled": row.enabled,
        "config": row.config,
        "credentialRefs": row.credential_refs,
        "healthStatus": row.health_status,
        "healthMessage": row.health_message,
        "lastHealthAt": row.last_health_at,
        "lastCatalogRefreshAt": row.last_catalog_refresh_at,
        "createdAt": row.created_at,
        "updatedAt": row.updated_at,
    })
}

async fn start_connection_authz(
    State(_s): State<AppState>,
    Path(connection_id): Path<String>,
    Json(_body): Json<Value>,
) -> Json<Value> {
    Json(json!({
        "connectionId": connection_id,
        "authorizationUrl": null
    }))
}

async fn connection_token(
    State(_s): State<AppState>,
    Path(connection_id): Path<String>,
    Json(_body): Json<Value>,
) -> Json<Value> {
    Json(json!({
        "connectionId": connection_id,
        "token": "stub-token"
    }))
}

async fn tool_gallery(State(_s): State<AppState>, Path(company_id): Path<Uuid>) -> Json<Value> {
    let _ = company_id;
    Json(json!({ "items": [] }))
}

async fn connect_tool_app(
    State(_s): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(_body): Json<Value>,
) -> impl IntoResponse {
    let _ = company_id;
    (
        StatusCode::ACCEPTED,
        Json(json!({ "status": "connect-queued" })),
    )
}

async fn start_company_connection_authz(
    State(_s): State<AppState>,
    Path((_company_id, connection_id)): Path<(Uuid, String)>,
    Json(_body): Json<Value>,
) -> Json<Value> {
    Json(json!({
        "connectionId": connection_id,
        "authorizationUrl": null
    }))
}

async fn oauth_start(
    State(_s): State<AppState>,
    Path(connection_id): Path<String>,
    Json(_body): Json<Value>,
) -> impl IntoResponse {
    let _ = connection_id;
    (
        StatusCode::ACCEPTED,
        Json(json!({ "status": "oauth-started" })),
    )
}

async fn oauth_callback(State(_s): State<AppState>) -> impl IntoResponse {
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "text/html")],
        "<html><body>OAuth callback received</body></html>".to_string(),
    )
}

async fn finish_oauth(
    State(_s): State<AppState>,
    Path((_company_id, _connection_id)): Path<(Uuid, String)>,
    Json(_body): Json<Value>,
) -> impl IntoResponse {
    (StatusCode::OK, Json(json!({ "finished": true })))
}

async fn list_connections(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows: Vec<ConnectionRow> = sqlx::query_as(
        "SELECT id, company_id, application_id, name, transport, status, enabled, config, \
                credential_refs, health_status, health_message, last_health_at, last_catalog_refresh_at, \
                created_at, updated_at \
         FROM tool_connections WHERE company_id = $1 ORDER BY created_at DESC",
    )
    .bind(company_id)
    .fetch_all(state.db.pool())
    .await?;
    let items: Vec<Value> = rows.iter().map(connection_json).collect();
    Ok(Json(json!({ "companyId": company_id, "items": items })))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateConnectionBody {
    application_id: Option<Uuid>,
    name: Option<String>,
    transport: Option<String>,
    config: Option<Value>,
}

async fn create_connection(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    headers: axum::http::HeaderMap,
    Json(body): Json<CreateConnectionBody>,
) -> ApiResult<impl IntoResponse> {
    use crate::require_user_id;
    let application_id = body
        .application_id
        .ok_or_else(|| ApiError::BadRequest("application_id required".into()))?;
    let user_id = require_user_id(&state, &headers).await?;
    let name = body
        .name
        .clone()
        .unwrap_or_else(|| "new-connection".to_owned());
    let transport = body.transport.clone().unwrap_or_else(|| "http".to_owned());
    let config = body.config.clone().unwrap_or(json!({}));
    let uid = format!("tc_{}", Uuid::now_v7().simple());
    let row: ConnectionRow = sqlx::query_as(
        "INSERT INTO tool_connections             (company_id, application_id, name, transport, config, uid, created_by_user_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) \
         RETURNING id, company_id, application_id, name, transport, status, enabled, config, \
                   credential_refs, health_status, health_message, last_health_at, \
                   last_catalog_refresh_at, created_at, updated_at",
    )
    .bind(company_id)
    .bind(application_id)
    .bind(&name)
    .bind(&transport)
    .bind(&config)
    .bind(&uid)
    .bind(&user_id)
    .fetch_one(state.db.pool())
    .await?;
    Ok((StatusCode::CREATED, Json(connection_json(&row))))
}

async fn get_connection(
    State(state): State<AppState>,
    Path((company_id, connection_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let row: Option<ConnectionRow> = sqlx::query_as(
        "SELECT id, company_id, application_id, name, transport, status, enabled, config, \
                credential_refs, health_status, health_message, last_health_at, last_catalog_refresh_at, \
                created_at, updated_at \
         FROM tool_connections WHERE id = $1 AND company_id = $2",
    )
    .bind(connection_id)
    .bind(company_id)
    .fetch_optional(state.db.pool())
    .await?;
    match row {
        Some(row) => Ok(Json(connection_json(&row))),
        None => Err(ApiError::NotFound(format!("connection {connection_id}"))),
    }
}

async fn delete_connection(
    State(state): State<AppState>,
    Path((company_id, connection_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<impl IntoResponse> {
    sqlx::query("DELETE FROM tool_connections WHERE id = $1 AND company_id = $2")
        .bind(connection_id)
        .bind(company_id)
        .execute(state.db.pool())
        .await?;
    Ok((StatusCode::NO_CONTENT, Json(json!({ "deleted": true }))))
}

async fn tool_categories(State(_s): State<AppState>, Path(company_id): Path<Uuid>) -> Json<Value> {
    let _ = company_id;
    Json(json!({ "items": [] }))
}

async fn tool_lookup(
    State(_s): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(_body): Json<Value>,
) -> impl IntoResponse {
    let _ = company_id;
    (StatusCode::OK, Json(json!({ "tools": [] })))
}

async fn get_tool(
    State(_s): State<AppState>,
    Path((_company_id, tool_id)): Path<(Uuid, String)>,
) -> Json<Value> {
    Json(json!({ "id": tool_id }))
}

async fn delete_tool(
    State(_s): State<AppState>,
    Path((_company_id, tool_id)): Path<(Uuid, String)>,
) -> impl IntoResponse {
    let _ = tool_id;
    (StatusCode::NO_CONTENT, Json(json!({ "deleted": true })))
}

async fn invoke_tool(
    State(_s): State<AppState>,
    Path((_company_id, tool_id)): Path<(Uuid, String)>,
    Json(_body): Json<Value>,
) -> impl IntoResponse {
    let _ = tool_id;
    (StatusCode::OK, Json(json!({ "result": null })))
}

async fn list_invocations(State(_s): State<AppState>, Path(company_id): Path<Uuid>) -> Json<Value> {
    let _ = company_id;
    Json(json!({ "items": [] }))
}
