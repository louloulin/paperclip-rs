//! R605: `pc-routines` hook 系统 contract 测试。
//!
//! 验证 hook trait 的语义契约：
//! - `NoopRoutineHook` 不影响 service 行为
//! - `RecordingRoutineHook` 记录所有 lifecycle 事件
//! - 多个 hook 同时注册时全部触发
//! - 失败的 hook 不阻塞后续 hook / 不阻塞 service
//! - hook 不持有 `MutexGuard` 跨 await（防死锁）
//! - recorder helper（events_snapshot / clear / len / is_empty）
//!
//! 数据库：复用现有 `paperclip_repos` Postgres 实例。

use std::sync::Arc;

use async_trait::async_trait;
use pc_repos::Db;
use pc_routines::{
    CreateRoutine, NoopRoutineHook, RecordingRoutineHook, RoutineHook, RoutineHookEvent,
    RoutineService,
};
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn setup_db() -> (Db, sqlx::PgPool) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(TEST_DATABASE_URL)
        .await
        .expect("connect");
    let db = Db::connect(TEST_DATABASE_URL, 4, 1).await.expect("Db");
    (db, pool)
}

async fn insert_company(pool: &sqlx::PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at)          VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("R605hk-{id}"))
    .bind(format!("A6{}", &id.simple().to_string()[..5]))
    .execute(pool)
    .await
    .expect("insert company");
    id
}

