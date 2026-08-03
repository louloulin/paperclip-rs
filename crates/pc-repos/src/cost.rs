//! 成本与财务事件仓储。

use chrono::{DateTime, Utc};
use pc_core::Timestamp;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use crate::Db;

#[derive(Debug, Clone, Copy)]
pub struct CostRange {
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct CostSummary {
    pub company_id: Uuid,
    pub spend_cents: i64,
    pub budget_cents: i32,
    pub utilization_percent: f64,
}

#[derive(Debug, Clone, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct CostByAgent {
    pub agent_id: Uuid,
    pub agent_name: Option<String>,
    pub agent_status: Option<String>,
    pub cost_cents: i64,
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
    pub output_tokens: i64,
    pub api_run_count: i64,
    pub subscription_run_count: i64,
    pub subscription_cached_input_tokens: i64,
    pub subscription_input_tokens: i64,
    pub subscription_output_tokens: i64,
}

#[derive(Debug, Clone, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct CostByProviderModel {
    pub provider: String,
    pub biller: String,
    pub billing_type: String,
    pub model: String,
    pub cost_cents: i64,
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
    pub output_tokens: i64,
    pub api_run_count: i64,
    pub subscription_run_count: i64,
    pub subscription_cached_input_tokens: i64,
    pub subscription_input_tokens: i64,
    pub subscription_output_tokens: i64,
}

#[derive(Debug, Clone, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct CostByBiller {
    pub biller: String,
    pub cost_cents: i64,
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
    pub output_tokens: i64,
    pub api_run_count: i64,
    pub subscription_run_count: i64,
    pub subscription_cached_input_tokens: i64,
    pub subscription_input_tokens: i64,
    pub subscription_output_tokens: i64,
    pub provider_count: i64,
    pub model_count: i64,
}

#[derive(Debug, Clone, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct CostByAgentModel {
    pub agent_id: Uuid,
    pub agent_name: Option<String>,
    pub provider: String,
    pub biller: String,
    pub billing_type: String,
    pub model: String,
    pub cost_cents: i64,
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
    pub output_tokens: i64,
}

#[derive(Debug, Clone, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct CostByProject {
    pub project_id: Option<Uuid>,
    pub project_name: Option<String>,
    pub cost_cents: i64,
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
    pub output_tokens: i64,
}

#[derive(Debug, Clone, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct CostWindowSpendRow {
    pub provider: String,
    pub biller: String,
    pub window: String,
    pub window_hours: i32,
    pub cost_cents: i64,
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
    pub output_tokens: i64,
}

#[derive(Debug, Clone, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct FinanceSummary {
    pub company_id: Uuid,
    pub debit_cents: i64,
    pub credit_cents: i64,
    pub net_cents: i64,
    pub estimated_debit_cents: i64,
    pub event_count: i64,
}

#[derive(Debug, Clone, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct FinanceByBiller {
    pub biller: String,
    pub debit_cents: i64,
    pub credit_cents: i64,
    pub net_cents: i64,
    pub estimated_debit_cents: i64,
    pub event_count: i64,
    pub kind_count: i64,
}

#[derive(Debug, Clone, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct FinanceByKind {
    pub event_kind: String,
    pub debit_cents: i64,
    pub credit_cents: i64,
    pub net_cents: i64,
    pub estimated_debit_cents: i64,
    pub event_count: i64,
    pub biller_count: i64,
}

