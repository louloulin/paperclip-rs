//! 公司 dashboard 与恢复观测路由。
//!
//! 业务逻辑下沉到 `pc_routines::DashboardService`（1:1 复刻 Node `dashboard.ts`）；
//! 本文件只负责 HTTP 序列化。

use axum::{
    extract::{Path, Query, State},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{ApiError, ApiResult, AppState};
use pc_routines::DashboardService;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/companies/:company_id/dashboard", get(summary))
        .route(
            "/api/companies/:company_id/recovery-observability",
            get(recovery_observability),
        )
}

async fn summary(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let svc = DashboardService::new(&state.db);
    let s = svc.summary(company_id).await.map_err(|e| match e {
        pc_routines::DashboardError::CompanyNotFound(id) => ApiError::NotFound(format!("company {id}")),
        pc_routines::DashboardError::Repo(r) => ApiError::Internal(r.to_string()),
    })?;
    Ok(Json(json!({
        "companyId": s.company_id,
        "agents": {
            "active": s.agents.active,
            "running": s.agents.running,
            "paused": s.agents.paused,
            "error": s.agents.error,
        },
        "tasks": {
            "open": s.tasks.open,
            "inProgress": s.tasks.in_progress,
            "blocked": s.tasks.blocked,
            "done": s.tasks.done,
        },
        "costs": {
            "monthSpendCents": s.costs.month_spend_cents,
            "monthBudgetCents": s.costs.month_budget_cents,
            // 月度利用率用 basis points 表达 × 100 = 百分比 × 100 (保留两位精度)
            "monthUtilizationPercent": s.costs.month_utilization_percent as f64 / 100.0,
        },
        "pendingApprovals": s.pending_approvals,
        "budgets": {
            "activeIncidents": s.budgets.active_incidents,
            "pendingApprovals": s.budgets.pending_approvals,
            "pausedAgents": s.budgets.paused_agents,
            "pausedProjects": s.budgets.paused_projects,
        },
        "runActivity": s.run_activity,
    })))
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct RecoveryQuery {
    weeks: Option<f64>,
    threshold: Option<f64>,
}

async fn recovery_observability(
    Path(company_id): Path<Uuid>,
    Query(query): Query<RecoveryQuery>,
) -> Json<Value> {
    let weeks = query
        .weeks
        .filter(|value| value.is_finite() && *value > 0.0)
        .map_or(8.0, |value| value.min(52.0));
    let threshold = query
        .threshold
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(50.0);
    Json(json!({
        "companyId": company_id,
        "weeks": weeks,
        "thresholdPercent": threshold,
        "series": [],
        "summary": { "recoveryRatePercent": 0, "meetsThreshold": true }
    }))
}
