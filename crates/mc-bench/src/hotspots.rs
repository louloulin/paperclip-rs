//! 三条上游热点（`plan1.md` §3.6）—— bench 与证据**共用同一份调用**。
//!
//! 三个 case 的入参构造与执行都写在这里：`benches/*.rs` 只做 criterion 的壳，不另写一份查询参数，
//! 这样「报告里的读数」与「这里定义的调用」是同一件事，不会出现两处漂移。

use mc_core::Id;
use mc_repos::inbox::{ArchivedCursor, ArchivedInboxFilter, ArchivedInboxPage, InboxRepo};
use mc_repos::issue::{IssueFilter, IssueOrderBy, IssueRepo};
use mc_repos::issue_table::{
    IssueTableRepo, TableFacetKind, TableFacetsPage, TableFacetsQuery, TableFilter,
};

use crate::config::PAGE_LIMIT;
use crate::dataset::Dataset;

/// 热点 ①：issue 列表的过滤条件（`limit=50` + 排序 + 可选过滤）。
///
/// `filtered = true` 时叠加 status / priority 两个过滤（上游 issue 列表的常用形态）。
pub fn issue_list_filter(ds: &Dataset, order: IssueOrderBy, filtered: bool) -> IssueFilter {
    let mut filter = IssueFilter::new(Id::from(ds.workspace_id));
    filter.limit = Some(PAGE_LIMIT);
    filter.order = order;
    filter.include_closed = true;
    if filtered {
        filter.statuses = Some(vec!["todo".to_string(), "in_progress".to_string()]);
        filter.priorities = Some(vec!["high".to_string(), "urgent".to_string()]);
    }
    filter
}

/// 热点 ①：跑一次列表（返回行数，喂 `black_box`）。
pub async fn run_issue_list(repo: &IssueRepo, filter: &IssueFilter) -> usize {
    repo.list(filter).await.expect("issue list").len()
}

/// 热点 ①：跑一次列表 + 总数（同一 WHERE 两条查询）。
pub async fn run_issue_list_with_total(repo: &IssueRepo, filter: &IssueFilter) -> (usize, i64) {
    let (rows, total) = repo
        .list_with_total(filter)
        .await
        .expect("issue list with total");
    (rows.len(), total)
}

/// 热点 ②：facets 请求（`all_five` = 全 5 维 + total；否则 Status/Priority 两维、不带 total）。
pub fn facets_query(ds: &Dataset, all_five: bool) -> TableFacetsQuery {
    TableFacetsQuery {
        workspace_id: Id::from(ds.workspace_id),
        filter: TableFilter::default(),
        facets: if all_five {
            vec![
                TableFacetKind::Status,
                TableFacetKind::Priority,
                TableFacetKind::Assignee,
                TableFacetKind::Creator,
                TableFacetKind::Project,
            ]
        } else {
            vec![TableFacetKind::Status, TableFacetKind::Priority]
        },
        include_total: all_five,
    }
}

/// 热点 ②：跑一次 facets 聚合。
pub async fn run_facets(repo: &IssueTableRepo, query: &TableFacetsQuery) -> TableFacetsPage {
    repo.table_facets(query).await.expect("table facets")
}

/// 热点 ③：inbox 归档游标的一页（`cursor = None` 是第一页）。
pub async fn run_inbox_page(
    repo: &InboxRepo,
    ds: &Dataset,
    cursor: Option<&ArchivedCursor>,
) -> ArchivedInboxPage {
    repo.list_archived_page(
        Id::from(ds.workspace_id),
        Id::from(ds.user_id),
        &ArchivedInboxFilter::default(),
        cursor,
        PAGE_LIMIT,
    )
    .await
    .expect("archived inbox page")
}

/// 翻第二页要用的游标：第一页最后一行的 `(created_at, id)`（行比较 keyset 翻页）。
pub fn cursor_after(page: &ArchivedInboxPage) -> Option<ArchivedCursor> {
    page.items.last().map(|row| ArchivedCursor {
        created_at: row.created_at,
        id: Id::from(row.id),
    })
}
