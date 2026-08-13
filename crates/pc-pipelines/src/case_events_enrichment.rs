//! Pipeline-level aggregation enrichment (R639.2.6).
//!
//! Node 上游在 `GET /companies/:companyId/pipelines` 端点把
//! `loadPipelineConnections` + `loadPipelineDescendantActiveWorkCounts`
//! 并发拉取后注入到每个 pipeline row 上:
//!
//! - `descendantActiveWorkCount` (i64)
//! - `connections: { upstreamPipelineIds, downstreamPipelineIds }`
//!
//! 本模块提供该 enrichment 的 Rust 等价实现 —— 单一 async 函数,
//! 内部用 `tokio::try_join!` 并发拉取两个数据源,保证 RTT 与 Node 一致。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use pc_repos::pipeline::{PipelineCaseRow, PipelineRow};

use crate::aggregation_db::{load_active_work_for_cases, ActiveWorkRow};
use crate::case_events_db::{
    load_descendant_active_work_counts_for_cases, load_pipeline_connections,
    load_pipeline_descendant_active_work_counts,
};

/// Cross-pipeline parent/child connections (Node 上游 `PipelineConnections`).
///
/// - `upstream_pipeline_ids`: 该 pipeline 在 case 树中是 child 时,
///   对应 parent 所在的 pipelines (即该 pipeline 的上游)
/// - `downstream_pipeline_ids`: 该 pipeline 在 case 树中是 parent 时,
///   对应 child 所在的 pipelines (即该 pipeline 的下游)
///
/// 与 Node 上游语义 1:1: `upstreamPipelineIds` + `downstreamPipelineIds`,
/// 双向都排好序且去重。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineConnections {
    #[serde(default)]
    pub upstream_pipeline_ids: Vec<Uuid>,
    #[serde(default)]
    pub downstream_pipeline_ids: Vec<Uuid>,
}

/// 单个 pipeline 的聚合字段 (Node 上游 `descendantActiveWorkCount + connections`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineAggregation {
    pub descendant_active_work_count: i64,
    #[serde(default)]
    pub connections: PipelineConnections,
}

/// 视图类型: `PipelineRow` + 聚合字段,序列化时 flatten (与 Node spread 行为一致)。
///
/// Node 上游端点: `res.json(rows.map((row) => ({ ...row.pipeline, descendantActiveWorkCount, connections })))`
/// Rust 用 `#[serde(flatten)]` 实现等价的扁平 JSON 形态。
///
/// 实现 `Deref<Target=PipelineRow>` 让调用方直接 `row.id` / `row.name` 等
/// 透明访问 PipelineRow 字段,无需 `row.pipeline.id` (减少样板)。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrichedPipelineRow {
    #[serde(flatten)]
    pub pipeline: PipelineRow,
    pub descendant_active_work_count: i64,
    pub connections: PipelineConnections,
}

impl std::ops::Deref for EnrichedPipelineRow {
    type Target = PipelineRow;

    fn deref(&self) -> &Self::Target {
        &self.pipeline
    }
}

/// 给一组 `PipelineRow` 注入 `descendantActiveWorkCount` + `connections`。
///
/// 内部并发:
/// 1. `load_pipeline_connections(company_id)` — 全公司一次拉取
/// 2. `load_pipeline_descendant_active_work_counts(company_id, pipeline_ids)` — 仅传入的 pipeline_ids
///
/// 与 Node 上游 `Promise.all` 行为 1:1。
///
/// 输入 rows 的顺序与输出 `EnrichedPipelineRow` 的顺序一致。
/// 空输入短路返回 `Ok(Vec::new())` 不打 DB。
pub async fn enrich_pipelines_with_aggregation(
    pool: &PgPool,
    company_id: Uuid,
    rows: Vec<PipelineRow>,
) -> sqlx::Result<Vec<EnrichedPipelineRow>> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let pipeline_ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();

    let (connections_rows, work_counts_rows) = tokio::try_join!(
        load_pipeline_connections(pool, company_id),
        load_pipeline_descendant_active_work_counts(pool, company_id, &pipeline_ids),
    )?;

    // Build connections map by pivoting on child + parent pipeline_id.
    let mut connections_map: HashMap<Uuid, PipelineConnections> = HashMap::new();
    for conn in connections_rows {
        // child pipeline 的 upstream = parent pipeline
        connections_map
            .entry(conn.child_pipeline_id)
            .or_default()
            .upstream_pipeline_ids
            .push(conn.parent_pipeline_id);
        // parent pipeline 的 downstream = child pipeline
        connections_map
            .entry(conn.parent_pipeline_id)
            .or_default()
            .downstream_pipeline_ids
            .push(conn.child_pipeline_id);
    }
    for conn in connections_map.values_mut() {
        conn.upstream_pipeline_ids.sort();
        conn.downstream_pipeline_ids.sort();
        conn.upstream_pipeline_ids.dedup();
        conn.downstream_pipeline_ids.dedup();
    }

    let work_counts_map: HashMap<Uuid, i64> =
        work_counts_map_from_rows(work_counts_rows);

    Ok(rows
        .into_iter()
        .map(|pipeline| {
            let entry = connections_map.remove(&pipeline.id).unwrap_or_default();
            let descendant_active_work_count =
                work_counts_map.get(&pipeline.id).copied().unwrap_or(0);
            EnrichedPipelineRow {
                pipeline,
                descendant_active_work_count,
                connections: entry,
            }
        })
        .collect())
}

