//! 成本、财务事件和预算路由。

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, patch, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use pc_repos::cost::{CostRange, CostRepo, CreateCostEvent};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::FromRow;
use uuid::Uuid;

use crate::{ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/companies/:company_id/cost-events",
            post(create_cost_event),
        )
        .route("/api/companies/:company_id/costs/summary", get(summary))
        .route("/api/companies/:company_id/costs/by-agent", get(by_agent))
        .route(
            "/api/companies/:company_id/costs/by-agent-model",
            get(by_agent_model),
        )
        .route(
            "/api/companies/:company_id/costs/by-provider",
            get(by_provider),
        )
        .route("/api/companies/:company_id/costs/by-biller", get(by_biller))
        .route(
            "/api/companies/:company_id/costs/by-project",
            get(by_project),
        )
        .route(
            "/api/companies/:company_id/costs/finance-summary",
            get(finance_summary),
        )
        .route(
            "/api/companies/:company_id/costs/finance-by-biller",
            get(finance_by_biller),
        )
        .route(
            "/api/companies/:company_id/costs/finance-by-kind",
            get(finance_by_kind),
        )
        .route(
            "/api/companies/:company_id/costs/finance-events",
            get(finance_events),
        )
        .route(
            "/api/companies/:company_id/costs/window-spend",
            get(window_spend),
        )
        .route(
            "/api/companies/:company_id/costs/quota-windows",
            get(quota_windows),
        )
        .route(
            "/api/companies/:company_id/budgets/overview",
            get(budget_overview),
        )
        .route(
            "/api/companies/:company_id/budgets",
            patch(update_company_budget),
        )
        .route("/api/agents/:agent_id/budgets", patch(update_agent_budget))
        .route(
            "/api/issues/:issue_id/cost-summary",
            get(issue_cost_summary),
        )
}

#[derive(Debug, Deserialize)]
struct CostQuery {
    from: Option<String>,
    to: Option<String>,
    limit: Option<i64>,
}

fn parse_date(raw: Option<String>, field: &str) -> ApiResult<Option<DateTime<Utc>>> {
    raw.map(|value| {
        DateTime::parse_from_rfc3339(&value)
            .map(|date| date.with_timezone(&Utc))
            .map_err(|_| ApiError::BadRequest(format!("invalid '{field}' date")))
    })
    .transpose()
}

fn parse_range(query: &CostQuery) -> ApiResult<CostRange> {
    Ok(CostRange {
        from: parse_date(query.from.clone(), "from")?,
        to: parse_date(query.to.clone(), "to")?,
    })
}

fn limit(query: &CostQuery) -> ApiResult<i64> {
    let value = query.limit.unwrap_or(100);
    if !(1..=500).contains(&value) {
        return Err(ApiError::BadRequest("invalid 'limit' value".to_owned()));
    }
    Ok(value)
}

async fn create_cost_event(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<CreateCostEvent>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let result = CostRepo::new(&state.db)
        .create_event(company_id, &body)
        .await?;
    Ok((StatusCode::CREATED, Json(result)))
}

async fn summary(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Query(query): Query<CostQuery>,
) -> ApiResult<Json<pc_repos::cost::CostSummary>> {
    Ok(Json(
        CostRepo::new(&state.db)
            .summary(company_id, parse_range(&query)?)
            .await?,
    ))
}

async fn by_agent(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Query(query): Query<CostQuery>,
) -> ApiResult<Json<Vec<pc_repos::cost::CostByAgent>>> {
    Ok(Json(
        CostRepo::new(&state.db)
            .by_agent(company_id, parse_range(&query)?)
            .await?,
    ))
}

async fn by_agent_model(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Query(query): Query<CostQuery>,
) -> ApiResult<Json<Vec<pc_repos::cost::CostByAgentModel>>> {
    Ok(Json(
        CostRepo::new(&state.db)
            .by_agent_model(company_id, parse_range(&query)?)
            .await?,
    ))
}

