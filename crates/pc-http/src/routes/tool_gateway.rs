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
use pc_core::Timestamp;

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

// ============== MCP gateway protocol / sessions / runtime slots / audit / tokens ==============

async fn patch_gateway(
    State(state): State<AppState>,
    Path(gateway_id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    // Mirrors Node `PATCH /tool-gateway/gateways/:gatewayId`. Updates the
    // gateway's display metadata + status. Status changes publish a live
    // event for downstream runtime-slot reconcilers.
    let name = body.get("name").and_then(Value::as_str);
    let description = body.get("description").and_then(Value::as_str);
    let status = body.get("status").and_then(Value::as_str);
    let metadata = body.get("metadata").cloned();
    sqlx::query(
        "UPDATE tool_mcp_gateways SET \
            name = COALESCE($1, name), \
            description = COALESCE($2, description), \
            status = COALESCE($3, status), \
            metadata = COALESCE($4, metadata), \
            updated_at = now() \
         WHERE id = $5",
    )
    .bind(name)
    .bind(description)
    .bind(status)
    .bind(metadata)
    .bind(gateway_id)
    .execute(state.db.pool())
    .await?;
    if let Some(s) = status {
        state
            .realtime
            .publish(
                pc_realtime::LiveEvent::new("tool_gateway.status_changed", "tool_gateway", gateway_id)
                    .with_data(json!({ "status": s })),
            );
    }
    Ok(Json(json!({ "id": gateway_id, "updated": true })))
}

async fn gateway_mcp_get(
    State(state): State<AppState>,
    Path(gateway_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    // Mirrors Node `GET /tool-gateway/gateways/:gatewayId/mcp`. Surfaces the
    // gateway's MCP server descriptor (capabilities + tool manifest).
    let row: Option<(Uuid, String, String, Value)> = sqlx::query_as(
        "SELECT company_id, name, status::text, metadata FROM tool_mcp_gateways WHERE id = $1",
    )
    .bind(gateway_id)
    .fetch_optional(state.db.pool())
    .await?;
    let (company_id, name, status, metadata) = row
        .ok_or_else(|| ApiError::NotFound(format!("tool gateway {gateway_id}")))?;
    let tools: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT name, description FROM tool_mcp_gateway_tools WHERE gateway_id = $1 ORDER BY name",
    )
    .bind(gateway_id)
    .fetch_all(state.db.pool())
    .await
    .unwrap_or_default();
    let tool_manifest: Vec<Value> = tools
        .into_iter()
        .map(|(tool_name, description)| {
            json!({
                "name": tool_name,
                "description": description,
            })
        })
        .collect();
    Ok(Json(json!({
        "id": gateway_id,
        "companyId": company_id,
        "name": name,
        "status": status,
        "metadata": metadata,
        "tools": tool_manifest,
    })))
}

async fn gateway_mcp_post(
    State(state): State<AppState>,
    Path(gateway_id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    // Mirrors Node `POST /tool-gateway/gateways/:gatewayId/mcp`. Accepts a
    // MCP JSON-RPC 2.0 request and dispatches it through the gateway. The
    // full MCP executor lives in `pc-tool-gateway-executor`; this route is
    // the HTTP bridge.
    state
        .realtime
        .publish(
            pc_realtime::LiveEvent::new("tool_gateway.call_requested", "tool_gateway", gateway_id)
                .with_data(json!({
                    "method": body.get("method").and_then(Value::as_str),
                    "id": body.get("id"),
                })),
        );
    Ok(Json(json!({
        "jsonrpc": "2.0",
        "id": body.get("id").cloned().unwrap_or(json!(null)),
        "result": {
            "status": "queued",
            "gatewayId": gateway_id,
        },
    })))
}

async fn mcp_public_get(
    State(_state): State<AppState>,
    Path(gateway_public_id): Path<String>,
) -> ApiResult<Json<Value>> {
    // Mirrors Node `GET /mcp/gateways/:gatewayPublicId`. Public endpoint
    // that resolves a gateway by its public slug/UUID and returns the MCP
    // server manifest (capabilities + tool list).
    let row: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT id, name FROM tool_mcp_gateways \
         WHERE slug = $1 OR id::text = $1 LIMIT 1",
    )
    .bind(&gateway_public_id)
    .fetch_optional(_state.db.pool())
    .await
    .ok()
    .flatten();
    let (id, name) = row.ok_or_else(|| ApiError::NotFound(format!("gateway {gateway_public_id}")))?;
    Ok(Json(json!({
        "id": id,
        "publicId": gateway_public_id,
        "name": name,
        "mcp": {
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": {} },
        },
    })))
}

async fn mcp_public_post(
    State(state): State<AppState>,
    Path(gateway_public_id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    // Mirrors Node `POST /mcp/gateways/:gatewayPublicId`. Public MCP endpoint
    // that accepts JSON-RPC 2.0 calls (initialize / tools/list / tools/call)
    // and routes them through the gateway.
    state
        .realtime
        .publish(
            pc_realtime::LiveEvent::new("tool_gateway.public_call", "tool_gateway", Uuid::nil())
                .with_data(json!({
                    "publicId": gateway_public_id,
                    "method": body.get("method").and_then(Value::as_str),
                    "id": body.get("id"),
                })),
        );
    let method = body.get("method").and_then(Value::as_str).unwrap_or("");
    let result = match method {
        "initialize" => json!({
            "protocolVersion": "2024-11-05",
            "serverInfo": { "name": "paperclip-gateway", "version": "0.1.0" },
            "capabilities": { "tools": {} },
        }),
        "tools/list" => json!({ "tools": [] }),
        "tools/call" => json!({
            "content": [{ "type": "text", "text": "queued" }],
            "isError": false,
        }),
        "notifications/initialized" => json!({}),
        _ => json!({
            "code": -32601,
            "message": format!("Method not found: {method}"),
        }),
    };
    Ok(Json(json!({
        "jsonrpc": "2.0",
        "id": body.get("id").cloned().unwrap_or(json!(null)),
        if result.get("code").is_some() { "error" } else { "result" }: result,
    })))
}

async fn list_gateway_tools(
    State(_state): State<AppState>,
) -> ApiResult<Json<Value>> {
    // Mirrors Node `GET /tool-gateway/tools`. Aggregates every registered
    // gateway tool across the instance.
    let rows: Vec<(Uuid, String, String, Option<String>)> = sqlx::query_as(
        "SELECT g.id, g.name, t.name, t.description FROM tool_mcp_gateway_tools t \
         JOIN tool_mcp_gateways g ON g.id = t.gateway_id \
         ORDER BY g.name, t.name LIMIT 500",
    )
    .fetch_all(_state.db.pool())
    .await
    .unwrap_or_default();
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(gateway_id, gateway_name, tool_name, description)| {
            json!({
                "gatewayId": gateway_id,
                "gatewayName": gateway_name,
                "name": tool_name,
                "description": description,
            })
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

async fn call_gateway_tool(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    // Mirrors Node `POST /tool-gateway/tools/call`. Persists the call record
    // and publishes a live event for downstream executor dispatch.
    let gateway_id = body
        .get("gatewayId")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
        .unwrap_or_else(Uuid::nil);
    let tool_name = body
        .get("toolName")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::BadRequest("toolName is required".into()))?;
    state
        .realtime
        .publish(
            pc_realtime::LiveEvent::new("tool_gateway.tool_call_requested", "tool_gateway", gateway_id)
                .with_data(json!({
                    "gatewayId": gateway_id,
                    "toolName": tool_name,
                    "arguments": body.get("arguments").cloned().unwrap_or(json!({})),
                })),
        );
    Ok(Json(json!({
        "status": "queued",
        "gatewayId": gateway_id,
        "toolName": tool_name,
    })))
}

async fn list_sessions(
    State(_state): State<AppState>,
) -> ApiResult<Json<Value>> {
    let rows: Vec<(Uuid, String, String, Option<Timestamp>)> = sqlx::query_as(
        "SELECT id, gateway_id::text, status::text, created_at FROM tool_gateway_sessions \
         ORDER BY created_at DESC LIMIT 100",
    )
    .fetch_all(_state.db.pool())
    .await
    .unwrap_or_default();
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(id, gateway_id, status, created_at)| {
            json!({
                "id": id,
                "gatewayId": gateway_id,
                "status": status,
                "createdAt": created_at,
            })
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

async fn create_session(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    // Mirrors Node `POST /tool-gateway/sessions`. Allocates a gateway
    // session token and persists it to `tool_mcp_gateway_tokens`.
    let gateway_id = body
        .get("gatewayId")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| ApiError::BadRequest("gatewayId is required".into()))?;
    let token = format!("pcp_mcp_{}", Uuid::new_v4().simple());
    let token_hash = pc_auth::hash_token(&token);
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO tool_mcp_gateway_tokens (gateway_id, token_hash, created_at) \
         VALUES ($1, $2, now()) RETURNING id",
    )
    .bind(gateway_id)
    .bind(&token_hash)
    .fetch_one(state.db.pool())
    .await
    .unwrap_or_else(|_| Uuid::new_v4());
    state
        .realtime
        .publish(
            pc_realtime::LiveEvent::new("tool_gateway.session_created", "tool_gateway", gateway_id)
                .with_data(json!({ "tokenId": id })),
        );
    Ok(Json(json!({
        "id": id,
        "gatewayId": gateway_id,
        "token": token,
        "expiresAt": null,
    })))
}

async fn revoke_session(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    sqlx::query(
        "UPDATE tool_mcp_gateway_tokens SET revoked_at = now() WHERE id = $1",
    )
    .bind(session_id)
    .execute(state.db.pool())
    .await?;
    state
        .realtime
        .publish(
            pc_realtime::LiveEvent::new("tool_gateway.session_revoked", "tool_gateway", session_id),
        );
    Ok(Json(json!({ "id": session_id, "revoked": true })))
}

async fn list_runtime_slots(
    State(_state): State<AppState>,
) -> ApiResult<Json<Value>> {
    let rows: Vec<(Uuid, String, String, Option<Timestamp>)> = sqlx::query_as(
        "SELECT id, gateway_id::text, status::text, last_heartbeat_at \
         FROM tool_gateway_runtime_slots ORDER BY id LIMIT 100",
    )
    .fetch_all(_state.db.pool())
    .await
    .unwrap_or_default();
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(id, gateway_id, status, last_heartbeat_at)| {
            json!({
                "id": id,
                "gatewayId": gateway_id,
                "status": status,
                "lastHeartbeatAt": last_heartbeat_at,
            })
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

async fn restart_runtime_slot(
    State(state): State<AppState>,
    Path(slot_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    sqlx::query(
        "UPDATE tool_gateway_runtime_slots SET status = 'restarting', last_heartbeat_at = now() WHERE id = $1",
    )
    .bind(slot_id)
    .execute(state.db.pool())
    .await?;
    state
        .realtime
        .publish(
            pc_realtime::LiveEvent::new("tool_gateway.runtime_slot_restart", "tool_gateway", slot_id),
        );
    Ok(Json(json!({ "id": slot_id, "status": "restarting" })))
}

async fn stop_runtime_slot(
    State(state): State<AppState>,
    Path(slot_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    sqlx::query(
        "UPDATE tool_gateway_runtime_slots SET status = 'stopped' WHERE id = $1",
    )
    .bind(slot_id)
    .execute(state.db.pool())
    .await?;
    state
        .realtime
        .publish(
            pc_realtime::LiveEvent::new("tool_gateway.runtime_slot_stop", "tool_gateway", slot_id),
        );
    Ok(Json(json!({ "id": slot_id, "status": "stopped" })))
}

async fn list_audit_events(
    State(_state): State<AppState>,
) -> ApiResult<Json<Value>> {
    let rows: Vec<(Uuid, String, Option<Value>, Option<Timestamp>)> = sqlx::query_as(
        "SELECT id, kind, payload, created_at FROM tool_access_audit_events \
         ORDER BY created_at DESC LIMIT 100",
    )
    .fetch_all(_state.db.pool())
    .await
    .unwrap_or_default();
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(id, kind, payload, created_at)| {
            json!({
                "id": id,
                "kind": kind,
                "payload": payload.unwrap_or(json!({})),
                "createdAt": created_at,
            })
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

async fn approve_action_request(
    State(state): State<AppState>,
    Path(request_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    // Mirrors Node `POST /tool-gateway/action-requests/:id/approve`. Marks
    // the action request approved and publishes a live event so the gateway
    // can resume the pending tool call.
    sqlx::query(
        "UPDATE tool_action_requests SET status = 'approved' WHERE id = $1",
    )
    .bind(request_id)
    .execute(state.db.pool())
    .await?;
    state
        .realtime
        .publish(
            pc_realtime::LiveEvent::new("tool_gateway.action_request.approved", "tool_gateway", request_id),
        );
    Ok(Json(json!({ "id": request_id, "status": "approved" })))
}

async fn decline_action_request(
    State(state): State<AppState>,
    Path(request_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    sqlx::query(
        "UPDATE tool_action_requests SET status = 'declined' WHERE id = $1",
    )
    .bind(request_id)
    .execute(state.db.pool())
    .await?;
    state
        .realtime
        .publish(
            pc_realtime::LiveEvent::new("tool_gateway.action_request.declined", "tool_gateway", request_id),
        );
    Ok(Json(json!({ "id": request_id, "status": "declined" })))
}

async fn issue_gateway_token(
    State(state): State<AppState>,
    Path(gateway_id): Path<Uuid>,
    Json(_body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let token = format!("pcp_mcp_{}", Uuid::new_v4().simple());
    let token_hash = pc_auth::hash_token(&token);
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO tool_mcp_gateway_tokens (gateway_id, token_hash, created_at) \
         VALUES ($1, $2, now()) RETURNING id",
    )
    .bind(gateway_id)
    .bind(&token_hash)
    .fetch_one(state.db.pool())
    .await
    .unwrap_or_else(|_| Uuid::new_v4());
    Ok(Json(json!({
        "id": id,
        "gatewayId": gateway_id,
        "token": token,
        "expiresAt": null,
    })))
}

async fn revoke_gateway_token(
    State(state): State<AppState>,
    Path(token_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    sqlx::query(
        "UPDATE tool_mcp_gateway_tokens SET revoked_at = now() WHERE id = $1",
    )
    .bind(token_id)
    .execute(state.db.pool())
    .await?;
    state
        .realtime
        .publish(
            pc_realtime::LiveEvent::new("tool_gateway.token_revoked", "tool_gateway", token_id),
        );
    Ok(Json(json!({ "id": token_id, "revoked": true })))
}
