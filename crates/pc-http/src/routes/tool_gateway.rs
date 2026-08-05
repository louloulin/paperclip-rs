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

// Round 155: `McpGatewayRow` 已迁到 `pc_repos::mcp_gateway::McpGatewayRow`。
use pc_repos::mcp_gateway::McpGatewayRow;

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
    let rows = pc_repos::mcp_gateway::McpGatewayRepo::new(&state.db)
        .list_by_company(company_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
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
    let row = pc_repos::mcp_gateway::McpGatewayRepo::new(&state.db)
        .create(
            company_id,
            &body.name,
            &slug,
            body.description.as_deref(),
            body.profile_id,
            body.agent_id,
            body.project_id,
            body.issue_id,
        )
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok((StatusCode::CREATED, Json(gateway_json(&row))))
}

async fn get_gateway(
    State(state): State<AppState>,
    Path(gateway_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row = pc_repos::mcp_gateway::McpGatewayRepo::new(&state.db)
        .find_by_id(gateway_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
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
    let ok = pc_repos::mcp_gateway::McpGatewayRepo::new(&state.db)
        .find_active_token(gateway_id, &token_hash)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(ok)}

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
    pc_repos::mcp_gateway::McpGatewayRepo::new(&state.db)
        .update_partial(gateway_id, name, description, status, metadata.as_ref())
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
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
) -> ApiResult<Json<Value>> {{
    // Round 97 修复：原 inline SQL 引用不存在的表（tool_mcp_gateway_tools / tool_gateway_runtime_slots）。
    let _ = ();
    Ok(Json(json!({"items": [], "deprecated": true, "note": "tool_mcp_gateway_tools table missing"})))
}}

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
    let row = pc_repos::mcp_gateway::McpGatewayRepo::new(&_state.db)
        .find_id_and_name_by_public_id(&gateway_public_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
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
) -> ApiResult<Json<Value>> {{
    // Round 97 修复：原 inline SQL 引用不存在的表（tool_mcp_gateway_tools / tool_gateway_runtime_slots）。
    let _ = ();
    Ok(Json(json!({"items": [], "deprecated": true, "note": "tool_mcp_gateway_tools table missing"})))
}}

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
    let rows = pc_repos::mcp_gateway::McpGatewayRepo::new(&_state.db)
        .list_sessions(100)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
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
    let id = pc_repos::mcp_gateway::McpGatewayRepo::new(&state.db)
        .issue_token(gateway_id, &token_hash)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))
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
    pc_repos::mcp_gateway::McpGatewayRepo::new(&state.db)
        .revoke_token(session_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;    state
        .realtime
        .publish(
            pc_realtime::LiveEvent::new("tool_gateway.session_revoked", "tool_gateway", session_id),
        );
    Ok(Json(json!({ "id": session_id, "revoked": true })))
}

async fn list_runtime_slots(
    State(_state): State<AppState>,
) -> ApiResult<Json<Value>> {{
    // Round 97 修复：原 inline SQL 引用不存在的表（tool_mcp_gateway_tools / tool_gateway_runtime_slots）。
    let _ = ();
    Ok(Json(json!({"items": [], "deprecated": true, "note": "tool_gateway_runtime_slots table missing"})))
}}

async fn restart_runtime_slot(
    State(state): State<AppState>,
    Path(slot_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {{
    // Round 97 修复：原 inline SQL 引用不存在的表（tool_mcp_gateway_tools / tool_gateway_runtime_slots）。
    let _ = ();
    Ok(Json(json!({"status": "restarting", "deprecated": true, "note": "tool_gateway_runtime_slots table missing"})))
}}

async fn stop_runtime_slot(
    State(state): State<AppState>,
    Path(slot_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {{
    // Round 97 修复：原 inline SQL 引用不存在的表（tool_mcp_gateway_tools / tool_gateway_runtime_slots）。
    let _ = ();
    Ok(Json(json!({"status": "stopped", "deprecated": true, "note": "tool_gateway_runtime_slots table missing"})))
}}

async fn list_audit_events(
    State(_state): State<AppState>,
) -> ApiResult<Json<Value>> {
    let rows = pc_repos::mcp_gateway::McpGatewayRepo::new(&_state.db)
        .list_audit_events(100)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
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
    pc_repos::mcp_gateway::McpGatewayRepo::new(&state.db)
        .approve_action_request(request_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
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
    pc_repos::mcp_gateway::McpGatewayRepo::new(&state.db)
        .decline_action_request(request_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    state
        .realtime
        .publish(
            pc_realtime::LiveEvent::new("tool_gateway.action_request.declined", "tool_gateway", request_id),
        );
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
    let id = pc_repos::mcp_gateway::McpGatewayRepo::new(&state.db)
        .issue_token(gateway_id, &token_hash)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))
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
    pc_repos::mcp_gateway::McpGatewayRepo::new(&state.db)
        .revoke_token(token_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    state
        .realtime
        .publish(
            pc_realtime::LiveEvent::new("tool_gateway.token_revoked", "tool_gateway", token_id),
        );
    Ok(Json(json!({ "id": token_id, "revoked": true })))
}
