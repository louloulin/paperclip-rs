/// Tool gateway (MCP 网关)。
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, patch, post},
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
            "/api/companies/:company_id/tools/gateways",
            get(list_gateways).post(create_gateway),
        )
        .route(
            "/api/tool-gateway/gateways/:gateway_id",
            get(get_gateway).patch(patch_gateway),
        )
        .route(
            "/api/tool-gateway/gateways/:gateway_id/mcp",
            get(gateway_mcp_get).post(gateway_mcp_post),
        )
        .route(
            "/api/mcp/gateways/:gateway_public_id",
            get(mcp_public_get).post(mcp_public_post),
        )
        .route("/api/tool-gateway/tools", get(list_gateway_tools))
        .route("/api/tool-gateway/tools/call", post(call_gateway_tool))
        .route("/api/tool-gateway/sessions", get(list_sessions).post(create_session))
        .route(
            "/api/tool-gateway/sessions/:session_id/revoke",
            post(revoke_session),
        )
        .route(
            "/api/tool-gateway/runtime-slots",
            get(list_runtime_slots),
        )
        .route(
            "/api/tool-gateway/runtime-slots/:slot_id/restart",
            post(restart_runtime_slot),
        )
        .route(
            "/api/tool-gateway/runtime-slots/:slot_id/stop",
            post(stop_runtime_slot),
        )
        .route("/api/tool-gateway/audit", get(list_audit_events))
        .route(
            "/api/tool-gateway/action-requests/:request_id/approve",
            post(approve_action_request),
        )
        .route(
            "/api/tool-gateway/action-requests/:request_id/decline",
            post(decline_action_request),
        )
        .route(
            "/api/tool-gateway/gateways/:gateway_id/tokens",
            post(issue_gateway_token),
        )
        .route(
            "/api/tool-gateway/gateway-tokens/:token_id/revoke",
            post(revoke_gateway_token),
        )
}

