//! 插件管理 HTTP API。
//!
//! 数据库资源由 `PluginRepo` 统一访问；需要 Node worker 等运行时能力的
//! endpoint 在能力未注册时返回明确的 501，而不是伪造成功响应。

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post, put},
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::fs;
use uuid::Uuid;

use pc_plugin_host::{
    notifications::{StreamBridgeEvent, SubscriptionKey},
    WorkerHandle,
};
use pc_plugin_protocol::{
    ExecuteToolParams, PaperclipPluginManifestV1, PluginLocalFolderAccess, PluginLocalFolderDeclaration,
    RunJobParams,
};
use pc_plugin_protocol::{GetDataParams, PerformActionParams};
use pc_realtime::LiveEvent;
use pc_repos::instance_user_role::InstanceUserRoleRepo;
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
        // ── Round 46: plugin local folders endpoints ──
        .route(
            "/api/plugins/:plugin_id/companies/:company_id/local-folders",
            get(plugin_local_folders_list),
        )
        .route(
            "/api/plugins/:plugin_id/companies/:company_id/local-folders/:folder_key/status",
            get(plugin_local_folder_status),
        )
        .route(
            "/api/plugins/:plugin_id/companies/:company_id/local-folders/:folder_key/validate",
            post(plugin_local_folder_validate),
        )
        .route(
            "/api/plugins/:plugin_id/companies/:company_id/local-folders/:folder_key",
            put(plugin_local_folder_save),
        )
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
    let is_admin = InstanceUserRoleRepo::new(&state.db)
        .is_admin(&user_id)
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
) -> ApiResult<Json<Value>> {
    // Aggregate tools from the static manifest catalog for every ready plugin.
    // Dynamic tool listings from the worker would require a `listTools` RPC,
    // which is not yet part of the protocol. The manifest-level catalog is
    // good enough for the UI to render pre-built tool palettes.
    let repo = PluginRepo::new(&state.db);
    let plugins = repo.list_filtered(Some("ready")).await?;
    let mut items: Vec<Value> = Vec::new();
    for plugin in plugins {
        let manifest_json = &plugin.manifest_json;
        if let Some(arr) = manifest_json.get("tools").and_then(|t| t.as_array()) {
            for tool in arr {
                items.push(json!({
                    "pluginId": plugin.id,
                    "pluginKey": plugin.plugin_key,
                    "tool": tool,
                }));
            }
        }
    }
    Ok(Json(json!({ "items": items })))
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
        Err(resp) => return Ok(resp.into_response()),
    };
    let Some(tool) = body.tool else {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "missing required field: tool"})),
        )
            .into_response());
    };
    let params = ExecuteToolParams {
        tool,
        args: body.parameters.unwrap_or(Value::Null),
        context: body.run_context.unwrap_or(Value::Null),
    };
    match worker.execute_tool(params).await {
        Ok(result) => Ok((StatusCode::OK, Json(json!({"result": result}))).into_response()),
        Err(e) => Err(ApiError::Internal(format!("worker rejected request: {e}"))),
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

// bridge_data: 优先转发到 plugin worker 的 getData RPC；若 worker 未运行，回落到 plugin_entities。
async fn bridge_data(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((plugin_id,)): Path<(String,)>,
    Json(body): Json<Value>,
) -> ApiResult<axum::response::Response> {
    require_authenticated(&state, &headers).await?;
    let pid = match Uuid::parse_str(&plugin_id) {
        Ok(id) => id,
        Err(_) => return Err(ApiError::BadRequest("invalid plugin id".into())),
    };
    let key = body.get("key").and_then(Value::as_str).map(String::from);
    let company_id = body
        .get("companyId")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
        .unwrap_or_else(Uuid::nil);
    if let (Some(key), Ok(worker)) = (
        key.as_deref(),
        resolve_worker_or_501(&state, &plugin_id).await,
    ) {
        let params = GetDataParams {
            key: key.to_string(),
            params: body.get("params").cloned().unwrap_or(Value::Null),
            company_id,
        };
        match worker.get_data(params).await {
            Ok(value) => {
                return Ok((
                    StatusCode::OK,
                    Json(json!({ "data": value, "source": "worker" })),
                )
                    .into_response())
            }
            Err(e) => return Err(ApiError::Internal(format!("worker rejected getData: {e}"))),
        }
    }
    // Fallback: 持久化到 plugin_entities 供离线检索。
    let entity_type = body
        .get("entityType")
        .and_then(Value::as_str)
        .unwrap_or("bridge_data");
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
    let cid = body
        .get("companyId")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok());
    let data = body.get("data").cloned().unwrap_or(json!({}));
    let result: Result<Uuid, _> = PluginRepo::new(&state.db)
        .upsert_entity(
            pid, entity_type, scope_kind, scope_id.as_deref(), external_id.as_deref(), title.as_deref(), &data, cid,
        )
        .await;
    match result {
        Ok(id) => Ok((
            StatusCode::OK,
            Json(json!({ "id": id, "ok": true, "source": "store" })),
        )
            .into_response()),
        Err(e) => Err(ApiError::Internal(e.to_string())),
    }
}

