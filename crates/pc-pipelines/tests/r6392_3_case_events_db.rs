//! R639.2.3: pc-pipelines::case_events_db DB glue integration tests (real PG).
//!
//! Verifies:
//! - list_company_case_events_page with and without type filter
//! - automation context enrichment (routine + issue)
//! - has_more pagination behavior
//! - get_direct_children_summary counts (done/dropped/in_motion)

use pc_pipelines::aggregation::{
    bounded_limit, stage_automation_from_config, COMPANY_CASE_EVENTS_DEFAULT_LIMIT,
};
use pc_pipelines::case_events_db::{
    get_direct_children_summary, list_company_case_events, list_company_case_events_page,
    get_case_children_tree, lookup_issues_by_ids, lookup_routines_by_ids,
    lookup_stages_by_pipeline_ids, IssueRefRow, RoutineRefRow, StageConfigRow,
    load_descendant_active_work_counts_for_cases,
    load_pipeline_descendant_active_work_counts,
    load_pipeline_connections,
};
use pc_pipelines::case_events_enrichment::{
    enrich_cases_with_aggregation, enrich_pipelines_with_aggregation,
    EnrichedCaseRow, EnrichedPipelineRow,
};
use pc_repos::Db;
use serde_json::json;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn connect() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn cleanup(db: &Db) {
    let _ = sqlx::query("DELETE FROM pipeline_case_events WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'r63923-%')")
        .execute(db.pool()).await;
    let _ = sqlx::query("DELETE FROM pipeline_case_issue_links WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'r63923-%')")
        .execute(db.pool()).await;
    let _ = sqlx::query("DELETE FROM pipeline_cases WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'r63923-%')")
        .execute(db.pool()).await;
    let _ = sqlx::query("DELETE FROM pipeline_stages WHERE pipeline_id IN (SELECT id FROM pipelines WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'r63923-%'))")
        .execute(db.pool()).await;
    let _ = sqlx::query("DELETE FROM pipelines WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'r63923-%')")
        .execute(db.pool()).await;
    let _ = sqlx::query("DELETE FROM companies WHERE name LIKE 'r63923-%'")
        .execute(db.pool()).await;
}

async fn fixture(db: &Db, label: &str) -> (Uuid, Uuid) {
    let company_id = Uuid::new_v4();
    let pipeline_id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) VALUES ($1, $2, 'active', $3, now(), now())")
        .bind(company_id).bind(format!("r63923-{label}-{company_id}")).bind(format!("R{}", Uuid::new_v4().simple().to_string().chars().take(5).collect::<String>()))
        .execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO pipelines (id, company_id, key, name, created_at, updated_at) VALUES ($1, $2, $3, $4, now(), now())")
        .bind(pipeline_id).bind(company_id).bind(format!("p-{label}")).bind(format!("Pipeline {label}"))
        .execute(db.pool()).await.unwrap();
    (company_id, pipeline_id)
}

async fn insert_stage(db: &Db, pipeline_id: Uuid, key: &str, kind: &str, config: serde_json::Value) -> Uuid {
    let stage_id = Uuid::new_v4();
    sqlx::query("INSERT INTO pipeline_stages (id, pipeline_id, key, name, kind, position, config, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, 0, $6, now(), now())")
        .bind(stage_id).bind(pipeline_id).bind(key).bind(format!("Stage {key}")).bind(kind).bind(config)
        .execute(db.pool()).await.unwrap();
    stage_id
}

async fn insert_case(db: &Db, company_id: Uuid, pipeline_id: Uuid, stage_id: Uuid, key_prefix: &str) -> Uuid {
    let case_id = Uuid::new_v4();
    sqlx::query("INSERT INTO pipeline_cases (id, company_id, pipeline_id, stage_id, case_key, title, fields, child_count, terminal_child_count, version, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, '{}'::jsonb, 0, 0, 1, now(), now())")
        .bind(case_id).bind(company_id).bind(pipeline_id).bind(stage_id).bind(format!("{key_prefix}-{case_id}")).bind(format!("Case {key_prefix} {case_id}"))
        .execute(db.pool()).await.unwrap();
    case_id
}

async fn insert_event(db: &Db, company_id: Uuid, case_id: Uuid, event_type: &str, payload: serde_json::Value) -> Uuid {
    let event_id = Uuid::new_v4();
    sqlx::query("INSERT INTO pipeline_case_events (id, company_id, case_id, type, actor_type, payload, created_at, updated_at) VALUES ($1, $2, $3, $4, 'system', $5, now(), now())")
        .bind(event_id).bind(company_id).bind(case_id).bind(event_type).bind(payload)
        .execute(db.pool()).await.unwrap();
    event_id
}

async fn insert_child_case(db: &Db, company_id: Uuid, pipeline_id: Uuid, stage_id: Uuid, parent_id: Uuid, terminal: Option<&str>) -> Uuid {
    let case_id = Uuid::new_v4();
    sqlx::query("INSERT INTO pipeline_cases (id, company_id, pipeline_id, stage_id, case_key, title, fields, child_count, terminal_child_count, version, parent_case_id, terminal_kind, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, '{}'::jsonb, 0, 0, 1, $7, $8, now(), now())")
        .bind(case_id).bind(company_id).bind(pipeline_id).bind(stage_id).bind(format!("CHILD-{case_id}")).bind(format!("Child {case_id}")).bind(parent_id).bind(terminal)
        .execute(db.pool()).await.unwrap();
    case_id
}

