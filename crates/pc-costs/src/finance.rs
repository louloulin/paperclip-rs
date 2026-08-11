//! Finance service 高级 facade。
//!
//! 对应 Node `server/src/services/finance.ts`（134 行）1:1 复刻。
//! （原 `pc-finance` crate 已下沉到 `pc-costs::finance`）。


use std::sync::Arc;

use chrono::{DateTime, Utc};
use thiserror::Error;
use uuid::Uuid;

// ============================================================================
// Re-exports from pc-repos
// ============================================================================

pub use pc_repos::cost::{
    CostRange, FinanceByBiller, FinanceByKind, FinanceCreateError, FinanceEventRow,
    FinanceSummary, NewFinanceEvent,
};

// ============================================================================
// Errors
// ============================================================================

/// Finance 服务错误。
#[derive(Debug, Error)]
pub enum FinanceError {
    #[error("finance row not found")]
    NotFound,
    #[error("FK does not belong to company: {0}")]
    FkMismatch(String),
    #[error("db error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("finance create error: {0}")]
    Create(#[from] FinanceCreateError),
}

pub type FinanceResult<T> = std::result::Result<T, FinanceError>;

// ============================================================================
// Date range
// ============================================================================

/// Finance 日期范围（与 Node `FinanceDateRange` 1:1 对齐）。
#[derive(Debug, Clone, Default)]
pub struct FinanceDateRange {
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
}

impl From<FinanceDateRange> for CostRange {
    fn from(r: FinanceDateRange) -> Self {
        CostRange {
            from: r.from,
            to: r.to,
        }
    }
}

// ============================================================================
// Service
// ============================================================================

/// Finance service handle（与 Node `financeService(db)` 返回 1:1 对齐）。
pub struct FinanceService {
    db: pc_repos::Db,
}

impl FinanceService {
    /// 构造（与 Node `financeService(db)` factory 1:1 对齐）。
    pub fn new(db: pc_repos::Db) -> Self {
        Self { db }
    }

    /// 内部获取 repo handle。
    fn repo(&self) -> pc_repos::cost::CostRepo<'_> {
        pc_repos::cost::CostRepo::new(&self.db)
    }

    /// 创建一条 finance event。
    ///
    /// 行为（与 Node `createEvent` 1:1）：
    /// - 校验 agent/issue/project/goal/heartbeat_run/cost_event FK 属于 company
    /// - 默认 `currency = "USD"` / `direction = "debit"` / `estimated = false`
    pub async fn create_event(
        &self,
        company_id: Uuid,
        data: NewFinanceEvent,
    ) -> FinanceResult<FinanceEventRow> {
        let row = self.repo().create_finance_event(company_id, &data).await?;
        Ok(row)
    }

    /// 汇总（与 Node `summary` 1:1）。
    pub async fn summary(
        &self,
        company_id: Uuid,
        range: Option<FinanceDateRange>,
    ) -> FinanceResult<FinanceSummary> {
        let range = range.unwrap_or_default();
        let summary = self
            .repo()
            .finance_summary(company_id, range.into())
            .await?;
        Ok(summary)
    }

    /// 按 biller 聚合（与 Node `byBiller` 1:1）。
    pub async fn by_biller(
        &self,
        company_id: Uuid,
        range: Option<FinanceDateRange>,
    ) -> FinanceResult<Vec<FinanceByBiller>> {
        let range = range.unwrap_or_default();
        let rows = self
            .repo()
            .finance_by_biller(company_id, range.into())
            .await?;
        Ok(rows)
    }

    /// 按 event_kind 聚合（与 Node `byKind` 1:1）。
    pub async fn by_kind(
        &self,
        company_id: Uuid,
        range: Option<FinanceDateRange>,
    ) -> FinanceResult<Vec<FinanceByKind>> {
        let range = range.unwrap_or_default();
        let rows = self
            .repo()
            .finance_by_kind(company_id, range.into())
            .await?;
        Ok(rows)
    }

    /// 列出 finance events（与 Node `list` 1:1）。
    pub async fn list(
        &self,
        company_id: Uuid,
        range: Option<FinanceDateRange>,
        limit: i64,
    ) -> FinanceResult<Vec<FinanceEventRow>> {
        let range = range.unwrap_or_default();
        let rows = self
            .repo()
            .finance_events(company_id, range.into(), limit)
            .await?;
        Ok(rows)
    }
}

// ============================================================================
// Shared service handle (alias)
// ============================================================================

/// 共享 handle（便于上层 clone）。
pub type SharedFinanceService = Arc<FinanceService>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r713_default_range_is_all_time() {
        let r = FinanceDateRange::default();
        assert!(r.from.is_none());
        assert!(r.to.is_none());
        let cr: CostRange = r.into();
        assert!(cr.from.is_none());
        assert!(cr.to.is_none());
    }

    #[test]
    fn r713_range_conversion_preserves_values() {
        let now = Utc::now();
        let r = FinanceDateRange {
            from: Some(now - chrono::Duration::days(7)),
            to: Some(now),
        };
        let cr: CostRange = r.into();
        assert!(cr.from.is_some());
        assert!(cr.to.is_some());
    }

    #[test]
    fn r713_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<FinanceService>();
        assert_send_sync::<FinanceSummary>();
        assert_send_sync::<FinanceEventRow>();
    }

    #[test]
    fn r713_finance_summary_fields() {
        // Type shape sanity check (compile-time)
        let s = FinanceSummary {
            company_id: Uuid::nil(),
            debit_cents: 0,
            credit_cents: 0,
            net_cents: 0,
            estimated_debit_cents: 0,
            event_count: 0,
        };
        assert_eq!(s.net_cents, 0);
    }
}
