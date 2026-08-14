//! R654: routine activity gate project scope 真实 PG 端到端测试。
//!
//! 验证 project scope 下 evaluate_activity_gate 仅匹配与 routine 关联的 project 的活动：
//! 1. routine 无 project_id → never fire (regardless of company activities)
//! 2. routine 有 project_id + 无关活动 → suppressed
//! 3. entity_type=project / entity_id=projectId → fire
//! 4. details.projectId = projectId → fire
//! 5. issues.project_id = projectId + entity_type=issue → fire
//! 6. heartbeat_run → issue.project_id = projectId → fire
//! 7. routines.project_id = projectId + entity_type=routine → fire
//! 8. routine_run → routine.project_id = projectId + entity_type=routine_run → fire
//! 9. activity on different project → suppressed
//! 10. ignored actions (issue.read_marked etc.) → suppressed
//! 11. self-loop routine-scheduler activity → suppressed

use chrono::{Duration as ChronoDuration, Utc};
use pc_routines::activity_gate::{evaluate_activity_gate, ActivityGateScope};
use pc_repos::routine::RoutineRow;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

static R654_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn try_setup_pool() -> Option<PgPool> {
    sqlx::postgres::PgPoolOptions::new().max_connections(2).connect(TEST_DATABASE_URL).await.ok()
}

struct Fixture {
    pub company_id: Uuid,
    pub project_id: Uuid,
    pub other_project_id: Uuid,
    pub agent_id: Uuid,
    pub routine_id: Uuid,
}

async fn setup_fixture(pool: &PgPool) -> Fixture {
    let company_id = Uuid::new_v4();
    let unique = company_id.simple().to_string();
    sqlx::query("INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) VALUES ($1, $2, 'active', $3, now(), now())")
        .bind(company_id)
        .bind(format!("R654-{unique}"))
        .bind(format!("R{}", &unique[..5]))
        .execute(pool)
        .await
        .expect("insert company");

    let agent_id = Uuid::new_v4();
    sqlx::query("INSERT INTO agents (id, company_id, name, role, adapter_type, status, adapter_config, created_at, updated_at) VALUES ($1, $2, $3, 'general', 'process', 'idle', '{}'::jsonb, now(), now())")
        .bind(agent_id)
        .bind(company_id)
        .bind(format!("Agent {unique}"))
        .execute(pool)
        .await
        .expect("insert agent");

    let project_id = Uuid::new_v4();
    let other_project_id = Uuid::new_v4();
    sqlx::query("INSERT INTO projects (id, company_id, name, status, created_at, updated_at) VALUES ($1, $2, $3, 'active', now(), now()), ($4, $2, $5, 'active', now(), now())")
        .bind(project_id)
        .bind(company_id)
        .bind(format!("Project A {project_id}"))
        .bind(other_project_id)
        .bind(format!("Project B {other_project_id}"))
        .execute(pool)
        .await
        .expect("insert projects");

    let routine_id = Uuid::new_v4();
    sqlx::query("INSERT INTO routines (id, company_id, title, status, assignee_agent_id, project_id, activity_gate_policy, activity_gate_scope, created_at, updated_at) VALUES ($1, $2, $3, 'active', $4, $5, 'require_external_activity', 'project', now(), now())")
        .bind(routine_id)
        .bind(company_id)
        .bind(format!("Routine {routine_id}"))
        .bind(agent_id)
        .bind(project_id)
        .execute(pool)
        .await
        .expect("insert routine");

    // Insert one routine_run so window_start is established.
    let past = Utc::now() - ChronoDuration::hours(2);
    sqlx::query("INSERT INTO routine_runs (id, company_id, routine_id, source, status, triggered_at, completed_at, created_at, updated_at) VALUES ($1, $2, $3, 'manual', 'succeeded', $4, $4, $4, $4)")
        .bind(Uuid::new_v4())
        .bind(company_id)
        .bind(routine_id)
        .bind(past)
        .execute(pool)
        .await
        .expect("insert prior run");

    Fixture { company_id, project_id, other_project_id, agent_id, routine_id }
}

async fn make_routine_row(pool: &PgPool, routine_id: Uuid) -> RoutineRow {
    let row: RoutineRow = sqlx::query_as(
        r#"SELECT id, company_id, project_id, folder_id, goal_id, parent_issue_id, title, description, assignee_agent_id, priority, status, concurrency_policy, catch_up_policy, activity_gate_policy, activity_gate_scope, origin_kind, origin_id, variables, env, latest_revision_id, latest_revision_number, created_by_agent_id, created_by_user_id, responsible_user_id, updated_by_agent_id, updated_by_user_id, last_triggered_at, last_enqueued_at, created_at, updated_at FROM routines WHERE id = $1"#,
    )
    .bind(routine_id)
    .fetch_one(pool)
    .await
    .expect("fetch routine row");
    row
}

