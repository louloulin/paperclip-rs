#![forbid(unsafe_code)]
//! `pc-recovery-observability` —— recovery observability report service。
//!
//! 对应 Node `server/src/services/recovery-observability.ts`（371 行）。
//!
//! 设计目标：1:1 复刻
//! - [`WeeklyRecoveryRate`] / [`RecoveryRateAlert`] / [`RecoveryCauseGroup`] /
//!   [`RecoveryHandoffSummary`] / [`RecoveryCauseRouting`] / [`RecoveryObservabilityReport`]：
//!   typed DTO
//! - [`classify_recovery_handoff`] —— handoff 分类（与 Node 1:1）
//! - [`evaluate_recovery_rate_alert`] —— pure alert 评估（与 Node 1:1）
//! - [`RecoveryObservabilityService`] —— 集成 DB 查询 + 聚合 + 报告生成
//!
//! 与 Node 的差异：
//! - DB 查询通过 [`RecoveryDataSource`] trait 注入（测试用 fake；生产连真实 Postgres）
//! - `now` 通过 `Options` 注入

use std::collections::HashMap;

use async_trait::async_trait;
use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Timelike, Utc};
use serde::{Deserialize, Serialize};

// ============================================================================
// Constants
// ============================================================================

/// 默认恢复率告警阈值（与 Node `DEFAULT_RECOVERY_RATE_THRESHOLD_PERCENT = 2` 1:1 对齐）。
pub const DEFAULT_RECOVERY_RATE_THRESHOLD_PERCENT: f64 = 2.0;

/// 默认窗口周数（与 Node `DEFAULT_WINDOW_WEEKS = 8` 1:1 对齐）。
pub const DEFAULT_WINDOW_WEEKS: u32 = 8;

/// 窗口周数上限（与 Node `MAX_WINDOW_WEEKS = 104` 1:1 对齐）。
pub const MAX_WINDOW_WEEKS: u32 = 104;

// ============================================================================
// DTO
// ============================================================================

/// 周恢复率（与 Node `WeeklyRecoveryRate` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WeeklyRecoveryRate {
    pub week_start: String,
    pub runs: i64,
    pub recovery_actions: i64,
    pub rate_percent: f64,
}

/// 告警（与 Node `RecoveryRateAlert` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryRateAlert {
    pub threshold_percent: f64,
    pub breached: bool,
    pub breached_weeks: Vec<WeeklyRecoveryRate>,
    pub latest_week: Option<WeeklyRecoveryRate>,
    pub latest_week_breached: bool,
}

/// 原因分组（与 Node `RecoveryCauseGroup` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryCauseGroup {
    pub cause: String,
    pub latest_run_error_code: String,
    pub count: i64,
    pub active_count: i64,
    pub resolved_count: i64,
    pub cancelled_count: i64,
}

/// handoff 分类（与 Node `HandoffClass` 1:1 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffClass {
    SelfRecovery,
    HandedBack,
    OwnerCompleted,
    BoardOwned,
    Active,
    Other,
}

/// handoff 汇总（与 Node `RecoveryHandoffSummary` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryHandoffSummary {
    pub resolved_takeovers: i64,
    pub handed_back: i64,
    pub owner_completed: i64,
    pub other_takeover: i64,
    pub self_recovery: i64,
    pub board_owned: i64,
    pub active_takeovers: i64,
    pub handed_back_ratio: Option<f64>,
    pub owner_completed_ratio: Option<f64>,
}

/// per-cause routing（与 Node `RecoveryCauseRouting` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryCauseRouting {
    pub cause: String,
    pub total: i64,
    pub active: i64,
    pub retried_by_original_succeeded: i64,
    pub handed_back: i64,
    pub owner_completed: i64,
    pub escalated: i64,
    pub false_positive: i64,
    pub cancelled: i64,
}

