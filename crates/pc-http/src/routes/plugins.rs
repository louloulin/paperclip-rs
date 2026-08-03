//! 插件管理 HTTP API。
//!
//! 数据库资源由 `PluginRepo` 统一访问；需要 Node worker 等运行时能力的
//! endpoint 在能力未注册时返回明确的 501，而不是伪造成功响应。

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::fs;
use uuid::Uuid;

use pc_plugin_host::WorkerHandle;
use pc_plugin_protocol::{ExecuteToolParams, RunJobParams};
use pc_realtime::LiveEvent;
use pc_repos::plugin::{
    PluginConfigRow, PluginJobRow, PluginJobRunRow, PluginLogRow, PluginRegistration, PluginRepo,
    PluginRow, PluginWebhookDeliveryRow,
};

use crate::{require_user_id, ApiError, ApiResult, AppState};

const PLUGIN_STATUSES: [&str; 6] = [
    "installed",
    "ready",
    "disabled",
    "error",
    "upgrade_pending",
    "uninstalled",
];

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/plugins", get(list_plugins))
        .route("/api/plugins/examples", get(plugin_examples))
        .route("/api/plugins/ui-contributions", get(ui_contributions))
        .route("/api/plugins/tools", get(list_plugin_tools))
        .route("/api/plugins/tools/execute", post(execute_plugin_tool))
        .route("/api/plugins/install", post(install_plugin))
        .route("/api/plugins/:plugin_id/bridge/data", post(bridge_data))
        .route("/api/plugins/:plugin_id/bridge/action", post(bridge_action))
        .route("/api/plugins/:plugin_id/data/:key", post(plugin_data))
        .route("/api/plugins/:plugin_id/actions/:key", post(plugin_action))
        .route(
            "/api/plugins/:plugin_id/bridge/stream/:channel",
            get(bridge_stream),
        )
        .route(
            "/api/plugins/:plugin_id",
            get(get_plugin).delete(delete_plugin),
        )
        .route("/api/plugins/:plugin_id/enable", post(enable_plugin))
        .route("/api/plugins/:plugin_id/disable", post(disable_plugin))
        .route("/api/plugins/:plugin_id/health", get(plugin_health))
        .route("/api/plugins/:plugin_id/logs", get(plugin_logs))
        .route("/api/plugins/:plugin_id/upgrade", post(upgrade_plugin))
        .route(
            "/api/plugins/:plugin_id/config",
            get(plugin_config).post(save_plugin_config),
        )
        .route(
            "/api/plugins/:plugin_id/config/test",
            post(test_plugin_config),
        )
        .route("/api/plugins/:plugin_id/jobs", get(plugin_jobs))
        .route(
            "/api/plugins/:plugin_id/jobs/:job_id/runs",
            get(plugin_job_runs),
        )
        .route(
            "/api/plugins/:plugin_id/jobs/:job_id/trigger",
            post(trigger_plugin_job),
        )
        .route(
            "/api/plugins/:plugin_id/webhooks/:endpoint_key",
            post(receive_plugin_webhook),
        )
        .route("/api/plugins/:plugin_id/dashboard", get(plugin_dashboard))
}