#[tokio::test]
async fn r63923_bounded_limit_inherited() {
    assert_eq!(bounded_limit(None, COMPANY_CASE_EVENTS_DEFAULT_LIMIT, 200), COMPANY_CASE_EVENTS_DEFAULT_LIMIT);
    assert_eq!(bounded_limit(Some(0), COMPANY_CASE_EVENTS_DEFAULT_LIMIT, 200), 1);
}

#[tokio::test]
async fn r63923_stage_automation_from_config_parses_on_enter() {
    let cfg = json!({"onEnter": {"type": "run_routine", "routineId": "r-1", "id": "auto-1"}});
    let auto = stage_automation_from_config("stage-1", &cfg).expect("auto");
    assert_eq!(auto.id, "auto-1");
    assert_eq!(auto.routine_id, "r-1");
    let cfg2 = json!({"onEnter": {"type": "run_routine", "routineId": "  r-2  "}});
    let auto2 = stage_automation_from_config("stage-2", &cfg2).expect("auto2");
    assert_eq!(auto2.routine_id, "r-2");
    assert_eq!(auto2.id, "stage-2:on_enter");
    let cfg3 = json!({"onEnter": {"type": "other"}});
    assert!(stage_automation_from_config("stage-3", &cfg3).is_none());
}

#[tokio::test]
async fn r63923_list_company_case_events_basic() {
    let db = connect().await;
    cleanup(&db).await;
    let (company_id, pipeline_id) = fixture(&db, "basic").await;
    let stage_id = insert_stage(&db, pipeline_id, "working", "working", json!({})).await;
    let case_id = insert_case(&db, company_id, pipeline_id, stage_id, "EVT").await;
    insert_event(&db, company_id, case_id, "ingested", json!({})).await;
    insert_event(&db, company_id, case_id, "claimed", json!({"agentId": "a-1"})).await;

    let rows = list_company_case_events(db.pool(), company_id, &[], Some(50), Some(0)).await.expect("list");
    assert_eq!(rows.len(), 2);
    let types: Vec<String> = rows.iter().map(|r| r.event_type.clone()).collect();
    let has_ingested = types.iter().any(|t| t == "ingested");
    let has_claimed = types.iter().any(|t| t == "claimed");
    assert!(has_ingested && has_claimed);

    let rows2 = list_company_case_events(db.pool(), company_id, &["claimed".to_string()], Some(50), Some(0)).await.expect("list filtered");
    assert_eq!(rows2.len(), 1);
    assert_eq!(rows2[0].event_type, "claimed");

    cleanup(&db).await;
}

#[tokio::test]
async fn r63923_list_company_case_events_page_pagination() {
    let db = connect().await;
    cleanup(&db).await;
    let (company_id, pipeline_id) = fixture(&db, "page").await;
    let stage_id = insert_stage(&db, pipeline_id, "working", "working", json!({})).await;
    let case_id = insert_case(&db, company_id, pipeline_id, stage_id, "PAG").await;
    for i in 0..5 {
        insert_event(&db, company_id, case_id, "ingested", json!({"i": i})).await;
    }
    let page = list_company_case_events_page(db.pool(), company_id, &[], Some(3), Some(0)).await.expect("page");
    assert_eq!(page.items.len(), 3);
    assert_eq!(page.limit, 3);
    assert_eq!(page.offset, 0);
    assert!(page.has_more);
    assert_eq!(page.total, 3);

    let page2 = list_company_case_events_page(db.pool(), company_id, &[], Some(3), Some(3)).await.expect("page2");
    assert_eq!(page2.items.len(), 2);
    assert!(!page2.has_more);
    cleanup(&db).await;
}

#[tokio::test]
async fn r63923_get_direct_children_summary() {
    let db = connect().await;
    cleanup(&db).await;
    let (company_id, pipeline_id) = fixture(&db, "children").await;
    let stage_id = insert_stage(&db, pipeline_id, "working", "working", json!({})).await;
    let parent_id = insert_case(&db, company_id, pipeline_id, stage_id, "PARENT").await;
    insert_child_case(&db, company_id, pipeline_id, stage_id, parent_id, Some("done")).await;
    insert_child_case(&db, company_id, pipeline_id, stage_id, parent_id, Some("done")).await;
    insert_child_case(&db, company_id, pipeline_id, stage_id, parent_id, Some("cancelled")).await;
    insert_child_case(&db, company_id, pipeline_id, stage_id, parent_id, None).await;

    let rollup = get_direct_children_summary(db.pool(), company_id, parent_id).await.expect("rollup");
    assert_eq!(rollup.total, 4);
    assert_eq!(rollup.done, 2);
    assert_eq!(rollup.dropped, 1);
    assert_eq!(rollup.in_motion, 1);

    let empty = get_direct_children_summary(db.pool(), company_id, Uuid::new_v4()).await.expect("empty");
    assert_eq!(empty.total, 0);

    cleanup(&db).await;
}

