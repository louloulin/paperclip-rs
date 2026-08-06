//! 成本、财务事件和预算路由。

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, patch, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use pc_repos::budget::{
    BudgetRepo, IncidentRow, PolicyRow, ResolveIncidentInput, UpsertPolicyInput,
};
use pc_repos::cost::{CostEventRow, CostRange, CostRepo, CreateCostEvent};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{ApiError, ApiResult, AppState};
use pc_repos::agent::AgentRepo;
use pc_repos::approval::ApprovalRepo;
use pc_repos::company::CompanyRepo;
use pc_repos::project::ProjectRepo;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/companies/:company_id/cost-events",
            post(create_cost_event).get(list_cost_events),
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
        .route(
            "/api/companies/:company_id/budgets/policies",
            get(list_budget_policies).post(upsert_budget_policy),
        )
        .route(
            "/api/companies/:company_id/budget-incidents/:incident_id/resolve",
            post(resolve_budget_incident),
        )
        .route(
            "/api/companies/:company_id/finance-events",
            post(create_finance_event),
        )
        .route("/api/agents/:agent_id/budgets", patch(update_agent_budget))
        .route(
            "/api/issues/:issue_id/cost-summary",
            get(issue_cost_summary),
        )
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
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
// ============================================================================
// Round 212: GET /api/companies/:company_id/cost-events (list)
// ============================================================================

#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)]
struct ListCostEventsQuery {
    #[serde(default = "default_list_limit")]
    limit: i64,
}

fn default_list_limit() -> i64 {
    100
}

fn cost_event_json(row: &CostEventRow) -> Value {
    json!({
        "id": row.id,
        "companyId": row.company_id,
        "agentId": row.agent_id,
        "issueId": row.issue_id,
        "projectId": row.project_id,
        "goalId": row.goal_id,
        "billingCode": row.billing_code,
        "provider": row.provider,
        "model": row.model,
        "inputTokens": row.input_tokens,
        "outputTokens": row.output_tokens,
        "costCents": row.cost_cents,
        "occurredAt": row.occurred_at,
        "createdAt": row.created_at,
    })
}

async fn list_cost_events(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Query(q): Query<ListCostEventsQuery>,
) -> ApiResult<Json<Value>> {
    let rows = CostRepo::new(&state.db)
        .list_cost_events(company_id, q.limit)
        .await?;
    let items: Vec<Value> = rows.iter().map(cost_event_json).collect();
    let total_cost: i64 = rows.iter().map(|r| r.cost_cents as i64).sum();
    Ok(Json(json!({
        "companyId": company_id,
        "total": items.len(),
        "totalCostCents": total_cost,
        "limit": q.limit,
        "items": items,
    })))
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

async fn budget_overview(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let repo = ApprovalRepo::new(&state.db);
    let pending_approvals = repo.count_pending(company_id).await.unwrap_or(0);
    let paused_agents = AgentRepo::new(&state.db)
        .count_paused_for_company(company_id)
        .await
        .unwrap_or(0);
    let paused_projects = ProjectRepo::new(&state.db)
        .count_paused(company_id)
        .await
        .unwrap_or(0);
    Ok(Json(json!({
        "activeIncidents": [],
        "pendingApprovalCount": pending_approvals,
        "pausedAgentCount": paused_agents,
        "pausedProjectCount": paused_projects,
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
    let (id, budget_monthly_cents) = CompanyRepo::new(&state.db)
        .set_budget(company_id, body.budget_monthly_cents)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("company {company_id}")))?;
    Ok(Json(json!({
        "id": id,
        "budgetMonthlyCents": budget_monthly_cents
    })))
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
    let (id, company_id_ret, budget_monthly_cents, spent_monthly_cents) = AgentRepo::new(&state.db)
        .set_budget(agent_id, body.budget_monthly_cents)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("agent {agent_id}")))?;
    Ok(Json(json!({
        "id": id,
        "companyId": company_id_ret,
        "budgetMonthlyCents": budget_monthly_cents,
        "spentMonthlyCents": spent_monthly_cents
    })))
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

async fn issue_cost_summary(
    State(state): State<AppState>,
    Path(issue_id): Path<Uuid>,
    Query(query): Query<IssueCostQuery>,
) -> ApiResult<Json<IssueCostSummary>> {
    let row = CostRepo::new(&state.db)
        .issue_summary(issue_id)
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
#[allow(dead_code)]
struct IssueCostQuery {
    #[serde(default)]
    exclude_root: bool,
}

// ===== Round 194: budget policies & incidents =====

async fn list_budget_policies(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Vec<PolicyRow>>> {
    if !CompanyRepo::new(&state.db).exists(company_id).await? {
        return Err(ApiError::NotFound(format!("company {company_id}")));
    }
    let rows = BudgetRepo::new(&state.db).list_policies(company_id).await?;
    Ok(Json(rows))
}

async fn upsert_budget_policy(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(input): Json<UpsertPolicyInput>,
) -> ApiResult<impl IntoResponse> {
    if !CompanyRepo::new(&state.db).exists(company_id).await? {
        return Err(ApiError::NotFound(format!("company {company_id}")));
    }
    if input.amount < 0 {
        return Err(ApiError::BadRequest("amount must be non-negative".into()));
    }
    if input.warn_percent < 1 || input.warn_percent > 99 {
        return Err(ApiError::BadRequest("warn_percent must be 1-99".into()));
    }
    let row = BudgetRepo::new(&state.db)
        .upsert_policy(company_id, &input)
        .await?;
    Ok((StatusCode::OK, Json(row)))
}

async fn resolve_budget_incident(
    State(state): State<AppState>,
    Path((company_id, incident_id)): Path<(Uuid, Uuid)>,
    Json(input): Json<ResolveIncidentInput>,
) -> ApiResult<Json<IncidentRow>> {
    if !CompanyRepo::new(&state.db).exists(company_id).await? {
        return Err(ApiError::NotFound(format!("company {company_id}")));
    }
    let row = BudgetRepo::new(&state.db)
        .resolve_incident(company_id, incident_id, &input)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("incident {incident_id}")))?;
    Ok(Json(row))
}

// ===== Round 194: finance events =====

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct FinanceEventBody {
    amount_cents: i64,
    biller: String,
    event_kind: String,
    direction: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    occurred_at: Option<DateTime<Utc>>,
}

async fn create_finance_event(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<FinanceEventBody>,
) -> ApiResult<impl IntoResponse> {
    if !CompanyRepo::new(&state.db).exists(company_id).await? {
        return Err(ApiError::NotFound(format!("company {company_id}")));
    }
    if body.amount_cents < 0 {
        return Err(ApiError::BadRequest(
            "amount_cents must be non-negative".into(),
        ));
    }
    let id = Uuid::new_v4();
    let occurred = body.occurred_at.unwrap_or_else(Utc::now);
    sqlx::query(
        "INSERT INTO finance_events             (id, company_id, amount_cents, biller, event_kind, direction, description, occurred_at)          VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(id)
    .bind(company_id)
    .bind(body.amount_cents)
    .bind(&body.biller)
    .bind(&body.event_kind)
    .bind(&body.direction)
    .bind(body.description.as_deref())
    .bind(occurred)
    .execute(state.db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    let row = json!({
        "id": id,
        "companyId": company_id,
        "amountCents": body.amount_cents,
        "biller": body.biller,
        "eventKind": body.event_kind,
        "direction": body.direction,
        "description": body.description,
        "occurredAt": occurred,
    });
    Ok((StatusCode::CREATED, Json(row)))
}
