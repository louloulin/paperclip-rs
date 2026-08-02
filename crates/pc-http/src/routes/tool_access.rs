/// 工具访问：connections、tool gallery、catalog、invocations。
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::FromRow;
use std::fmt::Write;
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

// ── Row types ──────────────────────────────────────────────

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
    last_health_at: Option<chrono::DateTime<chrono::Utc>>,
    last_catalog_refresh_at: Option<chrono::DateTime<chrono::Utc>>,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
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

#[derive(Debug, FromRow)]
struct ApplicationRow {
    id: Uuid,
    company_id: Uuid,
    name: String,
    r#type: String,
    status: String,
    metadata: Value,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

fn application_json(row: &ApplicationRow) -> Value {
    json!({
        "id": row.id,
        "companyId": row.company_id,
        "name": row.name,
        "type": row.r#type,
        "status": row.status,
        "metadata": row.metadata,
        "createdAt": row.created_at,
        "updatedAt": row.updated_at,
    })
}

#[derive(Debug, FromRow)]
struct CatalogEntryRow {
    id: Uuid,
    company_id: Uuid,
    connection_id: Uuid,
    name: String,
    title: Option<String>,
    description: Option<String>,
    input_schema: Value,
    risk_level: String,
    status: String,
    created_at: chrono::DateTime<chrono::Utc>,
}

fn catalog_entry_json(row: &CatalogEntryRow) -> Value {
    json!({
        "id": row.id,
        "companyId": row.company_id,
        "connectionId": row.connection_id,
        "name": row.name,
        "title": row.title,
        "description": row.description,
        "inputSchema": row.input_schema,
        "riskLevel": row.risk_level,
        "status": row.status,
        "createdAt": row.created_at,
    })
}

#[derive(Debug, FromRow)]
struct InvocationRow {
    id: Uuid,
    company_id: Uuid,
    actor_type: String,
    actor_id: Option<String>,
    agent_id: Option<Uuid>,
    issue_id: Option<Uuid>,
    run_id: Option<Uuid>,
    connection_id: Option<Uuid>,
    catalog_entry_id: Option<Uuid>,
    tool_name: String,
    status: String,
    result_summary: Option<Value>,
    error_code: Option<String>,
    error_message: Option<String>,
    started_at: Option<chrono::DateTime<chrono::Utc>>,
    completed_at: Option<chrono::DateTime<chrono::Utc>>,
    created_at: chrono::DateTime<chrono::Utc>,
}

fn invocation_json(row: &InvocationRow) -> Value {
    json!({
        "id": row.id,
        "companyId": row.company_id,
        "actorType": row.actor_type,
        "actorId": row.actor_id,
        "agentId": row.agent_id,
        "issueId": row.issue_id,
        "runId": row.run_id,
        "connectionId": row.connection_id,
        "catalogEntryId": row.catalog_entry_id,
        "toolName": row.tool_name,
        "status": row.status,
        "resultSummary": row.result_summary,
        "errorCode": row.error_code,
        "errorMessage": row.error_message,
        "startedAt": row.started_at,
        "completedAt": row.completed_at,
        "createdAt": row.created_at,
    })
}

// ── Handlers ───────────────────────────────────────────────

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
) -> impl IntoResponse {
    let _ = connection_id;
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({
            "error": "token issuance not yet implemented"
        })),
    )
}

async fn tool_gallery(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let apps: Vec<ApplicationRow> = sqlx::query_as(
        "SELECT id, company_id, name, type, status, metadata, created_at, updated_at \
         FROM tool_applications WHERE company_id = $1 AND status = 'active' \
         ORDER BY created_at DESC",
    )
    .bind(company_id)
    .fetch_all(state.db.pool())
    .await?;
    let items: Vec<Value> = apps.iter().map(application_json).collect();
    Ok(Json(json!({ "companyId": company_id, "items": items })))
}

async fn connect_tool_app(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    let name = body["name"]
        .as_str()
        .unwrap_or("unnamed-connection")
        .to_owned();
    let transport = body["transport"].as_str().unwrap_or("http").to_owned();
    let config = body.get("config").cloned().unwrap_or(json!({}));
    let app_type = body["type"].as_str().unwrap_or("mcp").to_owned();

    // Create application if body contains application-level fields
    let application_id: Uuid = if let Some(existing_id) = body["applicationId"].as_str() {
        Uuid::parse_str(existing_id)
            .map_err(|_| ApiError::BadRequest("invalid applicationId".into()))?
    } else {
        sqlx::query_scalar(
            "INSERT INTO tool_applications (company_id, name, type, metadata) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (company_id, name) DO UPDATE SET updated_at = now() \
             RETURNING id",
        )
        .bind(company_id)
        .bind(&name)
        .bind(&app_type)
        .bind(json!({}))
        .fetch_one(state.db.pool())
        .await?
    };

    let uid = format!("tc_{}", Uuid::now_v7().simple());
    let row: ConnectionRow = sqlx::query_as(
        "INSERT INTO tool_connections \
         (company_id, application_id, name, transport, config, uid) \
         VALUES ($1, $2, $3, $4, $5, $6) \
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
    .fetch_one(state.db.pool())
    .await?;

    Ok((StatusCode::CREATED, Json(connection_json(&row))))
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
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({
            "error": "OAuth flow not yet implemented"
        })),
    )
}

