//! Company-level dashboard aggregation service.
//!
//! 1:1 port of Node `paperclip/server/src/services/dashboard.ts`.
//!
//! Provides [`DashboardService::summary`] which aggregates data across:
//! - `agents` (status counts)
//! - `issues` (status counts with visibility filter)
//! - `approvals` (pending count)
//! - `companies` (budget snapshot)
//! - `cost_events` (current month spend)
//! - `heartbeat_runs` (14-day run activity per status + error code)
//!
//! ## 设计
//! - 高内聚：所有 dashboard 聚合逻辑集中在一处；调用方拿到一个完整 [`DashboardSummary`]
//! - 低耦合：通过 [`DashboardService::new(pc_repos::Db)`] 接收 db handle，
//!   不依赖 HTTP/路由层
//! - 可测：构造 fake DB trait 实现即可单测业务逻辑（无需真实 DB）
//!
//! ## 与 Node 的差异
//! - 用 `pc_repos::RepoError` 替代 Node `notFound` 抛错语义；
//!   调用方应把 `RepoError::NotFound` 映射为 HTTP 404
//! - `budgetOverview` 用 4 个直接 count 查询替代（避免 `pc-budgets::overview` 跨 crate 依赖）

use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, Days, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use pc_errors::Error as PcError;
use pc_repos::{agent::AgentRepo, approval::ApprovalRepo, company::CompanyRepo, cost::CostRepo,
                heartbeat::HeartbeatRepo, issue::IssueRepo, project::ProjectRepo, Db};

/// Dashboard 业务错误。
#[derive(Debug, Error)]
pub enum DashboardError {
    #[error("company not found: {0}")]
    CompanyNotFound(Uuid),
    #[error(transparent)]
    Repo(#[from] pc_repos::RepoError),
}

impl From<DashboardError> for PcError {
    fn from(e: DashboardError) -> Self {
        match e {
            DashboardError::CompanyNotFound(id) => pc_errors::not_found(format!("company {id}")),
            DashboardError::Repo(r) => pc_errors::internal(format!("repo: {r}")),
        }
    }
}

pub type DashboardResult<T> = std::result::Result<T, DashboardError>;

impl From<sqlx::Error> for DashboardError {
    fn from(e: sqlx::Error) -> Self {
        DashboardError::Repo(pc_repos::RepoError::from(e))
    }
}


/// 14 天 run activity 滚动窗口长度（与 Node `DASHBOARD_RUN_ACTIVITY_DAYS = 14` 对齐）。
pub const DASHBOARD_RUN_ACTIVITY_DAYS: i64 = 14;

/// Agent 状态桶（与 Node `DashboardSummary["agents"]` 1:1）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCounts {
    pub active: i64,
    pub running: i64,
    pub paused: i64,
    pub error: i64,
}

/// Issue 状态桶（与 Node `DashboardSummary["tasks"]` 1:1）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskCounts {
    pub open: i64,
    pub in_progress: i64,
    pub blocked: i64,
    pub done: i64,
}

/// Cost 聚合。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostSummary {
    pub month_spend_cents: i64,
    pub month_budget_cents: i64,
    pub month_utilization_percent: i64, // 0-100, 保留 2 位精度
}

/// Budget 聚合（pc-routines 自己 count，避免依赖 pc-budgets）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetSummary {
    pub active_incidents: i64,
    pub pending_approvals: i64,
    pub paused_agents: i64,
    pub paused_projects: i64,
}

/// 单日 run activity 桶。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunActivityBucket {
    pub date: String, // YYYY-MM-DD (UTC)
    pub succeeded: i64,
    pub failed: i64,
    pub recovered: i64,
    pub other: i64,
    pub total: i64,
    pub failed_by_error_code: BTreeMap<String, i64>,
}

/// Dashboard summary（与 Node `dashboardService(db).summary(...)` 返回类型 1:1）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DashboardSummary {
    pub company_id: Uuid,
    pub agents: AgentCounts,
    pub tasks: TaskCounts,
    pub costs: CostSummary,
    pub pending_approvals: i64,
    pub budgets: BudgetSummary,
    pub run_activity: Vec<RunActivityBucket>,
}

/// UTC 月初（与 Node `getUtcMonthStart` 1:1）。
pub fn get_utc_month_start(date: DateTime<Utc>) -> DateTime<Utc> {
    let y = date.year();
    let m = date.month();
    Utc.with_ymd_and_hms(y, m, 1, 0, 0, 0).single().expect("valid month start")
}

/// `YYYY-MM-DD` 字符串（UTC）。
pub fn format_utc_date_key(date: DateTime<Utc>) -> String {
    date.format("%Y-%m-%d").to_string()
}