#[derive(Debug, Clone, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct FinanceEventRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub agent_id: Option<Uuid>,
    pub issue_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub goal_id: Option<Uuid>,
    pub heartbeat_run_id: Option<Uuid>,
    pub cost_event_id: Option<Uuid>,
    pub billing_code: Option<String>,
    pub description: Option<String>,
    pub event_kind: String,
    pub direction: String,
    pub biller: String,
    pub provider: Option<String>,
    pub execution_adapter_type: Option<String>,
    pub pricing_tier: Option<String>,
    pub region: Option<String>,
    pub model: Option<String>,
    pub quantity: Option<i32>,
    pub unit: Option<String>,
    pub amount_cents: i32,
    pub currency: String,
    pub estimated: bool,
    pub external_invoice_id: Option<String>,
    pub metadata_json: Option<serde_json::Value>,
    pub occurred_at: Timestamp,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateCostEvent {
    pub agent_id: Uuid,
    pub issue_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub goal_id: Option<Uuid>,
    pub heartbeat_run_id: Option<Uuid>,
    pub billing_code: Option<String>,
    pub provider: String,
    #[serde(default = "default_biller")]
    pub biller: String,
    #[serde(default = "default_billing_type")]
    pub billing_type: String,
    pub model: String,
    #[serde(default)]
    pub input_tokens: i32,
    #[serde(default)]
    pub cached_input_tokens: i32,
    #[serde(default)]
    pub output_tokens: i32,
    pub cost_cents: i32,
    pub occurred_at: DateTime<Utc>,
}

fn default_biller() -> String {
    "unknown".to_owned()
}

fn default_billing_type() -> String {
    "unknown".to_owned()
}

pub struct CostRepo<'a> {
    pub db: &'a Db,
}

