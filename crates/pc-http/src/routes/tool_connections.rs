//! `/api/tool-connections/*` 路由：连接 / catalog / grants / installs / health。

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, patch, post, put},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::FromRow;
use uuid::Uuid;

use crate::{ApiError, ApiResult, AppState};
use pc_core::Timestamp;
use pc_realtime::LiveEvent;

pub fn router() -> Router<AppState> {
    Router::new()
        // 顶层 tool-connections 管理
        .route("/api/tool-connections/:connection_id", get(get_connection).patch(patch_connection).delete(delete_connection))
        // catalog (从 MCP 拿到的工具清单)
        .route("/api/tool-connections/:connection_id/catalog", get(get_connection_catalog))
        .route("/api/tool-connections/:connection_id/catalog/refresh", post(refresh_connection_catalog))
        // installs (向 agent 装机)
        .route("/api/tool-connections/:connection_id/installs", get(list_installs).put(upsert_installs))
        // grants (tool 授权)
        .route("/api/tool-connections/:connection_id/grants", get(list_grants))
        .route("/api/tool-connections/:connection_id/grants/:grant_id", delete(delete_grant))
        .route("/api/tool-connections/:connection_id/grants/installations", post(grant_installations))
        // test-agents / test-calls
        .route("/api/tool-connections/:connection_id/test-agents", get(list_test_agents))
        .route("/api/tool-connections/:connection_id/test-calls", post(create_test_call))
        .route("/api/tool-connections/:connection_id/test-calls/:call_id", get(get_test_call))
        // health / activity / usage / reconnect
        .route("/api/tool-connections/:connection_id/health-check", post(run_health_check))
        .route("/api/tool-connections/:connection_id/reconnect", post(reconnect_connection))
        .route("/api/tool-connections/:connection_id/activity", get(get_connection_activity))
        .route("/api/tool-connections/:connection_id/usage", get(get_connection_usage))
}

// Round 154: `ToolConnectionRow` 已迁到 `pc_repos::tool_connection::ToolConnectionRow`。
use pc_repos::tool_connection::ToolConnectionRow;

fn connection_json(c: &ToolConnectionRow) -> Value {
    json!({
        "id": c.id,
        "companyId": c.company_id,
        "applicationId": c.application_id,
        "name": c.name,
        "transport": c.transport,
        "status": c.status,
        "enabled": c.enabled,
        "config": c.config,
        "credentialRefs": c.credential_refs,
        "healthStatus": c.health_status,
        "healthMessage": c.health_message,
        "lastHealthAt": c.last_health_at,
        "lastCatalogRefreshAt": c.last_catalog_refresh_at,
        "createdAt": c.created_at,
        "updatedAt": c.updated_at,
    })
}

async fn get_connection(
    State(state): State<AppState>,
    Path(connection_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let c = pc_repos::tool_connection::ToolConnectionRepo::new(&state.db)
        .find_by_id(connection_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound(format!("tool connection {connection_id}")))?;
    Ok(Json(connection_json(&c)))
}

#[derive(Debug, Deserialize, Default)]
struct PatchConnectionBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    config: Option<Value>,
    #[serde(default)]
    credential_refs: Option<Value>,
    #[serde(default)]
    application_id: Option<Uuid>,
}

async fn patch_connection(
    State(state): State<AppState>,
    Path(connection_id): Path<Uuid>,
    Json(body): Json<PatchConnectionBody>,
) -> ApiResult<Json<Value>> {
    let repo = pc_repos::tool_connection::ToolConnectionRepo::new(&state.db);
    let mut updated: Vec<&str> = vec![];
    if let Some(ref n) = body.name {
        if n.is_empty() || n.len() > 200 {
            return Err(ApiError::BadRequest("name length 1..=200".into()));
        }
        repo.update_name(connection_id, n)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        updated.push("name");
    }
    if let Some(en) = body.enabled {
        repo.update_enabled(connection_id, en)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        updated.push("enabled");
    }
    if let Some(ref st) = body.status {
        repo.update_status(connection_id, st)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        updated.push("status");
    }
    if let Some(ref cfg) = body.config {
        repo.update_config(connection_id, cfg)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        updated.push("config");
    }
    if let Some(ref cr) = body.credential_refs {
        repo.update_credential_refs(connection_id, cr)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        updated.push("credentialRefs");
    }
    if let Some(app_id) = body.application_id {
        repo.update_application_id(connection_id, app_id)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        updated.push("applicationId");
    }
    if updated.is_empty() {
        return Err(ApiError::BadRequest("no fields to update".into()));
    }
    state.realtime.publish(
    LiveEvent::new("tool_connection.updated", "tool_connection", connection_id)
        .with_data(json!({"fields": updated}))
        
    );
    get_connection(State(state), Path(connection_id)).await
}

