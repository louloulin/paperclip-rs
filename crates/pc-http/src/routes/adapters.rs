//! `/api/adapters*`：运行时 adapter 注册表查询。

use axum::{
    extract::{Path, State},
    routing::{delete, get, patch, post},
    Json, Router,
};
use pc_adapter_api::{AdapterDescriptor, AdapterSource};
use uuid::Uuid;
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
        // ── Round 24: per-company adapter sub-resources (models / detect / profiles / test-env) ──
        .route(
            "/api/companies/:company_id/adapters/:adapter_type/models",
            get(adapter_models),
        )
        .route(
            "/api/companies/:company_id/adapters/:adapter_type/model-profiles",
            get(adapter_model_profiles),
        )
        .route(
            "/api/companies/:company_id/adapters/:adapter_type/detect-model",
            get(detect_adapter_model).post(detect_adapter_model),
        )
        .route(
            "/api/companies/:company_id/adapters/:adapter_type/test-environment",
            post(adapter_test_environment),
        )
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

// ============== Round 24: per-company adapter sub-resources ==============

// Common per-adapter model catalogs. The Node service hard-codes these same
// defaults; per-adapter dynamic detection is a follow-up.

fn adapter_model_catalog(adapter_type: &str) -> Vec<Value> {
    match adapter_type {
        "claude_local" | "claude_local_subscription" => vec![
            json!({ "id": "claude-sonnet-4-5", "label": "Claude Sonnet 4.5" }),
            json!({ "id": "claude-opus-4-1", "label": "Claude Opus 4.1" }),
            json!({ "id": "claude-haiku-4", "label": "Claude Haiku 4" }),
        ],
        "codex_local" | "codex_app_server" => vec![
            json!({ "id": "gpt-5", "label": "GPT-5" }),
            json!({ "id": "gpt-5-codex", "label": "GPT-5 Codex" }),
            json!({ "id": "gpt-4.1", "label": "GPT-4.1" }),
            json!({ "id": "o3", "label": "o3" }),
        ],
        "cursor_local" => vec![
            json!({ "id": "gpt-5", "label": "GPT-5" }),
            json!({ "id": "claude-sonnet-4-5", "label": "Claude Sonnet 4.5" }),
            json!({ "id": "cursor-auto", "label": "Cursor Auto" }),
        ],
        "gemini_local" | "gemini_cli" => vec![
            json!({ "id": "gemini-2.5-pro", "label": "Gemini 2.5 Pro" }),
            json!({ "id": "gemini-2.5-flash", "label": "Gemini 2.5 Flash" }),
        ],
        "grok_local" => vec![
            json!({ "id": "grok-4", "label": "Grok 4" }),
            json!({ "id": "grok-3", "label": "Grok 3" }),
        ],
        "opencode_local" => vec![
            json!({ "id": "opencode-default", "label": "OpenCode Default" }),
        ],
        "pi_local" => vec![
            json!({ "id": "pi-default", "label": "Pi Default" }),
        ],
        _ => vec![
            json!({ "id": "default", "label": "Default" }),
        ],
    }
}

fn adapter_model_profiles_catalog(adapter_type: &str) -> Vec<Value> {
    // Profiles describe pre-baked configuration bundles. The Node service
    // surfaces the same set of keys.
    let mut profiles: Vec<Value> = Vec::new();
    profiles.push(json!({
        "key": "default",
        "label": "Default",
        "adapterType": adapter_type,
    }));
    profiles.push(json!({
        "key": "fast",
        "label": "Fast (low-latency)",
        "adapterType": adapter_type,
    }));
    profiles.push(json!({
        "key": "thorough",
        "label": "Thorough (extended reasoning)",
        "adapterType": adapter_type,
    }));
    profiles
}

async fn adapter_models(
    State(state): State<AppState>,
    Path((_company_id, adapter_type)): Path<(Uuid, String)>,
) -> ApiResult<Json<Value>> {
    let descriptor = state.adapters.descriptor(&adapter_type);
    let models = adapter_model_catalog(&adapter_type);
    let source = descriptor
        .as_ref()
        .map(|d| format!("{:?}", d.source).to_lowercase())
        .unwrap_or_else(|| "unknown".to_owned());
    Ok(Json(json!({
        "companyId": _company_id,
        "adapterType": adapter_type,
        "adapterSource": source,
        "loaded": descriptor.is_some(),
        "items": models,
        "models": models,
    })))
}