async fn oauth_callback() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "text/html")],
        "<html><body><h1>OAuth Complete</h1><p>You may close this window.</p></body></html>",
    )
}

async fn finish_oauth(
    State(_s): State<AppState>,
    Path((_company_id, _connection_id)): Path<(Uuid, String)>,
    Json(_body): Json<Value>,
) -> impl IntoResponse {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({
            "error": "OAuth finish not yet implemented"
        })),
    )
}

async fn list_connections(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows: Vec<ConnectionRow> = sqlx::query_as(
        "SELECT id, company_id, application_id, name, transport, status, enabled, config, \
                credential_refs, health_status, health_message, last_health_at, \
                last_catalog_refresh_at, created_at, updated_at \
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
        "INSERT INTO tool_connections \
         (company_id, application_id, name, transport, config, uid) \
         VALUES ($1, $2, $3, $4, $5, $6) \
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
    .fetch_one(state.db.pool())
    .await?;

    // Log activity
    let _ = sqlx::query(
        "INSERT INTO activity_log \
         (company_id, actor_type, actor_id, action, entity_type, entity_id, details) \
         VALUES ($1, 'user', $2, 'tool_connection.created', 'tool_connection', $3, $4)",
    )
    .bind(company_id)
    .bind(&user_id)
    .bind(row.id)
    .bind(json!({ "name": &name }))
    .execute(state.db.pool())
    .await;

    Ok((StatusCode::CREATED, Json(connection_json(&row))))
}

async fn get_connection(
    State(state): State<AppState>,
    Path((company_id, connection_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let row: Option<ConnectionRow> = sqlx::query_as(
        "SELECT id, company_id, application_id, name, transport, status, enabled, config, \
                credential_refs, health_status, health_message, last_health_at, \
                last_catalog_refresh_at, created_at, updated_at \
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

// ── Catalog + categories + lookup ──────────────────────────

async fn tool_categories(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    // Return distinct risk_level values as categories
    let categories: Vec<(String,)> = sqlx::query_as(
        "SELECT DISTINCT risk_level FROM tool_catalog_entries \
         WHERE company_id = $1 AND status = 'active' ORDER BY risk_level",
    )
    .bind(company_id)
    .fetch_all(state.db.pool())
    .await?;
    let items: Vec<Value> = categories
        .into_iter()
        .map(|(r,)| json!({ "category": r }))
        .collect();
    Ok(Json(json!({ "companyId": company_id, "items": items })))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolLookupQuery {
    q: Option<String>,
    risk_level: Option<String>,
    connection_id: Option<Uuid>,
}

async fn tool_lookup(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(query): Json<ToolLookupQuery>,
) -> ApiResult<Json<Value>> {
    let has_q = query.q.as_deref().is_some_and(|s| !s.is_empty());
    let has_risk = query.risk_level.is_some();
    let has_cid = query.connection_id.is_some();

    let mut sql = String::from(
        "SELECT id, company_id, connection_id, name, title, description, \
         input_schema, risk_level, status, created_at \
         FROM tool_catalog_entries WHERE company_id = $1 AND status = 'active'",
    );

    // Always bind all 4 params; unused ones are harmless (never referenced in SQL)
    if has_q {
        sql.push_str(" AND (name ILIKE $2 OR title ILIKE $2 OR description ILIKE $2)");
    }
    if has_risk {
        let _ = write!(sql, " AND risk_level = ${n}", n = 2 + i32::from(has_q));
    }
    if has_cid {
        let _ = write!(
            sql,
            " AND connection_id = ${n}",
            n = 2 + i32::from(has_q) + i32::from(has_risk)
        );
    }
    sql.push_str(" ORDER BY name LIMIT 100");

    let like_pat = query
        .q
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|s| format!("%{s}%"))
        .unwrap_or_default();
    let risk_val = query.risk_level.clone().unwrap_or_default();
    let cid = query.connection_id.unwrap_or_else(Uuid::nil);

    let rows: Vec<CatalogEntryRow> = sqlx::query_as(&sql)
        .bind(company_id)
        .bind(&like_pat)
        .bind(&risk_val)
        .bind(cid)
        .fetch_all(state.db.pool())
        .await?;
    let tools: Vec<Value> = rows.iter().map(catalog_entry_json).collect();
    Ok(Json(json!({ "companyId": company_id, "tools": tools })))
}

async fn get_tool(
    State(state): State<AppState>,
    Path((_company_id, tool_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let row: Option<CatalogEntryRow> = sqlx::query_as(
        "SELECT id, company_id, connection_id, name, title, description, \
         input_schema, risk_level, status, created_at \
         FROM tool_catalog_entries WHERE id = $1 AND status = 'active'",
    )
    .bind(tool_id)
    .fetch_optional(state.db.pool())
    .await?;
    match row {
        Some(row) => Ok(Json(catalog_entry_json(&row))),
        None => Err(ApiError::NotFound(format!("tool {tool_id}"))),
    }
}

async fn delete_tool(
    State(state): State<AppState>,
    Path((_company_id, tool_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<impl IntoResponse> {
    sqlx::query("UPDATE tool_catalog_entries SET status = 'quarantined' WHERE id = $1")
        .bind(tool_id)
        .execute(state.db.pool())
        .await?;
    Ok((StatusCode::NO_CONTENT, Json(json!({ "deleted": true }))))
}

// ── Invocations ────────────────────────────────────────────

async fn invoke_tool(
    State(state): State<AppState>,
    Path((company_id, tool_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    // Look up the catalog entry to get tool_name + connection_id
    let entry: Option<CatalogEntryRow> = sqlx::query_as(
        "SELECT id, company_id, connection_id, name, title, description, \
         input_schema, risk_level, status, created_at \
         FROM tool_catalog_entries WHERE id = $1 AND company_id = $2 AND status = 'active'",
    )
    .bind(tool_id)
    .bind(company_id)
    .fetch_optional(state.db.pool())
    .await?;

    let Some(entry) = entry else {
        return Err(ApiError::NotFound(format!("tool {tool_id}")));
    };

    // Insert invocation record
    let arguments_summary = body.get("arguments").cloned();
    let row: InvocationRow = sqlx::query_as(
        "INSERT INTO tool_invocations \
         (company_id, actor_type, connection_id, catalog_entry_id, tool_name, \
          arguments_summary, status, started_at) \
         VALUES ($1, 'user', $2, $3, $4, $5, 'pending', now()) \
         RETURNING id, company_id, actor_type, actor_id, agent_id, issue_id, run_id, \
                   connection_id, catalog_entry_id, tool_name, status, result_summary, \
                   error_code, error_message, started_at, completed_at, created_at",
    )
    .bind(company_id)
    .bind(entry.connection_id)
    .bind(tool_id)
    .bind(&entry.name)
    .bind(&arguments_summary)
    .fetch_one(state.db.pool())
    .await?;

    // For now: invocation is queued; actual execution requires runtime slots / MCP bridge
    Ok((StatusCode::ACCEPTED, Json(invocation_json(&row))))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InvocationListQuery {
    connection_id: Option<Uuid>,
    #[expect(dead_code)]
    status: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

async fn list_invocations(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Query(q): Query<InvocationListQuery>,
) -> ApiResult<Json<Value>> {
    let limit = q.limit.unwrap_or(50).min(200);
    let offset = q.offset.unwrap_or(0);

    let rows: Vec<InvocationRow> = if let Some(cid) = q.connection_id {
        sqlx::query_as(
            "SELECT id, company_id, actor_type, actor_id, agent_id, issue_id, run_id, \
             connection_id, catalog_entry_id, tool_name, status, result_summary, \
             error_code, error_message, started_at, completed_at, created_at \
             FROM tool_invocations \
             WHERE company_id = $1 AND connection_id = $2 \
             ORDER BY created_at DESC LIMIT $3 OFFSET $4",
        )
        .bind(company_id)
        .bind(cid)
        .bind(limit)
        .bind(offset)
        .fetch_all(state.db.pool())
        .await?
    } else {
        sqlx::query_as(
            "SELECT id, company_id, actor_type, actor_id, agent_id, issue_id, run_id, \
             connection_id, catalog_entry_id, tool_name, status, result_summary, \
             error_code, error_message, started_at, completed_at, created_at \
             FROM tool_invocations \
             WHERE company_id = $1 \
             ORDER BY created_at DESC LIMIT $2 OFFSET $3",
        )
        .bind(company_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(state.db.pool())
        .await?
    };

    let items: Vec<Value> = rows.iter().map(invocation_json).collect();
    Ok(Json(json!({ "companyId": company_id, "items": items })))
}