#[derive(Debug, FromRow)]
struct McpGatewayRow {
    id: Uuid,
    company_id: Uuid,
    name: String,
    slug: String,
    description: Option<String>,
    status: String,
    profile_id: Uuid,
    agent_id: Option<Uuid>,
    project_id: Option<Uuid>,
    issue_id: Option<Uuid>,
    metadata: Value,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

fn gateway_json(row: &McpGatewayRow) -> Value {
    json!({
        "id": row.id,
        "companyId": row.company_id,
        "name": row.name,
        "slug": row.slug,
        "description": row.description,
        "status": row.status,
        "profileId": row.profile_id,
        "agentId": row.agent_id,
        "projectId": row.project_id,
        "issueId": row.issue_id,
        "metadata": row.metadata,
        "createdAt": row.created_at,
        "updatedAt": row.updated_at,
    })
}

async fn list_gateways(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows: Vec<McpGatewayRow> = sqlx::query_as(
        "SELECT id, company_id, name, slug, description, status, profile_id, \
         agent_id, project_id, issue_id, metadata, created_at, updated_at \
         FROM tool_mcp_gateways WHERE company_id = $1 ORDER BY created_at DESC",
    )
    .bind(company_id)
    .fetch_all(state.db.pool())
    .await?;
    let items: Vec<Value> = rows.iter().map(gateway_json).collect();
    Ok(Json(json!({ "companyId": company_id, "items": items })))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateGatewayBody {
    name: String,
    slug: Option<String>,
    description: Option<String>,
    profile_id: Uuid,
    agent_id: Option<Uuid>,
    project_id: Option<Uuid>,
    issue_id: Option<Uuid>,
}

async fn create_gateway(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<CreateGatewayBody>,
) -> ApiResult<impl IntoResponse> {
    let slug = body
        .slug
        .clone()
        .unwrap_or_else(|| body.name.to_lowercase().replace(' ', "-"));
    let row: McpGatewayRow = sqlx::query_as(
        "INSERT INTO tool_mcp_gateways \
         (company_id, name, slug, description, profile_id, agent_id, project_id, issue_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
         RETURNING id, company_id, name, slug, description, status, profile_id, \
                   agent_id, project_id, issue_id, metadata, created_at, updated_at",
    )
    .bind(company_id)
    .bind(&body.name)
    .bind(&slug)
    .bind(&body.description)
    .bind(body.profile_id)
    .bind(body.agent_id)
    .bind(body.project_id)
    .bind(body.issue_id)
    .fetch_one(state.db.pool())
    .await?;
    Ok((StatusCode::CREATED, Json(gateway_json(&row))))
}

async fn get_gateway(
    State(state): State<AppState>,
    Path(gateway_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row: Option<McpGatewayRow> = sqlx::query_as(
        "SELECT id, company_id, name, slug, description, status, profile_id, \
         agent_id, project_id, issue_id, metadata, created_at, updated_at \
         FROM tool_mcp_gateways WHERE id = $1",
    )
    .bind(gateway_id)
    .fetch_optional(state.db.pool())
    .await?;
    match row {
        Some(row) => Ok(Json(gateway_json(&row))),
        None => Err(ApiError::NotFound(format!("gateway {gateway_id}"))),
    }
}

/// Bearer-token check for the MCP gateway protocol. Returns `true` when
/// the gateway exists, is active, and the bearer token matches a stored
/// token hash on `tool_mcp_gateway_tokens`. Mirrors Node `handleMcpGatewayProtocol`
/// in `routes/tool-gateway.ts`.
async fn authorize_gateway(
    state: &AppState,
    gateway_id: Uuid,
    bearer: Option<&str>,
) -> ApiResult<bool> {
    let bearer = bearer.map(str::trim).filter(|v| !v.is_empty());
    let Some(bearer) = bearer else {
        return Ok(false);
    };
    let token_hash = pc_auth::hash_token(bearer);
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT t.gateway_id FROM tool_mcp_gateway_tokens t \
         INNER JOIN tool_mcp_gateways g ON g.id = t.gateway_id \
         WHERE g.id = $1 AND g.status = 'active' \
           AND t.token_hash = $2 \
           AND (t.expires_at IS NULL OR t.expires_at > now()) \
           AND t.revoked_at IS NULL \
         LIMIT 1",
    )
    .bind(gateway_id)
    .bind(&token_hash)
    .fetch_optional(state.db.pool())
    .await?;
    Ok(row.is_some())
}

async fn post_gateway(
    State(state): State<AppState>,
    Path(gateway_id): Path<Uuid>,
    headers: axum::http::HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    // Extract bearer token from Authorization header
    let bearer = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            let lower = v.to_ascii_lowercase();
            lower
                .strip_prefix("bearer")
                .map(|rest| rest.trim_start())
                .unwrap_or(v)
                .trim()
                .to_owned()
        });
    let bearer = bearer.filter(|v| !v.is_empty());
    if bearer.is_none() {
        return Err(ApiError::Unauthorized("Bearer token is required".into()));
    }
    if !authorize_gateway(&state, gateway_id, bearer.as_deref()).await? {
        return Err(ApiError::Unauthorized("invalid gateway token".into()));
    }
    let method = body
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();
    let id = body.get("id").cloned().unwrap_or(Value::Null);
    let route_resp = match method.as_str() {
        "initialize" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": "2025-03-26",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "Paperclip MCP Gateway", "version": "1.0.0" }
            }
        }),
        "notifications/initialized" => {
            state.realtime.publish(
                pc_realtime::LiveEvent::new(
                    "tool_gateway.initialized",
                    "tool_gateway",
                    gateway_id,
                ),
            );
            return Ok(Json(json!({ "status": "accepted" })));
        }
        "tools/list" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "tools": [] }
        }),
        "tools/call" => {
            let params = body.get("params").cloned().unwrap_or(json!({}));
            let name = params
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned();
            if name.is_empty() {
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32602, "message": "params.name is required" }
                })
            } else {
                state.realtime.publish(
                    pc_realtime::LiveEvent::new(
                        "tool_gateway.call_requested",
                        "tool_gateway",
                        gateway_id,
                    )
                    .with_data(json!({ "gatewayId": gateway_id, "tool": name, "params": params })),
                );
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{ "type": "text", "text": format!("Tool {name} received") }],
                        "isError": false,
                        "deferred": true
                    }
                })
            }
        }
        _ => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32601, "message": format!("method {method} not implemented") }
        }),
    };
    Ok(Json(route_resp))
}
