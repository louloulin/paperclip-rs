//! `/api/adapters*`：运行时 adapter 注册表查询。

use axum::{
    extract::{Path, State},
    routing::get,
    Json, Router,
};
use pc_adapter_api::{AdapterDescriptor, AdapterSource};
use serde::Serialize;

use crate::{ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/adapters", get(list))
        .route("/api/adapters/:adapter_type", get(get_one))
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