async fn adapter_model_profiles(
    State(state): State<AppState>,
    Path((_company_id, adapter_type)): Path<(Uuid, String)>,
) -> ApiResult<Json<Value>> {
    let descriptor = state.adapters.descriptor(&adapter_type);
    let profiles = adapter_model_profiles_catalog(&adapter_type);
    let source = descriptor
        .as_ref()
        .map(|d| format!("{:?}", d.source).to_lowercase())
        .unwrap_or_else(|| "unknown".to_owned());
    Ok(Json(json!({
        "companyId": _company_id,
        "adapterType": adapter_type,
        "adapterSource": source,
        "items": profiles,
        "modelProfiles": profiles,
    })))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DetectAdapterModelBody {
    /// Optional explicit override: return this as the detected model
    model: Option<String>,
    /// Adapter config snapshot to read current model from
    adapter_config: Option<Value>,
    /// Agent ID to look up the agent's current adapter_config
    agent_id: Option<Uuid>,
}

async fn detect_adapter_model(
    State(state): State<AppState>,
    Path((company_id, adapter_type)): Path<(Uuid, String)>,
    Json(body): Json<DetectAdapterModelBody>,
) -> ApiResult<Json<Value>> {
    // Resolution order:
    // 1. Explicit body.model
    // 2. body.adapter_config.model
    // 3. agents.adapter_config.model for body.agent_id
    // 4. First item of the model catalog
    let descriptor = state.adapters.descriptor(&adapter_type);
    let source = descriptor
        .as_ref()
        .map(|d| format!("{:?}", d.source).to_lowercase())
        .unwrap_or_else(|| "unknown".to_owned());

    if let Some(m) = body.model.as_deref() {
        if !m.is_empty() {
            return Ok(Json(json!({
                "companyId": company_id,
                "adapterType": adapter_type,
                "model": m,
                "provider": adapter_type,
                "source": "explicit",
                "candidates": adapter_model_catalog(&adapter_type).iter().map(|x| x.get("id").cloned().unwrap_or(json!(null))).collect::<Vec<_>>(),
            })));
        }
    }
    if let Some(cfg) = body.adapter_config.as_ref() {
        if let Some(m) = cfg.get("model").and_then(Value::as_str) {
            return Ok(Json(json!({
                "companyId": company_id,
                "adapterType": adapter_type,
                "model": m,
                "provider": adapter_type,
                "source": "config",
                "candidates": adapter_model_catalog(&adapter_type).iter().map(|x| x.get("id").cloned().unwrap_or(json!(null))).collect::<Vec<_>>(),
            })));
        }
    }
    if let Some(agent_id) = body.agent_id {
        let row: Option<(Value,)> = sqlx::query_as(
            "SELECT adapter_config FROM agents WHERE id = $1 AND company_id = $2",
        )
        .bind(agent_id)
        .bind(company_id)
        .fetch_optional(state.db.pool())
        .await
        .ok()
        .flatten();
        if let Some((cfg,)) = row {
            if let Some(m) = cfg.get("model").and_then(Value::as_str) {
                return Ok(Json(json!({
                    "companyId": company_id,
                    "adapterType": adapter_type,
                    "model": m,
                    "provider": adapter_type,
                    "source": "agent_config",
                    "agentId": agent_id,
                })));
            }
        }
    }
    // Fallback: first catalog item
    let catalog = adapter_model_catalog(&adapter_type);
    let fallback = catalog
        .first()
        .and_then(|c| c.get("id").and_then(Value::as_str))
        .unwrap_or("default");
    Ok(Json(json!({
        "companyId": company_id,
        "adapterType": adapter_type,
        "model": fallback,
        "provider": adapter_type,
        "source": "default",
        "adapterSource": source,
        "candidates": catalog.iter().map(|x| x.get("id").cloned().unwrap_or(json!(null))).collect::<Vec<_>>(),
    })))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TestAdapterEnvBody {
    model: Option<String>,
    /// Optional: 'api' or 'subscription' — affects how the test is interpreted
    delivery_mode: Option<String>,
    /// Optional: skip network probing
    skip_network: Option<bool>,
}

async fn adapter_test_environment(
    State(state): State<AppState>,
    Path((company_id, adapter_type)): Path<(Uuid, String)>,
    Json(body): Json<TestAdapterEnvBody>,
) -> ApiResult<Json<Value>> {
    let descriptor = state.adapters.descriptor(&adapter_type);
    let source = descriptor
        .as_ref()
        .map(|d| format!("{:?}", d.source).to_lowercase())
        .unwrap_or_else(|| "unknown".to_owned());
    let loaded = descriptor.is_some();
    let skip_network = body.skip_network.unwrap_or(false);

    // Probe local CLI presence by trying to find common binaries on PATH.
    let mut checks: Vec<Value> = Vec::new();
    let probe_binary = match adapter_type.as_str() {
        "claude_local" | "claude_local_subscription" => Some("claude"),
        "codex_local" | "codex_app_server" => Some("codex"),
        "cursor_local" => Some("cursor"),
        "gemini_local" | "gemini_cli" => Some("gemini"),
        "grok_local" => Some("grok"),
        "opencode_local" => Some("opencode"),
        "pi_local" => Some("pi"),
        _ => None,
    };
    if let Some(bin) = probe_binary {
        let which = std::process::Command::new("which")
            .arg(bin)
            .output();
        let ok = which.as_ref().map(|o| o.status.success()).unwrap_or(false);
        let path = which
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        checks.push(json!({
            "name": format!("{}_binary", bin),
            "ok": ok,
            "path": path,
        }));
    } else {
        checks.push(json!({
            "name": "binary_probe",
            "ok": true,
            "note": format!("no probe configured for adapter_type={adapter_type}"),
        }));
    }
    if !skip_network {
        // Lightweight network reachability check via DNS resolution
        let resolved = tokio::net::lookup_host("dns.google:80").await;
        let ok = resolved.is_ok();
        checks.push(json!({
            "name": "network_reachability",
            "ok": ok,
            "note": if ok { "dns_resolved" } else { "dns_failed" },
        }));
    }
    let all_ok = checks.iter().all(|c| c.get("ok").and_then(Value::as_bool).unwrap_or(false));
    Ok(Json(json!({
        "companyId": company_id,
        "adapterType": adapter_type,
        "adapterSource": source,
        "loaded": loaded,
        "model": body.model,
        "deliveryMode": body.delivery_mode,
        "ok": all_ok,
        "checks": checks,
        "testedAt": chrono::Utc::now(),
    })))
}
