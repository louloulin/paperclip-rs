//! Budget 业务层：window 计算 + status 推导 + 副作用 hook。
//!
//! ## 模块拆分
//! - 纯函数（无 DB 依赖，可单测）：`compute_window` / `infer_status` / `normalize_scope_name`
//! - 业务层：`BudgetService<'a>` 包装 `BudgetRepo` + `BudgetEnforcementHook`
//!
//! ## 状态机
//! `BudgetPolicyStatus`: `Ok` (用量 < warn 阈值) → `Warning` (≥ warn) → `HardStop` (≥ amount)
//!
//! ## 副作用抽象
//! `BudgetEnforcementHook` trait 让调用方注入：
//! - `on_hard_stop` — 取消/暂停 scope 内的工作（agent 暂停 / 项目冻结）
//! - `on_warning` — 发送告警通知
//! - `on_resolve` — incident 解决后清理

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Datelike, TimeZone, Utc};
use uuid::Uuid;

use pc_repos::budget::{
    BudgetRepo, IncidentRow, PolicyRow, ResolveIncidentInput, UpsertPolicyInput,
};

/// 时间窗口类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BudgetWindowKind {
    /// 自然月窗口（UTC）：[start_of_month, start_of_next_month)
    CalendarMonthUtc,
    /// 全生命周期窗口：[1970-01-01, 9999-01-01)
    Lifetime,
}

impl BudgetWindowKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CalendarMonthUtc => "calendar_month_utc",
            Self::Lifetime => "lifetime",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "calendar_month_utc" => Some(Self::CalendarMonthUtc),
            "lifetime" => Some(Self::Lifetime),
            _ => None,
        }
    }
}

/// 时间窗口。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetWindow {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

impl BudgetWindow {
    #[must_use]
    pub fn contains(&self, t: DateTime<Utc>) -> bool {
        t >= self.start && t < self.end
    }
}

/// 业务错误。
#[derive(Debug, thiserror::Error)]
pub enum BudgetError {
    #[error("invalid window kind: {0}")]
    InvalidWindowKind(String),
    #[error("invalid scope type: {0}")]
    InvalidScopeType(String),
    #[error("repository error: {0}")]
    Repo(String),
    #[error("hook error: {0}")]
    Hook(String),
}

impl From<pc_repos::RepoError> for BudgetError {
    fn from(e: pc_repos::RepoError) -> Self {
        Self::Repo(e.to_string())
    }
}

impl From<sqlx::Error> for BudgetError {
    fn from(e: sqlx::Error) -> Self {
        Self::Repo(format!("sqlx: {e}"))
    }
}

pub type BudgetResult<T> = Result<T, BudgetError>;

/// Policy 用量状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BudgetPolicyStatus {
    /// 用量未达 warn 阈值。
    Ok,
    /// 用量达到 warn 阈值但未达 hard stop。
    Warning,
    /// 用量达到 amount 上限。
    HardStop,
}

impl BudgetPolicyStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warning => "warning",
            Self::HardStop => "hard_stop",
        }
    }
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "ok" => Some(Self::Ok),
            "warning" => Some(Self::Warning),
            "hard_stop" => Some(Self::HardStop),
            _ => None,
        }
    }
    #[must_use]
    pub fn is_at_or_above_warning(self) -> bool {
        !matches!(self, Self::Ok)
    }
}

/// 计算时间窗口（纯函数）。
#[must_use]
pub fn compute_window(kind: BudgetWindowKind, now: DateTime<Utc>) -> BudgetWindow {
    match kind {
        BudgetWindowKind::CalendarMonthUtc => {
            let year = now.year();
            let month = now.month();
            let start = Utc
                .with_ymd_and_hms(year, month, 1, 0, 0, 0)
                .single()
                .expect("first day of month is valid");
            let (ny, nm) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
            let end = Utc
                .with_ymd_and_hms(ny, nm, 1, 0, 0, 0)
                .single()
                .expect("first day of next month is valid");
            BudgetWindow { start, end }
        }
        BudgetWindowKind::Lifetime => BudgetWindow {
            start: Utc
                .with_ymd_and_hms(1970, 1, 1, 0, 0, 0)
                .single()
                .unwrap(),
            end: Utc
                .with_ymd_and_hms(9999, 1, 1, 0, 0, 0)
                .single()
                .unwrap(),
        },
    }
}

