#![forbid(unsafe_code)]

//! Attention feed pure helpers — extracted from `AttentionService` to enable
//! pure-function unit testing of the aggregation / sort / count / clamp logic
//! without DB access.
//!
//! R745: 与 `pc-decisions/lifecycle_pure` 同模式：核心判断拆为纯函数，
//! service 层只负责 DB 聚合 + 调用 pure helpers。
//!
//! 对齐 `paperclip/server/src/services/attention.ts` 中的常量表 + 排序规则：
//! - 默认 severity 映射：每个 kind 都有默认 severity
//! - 排序：severity (Critical → High → Medium → Low → Info) 然后 created_at DESC
//! - limit clamp：默认 100 / 上限 500
//! - kind → count field 映射
//! - source_kind 常量表

use chrono::{DateTime, Utc};

/// 默认 open decision limit（与 Node `OPEN_DECISION_DEFAULT_LIMIT = 500` 对齐）。
pub const DEFAULT_OPEN_DECISION_LIMIT: i64 = 500;
/// open decision limit 上限（与 Node `OPEN_DECISION_MAX_LIMIT = 1000` 对齐）。
pub const MAX_OPEN_DECISION_LIMIT: i64 = 1_000;
/// 默认 attention list limit。
pub const DEFAULT_LIST_LIMIT: i64 = 100;
/// attention list limit 上限。
pub const MAX_LIST_LIMIT: i64 = 500;
/// detail excerpt 长度（与 Node `DETAIL_EXCERPT_LENGTH = 160` 对齐）。
pub const DETAIL_EXCERPT_LENGTH: usize = 160;
/// detail image 数量上限（与 Node `DETAIL_IMAGE_LIMIT = 3` 对齐）。
pub const DETAIL_IMAGE_LIMIT: usize = 3;

/// 将任意 timestamp 转为 epoch ms（与 Node `timestamp(value)` 对齐）。
///
/// 缺失 / 非有限 → 0。Node 用 `Number.isFinite` 守门。
pub fn to_epoch_ms(value: Option<DateTime<Utc>>) -> i64 {
    match value {
        None => 0,
        Some(dt) => {
            let ms = dt.timestamp_millis();
            ms
        }
    }
}

/// 将任意 timestamp 转为 RFC3339 字符串（与 Node `toIso(value)` 对齐）。
///
/// 缺失 / 无效 → unix epoch。
pub fn to_iso_string(value: Option<DateTime<Utc>>) -> String {
    match value {
        None => DateTime::<Utc>::from_timestamp(0, 0)
            .expect("epoch is valid")
            .to_rfc3339(),
        Some(dt) => dt.to_rfc3339(),
    }
}

/// clamp attention list limit 到 [1, MAX_LIST_LIMIT]。
pub fn clamp_list_limit(limit: i64) -> i64 {
    limit.clamp(1, MAX_LIST_LIMIT)
}

/// clamp open decision limit 到 [1, MAX_OPEN_DECISION_LIMIT]。
pub fn clamp_open_decision_limit(limit: i64) -> i64 {
    limit.clamp(1, MAX_OPEN_DECISION_LIMIT)
}

/// severity rank（Critical = 0 / High = 1 / Medium = 2 / Low = 3 / Info = 4）。
///
/// 与 Node `SEVERITY_RANK` 对齐：值越小越靠前。
pub fn severity_rank(sev: SeverityRankInput) -> u8 {
    match sev {
        SeverityRankInput::Critical => 0,
        SeverityRankInput::High => 1,
        SeverityRankInput::Medium => 2,
        SeverityRankInput::Low => 3,
        SeverityRankInput::Info => 4,
    }
}

/// 用作 severity_rank 入参的轻量 enum —— 不强制调用方引入完整 AttentionSeverity。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SeverityRankInput {
    Critical,
    High,
    Medium,
    Low,
    Info,
}

impl SeverityRankInput {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "critical" => Some(Self::Critical),
            "high" => Some(Self::High),
            "medium" => Some(Self::Medium),
            "low" => Some(Self::Low),
            "info" => Some(Self::Info),
            _ => None,
        }
    }
}

/// 按 severity 升序 + created_at 降序比较两个 attention item。
///
/// 返回 Ordering：
/// - `Less` —— a 应排在 b 之前
/// - `Greater` —— a 应排在 b 之后
/// - `Equal` —— 完全相同
pub fn cmp_attention_items(
    a_sev: SeverityRankInput,
    a_created_at: DateTime<Utc>,
    b_sev: SeverityRankInput,
    b_created_at: DateTime<Utc>,
) -> std::cmp::Ordering {
    severity_rank(a_sev)
        .cmp(&severity_rank(b_sev))
        .then_with(|| b_created_at.cmp(&a_created_at))
}