impl<'a> CostRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    pub async fn summary(&self, company_id: Uuid, range: CostRange) -> sqlx::Result<CostSummary> {
        sqlx::query_as::<_, CostSummary>(
            "SELECT c.id AS company_id, COALESCE(SUM(ce.cost_cents), 0)::bigint AS spend_cents, \
                    c.budget_monthly_cents AS budget_cents, \
                    CASE WHEN c.budget_monthly_cents > 0 \
                         THEN COALESCE(SUM(ce.cost_cents), 0)::double precision / c.budget_monthly_cents * 100 \
                         ELSE 0 END AS utilization_percent \
             FROM companies c LEFT JOIN cost_events ce ON ce.company_id = c.id \
               AND ($2::timestamptz IS NULL OR ce.occurred_at >= $2) \
               AND ($3::timestamptz IS NULL OR ce.occurred_at <= $3) \
             WHERE c.id = $1 GROUP BY c.id, c.budget_monthly_cents",
        )
        .bind(company_id)
        .bind(range.from)
        .bind(range.to)
        .fetch_one(self.db.pool())
        .await
    }

    pub async fn create_event(
        &self,
        company_id: Uuid,
        input: &CreateCostEvent,
    ) -> sqlx::Result<serde_json::Value> {
        let row: (Uuid,) = sqlx::query_as(
            "INSERT INTO cost_events \
                (company_id, agent_id, issue_id, project_id, goal_id, heartbeat_run_id, billing_code, \
                 provider, biller, billing_type, model, input_tokens, cached_input_tokens, output_tokens, cost_cents, occurred_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16) RETURNING id",
        )
        .bind(company_id)
        .bind(input.agent_id)
        .bind(input.issue_id)
        .bind(input.project_id)
        .bind(input.goal_id)
        .bind(input.heartbeat_run_id)
        .bind(&input.billing_code)
        .bind(&input.provider)
        .bind(&input.biller)
        .bind(&input.billing_type)
        .bind(&input.model)
        .bind(input.input_tokens)
        .bind(input.cached_input_tokens)
        .bind(input.output_tokens)
        .bind(input.cost_cents)
        .bind(input.occurred_at)
        .fetch_one(self.db.pool())
        .await?;
        Ok(serde_json::json!({ "id": row.0, "companyId": company_id }))
    }

    pub async fn by_agent(
        &self,
        company_id: Uuid,
        range: CostRange,
    ) -> sqlx::Result<Vec<CostByAgent>> {
        sqlx::query_as::<_, CostByAgent>(
            "SELECT ce.agent_id, a.name AS agent_name, a.status AS agent_status, \
                    COALESCE(SUM(ce.cost_cents),0)::bigint AS cost_cents, \
                    COALESCE(SUM(ce.input_tokens),0)::bigint AS input_tokens, \
                    COALESCE(SUM(ce.cached_input_tokens),0)::bigint AS cached_input_tokens, \
                    COALESCE(SUM(ce.output_tokens),0)::bigint AS output_tokens, \
                    COUNT(*) FILTER (WHERE ce.billing_type = 'api')::bigint AS api_run_count, \
                    COUNT(*) FILTER (WHERE ce.billing_type = 'subscription')::bigint AS subscription_run_count, \
                    COALESCE(SUM(ce.cached_input_tokens) FILTER (WHERE ce.billing_type = 'subscription'),0)::bigint AS subscription_cached_input_tokens, \
                    COALESCE(SUM(ce.input_tokens) FILTER (WHERE ce.billing_type = 'subscription'),0)::bigint AS subscription_input_tokens, \
                    COALESCE(SUM(ce.output_tokens) FILTER (WHERE ce.billing_type = 'subscription'),0)::bigint AS subscription_output_tokens \
             FROM cost_events ce LEFT JOIN agents a ON a.id = ce.agent_id \
             WHERE ce.company_id = $1 AND ($2::timestamptz IS NULL OR ce.occurred_at >= $2) \
               AND ($3::timestamptz IS NULL OR ce.occurred_at <= $3) \
             GROUP BY ce.agent_id, a.name, a.status ORDER BY cost_cents DESC",
        )
        .bind(company_id)
        .bind(range.from)
        .bind(range.to)
        .fetch_all(self.db.pool())
        .await
    }

    pub async fn by_agent_model(
        &self,
        company_id: Uuid,
        range: CostRange,
    ) -> sqlx::Result<Vec<CostByAgentModel>> {
        sqlx::query_as::<_, CostByAgentModel>(
            "SELECT ce.agent_id, a.name AS agent_name, ce.provider, ce.biller, ce.billing_type, ce.model, \
                    COALESCE(SUM(ce.cost_cents),0)::bigint AS cost_cents, \
                    COALESCE(SUM(ce.input_tokens),0)::bigint AS input_tokens, \
                    COALESCE(SUM(ce.cached_input_tokens),0)::bigint AS cached_input_tokens, \
                    COALESCE(SUM(ce.output_tokens),0)::bigint AS output_tokens \
             FROM cost_events ce LEFT JOIN agents a ON a.id = ce.agent_id \
             WHERE ce.company_id = $1 AND ($2::timestamptz IS NULL OR ce.occurred_at >= $2) \
               AND ($3::timestamptz IS NULL OR ce.occurred_at <= $3) \
             GROUP BY ce.agent_id, a.name, ce.provider, ce.biller, ce.billing_type, ce.model \
             ORDER BY cost_cents DESC",
        )
        .bind(company_id)
        .bind(range.from)
        .bind(range.to)
        .fetch_all(self.db.pool())
        .await
    }

    pub async fn by_provider(
        &self,
        company_id: Uuid,
        range: CostRange,
    ) -> sqlx::Result<Vec<CostByProviderModel>> {
        sqlx::query_as::<_, CostByProviderModel>(
            "SELECT ce.provider, ce.biller, ce.billing_type, ce.model, \
                    COALESCE(SUM(ce.cost_cents),0)::bigint AS cost_cents, \
                    COALESCE(SUM(ce.input_tokens),0)::bigint AS input_tokens, \
                    COALESCE(SUM(ce.cached_input_tokens),0)::bigint AS cached_input_tokens, \
                    COALESCE(SUM(ce.output_tokens),0)::bigint AS output_tokens, \
                    COUNT(*) FILTER (WHERE ce.billing_type = 'api')::bigint AS api_run_count, \
                    COUNT(*) FILTER (WHERE ce.billing_type = 'subscription')::bigint AS subscription_run_count, \
                    COALESCE(SUM(ce.cached_input_tokens) FILTER (WHERE ce.billing_type = 'subscription'),0)::bigint AS subscription_cached_input_tokens, \
                    COALESCE(SUM(ce.input_tokens) FILTER (WHERE ce.billing_type = 'subscription'),0)::bigint AS subscription_input_tokens, \
                    COALESCE(SUM(ce.output_tokens) FILTER (WHERE ce.billing_type = 'subscription'),0)::bigint AS subscription_output_tokens \
             FROM cost_events ce WHERE ce.company_id = $1 \
               AND ($2::timestamptz IS NULL OR ce.occurred_at >= $2) \
               AND ($3::timestamptz IS NULL OR ce.occurred_at <= $3) \
             GROUP BY ce.provider, ce.biller, ce.billing_type, ce.model ORDER BY cost_cents DESC",
        )
        .bind(company_id)
        .bind(range.from)
        .bind(range.to)
        .fetch_all(self.db.pool())
        .await
    }

    pub async fn by_biller(
        &self,
        company_id: Uuid,
        range: CostRange,
    ) -> sqlx::Result<Vec<CostByBiller>> {
        sqlx::query_as::<_, CostByBiller>(
            "SELECT ce.biller, COALESCE(SUM(ce.cost_cents),0)::bigint AS cost_cents, \
                    COALESCE(SUM(ce.input_tokens),0)::bigint AS input_tokens, \
                    COALESCE(SUM(ce.cached_input_tokens),0)::bigint AS cached_input_tokens, \
                    COALESCE(SUM(ce.output_tokens),0)::bigint AS output_tokens, \
                    COUNT(*) FILTER (WHERE ce.billing_type = 'api')::bigint AS api_run_count, \
                    COUNT(*) FILTER (WHERE ce.billing_type = 'subscription')::bigint AS subscription_run_count, \
                    COALESCE(SUM(ce.cached_input_tokens) FILTER (WHERE ce.billing_type = 'subscription'),0)::bigint AS subscription_cached_input_tokens, \
                    COALESCE(SUM(ce.input_tokens) FILTER (WHERE ce.billing_type = 'subscription'),0)::bigint AS subscription_input_tokens, \
                    COALESCE(SUM(ce.output_tokens) FILTER (WHERE ce.billing_type = 'subscription'),0)::bigint AS subscription_output_tokens, \
                    COUNT(DISTINCT ce.provider)::bigint AS provider_count, COUNT(DISTINCT ce.model)::bigint AS model_count \
             FROM cost_events ce WHERE ce.company_id = $1 \
               AND ($2::timestamptz IS NULL OR ce.occurred_at >= $2) \
               AND ($3::timestamptz IS NULL OR ce.occurred_at <= $3) \
             GROUP BY ce.biller ORDER BY cost_cents DESC",
        )
        .bind(company_id)
        .bind(range.from)
        .bind(range.to)
        .fetch_all(self.db.pool())
        .await
    }

    pub async fn by_project(
        &self,
        company_id: Uuid,
        range: CostRange,
    ) -> sqlx::Result<Vec<CostByProject>> {
        sqlx::query_as::<_, CostByProject>(
            "SELECT ce.project_id, p.name AS project_name, COALESCE(SUM(ce.cost_cents),0)::bigint AS cost_cents, \
                    COALESCE(SUM(ce.input_tokens),0)::bigint AS input_tokens, \
                    COALESCE(SUM(ce.cached_input_tokens),0)::bigint AS cached_input_tokens, \
                    COALESCE(SUM(ce.output_tokens),0)::bigint AS output_tokens \
             FROM cost_events ce LEFT JOIN projects p ON p.id = ce.project_id \
             WHERE ce.company_id = $1 AND ($2::timestamptz IS NULL OR ce.occurred_at >= $2) \
               AND ($3::timestamptz IS NULL OR ce.occurred_at <= $3) \
             GROUP BY ce.project_id, p.name ORDER BY cost_cents DESC",
        )
        .bind(company_id)
        .bind(range.from)
        .bind(range.to)
        .fetch_all(self.db.pool())
        .await
    }

    pub async fn window_spend(&self, company_id: Uuid) -> sqlx::Result<Vec<CostWindowSpendRow>> {
        sqlx::query_as::<_, CostWindowSpendRow>(
            "SELECT ce.provider, ce.biller, windows.window, windows.window_hours, \
                    COALESCE(SUM(ce.cost_cents),0)::bigint AS cost_cents, \
                    COALESCE(SUM(ce.input_tokens),0)::bigint AS input_tokens, \
                    COALESCE(SUM(ce.cached_input_tokens),0)::bigint AS cached_input_tokens, \
                    COALESCE(SUM(ce.output_tokens),0)::bigint AS output_tokens \
             FROM (VALUES ('5h', 5), ('24h', 24), ('7d', 168)) AS windows(window, window_hours) \
             JOIN cost_events ce ON ce.company_id = $1 \
               AND ce.occurred_at >= now() - (windows.window_hours * interval '1 hour') \
             GROUP BY ce.provider, ce.biller, windows.window, windows.window_hours \
             ORDER BY windows.window_hours, cost_cents DESC",
        )
        .bind(company_id)
        .fetch_all(self.db.pool())
        .await
    }

    pub async fn finance_summary(
        &self,
        company_id: Uuid,
        range: CostRange,
    ) -> sqlx::Result<FinanceSummary> {
        sqlx::query_as::<_, FinanceSummary>(
            "SELECT $1::uuid AS company_id, \
                    COALESCE(SUM(amount_cents) FILTER (WHERE direction = 'debit'),0)::bigint AS debit_cents, \
                    COALESCE(SUM(amount_cents) FILTER (WHERE direction = 'credit'),0)::bigint AS credit_cents, \
                    COALESCE(SUM(CASE WHEN direction = 'debit' THEN amount_cents ELSE -amount_cents END),0)::bigint AS net_cents, \
                    COALESCE(SUM(amount_cents) FILTER (WHERE direction = 'debit' AND estimated),0)::bigint AS estimated_debit_cents, \
                    COUNT(*)::bigint AS event_count FROM finance_events \
             WHERE company_id = $1 AND ($2::timestamptz IS NULL OR occurred_at >= $2) \
               AND ($3::timestamptz IS NULL OR occurred_at <= $3)",
        )
        .bind(company_id)
        .bind(range.from)
        .bind(range.to)
        .fetch_one(self.db.pool())
        .await
    }

    pub async fn finance_by_biller(
        &self,
        company_id: Uuid,
        range: CostRange,
    ) -> sqlx::Result<Vec<FinanceByBiller>> {
        sqlx::query_as::<_, FinanceByBiller>(
            "SELECT biller, COALESCE(SUM(amount_cents) FILTER (WHERE direction='debit'),0)::bigint AS debit_cents, \
                    COALESCE(SUM(amount_cents) FILTER (WHERE direction='credit'),0)::bigint AS credit_cents, \
                    COALESCE(SUM(CASE WHEN direction='debit' THEN amount_cents ELSE -amount_cents END),0)::bigint AS net_cents, \
                    COALESCE(SUM(amount_cents) FILTER (WHERE direction='debit' AND estimated),0)::bigint AS estimated_debit_cents, \
                    COUNT(*)::bigint AS event_count, COUNT(DISTINCT event_kind)::bigint AS kind_count \
             FROM finance_events WHERE company_id=$1 AND ($2::timestamptz IS NULL OR occurred_at >= $2) \
               AND ($3::timestamptz IS NULL OR occurred_at <= $3) GROUP BY biller ORDER BY net_cents DESC",
        )
        .bind(company_id)
        .bind(range.from)
        .bind(range.to)
        .fetch_all(self.db.pool())
        .await
    }

    pub async fn finance_by_kind(
        &self,
        company_id: Uuid,
        range: CostRange,
    ) -> sqlx::Result<Vec<FinanceByKind>> {
        sqlx::query_as::<_, FinanceByKind>(
            "SELECT event_kind, COALESCE(SUM(amount_cents) FILTER (WHERE direction='debit'),0)::bigint AS debit_cents, \
                    COALESCE(SUM(amount_cents) FILTER (WHERE direction='credit'),0)::bigint AS credit_cents, \
                    COALESCE(SUM(CASE WHEN direction='debit' THEN amount_cents ELSE -amount_cents END),0)::bigint AS net_cents, \
                    COALESCE(SUM(amount_cents) FILTER (WHERE direction='debit' AND estimated),0)::bigint AS estimated_debit_cents, \
                    COUNT(*)::bigint AS event_count, COUNT(DISTINCT biller)::bigint AS biller_count \
             FROM finance_events WHERE company_id=$1 AND ($2::timestamptz IS NULL OR occurred_at >= $2) \
               AND ($3::timestamptz IS NULL OR occurred_at <= $3) GROUP BY event_kind ORDER BY net_cents DESC",
        )
        .bind(company_id)
        .bind(range.from)
        .bind(range.to)
        .fetch_all(self.db.pool())
        .await
    }

    pub async fn finance_events(
        &self,
        company_id: Uuid,
        range: CostRange,
        limit: i64,
    ) -> sqlx::Result<Vec<FinanceEventRow>> {
        let query = sqlx::query_as::<_, FinanceEventRow>(
            "SELECT id, company_id, agent_id, issue_id, project_id, goal_id, heartbeat_run_id, cost_event_id, \
                    billing_code, description, event_kind, direction, biller, provider, execution_adapter_type, \
                    pricing_tier, region, model, quantity, unit, amount_cents, currency, estimated, external_invoice_id, \
                    metadata_json, occurred_at, created_at FROM finance_events \
             WHERE company_id=$1 AND ($2::timestamptz IS NULL OR occurred_at >= $2) \
               AND ($3::timestamptz IS NULL OR occurred_at <= $3) \
             ORDER BY occurred_at DESC LIMIT $4",
        )
        .bind(company_id)
        .bind(range.from)
        .bind(range.to)
        .bind(limit.clamp(1, 500));
        query.fetch_all(self.db.pool()).await
    }
}

