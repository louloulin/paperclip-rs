//! `/api/feature-flags*` 路由：暴露 pc-feature-flags。

use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use pc_feature_flags::{FeatureKey, RolloutStrategy};

use crate::{ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/feature-flags", get(list_flags).post(register_flag))
        .route("/api/feature-flags/evaluate", post(evaluate_flag))
        .route("/api/feature-flags/:key/enabled", post(set_enabled))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegisterBody {
    key: String,
    enabled: bool,
    rollout_pct: Option<u8>,
    rollout_allow_list: Option<Vec<Uuid>>,
    rollout_deny_list: Option<Vec<Uuid>>,
}

impl RegisterBody {
    fn into_rollout(self) -> Option<RolloutStrategy> {
        if let Some(pct) = self.rollout_pct {
            return Some(RolloutStrategy::Percentage { pct });
        }
        if let Some(allow) = self.rollout_allow_list {
            return Some(RolloutStrategy::AllowList { ids: allow });
        }
        if let Some(deny) = self.rollout_deny_list {
            return Some(RolloutStrategy::DenyList { ids: deny });
        }
        None
    }
}

async fn list_flags(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let snaps = state.feature_flags.catalog().list();
    let items: Vec<Value> = snaps
        .into_iter()
        .map(|s| {
            json!({
                "key": s.key.as_str(),
                "enabled": s.enabled,
                "hasRollout": s.rule.is_some(),
            })
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

async fn register_flag(
    State(state): State<AppState>,
    Json(body): Json<RegisterBody>,
) -> ApiResult<Json<Value>> {
    let key_str = body.key.clone();
    let enabled = body.enabled;
    let rule_strategy = body.into_rollout();
    let key = FeatureKey::new(Box::leak(key_str.clone().into_boxed_str()));
    let rule = rule_strategy.map(|s| pc_feature_flags::rules::RolloutRule { strategy: s });
    state.feature_flags.catalog().register(key, enabled, rule);
    Ok(Json(json!({ "key": key_str, "registered": true })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EvalBody {
    key: String,
    actor_id: Uuid,
}

async fn evaluate_flag(
    State(state): State<AppState>,
    Json(body): Json<EvalBody>,
) -> ApiResult<Json<Value>> {
    let key = FeatureKey::new(Box::leak(body.key.clone().into_boxed_str()));
    let enabled = state.feature_flags.is_enabled(&key, body.actor_id);
    Ok(Json(json!({
        "key": body.key,
        "actorId": body.actor_id,
        "enabled": enabled,
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EnableBody {
    enabled: bool,
}

async fn set_enabled(
    State(state): State<AppState>,
    Path(key): Path<String>,
    Json(body): Json<EnableBody>,
) -> ApiResult<Json<Value>> {
    let fk = FeatureKey::new(Box::leak(key.clone().into_boxed_str()));
    let updated = state.feature_flags.catalog().set_enabled(&fk, body.enabled);
    if !updated {
        return Err(ApiError::NotFound(format!("feature flag {key}")));
    }
    Ok(Json(json!({ "key": key, "enabled": body.enabled })))
}