async fn by_provider(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Query(query): Query<CostQuery>,
) -> ApiResult<Json<Vec<pc_repos::cost::CostByProviderModel>>> {
    Ok(Json(
        CostRepo::new(&state.db)
            .by_provider(company_id, parse_range(&query)?)
            .await?,
    ))
}

async fn by_biller(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Query(query): Query<CostQuery>,
) -> ApiResult<Json<Vec<pc_repos::cost::CostByBiller>>> {
    Ok(Json(
        CostRepo::new(&state.db)
            .by_biller(company_id, parse_range(&query)?)
            .await?,
    ))
}

async fn by_project(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Query(query): Query<CostQuery>,
) -> ApiResult<Json<Vec<pc_repos::cost::CostByProject>>> {
    Ok(Json(
        CostRepo::new(&state.db)
            .by_project(company_id, parse_range(&query)?)
            .await?,
    ))
}

async fn finance_summary(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Query(query): Query<CostQuery>,
) -> ApiResult<Json<pc_repos::cost::FinanceSummary>> {
    Ok(Json(
        CostRepo::new(&state.db)
            .finance_summary(company_id, parse_range(&query)?)
            .await?,
    ))
}

async fn finance_by_biller(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Query(query): Query<CostQuery>,
) -> ApiResult<Json<Vec<pc_repos::cost::FinanceByBiller>>> {
    Ok(Json(
        CostRepo::new(&state.db)
            .finance_by_biller(company_id, parse_range(&query)?)
            .await?,
    ))
}

async fn finance_by_kind(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Query(query): Query<CostQuery>,
) -> ApiResult<Json<Vec<pc_repos::cost::FinanceByKind>>> {
    Ok(Json(
        CostRepo::new(&state.db)
            .finance_by_kind(company_id, parse_range(&query)?)
            .await?,
    ))
}

async fn finance_events(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Query(query): Query<CostQuery>,
) -> ApiResult<Json<Vec<pc_repos::cost::FinanceEventRow>>> {
    Ok(Json(
        CostRepo::new(&state.db)
            .finance_events(company_id, parse_range(&query)?, limit(&query)?)
            .await?,
    ))
}

