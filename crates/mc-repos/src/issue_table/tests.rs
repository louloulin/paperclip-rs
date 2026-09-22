//! `IssueTableRepo` 的 PG 集成测试（M2-D / LUM-1355）。
//!
//! 从 `repo.rs` 拆出：R7 单文件 800 行上限（`docs/plan1.md` R7）。
//!
//! 运行：
//! ```text
//! MULTICA_TEST_DATABASE_URL=postgres://multica:multica@127.0.0.1:5432/multica_test \
//!   cargo test -p mc-repos --lib -- --ignored issue_table
//! ```
//! 没有该 env 时静默 skip（与 `crate::issue` 的 `db_tests` 一致）。

use std::env;

use uuid::Uuid;

use mc_core::issue::AssigneeType;
use mc_core::priority::Priority;
use mc_core::Id;
use mc_db::Db;

use super::{
    IssueTableRepo, TableActor, TableCursor, TableFacetKind, TableFacetValue, TableFacetsQuery,
    TableFilter, TableGroupCount, TableGroupKey, TableGroupKind, TableGroupSpec, TableGroupsQuery,
    TableOrder, TableRowsPage, TableRowsQuery, TableScope, TableSortDirection, TableSortField,
};
use crate::issue::{IssueRepo, NewIssue};

struct Fixture {
    db: Db,
    workspace_id: Id,
    user_id: Id,
}