#[derive(Debug, Deserialize, Default)]
struct PluginListQuery {
    status: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct DeleteQuery {
    purge: Option<bool>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct PluginConfigQuery {
    company_id: Option<Uuid>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct PluginConfigBody {
    company_id: Option<Uuid>,
    config_json: Option<Value>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct InstallBody {
    package_name: Option<String>,
    version: Option<String>,
    is_local_path: Option<bool>,
    manifest: Option<Value>,
}

#[derive(Debug, Deserialize, Default)]
struct LogQuery {
    limit: Option<i64>,
    level: Option<String>,
    since: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BundledPlugin {
    package_name: String,
    plugin_key: String,
    display_name: String,
    description: String,
    local_path: String,
    tag: String,
    experimental: bool,
    has_built_entrypoints: bool,
}

async fn require_authenticated(state: &AppState, headers: &HeaderMap) -> ApiResult<String> {
    require_user_id(state, headers).await
}

async fn require_instance_admin(state: &AppState, headers: &HeaderMap) -> ApiResult<String> {
    let user_id = require_authenticated(state, headers).await?;
    let is_admin: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM instance_user_roles \
         WHERE user_id = $1 AND role = 'instance_admin')",
    )
    .bind(&user_id)
    .fetch_one(state.db.pool())
    .await?;
    if !is_admin {
        return Err(ApiError::Forbidden("instance admin access required".into()));
    }
    Ok(user_id)
}

async fn resolve_plugin(
    repo: &PluginRepo<'_>,
    plugin_id_or_key: &str,
) -> ApiResult<Option<PluginRow>> {
    if let Ok(plugin_id) = Uuid::parse_str(plugin_id_or_key) {
        if let Some(plugin) = repo.get_by_id(plugin_id).await? {
            return Ok(Some(plugin));
        }
    }
    Ok(repo.get_by_key(plugin_id_or_key).await?)
}

fn require_status(status: Option<&str>) -> ApiResult<()> {
    if let Some(status) = status {
        if !PLUGIN_STATUSES.contains(&status) {
            return Err(ApiError::BadRequest(format!(
                "Invalid status '{status}'. Must be one of: {}",
                PLUGIN_STATUSES.join(", ")
            )));
        }
    }
    Ok(())
}

async fn list_plugins(
    State(state): State<AppState>,
    Query(query): Query<PluginListQuery>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<PluginRow>>> {
    require_authenticated(&state, &headers).await?;
    require_status(query.status.as_deref())?;
    let repo = PluginRepo::new(&state.db);
    let rows = match query.status.as_deref() {
        Some("uninstalled") => repo.list_filtered(Some("uninstalled")).await?,
        Some(status) => repo.list_filtered(Some(status)).await?,
        None => repo.list_installed().await?,
    };
    Ok(Json(rows))
}

async fn plugin_examples(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<BundledPlugin>>> {
    require_authenticated(&state, &headers).await?;
    Ok(Json(discover_bundled_plugins().await))
}

async fn ui_contributions(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<Value>>> {
    require_authenticated(&state, &headers).await?;
    let repo = PluginRepo::new(&state.db);
    let plugins = repo.list_filtered(Some("ready")).await?;
    let contributions = plugins
        .iter()
        .filter_map(ui_contribution)
        .collect::<Vec<_>>();
    Ok(Json(contributions))
}

async fn list_plugin_tools(
    State(state): State<AppState>,
    _headers: HeaderMap,
) -> impl axum::response::IntoResponse {
    // Tool listing requires aggregating across every ready worker. Until
    // WorkerHandle exposes a listTools JSON-RPC method, return 501.
    let _ = state.plugin_workers.len().await;
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({
            "error": "plugin tool discovery requires WorkerHandle::list_tools (not yet implemented)",
        })),
    )
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ToolExecBody {
    tool: Option<String>,
    parameters: Option<Value>,
    run_context: Option<Value>,
}

async fn execute_plugin_tool(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(plugin_id): Path<String>,
    Json(body): Json<ToolExecBody>,
) -> impl axum::response::IntoResponse {
    let _ = headers;
    let worker = match resolve_worker_or_501(&state, &plugin_id).await {
        Ok(w) => w,
        Err(resp) => return resp.into_response(),
    };
    let Some(tool) = body.tool else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "missing required field: tool"})),
        )
            .into_response();
    };
    let params = ExecuteToolParams {
        tool,
        args: body.parameters.unwrap_or(Value::Null),
        context: body.run_context.unwrap_or(Value::Null),
    };
    match worker.execute_tool(params).await {
        Ok(result) => (StatusCode::OK, Json(json!({"result": result}))).into_response(),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": "worker rejected request", "detail": e.to_string()})),
        )
            .into_response(),
    }
}

async fn install_plugin(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<InstallBody>,
) -> ApiResult<axum::response::Response> {
    require_instance_admin(&state, &headers).await?;
    let package_name = body
        .package_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            ApiError::BadRequest("packageName is required and must be a string".into())
        })?;
    if body.is_local_path != Some(true) && package_name.chars().any(|c| "<>:\"|?*".contains(c)) {
        return Err(ApiError::BadRequest(
            "packageName contains invalid characters".into(),
        ));
    }
    let manifest = body.manifest.ok_or_else(|| {
        ApiError::Other(anyhow::anyhow!(
            "plugin manifest loading is not enabled; provide a validated manifest or enable the plugin host"
        ))
    })?;
    let manifest_object = manifest
        .as_object()
        .ok_or_else(|| ApiError::BadRequest("manifest must be a JSON object".into()))?;
    let plugin_key = manifest_object
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| ApiError::BadRequest("manifest.id is required".into()))?;
    let version = body
        .version
        .as_deref()
        .or_else(|| manifest_object.get("version").and_then(Value::as_str))
        .unwrap_or("0.0.0");
    let categories = manifest_object
        .get("categories")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let package_path = (body.is_local_path == Some(true)).then_some(package_name);
    let repo = PluginRepo::new(&state.db);
    let api_version = manifest_object
        .get("apiVersion")
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
        .unwrap_or(1);
    let row = repo
        .register(&PluginRegistration {
            plugin_key: plugin_key.to_owned(),
            package_name: package_name.to_owned(),
            package_path: package_path.map(str::to_owned),
            version: version.to_owned(),
            api_version,
            categories,
            manifest_json: manifest,
        })
        .await?;
    state.realtime.publish(
        LiveEvent::new("plugin.installed", "plugin", row.id)
            .with_data(json!({ "pluginKey": row.plugin_key })),
    );
    Ok((StatusCode::OK, Json(row)).into_response())
}

