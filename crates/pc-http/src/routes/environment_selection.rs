//! 当前公司选择的环境：读写 instance_settings.general.currentEnvironmentId。

use axum::{
    extract::{Path, State},
    routing::{get, patch},
    Json, Router,
};
use pc_repos::settings::{InstanceSetting, SettingsRepo};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/companies/:company_id/environment-selection",
            get(get_selection).patch(update_selection),
        )
        .route(
            "/api/companies/:company_id/environment-selection/validate",
            axum::routing::post(validate_selection),
        )
}

fn default_setting() -> InstanceSetting {
    InstanceSetting {
        id: Uuid::nil(),
        singleton_key: "default".to_string(),
        default_environment_id: None,
        general: json!({}),
        experimental: json!({}),
        created_at: pc_core::Timestamp::now(),
        updated_at: pc_core::Timestamp::now(),
    }
}

async fn get_setting(state: &AppState) -> InstanceSetting {
    SettingsRepo::new(&state.db)
        .get("default")
        .await
        .unwrap_or_else(|_| default_setting())
}

async fn get_selection(State(state): State<AppState>, Path(company_id): Path<Uuid>) -> Json<Value> {
    let setting = get_setting(&state).await;
    let env_id = setting
        .general
        .get("currentEnvironmentId")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .or(setting.default_environment_id);
    Json(json!({
        "companyId": company_id,
        "environmentId": env_id,
        "updatedAt": setting.updated_at
    }))
}

async fn update_selection(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let env_id = body
        .get("environmentId")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok());
    let patch_result = SettingsRepo::new(&state.db)
        .patch(
            None,
            Some(json!({ "currentEnvironmentId": env_id.map(|u| u.to_string()) })),
            None,
        )
        .await;
    let updated = match patch_result {
        Ok(s) => s,
        Err(_) => get_setting(&state).await,
    };
    Json(json!({
        "companyId": company_id,
        "environmentId": env_id,
        "updatedAt": updated.updated_at
    }))
}

async fn validate_selection(
    State(state): State<AppState>,
    Path(_company_id): Path<Uuid>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let env_id_str = body.get("environmentId").and_then(|v| v.as_str());
    let env_id = match env_id_str.and_then(|s| Uuid::parse_str(s).ok()) {
        Some(id) => id,
        None => {
            return Json(json!({
                "valid": true,
                "reason": "no environment selected"
            }));
        }
    };
    let env = pc_repos::environment::EnvironmentRepo::new(&state.db)
        .get(env_id)
        .await
        .ok()
        .flatten();
    match env {
        Some(e) if e.status == "archived" => Json(json!({
            "valid": false,
            "reason": "Environment is archived."
        })),
        None => Json(json!({
            "valid": false,
            "reason": "Environment not found."
        })),
        Some(e) => Json(json!({
            "valid": true,
            "environment": {
                "id": e.id,
                "name": e.name,
                "driver": e.driver,
                "status": e.status
            }
        })),
    }
}