fn work_counts_map_from_rows(
    rows: Vec<crate::case_events_db::PipelineDescendantActiveWorkCountRow>,
) -> HashMap<Uuid, i64> {
    let mut map = HashMap::new();
    for row in rows {
        map.insert(row.pipeline_id, row.count);
    }
    map
}

/// 纯函数: 把 `(parent_pipeline_id, child_pipeline_id)` 边列表
/// 折叠成 `child -> upstream` + `parent -> downstream` 的 Map。
///
/// 导出以便单测覆盖 (无 DB 依赖)。
pub fn build_pipeline_connections_map(
    edges: &[(Uuid, Uuid)],
) -> HashMap<Uuid, PipelineConnections> {
    let mut map: HashMap<Uuid, PipelineConnections> = HashMap::new();
    for (parent_id, child_id) in edges {
        map.entry(*child_id)
            .or_default()
            .upstream_pipeline_ids
            .push(*parent_id);
        map.entry(*parent_id)
            .or_default()
            .downstream_pipeline_ids
            .push(*child_id);
    }
    for conn in map.values_mut() {
        conn.upstream_pipeline_ids.sort();
        conn.downstream_pipeline_ids.sort();
        conn.upstream_pipeline_ids.dedup();
        conn.downstream_pipeline_ids.dedup();
    }
    map
}


// ============================================================================
// R639.2.8: case-level aggregation enrichment
//   list_cases 端点 (Node /companies/:companyId/cases) 使用:
//     - loadActiveWorkForCases (R639.2.2 已复刻)
//     - loadDescendantActiveWorkCountsForCases (R639.2.5 已复刻)
//   返回 case 字段 + activeWork + descendantActiveWorkCount
// ============================================================================

/// 视图类型: `PipelineCaseRow` + 聚合字段,序列化时 flatten (与 Node 上游 spread 行为一致)。
///
/// Node 上游端点: `res.json(rows.map((row) => ({ case, stage, parentCase, activeWork, descendantActiveWorkCount })))`
/// 本类型提供 `PipelineCaseRow` 字段在顶层 + 聚合字段, 通过 serde flatten 实现等价扁平 JSON。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrichedCaseRow {
    #[serde(flatten)]
    pub case: PipelineCaseRow,
    /// 该 case 自身的 active work (latest by issue.updated_at), 没匹配 issue 时为 None。
    pub active_work: Option<ActiveWorkRef>,
    /// 该 case 子树中 in_progress work/automation 涉及的 descendant case 数 (默认 0)。
    pub descendant_active_work_count: i64,
}

impl std::ops::Deref for EnrichedCaseRow {
    type Target = PipelineCaseRow;

    fn deref(&self) -> &Self::Target {
        &self.case
    }
}

/// active work 引用 (Node 上游 `activeWork` 字段)。
///
/// Node 上游 `activeWork` 直接从 ActiveWorkRow 抽取需要的几个字段。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveWorkRef {
    pub issue_id: Uuid,
    pub issue_identifier: Option<String>,
    pub issue_title: String,
    pub status: String,
}

