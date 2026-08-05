//! 公司 dashboard 与恢复观测路由。

use axum::{
    extract::{Path, Query, State},
    routing::get,
    Json, Router,
};
use chrono::{Duration, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::FromRow;
use uuid::Uuid;

use crate::{ApiError, ApiResult, AppState};
use pc_realtime::LiveEvent;
use pc_repos::agent::AgentRepo;
use pc_repos::approval::ApprovalRepo;
use pc_repos::company::CompanyRepo;
use pc_repos::cost::CostRepo;
use pc_repos::heartbeat::HeartbeatRepo;
use pc_repos::issue::IssueRepo;
use pc_repos::project::ProjectRepo;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/companies/:company_id/dashboard", get(summary))
        .route(
            "/api/companies/:company_id/recovery-observability",
            get(recovery_observability),
        )
}

#[derive(Debug, FromRow)]
#[allow(dead_code)]
struct CompanyBudget {
    id: Uuid,
    budget_monthly_cents: i32,
}

#[derive(Debug, FromRow)]
#[allow(dead_code)]
struct StatusCount {
    status: String,
    count: i64,
}

#[derive(Debug, FromRow)]
#[allow(dead_code)]
struct RunActivityRow {
    date: String,
    status: String,
    error_code: Option<String>,
    count: i64,
}

fn date_key(date: chrono::DateTime<Utc>) -> String {
    date.format("%Y-%m-%d").to_string()
}

fn empty_run_activity(date: &str) -> Value {
    json!({
        "date": date,
        "succeeded": 0,
        "failed": 0,
        "recovered": 0,
        "other": 0,
        "total": 0,
        "failedByErrorCode": {}
    })
}

fn first_of_month(now: chrono::DateTime<Utc>) -> chrono::DateTime<Utc> {
    let Some(first_day) = now.date_naive().with_day(1) else {
        return now;
    };
    let Some(midnight) = first_day.and_hms_opt(0, 0, 0) else {
        return now;
    };
    midnight.and_utc()
}

fn category_counts<F>(rows: Vec<StatusCount>, mut classify: F) -> Vec<i64>
where
    F: FnMut(&str, i64) -> Vec<i64>,
{
    let mut buckets: Vec<i64> = Vec::new();
    for row in rows {
        let update = classify(&row.status, row.count);
        for (idx, delta) in update.into_iter().enumerate() {
            if buckets.len() <= idx {
                buckets.resize(idx + 1, 0);
            }
            buckets[idx] += delta;
        }
    }
    buckets
}

fn agent_buckets(rows: Vec<StatusCount>) -> (i64, i64, i64, i64) {
    let deltas = category_counts(rows, |status, count| match status {
        "idle" | "active" => vec![count, 0, 0, 0],
        "running" => vec![0, count, 0, 0],
        "paused" => vec![0, 0, count, 0],
        "error" => vec![0, 0, 0, count],
        _ => Vec::new(),
    });
    let active = deltas.first().copied().unwrap_or_default();
    let running = deltas.get(1).copied().unwrap_or_default();
    let paused = deltas.get(2).copied().unwrap_or_default();
    let error = deltas.get(3).copied().unwrap_or_default();
    (active, running, paused, error)
}

fn task_buckets(rows: Vec<StatusCount>) -> (i64, i64, i64, i64) {
    let deltas = category_counts(rows, |status, count| match status {
        "in_progress" => vec![0, count, 0, 0],
        "blocked" => vec![0, 0, count, 0],
        "done" => vec![0, 0, 0, count],
        "cancelled" => Vec::new(),
        _ => vec![count, 0, 0, 0],
    });
    let open = deltas.first().copied().unwrap_or_default();
    let in_progress = deltas.get(1).copied().unwrap_or_default();
    let blocked = deltas.get(2).copied().unwrap_or_default();
    let done = deltas.get(3).copied().unwrap_or_default();
    (open, in_progress, blocked, done)
}

fn build_run_activity(rows: Vec<RunActivityRow>, first_day: chrono::DateTime<Utc>) -> Vec<Value> {
    let mut run_activity: std::collections::BTreeMap<String, Value> = (0..14)
        .map(|index| {
            let date = first_day + Duration::days(index);
            let key = date_key(date);
            (key.clone(), empty_run_activity(&key))
        })
        .collect();
    for row in rows {
        let Some(bucket) = run_activity.get_mut(&row.date) else {
            continue;
        };
        if let Some(total) = bucket.get_mut("total") {
            *total = json!(total.as_i64().unwrap_or_default() + row.count);
        }
        let field = match row.status.as_str() {
            "succeeded" => "succeeded",
            "failed" | "timed_out" => "failed",
            _ => "other",
        };
        if let Some(value) = bucket.get_mut(field) {
            *value = json!(value.as_i64().unwrap_or_default() + row.count);
        }
        if matches!(field, "failed") {
            let code = row.error_code.unwrap_or_else(|| "unknown".to_owned());
            if let Some(codes) = bucket
                .get_mut("failedByErrorCode")
                .and_then(Value::as_object_mut)
            {
                let count = codes.get(&code).and_then(Value::as_i64).unwrap_or_default();
                codes.insert(code, json!(count + row.count));
            }
        }
    }
    run_activity.into_values().collect()
}