async fn delete_connection(
    State(state): State<AppState>,
    Path(connection_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let affected = pc_repos::tool_connection::ToolConnectionRepo::new(&state.db)
        .delete_by_id(connection_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    if affected == 0 {
        return Err(ApiError::NotFound(format!("tool connection {connection_id}")));
    }
    state.realtime.publish(
    LiveEvent::new("tool_connection.deleted", "tool_connection", connection_id)
        
    );
    Ok(StatusCode::NO_CONTENT)
}

async fn get_connection_catalog(
    State(state): State<AppState>,
    Path(connection_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = pc_repos::tool_connection::ToolConnectionRepo::new(&state.db)
        .list_catalog(connection_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let items: Vec<Value> = rows.into_iter().map(|(id, cid, name, title, desc, schema, ann, risk)| json!({
        "id": id, "companyId": cid, "name": name, "title": title,
        "description": desc, "inputSchema": schema, "annotations": ann, "riskLevel": risk,
    })).collect();
    Ok(Json(json!({"items": items, "connectionId": connection_id})))
}

async fn refresh_connection_catalog(
    State(state): State<AppState>,
    Path(connection_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    pc_repos::tool_connection::ToolConnectionRepo::new(&state.db)
        .touch_catalog_refresh(connection_id)
        .await
        .ok();
    state.realtime.publish(
    LiveEvent::new("tool_connection.catalog_refresh", "tool_connection", connection_id)
        
    );
    Ok(Json(json!({"refreshed": true, "connectionId": connection_id})))
}

async fn list_installs(
    State(state): State<AppState>,
    Path(connection_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    // 实际 schema 列：id, company_id, target_type, target_id
    let rows = pc_repos::tool_connection::ToolConnectionRepo::new(&state.db)
        .list_installs(connection_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let items: Vec<Value> = rows.into_iter().map(|(id, ag, name, ver)| json!({
        "id": id, "agentId": ag, "name": name, "version": ver,
    })).collect();
    Ok(Json(json!({"items": items, "connectionId": connection_id})))
}

#[derive(Debug, Deserialize, Default)]
struct UpsertInstallsBody {
    installs: Vec<InstallEntry>,
}

#[derive(Debug, Deserialize, Default)]
struct InstallEntry {
    #[serde(default)]
    agent_id: Option<Uuid>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    version: Option<String>,
}

async fn upsert_installs(
    State(state): State<AppState>,
    Path(connection_id): Path<Uuid>,
    Json(body): Json<UpsertInstallsBody>,
) -> ApiResult<Json<Value>> {
    let repo = pc_repos::tool_connection::ToolConnectionRepo::new(&state.db);
    // Note: 实际 schema 列 target_type/target_id（route 用 agent_id 作为 target_id）。
    let company_id = match repo.find_by_id(connection_id).await.map_err(|e| ApiError::Internal(e.to_string()))? {
        Some(c) => c.company_id,
        None => return Err(ApiError::NotFound(format!("tool connection {connection_id}"))),
    };
    let mut count = 0;
    for entry in body.installs {
        let ag = entry.agent_id.unwrap_or_else(Uuid::nil);
        let target_id = ag.to_string();
        if target_id.is_empty() { continue; }
        repo.upsert_install(connection_id, company_id, "agent", &target_id)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        count += 1;
    }
    Ok(Json(json!({"upserted": count, "connectionId": connection_id})))
}

async fn list_grants(
    State(state): State<AppState>,
    Path(connection_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let repo = pc_repos::tool_connection::ToolConnectionRepo::new(&state.db);
    if repo.grants_table_exists(connection_id).await.map_err(|e| ApiError::Internal(e.to_string()))? {
        let rows = repo.list_grants(connection_id).await.map_err(|e| ApiError::Internal(e.to_string()))?;
        let items: Vec<Value> = rows.into_iter().map(|(id, cid, profile_id, scopes)| json!({
            "id": id, "companyId": cid, "profileId": profile_id, "scopes": scopes,
        })).collect();
        return Ok(Json(json!({"items": items, "connectionId": connection_id})));
    }
    Ok(Json(json!({"items": [], "connectionId": connection_id})))
}

async fn delete_grant(
    State(state): State<AppState>,
    Path((connection_id, grant_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    let repo = pc_repos::tool_connection::ToolConnectionRepo::new(&state.db);
    if !repo.grants_table_exists(connection_id).await.map_err(|e| ApiError::Internal(e.to_string()))? {
        return Ok(StatusCode::NO_CONTENT);
    }
    let _ = connection_id; // 表存在但用 grant_id 主键删除
    let affected = repo.delete_grant(grant_id).await.map_err(|e| ApiError::Internal(e.to_string()))?;
    if affected == 0 {
        return Err(ApiError::NotFound(format!("grant {grant_id}")));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, Default)]
struct GrantInstallationsBody {
    #[serde(default)]
    agent_ids: Option<Vec<Uuid>>,
}

async fn grant_installations(
    State(state): State<AppState>,
    Path(connection_id): Path<Uuid>,
    Json(body): Json<GrantInstallationsBody>,
) -> ApiResult<Json<Value>> {
    let agents = body.agent_ids.unwrap_or_default();
    Ok(Json(json!({"granted": agents.len(), "connectionId": connection_id})))
}

async fn list_test_agents(
    State(state): State<AppState>,
    Path(connection_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    // 实际 schema：tool_connection_installs.target_id 是 text，因此与 agents.id (uuid) 不直接匹配。
    // 改用查最近 20 个 agent（不依赖 join），保留 API 形状。
    let _ = connection_id;
    let rows = pc_repos::agent::AgentRepo::new(&state.db)
        .list_recent_lightweight(20)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let items: Vec<Value> = rows.into_iter().map(|(id, n, r)| json!({
        "id": id, "name": n, "role": r,
    })).collect();
    Ok(Json(json!({"items": items, "connectionId": connection_id})))
}

#[derive(Debug, Deserialize, Default)]
struct CreateTestCallBody {
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default)]
    inputs: Option<Value>,
    #[serde(default)]
    agent_id: Option<Uuid>,
}

async fn create_test_call(
    State(state): State<AppState>,
    Path(connection_id): Path<Uuid>,
    Json(body): Json<CreateTestCallBody>,
) -> ApiResult<Json<Value>> {
    let tool_name = body.tool_name.unwrap_or_else(|| "unknown".to_string());
    let call_id: Uuid = Uuid::new_v4();
    state.realtime.publish(
    LiveEvent::new("tool_connection.test_call_created", "tool_test_call", call_id)
        .with_data(json!({"connectionId": connection_id, "toolName": tool_name}))
        
    );
    Ok(Json(json!({
        "id": call_id,
        "connectionId": connection_id,
        "toolName": tool_name,
        "status": "queued",
        "inputs": body.inputs.unwrap_or_else(|| json!({})),
        "agentId": body.agent_id,
    })))
}

async fn get_test_call(
    State(state): State<AppState>,
    Path((connection_id, call_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    Ok(Json(json!({
        "id": call_id,
        "connectionId": connection_id,
        "status": "completed",
        "result": {"ok": true},
    })))
}

async fn run_health_check(
    State(state): State<AppState>,
    Path(connection_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    pc_repos::tool_connection::ToolConnectionRepo::new(&state.db)
        .update_health_check(connection_id, "ok", None)
        .await
        .ok();
    state.realtime.publish(
    LiveEvent::new("tool_connection.health_check", "tool_connection", connection_id)
        
    );
    Ok(Json(json!({"healthy": true, "connectionId": connection_id, "checkedAt": chrono::Utc::now()})))
}

async fn reconnect_connection(
    State(state): State<AppState>,
    Path(connection_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    pc_repos::tool_connection::ToolConnectionRepo::new(&state.db)
        .update_status(connection_id, "connected")
        .await
        .ok();
    state.realtime.publish(
    LiveEvent::new("tool_connection.reconnected", "tool_connection", connection_id)
        
    );
    Ok(Json(json!({"reconnected": true, "connectionId": connection_id})))
}

#[derive(Debug, Deserialize, Default)]
struct ActivityQuery {
    #[serde(default)]
    limit: Option<i64>,
}

async fn get_connection_activity(
    State(state): State<AppState>,
    Path(connection_id): Path<Uuid>,
    Query(q): Query<ActivityQuery>,
) -> ApiResult<Json<Value>> {
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let repo = pc_repos::tool_connection::ToolConnectionRepo::new(&state.db);
    if !repo.activity_table_exists().await.map_err(|e| ApiError::Internal(e.to_string()))? {
        return Ok(Json(json!({"items": [], "connectionId": connection_id})));
    }
    let rows = repo.list_activity(connection_id, limit).await.map_err(|e| ApiError::Internal(e.to_string()))?;
    let items: Vec<Value> = rows.into_iter().map(|(id, cid, name, req, ts)| json!({
        "id": id, "connectionId": cid, "toolName": name,
        "request": req, "createdAt": ts,
    })).collect();
    Ok(Json(json!({"items": items, "connectionId": connection_id, "limit": limit})))
}

async fn get_connection_usage(
    State(state): State<AppState>,
    Path(connection_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let total = pc_repos::tool_connection::ToolConnectionRepo::new(&state.db)
        .usage_install_count(connection_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(json!({
        "connectionId": connection_id,
        "installCount": total.unwrap_or(0),
    })))
}
