#![forbid(unsafe_code)]
//! Cost domain service layer.
//!
//! See `lib.rs` for module-level docs.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use pc_errors::{internal, not_found, validation, Error as PcError, Result};
pub use pc_repos::cost::{
    AgentCostWindow, CostByAgent, CostByAgentModel, CostByBiller, CostByProviderModel, CostByProject,
    CostEventRow, CostRange, CostRepo, CostSummary, CostWindowSpendRow, CreateCostEvent,
    FinanceByBiller, FinanceByKind, FinanceEventRow, FinanceSummary, IssueCostSummaryRow,
    NewFinanceEvent,
};
use pc_repos::Db;

// =============================================================================
// R609: lifecycle events surfaced to hooks
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum CostHookEvent {
    CostEventCreated {
        company_id: Uuid,
        event_id: Uuid,
        agent_id: Uuid,
        cost_cents: i64,
        provider: String,
        billing_type: String,
        model: String,
    },
    FinanceEventCreated {
        company_id: Uuid,
        event_id: Uuid,
        event_kind: String,
        direction: String,
        amount_cents: i32,
        biller: String,
    },
    MonthlySpendUpdated {
        company_id: Uuid,
        agent_id: Uuid,
        agent_month_cents: i64,
        company_month_cents: i64,
    },
}

