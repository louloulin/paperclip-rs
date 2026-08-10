//! `CompanyBudgetHook` — R592。
//!
//! 监听 CompanyService 的 `Created` 事件，当 `budget_monthly_cents > 0`
//! 时通过 BudgetService.upsert_policy 自动建立 company 级月度预算策略。
//!
//! 对齐上游 `server/src/routes/companies.ts` POST `/` 行为：
//!   if (company.budgetMonthlyCents > 0) {
//!     await budgets.upsertPolicy(company.id, {...}, ...)
//!   }
//!
//! 设计：hook 接受 `Arc<BudgetService<'static>>`，避免每次调用重新构造。

use async_trait::async_trait;
use pc_budgets::BudgetService;
use pc_companies::{CompanyHook, CompanyLifecycleEvent, CompanyServiceResult};
use pc_repos::budget::UpsertPolicyInput;
use std::sync::Arc;

#[derive(Clone)]
pub struct CompanyBudgetHook {
    budget_service: Arc<BudgetService<'static>>,
}

impl std::fmt::Debug for CompanyBudgetHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompanyBudgetHook").finish()
    }
}

impl CompanyBudgetHook {
    /// 构造 hook。`budget_service` 需要 `'static` lifetime — 调用方通过
    /// `Box::leak` 或在 AppState 中常驻 budget service。
    #[must_use]
    pub fn new(budget_service: Arc<BudgetService<'static>>) -> Self {
        Self { budget_service }
    }
}

#[async_trait]
impl CompanyHook for CompanyBudgetHook {
    async fn on_lifecycle(
        &self,
        event: CompanyLifecycleEvent,
    ) -> CompanyServiceResult<()> {
        if let CompanyLifecycleEvent::Created {
            id,
            budget_monthly_cents: Some(amount),
            ..
        } = event
        {
            if amount <= 0 {
                return Ok(());
            }
            // 与上游一致：scope=company, window=calendar_month_utc, metric=cost_cents
            let input = UpsertPolicyInput {
                scope_type: "company".into(),
                scope_id: id,
                metric: "cost_cents".into(),
                window_kind: "calendar_month_utc".into(),
                amount,
                warn_percent: 80,
                hard_stop_enabled: true,
                notify_enabled: true,
                is_active: true,
                updated_by_user_id: Some("system".into()),
            };
            if let Err(e) = self
                .budget_service
                .upsert_policy(id, input)
                .await
            {
                tracing::warn!(
                    company_id = %id,
                    error = %e,
                    "company budget policy upsert failed"
                );
            }
        }
        Ok(())
    }
}
