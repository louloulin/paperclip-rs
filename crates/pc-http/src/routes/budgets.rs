//! `/api/companies/:company_id/budgets*` 与 `/api/agents/:agent_id/budgets` 路由：
//! 通过 `pc_budgets::BudgetService` 暴露 budget policy / incident 生命周期。
//!
//! 设计目标：
//! - 高内聚：所有 budget HTTP 入口集中在一处
//! - 低耦合：业务逻辑委托给 `BudgetService`（与上游 `services/budgets.ts` 对齐）
//! - 与 `routes::approvals` 协同：hire_agent 决策触发的 budget policy 由
//!   `DbHireAgentOps` 直接写库；这里提供查询/更新/解决 incident 端点

use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use pc_auth::AuthContext;
use pc_budgets::{
    BudgetPolicyStatus, BudgetService, BudgetThresholdType, BudgetWindowKind, FullEvaluation,
    IncidentOutcome,
};
use pc_repos::budget::{ResolveIncidentInput, UpsertPolicyInput};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use crate::{ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        // Policies
        .route(
            "/api/companies/:company_id/budgets/policies",
            get(list_policies).post(upsert_policy),
        )
        // Incidents
        .route(
            "/api/companies/:company_id/budget-incidents",
            get(list_incidents),
        )
        .route(
            "/api/companies/:company_id/budget-incidents/:incident_id/resolve",
            post(resolve_incident),
        )
        // Overview（聚合）
        .route(
            "/api/companies/:company_id/budgets/overview",
            get(budgets_overview),
        )
    // Agent 维度的 `/api/agents/:agent_id/budgets` 注册在 routes::agents
    // (Round 282 removal — 重复注册会触发 axum 0.7 的
    // "Overlapping method route" panic)
}

// =============================================================================
// Handlers
// =============================================================================