async fn window_spend(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Vec<pc_repos::cost::CostWindowSpendRow>>> {
    Ok(Json(
        CostRepo::new(&state.db).window_spend(company_id).await?,
    ))
}

async fn quota_windows() -> Json<Vec<Value>> {
    Json(Vec::new())
}

#[derive(Debug, FromRow)]
struct BudgetCounts {
    pending_approvals: i64,
    paused_agents: i64,
    paused_projects: i64,
}

async fn budget_overview(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let counts = sqlx::query_as::<_, BudgetCounts>(
        "SELECT (SELECT COUNT(*) FROM approvals WHERE company_id = $1 AND status = 'pending')::bigint AS pending_approvals, \
                (SELECT COUNT(*) FROM agents WHERE company_id = $1 AND status = 'paused')::bigint AS paused_agents, \
                (SELECT COUNT(*) FROM projects WHERE company_id = $1 AND status = 'paused')::bigint AS paused_projects",
    )
    .bind(company_id)
    .fetch_one(state.db.pool())
    .await?;
    Ok(Json(json!({
        "activeIncidents": [],
        "pendingApprovalCount": counts.pending_approvals,
        "pausedAgentCount": counts.paused_agents,
        "pausedProjectCount": counts.paused_projects,
        "policies": []
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BudgetBody {
    budget_monthly_cents: i32,
}

async fn update_company_budget(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<BudgetBody>,
) -> ApiResult<Json<Value>> {
    if body.budget_monthly_cents < 0 {
        return Err(ApiError::BadRequest(
            "budgetMonthlyCents must be non-negative".to_owned(),
        ));
    }
    let row = sqlx::query_as::<_, ValueRow>(
        "UPDATE companies SET budget_monthly_cents = $2, updated_at = now() \
         WHERE id = $1 RETURNING id, budget_monthly_cents",
    )
    .bind(company_id)
    .bind(body.budget_monthly_cents)
    .fetch_optional(state.db.pool())
    .await?
    .ok_or_else(|| ApiError::NotFound(format!("company {company_id}")))?;
    Ok(Json(json!({
        "id": row.id,
        "budgetMonthlyCents": row.budget_monthly_cents
    })))
}

#[derive(Debug, FromRow)]
struct ValueRow {
    id: Uuid,
    budget_monthly_cents: i32,
}

async fn update_agent_budget(
    State(state): State<AppState>,
    Path(agent_id): Path<Uuid>,
    Json(body): Json<BudgetBody>,
) -> ApiResult<Json<Value>> {
    if body.budget_monthly_cents < 0 {
        return Err(ApiError::BadRequest(
            "budgetMonthlyCents must be non-negative".to_owned(),
        ));
    }
    let row = sqlx::query_as::<_, AgentBudgetRow>(
        "UPDATE agents SET budget_monthly_cents = $2, updated_at = now() \
         WHERE id = $1 RETURNING id, company_id, budget_monthly_cents, spent_monthly_cents",
    )
    .bind(agent_id)
    .bind(body.budget_monthly_cents)
    .fetch_optional(state.db.pool())
    .await?
    .ok_or_else(|| ApiError::NotFound(format!("agent {agent_id}")))?;
    Ok(Json(json!({
        "id": row.id,
        "companyId": row.company_id,
        "budgetMonthlyCents": row.budget_monthly_cents,
        "spentMonthlyCents": row.spent_monthly_cents
    })))
}

#[derive(Debug, FromRow)]
struct AgentBudgetRow {
    id: Uuid,
    company_id: Uuid,
    budget_monthly_cents: i32,
    spent_monthly_cents: i32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct IssueCostSummary {
    issue_id: Uuid,
    issue_count: i64,
    include_descendants: bool,
    cost_cents: i64,
    input_tokens: i64,
    cached_input_tokens: i64,
    output_tokens: i64,
    run_count: i64,
    runtime_ms: i64,
}

#[derive(Debug, FromRow)]
struct IssueCostRow {
    cost_cents: i64,
    input_tokens: i64,
    cached_input_tokens: i64,
    output_tokens: i64,
    run_count: i64,
    runtime_ms: i64,
}

async fn issue_cost_summary(
    State(state): State<AppState>,
    Path(issue_id): Path<Uuid>,
    Query(query): Query<IssueCostQuery>,
) -> ApiResult<Json<IssueCostSummary>> {
    let row = sqlx::query_as::<_, IssueCostRow>(
        "SELECT COALESCE(SUM(ce.cost_cents),0)::bigint AS cost_cents, \
                COALESCE(SUM(ce.input_tokens),0)::bigint AS input_tokens, \
                COALESCE(SUM(ce.cached_input_tokens),0)::bigint AS cached_input_tokens, \
                COALESCE(SUM(ce.output_tokens),0)::bigint AS output_tokens, \
                COUNT(DISTINCT ce.heartbeat_run_id)::bigint AS run_count, \
                COALESCE(SUM(EXTRACT(EPOCH FROM (COALESCE(hr.finished_at, now()) - hr.started_at)) * 1000),0)::bigint AS runtime_ms \
         FROM issues i LEFT JOIN cost_events ce ON ce.issue_id = i.id \
         LEFT JOIN heartbeat_runs hr ON hr.id = ce.heartbeat_run_id WHERE i.id = $1 GROUP BY i.company_id",
    )
    .bind(issue_id)
    .fetch_optional(state.db.pool())
    .await?
    .ok_or_else(|| ApiError::NotFound(format!("issue {issue_id}")))?;
    Ok(Json(IssueCostSummary {
        issue_id,
        issue_count: i64::from(!query.exclude_root),
        include_descendants: true,
        cost_cents: row.cost_cents,
        input_tokens: row.input_tokens,
        cached_input_tokens: row.cached_input_tokens,
        output_tokens: row.output_tokens,
        run_count: row.run_count,
        runtime_ms: row.runtime_ms,
    }))
}

#[derive(Debug, Deserialize)]
struct IssueCostQuery {
    #[serde(default)]
    exclude_root: bool,
}