/// 排序 attention items in place（severity asc + created_at desc）。
///
/// 与 `AttentionService::list_for_company` 末尾排序对齐。
pub fn sort_by_severity_then_created_at<
    T,
    FSeverity: Fn(&T) -> SeverityRankInput,
    FCreated: Fn(&T) -> DateTime<Utc>,
>(
    items: &mut [T],
    severity_of: FSeverity,
    created_at_of: FCreated,
) {
    items.sort_by(|a, b| {
        cmp_attention_items(
            severity_of(a),
            created_at_of(a),
            severity_of(b),
            created_at_of(b),
        )
    });
}

/// 按 kind 过滤 items（保留顺序）。
pub fn filter_by_kind<T, FKind: Fn(&T) -> KindKind>(
    items: Vec<T>,
    target: KindKind,
    kind_of: FKind,
) -> Vec<T> {
    items
        .into_iter()
        .filter(|item| kind_of(item) == target)
        .collect()
}

/// 通用 kind 标识（与 AttentionItemKind 解耦，方便其他模块复用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KindKind {
    AgentError,
    ApprovalPending,
    BudgetIncident,
    DecisionOpen,
    HeartbeatFailed,
    IssueBlocked,
    IssueProductivityReview,
    IssueReview,
    IssuePendingInteraction,
    JoinRequestPending,
    PipelineAttention,
    ToolError,
}

impl KindKind {
    /// 所有 supported kinds（与 `AttentionService::supported_kinds` 对齐）。
    pub fn all() -> &'static [KindKind] {
        &[
            KindKind::AgentError,
            KindKind::ApprovalPending,
            KindKind::BudgetIncident,
            KindKind::DecisionOpen,
            KindKind::HeartbeatFailed,
            KindKind::IssueBlocked,
            KindKind::IssueProductivityReview,
            KindKind::IssueReview,
            KindKind::IssuePendingInteraction,
            KindKind::JoinRequestPending,
            KindKind::PipelineAttention,
            KindKind::ToolError,
        ]
    }
}

/// 把一个 kind 累加进 counts 结构体（用闭包提供 mutate 能力）。
pub fn accumulate_count(counts: &mut AttentionCountsLike, kind: KindKind) {
    match kind {
        KindKind::AgentError => counts.agent_error += 1,
        KindKind::ApprovalPending => counts.approval_pending += 1,
        KindKind::BudgetIncident => counts.budget_incident += 1,
        KindKind::DecisionOpen => counts.decision_open += 1,
        KindKind::HeartbeatFailed => counts.heartbeat_failed += 1,
        KindKind::IssueBlocked => counts.issue_blocked += 1,
        KindKind::IssueProductivityReview => counts.issue_productivity_review += 1,
        KindKind::IssueReview => counts.issue_review += 1,
        KindKind::IssuePendingInteraction => counts.issue_pending_interaction += 1,
        KindKind::JoinRequestPending => counts.join_request_pending += 1,
        KindKind::PipelineAttention => counts.pipeline_attention += 1,
        KindKind::ToolError => counts.tool_error += 1,
    }
}

/// 全 0 counts。
pub fn empty_counts() -> AttentionCountsLike {
    AttentionCountsLike::default()
}

/// 总计数（所有 kind 之和）。
pub fn total_counts(counts: &AttentionCountsLike) -> usize {
    counts.agent_error
        + counts.approval_pending
        + counts.budget_incident
        + counts.decision_open
        + counts.heartbeat_failed
        + counts.issue_blocked
        + counts.issue_productivity_review
        + counts.issue_review
        + counts.issue_pending_interaction
        + counts.join_request_pending
        + counts.pipeline_attention
        + counts.tool_error
}

/// counts 容器（与 AttentionCounts 字段对齐，但与 AttentionItemKind 解耦）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AttentionCountsLike {
    pub agent_error: usize,
    pub approval_pending: usize,
    pub budget_incident: usize,
    pub decision_open: usize,
    pub heartbeat_failed: usize,
    pub issue_blocked: usize,
    pub issue_productivity_review: usize,
    pub issue_review: usize,
    pub issue_pending_interaction: usize,
    pub join_request_pending: usize,
    pub pipeline_attention: usize,
    pub tool_error: usize,
}

impl AttentionCountsLike {
    pub fn is_empty(&self) -> bool {
        total_counts(self) == 0
    }
}

