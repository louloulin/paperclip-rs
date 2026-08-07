//! 高频使用的 issue 扩展端点：heartbeat-context、counts。
//!
//! 与原 `paperclip/server/src/routes/issues.ts` 中关键端点等价。

use axum::{
    extract::{Path, State},
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{ApiResult, AppState};
use pc_repos::heartbeat::HeartbeatRepo;
use pc_repos::issue::IssueRepo;

pub fn router() -> Router<AppState> {
    Router::new()
    // Canonical GET `/api/issues/:issue_id/heartbeat-context` is registered
    // by `routes::issues` (Round 27). Round 215 dedupe removed the
    // duplicate registration here because axum 0.7 treats `:id` and
    // `:issue_id` at the same position as a conflicting insertion and
    // panics at startup. The local handler is kept as dead code.
    // 注: /api/companies/:company_id/issues/count 由 routes/issues.rs 注册
}

async fn issue_heartbeat_context(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    // 最近 5 次 heartbeat run（按 started_at 倒序）
    let rows = HeartbeatRepo::new(&state.db)
        .list_recent_for_issue(id)
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

async fn company_issues_count(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = IssueRepo::new(&state.db)
        .count_by_status_visible(company_id)
        .await
        .unwrap_or_default();
    let by_status: serde_json::Map<String, Value> = rows
        .iter()
        .map(|(status, count)| (status.clone(), json!(count)))
        .collect();
    let total: i64 = rows.iter().map(|(_, count)| *count).sum();
    Ok(Json(json!({
        "companyId": company_id,
        "total": total,
        "byStatus": by_status,
    })))
}