// bridge_data: 在 plugin_entities 表按 (plugin_id, entity_type) upsert 一条记录
async fn bridge_data(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((plugin_id,)): Path<(String,)>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    require_authenticated(&state, &headers).await?;
    let pid = Uuid::parse_str(&plugin_id)
        .map_err(|_| ApiError::BadRequest("invalid plugin id".into()))?;
    let entity_type = body
        .get("entityType")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::BadRequest("entityType required".into()))?;
    let scope_kind = body
        .get("scopeKind")
        .and_then(Value::as_str)
        .unwrap_or("global");
    let scope_id = body
        .get("scopeId")
        .and_then(Value::as_str)
        .map(String::from);
    let external_id = body
        .get("externalId")
        .and_then(Value::as_str)
        .map(String::from);
    let title = body.get("title").and_then(Value::as_str).map(String::from);
    let company_id = body
        .get("companyId")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok());
    let data = body.get("data").cloned().unwrap_or(json!({}));
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO plugin_entities             (plugin_id, entity_type, scope_kind, scope_id, external_id, title, data, company_id)          VALUES ($1, $2, $3, $4, $5, $6, $7, $8)          ON CONFLICT (company_id, plugin_id, entity_type, external_id)          DO UPDATE SET data = EXCLUDED.data, title = EXCLUDED.title, updated_at = now()          RETURNING id",
    )
    .bind(pid).bind(entity_type).bind(scope_kind).bind(scope_id)
    .bind(external_id).bind(title).bind(data).bind(company_id)
    .fetch_one(state.db.pool())
    .await?;
    Ok(Json(json!({ "id": id, "ok": true })))
}

// bridge_action: 在 plugin_logs 写一条 log
async fn bridge_action(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((plugin_id,)): Path<(String,)>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    require_authenticated(&state, &headers).await?;
    let pid = Uuid::parse_str(&plugin_id)
        .map_err(|_| ApiError::BadRequest("invalid plugin id".into()))?;
    let level = body.get("level").and_then(Value::as_str).unwrap_or("info");
    let message = body
        .get("message")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::BadRequest("message required".into()))?;
    let meta = body.get("data").cloned().unwrap_or(json!({}));
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO plugin_logs (plugin_id, level, message, meta) VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(pid).bind(level).bind(message).bind(meta)
    .fetch_one(state.db.pool())
    .await?;
    state.realtime.publish(
        LiveEvent::new("plugin.action.logged", "plugin", pid)
            .with_data(json!({ "logId": id, "level": level })),
    );
    Ok(Json(json!({ "id": id, "ok": true })))
}

// plugin_data: 按 (plugin_id, entity_type, external_id) 读取单条
async fn plugin_data(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((plugin_id, key)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    require_authenticated(&state, &headers).await?;
    let pid = Uuid::parse_str(&plugin_id)
        .map_err(|_| ApiError::BadRequest("invalid plugin id".into()))?;
    let external_id = body
        .get("externalId")
        .and_then(Value::as_str)
        .unwrap_or(&key);
    let company_id = body
        .get("companyId")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok());
    let row: Option<(Uuid, Value)> = sqlx::query_as(
        "SELECT id, data FROM plugin_entities          WHERE plugin_id = $1 AND entity_type = $2 AND external_id = $3            AND ($4::uuid IS NULL OR company_id = $4)          LIMIT 1",
    )
    .bind(pid).bind(&key).bind(external_id).bind(company_id)
    .fetch_optional(state.db.pool())
    .await?;
    Ok(Json(match row {
        Some((id, data)) => json!({ "id": id, "found": true, "data": data }),
        None => json!({ "found": false }),
    }))
}