#[tokio::test]
async fn r63923_automation_context_enriches_when_stage_matches() {
    let db = connect().await;
    cleanup(&db).await;
    let (company_id, pipeline_id) = fixture(&db, "auto").await;
    let stage_cfg = json!({"onEnter": {"type": "run_routine", "routineId": "r-x", "id": "auto-x"}});
    let stage_id = insert_stage(&db, pipeline_id, "entry", "working", stage_cfg).await;
    let case_id = insert_case(&db, company_id, pipeline_id, stage_id, "AUTO").await;
    let routine_id = Uuid::new_v4();
    let issue_id = Uuid::new_v4();
    sqlx::query("INSERT INTO routines (id, company_id, title, priority, status, concurrency_policy, catch_up_policy, created_at, updated_at) VALUES ($1, $2, 'My Routine', 'medium', 'active', 'coalesce_if_active', 'skip_missed', now(), now())")
        .bind(routine_id).bind(company_id)
        .execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO issues (id, company_id, title, description, status, priority, hidden_at, harness_kind, created_at, updated_at) VALUES ($1, $2, 'My Issue', 'd', 'in_progress', 'medium', NULL, NULL, now(), now())")
        .bind(issue_id).bind(company_id)
        .execute(db.pool()).await.unwrap();
    insert_event(&db, company_id, case_id, "automation_executed", json!({"routineId": routine_id.to_string(), "issueId": issue_id.to_string()})).await;

    let page = list_company_case_events_page(db.pool(), company_id, &["automation_executed".to_string()], Some(50), Some(0)).await.expect("page");
    assert_eq!(page.items.len(), 1);
    let item = &page.items[0];
    let automation = item.automation.as_ref().expect("automation context");
    let routine = automation.routine.as_ref().expect("routine");
    assert_eq!(routine.id, routine_id.to_string());
    assert_eq!(routine.title, "My Routine");
    let issue = automation.issue.as_ref().expect("issue");
    assert_eq!(issue.id, issue_id.to_string());
    assert_eq!(issue.status, "in_progress");

    cleanup(&db).await;
}

#[tokio::test]
async fn r63923_lookup_helpers_skip_when_empty() {
    let db = connect().await;
    let empty_routines: Vec<RoutineRefRow> = lookup_routines_by_ids(db.pool(), Uuid::new_v4(), &[]).await.expect("empty routines");
    assert!(empty_routines.is_empty());
    let empty_issues: Vec<IssueRefRow> = lookup_issues_by_ids(db.pool(), Uuid::new_v4(), &[]).await.expect("empty issues");
    assert!(empty_issues.is_empty());
    let empty_stages: Vec<StageConfigRow> = lookup_stages_by_pipeline_ids(db.pool(), &[]).await.expect("empty stages");
    assert!(empty_stages.is_empty());
}

#[tokio::test]
async fn r63923_get_case_children_tree_returns_none_for_missing_case() {
    let db = connect().await;
    let result = get_case_children_tree(db.pool(), Uuid::new_v4(), Uuid::new_v4()).await.expect("tree");
    assert!(result.is_none(), "unknown case returns None");
}

#[tokio::test]
async fn r63923_get_case_children_tree_builds_nested_tree() {
    let db = connect().await;
    cleanup(&db).await;
    let (company_id, pipeline_id) = fixture(&db, "tree").await;
    let stage_id = insert_stage(&db, pipeline_id, "working", "working", json!({})).await;
    let root_id = insert_case(&db, company_id, pipeline_id, stage_id, "ROOT").await;
    let child1_id = insert_child_case(&db, company_id, pipeline_id, stage_id, root_id, None).await;
    let child2_id = insert_child_case(&db, company_id, pipeline_id, stage_id, root_id, Some("done")).await;
    insert_child_case(&db, company_id, pipeline_id, stage_id, child1_id, None).await;

    let tree = get_case_children_tree(db.pool(), company_id, root_id).await.expect("tree");
    let t = tree.expect("tree present");
    assert_eq!(t.case.id, root_id.to_string());
    assert!(!t.truncated);
    assert_eq!(t.total_nodes, 4);
    assert_eq!(t.case.child_groups.len(), 1);
    assert_eq!(t.case.child_groups[0].cases.len(), 2);
    let rollup = &t.case.rollup;
    assert_eq!(rollup.total, 3);
    assert_eq!(rollup.done, 1);
    assert_eq!(rollup.in_motion, 2);
    assert_eq!(rollup.dropped, 0);

    let grandchild = &t.case.child_groups[0].cases.iter().find(|c| c.id == child1_id.to_string()).expect("child1").child_groups;
    assert_eq!(grandchild.len(), 1);
    assert_eq!(grandchild[0].cases.len(), 1);
    let _ = child2_id;

    cleanup(&db).await;
}


// ============================================================================
// R639.2.5: pipelines-aggregation utility functions
// ============================================================================

