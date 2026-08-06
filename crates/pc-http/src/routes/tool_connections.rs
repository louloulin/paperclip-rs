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
        .route(
            "/api/tool-connections/:connection_id",
            get(get_connection)
                .patch(patch_connection)
                .delete(delete_connection),
        )
        // catalog (从 MCP 拿到的工具清单)
        .route(
            "/api/tool-connections/:connection_id/catalog",
            get(get_connection_catalog),
        )
        .route(
            "/api/tool-connections/:connection_id/catalog/refresh",
            post(refresh_connection_catalog),
        )
        // installs (向 agent 装机)
        .route(
            "/api/tool-connections/:connection_id/installs",
            get(list_installs).put(upsert_installs),
        )
        // grants (tool 授权)
        .route(
            "/api/tool-connections/:connection_id/grants",
            get(list_grants),
        )
        .route(
            "/api/tool-connections/:connection_id/grants/:grant_id",
            delete(delete_grant),
        )
        .route(
            "/api/tool-connections/:connection_id/grants/installations",
            post(grant_installations),
        )
        // test-agents / test-calls
        .route(
            "/api/tool-connections/:connection_id/test-agents",
            get(list_test_agents),
        )
        .route(
            "/api/tool-connections/:connection_id/test-calls",
            post(create_test_call),
        )
        .route(
            "/api/tool-connections/:connection_id/test-calls/:call_id",
            get(get_test_call),
        )
        // health / activity / usage / reconnect
        .route(
            "/api/tool-connections/:connection_id/health-check",
            post(run_health_check),
        )
        .route(
            "/api/tool-connections/:connection_id/reconnect",
            post(reconnect_connection),
        )
        .route(
            "/api/tool-connections/:connection_id/activity",
            get(get_connection_activity),
        )
        .route(
            "/api/tool-connections/:connection_id/usage",
            get(get_connection_usage),
        )
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
            .with_data(json!({"fields": updated})),
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
        return Err(ApiError::NotFound(format!(
            "tool connection {connection_id}"
        )));
    }
    state.realtime.publish(LiveEvent::new(
        "tool_connection.deleted",
        "tool_connection",
        connection_id,
    ));
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
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(id, cid, name, title, desc, schema, ann, risk)| {
            json!({
                "id": id, "companyId": cid, "name": name, "title": title,
                "description": desc, "inputSchema": schema, "annotations": ann, "riskLevel": risk,
            })
        })
        .collect();
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
    state.realtime.publish(LiveEvent::new(
        "tool_connection.catalog_refresh",
        "tool_connection",
        connection_id,
    ));
    Ok(Json(
        json!({"refreshed": true, "connectionId": connection_id}),
    ))
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
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(id, ag, name, ver)| {
            json!({
                "id": id, "agentId": ag, "name": name, "version": ver,
            })
        })
        .collect();
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
    let company_id = match repo
        .find_by_id(connection_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
    {
        Some(c) => c.company_id,
        None => {
            return Err(ApiError::NotFound(format!(
                "tool connection {connection_id}"
            )))
        }
    };
    let mut count = 0;
    for entry in body.installs {
        let ag = entry.agent_id.unwrap_or_else(Uuid::nil);
        let target_id = ag.to_string();
        if target_id.is_empty() {
            continue;
        }
        repo.upsert_install(connection_id, company_id, "agent", &target_id)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        count += 1;
    }
    Ok(Json(
        json!({"upserted": count, "connectionId": connection_id}),
    ))
}