/// 完整报告（与 Node `RecoveryObservabilityReport` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryObservabilityReport {
    pub company_id: String,
    pub generated_at: String,
    pub window: Window,
    pub threshold_percent: f64,
    pub weekly: Vec<WeeklyRecoveryRate>,
    pub alert: RecoveryRateAlert,
    pub by_cause: Vec<RecoveryCauseGroup>,
    pub handoff: RecoveryHandoffSummary,
    pub per_cause_routing: Vec<RecoveryCauseRouting>,
}

/// 窗口信息。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Window {
    pub weeks: u32,
    pub since: String,
}

// ============================================================================
// Recovery action facts (DB 行投影)
// ============================================================================

/// recovery action + joined issue 投影（与 Node `RecoveryActionFacts` 1:1 对齐）。
#[derive(Debug, Clone)]
pub struct RecoveryActionFacts {
    pub status: String,
    pub outcome: Option<String>,
    pub owner_agent_id: Option<String>,
    pub return_owner_agent_id: Option<String>,
    pub final_assignee_agent_id: Option<String>,
    pub final_issue_status: Option<String>,
}

// ============================================================================
// Pure helpers
// ============================================================================

/// 4 舍 5 入到 2 位小数（与 Node `round2` 1:1 对齐）。
pub fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// 计算指定 UTC 周的 Monday 起点（与 Node `utcWeekStart` 1:1 对齐）。
///
/// `weeks_ago == 0` → 当前周 Monday；`weeks_ago == 1` → 上周 Monday；以此类推。
pub fn utc_week_start(now: DateTime<Utc>, weeks_ago: u32) -> DateTime<Utc> {
    // 计算 UTC midnight
    let date = now.date_naive();
    let utc_midnight = Utc
        .from_utc_datetime(&date.and_hms_opt(0, 0, 0).unwrap());
    // day_of_week: 0 = Sunday, 1 = Monday, ..., 6 = Saturday
    let day_of_week = utc_midnight.weekday().num_days_from_sunday();
    // monday_offset = days since Monday
    let monday_offset = (day_of_week + 6) % 7;
    let monday = utc_midnight - Duration::days(monday_offset as i64);
    monday - Duration::weeks(weeks_ago as i64)
}

/// 把 DateTime 截断到 Monday 并格式化为 `YYYY-MM-DD`（与 Node 1:1 对齐）。
pub fn week_start_iso(now: DateTime<Utc>, weeks_ago: u32) -> String {
    let d = utc_week_start(now, weeks_ago);
    d.format("%Y-%m-%d").to_string()
}

/// 分类 handoff（与 Node `classifyRecoveryHandoff` 1:1 对齐）。
pub fn classify_recovery_handoff(facts: &RecoveryActionFacts) -> HandoffClass {
    const ACTIVE_STATUSES: &[&str] = &["active", "escalated"];
    const TERMINAL_ISSUE_STATUSES: &[&str] = &["done", "in_review"];

    if ACTIVE_STATUSES.contains(&facts.status.as_str()) {
        return HandoffClass::Active;
    }
    if facts.owner_agent_id.is_none() {
        return HandoffClass::BoardOwned;
    }
    // Original agent recovered its own issue
    if facts.owner_agent_id == facts.return_owner_agent_id {
        return HandoffClass::SelfRecovery;
    }
    // Genuine takeover: recovery owner differs from the original assignee
    let landed_elsewhere = facts.final_assignee_agent_id.is_some()
        && facts.final_assignee_agent_id.as_deref() != facts.owner_agent_id.as_deref();
    if landed_elsewhere {
        return HandoffClass::HandedBack;
    }
    let owner_kept = facts.final_assignee_agent_id.as_deref() == facts.owner_agent_id.as_deref();
    if owner_kept
        && facts
            .final_issue_status
            .as_deref()
            .map(|s| TERMINAL_ISSUE_STATUSES.contains(&s))
            .unwrap_or(false)
    {
        return HandoffClass::OwnerCompleted;
    }
    HandoffClass::Other
}