async fn insert_agent(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, status, adapter_type, adapter_config, capabilities, budget_monthly_cents, spent_monthly_cents, runtime_config, permissions, created_at, updated_at) VALUES ($1, $2, $3, 'idle', 'process', '{}'::jsonb, '', 0, 0, '{}'::jsonb, '{}'::jsonb, now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("agent-{}", id))
    .execute(db.pool())
    .await
    .expect("insert agent");
    id
}

async fn insert_issue(db: &Db, company_id: Uuid, assignee: Option<Uuid>, status: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, assignee_agent_id, request_depth, created_at, updated_at) VALUES ($1, $2, $3, $4, 'normal', $5, 0, now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("Issue-{}", id))
    .bind(status)
    .bind(assignee)
    .execute(db.pool())
    .await
    .expect("insert issue");
    id
}

async fn insert_case_link(db: &Db, company_id: Uuid, case_id: Uuid, issue_id: Uuid, role: &str) {
    sqlx::query(
        "INSERT INTO pipeline_case_issue_links (id, company_id, case_id, issue_id, role, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, now(), now())",
    )
    .bind(Uuid::new_v4())
    .bind(company_id)
    .bind(case_id)
    .bind(issue_id)
    .bind(role)
    .execute(db.pool())
    .await
    .expect("insert case link");
}

async fn insert_pipeline_with_stage(db: &Db, company_id: Uuid, label: &str) -> (Uuid, Uuid) {
    let pipeline_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO pipelines (id, company_id, key, name, created_at, updated_at) VALUES ($1, $2, $3, $4, now(), now())",
    )
    .bind(pipeline_id)
    .bind(company_id)
    .bind(format!("pipe-{}", label))
    .bind(format!("Pipeline {}", label))
    .execute(db.pool())
    .await
    .expect("insert pipeline");
    let stage_id = insert_stage(db, pipeline_id, "working", "working", json!({})).await;
    (pipeline_id, stage_id)
}

async fn cleanup_r63925(db: &Db) {
    let _ = sqlx::query("DELETE FROM pipeline_case_issue_links WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'r63925-%')")
        .execute(db.pool()).await;
    let _ = sqlx::query("DELETE FROM pipeline_cases WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'r63925-%')")
        .execute(db.pool()).await;
    let _ = sqlx::query("DELETE FROM pipeline_stages WHERE pipeline_id IN (SELECT id FROM pipelines WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'r63925-%'))")
        .execute(db.pool()).await;
    let _ = sqlx::query("DELETE FROM pipelines WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'r63925-%')")
        .execute(db.pool()).await;
    let _ = sqlx::query("DELETE FROM issues WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'r63925-%')")
        .execute(db.pool()).await;
    let _ = sqlx::query("DELETE FROM agents WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'r63925-%')")
        .execute(db.pool()).await;
    let _ = sqlx::query("DELETE FROM companies WHERE name LIKE 'r63925-%'")
        .execute(db.pool()).await;
}

async fn r63925_fixture(db: &Db, label: &str) -> (Uuid, Uuid, Uuid) {
    let company_id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) VALUES ($1, $2, 'active', $3, now(), now())")
        .bind(company_id)
        .bind(format!("r63925-{}-{}", label, company_id))
        .bind(format!("R{}", Uuid::new_v4().simple().to_string().chars().take(5).collect::<String>()))
        .execute(db.pool()).await.expect("insert company");
    let (pipeline_id, stage_id) = insert_pipeline_with_stage(db, company_id, label).await;
    (company_id, pipeline_id, stage_id)
}


#[tokio::test]
async fn r63925_load_descendant_active_work_counts_for_cases_empty_input() {
    let db = connect().await;
    let rows = load_descendant_active_work_counts_for_cases(db.pool(), Uuid::new_v4(), &[])
        .await
        .expect("empty");
    assert!(rows.is_empty());
}

#[tokio::test]
async fn r63925_load_descendant_active_work_counts_for_cases_counts_active_work() {
    let db = connect().await;
    cleanup_r63925(&db).await;
    let (company_id, pipeline_id, stage_id) = r63925_fixture(&db, "counts").await;
    let agent = insert_agent(&db, company_id).await;

    let root_empty = insert_case(&db, company_id, pipeline_id, stage_id, "ROOT-EMPTY").await;
    let root_with_work = insert_case(&db, company_id, pipeline_id, stage_id, "ROOT-WORK").await;
    let child_a = insert_child_case(&db, company_id, pipeline_id, stage_id, root_with_work, None).await;
    let child_b = insert_child_case(&db, company_id, pipeline_id, stage_id, root_with_work, None).await;

    let in_progress_a1 = insert_issue(&db, company_id, Some(agent), "in_progress").await;
    let in_progress_b1 = insert_issue(&db, company_id, Some(agent), "in_progress").await;
    let done_issue = insert_issue(&db, company_id, Some(agent), "done").await;
    let open_issue = insert_issue(&db, company_id, Some(agent), "open").await;

    // Two distinct descendant cases each carrying at least one in_progress work/automation link
    insert_case_link(&db, company_id, child_a, in_progress_a1, "work").await;
    insert_case_link(&db, company_id, child_b, in_progress_b1, "automation").await;
    // Non-counting links (done work on child_a, open automation on child_b) — verify they don't count
    insert_case_link(&db, company_id, child_a, done_issue, "work").await;
    insert_case_link(&db, company_id, child_b, open_issue, "work").await;

    let rows = load_descendant_active_work_counts_for_cases(
        db.pool(),
        company_id,
        &[root_empty, root_with_work, root_with_work],
    )
    .await
    .expect("rows");

    let by_root: std::collections::HashMap<Uuid, i64> =
        rows.iter().map(|r| (r.root_id, r.count)).collect();
    assert_eq!(by_root.get(&root_empty).copied().unwrap_or(0), 0);
    assert_eq!(by_root.get(&root_with_work).copied().unwrap_or(0), 2);

    cleanup_r63925(&db).await;
}