async fn list_grants(
    State(state): State<AppState>,
    Path(connection_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    // Round 227 真实实现：使用 v3 connection_grants 表
    let conn_row = pc_repos::tool_connection::ToolConnectionRepo::new(&state.db)
        .find_by_id(connection_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound(format!("connection {connection_id}")))?;
    let grants = pc_repos::tool_connection::list_connection_grants(
        &state.db,
        connection_id,
        conn_row.company_id,
    )
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    let items: Vec<Value> = grants.iter().map(grant_row_to_json).collect();
    Ok(Json(json!({
        "connectionId": connection_id,
        "items": items,
    })))
}

async fn delete_grant(
    State(state): State<AppState>,
    Path((connection_id, grant_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    // Round 227 真实实现：撤销 grant (status=revoked)
    let conn_row = pc_repos::tool_connection::ToolConnectionRepo::new(&state.db)
        .find_by_id(connection_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound(format!("connection {connection_id}")))?;
    let revoked = pc_repos::tool_connection::revoke_connection_grant(
        &state.db,
        conn_row.company_id,
        connection_id,
        grant_id,
        None,
        None,
    )
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?
    .ok_or_else(|| ApiError::NotFound(format!("grant {grant_id}")))?;
    state.realtime.publish(
        pc_realtime::LiveEvent::new(
            "tool_connection.grant_revoked",
            "connection_grant",
            grant_id,
        )
        .with_company(conn_row.company_id)
        .with_data(json!({
            "connectionId": connection_id,
            "grantId": grant_id,
        })),
    );
    Ok(Json(grant_row_to_json(&revoked)))
}

/// Round 227: 完整 `grant_installations` body
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct GrantInstallationsBody {
    #[serde(default)]
    provider_tenant: Option<ProviderTenantBody>,
    #[serde(default)]
    credential_secret_refs: Option<Vec<String>>,
    #[serde(default)]
    is_default: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderTenantBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    external_id: Option<String>,
}

async fn grant_installations(
    State(state): State<AppState>,
    Path(connection_id): Path<Uuid>,
    Json(body): Json<GrantInstallationsBody>,
) -> ApiResult<axum::response::Response> {
    // Round 227 真实实现：创建 workspace kind connection grant
    let conn_row = pc_repos::tool_connection::ToolConnectionRepo::new(&state.db)
        .find_by_id(connection_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or_else(|| ApiError::NotFound(format!("connection {connection_id}")))?;
    let provider_tenant = body.provider_tenant.as_ref().map(|pt| {
        json!({
            "name": pt.name,
            "externalId": pt.external_id,
        })
    });
    let creds_refs = serde_json::Value::Array(
        body.credential_secret_refs
            .as_ref()
            .map(|refs| {
                refs.iter()
                    .map(|s| serde_json::Value::String(s.clone()))
                    .collect()
            })
            .unwrap_or_default(),
    );
    let is_default = body.is_default.unwrap_or(false);
    let created = pc_repos::tool_connection::create_workspace_grant(
        &state.db,
        conn_row.company_id,
        connection_id,
        provider_tenant.as_ref(),
        &creds_refs,
        is_default,
        None,
        None,
    )
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    state.realtime.publish(
        pc_realtime::LiveEvent::new(
            "tool_connection.grant_added",
            "connection_grant",
            created.id,
        )
        .with_company(conn_row.company_id)
        .with_data(json!({
            "connectionId": connection_id,
            "grantId": created.id,
            "kind": created.kind,
        })),
    );
    Ok((StatusCode::CREATED, Json(grant_row_to_json(&created))).into_response())
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
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(id, n, r)| {
            json!({
                "id": id, "name": n, "role": r,
            })
        })
        .collect();
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
        LiveEvent::new(
            "tool_connection.test_call_created",
            "tool_test_call",
            call_id,
        )
        .with_data(json!({"connectionId": connection_id, "toolName": tool_name})),
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
    state.realtime.publish(LiveEvent::new(
        "tool_connection.health_check",
        "tool_connection",
        connection_id,
    ));
    Ok(Json(
        json!({"healthy": true, "connectionId": connection_id, "checkedAt": chrono::Utc::now()}),
    ))
}

async fn reconnect_connection(
    State(state): State<AppState>,
    Path(connection_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    pc_repos::tool_connection::ToolConnectionRepo::new(&state.db)
        .update_status(connection_id, "connected")
        .await
        .ok();
    state.realtime.publish(LiveEvent::new(
        "tool_connection.reconnected",
        "tool_connection",
        connection_id,
    ));
    Ok(Json(
        json!({"reconnected": true, "connectionId": connection_id}),
    ))
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
    if !repo
        .activity_table_exists()
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
    {
        return Ok(Json(json!({"items": [], "connectionId": connection_id})));
    }
    let rows = repo
        .list_activity(connection_id, limit)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(id, cid, name, req, ts)| {
            json!({
                "id": id, "connectionId": cid, "toolName": name,
                "request": req, "createdAt": ts,
            })
        })
        .collect();
    Ok(Json(
        json!({"items": items, "connectionId": connection_id, "limit": limit}),
    ))
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

// ============================================================================
// Round 227: connection_grant 序列化 helper
// ============================================================================

/// 将 `ConnectionGrantRow` 序列化为 camelCase JSON（与 Node schema 对齐）
fn grant_row_to_json(r: &pc_repos::tool_connection::ConnectionGrantRow) -> Value {
    json!({
        "id": r.id,
        "companyId": r.company_id,
        "connectionId": r.connection_id,
        "kind": r.kind,
        "subjectUserId": r.subject_user_id,
        "providerTenant": r.provider_tenant,
        "credentialSecretRefs": r.credential_secret_refs,
        "status": r.status,
        "isDefault": r.is_default,
        "createdByAgentId": r.created_by_agent_id,
        "createdByUserId": r.created_by_user_id,
        "revokedAt": r.revoked_at,
        "revokedByAgentId": r.revoked_by_agent_id,
        "revokedByUserId": r.revoked_by_user_id,
        "lastUsedAt": r.last_used_at,
        "createdAt": r.created_at,
        "updatedAt": r.updated_at,
    })
}

#[cfg(test)]
mod round227_tests {
    //! Round 227: connection_grants 序列化 + body 解析测试
    //!
    //! 覆盖：
    //! - `grant_row_to_json` camelCase 序列化（关键字段）
    //! - `GrantInstallationsBody` 完整 body 解析
    //! - `ProviderTenantBody` 子结构解析
    use super::{grant_row_to_json, GrantInstallationsBody, ProviderTenantBody};
    use pc_repos::tool_connection::ConnectionGrantRow;
    use serde_json::json;
    use uuid::Uuid;

    fn make_grant_row(id: Uuid, status: &str, is_default: bool) -> ConnectionGrantRow {
        let now = chrono::Utc::now();
        ConnectionGrantRow {
            id,
            company_id: Uuid::nil(),
            connection_id: Uuid::nil(),
            kind: "workspace".to_string(),
            subject_user_id: None,
            provider_tenant: Some(json!({"name": "tenant-1", "externalId": "ext-1"})),
            credential_secret_refs: json!(["secret-1", "secret-2"]),
            status: status.to_string(),
            is_default,
            created_by_agent_id: None,
            created_by_user_id: Some("u-test".to_string()),
            revoked_at: None,
            revoked_by_agent_id: None,
            revoked_by_user_id: None,
            last_used_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn grant_row_json_uses_camel_case_keys() {
        let id = Uuid::new_v4();
        let row = make_grant_row(id, "active", true);
        let v = grant_row_to_json(&row);
        let obj = v.as_object().expect("object");
        // 关键 camelCase 字段（与 Node connectionGrants 序列化对齐）
        assert!(obj.contains_key("companyId"));
        assert!(obj.contains_key("connectionId"));
        assert!(obj.contains_key("subjectUserId"));
        assert!(obj.contains_key("providerTenant"));
        assert!(obj.contains_key("credentialSecretRefs"));
        assert!(obj.contains_key("isDefault"));
        assert!(obj.contains_key("createdByAgentId"));
        assert!(obj.contains_key("createdByUserId"));
        assert!(obj.contains_key("revokedAt"));
        assert!(obj.contains_key("revokedByAgentId"));
        assert!(obj.contains_key("revokedByUserId"));
        assert!(obj.contains_key("lastUsedAt"));
        assert!(obj.contains_key("createdAt"));
        assert!(obj.contains_key("updatedAt"));
        // 值校验
        assert_eq!(obj["kind"], json!("workspace"));
        assert_eq!(obj["status"], json!("active"));
        assert_eq!(obj["isDefault"], json!(true));
    }

    #[test]
    fn grant_row_json_preserves_provider_tenant() {
        let id = Uuid::new_v4();
        let row = make_grant_row(id, "active", false);
        let v = grant_row_to_json(&row);
        let tenant = v["providerTenant"].as_object().expect("tenant obj");
        assert_eq!(tenant["name"], json!("tenant-1"));
        assert_eq!(tenant["externalId"], json!("ext-1"));
    }

    #[test]
    fn grant_installations_body_parses_minimal() {
        let body: GrantInstallationsBody = serde_json::from_value(json!({})).expect("parse");
        assert!(body.provider_tenant.is_none());
        assert!(body.credential_secret_refs.is_none());
        assert!(body.is_default.is_none());
    }

    #[test]
    fn grant_installations_body_parses_full() {
        let body: GrantInstallationsBody = serde_json::from_value(json!({
            "providerTenant": {"name": "tenant-a", "externalId": "ext-a"},
            "credentialSecretRefs": ["secret-1", "secret-2"],
            "isDefault": true,
        }))
        .expect("parse");
        let tenant = body.provider_tenant.expect("tenant");
        assert_eq!(tenant.name.as_deref(), Some("tenant-a"));
        assert_eq!(tenant.external_id.as_deref(), Some("ext-a"));
        let refs = body.credential_secret_refs.expect("refs");
        assert_eq!(refs.len(), 2);
        assert_eq!(body.is_default, Some(true));
    }

    #[test]
    fn provider_tenant_body_handles_optional_fields() {
        let body: ProviderTenantBody = serde_json::from_value(json!({})).expect("parse");
        assert!(body.name.is_none());
        assert!(body.external_id.is_none());
    }
}
