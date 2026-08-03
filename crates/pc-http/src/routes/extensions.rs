//! 高频使用的 issue 扩展端点：heartbeat-context、counts。
//!
//! 与原 `paperclip/server/src/routes/issues.ts` 中关键端点等价。

use axum::{
    extract::{Path, State},
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};
use sqlx::FromRow;
use uuid::Uuid;

use crate::{ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        // issues 扩展
        .route(
            "/api/issues/:id/heartbeat-context",
            get(issue_heartbeat_context),
        )
        // 注: /api/companies/:company_id/issues/count 由 routes/issues.rs 注册
}

#[derive(Debug, FromRow)]
struct HeartbeatRunRow {
    id: Uuid,
    agent_id: Uuid,
    status: String,
    started_at: chrono::DateTime<chrono::Utc>,
    finished_at: Option<chrono::DateTime<chrono::Utc>>,
    prompt: Option<String>,
    error: Option<String>,
}

async fn issue_heartbeat_context(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    // 最近 5 次 heartbeat run（按 started_at 倒序）
    let rows: Vec<HeartbeatRunRow> = sqlx::query_as(
        "SELECT id, agent_id, status, started_at, finished_at, prompt, error \
         FROM heartbeat_runs \
         WHERE issue_id = $1 \
         ORDER BY started_at DESC LIMIT 5",
    )
    .bind(id)
    .fetch_all(state.db.pool())
    .await
    .unwrap_or_default();
    let runs: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.id,
                "agentId": r.agent_id,
                "status": r.status,
                "startedAt": r.started_at,
                "finishedAt": r.finished_at,
                "prompt": r.prompt,
                "error": r.error,
            })
        })
        .collect();
    Ok(Json(json!({
        "issueId": id,
        "recentRuns": runs,
        "totalRuns": runs.len(),
    })))
}

#[derive(Debug, FromRow)]
struct CountRow {
    status: String,
    count: i64,
}

async fn company_issues_count(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows: Vec<CountRow> = sqlx::query_as(
        "SELECT status, COUNT(*)::bigint AS count FROM issues \
         WHERE company_id = $1 AND hidden_at IS NULL \
         GROUP BY status",
    )
    .bind(company_id)
    .fetch_all(state.db.pool())
    .await
    .unwrap_or_default();
    let by_status: serde_json::Map<String, Value> = rows
        .iter()
        .map(|r| (r.status.clone(), json!(r.count)))
        .collect();
    let total: i64 = rows.iter().map(|r| r.count).sum();
    Ok(Json(json!({
        "companyId": company_id,
        "total": total,
        "byStatus": by_status,
    })))
}
