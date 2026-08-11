//! R613: pc-project e2e service tests (Postgres-backed).
//!
//! Validates:
//! - ProjectService construction + hook attachment
//! - create validates inputs and inserts row
//! - list_by_company / get / get_id_only
//! - patch emits StatusChanged when status changes
//! - pause / resume state transitions
//! - archive / delete
//! - upsert_membership / list_memberships
//! - create_workspace / set_primary_workspace / list_workspaces
//! - attach_goal / detach_goal / goals_for_project

use std::sync::Arc;

use pc_project::{
    MembershipState, NewProject, ProjectHookEvent, ProjectService, ProjectStatus,
    RecordingProjectHook,
};
use pc_repos::Db;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn setup_db() -> (Db, PgPool) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(TEST_DATABASE_URL)
        .await
        .expect("connect");
    let db = Db::connect(TEST_DATABASE_URL, 4, 1).await.expect("Db");
    (db, pool)
}

async fn insert_company(pool: &PgPool) -> Uuid {
    let id = Uuid::new_v4();
    let prefix = format!(
        "R{}",
        Uuid::new_v4()
            .simple()
            .to_string()
            .chars()
            .take(5)
            .collect::<String>()
    );
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at)          VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("R613ct-{id}"))
    .bind(prefix)
    .execute(pool)
    .await
    .expect("insert company");
    id
}

async fn insert_goal(pool: &PgPool, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO goals (id, company_id, title, level, status, created_at, updated_at)          VALUES ($1, $2, $3, 'project', 'active', now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("R613g-{id}"))
    .execute(pool)
    .await
    .expect("insert goal");
    id
}

async fn cleanup(pool: &PgPool, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM project_goals WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM project_workspaces WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM project_memberships WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM goals WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM projects WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM company_memberships WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
}