// plugin_action: 在 plugin_jobs 创建/触发一个 job
async fn plugin_action(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((plugin_id, key)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    require_authenticated(&state, &headers).await?;
    let pid = Uuid::parse_str(&plugin_id)
        .map_err(|_| ApiError::BadRequest("invalid plugin id".into()))?;
    let schedule = body
        .get("schedule")
        .and_then(Value::as_str)
        .unwrap_or("on_demand");
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO plugin_jobs (plugin_id, job_key, schedule, status) VALUES ($1, $2, $3, 'active') ON CONFLICT (plugin_id, job_key) DO UPDATE SET schedule = EXCLUDED.schedule, updated_at = now() RETURNING id",
    )
    .bind(pid).bind(&key).bind(schedule)
    .fetch_one(state.db.pool())
    .await?;
    state.realtime.publish(
        LiveEvent::new("plugin.action.queued", "plugin", pid)
            .with_data(json!({ "jobId": id, "jobKey": key })),
    );
    Ok(Json(json!({ "id": id, "ok": true, "status": "queued" })))
}

// bridge_stream: SSE 推送 plugin 事件（替代 WebSocket）
async fn bridge_stream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((plugin_id, channel)): Path<(String, String)>,
) -> ApiResult<axum::response::Response> {
    require_authenticated(&state, &headers).await?;
    let pid = Uuid::parse_str(&plugin_id)
        .map_err(|_| ApiError::BadRequest("invalid plugin id".into()))?;
    let receiver = state.realtime.subscribe();
    let stream = async_stream::stream! {
        yield Ok::<_, std::convert::Infallible>(axum::response::sse::Event::default().data(
            serde_json::to_string(&json!({
                "channel": channel,
                "pluginId": pid.to_string(),
                "type": "subscribed",
            })).unwrap_or_default()
        ));
        let mut rx = receiver;
        while let Ok(ev) = rx.recv().await {
            let payload = json!({ "channel": channel, "event": ev });
            yield Ok::<_, std::convert::Infallible>(axum::response::sse::Event::default().data(
                serde_json::to_string(&payload).unwrap_or_default()
            ));
        }
    };
    Ok(axum::response::Sse::new(stream).into_response())
}

fn worker_not_running(plugin_id_str: &str) -> (StatusCode, Json<Value>) {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({
            "error": "plugin worker not running",
            "pluginId": plugin_id_str,
            "hint": "register a worker via app.bootstrap before calling this endpoint",
        })),
    )
}

/// Resolve `plugin_id` from path → running worker handle.
///
/// Returns `Ok(Arc<WorkerHandle>)` if the plugin is registered in the in-process
/// metadata registry AND the worker pool has a live worker for it. Otherwise
/// returns `Err((StatusCode::NOT_IMPLEMENTED, Json))` with a message that the
/// HTTP layer can return verbatim.
async fn resolve_worker_or_501(
    state: &AppState,
    plugin_id_str: &str,
) -> Result<Arc<WorkerHandle>, (StatusCode, Json<Value>)> {
    let Ok(plugin_id) = Uuid::parse_str(plugin_id_str) else {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid plugin id", "value": plugin_id_str})),
        ));
    };
    if state.plugin_registry.get_by_id(&plugin_id).is_none() {
        return Err(worker_not_running(plugin_id_str));
    }
    match state.plugin_workers.get(&plugin_id).await {
        Some(handle) => Ok(handle),
        None => Err(worker_not_running(plugin_id_str)),
    }
}