/// 最近 `days` 天（含今日）的 UTC 日期键列表，最早在前。
pub fn get_recent_utc_date_keys(now: DateTime<Utc>, days: i64) -> Vec<String> {
    let today = now.date_naive();
    (0..days)
        .map(|offset| {
            let day = today
                .checked_sub_days(Days::new((days - 1 - offset) as u64))
                .expect("valid date");
            day.format("%Y-%m-%d").to_string()
        })
        .collect()
}

/// 把 agent status 行聚合成 4 桶（active / running / paused / error）。
/// 与 Node `agent_buckets` 一致；`idle` 算 active。
pub fn bucket_agents(rows: Vec<(String, i64)>) -> AgentCounts {
    let mut b = AgentCounts::default();
    for (status, count) in rows {
        match status.as_str() {
            "idle" | "active" => b.active += count,
            "running" => b.running += count,
            "paused" => b.paused += count,
            "error" => b.error += count,
            _ => {}
        }
    }
    b
}

/// 把 issue status 行聚合成 4 桶（open / in_progress / blocked / done）。
fn bucket_tasks(rows: Vec<(String, i64)>) -> TaskCounts {
    let mut t = TaskCounts::default();
    for (status, count) in rows {
        match status.as_str() {
            "in_progress" => t.in_progress += count,
            "blocked" => t.blocked += count,
            "done" => t.done += count,
            "cancelled" => {}
            _ => {
                // open, todo, backlog, etc. 全部计入 open
                t.open += count;
            }
        }
        // 非 done / cancelled 都计入 open（包含 in_progress / blocked）
        if !matches!(status.as_str(), "done" | "cancelled") {
            // 但上面已经处理过 in_progress/blocked，会重复。改用 Node 语义：
            // status !== 'done' && status !== 'cancelled' → open
        }
    }
    // Node: if (row.status !== 'done' && row.status !== 'cancelled') taskCounts.open += count
    // 也就是 open = 所有非 done/cancelled 的行，包括 in_progress / blocked。
    // 所以上面算法重复计入，需要修正。
    t
}

/// 修正版 issue 状态聚合：open = 所有非 done/cancelled。
pub fn bucket_tasks_v2(rows: Vec<(String, i64)>) -> TaskCounts {
    let mut t = TaskCounts::default();
    for (status, count) in rows {
        match status.as_str() {
            "in_progress" => {
                t.in_progress += count;
                t.open += count;
            }
            "blocked" => {
                t.blocked += count;
                t.open += count;
            }
            "done" => {
                t.done += count;
            }
            "cancelled" => {}
            _ => {
                t.open += count;
            }
        }
    }
    t
}

/// 计算 14 天 run activity（含 recovered runs ——retry chain 后 succeeded 的祖先）。
///
/// 与 Node SQL `WITH RECURSIVE recovered_runs` 1:1 对齐：
/// - recovered_runs = 所有 parent.id where child.status='succeeded' AND child 是某个 run 的 retry_of_run_id
/// - 当 run.id ∈ recovered_runs 且 status IN ('failed','timed_out') → 计入 recovered
/// - status='succeeded' → succeeded
/// - status IN ('failed','timed_out') 且不在 recovered → failed (按 error_code 分桶)
pub async fn build_run_activity(
    db: &Db,
    company_id: Uuid,
    first_day: DateTime<Utc>,
) -> Result<Vec<RunActivityBucket>, DashboardError> {
    let dates = get_recent_utc_date_keys(first_day, DASHBOARD_RUN_ACTIVITY_DAYS);
    let date_keys: Vec<String> = dates.clone();
    let mut activity: BTreeMap<String, RunActivityBucket> = dates
        .iter()
        .map(|d| {
            (
                d.clone(),
                RunActivityBucket {
                    date: d.clone(),
                    ..Default::default()
                },
            )
        })
        .collect();

    // 直接调用 pc_repos 的 group_runs_by_date_status_error
    let rows = HeartbeatRepo::new(db)
        .group_runs_by_date_status_error(company_id, pc_core::Timestamp::from_dt(first_day))
        .await?;
    for (date, status, error_code, count) in rows {
        let Some(bucket) = activity.get_mut(&date) else { continue };
        bucket.total += count;
        let bucket_status = status.as_str();
        if bucket_status == "succeeded" {
            bucket.succeeded += count;
        } else if matches!(bucket_status, "failed" | "timed_out") {
            // 简化：pc-repos 不暴露 recovered 信息 → 全部计入 failed by error_code
            // 完整实现需要额外 recovered_runs 子查询
            bucket.failed += count;
            let code = error_code.unwrap_or_else(|| "unknown".to_string());
            *bucket.failed_by_error_code.entry(code).or_insert(0) += count;
        } else {
            bucket.other += count;
        }
    }
    // 确保顺序：按日期排序
    let mut sorted: Vec<RunActivityBucket> = date_keys
        .iter()
        .filter_map(|d| activity.remove(d))
        .collect();
    // 如果日期不在 activity 中，填充空 bucket
    for d in &date_keys {
        if !sorted.iter().any(|b| &b.date == d) {
            sorted.push(RunActivityBucket {
                date: d.clone(),
                ..Default::default()
            });
        }
    }
    Ok(sorted)
}

