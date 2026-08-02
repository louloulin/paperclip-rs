//! `/api/activity*` 路由：read + log。
use crate::{ApiResult, AppState};
#[allow(unused_imports)]
use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use pc_repos::activity::ActivityRepo;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/activity", get(list).post(log))
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    company_id: Uuid,
    #[serde(default = "default_limit")]
    limit: i64,
}
fn default_limit() -> i64 {
    100
}

async fn list(
    State(s): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        serde_json::to_value(
            ActivityRepo::new(&s.db)
                .list_by_company(q.company_id, q.limit)
                .await?,
        )
        .unwrap_or_default(),
    ))
}
#[derive(Debug, Deserialize)]
struct LogBody {
    company_id: Uuid,
    actor_type: String,
    actor_id: String,
    action: String,
    entity_type: String,
    entity_id: String,
}
async fn log(State(s): State<AppState>, Json(b): Json<LogBody>) -> ApiResult<Json<Value>> {
    let r = ActivityRepo::new(&s.db)
        .log(
            b.company_id,
            &b.actor_type,
            &b.actor_id,
            &b.action,
            &b.entity_type,
            &b.entity_id,
        )
        .await?;
    Ok(Json(
        json!({"id":r.id,"action":r.action,"entity_type":r.entity_type,"entity_id":r.entity_id}),
    ))
}