async fn get_plugin(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(plugin_id): Path<String>,
) -> ApiResult<Json<Value>> {
    require_authenticated(&state, &headers).await?;
    let repo = PluginRepo::new(&state.db);
    let plugin = resolve_plugin(&repo, &plugin_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Plugin not found".into()))?;
    Ok(Json(plugin_detail(plugin)))
}

async fn delete_plugin(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<DeleteQuery>,
    Path(plugin_id): Path<String>,
) -> ApiResult<Json<PluginRow>> {
    require_instance_admin(&state, &headers).await?;
    let repo = PluginRepo::new(&state.db);
    let plugin = resolve_plugin(&repo, &plugin_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Plugin not found".into()))?;
    let result = repo
        .uninstall(plugin.id, query.purge.unwrap_or(false))
        .await?
        .ok_or_else(|| ApiError::NotFound("Plugin not found".into()))?;
    state.realtime.publish(
        LiveEvent::new("plugin.uninstalled", "plugin", result.id)
            .with_data(json!({ "purge": query.purge.unwrap_or(false) })),
    );
    Ok(Json(result))
}

async fn enable_plugin(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(plugin_id): Path<String>,
) -> ApiResult<Json<PluginRow>> {
    require_instance_admin(&state, &headers).await?;
    transition_plugin(
        &state,
        &plugin_id,
        "ready",
        &["disabled", "error", "upgrade_pending"],
    )
    .await
}

async fn disable_plugin(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(plugin_id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<PluginRow>> {
    require_instance_admin(&state, &headers).await?;
    let reason = body.get("reason").and_then(Value::as_str);
    transition_plugin_with_error(&state, &plugin_id, "disabled", &["ready"], reason).await
}

async fn transition_plugin(
    state: &AppState,
    plugin_id: &str,
    target: &str,
    allowed: &[&str],
) -> ApiResult<Json<PluginRow>> {
    transition_plugin_with_error(state, plugin_id, target, allowed, None).await
}

async fn transition_plugin_with_error(
    state: &AppState,
    plugin_id: &str,
    target: &str,
    allowed: &[&str],
    last_error: Option<&str>,
) -> ApiResult<Json<PluginRow>> {
    let repo = PluginRepo::new(&state.db);
    let plugin = resolve_plugin(&repo, plugin_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Plugin not found".into()))?;
    if !allowed.contains(&plugin.status.as_str()) {
        return Err(ApiError::BadRequest(format!(
            "Cannot transition plugin from status '{}' to '{target}'",
            plugin.status
        )));
    }
    let updated = repo
        .update_status(plugin.id, target, last_error)
        .await?
        .ok_or_else(|| ApiError::NotFound("Plugin not found".into()))?;
    state.realtime.publish(
        LiveEvent::new("plugin.status_changed", "plugin", updated.id).with_data(json!({
            "previousStatus": plugin.status,
            "newStatus": target,
            "pluginKey": updated.plugin_key
        })),
    );
    Ok(Json(updated))
}

async fn plugin_health(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(plugin_id): Path<String>,
) -> ApiResult<Json<Value>> {
    require_authenticated(&state, &headers).await?;
    let repo = PluginRepo::new(&state.db);
    let plugin = resolve_plugin(&repo, &plugin_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Plugin not found".into()))?;
    Ok(Json(health_result(&plugin)))
}

async fn plugin_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(plugin_id): Path<String>,
    Query(query): Query<LogQuery>,
) -> ApiResult<Json<Vec<PluginLogRow>>> {
    require_authenticated(&state, &headers).await?;
    let repo = PluginRepo::new(&state.db);
    let plugin = resolve_plugin(&repo, &plugin_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Plugin not found".into()))?;
    let since = query
        .since
        .as_deref()
        .and_then(|value| value.parse::<DateTime<Utc>>().ok());
    let rows = repo
        .list_logs(
            plugin.id,
            query.limit.unwrap_or(25).clamp(1, 500),
            query.level.as_deref(),
            since,
        )
        .await?;
    Ok(Json(rows))
}

async fn upgrade_plugin(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(plugin_id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<impl axum::response::IntoResponse> {
    require_authenticated(&state, &headers).await?;
    let repo = PluginRepo::new(&state.db);
    let plugin = resolve_plugin(&repo, &plugin_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Plugin not found".into()))?;
    let new_version = body.get("version").and_then(|v| v.as_str()).unwrap_or("latest");
    sqlx::query(
        "UPDATE plugins SET manifest = manifest || jsonb_build_object('pendingVersion', $2::text),                 status = 'upgrade_pending', updated_at = now()          WHERE id = $1",
    )
    .bind(plugin.id)
    .bind(new_version)
    .execute(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "id": plugin.id,
            "version": new_version,
            "status": "upgrade-queued"
        })),
    ))
}

async fn plugin_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PluginConfigQuery>,
    Path(plugin_id): Path<String>,
) -> ApiResult<Json<Option<PluginConfigRow>>> {
    require_authenticated(&state, &headers).await?;
    let company_id = query
        .company_id
        .ok_or_else(|| ApiError::BadRequest("companyId is required".into()))?;
    let repo = PluginRepo::new(&state.db);
    let plugin = resolve_plugin(&repo, &plugin_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Plugin not found".into()))?;
    Ok(Json(repo.get_config(plugin.id, company_id).await?))
}

async fn save_plugin_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(plugin_id): Path<String>,
    Json(body): Json<PluginConfigBody>,
) -> ApiResult<Json<PluginConfigRow>> {
    require_instance_admin(&state, &headers).await?;
    let company_id = body
        .company_id
        .ok_or_else(|| ApiError::BadRequest("companyId is required".into()))?;
    let config_json = body
        .config_json
        .ok_or_else(|| ApiError::BadRequest("configJson is required".into()))?;
    if !config_json.is_object() {
        return Err(ApiError::BadRequest("configJson must be an object".into()));
    }
    let repo = PluginRepo::new(&state.db);
    let plugin = resolve_plugin(&repo, &plugin_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Plugin not found".into()))?;
    Ok(Json(
        repo.upsert_config(plugin.id, company_id, &config_json)
            .await?,
    ))
}

async fn test_plugin_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(plugin_id): Path<String>,
    Json(body): Json<Value>,
) -> impl axum::response::IntoResponse {
    let _ = headers;
    let worker = match resolve_worker_or_501(&state, &plugin_id).await {
        Ok(w) => w,
        Err(resp) => return resp.into_response(),
    };
    let config = body.get("config").cloned().unwrap_or(Value::Null);
    match worker.validate_config(config).await {
        Ok(outcome) => (StatusCode::OK, Json(outcome)).into_response(),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": "worker validate_config failed", "detail": e.to_string()})),
        )
            .into_response(),
    }
}

async fn plugin_jobs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(plugin_id): Path<String>,
) -> ApiResult<Json<Vec<PluginJobRow>>> {
    require_authenticated(&state, &headers).await?;
    let repo = PluginRepo::new(&state.db);
    let plugin = resolve_plugin(&repo, &plugin_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Plugin not found".into()))?;
    Ok(Json(repo.list_jobs(plugin.id).await?))
}

#[derive(Debug, Deserialize, Default)]
struct JobRunQuery {
    limit: Option<i64>,
}

async fn plugin_job_runs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((plugin_id, job_id)): Path<(String, Uuid)>,
    Query(query): Query<JobRunQuery>,
) -> ApiResult<Json<Vec<PluginJobRunRow>>> {
    require_authenticated(&state, &headers).await?;
    let repo = PluginRepo::new(&state.db);
    let plugin = resolve_plugin(&repo, &plugin_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Plugin not found".into()))?;
    if repo.get_job(plugin.id, job_id).await?.is_none() {
        return Err(ApiError::NotFound("Plugin job not found".into()));
    }
    Ok(Json(
        repo.list_job_runs(plugin.id, job_id, query.limit.unwrap_or(25))
            .await?,
    ))
}

async fn trigger_plugin_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((plugin_id, job_id)): Path<(String, Uuid)>,
    Json(body): Json<Value>,
) -> impl axum::response::IntoResponse {
    let _ = headers;
    let worker = match resolve_worker_or_501(&state, &plugin_id).await {
        Ok(w) => w,
        Err(resp) => return resp.into_response(),
    };
    // body should be { "jobKey": "...", "context": { ... } } per protocol.
    let job_key = match body.get("jobKey").and_then(Value::as_str) {
        Some(k) => k.to_string(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "missing or non-string field: jobKey"})),
            )
                .into_response();
        }
    };
    let context = body.get("context").cloned().unwrap_or(Value::Null);
    let params = RunJobParams {
        job_key,
        run_id: job_id,
        context: serde_json::from_value(context).unwrap_or_default(),
    };
    match worker.run_job(params).await {
        Ok(()) => (
            StatusCode::ACCEPTED,
            Json(json!({"runId": job_id, "accepted": true})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": "worker run_job failed", "detail": e.to_string()})),
        )
            .into_response(),
    }
}