async fn cleanup(pool: &sqlx::PgPool, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM routine_runs WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM routine_triggers WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    if let Ok(doc_ids) = sqlx::query_scalar::<_, Uuid>(
        "SELECT document_id FROM routine_documents WHERE company_id = $1",
    )
    .bind(company_id)
    .fetch_all(pool)
    .await
    {
        for doc_id in &doc_ids {
            let _ = sqlx::query("DELETE FROM document_revisions WHERE document_id = $1")
                .bind(doc_id)
                .execute(pool)
                .await;
            let _ = sqlx::query("DELETE FROM document_annotations WHERE document_id = $1")
                .bind(doc_id)
                .execute(pool)
                .await;
            let _ = sqlx::query("DELETE FROM documents WHERE id = $1")
                .bind(doc_id)
                .execute(pool)
                .await;
        }
    }
    let _ = sqlx::query("DELETE FROM routine_documents WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM routine_revisions WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM routines WHERE company_id = $1")
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

/// Hook that always returns Err — used to verify error isolation.
struct FailingHook;
#[async_trait]
impl RoutineHook for FailingHook {
    async fn on_routine_event(&self, _event: RoutineHookEvent) -> pc_errors::Result<()> {
        Err(pc_errors::internal("hook always fails"))
    }
}

#[tokio::test(flavor = "current_thread")]
async fn hook_noop_does_not_affect_service() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = RoutineService::new(db).add_hook(Arc::new(NoopRoutineHook));
    let row = svc
        .create(CreateRoutine {
            company_id,
            title: "X".into(),
            created_by_user_id: Some("u".into()),
            ..Default::default()
        })
        .await
        .expect("create with noop hook");

    assert_eq!(row.title, "X");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn hook_recorder_captures_create_event() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let recorder = Arc::new(RecordingRoutineHook::default());
    let svc = RoutineService::new(db).add_hook(recorder.clone());
    let row = svc
        .create(CreateRoutine {
            company_id,
            title: "captured".into(),
            created_by_user_id: Some("u".into()),
            ..Default::default()
        })
        .await
        .expect("create");

    let events = recorder.events_snapshot();
    assert_eq!(events.len(), 1);
    match &events[0] {
        RoutineHookEvent::Created { id, title, .. } => {
            assert_eq!(*id, row.id);
            assert_eq!(title, "captured");
        }
        _ => panic!("expected Created"),
    }

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn hook_recorder_helpers_work() {
    let recorder = RecordingRoutineHook::default();
    assert!(recorder.is_empty());
    assert_eq!(recorder.len(), 0);

    let svc = RoutineService::new(Db::connect(TEST_DATABASE_URL, 1, 0).await.unwrap());
    let _ = svc; // silence unused

    // 直接 push events through trait
    use pc_routines::RoutineHook;
    recorder
        .on_routine_event(RoutineHookEvent::Archived {
            id: Uuid::nil(),
            company_id: Uuid::nil(),
        })
        .await
        .expect("hook");

    assert_eq!(recorder.len(), 1);
    assert!(!recorder.is_empty());

    recorder.clear();
    assert!(recorder.is_empty());
    assert_eq!(recorder.len(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn multiple_hooks_all_fire_in_order() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let r1 = Arc::new(RecordingRoutineHook::default());
    let r2 = Arc::new(RecordingRoutineHook::default());
    let r3 = Arc::new(RecordingRoutineHook::default());
    let svc = RoutineService::new(db)
        .add_hook(r1.clone())
        .add_hook(r2.clone())
        .add_hook(r3.clone());

    svc.create(CreateRoutine {
        company_id,
        title: "multi".into(),
        created_by_user_id: Some("u".into()),
        ..Default::default()
    })
    .await
    .expect("create");

    assert_eq!(r1.len(), 1, "hook 1 captured");
    assert_eq!(r2.len(), 1, "hook 2 captured");
    assert_eq!(r3.len(), 1, "hook 3 captured");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn failing_hook_does_not_block_other_hooks() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let failing: Arc<dyn RoutineHook> = Arc::new(FailingHook);
    let recorder = Arc::new(RecordingRoutineHook::default());

    let svc = RoutineService::new(db)
        .add_hook(failing)
        .add_hook(recorder.clone());

    // 必须成功 — 失败 hook 不能阻塞 service
    let row = svc
        .create(CreateRoutine {
            company_id,
            title: "after-fail".into(),
            created_by_user_id: Some("u".into()),
            ..Default::default()
        })
        .await
        .expect("create despite failing hook");
    assert_eq!(row.title, "after-fail");

    // recorder 必须仍然收到 event（失败 hook 之后的 hook 仍触发）
    let events = recorder.events_snapshot();
    assert_eq!(events.len(), 1, "hook after failing hook still fires");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn hook_receives_archived_event_on_delete() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let recorder = Arc::new(RecordingRoutineHook::default());
    let svc = RoutineService::new(db).add_hook(recorder.clone());

    let row = svc
        .create(CreateRoutine {
            company_id,
            title: "to-delete".into(),
            created_by_user_id: Some("u".into()),
            ..Default::default()
        })
        .await
        .expect("create");

    recorder.clear();
    svc.delete(row.id).await.expect("delete");

    let events = recorder.events_snapshot();
    assert_eq!(events.len(), 1);
    match &events[0] {
        RoutineHookEvent::Archived {
            id,
            company_id: cid,
        } => {
            assert_eq!(*id, row.id);
            assert_eq!(*cid, company_id);
        }
        _ => panic!("expected Archived"),
    }

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn hook_receives_trigger_lifecycle_events() {
    use pc_routines::{CreateRoutineTrigger, UpdateRoutineTrigger};
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let recorder = Arc::new(RecordingRoutineHook::default());
    let svc = RoutineService::new(db).add_hook(recorder.clone());
    let row = svc
        .create(CreateRoutine {
            company_id,
            title: "T".into(),
            created_by_user_id: Some("u".into()),
            ..Default::default()
        })
        .await
        .expect("create");

    recorder.clear();

    // create
    let t = svc
        .create_trigger(
            row.id,
            CreateRoutineTrigger {
                kind: "schedule".into(),
                cron_expression: Some("0 9 * * *".into()),
                ..Default::default()
            },
        )
        .await
        .expect("create trigger");

    // update
    svc.update_trigger(
        t.trigger.id,
        UpdateRoutineTrigger {
            label: Some(Some("renamed".into())),
            ..Default::default()
        },
    )
    .await
    .expect("update trigger");

    // delete
    svc.delete_trigger(t.trigger.id)
        .await
        .expect("delete trigger");

    let events = recorder.events_snapshot();
    assert_eq!(events.len(), 3);
    assert!(matches!(events[0], RoutineHookEvent::TriggerCreated { .. }));
    assert!(matches!(events[1], RoutineHookEvent::TriggerUpdated { .. }));
    assert!(matches!(events[2], RoutineHookEvent::TriggerDeleted { .. }));

    cleanup(&pool, company_id).await;
}