/// 纯函数：评估告警（与 Node `evaluateRecoveryRateAlert` 1:1 对齐）。
pub fn evaluate_recovery_rate_alert(
    weekly: &[WeeklyRecoveryRate],
    threshold_percent: f64,
) -> RecoveryRateAlert {
    let breached_weeks: Vec<WeeklyRecoveryRate> = weekly
        .iter()
        .filter(|w| w.rate_percent > threshold_percent)
        .cloned()
        .collect();
    let latest_week = weekly.last().cloned();
    RecoveryRateAlert {
        threshold_percent,
        breached: !breached_weeks.is_empty(),
        breached_weeks,
        latest_week_breached: latest_week
            .as_ref()
            .map(|w| w.rate_percent > threshold_percent)
            .unwrap_or(false),
        latest_week,
    }
}

// ============================================================================
// Data source trait
// ============================================================================

/// 抽象 DB 数据源（测试可注入 fake）。
#[async_trait]
pub trait RecoveryDataSource: Send + Sync {
    /// 返回 `(week_start_iso, runs_count)` 周桶 runs。
    async fn weekly_runs(
        &self,
        company_id: &str,
        since: DateTime<Utc>,
    ) -> Vec<(String, i64)>;

    /// 返回 `(week_start_iso, actions_count)` 周桶 recovery actions。
    async fn weekly_actions(
        &self,
        company_id: &str,
        since: DateTime<Utc>,
    ) -> Vec<(String, i64)>;

    /// 返回按 cause + error_code 分组的 recovery action 计数。
    async fn cause_groups(
        &self,
        company_id: &str,
        since: DateTime<Utc>,
    ) -> Vec<RecoveryCauseGroup>;

    /// 返回 recovery actions + joined issues 投影，供 handoff 分类。
    async fn recovery_action_facts(
        &self,
        company_id: &str,
        since: DateTime<Utc>,
    ) -> Vec<(String, RecoveryActionFacts)>;
}

// ============================================================================
// Service
// ============================================================================

/// Report 选项。
#[derive(Debug, Clone)]
pub struct ReportOptions {
    pub now: Option<DateTime<Utc>>,
    pub weeks: Option<u32>,
    pub threshold_percent: Option<f64>,
}

impl Default for ReportOptions {
    fn default() -> Self {
        Self {
            now: None,
            weeks: None,
            threshold_percent: None,
        }
    }
}

/// Service。
pub struct RecoveryObservabilityService<D: RecoveryDataSource> {
    data: Box<D>,
}

impl<D: RecoveryDataSource> RecoveryObservabilityService<D> {
    pub fn new(data: D) -> Self {
        Self {
            data: Box::new(data),
        }
    }