/// Daily cost window for a single agent. Returns the sum of `cost_cents` for
/// the agent in `[window_start, window_end)`. Mirrors the Node-side
/// `currentUtcDayWindow` contract used by `getHeartbeatDailyCapBlock`.
#[derive(Debug, Clone, Copy)]
pub struct AgentCostWindow {
    pub company_id: Uuid,
    pub agent_id: Uuid,
    pub window_start: DateTime<Utc>,
    pub window_end: DateTime<Utc>,
}

impl<'a> CostRepo<'a> {
    /// Sum of `cost_cents` for one agent in `[window_start, window_end)`.
    /// Used by the heartbeat scheduler to enforce the per-agent daily cost cap.
    pub async fn sum_agent_window_cost_cents(
        &self,
        window: AgentCostWindow,
    ) -> sqlx::Result<i64> {
        let row: (Option<i64>,) = sqlx::query_as(
            "SELECT COALESCE(SUM(cost_cents), 0)::bigint FROM cost_events \
             WHERE company_id = $1 AND agent_id = $2 \
               AND occurred_at >= $3 AND occurred_at < $4",
        )
        .bind(window.company_id)
        .bind(window.agent_id)
        .bind(window.window_start)
        .bind(window.window_end)
        .fetch_one(self.db.pool())
        .await?;
        Ok(row.0.unwrap_or(0))
    }
}