#[tokio::test]
async fn r63925_load_descendant_active_work_counts_for_cases_unassigned_issues_excluded() {
    let db = connect().await;
    cleanup_r63925(&db).await;
    let (company_id, pipeline_id, stage_id) = r63925_fixture(&db, "no-agt").await;

    let root = insert_case(&db, company_id, pipeline_id, stage_id, "ROOT-NA").await;
    let child = insert_child_case(&db, company_id, pipeline_id, stage_id, root, None).await;

    let unassigned = insert_issue(&db, company_id, None, "in_progress").await;
    insert_case_link(&db, company_id, child, unassigned, "work").await;

    let rows = load_descendant_active_work_counts_for_cases(db.pool(), company_id, &[root])
        .await
        .expect("rows");
    let count = rows.iter().find(|r| r.root_id == root).map(|r| r.count).unwrap_or(0);
    assert_eq!(count, 0, "unassigned in_progress work must not count");

    cleanup_r63925(&db).await;
}

#[tokio::test]
async fn r63925_load_pipeline_descendant_active_work_counts_empty_input() {
    let db = connect().await;
    let rows = load_pipeline_descendant_active_work_counts(db.pool(), Uuid::new_v4(), &[])
        .await
        .expect("empty");
    assert!(rows.is_empty());
}

#[tokio::test]
async fn r63925_load_pipeline_descendant_active_work_counts_groups_by_pipeline() {
    let db = connect().await;
    cleanup_r63925(&db).await;
    let (company_id, p1, s1) = r63925_fixture(&db, "p1").await;
    let (p2, s2) = insert_pipeline_with_stage(&db, company_id, "p2").await;
    let agent = insert_agent(&db, company_id).await;

    let p1_root_a = insert_case(&db, company_id, p1, s1, "P1-A").await;
    let p1_root_b = insert_case(&db, company_id, p1, s1, "P1-B").await;
    let p1_child_a = insert_child_case(&db, company_id, p1, s1, p1_root_a, None).await;
    let p1_child_b = insert_child_case(&db, company_id, p1, s1, p1_root_b, None).await;

    let p2_root = insert_case(&db, company_id, p2, s2, "P2-A").await;
    let p2_child_a = insert_child_case(&db, company_id, p2, s2, p2_root, None).await;
    let p2_child_b = insert_child_case(&db, company_id, p2, s2, p2_root, None).await;

    let p1_issue_a = insert_issue(&db, company_id, Some(agent), "in_progress").await;
    let p1_issue_b = insert_issue(&db, company_id, Some(agent), "in_progress").await;
    let p2_issue_a = insert_issue(&db, company_id, Some(agent), "in_progress").await;
    let p2_issue_b = insert_issue(&db, company_id, Some(agent), "in_progress").await;
    let p2_done = insert_issue(&db, company_id, Some(agent), "done").await;

    insert_case_link(&db, company_id, p1_child_a, p1_issue_a, "work").await;
    insert_case_link(&db, company_id, p1_child_b, p1_issue_b, "work").await;
    insert_case_link(&db, company_id, p2_child_a, p2_issue_a, "work").await;
    insert_case_link(&db, company_id, p2_child_b, p2_issue_b, "automation").await;
    insert_case_link(&db, company_id, p2_child_a, p2_done, "work").await;

    let rows = load_pipeline_descendant_active_work_counts(
        db.pool(),
        company_id,
        &[p1, p2, p1],
    )
    .await
    .expect("rows");

    let by_pipe: std::collections::HashMap<Uuid, i64> =
        rows.iter().map(|r| (r.pipeline_id, r.count)).collect();
    assert_eq!(by_pipe.get(&p1).copied().unwrap_or(0), 2);
    assert_eq!(by_pipe.get(&p2).copied().unwrap_or(0), 2);

    cleanup_r63925(&db).await;
}

