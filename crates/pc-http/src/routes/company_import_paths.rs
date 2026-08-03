//! 公司可导入路径配置：从 instance_settings.general 读取 / 写入。

use axum::{
    extract::{Path, State},
    routing::get,
    Json, Router,
};
use pc_repos::settings::{InstanceSetting, SettingsRepo};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/api/companies/:company_id/import-paths",
        get(get_import_paths).patch(update_import_paths),
    )
}

fn extract_import_paths(setting: &InstanceSetting) -> Vec<Value> {
    setting
        .general
        .get("importPaths")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
}

async fn get_import_paths(
    State(state): State<AppState>,
    Path(_company_id): Path<Uuid>,
) -> Json<Value> {
    let setting = SettingsRepo::new(&state.db)
        .get()
        .await
        .unwrap_or_else(|_| InstanceSetting {
            id: Uuid::nil(),
            singleton_key: "default".to_string(),
            default_environment_id: None,
            general: json!({}),
            experimental: json!({}),
            created_at: pc_core::Timestamp::now(),
            updated_at: pc_core::Timestamp::now(),
        });
    Json(json!({
        "paths": extract_import_paths(&setting),
        "updatedAt": setting.updated_at
    }))
}

async fn update_import_paths(
    State(state): State<AppState>,
    Path(_company_id): Path<Uuid>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let paths = body.get("paths").cloned().unwrap_or(json!([]));
    let updated = SettingsRepo::new(&state.db)
        .patch_simple(None, Some(json!({ "importPaths": paths })), None)
        .await
        .unwrap();
    Json(json!({
        "paths": extract_import_paths(&updated),
        "updatedAt": updated.updated_at
    }))
}