/// 推导 policy 用量状态（纯函数）。
///
/// - `observed` 为当前累计用量
/// - `amount` 为 policy 上限
/// - `warn_percent` 为警告阈值百分比（0-100）
#[must_use]
pub fn infer_status(observed: i64, amount: i64, warn_percent: i32) -> BudgetPolicyStatus {
    if amount <= 0 {
        return BudgetPolicyStatus::Ok;
    }
    if observed >= amount {
        return BudgetPolicyStatus::HardStop;
    }
    // warn_percent <= 0 跳过 warning 检测
    if warn_percent <= 0 {
        return BudgetPolicyStatus::Ok;
    }
    // 警告阈值 = ceil(amount * warn_percent / 100)，防止浮点
    let p = warn_percent.max(0).min(100) as i64;
    let warn_threshold = (amount.saturating_mul(p) + 99) / 100;
    if observed >= warn_threshold {
        BudgetPolicyStatus::Warning
    } else {
        BudgetPolicyStatus::Ok
    }
}

/// 规范化 scope 名称（company 类型保留原名；其他 trim 后空字符串用 scopeType 兜底）。
#[must_use]
pub fn normalize_scope_name(scope_type: &str, name: &str) -> String {
    if scope_type == "company" {
        return name.to_string();
    }
    let trimmed = name.trim();
    if trimmed.is_empty() {
        scope_type.to_string()
    } else {
        trimmed.to_string()
    }
}

/// 副作用作用域。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetEnforcementScope {
    pub company_id: Uuid,
    pub scope_type: String,
    pub scope_id: Uuid,
}

/// Budget 副作用抽象。
#[async_trait]
pub trait BudgetEnforcementHook: Send + Sync {
    /// 触发 hard_stop 时调用（例如暂停 agent、取消 work item）。
    async fn on_hard_stop(&self, _scope: &BudgetEnforcementScope) -> BudgetResult<()> {
        Ok(())
    }
    /// 触发 warning 时调用（发送告警通知）。
    async fn on_warning(&self, _scope: &BudgetEnforcementScope) -> BudgetResult<()> {
        Ok(())
    }
    /// incident 解决后清理。
    async fn on_resolve(&self, _scope: &BudgetEnforcementScope) -> BudgetResult<()> {
        Ok(())
    }
}

/// 空 hook：用于纯状态机场景。
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopEnforcementHook;

#[async_trait]
impl BudgetEnforcementHook for NoopEnforcementHook {}

/// 业务层。
pub struct BudgetService<'a> {
    repo: BudgetRepo<'a>,
    hooks: Vec<Arc<dyn BudgetEnforcementHook>>,
}

