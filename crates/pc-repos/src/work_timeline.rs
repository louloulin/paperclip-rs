//! Work Timeline 数据访问 + 纯函数工具。
//!
//! 目标是与 Node `server/src/services/work-timeline.ts` 和共享 DTO
//! `packages/shared/src/types/work-timeline.ts` 完全对齐：
//! - 默认 7 天窗口，最大 31 天
//! - `actors` / `spans` / `events` / `edges` / `pagination` / `window` 六段
//!   返回结构。
//! - 事件源：`issues`、`heartbeat_runs`、`issue_comments`、`issue_approvals
//!   + approvals`、`issue_thread_interactions`、`activity_log`。
//! - `usage` 兼容 camelCase / snake_case 多种字段名。
//!
//! 当前实现提供：纯函数（窗口、limit、offset、usage、actor_id、empty
//! result），以及一个最小 `get_timeline` 入口，可被路由直接调用，未来接
//! 入数据源时无需再改 DTO。

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use uuid::Uuid;

use pc_db::Db;

pub const DEFAULT_LIMIT: i64 = 200;
pub const MAX_LIMIT: i64 = 500;
pub const MAX_WINDOW_MS: i64 = 31 * 24 * 60 * 60 * 1000;
pub const DEFAULT_WINDOW_MS: i64 = 7 * 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkTimelineQuery {
    pub company_id: Uuid,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub user_id: Option<String>,
    pub goal_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub issue_id: Option<Uuid>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct NormalizedWindow {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub capped: bool,
}

pub fn normalize_window(input: &WorkTimelineQuery, now: DateTime<Utc>) -> NormalizedWindow {
    let raw_to = input.to.unwrap_or(now);
    let to = if raw_to > now { now } else { raw_to };
    let requested_from = input
        .from
        .unwrap_or_else(|| to - Duration::milliseconds(DEFAULT_WINDOW_MS));
    let mut from = requested_from;
    let mut capped = false;
    if (to - from).num_milliseconds() > MAX_WINDOW_MS {
        from = to - Duration::milliseconds(MAX_WINDOW_MS);
        capped = true;
    }
    if from > to {
        from = to - Duration::milliseconds(DEFAULT_WINDOW_MS);
        capped = true;
    }
    NormalizedWindow { from, to, capped }
}

pub fn normalize_limit(value: Option<i64>) -> i64 {
    value.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

pub fn normalize_offset(value: Option<i64>) -> i64 {
    value.unwrap_or(0).max(0)
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkTimelineActor {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunUsage {
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkTimelineSpan {
    pub actor_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lane_hint: Option<String>,
    pub run_id: Uuid,
    pub issue_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_identifier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_title: Option<String>,
    pub start: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<String>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_of_run_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continuation_attempt: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invocation_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<RunUsage>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkTimelineEvent {
    pub actor_id: String,
    pub kind: String,
    pub issue_id: Uuid,
    pub at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkTimelineEdge {
    pub from_actor_id: String,
    pub to_actor_id: String,
    pub issue_id: Uuid,
    pub at: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkTimelinePagination {
    pub limit: i64,
    pub offset: i64,
    pub total_issues: i64,
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkTimelineResult {
    pub actors: Vec<WorkTimelineActor>,
    pub spans: Vec<WorkTimelineSpan>,
    pub events: Vec<WorkTimelineEvent>,
    pub edges: Vec<WorkTimelineEdge>,
    pub pagination: WorkTimelinePagination,
    pub window: NormalizedWindow,
}

pub fn empty_result(query: &WorkTimelineQuery, now: DateTime<Utc>) -> WorkTimelineResult {
    let window = normalize_window(query, now);
    let limit = normalize_limit(query.limit);
    let offset = normalize_offset(query.offset);
    WorkTimelineResult {
        actors: Vec::new(),
        spans: Vec::new(),
        events: Vec::new(),
        edges: Vec::new(),
        pagination: WorkTimelinePagination {
            limit,
            offset,
            total_issues: 0,
            has_more: false,
        },
        window,
    }
}

pub fn actor_id(kind: &str, id: &str) -> String {
    format!("{kind}:{id}")
}

pub fn parse_usage(source: Option<&serde_json::Value>) -> Option<RunUsage> {
    let value = source?;
    let obj = value.as_object()?;
    let input = read_tokens(
        obj,
        &[
            "inputTokens",
            "input_tokens",
            "rawInputTokens",
            "raw_input_tokens",
        ],
    );
    let cached = read_tokens(
        obj,
        &[
            "cachedInputTokens",
            "cached_input_tokens",
            "cacheReadInputTokens",
            "cache_read_input_tokens",
        ],
    );
    let output = read_tokens(
        obj,
        &[
            "outputTokens",
            "output_tokens",
            "rawOutputTokens",
            "raw_output_tokens",
        ],
    );
    let total = input + cached + output;
    if total > 0 {
        Some(RunUsage {
            input_tokens: input,
            cached_input_tokens: cached,
            output_tokens: output,
            total_tokens: total,
        })
    } else {
        None
    }
}

fn read_tokens(obj: &serde_json::Map<String, serde_json::Value>, keys: &[&str]) -> i64 {
    for key in keys {
        if let Some(value) = obj.get(*key) {
            if let Some(num) = value.as_i64() {
                return num.max(0);
            }
            if let Some(text) = value.as_str() {
                if let Ok(parsed) = text.trim().parse::<i64>() {
                    return parsed.max(0);
                }
            }
        }
    }
    0
}

/// Work Timeline 仓储。
///
/// 当前仅承载 `get_timeline` 入口；返回与 Node service 一致的
/// `WorkTimelineResult` 结构，数据源将在后续 round 接入。
#[derive(Clone)]
pub struct WorkTimelineRepo {
    #[allow(dead_code)]
    db: Db,
}

impl WorkTimelineRepo {
    pub fn new(db: &Db) -> Self {
        Self { db: db.clone() }
    }

    /// 返回与 Node 端一致的 Work Timeline 视图。
    ///
    /// 后续将替换为真实查询；当前先返回空结果 + 归一化窗口，保证 UI 兼容。
    pub async fn get_timeline(
        &self,
        query: WorkTimelineQuery,
        now: DateTime<Utc>,
    ) -> WorkTimelineResult {
        empty_result(&query, now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn q(
        limit: Option<i64>,
        offset: Option<i64>,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
    ) -> WorkTimelineQuery {
        WorkTimelineQuery {
            company_id: Uuid::nil(),
            from,
            to,
            user_id: None,
            goal_id: None,
            project_id: None,
            issue_id: None,
            limit,
            offset,
        }
    }

    #[test]
    fn window_defaults_to_seven_days() {
        let now = Utc.with_ymd_and_hms(2026, 8, 4, 12, 0, 0).unwrap();
        let window = normalize_window(&q(None, None, None, None), now);
        assert!(!window.capped);
        assert_eq!(window.to, now);
        assert_eq!(window.from, now - Duration::days(7));
    }

    #[test]
    fn window_caps_to_thirty_one_days() {
        let now = Utc.with_ymd_and_hms(2026, 8, 4, 12, 0, 0).unwrap();
        let too_old = now - Duration::days(60);
        let window = normalize_window(&q(None, None, Some(too_old), Some(now)), now);
        assert!(window.capped);
        assert_eq!(window.to - window.from, Duration::days(31));
    }

    #[test]
    fn window_clamps_to_when_in_future() {
        let now = Utc.with_ymd_and_hms(2026, 8, 4, 12, 0, 0).unwrap();
        let future = now + Duration::hours(2);
        let window = normalize_window(&q(None, None, None, Some(future)), now);
        assert_eq!(window.to, now);
    }

    #[test]
    fn window_inverts_when_from_after_to() {
        let now = Utc.with_ymd_and_hms(2026, 8, 4, 12, 0, 0).unwrap();
        let from = now - Duration::days(2);
        let to = now - Duration::days(5);
        let window = normalize_window(&q(None, None, Some(from), Some(to)), now);
        assert!(window.capped);
        assert_eq!(window.to - window.from, Duration::days(7));
    }

    #[test]
    fn limit_clamps_within_bounds() {
        assert_eq!(normalize_limit(None), DEFAULT_LIMIT);
        assert_eq!(normalize_limit(Some(0)), 1);
        assert_eq!(normalize_limit(Some(99_999)), MAX_LIMIT);
        assert_eq!(normalize_limit(Some(50)), 50);
    }

    #[test]
    fn offset_floors_to_zero() {
        assert_eq!(normalize_offset(None), 0);
        assert_eq!(normalize_offset(Some(-5)), 0);
        assert_eq!(normalize_offset(Some(7)), 7);
    }

    #[test]
    fn usage_aggregates_camel_and_snake() {
        let payload = serde_json::json!({
            "inputTokens": 3,
            "cached_input_tokens": 1,
            "outputTokens": "2",
            "rawInputTokens": 99,
        });
        let usage = parse_usage(Some(&payload)).expect("usage present");
        assert_eq!(usage.input_tokens, 3);
        assert_eq!(usage.cached_input_tokens, 1);
        assert_eq!(usage.output_tokens, 2);
        assert_eq!(usage.total_tokens, 6);
    }

    #[test]
    fn usage_absent_returns_none() {
        assert!(parse_usage(None).is_none());
        assert!(parse_usage(Some(&serde_json::json!({}))).is_none());
    }

    #[test]
    fn empty_result_matches_shared_contract() {
        let now = Utc.with_ymd_and_hms(2026, 8, 4, 12, 0, 0).unwrap();
        let query = q(Some(25), Some(10), None, None);
        let result = empty_result(&query, now);
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(value["pagination"]["limit"], 25);
        assert_eq!(value["pagination"]["offset"], 10);
        assert_eq!(value["pagination"]["totalIssues"], 0);
        assert_eq!(value["pagination"]["hasMore"], false);
        assert_eq!(value["actors"].as_array().unwrap().len(), 0);
        assert_eq!(value["events"].as_array().unwrap().len(), 0);
        assert_eq!(value["window"]["capped"], false);
    }

    #[test]
    fn actor_id_format_matches_node_helper() {
        assert_eq!(actor_id("agent", "abc"), "agent:abc");
        assert_eq!(actor_id("user", "xyz"), "user:xyz");
    }
}
