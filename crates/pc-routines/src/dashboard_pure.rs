#![forbid(unsafe_code)]

//! Routine dashboard aggregation pure helpers -- 1:1 port of
//! small utility helpers in paperclip/server/src/services/routines/dashboard.ts.
//!
//! R743: 零依赖 helpers (date keys, agent/task buckets).

use chrono::{DateTime, Datelike, Days, TimeZone, Utc};

/// 默认 dashboard 时间窗口（最近 30 天）。
pub const DEFAULT_DASHBOARD_DAYS: i64 = 30;

/// Bucket limits（防 DoS）。
pub const MAX_AGENT_ROWS: usize = 1000;
pub const MAX_TASK_ROWS: usize = 5000;

/// 把 DateTime 截断到 UTC 月初（day=1, hh:mm:ss=00:00:00）。
///
/// 与 Node getUtcMonthStart 1:1 对齐。
pub fn utc_month_start(date: DateTime<Utc>) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(date.year(), date.month(), 1, 0, 0, 0)
        .single()
        .expect("valid month start")
}

/// 格式化 UTC 日期为 YYYY-MM-DD 字符串。
pub fn utc_date_key(date: DateTime<Utc>) -> String {
    date.format("%Y-%m-%d").to_string()
}

/// 生成最近 N 天的 UTC 日期键列表（含今日，按时间正序）。
///
/// 与 Node getRecentUtcDateKeys 1:1。
pub fn recent_utc_date_keys(now: DateTime<Utc>, days: i64) -> Vec<String> {
    if days <= 0 {
        return vec![];
    }
    let today = now.date_naive();
    (0..days)
        .map(|offset| {
            let back = (days - 1 - offset) as u64;
            let day = today
                .checked_sub_days(Days::new(back))
                .unwrap_or(today);
            day.format("%Y-%m-%d").to_string()
        })
        .collect()
}

/// Agent status 桶汇总。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentCounts {
    pub active: i64,
    pub running: i64,
    pub paused: i64,
    pub error: i64,
}

/// 把 (status, count) 行聚合成 4 桶。
///
/// 与 Node agent_buckets 1:1：idle/active 合并为 active。
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

/// Task (issue) status 桶汇总。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskCounts {
    pub open: i64,
    pub in_progress: i64,
    pub blocked: i64,
    pub done: i64,
}

/// 把 (status, count) 行聚合成 4 桶。
///
/// 与 Node task_buckets 1:1：open = 所有非 done/cancelled（含 in_progress / blocked）。
pub fn bucket_tasks(rows: Vec<(String, i64)>) -> TaskCounts {
    let mut t = TaskCounts::default();
    for (status, count) in rows {
        match status.as_str() {
            "done" => t.done += count,
            "cancelled" => {}
            "in_progress" => {
                t.in_progress += count;
                t.open += count;
            }
            "blocked" => {
                t.blocked += count;
                t.open += count;
            }
            _ => t.open += count,
        }
    }
    t
}

/// Cost summary 桶。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CostSummary {
    pub total_cents: i64,
    pub by_category: std::collections::BTreeMap<String, i64>,
}

/// 聚合 cost 行。
pub fn aggregate_cost(rows: Vec<(String, i64)>) -> CostSummary {
    let mut sum = CostSummary::default();
    for (category, cents) in rows {
        sum.total_cents += cents;
        *sum.by_category.entry(category).or_insert(0) += cents;
    }
    sum
}

#[cfg(test)]
mod internal_tests {
    use super::*;
    use chrono::TimeZone;

    fn date(y: i32, m: u32, d: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, 12, 0, 0).single().unwrap()
    }

    #[test]
    fn utc_month_start_basic() {
        let d = date(2026, 8, 17);
        let s = utc_month_start(d);
        assert_eq!((s.year(), s.month(), s.day()), (2026, 8, 1));
    }

    #[test]
    fn utc_month_start_year_boundary() {
        let d = date(2026, 1, 31);
        let s = utc_month_start(d);
        assert_eq!((s.year(), s.month(), s.day()), (2026, 1, 1));
    }

    #[test]
    fn utc_date_key_format() {
        assert_eq!(utc_date_key(date(2026, 12, 31)), "2026-12-31");
    }

    #[test]
    fn recent_utc_date_keys_count() {
        let keys = recent_utc_date_keys(date(2026, 8, 17), 7);
        assert_eq!(keys.len(), 7);
        assert_eq!(keys.last().unwrap(), "2026-08-17");
    }

    #[test]
    fn recent_utc_date_keys_zero() {
        assert!(recent_utc_date_keys(date(2026, 8, 17), 0).is_empty());
        assert!(recent_utc_date_keys(date(2026, 8, 17), -1).is_empty());
    }

    #[test]
    fn bucket_agents_basic() {
        let rows = vec![
            ("idle".to_string(), 5),
            ("active".to_string(), 3),
            ("running".to_string(), 2),
            ("paused".to_string(), 1),
            ("error".to_string(), 1),
        ];
        let b = bucket_agents(rows);
        assert_eq!(b.active, 8);
        assert_eq!(b.running, 2);
        assert_eq!(b.paused, 1);
        assert_eq!(b.error, 1);
    }

    #[test]
    fn bucket_agents_unknown() {
        let rows = vec![("unknown".to_string(), 7)];
        let b = bucket_agents(rows);
        assert_eq!(b, AgentCounts::default());
    }

    #[test]
    fn bucket_tasks_basic() {
        let rows = vec![
            ("open".to_string(), 5),
            ("todo".to_string(), 3),
            ("backlog".to_string(), 2),
            ("in_progress".to_string(), 4),
            ("blocked".to_string(), 1),
            ("done".to_string(), 10),
            ("cancelled".to_string(), 2),
        ];
        let t = bucket_tasks(rows);
        assert_eq!(t.done, 10);
        assert_eq!(t.in_progress, 4);
        assert_eq!(t.blocked, 1);
        // open = 5+3+2 (todo/backlog/open) + 4 (in_progress) + 1 (blocked) = 15
        assert_eq!(t.open, 15);
    }

    #[test]
    fn bucket_tasks_empty() {
        let t = bucket_tasks(vec![]);
        assert_eq!(t, TaskCounts::default());
    }

    #[test]
    fn aggregate_cost_basic() {
        let rows = vec![
            ("compute".to_string(), 100),
            ("compute".to_string(), 50),
            ("storage".to_string(), 30),
        ];
        let s = aggregate_cost(rows);
        assert_eq!(s.total_cents, 180);
        assert_eq!(s.by_category.get("compute"), Some(&150));
        assert_eq!(s.by_category.get("storage"), Some(&30));
    }

    #[test]
    fn aggregate_cost_empty() {
        let s = aggregate_cost(vec![]);
        assert_eq!(s, CostSummary::default());
    }
}