async fn receive_plugin_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((plugin_id, endpoint_key)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult<impl axum::response::IntoResponse> {
    let plugin_uuid = Uuid::parse_str(&plugin_id)
        .map_err(|_| ApiError::BadRequest("invalid plugin id".into()))?;
    let expected = std::env::var("PAPERCLIP_PLUGIN_WEBHOOK_SECRET").ok();
    if let Some(expected_secret) = expected {
        let provided = headers
            .get("x-paperclip-webhook-secret")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if provided != expected_secret {
            return Err(ApiError::Unauthorized("invalid webhook secret".into()));
        }
    }
    let delivery_id: Uuid = sqlx::query_scalar(
        "INSERT INTO plugin_webhook_deliveries (plugin_id, endpoint_key, payload, status, received_at)          VALUES ($1, $2, $3, 'queued', now()) RETURNING id",
    )
    .bind(plugin_uuid)
    .bind(&endpoint_key)
    .bind(&body)
    .fetch_one(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    state.realtime.publish(
        LiveEvent::new("plugin.webhook.received", "plugin", plugin_uuid)
            .with_data(json!({
                "deliveryId": delivery_id,
                "endpointKey": endpoint_key,
            })),
    );
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "deliveryId": delivery_id,
            "status": "queued"
        })),
    ))
}