/// 把文本截断到 N 字符（excerpt）。若短于等于 N，返回原值。
pub fn truncate_excerpt(text: &str, max_length: usize) -> String {
    if text.len() <= max_length {
        return text.to_string();
    }
    // 按 char 边界截断（避免在 UTF-8 多字节字符中间切）。
    let mut end = max_length;
    while !text.is_char_boundary(end) && end > 0 {
        end -= 1;
    }
    text[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ts(year: i32, month: u32, day: u32, hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, hour, 0, 0).unwrap()
    }

    #[test]
    fn r745_to_epoch_ms_handles_none() {
        assert_eq!(to_epoch_ms(None), 0);
    }

    #[test]
    fn r745_to_epoch_ms_returns_positive_for_real_date() {
        let dt = ts(2026, 1, 1, 0);
        assert!(to_epoch_ms(Some(dt)) > 0);
    }

    #[test]
    fn r745_to_iso_string_handles_none() {
        assert_eq!(
            to_iso_string(None),
            "1970-01-01T00:00:00+00:00"
        );
    }

    #[test]
    fn r745_to_iso_string_formats_real_date() {
        let dt = ts(2026, 1, 1, 0);
        assert_eq!(to_iso_string(Some(dt)), "2026-01-01T00:00:00+00:00");
    }

    #[test]
    fn r745_clamp_list_limit_bounds() {
        assert_eq!(clamp_list_limit(0), 1);
        assert_eq!(clamp_list_limit(1), 1);
        assert_eq!(clamp_list_limit(100), 100);
        assert_eq!(clamp_list_limit(10_000), MAX_LIST_LIMIT);
        assert_eq!(clamp_list_limit(-5), 1);
    }

    #[test]
    fn r745_clamp_open_decision_limit_bounds() {
        assert_eq!(clamp_open_decision_limit(0), 1);
        assert_eq!(clamp_open_decision_limit(2000), MAX_OPEN_DECISION_LIMIT);
        assert_eq!(clamp_open_decision_limit(-1), 1);
    }

    #[test]
    fn r745_severity_rank_ordering() {
        assert!(severity_rank(SeverityRankInput::Critical) < severity_rank(SeverityRankInput::High));
        assert!(severity_rank(SeverityRankInput::High) < severity_rank(SeverityRankInput::Medium));
        assert!(severity_rank(SeverityRankInput::Medium) < severity_rank(SeverityRankInput::Low));
        assert!(severity_rank(SeverityRankInput::Low) < severity_rank(SeverityRankInput::Info));
    }

    #[test]
    fn r745_severity_rank_from_str_known() {
        assert_eq!(SeverityRankInput::from_str("critical"), Some(SeverityRankInput::Critical));
        assert_eq!(SeverityRankInput::from_str("high"), Some(SeverityRankInput::High));
        assert_eq!(SeverityRankInput::from_str("medium"), Some(SeverityRankInput::Medium));
        assert_eq!(SeverityRankInput::from_str("low"), Some(SeverityRankInput::Low));
        assert_eq!(SeverityRankInput::from_str("info"), Some(SeverityRankInput::Info));
    }

    #[test]
    fn r745_severity_rank_from_str_unknown() {
        assert_eq!(SeverityRankInput::from_str("urgent"), None);
        assert_eq!(SeverityRankInput::from_str(""), None);
    }

    #[test]
    fn r745_cmp_attention_items_severity_first() {
        let t1 = ts(2026, 1, 1, 0);
        let t2 = ts(2026, 1, 2, 0);
        // High @ later < Critical @ earlier
        assert_eq!(
            cmp_attention_items(
                SeverityRankInput::High,
                t1,
                SeverityRankInput::Critical,
                t2
            ),
            std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn r745_cmp_attention_items_created_at_desc_when_same_severity() {
        let t1 = ts(2026, 1, 1, 0);
        let t2 = ts(2026, 1, 2, 0);
        // same severity, t2 > t1 → t2 before t1 (descending)
        assert_eq!(
            cmp_attention_items(
                SeverityRankInput::High,
                t2,
                SeverityRankInput::High,
                t1
            ),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn r745_cmp_attention_items_equal() {
        let t = ts(2026, 1, 1, 0);
        assert_eq!(
            cmp_attention_items(
                SeverityRankInput::Medium,
                t,
                SeverityRankInput::Medium,
                t
            ),
            std::cmp::Ordering::Equal
        );
    }

    #[test]
    fn r745_sort_by_severity_then_created_at() {
        #[derive(Debug, Clone)]
        struct Item {
            sev: SeverityRankInput,
            t: DateTime<Utc>,
        }
        let mut items = vec![
            Item { sev: SeverityRankInput::Low, t: ts(2026, 1, 1, 0) },
            Item { sev: SeverityRankInput::Critical, t: ts(2026, 1, 5, 0) },
            Item { sev: SeverityRankInput::High, t: ts(2026, 1, 3, 0) },
            Item { sev: SeverityRankInput::High, t: ts(2026, 1, 4, 0) },
        ];
        sort_by_severity_then_created_at(
            &mut items,
            |i| i.sev,
            |i| i.t,
        );
        // Critical first, then High @ later before High @ earlier, then Low
        assert_eq!(items[0].sev, SeverityRankInput::Critical);
        assert_eq!(items[1].sev, SeverityRankInput::High);
        assert_eq!(items[1].t, ts(2026, 1, 4, 0));
        assert_eq!(items[2].sev, SeverityRankInput::High);
        assert_eq!(items[2].t, ts(2026, 1, 3, 0));
        assert_eq!(items[3].sev, SeverityRankInput::Low);
    }

    #[test]
    fn r745_sort_empty() {
        let mut items: Vec<(SeverityRankInput, DateTime<Utc>)> = vec![];
        sort_by_severity_then_created_at(
            &mut items,
            |i| i.0,
            |i| i.1,
        );
        assert!(items.is_empty());
    }

    #[test]
    fn r745_filter_by_kind_keeps_order() {
        let items = vec![
            (KindKind::IssueBlocked, "a"),
            (KindKind::DecisionOpen, "b"),
            (KindKind::IssueBlocked, "c"),
        ];
        let r = filter_by_kind(
            items,
            KindKind::IssueBlocked,
            |i| i.0,
        );
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].1, "a");
        assert_eq!(r[1].1, "c");
    }

    #[test]
    fn r745_filter_by_kind_no_match() {
        let items = vec![(KindKind::DecisionOpen, "x")];
        let r = filter_by_kind(items, KindKind::IssueBlocked, |i| i.0);
        assert!(r.is_empty());
    }

    #[test]
    fn r745_all_kinds_length() {
        assert_eq!(KindKind::all().len(), 12);
    }

    #[test]
    fn r745_accumulate_count_each_kind() {
        let mut counts = empty_counts();
        for kind in KindKind::all() {
            accumulate_count(&mut counts, *kind);
        }
        assert_eq!(counts.agent_error, 1);
        assert_eq!(counts.approval_pending, 1);
        assert_eq!(counts.tool_error, 1);
        assert_eq!(total_counts(&counts), 12);
        assert!(!counts.is_empty());
    }

    #[test]
    fn r745_empty_counts_zero() {
        let c = empty_counts();
        assert_eq!(total_counts(&c), 0);
        assert!(c.is_empty());
    }

    #[test]
    fn r745_accumulate_multiple_same_kind() {
        let mut counts = empty_counts();
        accumulate_count(&mut counts, KindKind::IssueBlocked);
        accumulate_count(&mut counts, KindKind::IssueBlocked);
        accumulate_count(&mut counts, KindKind::IssueBlocked);
        assert_eq!(counts.issue_blocked, 3);
    }

    #[test]
    fn r745_truncate_excerpt_short_text() {
        assert_eq!(truncate_excerpt("hello", 10), "hello");
    }

    #[test]
    fn r745_truncate_exact_length() {
        assert_eq!(truncate_excerpt("hello", 5), "hello");
    }

    #[test]
    fn r745_truncate_long_text() {
        let s = "a".repeat(200);
        let r = truncate_excerpt(&s, 100);
        assert_eq!(r.len(), 100);
    }

    #[test]
    fn r745_truncate_at_char_boundary() {
        // "你好世界" = 12 bytes (each Chinese char 3 bytes), len 12
        // truncate at byte 4 (between "你" and "好") → should give "你"
        let r = truncate_excerpt("你好世界", 4);
        assert_eq!(r, "你");
    }

    #[test]
    fn r745_default_limit_constants_match_node() {
        assert_eq!(DEFAULT_OPEN_DECISION_LIMIT, 500);
        assert_eq!(MAX_OPEN_DECISION_LIMIT, 1_000);
        assert_eq!(DEFAULT_LIST_LIMIT, 100);
        assert_eq!(MAX_LIST_LIMIT, 500);
        assert_eq!(DETAIL_EXCERPT_LENGTH, 160);
        assert_eq!(DETAIL_IMAGE_LIMIT, 3);
    }
}
