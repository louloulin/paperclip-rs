//! R606: `pc-folders` hook 系统 contract 测试。
//!
//! 验证 FolderHook trait 的语义契约：
//! - `NoopFolderHook` 不影响 service 行为
//! - `RecordingFolderHook` 记录所有 lifecycle 事件
//! - 多个 hook 同时注册时全部触发
//! - 失败的 hook 不阻塞 service
//! - recorder helper（events_snapshot / clear / len / is_empty）
//! - Created / Updated / Moved / Deleted event 序列化

use std::sync::Arc;

use async_trait::async_trait;
use pc_folders::{
    CreateFolder, FolderHook, FolderHookEvent, FolderPatch, FolderService, NoopFolderHook,
    RecordingFolderHook,
};
use pc_repos::{folder::FolderKind, Db};
use serde_json::Value;
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
        "F{}",
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
    .bind(format!("R606hk-{id}"))
    .bind(prefix)
    .execute(pool)
    .await
    .expect("insert company");
    id
}

async fn cleanup(pool: &PgPool, company_id: Uuid) {
    loop {
        let affected = sqlx::query(
            "DELETE FROM folders WHERE company_id=$1 AND              id NOT IN (SELECT parent_id FROM folders WHERE parent_id IS NOT NULL)",
        )
        .bind(company_id)
        .execute(pool)
        .await
        .map(|r| r.rows_affected())
        .unwrap_or(0);
        if affected == 0 {
            break;
        }
    }
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

struct FailingHook;
#[async_trait]
impl FolderHook for FailingHook {
    async fn on_folder_event(&self, _event: FolderHookEvent) -> pc_errors::Result<()> {
        Err(pc_errors::internal("hook always fails"))
    }
}

#[tokio::test(flavor = "current_thread")]
async fn hook_noop_does_not_affect_service() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = FolderService::new(db).add_hook(Arc::new(NoopFolderHook));
    let row = svc
        .create(CreateFolder {
            company_id,
            kind: FolderKind::Routine,
            parent_id: None,
            name: "X".into(),
            slug: None,
            color: None,
            system_key: None,
            position: None,
        })
        .await
        .expect("create with noop hook");
    assert_eq!(row.name, "X");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn hook_recorder_captures_create_event() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let recorder = Arc::new(RecordingFolderHook::default());
    let svc = FolderService::new(db).add_hook(recorder.clone());
    let row = svc
        .create(CreateFolder {
            company_id,
            kind: FolderKind::Routine,
            parent_id: None,
            name: "captured".into(),
            slug: None,
            color: None,
            system_key: None,
            position: None,
        })
        .await
        .expect("create");

    let events = recorder.events_snapshot();
    assert_eq!(events.len(), 1);
    match &events[0] {
        FolderHookEvent::Created { id, path, .. } => {
            assert_eq!(*id, row.id);
            assert_eq!(path, "captured");
        }
        _ => panic!("expected Created"),
    }

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn hook_recorder_helpers_work() {
    let recorder = RecordingFolderHook::default();
    assert!(recorder.is_empty());
    assert_eq!(recorder.len(), 0);

    recorder
        .on_folder_event(FolderHookEvent::Deleted {
            id: Uuid::nil(),
            company_id: Uuid::nil(),
            kind: "routine".into(),
        })
        .await
        .expect("hook");

    assert_eq!(recorder.len(), 1);
    assert!(!recorder.is_empty());

    recorder.clear();
    assert!(recorder.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn multiple_hooks_all_fire() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let r1 = Arc::new(RecordingFolderHook::default());
    let r2 = Arc::new(RecordingFolderHook::default());
    let r3 = Arc::new(RecordingFolderHook::default());
    let svc = FolderService::new(db)
        .add_hook(r1.clone())
        .add_hook(r2.clone())
        .add_hook(r3.clone());

    svc.create(CreateFolder {
        company_id,
        kind: FolderKind::Routine,
        parent_id: None,
        name: "multi".into(),
        slug: None,
        color: None,
        system_key: None,
        position: None,
    })
    .await
    .expect("create");

    assert_eq!(r1.len(), 1);
    assert_eq!(r2.len(), 1);
    assert_eq!(r3.len(), 1);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn failing_hook_does_not_block_other_hooks() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let failing: Arc<dyn FolderHook> = Arc::new(FailingHook);
    let recorder = Arc::new(RecordingFolderHook::default());

    let svc = FolderService::new(db)
        .add_hook(failing)
        .add_hook(recorder.clone());

    let row = svc
        .create(CreateFolder {
            company_id,
            kind: FolderKind::Routine,
            parent_id: None,
            name: "after-fail".into(),
            slug: None,
            color: None,
            system_key: None,
            position: None,
        })
        .await
        .expect("create despite failing hook");
    assert_eq!(row.name, "after-fail");

    let events = recorder.events_snapshot();
    assert_eq!(events.len(), 1, "hook after failing hook still fires");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn hook_receives_deleted_event() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let recorder = Arc::new(RecordingFolderHook::default());
    let svc = FolderService::new(db).add_hook(recorder.clone());

    let row = svc
        .create(CreateFolder {
            company_id,
            kind: FolderKind::Routine,
            parent_id: None,
            name: "to-delete".into(),
            slug: None,
            color: None,
            system_key: None,
            position: None,
        })
        .await
        .expect("create");

    recorder.clear();
    svc.delete(company_id, row.id).await.expect("delete");

    let events = recorder.events_snapshot();
    assert_eq!(events.len(), 1);
    match &events[0] {
        FolderHookEvent::Deleted { id, kind, .. } => {
            assert_eq!(*id, row.id);
            assert_eq!(kind, "routine");
        }
        _ => panic!("expected Deleted"),
    }

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn hook_events_serialize_for_realtime() {
    let event = FolderHookEvent::Created {
        id: Uuid::nil(),
        company_id: Uuid::nil(),
        kind: "routine".into(),
        parent_id: None,
        path: "inbox".into(),
    };
    let value: Value = serde_json::to_value(&event).expect("serialize Created");
    assert_eq!(value["type"], "created");
    assert_eq!(value["kind"], "routine");
    assert_eq!(value["path"], "inbox");
    assert_eq!(value["parentId"], Value::Null);

    let updated = FolderHookEvent::Updated {
        id: Uuid::nil(),
        company_id: Uuid::nil(),
        kind: "skill".into(),
        path: "skills/web".into(),
    };
    let updated_value: Value = serde_json::to_value(&updated).expect("serialize Updated");
    assert_eq!(updated_value["type"], "updated");

    let moved = FolderHookEvent::Moved {
        id: Uuid::nil(),
        company_id: Uuid::nil(),
        old_parent_id: Some(Uuid::nil()),
        new_parent_id: None,
    };
    let moved_value: Value = serde_json::to_value(&moved).expect("serialize Moved");
    assert_eq!(moved_value["type"], "moved");
    assert_eq!(moved_value["newParentId"], Value::Null);
}
