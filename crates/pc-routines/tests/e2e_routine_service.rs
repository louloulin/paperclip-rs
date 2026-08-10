//! R605: `pc-routines` 业务层 e2e 测试。
//!
//! 验证：
//! - `RoutineService` 构造（new / with_hooks / add_hook）
//! - `create` 业务校验 + hook 触发 + 初始 revision 创建
//! - `get` / `list_by_company` / `list_all` / `get_detail` 读取路径
//! - `update` 乐观锁（base_revision_id 冲突）/ 状态变更 dispatch
//! - `delete` hook Archived 触发
//! - `create_trigger` / `list_triggers` / `update_trigger` / `delete_trigger` + 校验
//! - `list_runs` / `list_revisions` / `restore_revision`
//! - 跨 routine / 跨 company 隔离
//!
//! 数据库：复用现有 `paperclip_repos` Postgres 实例（不引入新 schema）。

use std::sync::Arc;

use pc_repos::Db;
use pc_routines::{
    CreateRoutine, CreateRoutineTrigger, RoutineHookEvent, RoutinePatch, RoutineService,
    UpdateRoutineTrigger,
};
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
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at)          VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("R605-{id}"))
    .bind(format!("A6{}", &id.simple().to_string()[..5]))
    .execute(pool)
    .await
    .expect("insert company");
    id
}