impl ActiveWorkRef {
    fn from_row(row: &ActiveWorkRow) -> Self {
        Self {
            issue_id: row.issue_id,
            issue_identifier: row.issue_identifier.clone(),
            issue_title: row.issue_title.clone(),
            // Node 上游 activeWork.status 来自 issues.status (in_progress 已由 SQL 过滤)
            status: "in_progress".to_string(),
        }
    }
}

/// 给一组 `PipelineCaseRow` 注入 `activeWork` + `descendantActiveWorkCount`。
///
/// 内部并发拉取 active_work + descendant_active_work_counts (tokio::try_join!),
/// 与 Node 上游 `Promise.all` 1:1。
///
/// 输入 rows 的顺序与输出 `EnrichedCaseRow` 的顺序一致。
/// 空输入短路返回 `Ok(Vec::new())` 不打 DB。
///
/// Active work: 一个 case 可能有多条 active work issue links, 仅取第一条
/// (Node 上游 `activeWorkByCase.get(...).shift()` 行为)。
pub async fn enrich_cases_with_aggregation(
    pool: &PgPool,
    company_id: Uuid,
    rows: Vec<PipelineCaseRow>,
) -> sqlx::Result<Vec<EnrichedCaseRow>> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let case_ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();

    let (active_work_rows, descendant_rows) = tokio::try_join!(
        load_active_work_for_cases(pool, company_id, &case_ids),
        load_descendant_active_work_counts_for_cases(pool, company_id, &case_ids),
    )?;

    // active work: 按 case_id 折叠, 取首个 (row 已按 issue.updated_at DESC)
    let mut active_work_map: std::collections::HashMap<Uuid, ActiveWorkRef> =
        std::collections::HashMap::new();
    for row in &active_work_rows {
        active_work_map
            .entry(row.case_id)
            .or_insert_with(|| ActiveWorkRef::from_row(row));
    }

    // descendant counts: case_id -> count
    let descendant_map: std::collections::HashMap<Uuid, i64> = descendant_rows
        .into_iter()
        .map(|r| (r.root_id, r.count))
        .collect();

    Ok(rows
        .into_iter()
        .map(|case| {
            let active_work = active_work_map.remove(&case.id);
            let descendant_active_work_count = descendant_map
                .get(&case.id)
                .copied()
                .unwrap_or(0);
            EnrichedCaseRow {
                case,
                active_work,
                descendant_active_work_count,
            }
        })
        .collect())
}

/// 纯函数: 把 active work rows 按 case_id 折叠, 保留每个 case_id 的首条 (取首条语义)。
///
/// 导出以便单测覆盖 (无 DB 依赖)。
pub fn build_active_work_map(rows: &[ActiveWorkRow]) -> std::collections::HashMap<Uuid, ActiveWorkRef> {
    let mut map = std::collections::HashMap::new();
    for row in rows {
        map.entry(row.case_id)
            .or_insert_with(|| ActiveWorkRef::from_row(row));
    }
    map
}


#[cfg(test)]
mod tests {
    use super::*;

    fn u() -> Uuid {
        Uuid::new_v4()
    }

    #[test]
    fn build_connections_map_empty_input_returns_empty() {
        let map = build_pipeline_connections_map(&[]);
        assert!(map.is_empty());
    }

    #[test]
    fn build_connections_map_single_edge_populates_both_sides() {
        let parent = u();
        let child = u();
        let map = build_pipeline_connections_map(&[(parent, child)]);

        let child_view = map.get(&child).expect("child entry");
        assert_eq!(child_view.upstream_pipeline_ids, vec![parent]);
        assert!(child_view.downstream_pipeline_ids.is_empty());

        let parent_view = map.get(&parent).expect("parent entry");
        assert_eq!(parent_view.downstream_pipeline_ids, vec![child]);
        assert!(parent_view.upstream_pipeline_ids.is_empty());
    }

    #[test]
    fn build_connections_map_multiple_edges_sort_and_dedup() {
        // Deterministic UUIDs based on fixed bytes (v4 layout) for stable ordering.
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let c = Uuid::from_u128(3);
        // edges: a->b (twice), b->c (once), a->c (twice)
        let edges = vec![(a, b), (a, b), (b, c), (a, c), (a, c)];
        let map = build_pipeline_connections_map(&edges);

        let a_view = map.get(&a).expect("a");
        // sorted by Uuid internal u128 ordering (1 < 2 < 3)
        assert_eq!(a_view.downstream_pipeline_ids, vec![b, c]);
        assert!(a_view.upstream_pipeline_ids.is_empty());

        let b_view = map.get(&b).expect("b");
        assert_eq!(b_view.upstream_pipeline_ids, vec![a]);
        assert_eq!(b_view.downstream_pipeline_ids, vec![c]);

        let c_view = map.get(&c).expect("c");
        assert_eq!(c_view.upstream_pipeline_ids, vec![a, b]); // sorted by Uuid ord
        assert!(c_view.downstream_pipeline_ids.is_empty());
    }