async fn setup() -> Option<Fixture> {
    let url = env::var("MULTICA_TEST_DATABASE_URL").ok()?;
    let db = Db::connect(&url, 4, 1).await.ok()?;
    let pool = db.pool();
    let workspace_id: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-m2d', $1) RETURNING id",
    )
    .bind(format!("itest-m2d-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .ok()?;
    let user_id: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-m2d', $1) RETURNING id"#,
    )
    .bind(format!("itest-m2d-{}@example.com", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .ok()?;
    Some(Fixture {
        db,
        workspace_id: Id::from(workspace_id),
        user_id: Id::from(user_id),
    })
}

async fn teardown(fx: &Fixture) {
    let pool = fx.db.pool();
    let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
        .bind(fx.workspace_id.0)
        .execute(pool)
        .await;
    let _ = sqlx::query(r#"DELETE FROM "user" WHERE id = $1"#)
        .bind(fx.user_id.0)
        .execute(pool)
        .await;
}

fn new_issue(fx: &Fixture, title: &str) -> NewIssue {
    NewIssue::new(fx.workspace_id, title, fx.user_id.0.to_string())
}

fn workspace_filter() -> TableFilter {
    TableFilter {
        scope: TableScope::Workspace {
            assignee_types: Vec::new(),
        },
        ..TableFilter::default()
    }
}

fn group_spec(kind: TableGroupKind, include_empty: bool) -> TableGroupSpec {
    TableGroupSpec {
        kind,
        include_empty,
    }
}

fn no_group() -> TableGroupSpec {
    group_spec(TableGroupKind::None, false)
}

fn asc(field: TableSortField) -> TableOrder {
    TableOrder {
        field,
        direction: Some(TableSortDirection::Asc),
    }
}

fn count_of(groups: &[TableGroupCount], key: &str) -> Option<i64> {
    groups.iter().find(|g| g.key == key).map(|g| g.count)
}

fn count_of_in_values(values: &[TableFacetValue], key: &str) -> Option<i64> {
    values.iter().find(|v| v.key == key).map(|v| v.count)
}

/// `/rows` 单页（limit=2，按 `position` 升序），供分页测试复用。
async fn rows_page(
    table: &IssueTableRepo,
    workspace_id: Id,
    filter: TableFilter,
    cursor: Option<TableCursor>,
) -> TableRowsPage {
    table
        .table_rows(&TableRowsQuery {
            workspace_id,
            filter,
            group: no_group(),
            group_key: TableGroupKey::None,
            order: asc(TableSortField::Position),
            limit: 2,
            cursor,
            parent_id: None,
            hierarchy: false,
        })
        .await
        .expect("table_rows")
}

macro_rules! fixture {
    () => {
        match setup().await {
            Some(fx) => fx,
            None => {
                eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
                return;
            }
        }
    };
}

/// 种子：`todo` ×2（high / none）+ `done` ×1（urgent），返回按创建顺序的 issue。
async fn seed_three(fx: &Fixture) -> IssueRepo {
    let repo = IssueRepo::new(fx.db.clone());
    let mut high = new_issue(fx, "high-todo");
    high.priority = Priority::High;
    let plain = new_issue(fx, "plain-todo");
    let mut urgent_done = new_issue(fx, "urgent-done");
    urgent_done.status = "done".to_string();
    urgent_done.priority = Priority::Urgent;
    for issue in [high, plain, urgent_done] {
        repo.create(issue).await.expect("create issue");
    }
    repo
}

/// 1) `table_groups`：状态计数 + `include_empty` 补齐内置状态 + 固定顺序。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn db_table_groups_counts_by_status() {
    let fx = fixture!();
    seed_three(&fx).await;
    let table = IssueTableRepo::new(fx.db.clone());

    let page = table
        .table_groups(&TableGroupsQuery {
            workspace_id: fx.workspace_id,
            filter: workspace_filter(),
            group: group_spec(TableGroupKind::Status, false),
            limit: 50,
            cursor: None,
        })
        .await
        .expect("table_groups");
    assert_eq!(page.total, 3, "total = 全部匹配 issue，不是页内和");
    assert_eq!(page.groups.len(), 2, "只有出现过的状态");
    assert_eq!(count_of(&page.groups, "status:todo"), Some(2));
    assert_eq!(count_of(&page.groups, "status:done"), Some(1));
    assert!(page.next.is_none(), "3 个分组 < limit，不应该有下一页");

    // include_empty：补齐 7 个内置状态（计数 0 也出现），顺序按内置目录
    let with_empty = table
        .table_groups(&TableGroupsQuery {
            workspace_id: fx.workspace_id,
            filter: workspace_filter(),
            group: group_spec(TableGroupKind::Status, true),
            limit: 50,
            cursor: None,
        })
        .await
        .expect("table_groups include_empty");
    assert_eq!(with_empty.total, 3);
    assert_eq!(with_empty.groups.len(), 7);
    assert_eq!(count_of(&with_empty.groups, "status:blocked"), Some(0));
    assert_eq!(with_empty.groups[0].key, "status:backlog");
    assert_eq!(
        with_empty.groups.last().map(|g| g.key.as_str()),
        Some("status:cancelled")
    );

    teardown(&fx).await;
}

/// 2) `table_groups` + `limit`：分页游标能续到第二页。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn db_table_groups_paginates_with_cursor() {
    let fx = fixture!();
    seed_three(&fx).await;
    let table = IssueTableRepo::new(fx.db.clone());

    let first = table
        .table_groups(&TableGroupsQuery {
            workspace_id: fx.workspace_id,
            filter: workspace_filter(),
            group: group_spec(TableGroupKind::Status, true),
            limit: 2,
            cursor: None,
        })
        .await
        .expect("first page");
    assert_eq!(first.total, 3);
    assert_eq!(first.groups.len(), 2);
    let next = first.next.expect("limit=2 < 7 个内置状态，应有 next");
    assert_eq!(next.order, 2, "游标停在第二个分组的 order 上");

    let second = table
        .table_groups(&TableGroupsQuery {
            workspace_id: fx.workspace_id,
            filter: workspace_filter(),
            group: group_spec(TableGroupKind::Status, true),
            limit: 2,
            cursor: Some(next),
        })
        .await
        .expect("second page");
    assert_eq!(second.groups.len(), 2);
    assert_eq!(second.groups[0].key, "status:in_progress");
    assert_ne!(second.groups[0].key, first.groups[1].key);

    teardown(&fx).await;
}

/// 3) `table_rows`：keyset 分页 + `total` 只在首页出现。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn db_table_rows_keyset_pagination() {
    let fx = fixture!();
    let repo = IssueRepo::new(fx.db.clone());
    for title in ["r1", "r2", "r3", "r4"] {
        repo.create(new_issue(&fx, title)).await.expect("create");
    }
    let table = IssueTableRepo::new(fx.db.clone());

    let first = rows_page(&table, fx.workspace_id, workspace_filter(), None).await;
    assert_eq!(first.total, Some(4), "首页（group.kind=none）带总数");
    assert_eq!(first.rows.len(), 2);
    assert_eq!(first.rows[0].issue.number, 1);
    assert_eq!(first.rows[1].issue.number, 2);
    assert_eq!(first.rows[0].direct_child_count, 0);
    let next = first.next.expect("limit=2 < 4，应有 next cursor");

    let second = rows_page(&table, fx.workspace_id, workspace_filter(), Some(next)).await;
    assert_eq!(second.total, None, "翻页页头不带 total（与上游一致）");
    assert_eq!(second.rows.len(), 2);
    assert_eq!(second.rows[0].issue.number, 3);
    assert_eq!(second.rows[1].issue.number, 4);
    assert!(second.next.is_none(), "最后页不应再有 next");

    teardown(&fx).await;
}

