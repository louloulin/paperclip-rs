//! 公司侧边栏徽标聚合。

use axum::{
    extract::{Path, State},
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/api/companies/:company_id/sidebar-badges",
        get(sidebar_badges),
    )
}

async fn sidebar_badges(
    State(_state): State<AppState>,
    Path(_company_id): Path<Uuid>,
) -> Json<Value> {
    Json(json!({
        "agents": { "errors": 0, "running": 0, "paused": 0 },
        "issues": { "blocked": 0, "inProgress": 0, "needsReview": 0, "unread": 0 },
        "approvals": { "pending": 0 },
        "costs": { "alerts": 0 },
        "runs": { "failedRecent": 0, "running": 0 }
    }))
}