// bridge_action: 优先转发到 plugin worker 的 performAction RPC；否则写日志。
// bridge_action: 优先转发到 plugin worker 的 performAction RPC；否则写日志。
async fn bridge_action(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((plugin_id,)): Path<(String,)>,
    Json(body): Json<Value>,
) -> ApiResult<axum::response::Response> {
    require_authenticated(&state, &headers).await?;
    let pid = match Uuid::parse_str(&plugin_id) {
        Ok(id) => id,
        Err(_) => return Err(ApiError::BadRequest("invalid plugin id".into())),
    };
    let company_id = body
        .get("companyId")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
        .unwrap_or_else(Uuid::nil);
    if let Ok(worker) = resolve_worker_or_501(&state, &plugin_id).await {
        let action = body
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("default")
            .to_string();
        let params = PerformActionParams {
            action,
            params: body.get("params").cloned().unwrap_or(Value::Null),
            company_id,
        };
        match worker.perform_action(params).await {
            Ok(value) => {
                return Ok((
                    StatusCode::OK,
                    Json(json!({ "data": value, "source": "worker" })),
                )
                    .into_response())
            }
            Err(e) => {
                return Err(ApiError::Internal(format!(
                    "worker rejected performAction: {e}"
                )))
            }
        }
    }
    // Fallback: 记录日志。
    let level = body.get("level").and_then(Value::as_str).unwrap_or("info");
    let message = body
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("(no message)")
        .to_string();
    let meta = body.get("data").cloned().unwrap_or(json!({}));
    let result: Result<Uuid, _> = PluginRepo::new(&state.db)
        .create_log(pid, level, &message, &meta)
        .await;
    match result {
        Ok(id) => {
            state.realtime.publish(
                LiveEvent::new("plugin.action.logged", "plugin", pid)
                    .with_data(json!({ "logId": id, "level": level })),
            );
            Ok((
                StatusCode::OK,
                Json(json!({ "id": id, "ok": true, "source": "log" })),
            )
                .into_response())
        }
        Err(e) => Err(ApiError::Internal(e.to_string())),
    }
}