#[tokio::test]
async fn r63925_load_pipeline_connections_returns_cross_pipeline_parent_child() {
    let db = connect().await;
    cleanup_r63925(&db).await;
    let (company_id, p_a, s_a) = r63925_fixture(&db, "c-a").await;
    let (p_b, s_b) = insert_pipeline_with_stage(&db, company_id, "c-b").await;
    let (p_c, s_c) = insert_pipeline_with_stage(&db, company_id, "c-c").await;

    let parent_in_a = insert_case(&db, company_id, p_a, s_a, "PA-P").await;
    let _child_in_b = insert_child_case(&db, company_id, p_b, s_b, parent_in_a, None).await;

    let parent2_in_a = insert_case(&db, company_id, p_a, s_a, "PA-P2").await;
    let _child_in_c = insert_child_case(&db, company_id, p_c, s_c, parent2_in_a, None).await;

    let same_pipe_parent = insert_case(&db, company_id, p_a, s_a, "SAME").await;
    let _same_pipe_child = insert_child_case(&db, company_id, p_a, s_a, same_pipe_parent, None).await;

    let _lonely = insert_case(&db, company_id, p_b, s_b, "LONELY").await;

    let rows = load_pipeline_connections(db.pool(), company_id)
        .await
        .expect("rows");

    let mut pairs: Vec<(Uuid, Uuid)> =
        rows.iter().map(|r| (r.parent_pipeline_id, r.child_pipeline_id)).collect();
    pairs.sort_by_key(|p| (p.0.to_string(), p.1.to_string()));

    let mut expected = vec![(p_a, p_b), (p_a, p_c)];
    expected.sort_by_key(|p| (p.0.to_string(), p.1.to_string()));
    assert_eq!(pairs, expected, "cross-pipeline pairs must match exactly");

    cleanup_r63925(&db).await;
}

#[tokio::test]
async fn r63925_load_pipeline_connections_isolated_by_company() {
    let db = connect().await;
    cleanup_r63925(&db).await;
    let (c1, p_a, s_a) = r63925_fixture(&db, "iso-1").await;
    let (p_b, s_b) = insert_pipeline_with_stage(&db, c1, "iso-b").await;
    let parent = insert_case(&db, c1, p_a, s_a, "C1-P").await;
    let _child = insert_child_case(&db, c1, p_b, s_b, parent, None).await;

    let (c2, _p_x, _s_x) = r63925_fixture(&db, "iso-2").await;
    let rows_c2 = load_pipeline_connections(db.pool(), c2).await.expect("rows c2");
    assert!(rows_c2.is_empty(), "other-company connections must not leak");

    cleanup_r63925(&db).await;
}


// ============================================================================
// R639.2.6: pipeline-level aggregation enrichment integration tests
//   enrich_pipelines_with_aggregation (connections + descendant_active_work_count)
// ============================================================================

async fn insert_pipeline_via_repo(db: &Db, company_id: Uuid, key: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO pipelines (id, company_id, key, name, created_at, updated_at) VALUES ($1, $2, $3, $4, now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("{key}-{id}"))
    .bind(format!("Pipeline {key}"))
    .execute(db.pool())
    .await
    .expect("insert pipeline");
    id
}

#[tokio::test]
async fn r63926_enrich_pipelines_empty_input_returns_empty() {
    let db = connect().await;
    let enriched = enrich_pipelines_with_aggregation(db.pool(), Uuid::new_v4(), vec![])
        .await
        .expect("empty");
    assert!(enriched.is_empty());
}

#[tokio::test]
async fn r63926_enrich_pipelines_assigns_default_zero_and_empty_when_no_data() {
    let db = connect().await;
    cleanup_r63925(&db).await;
    let (company_id, _pipeline_id, stage_id) = r63925_fixture(&db, "enrich-empty").await;

    // pipeline with no cases / no cross-pipeline edges / no in-progress work
    let pipeline_id = insert_pipeline_via_repo(&db, company_id, "lonely").await;
    let _case = insert_case(&db, company_id, pipeline_id, stage_id, "LONELY").await;

    let rows = pc_repos::pipeline::PipelineRepo::new(&db)
        .list_by_company(company_id)
        .await
        .expect("list");
    assert!(!rows.is_empty(), "fixture pipeline must exist");

    let enriched = enrich_pipelines_with_aggregation(db.pool(), company_id, rows)
        .await
        .expect("enrich");
    let pipeline = enriched
        .iter()
        .find(|p| p.id == pipeline_id)
        .expect("our pipeline in result");
    assert_eq!(pipeline.descendant_active_work_count, 0);
    assert!(pipeline.connections.upstream_pipeline_ids.is_empty());
    assert!(pipeline.connections.downstream_pipeline_ids.is_empty());

    cleanup_r63925(&db).await;
}