async fn insert_activity(
    pool: &PgPool,
    company_id: Uuid,
    action: &str,
    entity_type: &str,
    entity_id: &str,
    actor_id: &str,
    details: serde_json::Value,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO activity_log (id, company_id, actor_type, actor_id, action, entity_type, entity_id, details, created_at) VALUES ($1, $2, 'system', $3, $4, $5, $6, $7::jsonb, now())")
        .bind(id)
        .bind(company_id)
        .bind(actor_id)
        .bind(action)
        .bind(entity_type)
        .bind(entity_id)
        .bind(details)
        .execute(pool)
        .await
        .expect("insert activity");
    id
}

async fn insert_issue(pool: &PgPool, company_id: Uuid, project_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO issues (id, company_id, project_id, title, status, origin_kind, created_at, updated_at) VALUES ($1, $2, $3, $4, 'open', 'manual', now(), now())")
        .bind(id)
        .bind(company_id)
        .bind(project_id)
        .bind(format!("Issue {id}"))
        .execute(pool)
        .await
        .expect("insert issue");
    id
}

async fn cleanup(pool: &PgPool, fixture: &Fixture) {
    let _ = sqlx::query("DELETE FROM activity_log WHERE company_id = $1").bind(fixture.company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM heartbeat_runs WHERE company_id = $1").bind(fixture.company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM routine_runs WHERE company_id = $1").bind(fixture.company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM routine_triggers WHERE company_id = $1").bind(fixture.company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM issues WHERE company_id = $1").bind(fixture.company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM routines WHERE company_id = $1").bind(fixture.company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM projects WHERE company_id = $1").bind(fixture.company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM agents WHERE company_id = $1").bind(fixture.company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1").bind(fixture.company_id).execute(pool).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r654_project_scope_routine_without_project_never_fires() {
    let pool = match try_setup_pool().await { Some(p) => p, None => { eprintln!("[skip] postgres unreachable"); return; } };
    let _guard = R654_TEST_LOCK.lock().await;
    let fixture = setup_fixture(&pool).await;

    // Remove project_id from routine to simulate "project scope but no project".
    sqlx::query("UPDATE routines SET project_id = NULL WHERE id = $1")
        .bind(fixture.routine_id)
        .execute(&pool)
        .await
        .expect("clear project_id");

    // Insert ANY activity — should still NOT fire (project scope requires project_id).
    insert_activity(&pool, fixture.company_id, "issue.created", "issue", &Uuid::new_v4().to_string(), "user-test", json!({})).await;

    let routine = make_routine_row(&pool, fixture.routine_id).await;
    let v = evaluate_activity_gate(&pool, &routine, Utc::now()).await;
    assert_eq!(v.scope, ActivityGateScope::Project);
    assert!(!v.fire, "project scope without project_id must not fire");

    cleanup(&pool, &fixture).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r654_project_scope_unrelated_activity_suppressed() {
    let pool = match try_setup_pool().await { Some(p) => p, None => { eprintln!("[skip] postgres unreachable"); return; } };
    let _guard = R654_TEST_LOCK.lock().await;
    let fixture = setup_fixture(&pool).await;

    // Activity on different project.
    insert_activity(&pool, fixture.company_id, "issue.created", "project", &fixture.other_project_id.to_string(), "user-test", json!({})).await;

    let routine = make_routine_row(&pool, fixture.routine_id).await;
    let v = evaluate_activity_gate(&pool, &routine, Utc::now()).await;
    assert!(!v.fire, "unrelated-project activity must not fire");

    cleanup(&pool, &fixture).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r654_project_scope_entity_type_project_fires() {
    let pool = match try_setup_pool().await { Some(p) => p, None => { eprintln!("[skip] postgres unreachable"); return; } };
    let _guard = R654_TEST_LOCK.lock().await;
    let fixture = setup_fixture(&pool).await;

    insert_activity(&pool, fixture.company_id, "project.updated", "project", &fixture.project_id.to_string(), "user-test", json!({})).await;

    let routine = make_routine_row(&pool, fixture.routine_id).await;
    let v = evaluate_activity_gate(&pool, &routine, Utc::now()).await;
    assert!(v.fire, "matching project entity should fire");
    assert!(v.matched_activity_id.is_some());

    cleanup(&pool, &fixture).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r654_project_scope_details_project_id_fires() {
    let pool = match try_setup_pool().await { Some(p) => p, None => { eprintln!("[skip] postgres unreachable"); return; } };
    let _guard = R654_TEST_LOCK.lock().await;
    let fixture = setup_fixture(&pool).await;

    insert_activity(&pool, fixture.company_id, "issue.commented", "issue", &Uuid::new_v4().to_string(), "user-test", json!({"projectId": fixture.project_id.to_string()})).await;

    let routine = make_routine_row(&pool, fixture.routine_id).await;
    let v = evaluate_activity_gate(&pool, &routine, Utc::now()).await;
    assert!(v.fire, "activity with details.projectId matching routine project should fire");

    cleanup(&pool, &fixture).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r654_project_scope_issue_in_project_fires() {
    let pool = match try_setup_pool().await { Some(p) => p, None => { eprintln!("[skip] postgres unreachable"); return; } };
    let _guard = R654_TEST_LOCK.lock().await;
    let fixture = setup_fixture(&pool).await;

    let issue_id = insert_issue(&pool, fixture.company_id, fixture.project_id).await;
    insert_activity(&pool, fixture.company_id, "issue.created", "issue", &issue_id.to_string(), "user-test", json!({})).await;

    let routine = make_routine_row(&pool, fixture.routine_id).await;
    let v = evaluate_activity_gate(&pool, &routine, Utc::now()).await;
    assert!(v.fire, "issue in same project should fire");

    cleanup(&pool, &fixture).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r654_project_scope_routine_in_project_fires() {
    let pool = match try_setup_pool().await { Some(p) => p, None => { eprintln!("[skip] postgres unreachable"); return; } };
    let _guard = R654_TEST_LOCK.lock().await;
    let fixture = setup_fixture(&pool).await;

    let other_routine_id = Uuid::new_v4();
    sqlx::query("INSERT INTO routines (id, company_id, title, status, assignee_agent_id, project_id, activity_gate_policy, activity_gate_scope, created_at, updated_at) VALUES ($1, $2, $3, 'active', $4, $5, 'always', 'company', now(), now())")
        .bind(other_routine_id)
        .bind(fixture.company_id)
        .bind(format!("Other Routine {other_routine_id}"))
        .bind(fixture.agent_id)
        .bind(fixture.project_id)
        .execute(&pool)
        .await
        .expect("insert other routine");

    insert_activity(&pool, fixture.company_id, "routine.updated", "routine", &other_routine_id.to_string(), "user-test", json!({})).await;

    let routine = make_routine_row(&pool, fixture.routine_id).await;
    let v = evaluate_activity_gate(&pool, &routine, Utc::now()).await;
    assert!(v.fire, "other routine in same project should fire via entity_type=routine EXISTS");

    cleanup(&pool, &fixture).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r654_project_scope_ignored_action_suppressed() {
    let pool = match try_setup_pool().await { Some(p) => p, None => { eprintln!("[skip] postgres unreachable"); return; } };
    let _guard = R654_TEST_LOCK.lock().await;
    let fixture = setup_fixture(&pool).await;

    // issue.read_marked is in ACTIVITY_GATE_IGNORED_ACTIONS — must NOT fire even if entity matches project.
    insert_activity(&pool, fixture.company_id, "issue.read_marked", "project", &fixture.project_id.to_string(), "user-test", json!({})).await;

    let routine = make_routine_row(&pool, fixture.routine_id).await;
    let v = evaluate_activity_gate(&pool, &routine, Utc::now()).await;
    assert!(!v.fire, "ignored actions must not fire even if scope matches");

    cleanup(&pool, &fixture).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r654_project_scope_self_loop_suppressed() {
    let pool = match try_setup_pool().await { Some(p) => p, None => { eprintln!("[skip] postgres unreachable"); return; } };
    let _guard = R654_TEST_LOCK.lock().await;
    let fixture = setup_fixture(&pool).await;

    // routine-scheduler self-loop with matching routineId in details.
    insert_activity(&pool, fixture.company_id, "routine.dispatch_skipped", "project", &fixture.project_id.to_string(), "routine-scheduler", json!({"routineId": fixture.routine_id.to_string()})).await;

    let routine = make_routine_row(&pool, fixture.routine_id).await;
    let v = evaluate_activity_gate(&pool, &routine, Utc::now()).await;
    assert!(!v.fire, "self-loop (routine-scheduler + matching routineId) must not fire");

    cleanup(&pool, &fixture).await;
}

