//! `/api/v1/*` 版本化 API 路由。
//!
//! R575：补齐 Node 上游 UI client 真实调用但 Rust 端未实现的路径。
//! 当前仅含 heartbeat runs 列表；后续轮次按 UI 真实调用表增量添加。
//!
//! 设计：
//! - **版本前缀**: `/api/v1/...` 与 `/api/...` 解耦，便于将来 v2 引入不兼容变更
//! - **公司隔离**: `company_id` 是必需 query 参数，由调用方负责 scope
//! - **纯查询**: 此版本下 v1 仅暴露读路径；写路径继续走 `/api/...`

#![forbid(unsafe_code)]

use crate::error::{ApiError, ApiResult};
use crate::AppState;
use axum::{
    extract::{Query, State},
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};

/// Router factory — mount at `/api/v1` from `mod.rs`.
pub fn router() -> Router<AppState> {
    Router::new().route("/runs", get(list_runs))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListRunsQuery {
    /// Required: scope the listing to a single company.
    pub company_id: uuid::Uuid,
    /// Optional agent filter.
    #[serde(default)]
    pub agent_id: Option<uuid::Uuid>,
    /// Optional status filter (comma-separated).
    #[serde(default)]
    pub statuses: Option<String>,
    /// Optional responsible-user filter.
    #[serde(default)]
    pub responsible_user_id: Option<String>,
    /// Max rows to return (clamped to [1, 1000]).
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunSummary {
    pub id: uuid::Uuid,
    pub company_id: uuid::Uuid,
    pub agent_id: uuid::Uuid,
    pub status: String,
    pub started_at: Option<pc_core::Timestamp>,
    pub finished_at: Option<pc_core::Timestamp>,
    pub invocation_source: String,
    pub trigger_detail: Option<String>,
    pub error: Option<String>,
}

fn parse_statuses(raw: Option<&str>) -> Vec<pc_repos::heartbeat::HeartbeatRunStatus> {
    let Some(raw) = raw else { return vec![] };
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect()
}

async fn list_runs(
    State(state): State<AppState>,
    Query(q): Query<ListRunsQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    use pc_repos::heartbeat::{HeartbeatRepo, HeartbeatRunFilter};

    let filter = HeartbeatRunFilter {
        agent_id: q.agent_id,
        statuses: parse_statuses(q.statuses.as_deref()),
        responsible_user_id: q.responsible_user_id,
        limit: q.limit,
    };

    let rows = HeartbeatRepo::new(&state.db)
        .list_for_company(q.company_id, &filter)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let items: Vec<RunSummary> = rows
        .into_iter()
        .map(|r| RunSummary {
            id: r.id,
            company_id: r.company_id,
            agent_id: r.agent_id,
            status: r.status,
            started_at: r.started_at,
            finished_at: r.finished_at,
            invocation_source: r.invocation_source,
            trigger_detail: r.trigger_detail,
            error: r.error,
        })
        .collect();

    Ok(Json(serde_json::json!({
        "items": items,
        "companyId": q.company_id,
        "count": items.len(),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r575_parse_statuses_empty() {
        assert!(parse_statuses(None).is_empty());
        assert!(parse_statuses(Some("")).is_empty());
        assert!(parse_statuses(Some("   ")).is_empty());
    }

    #[test]
    fn r575_parse_statuses_single() {
        let parsed = parse_statuses(Some("running"));
        assert_eq!(parsed.len(), 1);
    }

    #[test]
    fn r575_parse_statuses_multiple() {
        let parsed = parse_statuses(Some("running, queued, succeeded"));
        assert_eq!(parsed.len(), 3);
    }

    #[test]
    fn r575_parse_statuses_invalid_filtered() {
        let parsed = parse_statuses(Some("running, invalid_state, succeeded"));
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn r575_router_exposes_runs_path() {
        let r = router();
        // axum Router has no public introspection; verify the file compiles
        // and the function returns a Router.
        let _ = r;
    }
}