#[tokio::test]
async fn r63926_enrich_pipelines_populates_descendant_active_work_and_connections() {
    let db = connect().await;
    cleanup_r63925(&db).await;
    let (company_id, _default_pipe, _default_stage) =
        r63925_fixture(&db, "enrich-full").await;
    let agent = insert_agent(&db, company_id).await;

    // Pipeline A: root case + child case with in_progress work
    let p_a = insert_pipeline_via_repo(&db, company_id, "p-a").await;
    let s_a = insert_stage(&db, p_a, "working", "working", json!({})).await;
    let a_root = insert_case(&db, company_id, p_a, s_a, "PA-R").await;
    let a_child = insert_child_case(&db, company_id, p_a, s_a, a_root, None).await;
    let issue = insert_issue(&db, company_id, Some(agent), "in_progress").await;
    insert_case_link(&db, company_id, a_child, issue, "work").await;

    // Pipeline B: receives a cross-pipeline child (parent in A, child in B)
    let p_b = insert_pipeline_via_repo(&db, company_id, "p-b").await;
    let s_b = insert_stage(&db, p_b, "working", "working", json!({})).await;
    let _b_root = insert_case(&db, company_id, p_a, s_a, "PA-ROOT2").await;
    let _cross_child = insert_child_case(&db, company_id, p_b, s_b, _b_root, None).await;

    let rows = pc_repos::pipeline::PipelineRepo::new(&db)
        .list_by_company(company_id)
        .await
        .expect("list");

    let enriched: Vec<EnrichedPipelineRow> =
        enrich_pipelines_with_aggregation(db.pool(), company_id, rows)
            .await
            .expect("enrich");

    let a_row = enriched.iter().find(|p| p.id == p_a).expect("p_a in result");
    // Pipeline A's root has 1 in_progress descendant case (a_child) -> count=1
    assert_eq!(a_row.descendant_active_work_count, 1, "p_a has 1 in_progress descendant");
    // Pipeline A is parent of the cross-pipeline child in p_b -> downstream includes p_b
    assert!(a_row.connections.downstream_pipeline_ids.contains(&p_b));
    // Pipeline A is not a child in any cross-pipeline edge -> upstream empty
    assert!(a_row.connections.upstream_pipeline_ids.is_empty());

    let b_row = enriched.iter().find(|p| p.id == p_b).expect("p_b in result");
    // p_b has no active work of its own (the cross-child has no links)
    assert_eq!(b_row.descendant_active_work_count, 0);
    // p_b is child in the cross edge -> upstream includes p_a
    assert!(b_row.connections.upstream_pipeline_ids.contains(&p_a));
    assert!(b_row.connections.downstream_pipeline_ids.is_empty());

    // All connections arrays must be sorted + deduped (defensive invariant)
    for row in &enriched {
        let mut up_sorted = row.connections.upstream_pipeline_ids.clone();
        up_sorted.sort();
        assert_eq!(up_sorted, row.connections.upstream_pipeline_ids, "upstream sorted");
        let mut down_sorted = row.connections.downstream_pipeline_ids.clone();
        down_sorted.sort();
        assert_eq!(down_sorted, row.connections.downstream_pipeline_ids, "downstream sorted");
    }

    cleanup_r63925(&db).await;
}

#[tokio::test]
async fn r63926_enrich_pipelines_isolated_by_company() {
    let db = connect().await;
    cleanup_r63925(&db).await;
    let (c1, _p1, s1) = r63925_fixture(&db, "iso-c1").await;
    let (c2, _p2, s2) = r63925_fixture(&db, "iso-c2").await;

    // c1 has a cross-pipeline edge
    let p_a = insert_pipeline_via_repo(&db, c1, "iso-pa").await;
    let p_b = insert_pipeline_via_repo(&db, c1, "iso-pb").await;
    let pa_root = insert_case(&db, c1, p_a, s1, "ISO-PA-R").await;
    let _pb_child = insert_child_case(&db, c1, p_b, s1, pa_root, None).await;

    // c2 has its own pipeline but should not see c1's connections
    let _c2_pipe = insert_pipeline_via_repo(&db, c2, "iso-c2p").await;

    let rows_c1 = pc_repos::pipeline::PipelineRepo::new(&db)
        .list_by_company(c1)
        .await
        .expect("list c1");
    let enriched_c1 = enrich_pipelines_with_aggregation(db.pool(), c1, rows_c1)
        .await
        .expect("enrich c1");
    let a_in_c1 = enriched_c1.iter().find(|p| p.id == p_a).expect("p_a in c1");
    assert!(a_in_c1.connections.downstream_pipeline_ids.contains(&p_b));

    let rows_c2 = pc_repos::pipeline::PipelineRepo::new(&db)
        .list_by_company(c2)
        .await
        .expect("list c2");
    let enriched_c2 = enrich_pipelines_with_aggregation(db.pool(), c2, rows_c2)
        .await
        .expect("enrich c2");
    for row in &enriched_c2 {
        assert!(row.connections.upstream_pipeline_ids.is_empty());
        assert!(row.connections.downstream_pipeline_ids.is_empty());
    }

    cleanup_r63925(&db).await;
}


// ============================================================================
// R639.2.8: case-level aggregation enrichment integration tests
//   enrich_cases_with_aggregation (activeWork + descendantActiveWorkCount)
// ============================================================================

#[tokio::test]
async fn r63928_enrich_cases_empty_input_returns_empty() {
    let db = connect().await;
    let enriched = enrich_cases_with_aggregation(db.pool(), Uuid::new_v4(), vec![])
        .await
        .expect("empty");
    assert!(enriched.is_empty());
}