// plugin_data: 优先 worker getData(URL-keyed)；否则按 (plugin_id, entity_type) 查 plugin_entities。
async fn plugin_data(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((plugin_id, key)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult<axum::response::Response> {
    require_authenticated(&state, &headers).await?;
    let pid = match Uuid::parse_str(&plugin_id) {
        Ok(id) => id,
        Err(_) => return Err(ApiError::BadRequest("invalid plugin id".into())),
    };
    let company_id = body
        .get("companyId")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
        .unwrap_or_else(Uuid::nil);
    if let Ok(worker) = resolve_worker_or_501(&state, &plugin_id).await {
        let params = GetDataParams {
            key: key.clone(),
            params: body.get("params").cloned().unwrap_or(Value::Null),
            company_id,
        };
        match worker.get_data(params).await {
            Ok(value) => {
                return Ok((
                    StatusCode::OK,
                    Json(json!({ "data": value, "source": "worker" })),
                )
                    .into_response())
            }
            Err(e) => return Err(ApiError::Internal(format!("worker rejected getData: {e}"))),
        }
    }
    let external_id = body
        .get("externalId")
        .and_then(Value::as_str)
        .unwrap_or(&key);
    let cid = body
        .get("companyId")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok());
    let row: Result<Option<(Uuid, Value)>, _> = PluginRepo::new(&state.db)
        .find_entity(pid, &key, Some(external_id), cid)
        .await;
    match row {
        Ok(Some((id, data))) => Ok((
            StatusCode::OK,
            Json(json!({ "id": id, "found": true, "data": data, "source": "store" })),
        )
            .into_response()),
        Ok(None) => Ok((StatusCode::OK, Json(json!({ "found": false }))).into_response()),
        Err(e) => Err(ApiError::Internal(e.to_string())),
    }
}

// plugin_action: 优先 worker performAction(URL-keyed)；否则在 plugin_jobs 排程。
async fn plugin_action(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((plugin_id, key)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult<axum::response::Response> {
    require_authenticated(&state, &headers).await?;
    let pid = match Uuid::parse_str(&plugin_id) {
        Ok(id) => id,
        Err(_) => return Err(ApiError::BadRequest("invalid plugin id".into())),
    };
    let company_id = body
        .get("companyId")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
        .unwrap_or_else(Uuid::nil);
    if let Ok(worker) = resolve_worker_or_501(&state, &plugin_id).await {
        let params = PerformActionParams {
            action: key.clone(),
            params: body.get("params").cloned().unwrap_or(Value::Null),
            company_id,
        };
        match worker.perform_action(params).await {
            Ok(value) => {
                return Ok((
                    StatusCode::OK,
                    Json(json!({ "data": value, "source": "worker" })),
                )
                    .into_response())
            }
            Err(e) => {
                return Err(ApiError::Internal(format!(
                    "worker rejected performAction: {e}"
                )))
            }
        }
    }
    let schedule = body
        .get("schedule")
        .and_then(Value::as_str)
        .unwrap_or("on_demand");
    let result: Result<Uuid, _> = PluginRepo::new(&state.db)
        .upsert_job(pid, &key, schedule)
        .await;
    match result {
        Ok(id) => {
            state.realtime.publish(
                LiveEvent::new("plugin.action.queued", "plugin", pid)
                    .with_data(json!({ "jobId": id, "jobKey": key })),
            );
            Ok((
                StatusCode::OK,
                Json(json!({ "id": id, "ok": true, "status": "queued", "source": "store" })),
            )
                .into_response())
        }
        Err(e) => Err(ApiError::Internal(e.to_string())),
    }
}

// bridge_stream: 订阅 NotificationBus 并将 stream 事件以 SSE 推送给 UI。
async fn bridge_stream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((plugin_id, channel)): Path<(String, String)>,
    Query(query): Query<BridgeStreamQuery>,
) -> axum::response::Response {
    let _ = headers;
    let pid = match Uuid::parse_str(&plugin_id) {
        Ok(id) => id,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "invalid plugin id"})),
            )
                .into_response()
        }
    };
    let company_id = query.company_id.unwrap_or_else(Uuid::nil);
    let bus = state.plugin_bus.clone();
    let key = SubscriptionKey {
        plugin_id: pid,
        channel: channel.clone(),
        company_id,
    };
    let (guard, mut rx) = bus.subscribe_stream(key.clone());
    // keep the guard alive for the duration of the stream
    let stream = async_stream::stream! {
        let _guard = guard;
        yield Ok::<_, std::convert::Infallible>(axum::response::sse::Event::default().data(
            serde_json::to_string(&json!({
                "channel": channel,
                "pluginId": pid.to_string(),
                "companyId": company_id.to_string(),
                "type": "subscribed",
            })).unwrap_or_default()
        ));
        loop {
            match rx.recv().await {
                Ok(StreamBridgeEvent { event, event_type }) => {
                    if event_type != "message" {
                        yield Ok::<_, std::convert::Infallible>(axum::response::sse::Event::default().event(event_type).data(
                            serde_json::to_string(&event).unwrap_or_default()
                        ));
                    } else {
                        yield Ok::<_, std::convert::Infallible>(axum::response::sse::Event::default().data(
                            serde_json::to_string(&event).unwrap_or_default()
                        ));
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    yield Ok::<_, std::convert::Infallible>(axum::response::sse::Event::default().event("lagged").data(n.to_string()));
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    };
    axum::response::Sse::new(stream).into_response()
}

#[derive(Debug, Deserialize, Default)]
struct BridgeStreamQuery {
    #[serde(rename = "companyId")]
    company_id: Option<Uuid>,
}

fn worker_not_running(plugin_id_str: &str) -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_GATEWAY,
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
/// returns `Err((StatusCode::BAD_GATEWAY, Json))` with a message that the
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
    let new_version = body
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("latest");
    PluginRepo::new(&state.db)
        .set_pending_upgrade(plugin.id, new_version)
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
        Err(resp) => return Ok(resp.into_response()),
    };
    let config = body.get("config").cloned().unwrap_or(Value::Null);
    match worker.validate_config(config).await {
        Ok(outcome) => Ok((StatusCode::OK, Json(outcome)).into_response()),
        Err(e) => Err(ApiError::Internal(format!(
            "worker validate_config failed: {e}"
        ))),
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
) -> ApiResult<axum::response::Response> {
    let _ = headers;
    let worker = match resolve_worker_or_501(&state, &plugin_id).await {
        Ok(w) => w,
        Err(resp) => return Ok(resp.into_response()),
    };
    // body should be { "jobKey": "...", "context": { ... } } per protocol.
    let job_key = match body.get("jobKey").and_then(Value::as_str) {
        Some(k) => k.to_string(),
        None => {
            return Ok((
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "missing or non-string field: jobKey"})),
            )
                .into_response());
        }
    };
    let context = body.get("context").cloned().unwrap_or(Value::Null);
    let params = RunJobParams {
        job_key,
        run_id: job_id,
        context: serde_json::from_value(context).unwrap_or_default(),
    };
    match worker.run_job(params).await {
        Ok(()) => Ok((
            StatusCode::ACCEPTED,
            Json(json!({"runId": job_id, "accepted": true})),
        )
            .into_response()),
        Err(e) => Err(ApiError::Internal(format!("worker run_job failed: {e}"))),
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
    let delivery_id = PluginRepo::new(&state.db)
        .create_webhook_delivery(plugin_uuid, &endpoint_key, &body)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    state.realtime.publish(
        LiveEvent::new("plugin.webhook.received", "plugin", plugin_uuid).with_data(json!({
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

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::worker_not_running;

    #[test]
    fn missing_worker_maps_to_bad_gateway() {
        let (status, body) = worker_not_running("plugin-1");

        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(body.0["error"], "plugin worker not running");
        assert_eq!(body.0["pluginId"], "plugin-1");
    }
}


// ============================================================================
// Round 46: Plugin local folders
// ============================================================================

#[derive(Debug, Default, Deserialize)]
struct LocalFolderValidateBody {
    path: Option<String>,
    #[serde(default)]
    access: Option<PluginLocalFolderAccess>,
    #[serde(default)]
    required_directories: Vec<String>,
    #[serde(default)]
    required_files: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct LocalFolderSaveBody {
    path: String,
    #[serde(default)]
    access: PluginLocalFolderAccess,
    #[serde(default)]
    required_directories: Vec<String>,
    #[serde(default)]
    required_files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LocalFolderStoredConfig {
    path: String,
    #[serde(default)]
    access: PluginLocalFolderAccess,
    #[serde(default)]
    required_directories: Vec<String>,
    #[serde(default)]
    required_files: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct LocalFolderProblem {
    code: String,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct LocalFolderStatus {
    folder_key: String,
    configured: bool,
    path: Option<String>,
    real_path: Option<String>,
    access: PluginLocalFolderAccess,
    readable: bool,
    writable: bool,
    required_directories: Vec<String>,
    required_files: Vec<String>,
    missing_directories: Vec<String>,
    missing_files: Vec<String>,
    healthy: bool,
    problems: Vec<LocalFolderProblem>,
    checked_at: chrono::DateTime<chrono::Utc>,
}

async fn get_plugin_manifest_from_db(state: &AppState, plugin_id: Uuid) -> ApiResult<Option<PaperclipPluginManifestV1>> {
    use pc_repos::plugin::PluginRepo;
    let row = PluginRepo::new(&state.db)
        .get_by_id(plugin_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(row.and_then(|r| serde_json::from_value(r.manifest_json).ok()))
}

fn get_stored_local_folders(settings_json: Option<&Value>) -> std::collections::HashMap<String, LocalFolderStoredConfig> {
    let mut out = std::collections::HashMap::new();
    let Some(v) = settings_json else { return out };
    let Some(map) = v.get("localFolders").and_then(|x| x.as_object()) else { return out };
    for (k, val) in map {
        if let Ok(cfg) = serde_json::from_value::<LocalFolderStoredConfig>(val.clone()) {
            out.insert(k.clone(), cfg);
        }
    }
    out
}

fn upsert_stored_local_folder(settings_json: Option<Value>, folder_key: &str, cfg: LocalFolderStoredConfig) -> Value {
    let mut v = settings_json.unwrap_or_else(|| serde_json::json!({}));
    if !v.is_object() {
        v = serde_json::json!({});
    }
    let obj = v.as_object_mut().unwrap();
    let lf = obj.entry("localFolders".to_string()).or_insert_with(|| serde_json::json!({}));
    if !lf.is_object() {
        *lf = serde_json::json!({});
    }
    let lf_obj = lf.as_object_mut().unwrap();
    lf_obj.insert(
        folder_key.to_string(),
        serde_json::to_value(&cfg).unwrap_or(serde_json::json!({})),
    );
    v
}

fn access_default(access: Option<PluginLocalFolderAccess>) -> PluginLocalFolderAccess {
    access.unwrap_or(PluginLocalFolderAccess::ReadWrite)
}

async fn inspect_local_folder(
    folder_key: &str,
    declaration: Option<&PluginLocalFolderDeclaration>,
    stored: Option<&LocalFolderStoredConfig>,
    override_cfg: Option<&LocalFolderStoredConfig>,
) -> LocalFolderStatus {
    let now = chrono::Utc::now();
    let access = override_cfg
        .map(|c| c.access.clone())
        .or_else(|| stored.map(|c| c.access.clone()))
        .or_else(|| declaration.and_then(|d| d.access.clone()))
        .unwrap_or(PluginLocalFolderAccess::ReadWrite);
    let required_directories = override_cfg
        .map(|c| c.required_directories.clone())
        .or_else(|| stored.map(|c| c.required_directories.clone()))
        .unwrap_or_default();
    let required_files = override_cfg
        .map(|c| c.required_files.clone())
        .or_else(|| stored.map(|c| c.required_files.clone()))
        .unwrap_or_default();
    let configured_path = override_cfg
        .map(|c| c.path.clone())
        .or_else(|| stored.map(|c| c.path.clone()));

    let Some(path) = configured_path else {
        return LocalFolderStatus {
            folder_key: folder_key.to_string(),
            configured: false,
            path: None,
            real_path: None,
            access,
            readable: false,
            writable: false,
            required_directories: required_directories.clone(),
            required_files: required_files.clone(),
            missing_directories: required_directories,
            missing_files: required_files,
            healthy: false,
            problems: vec![LocalFolderProblem {
                code: "not_configured".into(),
                message: "No local folder path is configured.".into(),
                detail: None,
            }],
            checked_at: now,
        };
    };

    let mut problems: Vec<LocalFolderProblem> = Vec::new();
    let mut missing_directories: Vec<String> = Vec::new();
    let mut missing_files: Vec<String> = Vec::new();
    let mut readable = false;
    let mut writable = false;
    let mut real_path: Option<String> = None;
    let mut configured = true;
    let mut healthy = true;

    if !std::path::Path::new(&path).is_absolute() {
        problems.push(LocalFolderProblem {
            code: "not_absolute".into(),
            message: "Local folder path must be absolute.".into(),
            detail: Some(path.clone()),
        });
        healthy = false;
    }

    match tokio::fs::metadata(&path).await {
        Ok(meta) if meta.is_dir() => {
            real_path = tokio::fs::canonicalize(&path)
                .await
                .ok()
                .map(|p| p.to_string_lossy().into_owned());
            match tokio::fs::try_exists(&path).await {
                Ok(true) => readable = true,
                _ => {}
            }
            if access == PluginLocalFolderAccess::ReadWrite {
                // Probe write access by attempting to create a temp file.
                let probe = format!("{}/.paperclip-write-probe", path.trim_end_matches('/'));
                if tokio::fs::write(&probe, b"ok").await.is_ok() {
                    writable = true;
                    let _ = tokio::fs::remove_file(&probe).await;
                }
            } else {
                writable = false;
            }
            for sub in &required_directories {
                let full = format!("{}/{}", path.trim_end_matches('/'), sub);
                if !tokio::fs::try_exists(&full).await.unwrap_or(false) {
                    missing_directories.push(sub.clone());
                }
            }
            for f in &required_files {
                let full = format!("{}/{}", path.trim_end_matches('/'), f);
                if !tokio::fs::try_exists(&full).await.unwrap_or(false) {
                    missing_files.push(f.clone());
                }
            }
            if !missing_directories.is_empty() || !missing_files.is_empty() {
                healthy = false;
            }
        }
        Ok(_) => {
            problems.push(LocalFolderProblem {
                code: "not_directory".into(),
                message: "Configured local folder path is not a directory.".into(),
                detail: Some(path.clone()),
            });
            healthy = false;
        }
        Err(_) => {
            configured = false;
            problems.push(LocalFolderProblem {
                code: "path_missing".into(),
                message: "Configured path does not exist.".into(),
                detail: Some(path.clone()),
            });
            healthy = false;
        }
    }

    LocalFolderStatus {
        folder_key: folder_key.to_string(),
        configured,
        path: Some(path),
        real_path,
        access,
        readable,
        writable,
        required_directories,
        required_files,
        missing_directories,
        missing_files,
        healthy,
        problems,
        checked_at: now,
    }
}

async fn plugin_local_folders_list(
    State(state): State<AppState>,
    Path((plugin_id, company_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    // Optional auth: list endpoints are informational; tolerate anonymous.
    let _ = crate::require_user_id(
        &state,
        &<axum::http::HeaderMap as std::default::Default>::default(),
    )
    .await;

    let manifest = get_plugin_manifest_from_db(&state, plugin_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("plugin not found".into()))?;
    let settings = pc_repos::plugin::PluginRepo::new(&state.db)
        .get_company_settings(plugin_id, company_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .map(|r| r.settings_json);
    let stored = get_stored_local_folders(settings.as_ref());

    let declarations = manifest.local_folders.clone();
    let mut statuses = Vec::with_capacity(declarations.len());
    for decl in &declarations {
        let status = inspect_local_folder(
            &decl.folder_key,
            Some(decl),
            stored.get(&decl.folder_key),
            None,
        )
        .await;
        statuses.push(serde_json::to_value(&status).unwrap_or(Value::Null));
    }
    Ok(Json(serde_json::json!({
        "pluginId": plugin_id,
        "companyId": company_id,
        "declarations": declarations,
        "folders": statuses,
    })))
}

async fn plugin_local_folder_status(
    State(state): State<AppState>,
    Path((plugin_id, company_id, folder_key)): Path<(Uuid, Uuid, String)>,
) -> ApiResult<Json<Value>> {
    let Some(manifest) = get_plugin_manifest_from_db(&state, plugin_id).await? else {
        return Err(ApiError::NotFound("plugin not found".into()));
    };
    let declaration = manifest.local_folders.iter().find(|d| d.folder_key == folder_key).cloned();
    if declaration.is_none() {
        return Err(ApiError::NotFound(format!("folder {folder_key} not declared by plugin")));
    }
    let settings = pc_repos::plugin::PluginRepo::new(&state.db)
        .get_company_settings(plugin_id, company_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .map(|r| r.settings_json);
    let stored = get_stored_local_folders(settings.as_ref())
        .remove(&folder_key);
    let status = inspect_local_folder(
        &folder_key,
        declaration.as_ref(),
        stored.as_ref(),
        None,
    )
    .await;
    Ok(Json(serde_json::to_value(&status).unwrap_or(Value::Null)))
}

async fn plugin_local_folder_validate(
    State(state): State<AppState>,
    Path((plugin_id, company_id, folder_key)): Path<(Uuid, Uuid, String)>,
    Json(body): Json<LocalFolderValidateBody>,
) -> ApiResult<Json<Value>> {
    let _ = (plugin_id, company_id);
    let Some(path) = body.path else {
        return Err(ApiError::BadRequest("\"path\" is required and must be a non-empty string".into()));
    };
    if path.trim().is_empty() {
        return Err(ApiError::BadRequest("\"path\" is required and must be a non-empty string".into()));
    }
    let override_cfg = LocalFolderStoredConfig {
        path: path.clone(),
        access: access_default(body.access.clone()),
        required_directories: body.required_directories.clone(),
        required_files: body.required_files.clone(),
    };
    let status = inspect_local_folder(&folder_key, None, None, Some(&override_cfg)).await;
    Ok(Json(serde_json::to_value(&status).unwrap_or(Value::Null)))
}

async fn plugin_local_folder_save(
    State(state): State<AppState>,
    Path((plugin_id, company_id, folder_key)): Path<(Uuid, Uuid, String)>,
    Json(body): Json<LocalFolderSaveBody>,
) -> ApiResult<Json<Value>> {
    if body.path.trim().is_empty() {
        return Err(ApiError::BadRequest("\"path\" is required and must be a non-empty string".into()));
    }
    let Some(manifest) = get_plugin_manifest_from_db(&state, plugin_id).await? else {
        return Err(ApiError::NotFound("plugin not found".into()));
    };
    let declaration = manifest
        .local_folders
        .iter()
        .find(|d| d.folder_key == folder_key)
        .cloned()
        .ok_or_else(|| ApiError::NotFound(format!("folder {folder_key} not declared by plugin")))?;

    let cfg = LocalFolderStoredConfig {
        path: body.path.clone(),
        access: body.access.clone(),
        required_directories: body.required_directories.clone(),
        required_files: body.required_files.clone(),
    };

    // Read-modify-write plugin_company_settings.settings_json
    let existing = pc_repos::plugin::PluginRepo::new(&state.db)
        .get_company_settings(plugin_id, company_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .map(|r| r.settings_json);
    let new_settings = upsert_stored_local_folder(existing, &folder_key, cfg.clone());
    pc_repos::plugin::PluginRepo::new(&state.db)
        .upsert_company_settings(plugin_id, company_id, true, &new_settings)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    state.realtime.publish(
        pc_realtime::LiveEvent::new(
            "plugin.local_folder.saved",
            "plugin",
            plugin_id,
        )
        .with_data(serde_json::json!({
            "pluginId": plugin_id,
            "companyId": company_id,
            "folderKey": folder_key,
        })),
    );

    let stored = Some(cfg.clone());
    let status = inspect_local_folder(
        &folder_key,
        Some(&declaration),
        stored.as_ref(),
        None,
    )
    .await;
    Ok(Json(serde_json::json!({
        "pluginId": plugin_id,
        "companyId": company_id,
        "folderKey": folder_key,
        "config": cfg,
        "status": serde_json::to_value(&status).unwrap_or(Value::Null),
    })))
}
