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

/// Insert payload for `finance_events`. Mirrors Node
/// `paperclip/server/src/services/finance.ts::createEvent`.
/// All FK columns are optional; if set, the FK row must belong to the same company.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct NewFinanceEvent {
    pub agent_id: Option<Uuid>,
    pub issue_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub goal_id: Option<Uuid>,
    pub heartbeat_run_id: Option<Uuid>,
    pub cost_event_id: Option<Uuid>,
    pub billing_code: Option<String>,
    pub description: Option<String>,
    pub event_kind: String,
    pub direction: Option<String>,
    pub biller: String,
    pub provider: Option<String>,
    pub execution_adapter_type: Option<String>,
    pub pricing_tier: Option<String>,
    pub region: Option<String>,
    pub model: Option<String>,
    pub quantity: Option<i32>,
    pub unit: Option<String>,
    pub amount_cents: i32,
    pub currency: Option<String>,
    pub estimated: Option<bool>,
    pub external_invoice_id: Option<String>,
    pub metadata_json: Option<serde_json::Value>,
    pub occurred_at: Option<Timestamp>,
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

/// Round 212: cost_events 表行（与 drizzle schema 1:1）。
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CostEventRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub agent_id: Uuid,
    pub issue_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub goal_id: Option<Uuid>,
    pub billing_code: Option<String>,
    pub provider: String,
    pub model: String,
    pub input_tokens: i32,
    pub output_tokens: i32,
    pub cost_cents: i32,
    pub occurred_at: Timestamp,
    pub created_at: Timestamp,
}


#[derive(Debug, sqlx::FromRow, serde::Serialize, serde::Deserialize)]
pub struct IssueCostSummaryRow {
    pub cost_cents: i64,
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
    pub output_tokens: i64,
    pub run_count: i64,
    pub runtime_ms: i64,
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
    /// Round 212: 列出 company 的 cost_events（按 occurred_at DESC）。
    pub async fn list_cost_events(
        &self,
        company_id: Uuid,
        limit: i64,
    ) -> sqlx::Result<Vec<CostEventRow>> {
        let rows = sqlx::query_as::<_, CostEventRow>(
            "SELECT id, company_id, agent_id, issue_id, project_id, goal_id, billing_code, \
                    provider, model, input_tokens, output_tokens, cost_cents, occurred_at, created_at \
             FROM cost_events WHERE company_id = $1 \
             ORDER BY occurred_at DESC LIMIT $2",
        )
        .bind(company_id)
        .bind(limit.clamp(1, 500))
        .fetch_all(self.db.pool())
        .await?;
        Ok(rows)
    }