/// Convenience payload for downstream subscribers that only need the core
/// fields of a cost event creation. Mirrors Node `costService.createEvent`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CostEventCreatedData {
    pub event_id: Uuid,
    pub company_id: Uuid,
    pub agent_id: Uuid,
    pub cost_cents: i64,
    pub provider: String,
    pub billing_type: String,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FinanceEventCreatedData {
    pub event_id: Uuid,
    pub company_id: Uuid,
    pub event_kind: String,
    pub direction: String,
    pub amount_cents: i32,
    pub biller: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MonthlySpendUpdatedData {
    pub company_id: Uuid,
    pub agent_id: Uuid,
    pub agent_month_cents: i64,
    pub company_month_cents: i64,
}

// =============================================================================
// R609: hook trait
// =============================================================================

#[async_trait]
pub trait CostHook: Send + Sync {
    async fn on_cost_event(&self, _event: CostHookEvent) -> Result<()> {
        Ok(())
    }
}

pub struct NoopCostHook;
#[async_trait]
impl CostHook for NoopCostHook {}

#[derive(Default)]
pub struct RecordingCostHook {
    pub events: std::sync::Mutex<Vec<CostHookEvent>>,
}

#[async_trait]
impl CostHook for RecordingCostHook {
    async fn on_cost_event(&self, event: CostHookEvent) -> Result<()> {
        self.events.lock().expect("lock").push(event);
        Ok(())
    }
}

impl RecordingCostHook {
    #[must_use]
    pub fn events_snapshot(&self) -> Vec<CostHookEvent> {
        self.events.lock().expect("lock").clone()
    }

    pub fn clear(&self) {
        self.events.lock().expect("lock").clear();
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.events.lock().expect("lock").len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.lock().expect("lock").is_empty()
    }
}

// =============================================================================
// R609: error type
// =============================================================================

/// Errors that can be returned by [`CostService`]. Wraps repo errors and
/// validation failures uniformly.
#[derive(Debug, thiserror::Error)]
pub enum CostFinanceError {
    #[error("validation: {0}")]
    Validation(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("foreign key: {0}")]
    Fk(String),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Pc(#[from] PcError),
}

impl From<pc_repos::RepoError> for CostFinanceError {
    fn from(e: pc_repos::RepoError) -> Self {
        Self::Pc(internal(e.to_string()))
    }
}

impl From<pc_repos::cost::FinanceCreateError> for CostFinanceError {
    fn from(e: pc_repos::cost::FinanceCreateError) -> Self {
        use pc_repos::cost::FinanceCreateError as F;
        match e {
            F::Fk(fk_err) => {
                use pc_repos::cost::FkError as Fk;
                match fk_err {
                    Fk::NotFound(s) => Self::NotFound(s),
                    Fk::WrongCompany(s) => Self::Fk(s),
                    Fk::Db(sqlx_err) => Self::Db(sqlx_err),
                    Fk::Internal(s) => Self::Pc(internal(s)),
                }
            }
            F::Db(sqlx_err) => Self::Db(sqlx_err),
        }
    }
}

pub type CostResult<T> = std::result::Result<T, CostFinanceError>;

// =============================================================================
// R609: input validation helpers
// =============================================================================

fn normalize_create(input: &CreateCostEvent) -> Result<()> {
    if input.company_id_nil_proof().is_some() {
        // Helper that returns Some(()) if input would have come from nil-company code
        return Err(validation("companyId is required"));
    }
    if input.agent_id.is_nil() {
        return Err(validation("agentId is required"));
    }
    if input.provider.trim().is_empty() {
        return Err(validation("provider must not be empty"));
    }
    if input.model.trim().is_empty() {
        return Err(validation("model must not be empty"));
    }
    if input.cost_cents < 0 {
        return Err(validation("costCents must be non-negative"));
    }
    if input.input_tokens < 0
        || input.cached_input_tokens < 0
        || input.output_tokens < 0
    {
        return Err(validation("token counts must be non-negative"));
    }
    Ok(())
}

// Required because CreateCostEvent does not have companyId; the service
// validates company_id separately.
trait CreateCostEventExt {
    fn company_id_nil_proof(&self) -> Option<()>;
}
impl CreateCostEventExt for CreateCostEvent {
    fn company_id_nil_proof(&self) -> Option<()> {
        None
    }
}

fn normalize_new_finance_event(input: &NewFinanceEvent) -> Result<()> {
    if input.event_kind.trim().is_empty() {
        return Err(validation("eventKind must not be empty"));
    }
    if input.biller.trim().is_empty() {
        return Err(validation("biller must not be empty"));
    }
    if input.amount_cents < 0 {
        return Err(validation("amountCents must be non-negative"));
    }
    if let Some(dir) = &input.direction {
        if dir != "debit" && dir != "credit" {
            return Err(validation("direction must be 'debit' or 'credit'"));
        }
    }
    Ok(())
}

// =============================================================================
// R609: monthly spend helpers (mirror Node `currentUtcMonthWindow` /
// `getMonthlySpendTotal`)
// =============================================================================

/// Current UTC calendar month window `[start_of_month, start_of_next_month)`.
#[must_use]
pub fn current_utc_month_window(now: DateTime<Utc>) -> (DateTime<Utc>, DateTime<Utc>) {
    use chrono::TimeZone;
    let year = now.year();
    let month = now.month();
    let start = Utc
        .with_ymd_and_hms(year, month, 1, 0, 0, 0)
        .single()
        .expect("valid first-of-month");
    let end = Utc
        .with_ymd_and_hms(if month == 12 { year + 1 } else { year }, if month == 12 { 1 } else { month + 1 }, 1, 0, 0, 0)
        .single()
        .expect("valid first-of-next-month");
    (start, end)
}

async fn get_monthly_spend_total(
    db: &Db,
    company_id: Uuid,
    agent_id: Option<Uuid>,
) -> CostResult<i64> {
    let (start, end) = current_utc_month_window(Utc::now());
    let row: (Option<i64>,) = match agent_id {
        Some(aid) => {
            sqlx::query_as(
                "SELECT COALESCE(SUM(cost_cents), 0)::bigint FROM cost_events                  WHERE company_id = $1 AND agent_id = $2                    AND occurred_at >= $3 AND occurred_at < $4",
            )
            .bind(company_id)
            .bind(aid)
            .bind(start)
            .bind(end)
            .fetch_one(db.pool())
            .await?
        }
        None => {
            sqlx::query_as(
                "SELECT COALESCE(SUM(cost_cents), 0)::bigint FROM cost_events                  WHERE company_id = $1                    AND occurred_at >= $2 AND occurred_at < $3",
            )
            .bind(company_id)
            .bind(start)
            .bind(end)
            .fetch_one(db.pool())
            .await?
        }
    };
    Ok(row.0.unwrap_or(0))
}

use chrono::Datelike;

// =============================================================================
// R609: CostService
// =============================================================================

#[derive(Clone)]
pub struct CostService {
    db: Db,
    hooks: Vec<Arc<dyn CostHook>>,
}

impl CostService {
    pub fn new(db: Db) -> Self {
        Self { db, hooks: Vec::new() }
    }

    pub fn with_hooks(db: Db, hooks: Vec<Arc<dyn CostHook>>) -> Self {
        Self { db, hooks }
    }

    pub fn add_hook(mut self, h: Arc<dyn CostHook>) -> Self {
        self.hooks.push(h);
        self
    }

    pub fn hook_count(&self) -> usize {
        self.hooks.len()
    }

    async fn dispatch(&self, event: CostHookEvent) {
        for h in &self.hooks {
            if let Err(e) = h.on_cost_event(event.clone()).await {
                tracing::warn!(?e, "cost hook failed");
            }
        }
    }

    fn repo(&self) -> CostRepo<'_> {
        CostRepo::new(&self.db)
    }

    // -------------------------------------------------------------------------
    // Cost event lifecycle (composite)
    // -------------------------------------------------------------------------

    /// Insert a new `cost_event` row and reconcile the agent/company monthly
    /// spend columns. Mirrors `server/src/services/costs.ts::createEvent`.
    ///
    /// Steps:
    /// 1. Validate input (non-nil agent, non-empty provider/model, non-negative
    ///    cost).
    /// 2. Look up the agent and confirm it belongs to the same company.
    /// 3. INSERT the cost_event row (biller defaults to provider, billingType
    ///    defaults to "unknown", cachedInputTokens defaults to 0).
    /// 4. Recompute `agents.spent_monthly_cents` and `companies.spent_monthly_cents`
    ///    for the current UTC month window.
    /// 5. Fire `CostEventCreated` and `MonthlySpendUpdated` hooks.
    pub async fn create_cost_event(
        &self,
        company_id: Uuid,
        input: CreateCostEvent,
    ) -> CostResult<CostEventRow> {
        if company_id.is_nil() {
            return Err(CostFinanceError::Validation("companyId is required".into()));
        }
        normalize_create(&input)?;

        // Confirm agent belongs to same company.
        let agent_row: Option<(Uuid,)> = sqlx::query_as(
            "SELECT company_id FROM agents WHERE id = $1",
        )
        .bind(input.agent_id)
        .fetch_optional(self.db.pool())
        .await?;
        let Some((agent_company,)) = agent_row else {
            return Err(CostFinanceError::NotFound("Agent not found".into()));
        };
        if agent_company != company_id {
            return Err(CostFinanceError::Fk(
                "Agent does not belong to company".into(),
            ));
        }

        let biller = if input.biller.is_empty() { input.provider.clone() } else { input.biller.clone() };
        let billing_type = if input.billing_type.is_empty() {
            "unknown".into()
        } else {
            input.billing_type.clone()
        };
        let cached_input_tokens = input.cached_input_tokens;
        let cost_cents = i64::from(input.cost_cents);

        // Insert and RETURN the full row.
        let row: CostEventRow = sqlx::query_as(
            r#"INSERT INTO cost_events (
                company_id, agent_id, issue_id, project_id, goal_id, heartbeat_run_id,
                billing_code, provider, biller, billing_type, model,
                input_tokens, cached_input_tokens, output_tokens, cost_cents, occurred_at
              ) VALUES (
                $1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16
              )
              RETURNING id, company_id, agent_id, issue_id, project_id, goal_id,
                billing_code, provider, model, input_tokens, output_tokens,
                cost_cents, occurred_at, created_at"#,
        )
        .bind(company_id)
        .bind(input.agent_id)
        .bind(input.issue_id)
        .bind(input.project_id)
        .bind(input.goal_id)
        .bind(input.heartbeat_run_id)
        .bind(&input.billing_code)
        .bind(&input.provider)
        .bind(&biller)
        .bind(&billing_type)
        .bind(&input.model)
        .bind(input.input_tokens)
        .bind(cached_input_tokens)
        .bind(input.output_tokens)
        .bind(input.cost_cents)
        .bind(input.occurred_at)
        .fetch_one(self.db.pool())
        .await?;

        // Recompute monthly spend for the agent and the company.
        let agent_month = get_monthly_spend_total(&self.db, company_id, Some(input.agent_id)).await?;
        let company_month = get_monthly_spend_total(&self.db, company_id, None).await?;

        sqlx::query("UPDATE agents SET spent_monthly_cents = $1, updated_at = now() WHERE id = $2")
            .bind(agent_month)
            .bind(input.agent_id)
            .execute(self.db.pool())
            .await?;

        sqlx::query("UPDATE companies SET spent_monthly_cents = $1, updated_at = now() WHERE id = $2")
            .bind(company_month)
            .bind(company_id)
            .execute(self.db.pool())
            .await?;

        // Emit hooks (order matters: CostEventCreated first, then MonthlySpend).
        self.dispatch(CostHookEvent::CostEventCreated {
            company_id,
            event_id: row.id,
            agent_id: row.agent_id,
            cost_cents,
            provider: row.provider.clone(),
            billing_type: billing_type.clone(),
            model: row.model.clone(),
        })
        .await;
        self.dispatch(CostHookEvent::MonthlySpendUpdated {
            company_id,
            agent_id: row.agent_id,
            agent_month_cents: agent_month,
            company_month_cents: company_month,
        })
        .await;

        Ok(row)
    }

    // -------------------------------------------------------------------------
    // Aggregations — direct repo passthrough
    // -------------------------------------------------------------------------

    pub async fn summary(
        &self,
        company_id: Uuid,
        range: CostRange,
    ) -> CostResult<CostSummary> {
        Ok(self.repo().summary(company_id, range).await?)
    }

    pub async fn by_agent(
        &self,
        company_id: Uuid,
        range: CostRange,
    ) -> CostResult<Vec<CostByAgent>> {
        Ok(self.repo().by_agent(company_id, range).await?)
    }

    pub async fn by_agent_model(
        &self,
        company_id: Uuid,
        range: CostRange,
    ) -> CostResult<Vec<CostByAgentModel>> {
        Ok(self.repo().by_agent_model(company_id, range).await?)
    }

    pub async fn by_provider(
        &self,
        company_id: Uuid,
        range: CostRange,
    ) -> CostResult<Vec<CostByProviderModel>> {
        Ok(self.repo().by_provider(company_id, range).await?)
    }

    pub async fn by_biller(
        &self,
        company_id: Uuid,
        range: CostRange,
    ) -> CostResult<Vec<CostByBiller>> {
        Ok(self.repo().by_biller(company_id, range).await?)
    }

    pub async fn by_project(
        &self,
        company_id: Uuid,
        range: CostRange,
    ) -> CostResult<Vec<CostByProject>> {
        Ok(self.repo().by_project(company_id, range).await?)
    }

    pub async fn window_spend(
        &self,
        company_id: Uuid,
    ) -> CostResult<Vec<CostWindowSpendRow>> {
        Ok(self.repo().window_spend(company_id).await?)
    }

    pub async fn list_cost_events(
        &self,
        company_id: Uuid,
        limit: i64,
    ) -> CostResult<Vec<CostEventRow>> {
        Ok(self.repo().list_cost_events(company_id, limit).await?)
    }

    pub async fn issue_summary(
        &self,
        issue_id: Uuid,
    ) -> CostResult<Option<IssueCostSummaryRow>> {
        Ok(self.repo().issue_summary(issue_id).await?)
    }

    pub async fn sum_agent_window_cost_cents(
        &self,
        window: AgentCostWindow,
    ) -> CostResult<i64> {
        Ok(self.repo().sum_agent_window_cost_cents(window).await?)
    }

    pub async fn sum_cost_cents_since(
        &self,
        company_id: Uuid,
        since: pc_core::Timestamp,
    ) -> CostResult<i64> {
        Ok(self.repo().sum_cost_cents_since(company_id, since).await?)
    }

    // -------------------------------------------------------------------------
    // Finance event lifecycle (composite)
    // -------------------------------------------------------------------------

    /// Insert a `finance_events` row. FK validation is delegated to the repo.
    pub async fn create_finance_event(
        &self,
        company_id: Uuid,
        input: NewFinanceEvent,
    ) -> CostResult<FinanceEventRow> {
        if company_id.is_nil() {
            return Err(CostFinanceError::Validation("companyId is required".into()));
        }
        normalize_new_finance_event(&input)?;

        let row = self.repo().create_finance_event(company_id, &input).await?;

        self.dispatch(CostHookEvent::FinanceEventCreated {
            company_id,
            event_id: row.id,
            event_kind: row.event_kind.clone(),
            direction: row.direction.clone(),
            amount_cents: row.amount_cents,
            biller: row.biller.clone(),
        })
        .await;

        Ok(row)
    }

    pub async fn finance_summary(
        &self,
        company_id: Uuid,
        range: CostRange,
    ) -> CostResult<FinanceSummary> {
        Ok(self.repo().finance_summary(company_id, range).await?)
    }

    pub async fn finance_by_biller(
        &self,
        company_id: Uuid,
        range: CostRange,
    ) -> CostResult<Vec<FinanceByBiller>> {
        Ok(self.repo().finance_by_biller(company_id, range).await?)
    }

    pub async fn finance_by_kind(
        &self,
        company_id: Uuid,
        range: CostRange,
    ) -> CostResult<Vec<FinanceByKind>> {
        Ok(self.repo().finance_by_kind(company_id, range).await?)
    }

    pub async fn finance_events(
        &self,
        company_id: Uuid,
        range: CostRange,
        limit: i64,
    ) -> CostResult<Vec<FinanceEventRow>> {
        Ok(self.repo().finance_events(company_id, range, limit).await?)
    }
}