impl<'a> BudgetService<'a> {
    #[must_use]
    pub fn new(db: &'a pc_repos::Db) -> Self {
        Self {
            repo: BudgetRepo::new(db),
            hooks: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_hooks(db: &'a pc_repos::Db, hooks: Vec<Arc<dyn BudgetEnforcementHook>>) -> Self {
        Self {
            repo: BudgetRepo::new(db),
            hooks,
        }
    }

    pub fn add_hook(mut self, hook: Arc<dyn BudgetEnforcementHook>) -> Self {
        self.hooks.push(hook);
        self
    }

    #[must_use]
    pub fn hook_count(&self) -> usize {
        self.hooks.len()
    }

    // ------------------------------------------------------------------
    // 查询 / 写入（delegates to repo）
    // ------------------------------------------------------------------

    pub async fn list_policies(&self, company_id: Uuid) -> BudgetResult<Vec<PolicyRow>> {
        Ok(self.repo.list_policies(company_id).await?)
    }

    pub async fn upsert_policy(
        &self,
        company_id: Uuid,
        input: UpsertPolicyInput,
    ) -> BudgetResult<PolicyRow> {
        // 校验 window_kind
        if BudgetWindowKind::parse(&input.window_kind).is_none() {
            return Err(BudgetError::InvalidWindowKind(input.window_kind));
        }
        Ok(self.repo.upsert_policy(company_id, &input).await?)
    }

    pub async fn list_incidents(&self, company_id: Uuid) -> BudgetResult<Vec<IncidentRow>> {
        Ok(self.repo.list_incidents(company_id).await?)
    }

    pub async fn list_open_attention(&self, company_id: Uuid) -> BudgetResult<Vec<IncidentRow>> {
        Ok(self.repo.list_open_attention(company_id).await?)
    }

    pub async fn get_incident(&self, company_id: Uuid, id: Uuid) -> BudgetResult<Option<IncidentRow>> {
        Ok(self.repo.get_incident(company_id, id).await?)
    }

    // ------------------------------------------------------------------
    // 业务 API：评估 policy + 触发 hook
    // ------------------------------------------------------------------

    /// 给定 observed 用量，评估 policy 状态 + 触发相应 hook。
    ///
    /// 返回计算得到的 status。
    pub async fn evaluate_and_enforce(
        &self,
        policy: &PolicyRow,
        observed: i64,
        now: DateTime<Utc>,
    ) -> BudgetResult<BudgetPolicyStatus> {
        let status = infer_status(observed, policy.amount as i64, policy.warn_percent);
        let scope = BudgetEnforcementScope {
            company_id: policy.company_id,
            scope_type: policy.scope_type.clone(),
            scope_id: policy.scope_id,
        };
        match status {
            BudgetPolicyStatus::HardStop if policy.hard_stop_enabled => {
                self.run_hooks(HookPhase::HardStop, &scope).await?;
            }
            BudgetPolicyStatus::Warning if policy.notify_enabled => {
                self.run_hooks(HookPhase::Warning, &scope).await?;
            }
            _ => {}
        }
        // 静默消费 now — 留作未来 metrics hook
        let _ = now;
        Ok(status)
    }

    /// 解决 incident + 触发 on_resolve hook。
    pub async fn resolve_incident(
        &self,
        company_id: Uuid,
        id: Uuid,
        input: ResolveIncidentInput,
    ) -> BudgetResult<Option<IncidentRow>> {
        let row = self.repo.resolve_incident(company_id, id, &input).await?;
        if let Some(inc) = &row {
            let scope = BudgetEnforcementScope {
                company_id: inc.company_id,
                scope_type: inc.scope_type.clone(),
                scope_id: inc.scope_id,
            };
            self.run_hooks(HookPhase::Resolve, &scope).await?;
        }
        Ok(row)
    }

    async fn run_hooks(
        &self,
        phase: HookPhase,
        scope: &BudgetEnforcementScope,
    ) -> BudgetResult<()> {
        for (idx, hook) in self.hooks.iter().enumerate() {
            let outcome = match phase {
                HookPhase::HardStop => hook.on_hard_stop(scope).await,
                HookPhase::Warning => hook.on_warning(scope).await,
                HookPhase::Resolve => hook.on_resolve(scope).await,
            };
            if let Err(e) = outcome {
                tracing::warn!(
                    scope_type = %scope.scope_type,
                    scope_id = %scope.scope_id,
                    hook_index = idx,
                    phase = ?phase,
                    "budget enforcement hook failed: {e}"
                );
                return Err(BudgetError::Hook(e.to_string()));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
enum HookPhase {
    HardStop,
    Warning,
    Resolve,
}

// ----------------------------------------------------------------------
// 测试（纯逻辑层 + hook dispatch）
// ----------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        futures_executor::block_on(f)
    }

    // Window 计算

    #[test]
    fn r575_compute_window_calendar_month_utc_january() {
        let now = Utc.with_ymd_and_hms(2026, 1, 15, 12, 0, 0).unwrap();
        let w = compute_window(BudgetWindowKind::CalendarMonthUtc, now);
        assert_eq!(w.start, Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap());
        assert_eq!(w.end, Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap());
    }

    #[test]
    fn r575_compute_window_calendar_month_utc_december_rolls_year() {
        let now = Utc.with_ymd_and_hms(2026, 12, 31, 23, 59, 59).unwrap();
        let w = compute_window(BudgetWindowKind::CalendarMonthUtc, now);
        assert_eq!(w.start, Utc.with_ymd_and_hms(2026, 12, 1, 0, 0, 0).unwrap());
        assert_eq!(w.end, Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap());
    }

    #[test]
    fn r575_compute_window_lifetime_spans_1970_to_9999() {
        let now = Utc.with_ymd_and_hms(2026, 6, 15, 0, 0, 0).unwrap();
        let w = compute_window(BudgetWindowKind::Lifetime, now);
        assert_eq!(w.start.year(), 1970);
        assert_eq!(w.end.year(), 9999);
    }

    #[test]
    fn r575_window_contains_inclusive_start_exclusive_end() {
        let w = BudgetWindow {
            start: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            end: Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap(),
        };
        assert!(w.contains(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()));
        assert!(w.contains(Utc.with_ymd_and_hms(2026, 1, 31, 23, 59, 59).unwrap()));
        assert!(!w.contains(Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap()));
    }

    // Status 推导

    #[test]
    fn r575_infer_status_zero_amount_is_always_ok() {
        assert_eq!(infer_status(0, 0, 80), BudgetPolicyStatus::Ok);
        assert_eq!(infer_status(100, 0, 80), BudgetPolicyStatus::Ok);
    }

    #[test]
    fn r575_infer_status_below_warn_threshold_is_ok() {
        assert_eq!(infer_status(50, 100, 80), BudgetPolicyStatus::Ok);
        assert_eq!(infer_status(79, 100, 80), BudgetPolicyStatus::Ok);
    }

    #[test]
    fn r575_infer_status_at_warn_threshold_is_warning() {
        // warn_percent=80, amount=100, threshold=80
        assert_eq!(infer_status(80, 100, 80), BudgetPolicyStatus::Warning);
        assert_eq!(infer_status(90, 100, 80), BudgetPolicyStatus::Warning);
    }

    #[test]
    fn r575_infer_status_at_or_above_amount_is_hard_stop() {
        assert_eq!(infer_status(100, 100, 80), BudgetPolicyStatus::HardStop);
        assert_eq!(infer_status(150, 100, 80), BudgetPolicyStatus::HardStop);
    }

    #[test]
    fn r575_infer_status_warn_threshold_uses_ceil() {
        // amount=33, warn=80 -> threshold = ceil(33*80/100) = ceil(26.4) = 27
        assert_eq!(infer_status(26, 33, 80), BudgetPolicyStatus::Ok);
        assert_eq!(infer_status(27, 33, 80), BudgetPolicyStatus::Warning);
    }

    // normalize_scope_name

    #[test]
    fn r575_normalize_company_scope_keeps_raw_name() {
        assert_eq!(normalize_scope_name("company", "Acme Corp"), "Acme Corp");
    }

    #[test]
    fn r575_normalize_other_scope_trims_whitespace() {
        assert_eq!(normalize_scope_name("agent", "  Agent X  "), "Agent X");
    }

    #[test]
    fn r575_normalize_empty_other_scope_falls_back_to_scope_type() {
        assert_eq!(normalize_scope_name("project", "   "), "project");
        assert_eq!(normalize_scope_name("agent", ""), "agent");
    }

    // WindowKind / Status roundtrip

    #[test]
    fn r575_window_kind_roundtrip() {
        for k in [BudgetWindowKind::CalendarMonthUtc, BudgetWindowKind::Lifetime] {
            assert_eq!(BudgetWindowKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(BudgetWindowKind::parse("bogus"), None);
    }

    #[test]
    fn r575_policy_status_roundtrip() {
        for s in [
            BudgetPolicyStatus::Ok,
            BudgetPolicyStatus::Warning,
            BudgetPolicyStatus::HardStop,
        ] {
            assert_eq!(BudgetPolicyStatus::parse(s.as_str()), Some(s));
        }
        assert_eq!(BudgetPolicyStatus::parse("bogus"), None);
    }

    #[test]
    fn r575_policy_status_is_at_or_above_warning() {
        assert!(!BudgetPolicyStatus::Ok.is_at_or_above_warning());
        assert!(BudgetPolicyStatus::Warning.is_at_or_above_warning());
        assert!(BudgetPolicyStatus::HardStop.is_at_or_above_warning());
    }

    // Hooks

    #[derive(Default)]
    struct CountingHook {
        hard_stops: std::sync::atomic::AtomicU32,
        warnings: std::sync::atomic::AtomicU32,
        resolves: std::sync::atomic::AtomicU32,
    }

    #[async_trait]
    impl BudgetEnforcementHook for CountingHook {
        async fn on_hard_stop(&self, _: &BudgetEnforcementScope) -> BudgetResult<()> {
            self.hard_stops.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
        async fn on_warning(&self, _: &BudgetEnforcementScope) -> BudgetResult<()> {
            self.warnings.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
        async fn on_resolve(&self, _: &BudgetEnforcementScope) -> BudgetResult<()> {
            self.resolves.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn r575_counting_hook_dispatches_all_phases() {
        let hook = CountingHook::default();
        let scope = BudgetEnforcementScope {
            company_id: Uuid::new_v4(),
            scope_type: "agent".into(),
            scope_id: Uuid::new_v4(),
        };
        block_on(async {
            hook.on_hard_stop(&scope).await.unwrap();
            hook.on_warning(&scope).await.unwrap();
            hook.on_resolve(&scope).await.unwrap();
        });
        assert_eq!(hook.hard_stops.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(hook.warnings.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(hook.resolves.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn r575_noop_hook_is_default_no_op() {
        let h = NoopEnforcementHook;
        let scope = BudgetEnforcementScope {
            company_id: Uuid::new_v4(),
            scope_type: "company".into(),
            scope_id: Uuid::new_v4(),
        };
        block_on(async {
            assert!(h.on_hard_stop(&scope).await.is_ok());
            assert!(h.on_warning(&scope).await.is_ok());
            assert!(h.on_resolve(&scope).await.is_ok());
        });
    }

    #[test]
    fn r575_policy_status_classification_table() {
        // amount=1000, warn=80 -> warn_threshold = 800
        assert_eq!(infer_status(0, 1000, 80), BudgetPolicyStatus::Ok);
        assert_eq!(infer_status(799, 1000, 80), BudgetPolicyStatus::Ok);
        assert_eq!(infer_status(800, 1000, 80), BudgetPolicyStatus::Warning);
        assert_eq!(infer_status(999, 1000, 80), BudgetPolicyStatus::Warning);
        assert_eq!(infer_status(1000, 1000, 80), BudgetPolicyStatus::HardStop);
        assert_eq!(infer_status(1500, 1000, 80), BudgetPolicyStatus::HardStop);
    }

    #[test]
    fn r575_infer_status_with_warn_percent_zero_skips_warning() {
        // warn_percent <= 0 视为不启用警告，所以即使是 observed > 0 也不警告
        assert_eq!(infer_status(0, 100, 0), BudgetPolicyStatus::Ok);
        assert_eq!(infer_status(1, 100, 0), BudgetPolicyStatus::Ok);
        assert_eq!(infer_status(99, 100, 0), BudgetPolicyStatus::Ok);
    }
}