    /// Round 176: 统计单个 issue 的成本/输入/输出/运行数/运行时间（聚合 issues+cost_events+heartbeat_runs）。
    pub async fn issue_summary(&self, issue_id: Uuid) -> sqlx::Result<Option<IssueCostSummaryRow>> {
        sqlx::query_as::<_, IssueCostSummaryRow>(
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
        .fetch_optional(self.db.pool())
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

    /// Insert a `finance_events` row.
    ///
    /// Mirrors `server/src/services/finance.ts::createEvent`:
    /// 1. Validate any provided FK (agent / issue / project / goal /
    ///    heartbeat_run / cost_event) belongs to the same company.
    /// 2. Defaults: `currency = "USD"`, `direction = "debit"`,
    ///    `estimated = false`, `occurred_at = now()`.
    /// 3. Return the inserted row.
    pub async fn create_finance_event(
        &self,
        company_id: Uuid,
        input: &NewFinanceEvent,
    ) -> Result<FinanceEventRow, FinanceCreateError> {
        if let Some(id) = input.agent_id {
            Self::assert_fk_belongs_to_company(
                self.db.pool(),
                "agents",
                id,
                company_id,
                "Agent",
            )
            .await
            .map_err(FinanceCreateError::Fk)?;
        }
        if let Some(id) = input.issue_id {
            Self::assert_fk_belongs_to_company(
                self.db.pool(),
                "issues",
                id,
                company_id,
                "Issue",
            )
            .await
            .map_err(FinanceCreateError::Fk)?;
        }
        if let Some(id) = input.project_id {
            Self::assert_fk_belongs_to_company(
                self.db.pool(),
                "projects",
                id,
                company_id,
                "Project",
            )
            .await
            .map_err(FinanceCreateError::Fk)?;
        }
        if let Some(id) = input.goal_id {
            Self::assert_fk_belongs_to_company(
                self.db.pool(),
                "goals",
                id,
                company_id,
                "Goal",
            )
            .await
            .map_err(FinanceCreateError::Fk)?;
        }
        if let Some(id) = input.heartbeat_run_id {
            Self::assert_fk_belongs_to_company(
                self.db.pool(),
                "heartbeat_runs",
                id,
                company_id,
                "Heartbeat run",
            )
            .await
            .map_err(FinanceCreateError::Fk)?;
        }
        if let Some(id) = input.cost_event_id {
            Self::assert_fk_belongs_to_company(
                self.db.pool(),
                "cost_events",
                id,
                company_id,
                "Cost event",
            )
            .await
            .map_err(FinanceCreateError::Fk)?;
        }

        let direction = input.direction.clone().unwrap_or_else(|| "debit".into());
        let currency = input.currency.clone().unwrap_or_else(|| "USD".into());
        let estimated = input.estimated.unwrap_or(false);
        let occurred_at = input.occurred_at.unwrap_or_else(Timestamp::now);

        let row: FinanceEventRow = sqlx::query_as(
            r#"
            INSERT INTO finance_events (
              company_id, agent_id, issue_id, project_id, goal_id,
              heartbeat_run_id, cost_event_id, billing_code, description,
              event_kind, direction, biller, provider, execution_adapter_type,
              pricing_tier, region, model, quantity, unit, amount_cents,
              currency, estimated, external_invoice_id, metadata_json, occurred_at
            ) VALUES (
              $1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,
              $21,$22,$23,$24,$25
            )
            RETURNING id, company_id, agent_id, issue_id, project_id, goal_id,
              heartbeat_run_id, cost_event_id, billing_code, description,
              event_kind, direction, biller, provider, execution_adapter_type,
              pricing_tier, region, model, quantity, unit, amount_cents,
              currency, estimated, external_invoice_id, metadata_json,
              occurred_at, created_at
            "#,
        )
        .bind(company_id)
        .bind(input.agent_id)
        .bind(input.issue_id)
        .bind(input.project_id)
        .bind(input.goal_id)
        .bind(input.heartbeat_run_id)
        .bind(input.cost_event_id)
        .bind(input.billing_code.as_deref())
        .bind(input.description.as_deref())
        .bind(&input.event_kind)
        .bind(&direction)
        .bind(&input.biller)
        .bind(input.provider.as_deref())
        .bind(input.execution_adapter_type.as_deref())
        .bind(input.pricing_tier.as_deref())
        .bind(input.region.as_deref())
        .bind(input.model.as_deref())
        .bind(input.quantity)
        .bind(input.unit.as_deref())
        .bind(input.amount_cents)
        .bind(&currency)
        .bind(estimated)
        .bind(input.external_invoice_id.as_deref())
        .bind(input.metadata_json.clone())
        .bind(occurred_at)
        .fetch_one(self.db.pool())
        .await
        .map_err(FinanceCreateError::Db)?;
        Ok(row)
    }

    /// Validate that a row in `table` with `id` belongs to `company_id`.
    /// Returns `FkError::NotFound` if the row is missing, or
    /// `FkError::WrongCompany` if `company_id` doesn't match.
    /// `table` is restricted to a small allow-list to prevent SQL injection.
    async fn assert_fk_belongs_to_company(
        pool: &sqlx::PgPool,
        table: &'static str,
        id: Uuid,
        company_id: Uuid,
        label: &str,
    ) -> Result<(), FkError> {
        let sql = match table {
            "agents" => "SELECT company_id FROM agents WHERE id = $1",
            "issues" => "SELECT company_id FROM issues WHERE id = $1",
            "projects" => "SELECT company_id FROM projects WHERE id = $1",
            "goals" => "SELECT company_id FROM goals WHERE id = $1",
            "heartbeat_runs" => {
                "SELECT company_id FROM heartbeat_runs WHERE id = $1"
            }
            "cost_events" => "SELECT company_id FROM cost_events WHERE id = $1",
            _ => return Err(FkError::Internal(format!("unknown table: {table}"))),
        };
        let row: Option<(Uuid,)> = sqlx::query_as(sql)
            .bind(id)
            .fetch_optional(pool)
            .await
            .map_err(FkError::Db)?;
        match row {
            None => Err(FkError::NotFound(label.to_string())),
            Some((owner,)) if owner != company_id => {
                Err(FkError::WrongCompany(label.to_string()))
            }
            Some(_) => Ok(()),
        }
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

    /// Round 168: 统计 company 从某个时间点之后的 cost_cents 总和。
    pub async fn sum_cost_cents_since(
        &self,
        company_id: Uuid,
        since: pc_core::Timestamp,
    ) -> sqlx::Result<i64> {
        let row: (Option<i64>,) = sqlx::query_as(
            "SELECT COALESCE(SUM(cost_cents),0)::bigint FROM cost_events \
             WHERE company_id = $1 AND occurred_at >= $2",
        )
        .bind(company_id)
        .bind(since)
        .fetch_one(self.db.pool())
        .await?;
        Ok(row.0.unwrap_or(0))
    }
}

/// Errors for `CostRepo::create_finance_event`.
#[derive(Debug, thiserror::Error)]
pub enum FinanceCreateError {
    /// Provided FK id is missing or belongs to a different company.
    #[error("finance event FK error: {0}")]
    Fk(#[from] FkError),
    /// Underlying DB error.
    #[error(transparent)]
    Db(sqlx::Error),
}

/// Specific failure modes for FK validation.
#[derive(Debug, thiserror::Error)]
pub enum FkError {
    #[error("{0} not found")]
    NotFound(String),
    #[error("{0} does not belong to company")]
    WrongCompany(String),
    #[error("finance FK lookup failed: {0}")]
    Db(sqlx::Error),
    #[error("finance FK lookup internal error: {0}")]
    Internal(String),
}

#[cfg(test)]
mod finance_create_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn new_finance_event_parses_camel_case_minimal() {
        let body = json!({
            "eventKind": "compute.usage",
            "biller": "openai",
            "amountCents": 123,
        });
        let parsed: NewFinanceEvent = serde_json::from_value(body).unwrap();
        assert_eq!(parsed.event_kind, "compute.usage");
        assert_eq!(parsed.biller, "openai");
        assert_eq!(parsed.amount_cents, 123);
        assert!(parsed.agent_id.is_none());
        assert!(parsed.issue_id.is_none());
        assert_eq!(parsed.direction, None);
        assert_eq!(parsed.currency, None);
        assert_eq!(parsed.estimated, None);
    }

    #[test]
    fn new_finance_event_parses_all_optional_fks() {
        let body = json!({
            "eventKind": "tool.usage",
            "biller": "openai",
            "amountCents": 50,
            "agentId": "11111111-1111-1111-1111-111111111111",
            "issueId": "22222222-2222-2222-2222-222222222222",
            "projectId": "33333333-3333-3333-3333-333333333333",
            "goalId": "44444444-4444-4444-4444-444444444444",
            "heartbeatRunId": "55555555-5555-5555-5555-555555555555",
            "costEventId": "66666666-6666-6666-6666-666666666666",
            "description": "gpt-4o batch",
            "direction": "credit",
            "currency": "EUR",
            "estimated": true,
            "externalInvoiceId": "inv-1",
            "metadataJson": {"foo": "bar"},
        });
        let parsed: NewFinanceEvent = serde_json::from_value(body).unwrap();
        assert_eq!(parsed.event_kind, "tool.usage");
        assert_eq!(parsed.biller, "openai");
        assert_eq!(parsed.amount_cents, 50);
        assert_eq!(
            parsed.agent_id,
            Some(Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap())
        );
        assert_eq!(parsed.issue_id.is_some(), true);
        assert_eq!(parsed.direction.as_deref(), Some("credit"));
        assert_eq!(parsed.currency.as_deref(), Some("EUR"));
        assert_eq!(parsed.estimated, Some(true));
        assert_eq!(parsed.external_invoice_id.as_deref(), Some("inv-1"));
        assert_eq!(parsed.metadata_json, Some(json!({"foo": "bar"})));
    }

    #[test]
    fn new_finance_event_rejects_missing_required_fields() {
        // missing eventKind
        let body = json!({
            "biller": "openai",
            "amountCents": 0,
        });
        let parsed: Result<NewFinanceEvent, _> = serde_json::from_value(body);
        assert!(parsed.is_err());
        // missing biller
        let body = json!({
            "eventKind": "x",
            "amountCents": 0,
        });
        let parsed: Result<NewFinanceEvent, _> = serde_json::from_value(body);
        assert!(parsed.is_err());
        // missing amountCents
        let body = json!({
            "eventKind": "x",
            "biller": "openai",
        });
        let parsed: Result<NewFinanceEvent, _> = serde_json::from_value(body);
        assert!(parsed.is_err());
    }

    #[test]
    fn fk_error_display_is_user_facing() {
        assert_eq!(FkError::NotFound("Agent".into()).to_string(), "Agent not found");
        assert_eq!(
            FkError::WrongCompany("Issue".into()).to_string(),
            "Issue does not belong to company"
        );
    }

}