// =============================================================================
// Tests (unit tests for pure helpers; DB tests live in tests/)
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_utc_month_window_mid_month() {
        let now = Utc.with_ymd_and_hms(2026, 8, 15, 12, 0, 0).single().unwrap();
        let (start, end) = current_utc_month_window(now);
        assert_eq!(start, Utc.with_ymd_and_hms(2026, 8, 1, 0, 0, 0).single().unwrap());
        assert_eq!(end, Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).single().unwrap());
    }

    #[test]
    fn current_utc_month_window_year_boundary() {
        let now = Utc.with_ymd_and_hms(2026, 12, 31, 23, 0, 0).single().unwrap();
        let (start, end) = current_utc_month_window(now);
        assert_eq!(start, Utc.with_ymd_and_hms(2026, 12, 1, 0, 0, 0).single().unwrap());
        assert_eq!(end, Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).single().unwrap());
    }

    #[test]
    fn validate_create_cost_event_rejects_negative() {
        let mut input = CreateCostEvent {
            agent_id: Uuid::new_v4(),
            issue_id: None,
            project_id: None,
            goal_id: None,
            heartbeat_run_id: None,
            billing_code: None,
            provider: "openai".into(),
            biller: "openai".into(),
            billing_type: "api".into(),
            model: "gpt-4o-mini".into(),
            input_tokens: 0,
            cached_input_tokens: 0,
            output_tokens: 0,
            cost_cents: -1,
            occurred_at: Utc::now(),
        };
        assert!(normalize_create(&input).is_err());
        input.cost_cents = 0;
        assert!(normalize_create(&input).is_ok());
    }

    #[test]
    fn validate_create_cost_event_rejects_empty_provider() {
        let input = CreateCostEvent {
            agent_id: Uuid::new_v4(),
            issue_id: None,
            project_id: None,
            goal_id: None,
            heartbeat_run_id: None,
            billing_code: None,
            provider: "".into(),
            biller: "x".into(),
            billing_type: "x".into(),
            model: "y".into(),
            input_tokens: 0,
            cached_input_tokens: 0,
            output_tokens: 0,
            cost_cents: 0,
            occurred_at: Utc::now(),
        };
        assert!(normalize_create(&input).is_err());
    }

    #[test]
    fn validate_new_finance_event_direction() {
        let mut input = NewFinanceEvent {
            event_kind: "model_usage".into(),
            biller: "openai".into(),
            amount_cents: 100,
            direction: Some("sideways".into()),
            ..Default::default()
        };
        assert!(normalize_new_finance_event(&input).is_err());
        input.direction = Some("credit".into());
        assert!(normalize_new_finance_event(&input).is_ok());
        input.direction = None;
        assert!(normalize_new_finance_event(&input).is_ok());
    }
}