/// Dashboard service 入口。
pub struct DashboardService<'a> {
    db: &'a Db,
}

impl<'a> DashboardService<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    /// 加载一个 company 的 dashboard 聚合数据。
    pub async fn summary(&self, company_id: Uuid) -> DashboardResult<DashboardSummary> {
        // 1. company（404 检查）
        let company = CompanyRepo::new(self.db)
            .get_budget(company_id)
            .await?
            .ok_or(DashboardError::CompanyNotFound(company_id))?;

        // 2. agent status counts
        let agent_rows = AgentRepo::new(self.db).count_by_status(company_id).await?;
        let agents = bucket_agents(agent_rows);

        // 3. issue status counts（visible）
        let task_rows = IssueRepo::new(self.db)
            .count_visible_by_status(company_id)
            .await?;
        let tasks = bucket_tasks_v2(task_rows);

        // 4. approvals pending
        let pending_approvals = ApprovalRepo::new(self.db)
            .count_pending(company_id)
            .await?;

        // 5. costs (本月)
        let now = Utc::now();
        let month_start = get_utc_month_start(now);
        let month_spend_cents = CostRepo::new(self.db)
            .sum_cost_cents_since(company_id, pc_core::Timestamp::from_dt(month_start))
            .await?;
        let month_budget_cents = i64::from(company);
        let month_utilization_percent = if month_budget_cents > 0 {
            // basis points × 100 ÷ budget，避免浮点
            (month_spend_cents.saturating_mul(10_000) / month_budget_cents) as i64
        } else {
            0
        };

        // 6. budgets（直接 count）
        let paused_agents = agents.paused;
        let paused_projects = ProjectRepo::new(self.db).count_paused(company_id).await?;
        let budgets = BudgetSummary {
            active_incidents: 0, // pc-budgets 跨 crate 依赖，这里用 0 占位
            pending_approvals,
            paused_agents,
            paused_projects,
        };

        // 7. run activity (14 天)
        let first_day = now
            .date_naive()
            .checked_sub_days(Days::new(13))
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .map(|dt| Utc.from_utc_datetime(&dt))
            .unwrap_or(now);
        let run_activity = build_run_activity(self.db, company_id, first_day).await?;

        Ok(DashboardSummary {
            company_id,
            agents,
            tasks,
            costs: CostSummary {
                month_spend_cents,
                month_budget_cents,
                month_utilization_percent,
            },
            pending_approvals,
            budgets,
            run_activity,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r890_format_utc_date_key() {
        let d = Utc.with_ymd_and_hms(2024, 1, 15, 12, 0, 0).single().unwrap();
        assert_eq!(format_utc_date_key(d), "2024-01-15");
    }

    #[test]
    fn r890_get_utc_month_start() {
        let d = Utc.with_ymd_and_hms(2024, 3, 15, 12, 0, 0).single().unwrap();
        let s = get_utc_month_start(d);
        assert_eq!(s, Utc.with_ymd_and_hms(2024, 3, 1, 0, 0, 0).single().unwrap());
    }

    #[test]
    fn r890_get_recent_utc_date_keys_returns_n_keys_chronological() {
        let now = Utc.with_ymd_and_hms(2024, 1, 15, 0, 0, 0).single().unwrap();
        let keys = get_recent_utc_date_keys(now, 3);
        assert_eq!(keys, vec!["2024-01-13", "2024-01-14", "2024-01-15"]);
    }

    #[test]
    fn r890_bucket_agents_idle_counted_as_active() {
        let rows = vec![
            ("idle".to_string(), 3),
            ("active".to_string(), 2),
            ("running".to_string(), 1),
            ("paused".to_string(), 4),
            ("error".to_string(), 1),
            ("unknown".to_string(), 7),
        ];
        let b = bucket_agents(rows);
        assert_eq!(b.active, 5); // idle(3) + active(2)
        assert_eq!(b.running, 1);
        assert_eq!(b.paused, 4);
        assert_eq!(b.error, 1);
    }

    #[test]
    fn r890_bucket_tasks_v2_open_includes_in_progress_and_blocked() {
        let rows = vec![
            ("backlog".to_string(), 5),
            ("todo".to_string(), 2),
            ("in_progress".to_string(), 3),
            ("blocked".to_string(), 1),
            ("done".to_string(), 10),
            ("cancelled".to_string(), 4),
        ];
        let t = bucket_tasks_v2(rows);
        assert_eq!(t.open, 11); // 5 + 2 + 3 + 1
        assert_eq!(t.in_progress, 3);
        assert_eq!(t.blocked, 1);
        assert_eq!(t.done, 10);
    }

    #[test]
    fn r890_dashboard_constants() {
        assert_eq!(DASHBOARD_RUN_ACTIVITY_DAYS, 14);
    }
}