fn new_project(company_id: Uuid) -> NewProject {
    NewProject {
        company_id,
        goal_id: None,
        name: format!("R613-{}", Uuid::new_v4().simple()),
        description: Some("test".into()),
        status: ProjectStatus::Backlog,
        lead_agent_id: None,
        target_date: None,
        color: None,
        icon: None,
        env: None,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn service_constructs_with_new_and_with_hooks() {
    let _guard = TEST_LOCK.lock().await;
    let (db, _pool) = setup_db().await;
    let svc = ProjectService::new(db.clone());
    assert_eq!(svc.hook_count(), 0);
    let recorder = Arc::new(RecordingProjectHook::default());
    let svc2 = ProjectService::with_hooks(db, vec![recorder.clone()]);
    assert_eq!(svc2.hook_count(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn create_rejects_empty_name() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let svc = ProjectService::new(db);
    let mut input = new_project(company_id);
    input.name = "  ".into();
    let res = svc.create(input).await;
    assert!(res.is_err());
    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn create_emits_created_hook() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let recorder = Arc::new(RecordingProjectHook::default());
    let svc = ProjectService::with_hooks(db, vec![recorder.clone()]);

    let row = svc.create(new_project(company_id)).await.expect("create");
    assert_eq!(row.company_id, company_id);
    assert_eq!(row.status, "backlog");

    let events = recorder.events_snapshot();
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], ProjectHookEvent::Created { .. }));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn list_and_get_return_inserted_row() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let svc = ProjectService::new(db);

    let row = svc.create(new_project(company_id)).await.expect("create");
    let rows = svc.list_by_company(company_id, false).await.expect("list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, row.id);

    let got = svc
        .get(company_id, row.id)
        .await
        .expect("get")
        .expect("exists");
    assert_eq!(got.id, row.id);

    let id_only = svc
        .get_id_only(row.id)
        .await
        .expect("get_id_only")
        .expect("exists");
    assert_eq!(id_only.id, row.id);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn patch_with_status_change_emits_status_changed() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let recorder = Arc::new(RecordingProjectHook::default());
    let svc = ProjectService::with_hooks(db, vec![recorder.clone()]);

    let row = svc.create(new_project(company_id)).await.expect("create");
    recorder.clear();

    let patch = pc_project::ProjectPatch {
        status: Some(ProjectStatus::Active),
        ..Default::default()
    };
    let updated = svc
        .patch(company_id, row.id, patch)
        .await
        .expect("patch")
        .expect("row");
    assert_eq!(updated.status, "active");

    let events = recorder.events_snapshot();
    assert!(events.len() >= 2, "expected Patched + StatusChanged");
    let mut saw_patched = false;
    let mut saw_status = false;
    for e in &events {
        match e {
            ProjectHookEvent::Patched { .. } => saw_patched = true,
            ProjectHookEvent::StatusChanged {
                old_status,
                new_status,
                ..
            } => {
                assert_eq!(*old_status, Some(ProjectStatus::Backlog));
                assert_eq!(*new_status, ProjectStatus::Active);
                saw_status = true;
            }
            _ => {}
        }
    }
    assert!(saw_patched && saw_status);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn pause_and_resume_transition() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let svc = ProjectService::new(db);

    let row = svc
        .create({
            let mut p = new_project(company_id);
            p.status = ProjectStatus::Active;
            p
        })
        .await
        .expect("create");

    let paused = svc
        .pause(company_id, row.id, Some("down for maintenance"))
        .await
        .expect("pause")
        .expect("row");
    assert_eq!(paused.status, "paused");

    let resumed = svc
        .resume(company_id, row.id)
        .await
        .expect("resume")
        .expect("row");
    assert_eq!(resumed.status, "active");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn archive_then_delete_emits_both_hooks() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let recorder = Arc::new(RecordingProjectHook::default());
    let svc = ProjectService::with_hooks(db, vec![recorder.clone()]);

    let row = svc.create(new_project(company_id)).await.expect("create");
    recorder.clear();

    svc.archive(company_id, row.id).await.expect("archive");
    svc.delete(company_id, row.id).await.expect("delete");

    let events = recorder.events_snapshot();
    let mut saw_archived = false;
    let mut saw_deleted = false;
    for e in &events {
        match e {
            ProjectHookEvent::Archived { .. } => saw_archived = true,
            ProjectHookEvent::Deleted { .. } => saw_deleted = true,
            _ => {}
        }
    }
    assert!(saw_archived && saw_deleted);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn membership_upsert_emits_hook() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let recorder = Arc::new(RecordingProjectHook::default());
    let svc = ProjectService::with_hooks(db, vec![recorder.clone()]);

    let row = svc.create(new_project(company_id)).await.expect("create");
    recorder.clear();

    svc.upsert_membership(company_id, row.id, "u1", MembershipState::Joined)
        .await
        .expect("upsert");
    let memberships = svc.list_memberships(row.id).await.expect("list");
    assert_eq!(memberships.len(), 1);
    assert_eq!(memberships[0].user_id, "u1");
    assert_eq!(memberships[0].state, "joined");

    let events = recorder.events_snapshot();
    assert_eq!(events.len(), 1);
    match &events[0] {
        ProjectHookEvent::MembershipUpserted { state, .. } => {
            assert_eq!(*state, MembershipState::Joined);
        }
        _ => panic!("expected MembershipUpserted"),
    }

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn workspace_create_and_set_primary() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let recorder = Arc::new(RecordingProjectHook::default());
    let svc = ProjectService::with_hooks(db, vec![recorder.clone()]);

    let row = svc.create(new_project(company_id)).await.expect("create");
    recorder.clear();

    let ws = svc
        .create_workspace(
            company_id,
            row.id,
            "local",
            None,
            Some("main"),
            Some("/tmp/ws"),
            true,
        )
        .await
        .expect("create_workspace");
    assert_eq!(ws.project_id, row.id);
    assert!(ws.is_primary);

    let workspaces = svc.list_workspaces(row.id).await.expect("list");
    assert_eq!(workspaces.len(), 1);

    let primary = svc.get_primary_workspace(row.id).await.expect("primary");
    assert!(primary.is_some());

    let ok = svc
        .set_primary_workspace(company_id, row.id, ws.id)
        .await
        .expect("set_primary");
    assert!(ok);

    let events = recorder.events_snapshot();
    assert!(events.len() >= 2, "WorkspaceCreated + WorkspaceSetPrimary");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn attach_and_detach_goal() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let goal_id = insert_goal(&pool, company_id).await;
    let svc = ProjectService::new(db);

    let row = svc.create(new_project(company_id)).await.expect("create");
    let ok = svc
        .attach_goal(company_id, row.id, goal_id)
        .await
        .expect("attach");
    assert!(ok);

    let goals = svc.goals_for_project(row.id).await.expect("goals");
    assert_eq!(goals.len(), 1);

    let ok = svc
        .detach_goal(company_id, row.id, goal_id)
        .await
        .expect("detach");
    assert!(ok);

    let goals = svc.goals_for_project(row.id).await.expect("goals");
    assert!(goals.is_empty());

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn create_simple_returns_backlog_project() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let svc = ProjectService::new(db);

    let row = svc
        .create_simple(company_id, "alpha", None)
        .await
        .expect("create_simple");
    assert_eq!(row.status, "backlog");
    assert_eq!(row.name, "alpha");

    cleanup(&pool, company_id).await;
}