async fn plugin_dashboard(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(plugin_id): Path<String>,
) -> ApiResult<Json<Value>> {
    require_authenticated(&state, &headers).await?;
    let repo = PluginRepo::new(&state.db);
    let plugin = resolve_plugin(&repo, &plugin_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Plugin not found".into()))?;
    let recent_runs = recent_job_runs(&repo, &plugin).await?;
    let recent_webhooks = repo
        .list_webhook_deliveries(plugin.id, 10)
        .await?
        .iter()
        .map(webhook_dashboard_json)
        .collect::<Vec<_>>();
    Ok(Json(json!({
        "pluginId": plugin.id,
        "worker": null,
        "recentJobRuns": recent_runs,
        "recentWebhookDeliveries": recent_webhooks,
        "health": health_result(&plugin),
        "checkedAt": Utc::now()
    })))
}

async fn recent_job_runs(repo: &PluginRepo<'_>, plugin: &PluginRow) -> ApiResult<Vec<Value>> {
    let jobs = repo.list_jobs(plugin.id).await?;
    let mut runs = Vec::new();
    for job in jobs {
        let job_runs = repo.list_job_runs(plugin.id, job.id, 10).await?;
        runs.extend(job_runs.into_iter().map(|run| {
            json!({
                "id": run.id,
                "jobId": run.job_id,
                "jobKey": job.job_key,
                "trigger": run.trigger,
                "status": run.status,
                "durationMs": run.duration_ms,
                "error": run.error,
                "startedAt": run.started_at,
                "finishedAt": run.finished_at,
                "createdAt": run.created_at
            })
        }));
    }
    runs.sort_by(|left, right| {
        right
            .get("createdAt")
            .and_then(Value::as_str)
            .cmp(&left.get("createdAt").and_then(Value::as_str))
    });
    runs.truncate(10);
    Ok(runs)
}

fn plugin_detail(plugin: PluginRow) -> Value {
    let mut result = serde_json::to_value(plugin).unwrap_or_else(|_| json!({}));
    if let Some(object) = result.as_object_mut() {
        object.insert("supportsConfigTest".into(), Value::Bool(false));
    }
    result
}

fn health_result(plugin: &PluginRow) -> Value {
    let has_valid_manifest = plugin
        .manifest_json
        .get("id")
        .and_then(Value::as_str)
        .is_some_and(|id| !id.is_empty());
    let is_ready = plugin.status == "ready";
    let has_no_error = plugin.last_error.is_none();
    let mut checks = vec![
        json!({
            "name": "registry",
            "passed": true,
            "message": "Plugin found in registry"
        }),
        json!({
            "name": "manifest",
            "passed": has_valid_manifest,
            "message": if has_valid_manifest { "Manifest is valid" } else { "Manifest is invalid or missing" }
        }),
        json!({
            "name": "status",
            "passed": is_ready,
            "message": format!("Current status: {}", plugin.status)
        }),
    ];
    if !has_no_error {
        checks.push(json!({
            "name": "error_state",
            "passed": false,
            "message": plugin.last_error
        }));
    }
    json!({
        "pluginId": plugin.id,
        "status": plugin.status,
        "healthy": has_valid_manifest && is_ready && has_no_error,
        "checks": checks,
        "lastError": plugin.last_error
    })
}

fn ui_contribution(plugin: &PluginRow) -> Option<Value> {
    let ui = plugin.manifest_json.get("ui");
    let slots = ui
        .and_then(|value| value.get("slots"))
        .cloned()
        .unwrap_or_else(|| json!([]));
    let mut launchers = plugin
        .manifest_json
        .get("launchers")
        .cloned()
        .unwrap_or_else(|| json!([]));
    if let (Some(existing), Some(extra)) = (
        launchers.as_array_mut(),
        ui.and_then(|value| value.get("launchers"))
            .and_then(Value::as_array),
    ) {
        existing.extend(extra.iter().cloned());
    }
    let has_slots = slots.as_array().is_some_and(|items| !items.is_empty());
    let has_launchers = launchers.as_array().is_some_and(|items| !items.is_empty());
    if !has_slots && !has_launchers {
        return None;
    }
    let display_name = plugin
        .manifest_json
        .get("displayName")
        .and_then(Value::as_str)
        .unwrap_or(&plugin.plugin_key);
    Some(json!({
        "pluginId": plugin.id,
        "pluginKey": plugin.plugin_key,
        "displayName": display_name,
        "version": plugin.version,
        "updatedAt": plugin.updated_at,
        "uiEntryFile": "index.js",
        "slots": slots,
        "launchers": launchers
    }))
}

fn webhook_dashboard_json(row: &PluginWebhookDeliveryRow) -> Value {
    json!({
        "id": row.id,
        "webhookKey": row.webhook_key,
        "status": row.status,
        "durationMs": row.duration_ms,
        "error": row.error,
        "startedAt": row.started_at,
        "finishedAt": row.finished_at,
        "createdAt": row.created_at
    })
}

async fn discover_bundled_plugins() -> Vec<BundledPlugin> {
    let Some(root) = bundled_plugin_root() else {
        return Vec::new();
    };
    let mut package_files = Vec::new();
    collect_package_files(&root, &mut package_files).await;
    let mut plugins = Vec::new();
    for package_file in package_files {
        let Ok(contents) = fs::read_to_string(&package_file).await else {
            continue;
        };
        let Ok(package) = serde_json::from_str::<Value>(&contents) else {
            continue;
        };
        if package.get("paperclipPlugin").is_none() {
            continue;
        }
        let Some(package_name) = package.get("name").and_then(Value::as_str) else {
            continue;
        };
        let package_root = package_file.parent().unwrap_or(&root);
        let plugin_key = package
            .get("paperclipPlugin")
            .and_then(|value| value.get("pluginKey"))
            .and_then(Value::as_str)
            .unwrap_or(package_name);
        let display_name = package
            .get("displayName")
            .and_then(Value::as_str)
            .unwrap_or(package_name);
        let has_built_entrypoints = package
            .get("paperclipPlugin")
            .and_then(|value| value.get("manifest"))
            .and_then(Value::as_str)
            .is_some_and(|manifest| package_root.join(manifest).exists());
        let package_root_string = package_root.to_string_lossy().into_owned();
        plugins.push(BundledPlugin {
            package_name: package_name.to_owned(),
            plugin_key: plugin_key.to_owned(),
            display_name: display_name.to_owned(),
            description: package
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("Bundled Paperclip plugin")
                .to_owned(),
            local_path: package_root_string.clone(),
            tag: if package_root_string.contains("/examples/") {
                "example".into()
            } else {
                "first-party".into()
            },
            experimental: package_root_string.contains("sandbox-providers")
                || package_name.contains("sandbox"),
            has_built_entrypoints,
        });
    }
    plugins.sort_by(|left, right| left.display_name.cmp(&right.display_name));
    plugins
}

fn bundled_plugin_root() -> Option<std::path::PathBuf> {
    if let Ok(root) = std::env::var("PAPERCLIP_PLUGIN_ROOT") {
        let path = std::path::PathBuf::from(root);
        if path.is_dir() {
            return Some(path);
        }
    }
    let candidates = [
        std::env::current_dir()
            .ok()?
            .join("../paperclip/packages/plugins"),
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../paperclip/packages/plugins"),
    ];
    candidates.into_iter().find(|path| path.is_dir())
}

async fn collect_package_files(directory: &std::path::Path, output: &mut Vec<std::path::PathBuf>) {
    let mut pending = vec![(directory.to_path_buf(), 0_usize)];
    while let Some((current, depth)) = pending.pop() {
        if depth > 5 {
            continue;
        }
        let Ok(mut entries) = fs::read_dir(&current).await else {
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path
                .file_name()
                .is_some_and(|name| name == "node_modules" || name == "dist")
            {
                continue;
            }
            if path.file_name().is_some_and(|name| name == "package.json") {
                output.push(path);
            } else if path.is_dir() {
                pending.push((path, depth + 1));
            }
        }
    }
}
