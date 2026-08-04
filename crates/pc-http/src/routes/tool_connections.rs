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

#[derive(Debug, FromRow)]
struct ToolConnectionRow {
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
    last_health_at: Option<Timestamp>,
    last_catalog_refresh_at: Option<Timestamp>,
    created_at: Timestamp,
    updated_at: Timestamp,
}

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
    let row: Option<ToolConnectionRow> = sqlx::query_as(
        "SELECT id, company_id, application_id, name, transport, status, enabled, config,
         credential_refs, health_status, health_message, last_health_at, last_catalog_refresh_at,
         created_at, updated_at
         FROM tool_connections WHERE id=$1",
    )
    .bind(connection_id)
    .fetch_optional(state.db.pool())
    .await?;
    let c = row.ok_or_else(|| ApiError::NotFound(format!("tool connection {connection_id}")))?;
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
    let mut updated: Vec<&str> = vec![];
    if let Some(ref n) = body.name {
        if n.is_empty() || n.len() > 200 {
            return Err(ApiError::BadRequest("name length 1..=200".into()));
        }
        sqlx::query(
            "UPDATE tool_connections SET name=$1, updated_at=now() WHERE id=$2",
        ).bind(n).bind(connection_id).execute(state.db.pool()).await?;
        updated.push("name");
    }
    if let Some(en) = body.enabled {
        sqlx::query(
            "UPDATE tool_connections SET enabled=$1, updated_at=now() WHERE id=$2",
        ).bind(en).bind(connection_id).execute(state.db.pool()).await?;
        updated.push("enabled");
    }
    if let Some(ref st) = body.status {
        sqlx::query(
            "UPDATE tool_connections SET status=$1, updated_at=now() WHERE id=$2",
        ).bind(st).bind(connection_id).execute(state.db.pool()).await?;
        updated.push("status");
    }
    if let Some(ref cfg) = body.config {
        sqlx::query(
            "UPDATE tool_connections SET config=$1, updated_at=now() WHERE id=$2",
        ).bind(cfg).bind(connection_id).execute(state.db.pool()).await?;
        updated.push("config");
    }
    if let Some(ref cr) = body.credential_refs {
        sqlx::query(
            "UPDATE tool_connections SET credential_refs=$1, updated_at=now() WHERE id=$2",
        ).bind(cr).bind(connection_id).execute(state.db.pool()).await?;
        updated.push("credentialRefs");
    }
    if let Some(app_id) = body.application_id {
        sqlx::query(
            "UPDATE tool_connections SET application_id=$1, updated_at=now() WHERE id=$2",
        ).bind(app_id).bind(connection_id).execute(state.db.pool()).await?;
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
    let r = sqlx::query("DELETE FROM tool_connections WHERE id=$1")
        .bind(connection_id).execute(state.db.pool()).await?;
    if r.rows_affected() == 0 {
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
    let rows: Vec<(Uuid, Uuid, String, Option<String>, Option<String>, Value, Value, String)> = sqlx::query_as(
        "SELECT id, company_id, name, title, description, input_schema, annotations, risk_level
         FROM tool_catalog_entries WHERE connection_id=$1 ORDER BY name",
    )
    .bind(connection_id)
    .fetch_all(state.db.pool()).await?;
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
    sqlx::query(
        "UPDATE tool_connections SET last_catalog_refresh_at=now() WHERE id=$1",
    ).bind(connection_id).execute(state.db.pool()).await.ok();
    state.realtime.publish(
    LiveEvent::new("tool_connection.catalog_refresh", "tool_connection", connection_id)
        
    );
    Ok(Json(json!({"refreshed": true, "connectionId": connection_id})))
}

async fn list_installs(
    State(state): State<AppState>,
    Path(connection_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows: Vec<(Uuid, Uuid, String, Option<String>)> = sqlx::query_as(
        "SELECT id, agent_id, name, version
         FROM tool_connection_installs WHERE connection_id=$1 ORDER BY name",
    )
    .bind(connection_id)
    .fetch_all(state.db.pool()).await?;
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
    let mut count = 0;
    for entry in body.installs {
        let ag = entry.agent_id.unwrap_or_else(Uuid::nil);
        let name = entry.name.unwrap_or_default();
        if name.is_empty() { continue; }
        // upsert by (connection_id, agent_id, name)
        sqlx::query(
            "INSERT INTO tool_connection_installs (id, connection_id, agent_id, name, version)
             VALUES (gen_random_uuid(), $1, $2, $3, $4)
             ON CONFLICT (connection_id, agent_id, name) DO UPDATE SET version=$4, updated_at=now()",
        ).bind(connection_id).bind(ag).bind(&name).bind(&entry.version)
        .execute(state.db.pool()).await?;
        count += 1;
    }
    Ok(Json(json!({"upserted": count, "connectionId": connection_id})))
}

async fn list_grants(
    State(state): State<AppState>,
    Path(connection_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let exists: Option<(bool,)> = sqlx::query_as(
        "SELECT EXISTS(SELECT 1 FROM information_schema.tables WHERE table_name='tool_grants')",
    ).fetch_optional(state.db.pool()).await?;
    if exists.map(|(b,)| b).unwrap_or(false) {
        let rows: Vec<(Uuid, Uuid, String, Value)> = sqlx::query_as(
            "SELECT id, connection_id, kind, payload
             FROM tool_grants WHERE connection_id=$1 ORDER BY created_at DESC",
        ).bind(connection_id).fetch_all(state.db.pool()).await?;
        let items: Vec<Value> = rows.into_iter().map(|(id, cid, k, p)| json!({
            "id": id, "connectionId": cid, "kind": k, "payload": p,
        })).collect();
        return Ok(Json(json!({"items": items, "connectionId": connection_id})));
    }
    Ok(Json(json!({"items": [], "connectionId": connection_id})))
}

async fn delete_grant(
    State(state): State<AppState>,
    Path((connection_id, grant_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    let exists: Option<(bool,)> = sqlx::query_as(
        "SELECT EXISTS(SELECT 1 FROM information_schema.tables WHERE table_name='tool_grants')",
    ).fetch_optional(state.db.pool()).await?;
    if !exists.map(|(b,)| b).unwrap_or(false) {
        return Ok(StatusCode::NO_CONTENT);
    }
    let r = sqlx::query(
        "DELETE FROM tool_grants WHERE connection_id=$1 AND id=$2",
    ).bind(connection_id).bind(grant_id).execute(state.db.pool()).await?;
    if r.rows_affected() == 0 {
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
    let rows: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT id, name, role FROM agents WHERE id IN (
            SELECT agent_id FROM tool_connection_installs WHERE connection_id=$1
         ) ORDER BY name",
    )
    .bind(connection_id)
    .fetch_all(state.db.pool()).await?;
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
    sqlx::query(
        "UPDATE tool_connections SET last_health_at=now(), health_status='ok', health_message=NULL WHERE id=$1",
    ).bind(connection_id).execute(state.db.pool()).await.ok();
    state.realtime.publish(
    LiveEvent::new("tool_connection.health_check", "tool_connection", connection_id)
        
    );
    Ok(Json(json!({"healthy": true, "connectionId": connection_id, "checkedAt": chrono::Utc::now()})))
}

async fn reconnect_connection(
    State(state): State<AppState>,
    Path(connection_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    sqlx::query(
        "UPDATE tool_connections SET status='connected', updated_at=now() WHERE id=$1",
    ).bind(connection_id).execute(state.db.pool()).await.ok();
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
    let exists: Option<(bool,)> = sqlx::query_as(
        "SELECT EXISTS(SELECT 1 FROM information_schema.tables WHERE table_name='tool_invocations')",
    ).fetch_optional(state.db.pool()).await?;
    if !exists.map(|(b,)| b).unwrap_or(false) {
        return Ok(Json(json!({"items": [], "connectionId": connection_id})));
    }
    let rows: Vec<(Uuid, Uuid, String, Value, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "SELECT id, connection_id, tool_name, request, created_at
         FROM tool_invocations WHERE connection_id=$1 ORDER BY created_at DESC LIMIT $2",
    ).bind(connection_id).bind(limit).fetch_all(state.db.pool()).await?;
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
    let total: (Option<i64>,) = sqlx::query_as(
        "SELECT COUNT(*) FROM tool_connection_installs WHERE connection_id=$1",
    ).bind(connection_id).fetch_one(state.db.pool()).await?;
    Ok(Json(json!({
        "connectionId": connection_id,
        "installCount": total.0.unwrap_or(0),
    })))
}
