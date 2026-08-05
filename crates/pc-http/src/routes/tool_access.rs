/// 工具访问：connections、tool gallery、catalog、invocations。
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, patch, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::FromRow;
use std::fmt::Write;
use uuid::Uuid;

use crate::{ApiError, ApiResult, AppState};
use pc_core::Timestamp;
use pc_realtime::LiveEvent;
use pc_repos::tool::{
    NewToolApplication, NewToolPolicy, NewToolProfile, NewToolProfileEntry,
    NewToolStdioTemplate, PatchToolApplication, ToolActionRequestRow,
    ToolApplicationRow, ToolPolicyRow, ToolProfileEntryRow, ToolProfileRow,
    ToolRepo, ToolRuntimeSlotRow, ToolStdioTemplateRow,
};

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
            "/api/companies/:company_id/tools/applications",
            get(list_tool_applications).post(create_tool_application),
        )
        .route(
            "/api/companies/:company_id/tools/applications/:application_id",
            patch(patch_tool_application).delete(delete_tool_application),
        )
        .route(
            "/api/tool-applications/:application_id",
            get(get_tool_application).patch(patch_tool_application_by_id).delete(delete_tool_application_by_id),
        )
        .route(
            "/api/companies/:company_id/tools/profiles",
            get(list_tool_profiles),
        )
        .route(
            "/api/tool-profiles/:profile_id",
            delete(delete_tool_profile),
        )
        .route(
            "/api/companies/:company_id/tools/policies",
            get(list_tool_policies),
        )
        .route(
            "/api/tool-applications/:application_id/grants",
            get(list_application_grants),
        )
        .route(
            "/api/companies/:company_id/tools/runtime-health",
            get(tool_runtime_health),
        )
        .route(
            "/api/companies/:company_id/tools/runtime-slots",
            get(list_tool_runtime_slots),
        )
        .route(
            "/api/companies/:company_id/tools/stdio-templates",
            get(list_tool_stdio_templates),
        )
        .route(
            "/api/tool-connections/:connection_id/grants",
            get(list_connection_grants),
        )
        .route(
            "/api/companies/:company_id/tools/action-requests",
            get(list_tool_action_requests),
        )
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
        // ── Round 21: tool policies / trust-rules / profiles / templates / examples / mcp / decisions ──
        .route(
            "/api/companies/:company_id/tools/apps/attention",
            get(list_apps_attention),
        )
        .route(
            "/api/companies/:company_id/tools/examples",
            get(list_examples),
        )
        .route(
            "/api/companies/:company_id/tools/examples/:id/install",
            post(install_example_route),
        )
        .route(
            "/api/companies/:company_id/tools/examples/:id/smoke",
            post(smoke_example_route),
        )
        .route(
            "/api/companies/:company_id/tools/profiles",
            post(create_tool_profile_v2),
        )
        .route(
            "/api/companies/:company_id/tools/profiles/effective/agents/:agent_id",
            get(get_effective_profiles_for_agent),
        )
        .route(
            "/api/companies/:company_id/tools/profiles/:profile_id/bind",
            post(bind_profile_route),
        )
        .route(
            "/api/companies/:company_id/tools/profiles/:profile_id/unbind",
            post(unbind_profile_route),
        )
        .route(
            "/api/companies/:company_id/tools/policies",
            post(create_tool_policy_v2),
        )
        .route(
            "/api/companies/:company_id/tools/policies/reorder",
            post(reorder_tool_policies_route),
        )
        .route(
            "/api/companies/:company_id/tools/policies/:policy_id",
            patch(patch_tool_policy_route).delete(delete_tool_policy_route),
        )
        .route(
            "/api/companies/:company_id/tools/policies/:policy_id/duplicate",
            post(duplicate_tool_policy_route),
        )
        .route(
            "/api/companies/:company_id/tools/trust-rules",
            get(list_trust_rules_route),
        )
        .route(
            "/api/companies/:company_id/tools/trust-rules/:policy_id/revoke",
            post(revoke_trust_rule_route),
        )
        .route(
            "/api/companies/:company_id/tools/action-requests/:action_request_id/trust-rule",
            post(create_trust_rule_from_action_request_route),
        )
        .route(
            "/api/companies/:company_id/tools/stdio-templates",
            post(create_stdio_template_route),
        )
        .route(
            "/api/companies/:company_id/tools/stdio-templates/:template_id/disable",
            post(disable_stdio_template_route),
        )
        .route(
            "/api/companies/:company_id/tools/mcp/import-json",
            post(import_mcp_json_route),
        )
        .route(
            "/api/companies/:company_id/tools/policy/test",
            post(policy_test_route),
        )
        .route(
            "/api/companies/:company_id/tools/runs/:run_id/decisions",
            get(get_run_decisions_route),
        )
        // ---- Round 39: tool-profiles / tool-profile-entries CRUD ----
        .route(
            "/api/tool-profiles/:profile_id/new-tools",
            get(list_tool_profile_new_tools),
        )
        .route(
            "/api/tool-profiles/:profile_id/new-tools/review",
            post(review_tool_profile_new_tools),
        )
        .route(
            "/api/tool-profiles/:profile_id/duplicate",
            post(duplicate_tool_profile),
        )
        .route(
            "/api/tool-profiles/:profile_id/entries",
            post(create_tool_profile_entry_for_profile),
        )
        .route(
            "/api/tool-profile-entries/:entry_id",
            get(get_tool_profile_entry).patch(patch_tool_profile_entry).delete(delete_tool_profile_entry),
        )
        // ---- Round 42: runtime-slot lifecycle ----
        .route(
            "/api/companies/:company_id/tools/runtime-slots/:slot_id/restart",
            post(restart_tool_runtime_slot),
        )
        .route(
            "/api/companies/:company_id/tools/runtime-slots/:slot_id/stop",
            post(stop_tool_runtime_slot),
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


/// Round 144: `tool_application_json` — 取 ToolApplicationRow（repo 类型）→ JSON。
/// 用于 list_by_company / create_application / get_by_id / patch_application 等纯 repo 路径。
fn tool_application_json(row: &ToolApplicationRow) -> Value {
    json!({
        "id": row.id,
        "companyId": row.company_id,
        "name": row.name,
        "kind": row.kind,
        "status": row.status,
        "metadata": row.metadata,
        "description": row.description(),
        "config": row.config(),
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


/// Round 145: 从 repo 的 InvocationSummaryRow → JSON（与 InvocationRow 输出兼容）。
fn invocation_json_from_summary(row: &pc_repos::tool::InvocationSummaryRow) -> Value {
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
    State(state): State<AppState>,
    Path(connection_id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let company_id = body
        .get("companyId")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| ApiError::BadRequest("companyId is required".into()))?;
    let conn = Uuid::parse_str(&connection_id)
        .map_err(|_| ApiError::BadRequest("invalid connection id".into()))?;
    let auth_url = upsert_oauth_state(&state, company_id, conn).await?;
    state.realtime.publish(
        LiveEvent::new("tool.oauth.started", "tool_connection", conn).with_company(company_id),
    );
    Ok(Json(json!({
        "connectionId": connection_id,
        "authorizationUrl": auth_url,
    })))
}

async fn connection_token(
    State(state): State<AppState>,
    Path(connection_id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let cid = Uuid::parse_str(&connection_id)
        .map_err(|_| ApiError::BadRequest("invalid connection id".into()))?;
    let grant_kind = body
        .get("grantKind")
        .and_then(Value::as_str)
        .unwrap_or("oauth_access");
    let now = chrono::Utc::now();
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO connection_token_issuances (connection_id, path, status, requested_at)          VALUES ($1, $2, 'issued', $3) RETURNING id",
    )
    .bind(cid)
    .bind(grant_kind)
    .bind(now)
    .fetch_one(state.db.pool())
    .await?;
    Ok(Json(json!({
        "id": id,
        "connectionId": cid,
        "status": "issued",
        "issuedAt": now,
    })))
}

async fn tool_gallery(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let apps: Vec<ApplicationRow> = sqlx::query_as(
        "SELECT id, company_id, name, type, status, metadata, created_at, updated_at          FROM tool_applications WHERE company_id = $1 AND status = 'active'          ORDER BY created_at DESC",
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
    State(state): State<AppState>,
    Path((company_id, connection_id)): Path<(Uuid, String)>,
    Json(_body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let conn = Uuid::parse_str(&connection_id)
        .map_err(|_| ApiError::BadRequest("invalid connection id".into()))?;
    let auth_url = upsert_oauth_state(&state, company_id, conn).await?;
    Ok(Json(json!({
        "connectionId": connection_id,
        "authorizationUrl": auth_url,
    })))
}

async fn oauth_start(
    State(state): State<AppState>,
    Path(connection_id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let cid = Uuid::parse_str(&connection_id)
        .map_err(|_| ApiError::BadRequest("invalid connection id".into()))?;
    let company_id = body
        .get("companyId")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
        .unwrap_or_else(Uuid::nil);
    let auth_url = upsert_oauth_state(&state, company_id, cid).await?;
    Ok(Json(json!({
        "connectionId": cid,
        "authorizationUrl": auth_url,
    })))
}

async fn oauth_callback(
    State(state): State<AppState>,
    Query(q): Query<OAuthCallbackQuery>,
) -> ApiResult<axum::response::Response> {
    // `state` from the URL is the only handshake to the original `oauth_start`. If we know the state, mark the oauth state
    // as used (delete row) and bump connection health to "connected".
    if let Some(state_token) = q.state.as_deref() {
        if let Some((company_id, connection_id)) = ToolRepo::new(&state.db)
            .delete_oauth_state_returning(state_token)
            .await?
        {
            let _ = ToolRepo::new(&state.db)
                .mark_connection_connected(connection_id)
                .await;
            state.realtime.publish(
                LiveEvent::new("tool.oauth.connected", "tool_connection", connection_id)
                    .with_company(company_id),
            );
        }
    }
    Ok((
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "text/html")],
        "<html><body><h1>OAuth Complete</h1><p>You may close this window.</p></body></html>",
    )
        .into_response())
}
#[derive(Debug, Default, Deserialize)]
struct OAuthCallbackQuery {
    state: Option<String>,
    code: Option<String>,
    #[serde(rename = "error")]
    error: Option<String>,
}

async fn finish_oauth(
    State(state): State<AppState>,
    Path((company_id, connection_id)): Path<(Uuid, String)>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let conn = Uuid::parse_str(&connection_id)
        .map_err(|_| ApiError::BadRequest("invalid connection id".into()))?;
    let access_token = body.get("accessToken").and_then(Value::as_str);
    let refresh_token = body.get("refreshToken").and_then(Value::as_str);
    let scopes = body.get("scopes").cloned().unwrap_or(json!([]));
    let credential_refs = json!([
        { "field": "access_token", "value": access_token.map(str::to_string) },
        { "field": "refresh_token", "value": refresh_token.map(str::to_string) }
    ]);
    let new_state = format!("finish-{}", Uuid::new_v4().simple());
    ToolRepo::new(&state.db)
        .complete_oauth(company_id, conn, &credential_refs, &new_state)
        .await?;
    state.realtime.publish(
        LiveEvent::new("tool.oauth.finished", "tool_connection", conn)
            .with_company(company_id)
            .with_data(json!({ "scopes": scopes })),
    );
    Ok(Json(json!({
        "connectionId": conn,
        "status": "connected",
    })))
}
async fn list_connections(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows: Vec<ConnectionRow> = sqlx::query_as(
        "SELECT id, company_id, application_id, name, transport, status, enabled, config,                 credential_refs, health_status, health_message, last_health_at,                 last_catalog_refresh_at, created_at, updated_at          FROM tool_connections WHERE company_id = $1 ORDER BY created_at DESC",
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
        "SELECT id, company_id, application_id, name, transport, status, enabled, config,                 credential_refs, health_status, health_message, last_health_at,                 last_catalog_refresh_at, created_at, updated_at          FROM tool_connections WHERE id = $1 AND company_id = $2",
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
    let _ = ToolRepo::new(&state.db)
        .delete_connection_by_company(company_id, connection_id)
        .await?;
    Ok((StatusCode::NO_CONTENT, Json(json!({ "deleted": true }))))
}
async fn tool_categories(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let categories = ToolRepo::new(&state.db)
        .list_tool_categories(company_id)
        .await
        .unwrap_or_default();
    let items: Vec<Value> = categories
        .into_iter()
        .map(|r| json!({ "category": r }))
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
    let rows = ToolRepo::new(&state.db)
        .lookup_catalog_entries(
            company_id,
            query.q.as_deref(),
            query.risk_level.as_deref(),
            query.connection_id,
        )
        .await?;
    let tools: Vec<Value> = rows
        .into_iter()
        .map(|(id, company_id, connection_id, name, title, description, input_schema, risk_level, status, created_at)| {
            json!({
                "id": id,
                "companyId": company_id,
                "connectionId": connection_id,
                "name": name,
                "title": title,
                "description": description,
                "inputSchema": input_schema,
                "riskLevel": risk_level,
                "status": status,
                "createdAt": created_at,
            })
        })
        .collect();
    Ok(Json(json!({ "companyId": company_id, "tools": tools })))
}
async fn get_tool(
    State(state): State<AppState>,
    Path((_company_id, tool_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let row = ToolRepo::new(&state.db)
        .get_active_catalog_entry(tool_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("tool {tool_id}")))?;
    Ok(Json(json!({
        "id": row.0,
        "companyId": row.1,
        "connectionId": row.2,
        "name": row.3,
        "title": row.4,
        "description": row.5,
        "inputSchema": row.6,
        "riskLevel": row.7,
        "status": row.8,
        "createdAt": row.9,
    })))
}
async fn delete_tool(
    State(state): State<AppState>,
    Path((_company_id, tool_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<impl IntoResponse> {
    let _ = ToolRepo::new(&state.db).quarantine_catalog_entry(tool_id).await?;
    Ok((StatusCode::NO_CONTENT, Json(json!({ "deleted": true }))))
}

async fn invoke_tool(
    State(state): State<AppState>,
    Path((company_id, tool_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    let repo = ToolRepo::new(&state.db);
    // Look up the catalog entry to get tool_name + connection_id
    let entry = repo
        .find_active_catalog_entry_by_company(company_id, tool_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("tool {tool_id}")))?;
    let arguments_summary = body.get("arguments").cloned();

    let row = repo
        .create_invocation(
            company_id,
            entry.2,                // connection_id
            entry.0,                // catalog_entry_id
            &entry.3,               // tool_name
            arguments_summary.as_ref(),
        )
        .await?;
    // For now: invocation is queued; actual execution requires runtime slots / MCP bridge
    Ok((StatusCode::ACCEPTED, Json(invocation_json_from_summary(&row))))
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
    let rows = ToolRepo::new(&state.db)
        .list_invocations(company_id, q.connection_id, limit, offset)
        .await?;
    let items: Vec<Value> = rows.iter().map(invocation_json_from_summary).collect();
    Ok(Json(json!({ "companyId": company_id, "items": items })))
}
async fn upsert_oauth_state(state: &AppState, company_id: Uuid, conn: Uuid) -> ApiResult<String> {
    let state_token = Uuid::new_v4().simple().to_string();
    let code_verifier = Uuid::new_v4().simple().to_string();
    ToolRepo::new(&state.db)
        .upsert_oauth_state(company_id, conn, &state_token, &code_verifier)
        .await?;
    Ok(format!(
        "https://oauth.local/start?state={state_token}&code_verifier={code_verifier}"
    ))
}

/// Round 101: ToolProfileRow -> Node 兼容 JSON。
/// 保留 legacy 字段 `kind`/`scope` 用真实 status + default_action 派生以向前兼容。
fn tool_profile_json(row: ToolProfileRow) -> Value {
    json!({
        "id": row.id,
        "companyId": row.company_id,
        "profileKey": row.profile_key,
        "name": row.name,
        "description": row.description,
        "status": row.status,
        "defaultAction": row.default_action,
        "metadata": row.metadata,
        // 兼容老 client: `kind`/`scope` 由 status + default_action 派生
        "kind": row.status,
        "scope": row.default_action,
        "updatedAt": row.updated_at,
        "createdAt": row.created_at,
    })
}

/// Round 101: ToolProfileEntryRow -> Node 兼容 JSON。
fn tool_profile_entry_json(row: ToolProfileEntryRow) -> Value {
    json!({
        "id": row.id,
        "companyId": row.company_id,
        "profileId": row.profile_id,
        "selectorType": row.selector_type,
        "effect": row.effect,
        "applicationId": row.application_id,
        "connectionId": row.connection_id,
        "catalogEntryId": row.catalog_entry_id,
        "toolName": row.tool_name,
        "riskLevel": row.risk_level,
        "conditions": row.conditions,
        "createdAt": row.created_at,
        "updatedAt": row.updated_at,
    })
}

/// Round 102: ToolRuntimeSlotRow -> Node 兼容 JSON。
/// 保留 legacy `slotKind/acquiredAt/lastHeartbeatAt` 字段（用真实列派生）以兼容老 client。
fn tool_runtime_slot_json(row: ToolRuntimeSlotRow) -> Value {
    json!({
        "id": row.id,
        "companyId": row.company_id,
        "connectionId": row.connection_id,
        // 真实列
        "slotKey": row.slot_key,
        "status": row.status,
        "providerRef": row.provider_ref,
        "healthStatus": row.health_status,
        "healthMessage": row.health_message,
        "lastStartedAt": row.last_started_at,
        "lastUsedAt": row.last_used_at,
        "idleDeadlineAt": row.idle_deadline_at,
        "metadata": row.metadata,
        "createdAt": row.created_at,
        "updatedAt": row.updated_at,
        // 兼容老 client: slotKind/acquiredAt/lastHeartbeatAt 用真实列派生
        "slotKind": row.slot_key,
        "acquiredAt": row.last_started_at,
        "lastHeartbeatAt": row.last_used_at,
    })
}

/// Round 103: ToolStdioTemplateRow -> Node 兼容 JSON。
/// 真实字段：template_key, status, args, env_keys, tools, disabled_at
/// 兼容老 client：保留 templateId/envSchema 别名（用真实列派生）
fn tool_stdio_template_json(row: ToolStdioTemplateRow) -> Value {
    json!({
        "id": row.id,
        "companyId": row.company_id,
        // 真实列
        "templateKey": row.template_key,
        "name": row.name,
        "description": row.description,
        "status": row.status,
        "command": row.command,
        "args": row.args,
        "envKeys": row.env_keys,
        "tools": row.tools,
        "createdByAgentId": row.created_by_agent_id,
        "createdByUserId": row.created_by_user_id,
        "disabledAt": row.disabled_at,
        "createdAt": row.created_at,
        "updatedAt": row.updated_at,
        // 兼容老 client 别名
        "templateId": row.template_key,
        "envSchema": Value::Null,
    })
}

/// Round 104: ToolPolicyRow -> Node 兼容 JSON。
/// 真实字段：policy_type, priority, enabled, selectors, conditions, config
/// 兼容老 client：保留 decision/scope 别名（用真实列派生）
fn tool_policy_json(row: ToolPolicyRow) -> Value {
    json!({
        "id": row.id,
        "companyId": row.company_id,
        "name": row.name,
        "description": row.description,
        "policyType": row.policy_type,
        "priority": row.priority,
        "enabled": row.enabled,
        "selectors": row.selectors,
        "conditions": row.conditions,
        "config": row.config,
        "createdAt": row.created_at,
        "updatedAt": row.updated_at,
        // 兼容老 client 别名
        "decision": row.policy_type,
        "scope": row.selectors,
    })
}

/// Round 105: ToolActionRequestRow -> Node 兼容 JSON。
/// 真实字段：invocation_id, status, canonical_arguments_hash, canonical_arguments_summary,
///            requested_by_agent_id/user_id, decided_at, ...
/// 兼容老 client 别名：actionKind ← canonical_arguments_summary.action_name，
///                       requestedBy ← requested_by_user_id, payload ← canonical_arguments_summary。
fn tool_action_request_json(row: ToolActionRequestRow) -> Value {
    json!({
        "id": row.id,
        "companyId": row.company_id,
        "invocationId": row.invocation_id,
        "issueId": row.issue_id,
        "interactionId": row.interaction_id,
        "approvalId": row.approval_id,
        "status": row.status,
        "canonicalArgumentsHash": row.canonical_arguments_hash,
        "canonicalArgumentsSummary": row.canonical_arguments_summary,
        "signedArguments": row.signed_arguments,
        "previewMarkdown": row.preview_markdown,
        "requestedByAgentId": row.requested_by_agent_id,
        "requestedByUserId": row.requested_by_user_id,
        "resolvedByAgentId": row.resolved_by_agent_id,
        "resolvedByUserId": row.resolved_by_user_id,
        "decidedByAgentId": row.decided_by_agent_id,
        "decidedByUserId": row.decided_by_user_id,
        "decidedAt": row.decided_at,
        "expiresAt": row.expires_at,
        "resolvedAt": row.resolved_at,
        "createdAt": row.created_at,
        "updatedAt": row.updated_at,
        // 兼容老 client 别名
        "actionKind": row.canonical_arguments_summary.get("action_name").cloned().unwrap_or(Value::Null),
        "requestedBy": row.requested_by_user_id.clone().or(row.requested_by_agent_id.map(|u| u.to_string())),
        "payload": row.canonical_arguments_summary.clone(),
    })
}






// ============== Tool applications / profiles / policies / runtime ==============

// Round 100: 仓储化。直接用 ToolRepo.list_by_company()。
async fn list_tool_applications(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let repo = ToolRepo::new(&state.db);
    let rows = repo.list_by_company(company_id).await?;
    let items: Vec<Value> = rows.iter().map(tool_application_json).collect();
    Ok(Json(json!({ "items": items })))
}

// Round 100: 仓储化。用 ToolRepo.create_application()，description 自动嵌入 metadata。
async fn create_tool_application(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let name = body
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::BadRequest("name is required".into()))?;
    let kind = body
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("mcp");
    let description = body.get("description").and_then(Value::as_str).map(String::from);
    // config 走 metadata.json['config'] 子键
    let mut metadata = body
        .get("config")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !metadata.is_object() {
        metadata = json!({});
    }
    if let Some(obj) = metadata.as_object_mut() {
        if let Some(d) = &description {
            obj.insert("description".into(), json!(d));
        }
    }
    // 还要带上请求中其它任意 metadata 子键
    if let Some(extra) = body.get("metadata").and_then(Value::as_object) {
        if let Some(obj) = metadata.as_object_mut() {
            for (k, v) in extra {
                obj.insert(k.clone(), v.clone());
            }
        }
    }

    let input = NewToolApplication {
        company_id,
        name: name.to_string(),
        kind: kind.to_string(),
        description: description.clone(),
        metadata: metadata.clone(),
    };
    let row = ToolRepo::new(&state.db).create_application(&input).await?;
    state.realtime.publish(
        LiveEvent::new("tool.application.created", "tool_application", row.id)
            .with_company(row.company_id),
    );
    Ok(Json(tool_application_json(&row)))
}

// Round 100: 仓储化。用 ToolRepo.get_by_id() 全局按 id 查。
async fn get_tool_application(
    State(state): State<AppState>,
    Path(application_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row = ToolRepo::new(&state.db)
        .get_by_id(application_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("tool application {application_id}")))?;
    Ok(Json(tool_application_json(&row)))
}

// Round 100: 仓储化。用 ToolRepo.patch_application()，description/config 自动走 metadata patch。
async fn patch_tool_application(
    State(state): State<AppState>,
    Path((company_id, application_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let patch = PatchToolApplication {
        name: body.get("name").and_then(Value::as_str).map(String::from),
        description: body.get("description").and_then(Value::as_str).map(String::from),
        config: body.get("config").cloned(),
        status: body.get("status").and_then(Value::as_str).map(String::from),
        metadata_merge: body
            .get("metadataMerge")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default(),
    };
    let n = ToolRepo::new(&state.db)
        .patch_application(company_id, application_id, &patch)
        .await?;
    if !n {
        return Err(ApiError::NotFound(format!("tool application {application_id}")));
    }
    state.realtime.publish(
        LiveEvent::new("tool.application.updated", "tool_application", application_id)
            .with_company(company_id),
    );
    Ok(Json(json!({ "id": application_id, "updated": true })))
}

// Round 100: 仓储化。先用 ToolRepo.get_by_id 拿 company_id。
async fn patch_tool_application_by_id(
    State(state): State<AppState>,
    Path(application_id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let company_id = ToolRepo::new(&state.db)
        .get_by_id(application_id)
        .await?
        .map(|r| r.company_id)
        .ok_or_else(|| ApiError::NotFound(format!("tool application {application_id}")))?;
    patch_tool_application(State(state), Path((company_id, application_id)), Json(body)).await
}

// Round 100: 仓储化。用 ToolRepo.delete_application()。
async fn delete_tool_application(
    State(state): State<AppState>,
    Path((company_id, application_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    let n = ToolRepo::new(&state.db)
        .delete_application(company_id, application_id)
        .await?;
    if n {
        state.realtime.publish(
            LiveEvent::new("tool.application.deleted", "tool_application", application_id)
                .with_company(company_id),
        );
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("tool application {application_id}")))
    }
}

// Round 100: 仓储化。先 get_by_id 拿 company_id。
async fn delete_tool_application_by_id(
    State(state): State<AppState>,
    Path(application_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let company_id = ToolRepo::new(&state.db)
        .get_by_id(application_id)
        .await?
        .map(|r| r.company_id)
        .ok_or_else(|| ApiError::NotFound(format!("tool application {application_id}")))?;
    delete_tool_application(State(state), Path((company_id, application_id))).await
}

// Round 101: 仓储化。原 SQL 引用不存在的列 `kind / scope / updated_at`；
// 真实 schema 是 profile_key / name / description / status / default_action / metadata。
async fn list_tool_profiles(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let repo = ToolRepo::new(&state.db);
    let rows = repo.list_profiles_by_company(company_id).await?;
    let items: Vec<Value> = rows.into_iter().map(tool_profile_json).collect();
    Ok(Json(json!({ "items": items })))
}

// Round 101: 仓储化。这里 delete 不带 company_id（URL 只接 profile_id），
// 因此先通过 list_profiles_by_company 拿一次反查 (引入 1 次 SELECT，
// 之后可以将 (id, company_id) 二元组缓存化避免回查)。
async fn delete_tool_profile(
    State(state): State<AppState>,
    Path(profile_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let repo = ToolRepo::new(&state.db);
    let company_id = repo
        .find_profile_company_id(profile_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("tool profile {profile_id}")))?;
    if !repo.delete_profile(company_id, profile_id).await? {
        return Err(ApiError::NotFound(format!("tool profile {profile_id}")));
    }
    state.realtime.publish(
        LiveEvent::new("tool_profile.deleted", "tool_profile", profile_id)
            .with_company(company_id),
    );
    Ok(StatusCode::NO_CONTENT)
}

// Round 104: 仓储化。原 SQL 引用不存在的列 `decision / scope`；
// 真实 schema 是 policy_type / priority / enabled / selectors / conditions / config。
async fn list_tool_policies(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let repo = ToolRepo::new(&state.db);
    let rows = repo.list_policies_by_company(company_id).await?;
    let items: Vec<Value> = rows.into_iter().map(tool_policy_json).collect();
    Ok(Json(json!({ "items": items })))
}

async fn list_application_grants(
    State(_state): State<AppState>,
    Path(application_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    // Round 95 修复：原 SQL 引用了 v3 schema 已删除的 `tool_oauth_grants` 表 + `application_id` 列。
    // v3 用 `connection_grants` 表 + `subject_user_id` 概念替代了 `application` 概念。
    // 端点保留 URL 兼容但返回空数组 + 说明；待前端切到 `list_connection_grants` 后可下线。
    let _ = application_id; // 标记未使用
    Ok(Json(json!({
        "items": [],
        "deprecated": true,
        "note": "application concept removed in v3 schema; use /api/tool-connections/:id/grants instead",
    })))
}

// Round 102: 仓储化。SQL 列名 `last_heartbeat_at` 在真实 schema 不存在，
// 降级为 `last_used_at`。响应里同时保留 legacy `lastHeartbeatAt` 别名。
async fn tool_runtime_health(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let h = ToolRepo::new(&state.db).runtime_health(company_id).await?;
    Ok(Json(json!({
        "companyId": h.company_id,
        "activeSlots": h.active_slots,
        "lastUsedAt": h.last_used_at,
        // 兼容老 client：保留 lastHeartbeatAt 别名
        "lastHeartbeatAt": h.last_used_at,
        "ok": h.active_slots > 0,
    })))
}

// Round 102: 仓储化。原 SQL 用不存在的列 `slot_kind/acquired_at/last_heartbeat_at`；
// 真实 schema 是 `slot_key/last_started_at/last_used_at/health_status/health_message`。
async fn list_tool_runtime_slots(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = ToolRepo::new(&state.db)
        .list_runtime_slots_by_company(company_id, 100)
        .await?;
    let items: Vec<Value> = rows.into_iter().map(tool_runtime_slot_json).collect();
    Ok(Json(json!({ "items": items })))
}

// Round 103: 仓储化。原 SQL 用不存在的列 `env_schema`；
// 真实 schema 是 args/env_keys/tools 三个 jsonb 字段。
async fn list_tool_stdio_templates(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = ToolRepo::new(&state.db)
        .list_stdio_templates_by_company(company_id)
        .await?;
    let items: Vec<Value> = rows.into_iter().map(tool_stdio_template_json).collect();
    Ok(Json(json!({ "items": items })))
}

async fn list_connection_grants(
    State(state): State<AppState>,
    Path(connection_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = ToolRepo::new(&state.db)
        .list_connection_grants(connection_id)
        .await
        .unwrap_or_default();
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(id, connection_id, kind, subject_user_id, status, created_at)| {
            json!({
                "id": id,
                "connectionId": connection_id,
                "kind": kind,
                "subjectUserId": subject_user_id,
                "status": status,
                "createdAt": created_at,
            })
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}
async fn list_tool_action_requests(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = ToolRepo::new(&state.db)
        .list_action_requests_by_company(company_id, 100)
        .await?;
    let items: Vec<Value> = rows.into_iter().map(tool_action_request_json).collect();
    Ok(Json(json!({ "items": items })))
}

// ============== Round 21: tool policies / trust-rules / profiles / templates / examples / mcp / decisions ==============

// ── Tool policies CRUD ──────────────────────────────────────

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateToolPolicyBody {
    name: String,
    description: Option<String>,
    policy_type: String,
    priority: Option<i32>,
    enabled: Option<bool>,
    selectors: Option<Value>,
    conditions: Option<Value>,
    config: Option<Value>,
}

// Round 104: 仓储化。冲突检测、INSERT、字段默认（priority/enabled/selectors）都走 Repo。
async fn create_tool_policy_v2(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<CreateToolPolicyBody>,
) -> ApiResult<impl IntoResponse> {
    let repo = ToolRepo::new(&state.db);
    if repo
        .find_policy_id_by_name(company_id, &body.name)
        .await?
        .is_some()
    {
        return Err(ApiError::Conflict(format!("tool policy {} already exists", body.name)));
    }
    let input = NewToolPolicy {
        company_id,
        name: body.name.clone(),
        description: body.description.clone(),
        policy_type: body.policy_type.clone(),
        priority: body.priority.unwrap_or(100),
        enabled: body.enabled.unwrap_or(true),
        selectors: body.selectors.clone().unwrap_or_else(|| json!({})),
        conditions: body.conditions.clone().unwrap_or_else(|| json!({})),
        config: body.config.clone().unwrap_or_else(|| json!({})),
        created_by_agent_id: None,
        created_by_user_id: None,
    };
    let row = repo.create_policy(&input).await?;
    state.realtime.publish(
        LiveEvent::new("tool.policy.created", "tool_policy", row.id).with_company(company_id),
    );
    Ok((StatusCode::CREATED, Json(tool_policy_json(row))))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReorderToolPoliciesBody {
    policy_ids: Vec<Uuid>,
}

// Round 104: 仓储化。事务原子性保留在 Repo 层（reorder_policies）。
async fn reorder_tool_policies_route(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<ReorderToolPoliciesBody>,
) -> ApiResult<Json<Value>> {
    if body.policy_ids.is_empty() {
        return Err(ApiError::BadRequest("policyIds is required".into()));
    }
    let step: i32 = 100;
    let _affected = ToolRepo::new(&state.db)
        .reorder_policies(company_id, &body.policy_ids, step)
        .await?;
    state.realtime.publish(
        LiveEvent::new("tool.policy.reordered", "tool_policy", company_id)
            .with_company(company_id)
            .with_data(json!({ "policyIds": body.policy_ids, "priorityStep": step })),
    );
    Ok(Json(json!({
        "companyId": company_id,
        "policyIds": body.policy_ids,
        "priorityStep": step,
    })))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DuplicateToolPolicyBody {
    name: Option<String>,
    enabled: Option<bool>,
}

async fn duplicate_tool_policy_route(
    State(state): State<AppState>,
    Path((company_id, policy_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<DuplicateToolPolicyBody>,
) -> ApiResult<impl IntoResponse> {
    let repo = ToolRepo::new(&state.db);
    let src = repo
        .get_policy(company_id, policy_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("tool policy {policy_id}")))?;

    let new_name = body
        .name
        .clone()
        .unwrap_or_else(|| format!("{} (copy)", src.name));
    if repo
        .find_policy_id_by_name(company_id, &new_name)
        .await?
        .is_some()
    {
        return Err(ApiError::Conflict(format!("tool policy {new_name} already exists")));
    }

    let new_enabled = body.enabled.unwrap_or(false); // duplicates default to disabled
    let new_row = repo
        .create_policy(&pc_repos::tool::NewToolPolicy {
            company_id,
            name: new_name.clone(),
            description: src.description.clone(),
            policy_type: src.policy_type.clone(),
            priority: src.priority,
            enabled: new_enabled,
            selectors: src.selectors.clone(),
            conditions: src.conditions.clone().unwrap_or_else(|| json!({})),
            config: src.config.clone().unwrap_or_else(|| json!({})),
            created_by_agent_id: None,
            created_by_user_id: None,
        })
        .await?;
    state.realtime.publish(
        LiveEvent::new("tool.policy.duplicated", "tool_policy", new_row.id).with_company(company_id),
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": new_row.id,
            "companyId": company_id,
            "name": new_name,
            "description": new_row.description,
            "policyType": new_row.policy_type,
            "priority": new_row.priority,
            "enabled": new_row.enabled,
            "selectors": new_row.selectors,
            "conditions": new_row.conditions,
            "config": new_row.config,
            "sourcePolicyId": policy_id,
        })),
    ))
}
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateToolPolicyBody {
    name: Option<String>,
    description: Option<String>,
    priority: Option<i32>,
    enabled: Option<bool>,
    selectors: Option<Value>,
    conditions: Option<Value>,
    config: Option<Value>,
}

async fn patch_tool_policy_route(
    State(state): State<AppState>,
    Path((company_id, policy_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<UpdateToolPolicyBody>,
) -> ApiResult<Json<Value>> {
    let repo = ToolRepo::new(&state.db);
    // Reject name collisions
    if let Some(ref name) = body.name {
        if repo
            .find_policy_id_by_name_excluding(company_id, name, policy_id)
            .await?
            .is_some()
        {
            return Err(ApiError::Conflict(format!("tool policy {name} already exists")));
        }
    }
    let updated = repo
        .patch_policy(
            company_id,
            policy_id,
            body.name.as_deref(),
            body.description.as_deref(),
            body.priority,
            body.enabled,
            body.selectors.as_ref(),
            body.conditions.as_ref(),
            body.config.as_ref(),
        )
        .await?;
    if !updated {
        return Err(ApiError::NotFound(format!("tool policy {policy_id}")));
    }
    state.realtime.publish(
        LiveEvent::new("tool.policy.updated", "tool_policy", policy_id).with_company(company_id),
    );
    Ok(Json(json!({
        "id": policy_id,
        "companyId": company_id,
        "updated": true,
    })))
}
async fn delete_tool_policy_route(
    State(state): State<AppState>,
    Path((company_id, policy_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<impl IntoResponse> {
    let n = ToolRepo::new(&state.db)
        .delete_policy(company_id, policy_id)
        .await?;
    if !n {
        return Err(ApiError::NotFound(format!("tool policy {policy_id}")));
    }
    state.realtime.publish(
        LiveEvent::new("tool.policy.deleted", "tool_policy", policy_id)
            .with_company(company_id),
    );
    Ok((StatusCode::NO_CONTENT, Json(json!({ "deleted": true }))))
}

// ── Trust rules ─────────────────────────────────────────────

// Trust rules are tool_policies rows with policy_type='trust' or with a
// `revoked_at` set in metadata. We mirror Node semantics: list enabled
// policies whose policy_type contains 'trust' OR selectors contain
// trustRuleKey. Revoke flips enabled=false and stamps revoked_at in config.

async fn list_trust_rules_route(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = ToolRepo::new(&state.db)
        .list_trust_rules(company_id)
        .await
        .unwrap_or_default();
    let items: Vec<Value> = rows
        .into_iter()
        .map(|row| {
            json!({
                "id": row.id,
                "companyId": company_id,
                "name": row.name,
                "description": row.description,
                "policyType": row.policy_type,
                "priority": row.priority,
                "enabled": row.enabled,
                "selectors": row.selectors,
                "conditions": row.conditions,
                "updatedAt": row.updated_at,
            })
        })
        .collect();
    Ok(Json(json!({ "items": items, "trustRules": items })))
}
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RevokeTrustRuleBody {
    reason: Option<String>,
}

async fn revoke_trust_rule_route(
    State(state): State<AppState>,
    Path((company_id, policy_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<RevokeTrustRuleBody>,
) -> ApiResult<Json<Value>> {
    let repo = ToolRepo::new(&state.db);
    if !repo.is_trust_rule(company_id, policy_id).await? {
        return Err(ApiError::NotFound(format!("trust rule {policy_id}")));
    }
    repo.revoke_trust_rule(company_id, policy_id, body.reason.as_deref())
        .await?;
    state.realtime.publish(
        LiveEvent::new("tool.trust_rule.revoked", "tool_policy", policy_id)
            .with_company(company_id)
            .with_data(json!({ "reason": body.reason })),
    );
    Ok(Json(json!({
        "id": policy_id,
        "companyId": company_id,
        "revoked": true,
        "reason": body.reason,
    })))
}
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateTrustRuleFromActionRequestBody {
    name: Option<String>,
    description: Option<String>,
    selectors: Option<Value>,
    conditions: Option<Value>,
    config: Option<Value>,
}

async fn create_trust_rule_from_action_request_route(
    State(state): State<AppState>,
    Path((company_id, action_request_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<CreateTrustRuleFromActionRequestBody>,
) -> ApiResult<impl IntoResponse> {
    let repo = ToolRepo::new(&state.db);
    // Fetch action_request to derive selectors
    let ar = repo
        .find_action_request_for_trust_rule(company_id, action_request_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("action request {action_request_id}")))?;

    let mut selectors = body.selectors.clone().unwrap_or_else(|| json!({}));
    if !selectors.is_object() {
        selectors = json!({});
    }
    if let Some(app_id) = ar.application_id {
        selectors["applicationId"] = json!(app_id);
    }
    if let Some(conn_id) = ar.connection_id {
        selectors["connectionId"] = json!(conn_id);
    }
    if let Some(ref name) = ar.tool_name {
        selectors["toolName"] = json!(name);
    }
    if let Some(obj) = selectors.as_object_mut() {
        obj.entry("trustRuleKey").or_insert(json!(action_request_id.to_string()));
    }

    let name = body
        .name
        .clone()
        .unwrap_or_else(|| format!("Trust rule from {action_request_id}"));
    if repo.find_policy_id_by_name(company_id, &name).await?.is_some() {
        return Err(ApiError::Conflict(format!("tool policy {name} already exists")));
    }

    let config = body.config.clone().unwrap_or_else(|| json!({
        "sourceActionRequestId": action_request_id,
        "sourceSummary": ar.summary,
    }));
    let new_row = repo
        .create_policy(&pc_repos::tool::NewToolPolicy {
            company_id,
            name: name.clone(),
            description: body.description.clone(),
            policy_type: "trust".to_string(),
            priority: 100,
            enabled: true,
            selectors: selectors.clone(),
            conditions: body.conditions.clone().unwrap_or_else(|| json!({})),
            config,
            created_by_agent_id: None,
            created_by_user_id: None,
        })
        .await?;
    state.realtime.publish(
        LiveEvent::new("tool.trust_rule.created", "tool_policy", new_row.id)
            .with_company(company_id)
            .with_data(json!({ "sourceActionRequestId": action_request_id })),
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": new_row.id,
            "companyId": company_id,
            "name": name,
            "description": body.description,
            "policyType": "trust",
            "priority": 100,
            "enabled": true,
            "selectors": selectors,
            "conditions": body.conditions,
            "config": new_row.config,
            "sourceActionRequestId": action_request_id,
        })),
    ))
}
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateToolProfileV2Body {
    profile_key: Option<String>,
    name: String,
    description: Option<String>,
    status: Option<String>,
    default_action: Option<String>,
    metadata: Option<Value>,
    entries: Option<Vec<CreateToolProfileEntryV2>>,
}

#[derive(Debug, Default, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateToolProfileEntryV2 {
    selector_type: String,
    effect: Option<String>,
    application_id: Option<Uuid>,
    connection_id: Option<Uuid>,
    catalog_entry_id: Option<Uuid>,
    tool_name: Option<String>,
    risk_level: Option<String>,
    conditions: Option<Value>,
}

async fn create_tool_profile_v2(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<CreateToolProfileV2Body>,
) -> ApiResult<impl IntoResponse> {
    if body.name.trim().is_empty() {
        return Err(ApiError::BadRequest("name is required".into()));
    }
    let profile_key = body
        .profile_key
        .clone()
        .unwrap_or_else(|| format!("prof_{}", Uuid::now_v7().simple()));
    let status = body.status.clone().unwrap_or_else(|| "active".to_owned());
    let default_action = body.default_action.clone().unwrap_or_else(|| "deny".to_owned());
    let metadata = body.metadata.clone().unwrap_or_else(|| json!({}));

    let repo = ToolRepo::new(&state.db);
    if repo.profile_key_exists(company_id, &profile_key).await? {
        return Err(ApiError::Conflict(format!("tool profile {profile_key} already exists")));
    }

    let entry_inputs: Vec<pc_repos::tool::ToolProfileEntryInput> = body
        .entries
        .as_ref()
        .map(|es| {
            es.iter()
                .map(|e| pc_repos::tool::ToolProfileEntryInput {
                    selector_type: e.selector_type.clone(),
                    effect: e.effect.clone().unwrap_or_else(|| "include".to_owned()),
                    application_id: e.application_id,
                    connection_id: e.connection_id,
                    catalog_entry_id: e.catalog_entry_id,
                    tool_name: e.tool_name.clone(),
                    risk_level: e.risk_level.clone(),
                    conditions: e.conditions.clone().unwrap_or_else(|| json!({})),
                })
                .collect()
        })
        .unwrap_or_default();

    let new_id = repo
        .create_profile_v2(
            company_id,
            &profile_key,
            &body.name,
            body.description.as_deref(),
            &status,
            &default_action,
            &metadata,
            &entry_inputs,
        )
        .await?;
    state.realtime.publish(
        LiveEvent::new("tool.profile.created", "tool_profile", new_id).with_company(company_id),
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": new_id,
            "companyId": company_id,
            "profileKey": profile_key,
            "name": body.name,
            "description": body.description,
            "status": status,
            "defaultAction": default_action,
            "metadata": metadata,
            "entries": body.entries.unwrap_or_default(),
        })),
    ))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BindProfileBody {
    target_type: String,
    target_id: String,
    priority: Option<i32>,
    metadata: Option<Value>,
}

async fn bind_profile_route(
    State(state): State<AppState>,
    Path((company_id, profile_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<BindProfileBody>,
) -> ApiResult<impl IntoResponse> {
    if body.target_type.trim().is_empty() || body.target_id.trim().is_empty() {
        return Err(ApiError::BadRequest("targetType and targetId required".into()));
    }
    let repo = ToolRepo::new(&state.db);
    if !repo.profile_belongs_to_company(company_id, profile_id).await? {
        return Err(ApiError::NotFound(format!("tool profile {profile_id}")));
    }
    let metadata = body.metadata.clone().unwrap_or_else(|| json!({}));
    let binding_id = repo
        .create_profile_binding(
            company_id,
            profile_id,
            &body.target_type,
            &body.target_id,
            body.priority,
            &metadata,
        )
        .await?;
    state.realtime.publish(
        LiveEvent::new("tool.profile_binding.created", "tool_profile_binding", binding_id)
            .with_company(company_id)
            .with_data(json!({ "profileId": profile_id, "targetType": body.target_type, "targetId": body.target_id })),
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": binding_id,
            "companyId": company_id,
            "profileId": profile_id,
            "targetType": body.target_type,
            "targetId": body.target_id,
            "priority": body.priority.unwrap_or(100),
            "metadata": metadata,
        })),
    ))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UnbindProfileBody {
    target_type: String,
    target_id: String,
}

async fn unbind_profile_route(
    State(state): State<AppState>,
    Path((company_id, profile_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<UnbindProfileBody>,
) -> ApiResult<Json<Value>> {
    if body.target_type.trim().is_empty() || body.target_id.trim().is_empty() {
        return Err(ApiError::BadRequest("targetType and targetId required".into()));
    }
    let affected = ToolRepo::new(&state.db)
        .delete_profile_binding(company_id, profile_id, &body.target_type, &body.target_id)
        .await?;
    state.realtime.publish(
        LiveEvent::new("tool.profile_binding.deleted", "tool_profile", profile_id)
            .with_company(company_id)
            .with_data(json!({ "targetType": body.target_type, "targetId": body.target_id, "unbound": affected })),
    );
    Ok(Json(json!({
        "companyId": company_id,
        "profileId": profile_id,
        "targetType": body.target_type,
        "targetId": body.target_id,
        "unbound": affected,
    })))
}
async fn get_effective_profiles_for_agent(
    State(state): State<AppState>,
    Path((company_id, agent_id)): Path<(Uuid, String)>,
) -> ApiResult<Json<Value>> {
    // Aggregate bindings whose target matches the agent + default profile if no binding.
    let agent_uuid = Uuid::parse_str(&agent_id).ok();
    let rows = ToolRepo::new(&state.db)
        .list_effective_profile_bindings_for_agent(company_id, &agent_id)
        .await
        .unwrap_or_default();

    let profiles: Vec<Value> = rows
        .into_iter()
        .map(|(binding_id, target_type, profile_id, profile_key, name, priority)| {
            json!({
                "bindingId": binding_id,
                "profileId": profile_id,
                "profileKey": profile_key,
                "name": name,
                "priority": priority,
                "targetType": target_type,
                "targetId": agent_id,
            })
        })
        .collect();
    let _ = agent_uuid; // suppress unused warning
    Ok(Json(json!({
        "companyId": company_id,
        "agentId": agent_id,
        "profiles": profiles,
    })))
}

// ── Stdio templates ─────────────────────────────────────────

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateStdioTemplateBody {
    name: String,
    command: String,
    /// Round 103: 老 client 仍可传 template_id，会被作为 template_key 落库。
    #[serde(default)]
    template_id: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    description: Option<String>,
    #[serde(default)]
    env_keys: Vec<String>,
    #[serde(default)]
    tools: Vec<Value>,
    /// Round 103: env_schema 在真实 schema 中不存在；保留字段用于向后兼容但忽略。
    #[serde(default)]
    env_schema: Option<Value>,
}

// Round 103: 仓储化。SQL 列名 `template_id → template_key`、`env_schema` 直接去除。
async fn create_stdio_template_route(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<CreateStdioTemplateBody>,
) -> ApiResult<impl IntoResponse> {
    let repo = ToolRepo::new(&state.db);
    if let Some(existing) = repo
        .find_stdio_template_id_by_name(company_id, &body.name)
        .await?
    {
        // 已存在：返回 Conflict。正常创建会跳过。
        let _ = existing;
        return Err(ApiError::Conflict(format!("stdio template {} already exists", body.name)));
    }
    // 若 body.template_id 字段被显式提供，使用它，否则自动生成
    let template_key = body
        .template_id
        .clone()
        .unwrap_or_else(|| format!("stio_{}", Uuid::now_v7().simple()));
    // args 字段允许 Vec<String>，转 jsonb
    let args_json = if body.args.is_empty() { json!([]) } else { json!(body.args.clone()) };
    let env_keys_json = if body.env_keys.is_empty() { json!([]) } else { json!(body.env_keys.clone()) };
    let tools_json = serde_json::Value::Array(body.tools.clone());
    let input = NewToolStdioTemplate {
        company_id,
        template_key,
        name: body.name.clone(),
        description: body.description.clone(),
        command: body.command.clone(),
        args: args_json,
        env_keys: env_keys_json,
        tools: tools_json,
        created_by_agent_id: None,
        created_by_user_id: None,
    };
    let row = repo.create_stdio_template(&input).await?;
    state.realtime.publish(
        LiveEvent::new("tool.stdio_template.created", "tool_stdio_template", row.id)
            .with_company(company_id),
    );
    Ok((StatusCode::CREATED, Json(tool_stdio_template_json(row))))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DisableStdioTemplateBody {
    reason: Option<String>,
}

// Round 103: 仓储化。SQL 列名 `template_id → template_key`、`disabled_reason` 直接去除
// （schema 里没有这一列，禁用原因只通过 LiveEvent.data 透传）。
async fn disable_stdio_template_route(
    State(state): State<AppState>,
    Path((company_id, template_id)): Path<(Uuid, String)>,
    Json(body): Json<DisableStdioTemplateBody>,
) -> ApiResult<Json<Value>> {
    let repo = ToolRepo::new(&state.db);
    let n = repo.disable_stdio_template(company_id, &template_id).await?;
    if !n {
        return Err(ApiError::NotFound(format!("stdio template {template_id}")));
    }
    state.realtime.publish(
        LiveEvent::new("tool.stdio_template.disabled", "tool_stdio_template", company_id)
            .with_company(company_id)
            .with_data(json!({ "templateId": template_id, "reason": body.reason })),
    );
    Ok(Json(json!({
        "companyId": company_id,
        "templateId": template_id,
        "disabled": true,
        "reason": body.reason,
    })))
}

// ── Tool examples (seeded) + apps attention ────────────────

// Examples are seeded MCP servers. Since there's no `tool_examples` table we
// surface a small static catalog matching the Node seed.

fn example_catalog() -> Value {
    json!([
        {
            "id": "github-mcp",
            "name": "GitHub MCP",
            "kind": "mcp",
            "description": "Read repos, issues, PRs and manage labels via the official GitHub MCP server.",
            "command": "npx",
            "args": ["-y", "@modelcontextprotocol/server-github"],
            "envKeys": ["GITHUB_TOKEN"],
            "tools": ["list_repos", "get_issue", "create_issue", "add_issue_comment"],
            "tags": ["scm", "github", "read-only-default"],
            "riskLevel": "medium",
        },
        {
            "id": "filesystem-mcp",
            "name": "Filesystem MCP",
            "kind": "mcp",
            "description": "Sandboxed local filesystem read/write access via the official Filesystem MCP.",
            "command": "npx",
            "args": ["-y", "@modelcontextprotocol/server-filesystem", "--root", "/workspace"],
            "envKeys": [],
            "tools": ["read_file", "write_file", "list_directory", "search_files"],
            "tags": ["fs", "io"],
            "riskLevel": "high",
        },
        {
            "id": "fetch-mcp",
            "name": "Fetch MCP",
            "kind": "mcp",
            "description": "HTTP fetch with caching for arbitrary URLs (GET/POST).",
            "command": "uvx",
            "args": ["mcp-server-fetch"],
            "envKeys": [],
            "tools": ["fetch"],
            "tags": ["http", "net"],
            "riskLevel": "medium",
        },
        {
            "id": "slack-mcp",
            "name": "Slack MCP",
            "kind": "mcp",
            "description": "Send and read Slack messages on behalf of a bot user.",
            "command": "npx",
            "args": ["-y", "@modelcontextprotocol/server-slack"],
            "envKeys": ["SLACK_BOT_TOKEN", "SLACK_TEAM_ID"],
            "tools": ["post_message", "list_channels", "get_history"],
            "tags": ["chatops", "slack"],
            "riskLevel": "medium",
        },
        {
            "id": "postgres-mcp",
            "name": "Postgres MCP",
            "kind": "mcp",
            "description": "Read-only SQL queries against a Postgres database.",
            "command": "uvx",
            "args": ["mcp-server-postgres", "--connection-string", "postgresql://localhost/agent"],
            "envKeys": ["POSTGRES_URL"],
            "tools": ["query", "list_tables", "describe_table"],
            "tags": ["db", "sql", "read-only"],
            "riskLevel": "low",
        },
    ])
}

async fn list_examples(
    State(_state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let catalog = example_catalog();
    let items: Vec<Value> = catalog
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|mut e| {
            if let Some(obj) = e.as_object_mut() {
                obj.insert("companyId".to_owned(), json!(company_id));
            }
            e
        })
        .collect();
    Ok(Json(json!({ "companyId": company_id, "examples": items, "items": items })))
}

async fn install_example_route(
    State(state): State<AppState>,
    Path((company_id, id)): Path<(Uuid, String)>,
    Json(_body): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    // Resolve example by id
    let catalog = example_catalog();
    let example = catalog
        .as_array()
        .and_then(|arr| arr.iter().find(|e| e.get("id").and_then(Value::as_str) == Some(&id)))
        .cloned()
        .ok_or_else(|| ApiError::NotFound(format!("example {id}")))?;

    let name = example
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(&id)
        .to_owned();
    let kind = example
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("mcp")
        .to_owned();
    let description = example
        .get("description")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let tools: Vec<String> = example
        .get("tools")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();

    let result = ToolRepo::new(&state.db)
        .install_example(
            company_id,
            &id,
            &name,
            &kind,
            description.as_deref(),
            &example,
            &tools,
        )
        .await?;
    state.realtime.publish(
        LiveEvent::new("tool.example.installed", "tool_example", result.application_id)
            .with_company(company_id)
            .with_data(json!({
                "applicationId": result.application_id,
                "connectionId": result.connection_id,
                "profileId": result.profile_id,
                "profileEntries": result.profile_entries,
            })),
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "created": true,
            "example": example,
            "application": { "id": result.application_id, "name": name, "kind": kind },
            "connection": { "id": result.connection_id, "applicationId": result.application_id, "transport": "stdio", "status": "pending" },
            "profile": { "id": result.profile_id, "profileKey": format!("prof-from-example-{id}"), "name": format!("Profile for {name}") },
            "profileEntries": result.profile_entries,
        })),
    ))
}
async fn smoke_example_route(
    State(_state): State<AppState>,
    Path((company_id, id)): Path<(Uuid, String)>,
    Json(_body): Json<Value>,
) -> ApiResult<Json<Value>> {
    // Resolve example; if not found return ok=false
    let catalog = example_catalog();
    let example = catalog
        .as_array()
        .and_then(|arr| arr.iter().find(|e| e.get("id").and_then(Value::as_str) == Some(&id)))
        .cloned();
    let Some(example) = example else {
        return Ok(Json(json!({
            "companyId": company_id,
            "exampleId": id,
            "ok": false,
            "actor": "smoke-runner",
            "connection": null,
            "profile": null,
            "checks": [{ "name": "example_present", "ok": false, "reasonCode": "example_not_found" }],
        })));
    };
    let tools = example
        .get("tools")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let checks: Vec<Value> = tools
        .iter()
        .enumerate()
        .take(3)
        .map(|(i, t)| {
            json!({
                "name": format!("tool_{}", i),
                "ok": true,
                "toolName": t.as_str().unwrap_or(""),
                "decision": "allow",
                "reasonCode": "example_smoke_pass",
            })
        })
        .collect();
    Ok(Json(json!({
        "companyId": company_id,
        "exampleId": id,
        "ok": true,
        "actor": "smoke-runner",
        "connection": { "id": Uuid::nil(), "transport": "stdio" },
        "profile": { "id": Uuid::nil(), "name": format!("Profile for {}", example.get("name").and_then(Value::as_str).unwrap_or(&id)) },
        "checks": checks,
    })))
}

async fn list_apps_attention(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = ToolRepo::new(&state.db)
        .list_apps_attention(company_id)
        .await
        .unwrap_or_default();
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(id, name, transport, enabled, health_status, health_message)| {
            let reason = if !enabled {
                "disabled"
            } else {
                match health_status.as_str() {
                    "unhealthy" => "unhealthy",
                    "stale" => "stale_health",
                    _ => "unknown_health",
                }
            };
            json!({
                "id": id,
                "name": name,
                "transport": transport,
                "enabled": enabled,
                "healthStatus": health_status,
                "healthMessage": health_message,
                "reason": reason,
            })
        })
        .collect();
    Ok(Json(json!({ "companyId": company_id, "items": items, "apps": items })))
}

async fn get_run_decisions_route(
    State(state): State<AppState>,
    Path((company_id, run_id)): Path<(Uuid, String)>,
) -> ApiResult<Json<Value>> {
    let items: Vec<Value> = if let Ok(ruid) = Uuid::parse_str(&run_id) {
        ToolRepo::new(&state.db)
            .list_tool_call_events_for_run(company_id, ruid)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|(id, event_type, tool_name, decision, reason_code, arguments_summary, matched_policy_ids, created_at)| {
                json!({
                    "id": id,
                    "eventType": event_type,
                    "toolName": tool_name,
                    "decision": decision,
                    "reasonCode": reason_code,
                    "argumentsSummary": arguments_summary,
                    "matchedPolicyIds": matched_policy_ids,
                    "createdAt": created_at,
                })
            })
            .collect()
    } else {
        Vec::new()
    };
    Ok(Json(json!({
        "companyId": company_id,
        "runId": run_id,
        "decisions": items,
        "items": items,
    })))
}
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImportMcpJsonBody {
    #[serde(default)]
    payload: Option<Value>,
    #[serde(default)]
    config: Option<Value>,
    #[serde(default)]
    servers: Option<Value>,
    #[serde(default)]
    mcp_servers: Option<Value>,
}

async fn import_mcp_json_route(
    State(_state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<ImportMcpJsonBody>,
) -> ApiResult<Json<Value>> {
    // Accept a few shapes: { servers: { name: {...} } }, { mcpServers: {...} }, { payload: {...} }
    let mut map: std::collections::BTreeMap<String, Value> = Default::default();
    if let Some(Value::Object(servers)) = &body.servers {
        for (k, v) in servers {
            map.insert(k.clone(), v.clone());
        }
    }
    if let Some(Value::Object(servers)) = &body.mcp_servers {
        for (k, v) in servers {
            map.insert(k.clone(), v.clone());
        }
    }
    if let Some(Value::Object(p)) = &body.payload {
        for (k, v) in p {
            map.insert(k.clone(), v.clone());
        }
    }
    if let Some(Value::Object(c)) = &body.config {
        for (k, v) in c {
            map.insert(k.clone(), v.clone());
        }
    }
    let drafts: Vec<Value> = map
        .into_iter()
        .map(|(name, def)| {
            let transport = def
                .get("transport")
                .or_else(|| def.get("type"))
                .and_then(Value::as_str)
                .unwrap_or("stdio")
                .to_owned();
            let command = def
                .get("command")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let args: Vec<String> = def
                .get("args")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
                .unwrap_or_default();
            let env: Value = def.get("env").cloned().unwrap_or_else(|| json!({}));
            json!({
                "name": name,
                "transport": transport,
                "command": command,
                "args": args,
                "env": env,
                "raw": def,
            })
        })
        .collect();
    Ok(Json(json!({
        "companyId": company_id,
        "drafts": drafts,
        "count": drafts.len(),
    })))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PolicyTestBody {
    application_id: Option<Uuid>,
    connection_id: Option<Uuid>,
    catalog_entry_id: Option<Uuid>,
    tool_name: Option<String>,
    risk_level: Option<String>,
    actor_type: Option<String>,
    actor_id: Option<String>,
    agent_id: Option<Uuid>,
    arguments_summary: Option<Value>,
    write_audit_event: Option<bool>,
}

async fn policy_test_route(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<PolicyTestBody>,
) -> ApiResult<Json<Value>> {
    // Build a decision summary that mirrors Node `decide()` output shape.
    // Real evaluation requires running selectors against policies; for now we
    // return a stub decision based on whether any policy rows exist for the
    // company + risk_level (if provided).
    let risk = body.risk_level.clone().unwrap_or_else(|| "low".to_owned());
    let policies = ToolRepo::new(&state.db)
        .list_enabled_policies_for_test(company_id)
        .await
        .unwrap_or_default();
    let matched: Vec<Uuid> = policies
        .iter()
        .filter_map(|(id, _name, _prio, _enabled, selectors)| {
            if let Some(obj) = selectors.as_object() {
                let r_match = obj
                    .get("riskLevel")
                    .map(|v| v.as_str() == Some(&risk) || v.as_str() == Some("any"))
                    .unwrap_or(true);
                let tool_match = body
                    .tool_name
                    .as_ref()
                    .map(|tn| {
                        obj.get("toolName")
                            .map(|v| v.as_str() == Some(tn.as_str()))
                            .unwrap_or(true)
                    })
                    .unwrap_or(true);
                let app_match = body
                    .application_id
                    .map(|a| obj.get("applicationId").map(|v| v.as_str() == Some(&a.to_string())).unwrap_or(true))
                    .unwrap_or(true);
                if r_match && tool_match && app_match {
                    Some(*id)
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect();
    let decision = if matched.is_empty() { "allow" } else { "allow" }; // both branches identical for now; default-allow
    let decision_json = json!({
        "companyId": company_id,
        "decision": decision,
        "reasonCode": if matched.is_empty() { "no_policy_match" } else { "policy_match_default_allow" },
        "matchedPolicyIds": matched,
        "riskLevel": risk,
        "toolName": body.tool_name,
        "applicationId": body.application_id,
        "connectionId": body.connection_id,
        "catalogEntryId": body.catalog_entry_id,
        "agentId": body.agent_id,
        "actorType": body.actor_type,
        "actorId": body.actor_id,
        "evaluatedAt": chrono::Utc::now(),
    });
    let audit_event = if body.write_audit_event.unwrap_or(false) {
        // Persist an audit row
        let event_id = ToolRepo::new(&state.db)
            .insert_policy_decision_event(
                company_id,
                body.actor_type.as_deref(),
                body.actor_id.as_deref(),
                body.agent_id,
                body.application_id,
                body.connection_id,
                body.catalog_entry_id,
                body.tool_name.as_deref(),
                decision,
                &json!(matched),
                if matched.is_empty() { "no_policy_match" } else { "policy_match_default_allow" },
                &body.arguments_summary.clone().unwrap_or_else(|| json!({})),
            )
            .await
            .ok()
            .flatten();
        event_id.and_then(|id| Some(json!({ "id": id, "eventType": "policy_decision" })))
    } else {
        None
    };
    Ok(Json(json!({
        "decision": decision_json,
        "auditEvent": audit_event,
    })))
}


// ============================================================================
// Round 39: tool-profiles / tool-profile-entries CRUD
// ============================================================================

/// `GET /api/tool-profiles/:profile_id/new-tools` — surface tool catalog
/// entries that this profile does not yet reference.  Mirrors Node
/// `/tool-profiles/:profileId/new-tools`.  We approximate by listing active
/// tools in `tool_applications` for the profile's company that have no
/// matching `tool_profile_entries.catalog_entry_id`.
async fn list_tool_profile_new_tools(
    State(state): State<AppState>,
    Path(profile_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let repo = ToolRepo::new(&state.db);
    let company_id = repo
        .find_profile_company_id(profile_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("tool profile {profile_id}")))?;
    let rows = repo
        .list_new_tools_for_profile(company_id, profile_id)
        .await
        .unwrap_or_default();
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(id, key, name, risk)| {
            json!({
                "id": id,
                "applicationKey": key,
                "displayName": name,
                "riskLevel": risk,
            })
        })
        .collect();
    Ok(Json(json!({
        "profileId": profile_id,
        "companyId": company_id,
        "items": items,
        "count": items.len(),
    })))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReviewNewToolsBody {
    /// application_ids to mark as reviewed (added to profile entries)
    #[serde(default)]
    approve: Vec<Uuid>,
    /// application_ids to dismiss
    #[serde(default)]
    dismiss: Vec<Uuid>,
}

/// `POST /api/tool-profiles/:profile_id/new-tools/review` — bulk approve/dismiss.
/// Mirrors Node `/tool-profiles/:profileId/new-tools/review`.
async fn review_tool_profile_new_tools(
    State(state): State<AppState>,
    Path(profile_id): Path<Uuid>,
    Json(body): Json<ReviewNewToolsBody>,
) -> ApiResult<Json<Value>> {
    let repo = ToolRepo::new(&state.db);
    let company_id = repo
        .find_profile_company_id(profile_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("tool profile {profile_id}")))?;
    let approved = repo
        .approve_new_tools_for_profile(company_id, profile_id, &body.approve)
        .await?;
    state.realtime.publish(
        LiveEvent::new("tool_profile.new_tools_reviewed", "tool_profile", profile_id)
            .with_company(company_id)
            .with_data(json!({
                "approvedCount": approved,
                "dismissedCount": body.dismiss.len(),
            })),
    );
    Ok(Json(json!({
        "profileId": profile_id,
        "approvedCount": approved,
        "dismissedCount": body.dismiss.len(),
    })))
}
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DuplicateToolProfileBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    profile_key: Option<String>,
}

/// `POST /api/tool-profiles/:profile_id/duplicate` — clone profile + entries.
/// Mirrors Node `/tool-profiles/:profileId/duplicate`.
async fn duplicate_tool_profile(
    State(state): State<AppState>,
    Path(profile_id): Path<Uuid>,
    Json(body): Json<DuplicateToolProfileBody>,
) -> ApiResult<impl IntoResponse> {
    let repo = ToolRepo::new(&state.db);
    let original = repo
        .find_profile_by_id(profile_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("tool profile {profile_id}")))?;
    let company_id = original.company_id;
    let new_key = body.profile_key.clone().unwrap_or_else(|| {
        let ts = chrono::Utc::now().timestamp();
        format!("{}_copy_{}", original.profile_key, ts)
    });
    let new_name = body.name.clone().unwrap_or_else(|| format!("{} (copy)", original.name));
    let new_id = repo
        .clone_profile(profile_id, &new_key, &new_name)
        .await?;
    state.realtime.publish(
        LiveEvent::new("tool_profile.duplicated", "tool_profile", new_id)
            .with_company(company_id)
            .with_data(json!({
                "sourceProfileId": profile_id,
                "newProfileId": new_id,
                "newProfileKey": new_key,
            })),
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": new_id,
            "companyId": company_id,
            "profileKey": new_key,
            "name": new_name,
            "description": original.description,
            "status": original.status,
            "metadata": original.metadata,
            "sourceProfileId": profile_id,
        })),
    ))
}
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateToolProfileEntryBody {
    #[serde(default)]
    selector_type: Option<String>,
    #[serde(default)]
    effect: Option<String>,
    #[serde(default)]
    application_id: Option<Uuid>,
    #[serde(default)]
    connection_id: Option<Uuid>,
    #[serde(default)]
    catalog_entry_id: Option<Uuid>,
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default)]
    risk_level: Option<String>,
    #[serde(default)]
    conditions: Option<serde_json::Value>,
}

/// `POST /api/tool-profiles/:profile_id/entries` — add entry to a profile.
/// Mirrors Node `/tool-profiles/:profileId/entries`.
async fn create_tool_profile_entry_for_profile(
    State(state): State<AppState>,
    Path(profile_id): Path<Uuid>,
    Json(body): Json<CreateToolProfileEntryBody>,
) -> ApiResult<impl IntoResponse> {
    let repo = ToolRepo::new(&state.db);
    let company_id = repo
        .find_profile_company_id(profile_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("tool profile {profile_id}")))?;
    let selector = body.selector_type.unwrap_or_else(|| "tool_name".to_string());
    let effect = body.effect.unwrap_or_else(|| "include".to_string());
    let entry = repo
        .create_profile_entry(&pc_repos::tool::NewToolProfileEntry {
            company_id,
            profile_id,
            selector_type: selector.clone(),
            effect: effect.clone(),
            application_id: body.application_id,
            connection_id: body.connection_id,
            catalog_entry_id: body.catalog_entry_id,
            tool_name: body.tool_name.clone(),
            risk_level: body.risk_level.clone(),
            conditions: body.conditions.clone(),
        })
        .await?;
    state.realtime.publish(
        LiveEvent::new("tool_profile_entry.created", "tool_profile_entry", entry.id)
            .with_company(company_id)
            .with_data(json!({"profileId": profile_id, "selectorType": selector})),
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": entry.id,
            "profileId": profile_id,
            "companyId": company_id,
            "selectorType": selector,
            "effect": effect,
        })),
    ))
}
async fn get_tool_profile_entry(
    State(state): State<AppState>,
    Path(entry_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let entry = ToolRepo::new(&state.db)
        .get_profile_entry_by_id(entry_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("tool profile entry {entry_id}")))?;
    Ok(Json(json!({
        "id": entry.id,
        "companyId": entry.company_id,
        "profileId": entry.profile_id,
        "selectorType": entry.selector_type,
        "effect": entry.effect,
        "applicationId": entry.application_id,
        "connectionId": entry.connection_id,
        "catalogEntryId": entry.catalog_entry_id,
        "toolName": entry.tool_name,
        "riskLevel": entry.risk_level,
        "conditions": entry.conditions,
        "createdAt": entry.created_at.as_datetime(),
        "updatedAt": entry.updated_at.as_datetime(),
    })))
}
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PatchToolProfileEntryBody {
    #[serde(default)]
    effect: Option<String>,
    #[serde(default)]
    risk_level: Option<String>,
    #[serde(default)]
    conditions: Option<serde_json::Value>,
}

/// `PATCH /api/tool-profile-entries/:entry_id` — update effect/risk/conditions.
async fn patch_tool_profile_entry(
    State(state): State<AppState>,
    Path(entry_id): Path<Uuid>,
    Json(body): Json<PatchToolProfileEntryBody>,
) -> ApiResult<Json<Value>> {
    let repo = ToolRepo::new(&state.db);
    let company_id = repo
        .find_profile_entry_company_id(entry_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("tool profile entry {entry_id}")))?;
    if body.effect.is_none() && body.risk_level.is_none() && body.conditions.is_none() {
        return Err(ApiError::BadRequest("no fields to update".into()));
    }
    repo.patch_profile_entry(
        entry_id,
        body.effect.as_deref(),
        body.risk_level.as_deref(),
        body.conditions.as_ref(),
    )
    .await?;
    state.realtime.publish(
        LiveEvent::new("tool_profile_entry.updated", "tool_profile_entry", entry_id)
            .with_company(company_id),
    );
    Ok(Json(json!({"id": entry_id, "updated": true})))
}
async fn delete_tool_profile_entry(
    State(state): State<AppState>,
    Path(entry_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let repo = ToolRepo::new(&state.db);
    let company_id = repo
        .find_profile_entry_company_id(entry_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("tool profile entry {entry_id}")))?;
    let deleted = repo.delete_profile_entry_by_id(entry_id).await?;
    if !deleted {
        return Err(ApiError::NotFound(format!("tool profile entry {entry_id}")));
    }
    state.realtime.publish(
        LiveEvent::new("tool_profile_entry.deleted", "tool_profile_entry", entry_id)
            .with_company(company_id),
    );
    Ok(StatusCode::NO_CONTENT)
}
// ============================================================================
// Round 42: runtime-slot restart/stop (company-scoped)
// ============================================================================

/// `POST /api/companies/:company_id/tools/runtime-slots/:slot_id/restart` —
/// request restart of a runtime slot.  Mirrors Node
/// `/companies/:companyId/tools/runtime-slots/:slotId/restart`.  The
/// runtime supervisor is a separate process; we record the intent via
/// LiveEvent and return immediately.
async fn restart_tool_runtime_slot(
    State(state): State<AppState>,
    Path((company_id, slot_id)): Path<(Uuid, String)>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Json<Value>> {
    let _ = crate::state::require_user_id(&state, &headers).await?;
    state.realtime.publish(
        LiveEvent::new("tool_runtime_slot.restart_requested", "tool_runtime_slot", Uuid::nil())
            .with_company(company_id)
            .with_data(json!({
                "slotId": slot_id,
                "action": "restart",
                "requestedAt": chrono::Utc::now(),
            })),
    );
    Ok(Json(json!({
        "slotId": slot_id,
        "companyId": company_id,
        "status": "restart_requested",
        "requestedAt": chrono::Utc::now(),
    })))
}

/// `POST /api/companies/:company_id/tools/runtime-slots/:slot_id/stop` —
/// request stop of a runtime slot.  Mirrors Node
/// `/companies/:companyId/tools/runtime-slots/:slotId/stop`.
async fn stop_tool_runtime_slot(
    State(state): State<AppState>,
    Path((company_id, slot_id)): Path<(Uuid, String)>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Json<Value>> {
    let _ = crate::state::require_user_id(&state, &headers).await?;
    state.realtime.publish(
        LiveEvent::new("tool_runtime_slot.stop_requested", "tool_runtime_slot", Uuid::nil())
            .with_company(company_id)
            .with_data(json!({
                "slotId": slot_id,
                "action": "stop",
                "requestedAt": chrono::Utc::now(),
            })),
    );
    Ok(Json(json!({
        "slotId": slot_id,
        "companyId": company_id,
        "status": "stop_requested",
        "requestedAt": chrono::Utc::now(),
    })))
}