    #[test]
    fn pipeline_connections_default_is_empty_both_sides() {
        let conn = PipelineConnections::default();
        assert!(conn.upstream_pipeline_ids.is_empty());
        assert!(conn.downstream_pipeline_ids.is_empty());
    }

    #[test]
    fn pipeline_aggregation_default_is_zero_count_and_empty_connections() {
        let agg = PipelineAggregation::default();
        assert_eq!(agg.descendant_active_work_count, 0);
        assert_eq!(agg.connections, PipelineConnections::default());
    }

    #[test]
    fn enriched_pipeline_row_serializes_flatten_with_camel_case() {
        let pipeline = PipelineRow {
            id: Uuid::nil(),
            company_id: Uuid::nil(),
            project_id: None,
            key: "k".into(),
            name: "n".into(),
            description: None,
            enforce_transitions: false,
            created_by_user_id: None,
            created_by_agent_id: None,
            archived_at: None,
            created_at: pc_core::Timestamp::default(),
            updated_at: pc_core::Timestamp::default(),
        };
        let row = EnrichedPipelineRow {
            pipeline,
            descendant_active_work_count: 7,
            connections: PipelineConnections {
                upstream_pipeline_ids: vec![Uuid::nil()],
                downstream_pipeline_ids: vec![],
            },
        };
        let v = serde_json::to_value(&row).expect("serialize");
        // flatten: pipeline fields appear at top-level
        assert_eq!(v["key"], "k");
        assert_eq!(v["name"], "n");
        // camelCase aggregation fields
        assert_eq!(v["descendantActiveWorkCount"], 7);
        let conns = v["connections"].as_object().expect("connections obj");
        assert_eq!(conns["upstreamPipelineIds"][0], Uuid::nil().to_string());
        assert!(conns["downstreamPipelineIds"].as_array().unwrap().is_empty());
    }

    fn dummy_active_row(case_id: Uuid, issue_id: Uuid) -> ActiveWorkRow {
        ActiveWorkRow {
            case_id,
            issue_id,
            issue_identifier: Some(format!("P-{}", &issue_id.to_string()[..4])),
            issue_title: "Active work".into(),
            issue_role: "work".into(),
            agent_id: Uuid::new_v4(),
            agent_name: "agent-1".into(),
            started_at: None,
            issue_updated_at: pc_core::Timestamp::default(),
        }
    }

    #[test]
    fn build_active_work_map_empty_input_returns_empty() {
        let map = build_active_work_map(&[]);
        assert!(map.is_empty());
    }

    #[test]
    fn build_active_work_map_keeps_first_row_per_case_id() {
        let case_id = Uuid::from_u128(1);
        let issue_a = Uuid::from_u128(2);
        let issue_b = Uuid::from_u128(3);
        let rows = vec![dummy_active_row(case_id, issue_a), dummy_active_row(case_id, issue_b)];
        let map = build_active_work_map(&rows);
        assert_eq!(map.len(), 1);
        let entry = map.get(&case_id).expect("entry");
        assert_eq!(entry.issue_id, issue_a, "must keep first row");
        assert_eq!(entry.issue_title, "Active work");
        assert_eq!(entry.status, "in_progress");
    }

    #[test]
    fn active_work_ref_serializes_with_camel_case_keys() {
        let r = ActiveWorkRef {
            issue_id: Uuid::nil(),
            issue_identifier: Some("P-1".into()),
            issue_title: "T".into(),
            status: "in_progress".into(),
        };
        let v = serde_json::to_value(&r).expect("serialize");
        assert_eq!(v["issueId"], Uuid::nil().to_string());
        assert_eq!(v["issueIdentifier"], "P-1");
        assert_eq!(v["issueTitle"], "T");
        assert_eq!(v["status"], "in_progress");
    }
}