/// 清理 routine + 所有关联子表 + company
async fn cleanup(pool: &PgPool, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM routine_runs WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM routine_triggers WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    // routine_documents 关联的 documents / document_revisions
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

#[tokio::test(flavor = "current_thread")]
async fn r605_service_constructors_and_recorder() {
    let (db, _pool) = setup_db().await;

    // new + add_hook builds without panic
    let _svc = RoutineService::new(db.clone()).add_hook(Arc::new(
        pc_routines::RecordingRoutineHook::default(),
    ));

    // with_hooks accepts an empty vec
    let _svc2 = RoutineService::with_hooks(db, vec![]);

    // NoopRoutineHook constructs
    let _noop: Arc<dyn pc_routines::RoutineHook> = Arc::new(pc_routines::NoopRoutineHook);
}

#[tokio::test(flavor = "current_thread")]
async fn r605_create_persists_routine_and_dispatches_created() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let recorder = Arc::new(pc_routines::RecordingRoutineHook::default());
    let svc = RoutineService::new(db.clone()).add_hook(recorder.clone());

    let row = svc
        .create(CreateRoutine {
            company_id,
            title: "Daily standup".into(),
            description: Some("Run daily at 9am".into()),
            created_by_user_id: Some("user-1".into()),
            ..Default::default()
        })
        .await
        .expect("create");

    assert_eq!(row.company_id, company_id);
    assert_eq!(row.title, "Daily standup");
    assert_eq!(row.status, "active");
    assert_eq!(row.priority, "medium");
    assert_eq!(row.responsible_user_id.as_deref(), Some("user-1"));
    assert!(row.latest_revision_id.is_some(), "should have initial revision");

    let events = recorder.events_snapshot();
    assert_eq!(events.len(), 1);
    match &events[0] {
        RoutineHookEvent::Created { id, title, status, .. } => {
            assert_eq!(*id, row.id);
            assert_eq!(title, "Daily standup");
            assert_eq!(status, "active");
        }
        other => panic!("expected Created, got {other:?}"),
    }

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r605_create_rejects_empty_title() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = RoutineService::new(db);
    let err = svc
        .create(CreateRoutine {
            company_id,
            title: "   ".into(),
            created_by_user_id: Some("user-x".into()),
            ..Default::default()
        })
        .await
        .expect_err("empty title should fail");
    assert!(
        matches!(err, pc_errors::Error::Validation { .. }),
        "got: {err:?}"
    );

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r605_create_rejects_nil_company_id() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = RoutineService::new(db);
    let err = svc
        .create(CreateRoutine {
            company_id: Uuid::nil(),
            title: "X".into(),
            ..Default::default()
        })
        .await
        .expect_err("nil company_id should fail");
    assert!(matches!(err, pc_errors::Error::Validation { .. }));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r605_create_rejects_bad_priority() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = RoutineService::new(db);
    let err = svc
        .create(CreateRoutine {
            company_id,
            title: "X".into(),
            priority: Some("catastrophic".into()),
            created_by_user_id: Some("u".into()),
            ..Default::default()
        })
        .await
        .expect_err("bad priority should fail");
    assert!(matches!(err, pc_errors::Error::Validation { .. }));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r605_create_requires_responsible_user() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = RoutineService::new(db);
    let err = svc
        .create(CreateRoutine {
            company_id,
            title: "X".into(),
            ..Default::default()
        })
        .await
        .expect_err("missing responsible user should fail");
    assert!(matches!(err, pc_errors::Error::Unprocessable { .. }));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r605_get_returns_some_and_none() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = RoutineService::new(db.clone());
    let row = svc
        .create(CreateRoutine {
            company_id,
            title: "R1".into(),
            created_by_user_id: Some("u".into()),
            ..Default::default()
        })
        .await
        .expect("create");

    let fetched = svc.get(row.id).await.expect("get");
    assert!(fetched.is_some());
    assert_eq!(fetched.unwrap().id, row.id);

    let missing = svc.get(Uuid::new_v4()).await.expect("get missing");
    assert!(missing.is_none(), "unknown id returns None");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r605_list_by_company_filters_scope() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let co_a = insert_company(&pool).await;
    let co_b = insert_company(&pool).await;

    let svc = RoutineService::new(db.clone());
    for title in ["a1", "a2"] {
        svc.create(CreateRoutine {
            company_id: co_a,
            title: title.into(),
            created_by_user_id: Some("u".into()),
            ..Default::default()
        })
        .await
        .expect("create a");
    }
    svc.create(CreateRoutine {
        company_id: co_b,
        title: "b1".into(),
        created_by_user_id: Some("u".into()),
        ..Default::default()
    })
    .await
    .expect("create b");

    let a_rows = svc.list_by_company(co_a, None).await.expect("list a");
    let b_rows = svc.list_by_company(co_b, None).await.expect("list b");
    assert_eq!(a_rows.len(), 2, "company a only sees 2");
    assert_eq!(b_rows.len(), 1, "company b only sees 1");
    for r in &a_rows {
        assert_eq!(r.company_id, co_a);
    }
    for r in &b_rows {
        assert_eq!(r.company_id, co_b);
    }

    cleanup(&pool, co_a).await;
    cleanup(&pool, co_b).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r605_get_detail_aggregates() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = RoutineService::new(db);
    let row = svc
        .create(CreateRoutine {
            company_id,
            title: "Detail test".into(),
            description: Some("with description".into()),
            created_by_user_id: Some("u".into()),
            ..Default::default()
        })
        .await
        .expect("create");

    let detail = svc.get_detail(row.id).await.expect("detail").expect("some");
    assert_eq!(detail.routine.id, row.id);
    assert!(detail.description_document.is_some(), "should have description document");
    assert_eq!(detail.triggers.len(), 0, "fresh routine has no triggers");
    assert_eq!(detail.recent_runs.len(), 0, "fresh routine has no runs");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r605_update_title_only_dispatches_updated() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let recorder = Arc::new(pc_routines::RecordingRoutineHook::default());
    let svc = RoutineService::new(db.clone()).add_hook(recorder.clone());
    let row = svc
        .create(CreateRoutine {
            company_id,
            title: "before".into(),
            created_by_user_id: Some("u".into()),
            ..Default::default()
        })
        .await
        .expect("create");

    recorder.clear();
    let updated = svc
        .update(
            row.id,
            RoutinePatch {
                title: Some("after".into()),
                updated_by_user_id: Some("u".into()),
                ..Default::default()
            },
        )
        .await
        .expect("update");
    let updated = updated.expect("some");
    assert_eq!(updated.title, "after");
    assert_eq!(updated.latest_revision_number, 2, "should create revision 2");

    let events = recorder.events_snapshot();
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], RoutineHookEvent::Updated { .. }));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r605_update_to_archived_dispatches_archived() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let recorder = Arc::new(pc_routines::RecordingRoutineHook::default());
    let svc = RoutineService::new(db.clone()).add_hook(recorder.clone());
    let row = svc
        .create(CreateRoutine {
            company_id,
            title: "to-archive".into(),
            created_by_user_id: Some("u".into()),
            ..Default::default()
        })
        .await
        .expect("create");

    recorder.clear();
    svc.update(
        row.id,
        RoutinePatch {
            status: Some("archived".into()),
            ..Default::default()
        },
    )
    .await
    .expect("update");

    let events = recorder.events_snapshot();
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], RoutineHookEvent::Archived { .. }));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r605_update_rejects_bad_priority() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = RoutineService::new(db.clone());
    let row = svc
        .create(CreateRoutine {
            company_id,
            title: "X".into(),
            created_by_user_id: Some("u".into()),
            ..Default::default()
        })
        .await
        .expect("create");

    let err = svc
        .update(
            row.id,
            RoutinePatch {
                priority: Some("nuclear".into()),
                ..Default::default()
            },
        )
        .await
        .expect_err("bad priority");
    assert!(matches!(err, pc_errors::Error::Validation { .. }));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r605_update_base_revision_mismatch_returns_conflict() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = RoutineService::new(db.clone());
    let row = svc
        .create(CreateRoutine {
            company_id,
            title: "X".into(),
            created_by_user_id: Some("u".into()),
            ..Default::default()
        })
        .await
        .expect("create");

    let wrong_revision = Uuid::new_v4();
    let err = svc
        .update(
            row.id,
            RoutinePatch {
                title: Some("y".into()),
                base_revision_id: Some(wrong_revision),
                ..Default::default()
            },
        )
        .await
        .expect_err("base revision mismatch");
    assert!(matches!(err, pc_errors::Error::Conflict { .. }));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r605_delete_dispatches_archived_and_returns_true() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let recorder = Arc::new(pc_routines::RecordingRoutineHook::default());
    let svc = RoutineService::new(db.clone()).add_hook(recorder.clone());
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
    let removed = svc.delete(row.id).await.expect("delete");
    assert!(removed, "should return true");

    let events = recorder.events_snapshot();
    assert_eq!(events.len(), 1);
    match &events[0] {
        RoutineHookEvent::Archived { id, company_id: cid } => {
            assert_eq!(*id, row.id);
            assert_eq!(*cid, company_id);
        }
        other => panic!("expected Archived, got {other:?}"),
    }

    let after = svc.get(row.id).await.expect("get after");
    assert!(after.is_none(), "routine should be gone");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r605_delete_nonexistent_returns_false() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = RoutineService::new(db);
    let removed = svc.delete(Uuid::new_v4()).await.expect("delete missing");
    assert!(!removed);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r605_trigger_lifecycle_create_list_update_delete() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let recorder = Arc::new(pc_routines::RecordingRoutineHook::default());
    let svc = RoutineService::new(db.clone()).add_hook(recorder.clone());
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

    // create schedule trigger
    let result = svc
        .create_trigger(
            row.id,
            CreateRoutineTrigger {
                kind: "schedule".into(),
                label: Some("Daily".into()),
                cron_expression: Some("0 9 * * *".into()),
                timezone: Some("UTC".into()),
                ..Default::default()
            },
        )
        .await
        .expect("create trigger");
    let trigger_id = result.trigger.id;
    assert_eq!(result.trigger.kind, "schedule");

    // list
    let triggers = svc.list_triggers(row.id).await.expect("list");
    assert_eq!(triggers.len(), 1);
    assert_eq!(triggers[0].id, trigger_id);

    // get
    let fetched = svc.get_trigger(trigger_id).await.expect("get").expect("some");
    assert_eq!(fetched.label.as_deref(), Some("Daily"));

    // update
    let upd = svc
        .update_trigger(
            trigger_id,
            UpdateRoutineTrigger {
                label: Some(Some("Weekly".into())),
                ..Default::default()
            },
        )
        .await
        .expect("update")
        .expect("some");
    assert_eq!(upd.trigger.label.as_deref(), Some("Weekly"));

    // delete
    let del = svc.delete_trigger(trigger_id).await.expect("delete");
    assert!(del.is_some(), "returns revision row");

    let after = svc.list_triggers(row.id).await.expect("list after");
    assert_eq!(after.len(), 0);

    let events = recorder.events_snapshot();
    assert_eq!(events.len(), 3, "create + update + delete");
    assert!(matches!(events[0], RoutineHookEvent::TriggerCreated { .. }));
    assert!(matches!(events[1], RoutineHookEvent::TriggerUpdated { .. }));
    assert!(matches!(events[2], RoutineHookEvent::TriggerDeleted { .. }));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r605_trigger_schedule_requires_cron() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = RoutineService::new(db.clone());
    let row = svc
        .create(CreateRoutine {
            company_id,
            title: "T".into(),
            created_by_user_id: Some("u".into()),
            ..Default::default()
        })
        .await
        .expect("create");

    let err = svc
        .create_trigger(
            row.id,
            CreateRoutineTrigger {
                kind: "schedule".into(),
                cron_expression: None,
                ..Default::default()
            },
        )
        .await
        .expect_err("missing cron");
    assert!(matches!(err, pc_errors::Error::Unprocessable { .. }));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r605_trigger_webhook_rejects_cron() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = RoutineService::new(db.clone());
    let row = svc
        .create(CreateRoutine {
            company_id,
            title: "T".into(),
            created_by_user_id: Some("u".into()),
            ..Default::default()
        })
        .await
        .expect("create");

    let err = svc
        .create_trigger(
            row.id,
            CreateRoutineTrigger {
                kind: "webhook".into(),
                cron_expression: Some("0 9 * * *".into()),
                ..Default::default()
            },
        )
        .await
        .expect_err("webhook with cron");
    assert!(matches!(err, pc_errors::Error::Unprocessable { .. }));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r605_trigger_unknown_kind_rejected() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = RoutineService::new(db.clone());
    let row = svc
        .create(CreateRoutine {
            company_id,
            title: "T".into(),
            created_by_user_id: Some("u".into()),
            ..Default::default()
        })
        .await
        .expect("create");

    let err = svc
        .create_trigger(
            row.id,
            CreateRoutineTrigger {
                kind: "carrier-pigeon".into(),
                ..Default::default()
            },
        )
        .await
        .expect_err("unknown kind");
    assert!(matches!(err, pc_errors::Error::Unprocessable { .. }));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r605_list_runs_empty_after_create() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = RoutineService::new(db.clone());
    let row = svc
        .create(CreateRoutine {
            company_id,
            title: "T".into(),
            created_by_user_id: Some("u".into()),
            ..Default::default()
        })
        .await
        .expect("create");

    let runs = svc.list_runs(row.id, 50).await.expect("list runs");
    assert!(runs.is_empty());

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r605_list_revisions_returns_initial() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = RoutineService::new(db.clone());
    let row = svc
        .create(CreateRoutine {
            company_id,
            title: "T".into(),
            description: Some("d".into()),
            created_by_user_id: Some("u".into()),
            ..Default::default()
        })
        .await
        .expect("create");

    let revs = svc.list_revisions(row.id).await.expect("list revs");
    assert_eq!(revs.len(), 1, "create produces revision 1");
    assert_eq!(revs[0].revision_number, 1);
    assert_eq!(revs[0].title.as_str(), "T");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r605_restore_revision_creates_new_revision() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = RoutineService::new(db.clone());
    let row = svc
        .create(CreateRoutine {
            company_id,
            title: "v1-title".into(),
            created_by_user_id: Some("u".into()),
            ..Default::default()
        })
        .await
        .expect("create");
    let rev1_id = row.latest_revision_id.expect("rev1");

    svc.update(
        row.id,
        RoutinePatch {
            title: Some("v2-title".into()),
            ..Default::default()
        },
    )
    .await
    .expect("update v2");

    let restored = svc
        .restore_revision(row.id, rev1_id)
        .await
        .expect("restore")
        .expect("some");
    assert_eq!(restored.restored_from_revision_number, 1);
    assert_eq!(restored.routine.title, "v1-title", "restored to rev1 title");

    let revs = svc.list_revisions(row.id).await.expect("list revs");
    assert_eq!(revs.len(), 3, "create + update + restore");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r605_restore_missing_revision_returns_none() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = RoutineService::new(db.clone());
    let row = svc
        .create(CreateRoutine {
            company_id,
            title: "X".into(),
            created_by_user_id: Some("u".into()),
            ..Default::default()
        })
        .await
        .expect("create");

    let restored = svc
        .restore_revision(row.id, Uuid::new_v4())
        .await
        .expect("restore");
    assert!(restored.is_none(), "unknown revision id returns None");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r605_update_rejects_empty_title() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = RoutineService::new(db.clone());
    let row = svc
        .create(CreateRoutine {
            company_id,
            title: "X".into(),
            created_by_user_id: Some("u".into()),
            ..Default::default()
        })
        .await
        .expect("create");

    let err = svc
        .update(
            row.id,
            RoutinePatch {
                title: Some("   ".into()),
                ..Default::default()
            },
        )
        .await
        .expect_err("empty title in patch");
    assert!(matches!(err, pc_errors::Error::Validation { .. }));

    cleanup(&pool, company_id).await;
}