async fn list_policies(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let svc = BudgetService::new(&state.db);
    let rows = svc
        .list_policies(company_id)
        .await
        .map_err(map_budget_error)?;
    Ok(Json(json!({
        "companyId": company_id,
        "policies": rows,
        "items": rows,
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpsertPolicyBody {
    pub scope_type: String,
    pub scope_id: Uuid,
    #[serde(default = "default_metric")]
    pub metric: String,
    #[serde(default = "default_window_kind")]
    pub window_kind: String,
    pub amount: i64,
    #[serde(default = "default_warn_percent")]
    pub warn_percent: i32,
    #[serde(default)]
    pub hard_stop_enabled: bool,
    #[serde(default = "default_true")]
    pub notify_enabled: bool,
    #[serde(default = "default_true")]
    pub is_active: bool,
    #[serde(default)]
    pub updated_by_user_id: Option<String>,
}

fn default_metric() -> String {
    "billed_cents".into()
}
fn default_window_kind() -> String {
    "calendar_month_utc".into()
}
fn default_warn_percent() -> i32 {
    80
}
fn default_true() -> bool {
    true
}

async fn upsert_policy(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    _actor: axum::Extension<AuthContext>,
    Json(body): Json<UpsertPolicyBody>,
) -> ApiResult<Json<Value>> {
    let input = UpsertPolicyInput {
        scope_type: body.scope_type.clone(),
        scope_id: body.scope_id,
        metric: body.metric,
        window_kind: body.window_kind.clone(),
        amount: body.amount as i32,
        warn_percent: body.warn_percent,
        hard_stop_enabled: body.hard_stop_enabled,
        notify_enabled: body.notify_enabled,
        is_active: body.is_active,
        updated_by_user_id: body.updated_by_user_id.clone(),
    };
    let svc = BudgetService::new(&state.db);
    let row = svc
        .upsert_policy(company_id, input)
        .await
        .map_err(map_budget_error)?;
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn list_incidents(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let svc = BudgetService::new(&state.db);
    let rows = svc
        .list_incidents(company_id)
        .await
        .map_err(map_budget_error)?;
    Ok(Json(json!({
        "companyId": company_id,
        "incidents": rows,
        "items": rows,
    })))
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ResolveIncidentBody {
    #[serde(default)]
    pub resolution_note: Option<String>,
    #[serde(default)]
    pub resolution_kind: Option<String>,
    #[serde(default)]
    pub resolved_by_user_id: Option<String>,
}

async fn resolve_incident(
    State(state): State<AppState>,
    Path((company_id, incident_id)): Path<(Uuid, Uuid)>,
    _actor: axum::Extension<AuthContext>,
    Json(body): Json<ResolveIncidentBody>,
) -> ApiResult<Json<Value>> {
    let input = ResolveIncidentInput {
        action: body
            .resolution_kind
            .clone()
            .unwrap_or_else(|| "dismissed".into()),
        amount: None,
        decision_note: body.resolution_note.clone(),
    };
    let svc = BudgetService::new(&state.db);
    let row = svc
        .resolve_incident(company_id, incident_id, input)
        .await
        .map_err(map_budget_error)?;
    match row {
        Some(r) => Ok(Json(serde_json::to_value(r).unwrap_or_default())),
        None => Err(ApiError::NotFound(format!("incident {incident_id}"))),
    }
}

/// Budget overview: 各 policy 当前窗口 + 状态统计。
async fn budgets_overview(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let svc = BudgetService::new(&state.db);
    let policies = svc
        .list_policies(company_id)
        .await
        .map_err(map_budget_error)?;
    let incidents = svc
        .list_incidents(company_id)
        .await
        .map_err(map_budget_error)?;
    let open = incidents.iter().filter(|i| i.status != "dismissed").count();
    let warning_count = incidents
        .iter()
        .filter(|i| i.threshold_type == "warning")
        .count();
    let hard_stop_count = incidents
        .iter()
        .filter(|i| i.threshold_type == "hard_stop")
        .count();
    Ok(Json(json!({
        "companyId": company_id,
        "policies": policies,
        "incidents": incidents,
        "summary": {
            "policyCount": policies.len(),
            "incidentCount": incidents.len(),
            "openIncidentCount": open,
            "warningCount": warning_count,
            "hardStopCount": hard_stop_count,
        }
    })))
}

/// Agent 维度的预算：返回 policy + 当前窗口 evaluated status。
///
/// 实际策略：
/// 1. 查 agent 所属 company
/// 2. 列出该公司所有 scope_id=agent 的 policies
/// 3. 对每个 policy 计算 observed = 当前公司 cost 总和（简化）
/// 4. 调 `evaluate_full` 拿 status + 触发 hook（如果配置）
async fn agent_budgets(
    State(state): State<AppState>,
    Path(agent_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    // 查 agent → company
    let agent: Option<(Uuid,)> = sqlx::query_as("SELECT company_id FROM agents WHERE id = $1")
        .bind(agent_id)
        .fetch_optional(state.db.pool())
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let company_id = agent
        .ok_or_else(|| ApiError::NotFound(format!("agent {agent_id}")))?
        .0;

    let svc = BudgetService::new(&state.db);
    let all_policies = svc
        .list_policies(company_id)
        .await
        .map_err(map_budget_error)?;
    let agent_policies: Vec<_> = all_policies
        .into_iter()
        .filter(|p| p.scope_id == agent_id && p.is_active)
        .collect();

    let now = chrono::Utc::now();
    let mut evaluations = Vec::with_capacity(agent_policies.len());
    for policy in agent_policies {
        let observed = observed_for_agent(&state.db, agent_id, &policy, now).await?;
        let eval = svc
            .evaluate_full(&policy, observed, now)
            .await
            .map_err(map_budget_error)?;
        evaluations.push(json!({
            "policyId": policy.id,
            "scopeType": policy.scope_type,
            "scopeId": policy.scope_id,
            "evaluation": eval,
        }));
    }
    Ok(Json(json!({
        "agentId": agent_id,
        "companyId": company_id,
        "evaluations": evaluations,
    })))
}

async fn observed_for_agent(
    db: &pc_db::Db,
    agent_id: Uuid,
    policy: &pc_repos::budget::PolicyRow,
    now: chrono::DateTime<chrono::Utc>,
) -> ApiResult<i64> {
    // 简化实现：从 cost_events 聚合 metric 对应字段（cost_cents）。
    // 真实生产应该按 policy.metric（billed_cents 等）取对应字段，
    // 这里统一用 cost_cents 占位以满足 evaluate_full 签名。
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT COALESCE(SUM(cost_cents), 0)::bigint FROM cost_events          WHERE agent_id = $1 AND occurred_at >= $2",
    )
    .bind(agent_id)
    .bind(policy_window_start(policy, now))
    .fetch_optional(db.pool())
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(row.map(|(v,)| v).unwrap_or(0))
}

fn policy_window_start(
    policy: &pc_repos::budget::PolicyRow,
    now: chrono::DateTime<chrono::Utc>,
) -> chrono::DateTime<chrono::Utc> {
    pc_budgets::compute_window(parse_window(&policy.window_kind), now).start
}

fn parse_window(s: &str) -> pc_budgets::BudgetWindowKind {
    pc_budgets::BudgetWindowKind::parse(s).unwrap_or(pc_budgets::BudgetWindowKind::CalendarMonthUtc)
}

// =============================================================================
// 错误映射
// =============================================================================

fn map_budget_error(e: pc_budgets::BudgetError) -> ApiError {
    use pc_budgets::BudgetError;
    match e {
        BudgetError::NotFound(m) => ApiError::NotFound(m),
        BudgetError::InvalidWindowKind(m) => {
            ApiError::BadRequest(format!("invalid window kind: {m}"))
        }
        BudgetError::InvalidScopeType(m) => {
            ApiError::BadRequest(format!("invalid scope type: {m}"))
        }
        BudgetError::Repo(m) => ApiError::Internal(format!("budget repo error: {m}")),
        BudgetError::Hook(m) => ApiError::Internal(format!("budget hook error: {m}")),
    }
}

// 类型位置引用，避免 dead_code 误报（rustc 看见 pub use 也会触发）
#[allow(dead_code)]
fn _ensure_imports_used(
    _w: BudgetWindowKind,
    _s: BudgetPolicyStatus,
    _t: BudgetThresholdType,
    _o: IncidentOutcome,
    _e: FullEvaluation,
    _a: Arc<()>,
) {
}