    /// 生成报告。
    pub async fn report(
        &self,
        company_id: &str,
        opts: ReportOptions,
    ) -> RecoveryObservabilityReport {
        let now = opts.now.unwrap_or_else(Utc::now);
        let raw_weeks = opts.weeks.unwrap_or(DEFAULT_WINDOW_WEEKS);
        let weeks = std::cmp::min(MAX_WINDOW_WEEKS, std::cmp::max(1, raw_weeks));
        let threshold_percent = opts
            .threshold_percent
            .unwrap_or(DEFAULT_RECOVERY_RATE_THRESHOLD_PERCENT);

        let since = utc_week_start(now, weeks.saturating_sub(1));
        let since_iso = since.format("%Y-%m-%d").to_string();
        let generated_at = now.to_utc().to_rfc3339();

        // 拉数据
        let runs_rows = self.data.weekly_runs(company_id, since).await;
        let actions_rows = self.data.weekly_actions(company_id, since).await;
        let cause_rows = self.data.cause_groups(company_id, since).await;
        let facts_rows = self.data.recovery_action_facts(company_id, since).await;

        // 1. weekly
        let mut runs_by_week: HashMap<String, i64> = HashMap::new();
        for (ws, c) in runs_rows {
            runs_by_week.insert(ws, c);
        }
        let mut actions_by_week: HashMap<String, i64> = HashMap::new();
        for (ws, c) in actions_rows {
            actions_by_week.insert(ws, c);
        }
        let weekly: Vec<WeeklyRecoveryRate> = (0..weeks)
            .map(|idx| {
                let week_start = week_start_iso(now, weeks.saturating_sub(1).saturating_sub(idx));
                let runs = runs_by_week.get(&week_start).copied().unwrap_or(0);
                let recovery_actions = actions_by_week.get(&week_start).copied().unwrap_or(0);
                let rate_percent = if runs > 0 {
                    round2((recovery_actions as f64 / runs as f64) * 100.0)
                } else {
                    0.0
                };
                WeeklyRecoveryRate {
                    week_start,
                    runs,
                    recovery_actions,
                    rate_percent,
                }
            })
            .collect();

        // 2. alert
        let alert = evaluate_recovery_rate_alert(&weekly, threshold_percent);

        // 3. by_cause（已经在数据源里聚合好）
        let by_cause = cause_rows;

        // 4. handoff + per-cause routing
        let mut handoff = RecoveryHandoffSummary {
            resolved_takeovers: 0,
            handed_back: 0,
            owner_completed: 0,
            other_takeover: 0,
            self_recovery: 0,
            board_owned: 0,
            active_takeovers: 0,
            handed_back_ratio: None,
            owner_completed_ratio: None,
        };

        let mut routing_by_cause: HashMap<String, RecoveryCauseRouting> = HashMap::new();
        for (cause, facts) in facts_rows {
            let klass = classify_recovery_handoff(&facts);
            let routing = routing_by_cause
                .entry(cause.clone())
                .or_insert_with(|| RecoveryCauseRouting {
                    cause: cause.clone(),
                    total: 0,
                    active: 0,
                    retried_by_original_succeeded: 0,
                    handed_back: 0,
                    owner_completed: 0,
                    escalated: 0,
                    false_positive: 0,
                    cancelled: 0,
                });
            routing.total += 1;

            if klass == HandoffClass::Active {
                routing.active += 1;
                if facts.status == "escalated" {
                    routing.escalated += 1;
                }
                if facts.owner_agent_id.is_some()
                    && facts.owner_agent_id != facts.return_owner_agent_id
                {
                    handoff.active_takeovers += 1;
                }
                continue;
            }

            // 解析 routing verification counters
            if facts.status == "escalated" {
                routing.escalated += 1;
            }
            if facts.outcome.as_deref() == Some("false_positive") {
                routing.false_positive += 1;
            }
            if facts.status == "cancelled"
                && facts.outcome.as_deref() != Some("false_positive")
            {
                routing.cancelled += 1;
            }

            match klass {
                HandoffClass::SelfRecovery => {
                    handoff.self_recovery += 1;
                    if facts.status == "resolved" {
                        routing.retried_by_original_succeeded += 1;
                    }
                }
                HandoffClass::HandedBack => {
                    handoff.handed_back += 1;
                    handoff.resolved_takeovers += 1;
                    routing.handed_back += 1;
                }
                HandoffClass::OwnerCompleted => {
                    handoff.owner_completed += 1;
                    handoff.resolved_takeovers += 1;
                    routing.owner_completed += 1;
                }
                HandoffClass::BoardOwned => {
                    handoff.board_owned += 1;
                }
                HandoffClass::Other => {
                    if facts.owner_agent_id.is_some()
                        && facts.owner_agent_id != facts.return_owner_agent_id
                    {
                        handoff.other_takeover += 1;
                    }
                }
                _ => {}
            }
        }

        let decided = handoff.handed_back + handoff.owner_completed;
        if decided > 0 {
            handoff.handed_back_ratio = Some(round2((handoff.handed_back as f64 / decided as f64) * 100.0));
            handoff.owner_completed_ratio =
                Some(round2((handoff.owner_completed as f64 / decided as f64) * 100.0));
        }

        let mut per_cause_routing: Vec<RecoveryCauseRouting> =
            routing_by_cause.into_values().collect();
        per_cause_routing.sort_by(|a, b| b.total.cmp(&a.total));

        RecoveryObservabilityReport {
            company_id: company_id.to_string(),
            generated_at,
            window: Window {
                weeks,
                since: since_iso,
            },
            threshold_percent,
            weekly,
            alert,
            by_cause,
            handoff,
            per_cause_routing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ----- round2 -----

    #[test]
    fn r714_round2() {
        assert_eq!(round2(12.345), 12.35);
        assert_eq!(round2(0.0), 0.0);
        assert_eq!(round2(0.001), 0.0);
        assert_eq!(round2(99.999), 100.0);
    }

    // ----- utcWeekStart -----

    #[test]
    fn r714_week_start_on_monday() {
        // 2025-06-02 是 Monday
        let monday = Utc.with_ymd_and_hms(2025, 6, 2, 10, 0, 0).unwrap();
        let monday_start = utc_week_start(monday, 0);
        assert_eq!(monday_start.date_naive(), NaiveDate::from_ymd_opt(2025, 6, 2).unwrap());
        assert_eq!(monday_start.hour(), 0);
    }

    #[test]
    fn r714_week_start_on_wednesday() {
        // 2025-06-04 是 Wednesday → Monday = 2025-06-02
        let wed = Utc.with_ymd_and_hms(2025, 6, 4, 15, 0, 0).unwrap();
        let monday_start = utc_week_start(wed, 0);
        assert_eq!(
            monday_start.date_naive(),
            NaiveDate::from_ymd_opt(2025, 6, 2).unwrap()
        );
    }

    #[test]
    fn r714_week_start_on_sunday() {
        // 2025-06-08 是 Sunday → Monday = 2025-06-02
        let sun = Utc.with_ymd_and_hms(2025, 6, 8, 23, 0, 0).unwrap();
        let monday_start = utc_week_start(sun, 0);
        assert_eq!(
            monday_start.date_naive(),
            NaiveDate::from_ymd_opt(2025, 6, 2).unwrap()
        );
    }

    #[test]
    fn r714_week_start_weeks_ago() {
        let now = Utc.with_ymd_and_hms(2025, 6, 4, 10, 0, 0).unwrap();
        let last_week = utc_week_start(now, 1);
        assert_eq!(
            last_week.date_naive(),
            NaiveDate::from_ymd_opt(2025, 5, 26).unwrap()
        );
    }

    // ----- classify -----

    fn facts(status: &str, owner: Option<&str>, ret: Option<&str>, fin: Option<&str>, fin_status: Option<&str>) -> RecoveryActionFacts {
        RecoveryActionFacts {
            status: status.into(),
            outcome: None,
            owner_agent_id: owner.map(String::from),
            return_owner_agent_id: ret.map(String::from),
            final_assignee_agent_id: fin.map(String::from),
            final_issue_status: fin_status.map(String::from),
        }
    }

    #[test]
    fn r714_classify_active_status() {
        let f = facts("active", Some("a"), Some("a"), Some("a"), Some("open"));
        assert_eq!(classify_recovery_handoff(&f), HandoffClass::Active);
    }

    #[test]
    fn r714_classify_escalated_is_active() {
        let f = facts("escalated", Some("a"), Some("a"), Some("a"), Some("open"));
        assert_eq!(classify_recovery_handoff(&f), HandoffClass::Active);
    }

    #[test]
    fn r714_classify_board_owned_no_owner() {
        let f = facts("resolved", None, None, Some("a"), Some("done"));
        assert_eq!(classify_recovery_handoff(&f), HandoffClass::BoardOwned);
    }

    #[test]
    fn r714_classify_self_recovery_same_owner_return() {
        let f = facts("resolved", Some("a"), Some("a"), Some("a"), Some("done"));
        assert_eq!(classify_recovery_handoff(&f), HandoffClass::SelfRecovery);
    }

    #[test]
    fn r714_classify_handed_back_landed_elsewhere() {
        let f = facts("resolved", Some("manager"), Some("a"), Some("a"), Some("done"));
        assert_eq!(classify_recovery_handoff(&f), HandoffClass::HandedBack);
    }

    #[test]
    fn r714_classify_owner_completed_terminal() {
        let f = facts("resolved", Some("manager"), Some("a"), Some("manager"), Some("done"));
        assert_eq!(classify_recovery_handoff(&f), HandoffClass::OwnerCompleted);
    }

    #[test]
    fn r714_classify_owner_completed_in_review_terminal() {
        let f = facts("resolved", Some("manager"), Some("a"), Some("manager"), Some("in_review"));
        assert_eq!(classify_recovery_handoff(&f), HandoffClass::OwnerCompleted);
    }

    #[test]
    fn r714_classify_other_fallback() {
        let f = facts("resolved", Some("manager"), Some("a"), Some("manager"), Some("open"));
        assert_eq!(classify_recovery_handoff(&f), HandoffClass::Other);
    }

    // ----- evaluateRecoveryRateAlert -----

    fn week(week_start: &str, runs: i64, actions: i64, rate: f64) -> WeeklyRecoveryRate {
        WeeklyRecoveryRate {
            week_start: week_start.into(),
            runs,
            recovery_actions: actions,
            rate_percent: rate,
        }
    }

    #[test]
    fn r714_alert_not_breached() {
        let weekly = vec![week("2025-06-02", 100, 1, 1.0)];
        let alert = evaluate_recovery_rate_alert(&weekly, 2.0);
        assert!(!alert.breached);
        assert!(alert.breached_weeks.is_empty());
        assert!(!alert.latest_week_breached);
    }

    #[test]
    fn r714_alert_breached_when_rate_above_threshold() {
        let weekly = vec![
            week("2025-05-26", 100, 1, 1.0),
            week("2025-06-02", 100, 5, 5.0),
        ];
        let alert = evaluate_recovery_rate_alert(&weekly, 2.0);
        assert!(alert.breached);
        assert_eq!(alert.breached_weeks.len(), 1);
        assert!(alert.latest_week_breached);
    }

    #[test]
    fn r714_alert_threshold_not_strict_greater() {
        // rate_percent == threshold → NOT breached（`>` not `>=`）
        let weekly = vec![week("2025-06-02", 100, 2, 2.0)];
        let alert = evaluate_recovery_rate_alert(&weekly, 2.0);
        assert!(!alert.breached);
    }

    #[test]
    fn r714_alert_empty_weekly() {
        let alert = evaluate_recovery_rate_alert(&[], 2.0);
        assert!(!alert.breached);
        assert!(alert.latest_week.is_none());
        assert!(!alert.latest_week_breached);
    }

    // ----- service integration -----

    /// Fake data source for testing
    struct FakeData {
        runs: Vec<(String, i64)>,
        actions: Vec<(String, i64)>,
        causes: Vec<RecoveryCauseGroup>,
        facts: Vec<(String, RecoveryActionFacts)>,
    }

    #[async_trait]
    impl RecoveryDataSource for FakeData {
        async fn weekly_runs(&self, _: &str, _: DateTime<Utc>) -> Vec<(String, i64)> {
            self.runs.clone()
        }
        async fn weekly_actions(&self, _: &str, _: DateTime<Utc>) -> Vec<(String, i64)> {
            self.actions.clone()
        }
        async fn cause_groups(&self, _: &str, _: DateTime<Utc>) -> Vec<RecoveryCauseGroup> {
            self.causes.clone()
        }
        async fn recovery_action_facts(
            &self,
            _: &str,
            _: DateTime<Utc>,
        ) -> Vec<(String, RecoveryActionFacts)> {
            self.facts.clone()
        }
    }

    #[tokio::test]
    async fn r714_service_report_assembles_all_fields() {
        let now = Utc.with_ymd_and_hms(2025, 6, 4, 10, 0, 0).unwrap();
        let data = FakeData {
            runs: vec![(week_start_iso(now, 0), 100)],
            actions: vec![(week_start_iso(now, 0), 5)],
            causes: vec![RecoveryCauseGroup {
                cause: "stall".into(),
                latest_run_error_code: "TIMEOUT".into(),
                count: 5,
                active_count: 1,
                resolved_count: 3,
                cancelled_count: 1,
            }],
            facts: vec![
                // Owner completed: owner == final_assignee, terminal status
                (
                    "stall".into(),
                    facts("resolved", Some("mgr"), Some("orig"), Some("mgr"), Some("done")),
                ),
                // Active takeover: owner != return_owner, status=active
                (
                    "stall".into(),
                    facts("active", Some("mgr"), Some("orig"), None, None),
                ),
            ],
        };
        let svc = RecoveryObservabilityService::new(data);
        let report = svc
            .report(
                "co-1",
                ReportOptions {
                    now: Some(now),
                    weeks: Some(8),
                    threshold_percent: Some(2.0),
                },
            )
            .await;

        assert_eq!(report.company_id, "co-1");
        assert_eq!(report.window.weeks, 8);
        assert_eq!(report.threshold_percent, 2.0);
        assert_eq!(report.weekly.len(), 8);
        assert!(report.alert.breached); // 5/100 = 5%
        assert_eq!(report.by_cause.len(), 1);
        assert_eq!(report.by_cause[0].cause, "stall");
        assert_eq!(report.handoff.owner_completed, 1);
        assert_eq!(report.handoff.active_takeovers, 1);
        assert_eq!(report.per_cause_routing.len(), 1);
        assert_eq!(report.per_cause_routing[0].cause, "stall");
    }

    #[tokio::test]
    async fn r714_service_clamps_weeks() {
        let data = FakeData {
            runs: vec![],
            actions: vec![],
            causes: vec![],
            facts: vec![],
        };
        let svc = RecoveryObservabilityService::new(data);

        // weeks > MAX → clamp
        let r1 = svc
            .report("co", ReportOptions {
                now: Some(Utc::now()),
                weeks: Some(9999),
                ..Default::default()
            })
            .await;
        assert_eq!(r1.window.weeks, MAX_WINDOW_WEEKS);

        // weeks == 0 → clamp to 1
        let r2 = svc
            .report("co", ReportOptions {
                now: Some(Utc::now()),
                weeks: Some(0),
                ..Default::default()
            })
            .await;
        assert_eq!(r2.window.weeks, 1);
    }

    #[test]
    fn r714_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RecoveryObservabilityReport>();
        assert_send_sync::<WeeklyRecoveryRate>();
        assert_send_sync::<RecoveryHandoffSummary>();
        assert_send_sync::<RecoveryCauseRouting>();
    }

    #[test]
    fn r714_handoff_class_serializes_snake_case() {
        let v = serde_json::to_value(HandoffClass::SelfRecovery).unwrap();
        assert_eq!(v, serde_json::json!("self_recovery"));
    }

    #[test]
    fn r714_report_serializes_camel_case() {
        let r = RecoveryObservabilityReport {
            company_id: "x".into(),
            generated_at: "2025-01-01T00:00:00Z".into(),
            window: Window {
                weeks: 8,
                since: "2025-01-01".into(),
            },
            threshold_percent: 2.0,
            weekly: vec![],
            alert: RecoveryRateAlert {
                threshold_percent: 2.0,
                breached: false,
                breached_weeks: vec![],
                latest_week: None,
                latest_week_breached: false,
            },
            by_cause: vec![],
            handoff: RecoveryHandoffSummary {
                resolved_takeovers: 0,
                handed_back: 0,
                owner_completed: 0,
                other_takeover: 0,
                self_recovery: 0,
                board_owned: 0,
                active_takeovers: 0,
                handed_back_ratio: None,
                owner_completed_ratio: None,
            },
            per_cause_routing: vec![],
        };
        let v = serde_json::to_value(&r).unwrap();
        assert!(v.get("companyId").is_some());
        assert!(v.get("byCause").is_some());
        assert!(v.get("perCauseRouting").is_some());
    }
}