#[tokio::test]
async fn r63928_enrich_cases_assigns_default_none_and_zero_when_no_data() {
    let db = connect().await;
    cleanup_r63925(&db).await;
    let (company_id, pipeline_id, stage_id) = r63925_fixture(&db, "no-enrich").await;

    // case without any issue link + no children -> activeWork=None, count=0
    let case_id = insert_case(&db, company_id, pipeline_id, stage_id, "LONELY").await;

    let rows = pc_repos::pipeline::PipelineRepo::new(&db)
        .list_cases(pipeline_id, None)
        .await
        .expect("list cases");
    let our = rows.iter().find(|c| c.id == case_id).expect("our case").clone();
    assert_eq!(rows.len(), 1);

    let enriched = enrich_cases_with_aggregation(db.pool(), company_id, rows)
        .await
        .expect("enrich");
    let row = enriched.first().expect("one row");
    assert_eq!(row.id, case_id);
    assert!(row.active_work.is_none());
    assert_eq!(row.descendant_active_work_count, 0);

    cleanup_r63925(&db).await;
}

#[tokio::test]
async fn r63928_enrich_cases_populates_active_work_and_descendant_count() {
    let db = connect().await;
    cleanup_r63925(&db).await;
    let (company_id, pipeline_id, stage_id) = r63925_fixture(&db, "enrich-c").await;
    let agent = insert_agent(&db, company_id).await;

    // case A: has its own in_progress work link -> activeWork present
    let case_a = insert_case(&db, company_id, pipeline_id, stage_id, "A").await;
    let issue_a = insert_issue(&db, company_id, Some(agent), "in_progress").await;
    insert_case_link(&db, company_id, case_a, issue_a, "work").await;

    // case B: no own link, but has child case B-child with in_progress work
    let case_b = insert_case(&db, company_id, pipeline_id, stage_id, "B").await;
    let child_b = insert_child_case(&db, company_id, pipeline_id, stage_id, case_b, None).await;
    let issue_b = insert_issue(&db, company_id, Some(agent), "in_progress").await;
    insert_case_link(&db, company_id, child_b, issue_b, "automation").await;

    // case C: only done work -> activeWork None, count 0
    let case_c = insert_case(&db, company_id, pipeline_id, stage_id, "C").await;
    let issue_done = insert_issue(&db, company_id, Some(agent), "done").await;
    insert_case_link(&db, company_id, case_c, issue_done, "work").await;

    let rows = pc_repos::pipeline::PipelineRepo::new(&db)
        .list_cases(pipeline_id, None)
        .await
        .expect("list cases");
    assert!(rows.iter().any(|c| c.id == case_a));
    assert!(rows.iter().any(|c| c.id == case_b));
    assert!(rows.iter().any(|c| c.id == case_c));

    let enriched: Vec<EnrichedCaseRow> =
        enrich_cases_with_aggregation(db.pool(), company_id, rows)
            .await
            .expect("enrich");

    let a = enriched.iter().find(|r| r.id == case_a).expect("a in result");
    let aw = a.active_work.as_ref().expect("a must have active_work");
    assert_eq!(aw.issue_id, issue_a);
    assert_eq!(aw.status, "in_progress");
    assert_eq!(a.descendant_active_work_count, 0);

    let b = enriched.iter().find(|r| r.id == case_b).expect("b in result");
    assert!(b.active_work.is_none(), "b has no own active work");
    assert_eq!(b.descendant_active_work_count, 1, "b has 1 descendant with active work");

    let c = enriched.iter().find(|r| r.id == case_c).expect("c in result");
    assert!(c.active_work.is_none(), "c has done work only");
    assert_eq!(c.descendant_active_work_count, 0);

    cleanup_r63925(&db).await;
}

#[tokio::test]
async fn r63928_enrich_cases_isolated_by_company() {
    let db = connect().await;
    cleanup_r63925(&db).await;
    let (c1, p1, s1) = r63925_fixture(&db, "iso-1").await;
    let (c2, p2, s2) = r63925_fixture(&db, "iso-2").await;
    let agent = insert_agent(&db, c1).await;

    // c1: case with in_progress work
    let c1_case = insert_case(&db, c1, p1, s1, "C1").await;
    let c1_issue = insert_issue(&db, c1, Some(agent), "in_progress").await;
    insert_case_link(&db, c1, c1_case, c1_issue, "work").await;

    // c2: own case (no cross-pollination)
    let _c2_case = insert_case(&db, c2, p2, s2, "C2").await;

    let c1_rows = pc_repos::pipeline::PipelineRepo::new(&db)
        .list_cases(p1, None)
        .await
        .expect("list c1");
    let c1_enriched = enrich_cases_with_aggregation(db.pool(), c1, c1_rows)
        .await
        .expect("enrich c1");
    let c1_row = c1_enriched.first().expect("c1 row");
    assert!(c1_row.active_work.is_some());

    let c2_rows = pc_repos::pipeline::PipelineRepo::new(&db)
        .list_cases(p2, None)
        .await
        .expect("list c2");
    let c2_enriched = enrich_cases_with_aggregation(db.pool(), c2, c2_rows)
        .await
        .expect("enrich c2");
    let c2_row = c2_enriched.first().expect("c2 row");
    assert!(c2_row.active_work.is_none());
    assert_eq!(c2_row.descendant_active_work_count, 0);

    cleanup_r63925(&db).await;
}