/// 4) `table_rows` + `group_key=status:todo`：只取该分组的行，且不是首页（无 total）。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn db_table_rows_filtered_by_group_key() {
    let fx = fixture!();
    seed_three(&fx).await;
    let table = IssueTableRepo::new(fx.db.clone());

    let page = table
        .table_rows(&TableRowsQuery {
            workspace_id: fx.workspace_id,
            filter: workspace_filter(),
            group: group_spec(TableGroupKind::Status, false),
            group_key: TableGroupKey::Status("todo".to_string()),
            order: asc(TableSortField::Position),
            limit: 50,
            cursor: None,
            parent_id: None,
            hierarchy: false,
        })
        .await
        .expect("table_rows by group");
    assert_eq!(page.rows.len(), 2, "只有 2 个 todo");
    assert!(page.rows.iter().all(|row| row.issue.status == "todo"));
    assert_eq!(page.total, None, "带 group_key 的行页不算首页");

    teardown(&fx).await;
}

/// 5) `table_facets`：多维度计数 + disjunctive（facet 不吃自己那一维的过滤）+ total。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn db_table_facets_disjunctive_counts() {
    let fx = fixture!();
    seed_three(&fx).await;
    let table = IssueTableRepo::new(fx.db.clone());

    let mut filter = workspace_filter();
    filter.statuses = vec!["todo".to_string()];
    let page = table
        .table_facets(&TableFacetsQuery {
            workspace_id: fx.workspace_id,
            filter: filter.clone(),
            facets: vec![TableFacetKind::Status, TableFacetKind::Priority],
            include_total: true,
        })
        .await
        .expect("table_facets");
    assert_eq!(page.total, 2, "total 带上 statuses 过滤");
    assert_eq!(page.facets.len(), 2);

    let status = &page.facets[0];
    assert_eq!(
        count_of_in_values(&status.values, "todo"),
        Some(2),
        "status facet 忽略自身维度过滤"
    );
    assert_eq!(count_of_in_values(&status.values, "done"), Some(1));

    let priority = &page.facets[1];
    assert_eq!(count_of_in_values(&priority.values, "high"), Some(1));
    assert_eq!(
        count_of_in_values(&priority.values, "urgent"),
        None,
        "urgent 属于 done，被 statuses=todo 过滤掉"
    );

    // include_total=false → total = 0（上游零值）
    let without_total = table
        .table_facets(&TableFacetsQuery {
            workspace_id: fx.workspace_id,
            filter: workspace_filter(),
            facets: vec![TableFacetKind::Priority],
            include_total: false,
        })
        .await
        .expect("facets without total");
    assert_eq!(without_total.total, 0);
    assert_eq!(without_total.facets.len(), 1);

    teardown(&fx).await;
}

/// 6) 作用域：`assignee` scope 只取该 assignee 的 issue；未指派分组归入 unassigned。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn db_table_filters_by_assignee_scope() {
    let fx = fixture!();
    let repo = IssueRepo::new(fx.db.clone());
    let mut assigned = new_issue(&fx, "assigned");
    assigned.assignee_type = Some(AssigneeType::User);
    assigned.assignee_id = Some(fx.user_id.0.to_string());
    repo.create(assigned).await.expect("create assigned");
    repo.create(new_issue(&fx, "unassigned"))
        .await
        .expect("create unassigned");
    let table = IssueTableRepo::new(fx.db.clone());

    let rows = table
        .table_rows(&TableRowsQuery {
            workspace_id: fx.workspace_id,
            filter: TableFilter {
                scope: TableScope::Assignee(TableActor {
                    kind: "user".to_string(),
                    id: fx.user_id.0,
                }),
                ..workspace_filter()
            },
            group: no_group(),
            group_key: TableGroupKey::None,
            order: asc(TableSortField::Position),
            limit: 50,
            cursor: None,
            parent_id: None,
            hierarchy: false,
        })
        .await
        .expect("assignee scope rows");
    assert_eq!(rows.rows.len(), 1);
    assert_eq!(rows.rows[0].issue.title, "assigned");

    let groups = table
        .table_groups(&TableGroupsQuery {
            workspace_id: fx.workspace_id,
            filter: workspace_filter(),
            group: group_spec(TableGroupKind::Assignee, false),
            limit: 50,
            cursor: None,
        })
        .await
        .expect("assignee groups");
    assert_eq!(groups.total, 2);
    assert_eq!(
        count_of(&groups.groups, &format!("assignee:user:{}", fx.user_id.0)),
        Some(1)
    );
    assert_eq!(count_of(&groups.groups, "assignee:unassigned"), Some(1));

    teardown(&fx).await;
}
