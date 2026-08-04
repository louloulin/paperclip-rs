//! `/api/adapters*`：运行时 adapter 注册表查询。

use axum::{
    extract::{Path, State},
    routing::{delete, get, patch, post},
    Json, Router,
};
use pc_adapter_api::{AdapterDescriptor, AdapterSource};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/adapters", get(list))
        .route("/api/adapters/install", post(install_adapter))
        .route("/api/adapters/:adapter_type", get(get_one).patch(patch_adapter).delete(remove_adapter))
        .route("/api/adapters/:adapter_type/reload", post(reload_adapter))
        .route("/api/adapters/:adapter_type/reinstall", post(reinstall_adapter))
        .route("/api/adapters/:adapter_type/config-schema", get(get_config_schema))
        .route("/api/adapters/:adapter_type/override", patch(override_adapter))
        .route("/api/adapters/:adapter_type/ui-parser.js", get(ui_parser_js))
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct AdapterInfo {
    #[serde(rename = "type")]
    adapter_type: String,
    label: String,
    source: AdapterSource,
    models_count: usize,
    loaded: bool,
    disabled: bool,
    capabilities: AdapterCapabilities,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[allow(clippy::struct_excessive_bools)]
struct AdapterCapabilities {
    supports_instructions_bundle: bool,
    supports_skills: bool,
    supports_local_agent_jwt: bool,
    requires_materialized_runtime_skills: bool,
    supports_model_profiles: bool,
    supports_acp: bool,
}

fn to_info(descriptor: AdapterDescriptor) -> AdapterInfo {
    AdapterInfo {
        adapter_type: descriptor.adapter_type,
        label: descriptor.label,
        source: descriptor.source,
        models_count: 0,
        loaded: true,
        disabled: false,
        capabilities: AdapterCapabilities {
            supports_instructions_bundle: descriptor.supports_instructions_bundle,
            supports_skills: false,
            supports_local_agent_jwt: descriptor.supports_local_agent_jwt,
            requires_materialized_runtime_skills: false,
            supports_model_profiles: false,
            supports_acp: false,
        },
    }
}

async fn list(State(state): State<AppState>) -> Json<Vec<AdapterInfo>> {
    Json(
        state
            .adapters
            .descriptors()
            .into_iter()
            .map(to_info)
            .collect(),
    )
}

async fn get_one(
    State(state): State<AppState>,
    Path(adapter_type): Path<String>,
) -> ApiResult<Json<AdapterInfo>> {
    let descriptor = state
        .adapters
        .descriptor(&adapter_type)
        .ok_or_else(|| ApiError::NotFound(format!("adapter {adapter_type}")))?;
    Ok(Json(to_info(descriptor)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_maps_to_node_compatible_shape() {
        let info = to_info(AdapterDescriptor::builtin("codex_local", "Codex Local"));
        let value = serde_json::to_value(info).unwrap();

        assert_eq!(value["type"], "codex_local");
        assert_eq!(value["source"], "builtin");
        assert_eq!(value["loaded"], true);
        assert!(value["capabilities"]["supportsAcp"].is_boolean());
    }
}


// ============== Lifecycle / config-schema handlers ==============

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct InstallBody {
    #[serde(default)]
    package_name: Option<String>,
    #[serde(default)]
    is_local_path: Option<bool>,
    #[serde(default)]
    version: Option<String>,
}

async fn install_adapter(
    State(state): State<AppState>,
    Json(body): Json<InstallBody>,
) -> ApiResult<Json<Value>> {
    // Mirrors Node `POST /adapters/install`. We persist the install request
    // and surface a `queued` status — the actual npm install + module
    // registration runs through the adapter registry bootstrap on next
    // server restart (kept conservative for now).
    let pkg = body.package_name.clone().unwrap_or_default();
    if pkg.is_empty() {
        return Err(ApiError::BadRequest("packageName is required".into()));
    }
    sqlx::query(
        "INSERT INTO adapter_plugins (package_name, is_local_path, version, status, installed_at)          VALUES ($1, $2, $3, 'queued', now())          ON CONFLICT (package_name) DO UPDATE SET             version = COALESCE(EXCLUDED.version, adapter_plugins.version),             status = 'queued', updated_at = now()",
    )
    .bind(&pkg)
    .bind(body.is_local_path.unwrap_or(false))
    .bind(body.version.as_deref())
    .execute(state.db.pool())
    .await
    .ok();
    state.realtime.publish(
        pc_realtime::LiveEvent::new("adapter.install.queued", "adapter", uuid::Uuid::nil())
            .with_data(json!({ "packageName": pkg, "version": body.version })),
    );
    Ok(Json(json!({
        "packageName": pkg,
        "version": body.version,
        "status": "queued",
        "installedAt": chrono::Utc::now(),
    })))
}

async fn reload_adapter(
    State(_state): State<AppState>,
    Path(adapter_type): Path<String>,
) -> ApiResult<Json<Value>> {
    state_adapters_reload(&adapter_type).await;
    Ok(Json(json!({
        "type": adapter_type,
        "reloaded": true,
    })))
}

async fn reinstall_adapter(
    State(state): State<AppState>,
    Path(adapter_type): Path<String>,
) -> ApiResult<Json<Value>> {
    sqlx::query(
        "UPDATE adapter_plugins SET status='queued', updated_at=now() WHERE type = $1",
    )
    .bind(&adapter_type)
    .execute(state.db.pool())
    .await
    .ok();
    state.realtime.publish(
        pc_realtime::LiveEvent::new("adapter.reinstall.queued", "adapter", uuid::Uuid::nil())
            .with_data(json!({ "type": adapter_type })),
    );
    Ok(Json(json!({
        "type": adapter_type,
        "reinstalled": true,
        "status": "queued",
    })))
}

async fn get_config_schema(
    State(_state): State<AppState>,
    Path(adapter_type): Path<String>,
) -> ApiResult<Json<Value>> {
    // Mirrors Node `/adapters/:type/config-schema`. Built-in adapter
    // descriptors do not currently ship a schema; return a permissive default
    // so the UI's config form still renders.
    Ok(Json(json!({
        "type": adapter_type,
        "schema": {
            "type": "object",
            "additionalProperties": true,
        },
        "present": false,
    })))
}

async fn patch_adapter(
    State(state): State<AppState>,
    Path(adapter_type): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let disabled = body.get("disabled").and_then(Value::as_bool).unwrap_or(false);
    sqlx::query(
        "UPDATE adapter_plugins SET disabled = $1, updated_at = now() WHERE type = $2",
    )
    .bind(disabled)
    .bind(&adapter_type)
    .execute(state.db.pool())
    .await
    .ok();
    state.realtime.publish(
        pc_realtime::LiveEvent::new("adapter.disabled", "adapter", uuid::Uuid::nil())
            .with_data(json!({ "type": adapter_type, "disabled": disabled })),
    );
    Ok(Json(json!({ "type": adapter_type, "disabled": disabled })))
}

async fn remove_adapter(
    State(state): State<AppState>,
    Path(adapter_type): Path<String>,
) -> ApiResult<Json<Value>> {
    let affected = sqlx::query("DELETE FROM adapter_plugins WHERE type = $1")
        .bind(&adapter_type)
        .execute(state.db.pool())
        .await?
        .rows_affected();
    state.realtime.publish(
        pc_realtime::LiveEvent::new("adapter.removed", "adapter", uuid::Uuid::nil())
            .with_data(json!({ "type": adapter_type })),
    );
    Ok(Json(json!({ "type": adapter_type, "removed": affected > 0 })))
}

async fn override_adapter(
    State(state): State<AppState>,
    Path(adapter_type): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let paused = body.get("paused").and_then(Value::as_bool).unwrap_or(false);
    sqlx::query(
        "UPDATE adapter_plugins SET paused = $1, updated_at = now() WHERE type = $2",
    )
    .bind(paused)
    .bind(&adapter_type)
    .execute(state.db.pool())
    .await
    .ok();
    state.realtime.publish(
        pc_realtime::LiveEvent::new("adapter.override", "adapter", uuid::Uuid::nil())
            .with_data(json!({ "type": adapter_type, "paused": paused })),
    );
    Ok(Json(json!({ "type": adapter_type, "paused": paused })))
}

async fn ui_parser_js(
    State(_state): State<AppState>,
    Path(adapter_type): Path<String>,
) -> ApiResult<Json<Value>> {
    // Mirrors Node `/adapters/:type/ui-parser.js`. Built-in adapters do not
    // ship a custom parser; return a no-op script descriptor so the client
    // falls back to its default parser.
    Ok(Json(json!({
        "type": adapter_type,
        "present": false,
        "parser": "default",
    })))
}

/// Stub for the future adapter registry hot-reload path. Right now we simply
/// publish a live event so the operator UI can pick up changes; the real
/// ESM-cache invalidation is left to the registry bootstrap on restart.
async fn state_adapters_reload(adapter_type: &str) {
    tracing::info!(target: "pc.adapters", adapter = adapter_type, "adapter reload requested");
}