fn utilization_percent(spend: i64, budget: i64) -> f64 {
    if budget <= 0 {
        return 0.0;
    }
    // Compute in basis points (× 100) using integer arithmetic and divide by 100
    // at the end. The intermediate `basis_points` value is bounded by `10_000`,
    // well within f64 precision.
    let basis_points = spend.saturating_mul(10_000) / budget;
    #[allow(clippy::cast_precision_loss)]
    let percent = basis_points as f64 / 100.0;
    percent
}

async fn load_company(state: &AppState, company_id: Uuid) -> ApiResult<CompanyBudget> {
    let budget = CompanyRepo::new(&state.db)
        .get_budget(company_id)
        .await?;
    budget
        .map(|b| CompanyBudget { id: company_id, budget_monthly_cents: b })
        .ok_or_else(|| ApiError::NotFound(format!("company {company_id}")))
}

async fn load_agent_status_counts(
    state: &AppState,
    company_id: Uuid,
) -> ApiResult<Vec<StatusCount>> {
    let rows = AgentRepo::new(&state.db)
        .count_by_status(company_id)
        .await?;
    Ok(rows.into_iter().map(|(status, count)| StatusCount { status, count }).collect())
}

async fn load_task_status_counts(
    state: &AppState,
    company_id: Uuid,
) -> ApiResult<Vec<StatusCount>> {
    let rows = IssueRepo::new(&state.db)
        .count_visible_by_status(company_id)
        .await?;
    Ok(rows.into_iter().map(|(status, count)| StatusCount { status, count }).collect())
}

async fn load_pending_approvals(state: &AppState, company_id: Uuid) -> ApiResult<i64> {
    Ok(ApprovalRepo::new(&state.db).count_pending(company_id).await.map_err(|e| ApiError::Internal(e.to_string()))?)
}

async fn load_month_spend(
    state: &AppState,
    company_id: Uuid,
    month_start: chrono::DateTime<Utc>,
) -> ApiResult<i64> {
    let ts = pc_core::Timestamp::from_dt(month_start);
    Ok(CostRepo::new(&state.db).sum_cost_cents_since(company_id, ts).await.map_err(|e| ApiError::Internal(e.to_string()))?)
}

async fn load_run_activity(
    state: &AppState,
    company_id: Uuid,
    first_day: chrono::DateTime<Utc>,
) -> ApiResult<Vec<RunActivityRow>> {
    let ts = pc_core::Timestamp::from_dt(first_day);
    let rows = HeartbeatRepo::new(&state.db)
        .group_runs_by_date_status_error(company_id, ts)
        .await?;
    Ok(rows.into_iter().map(|(date, status, error_code, count)| RunActivityRow {
        date, status, error_code, count,
    }).collect())
}

async fn load_paused_projects(state: &AppState, company_id: Uuid) -> ApiResult<i64> {
    Ok(ProjectRepo::new(&state.db).count_paused(company_id).await.map_err(|e| ApiError::Internal(e.to_string()))?)
}

async fn summary(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let company = load_company(&state, company_id).await?;
    let agent_rows = load_agent_status_counts(&state, company_id).await?;
    let task_rows = load_task_status_counts(&state, company_id).await?;
    let pending_approvals = load_pending_approvals(&state, company_id).await?;
    let month_start = first_of_month(Utc::now());
    let month_spend = load_month_spend(&state, company_id, month_start).await?;
    let (active, running, paused, error) = agent_buckets(agent_rows);
    let (open, in_progress, blocked, done) = task_buckets(task_rows);

    let today = Utc::now().date_naive();
    let first_day = today
        .checked_sub_days(chrono::Days::new(13))
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map_or(Utc::now(), |value| value.and_utc());
    let run_rows = load_run_activity(&state, company_id, first_day).await?;
    let run_activity = build_run_activity(run_rows, first_day);
    let budget = i64::from(company.budget_monthly_cents);
    let utilization = utilization_percent(month_spend, budget);
    let paused_projects = load_paused_projects(&state, company_id).await?;
    Ok(Json(json!({
        "companyId": company.id,
        "agents": { "active": active, "running": running, "paused": paused, "error": error },
        "tasks": { "open": open, "inProgress": in_progress, "blocked": blocked, "done": done },
        "costs": { "monthSpendCents": month_spend, "monthBudgetCents": budget, "monthUtilizationPercent": utilization },
        "pendingApprovals": pending_approvals,
        "budgets": { "activeIncidents": 0, "pendingApprovals": pending_approvals, "pausedAgents": paused, "pausedProjects": paused_projects },
        "runActivity": run_activity
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

trait FirstDay {
    fn with_day(self, day: u32) -> Option<chrono::NaiveDate>;
}

impl FirstDay for chrono::NaiveDate {
    fn with_day(self, day: u32) -> Option<chrono::NaiveDate> {
        chrono::NaiveDate::from_ymd_opt(self.year(), self.month(), day)
    }
}

use chrono::Datelike;
