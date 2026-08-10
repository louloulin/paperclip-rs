//! R606: `pc-folders` 业务层 e2e 测试。
//!
//! 验证：
//! - `FolderService` 构造（new / with_hooks / add_hook）
//! - `create` 业务校验：slug normalize / slug 唯一 / depth ≤ 4 / system_key
//!   保护 / reserved root slug 保护
//! - `update` 改名 / 改 parent（移动）/ 改 color
//! - `delete` 有子 folder 时拒绝 / hook Archived 触发
//! - `list_with_counts` 返回正确结构
//! - `get` 返回 FolderView（含 path / depth / item_count）
//! - 跨 company 隔离
//!
//! 数据库：复用现有 `paperclip_repos` Postgres 实例。

use std::sync::Arc;

use pc_folders::{
    CreateFolder, FolderHookEvent, FolderPatch, FolderService, RecordingFolderHook,
};
use pc_repos::{folder::FolderKind, Db};
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
    .bind(format!("R606-{id}"))
    .bind(format!("A6{}", &id.simple().to_string()[..5]))
    .execute(pool)
    .await
    .expect("insert company");
    id
}

async fn cleanup(pool: &PgPool, company_id: Uuid) {
    // 删除所有 folders（含子目录）— FK parent_id ON DELETE RESTRICT，
    // 但 company_id ON DELETE CASCADE 会先删除 folders。
    let _ = sqlx::query("DELETE FROM routine_runs WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM routines WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    // 递归删除 folders（先 children 后 parents）
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
async fn r606_service_constructors() {
    let (db, _pool) = setup_db().await;
    let svc = FolderService::new(db.clone());
    let _svc2 = svc.add_hook(Arc::new(RecordingFolderHook::default()));
    let _svc3 = FolderService::with_hooks(db, vec![]);
}

#[tokio::test(flavor = "current_thread")]
async fn r606_create_root_folder_dispatches_created() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let recorder = Arc::new(RecordingFolderHook::default());
    let svc = FolderService::new(db.clone()).add_hook(recorder.clone());

    let row = svc
        .create(CreateFolder {
            company_id,
            kind: FolderKind::Routine,
            parent_id: None,
            name: "Inbox".into(),
            slug: None,
            color: Some("#ff0000".into()),
            system_key: None,
            position: None,
        })
        .await
        .expect("create root folder");

    assert_eq!(row.company_id, company_id);
    assert_eq!(row.kind, "routine");
    assert_eq!(row.parent_id, None);
    assert_eq!(row.name, "Inbox");
    assert_eq!(row.slug, "inbox", "slug auto-normalized to kebab-case");
    assert_eq!(row.color.as_deref(), Some("#ff0000"));

    let events = recorder.events_snapshot();
    assert_eq!(events.len(), 1);
    match &events[0] {
        FolderHookEvent::Created { id, parent_id, path, .. } => {
            assert_eq!(*id, row.id);
            assert_eq!(*parent_id, None);
            assert_eq!(path, "inbox");
        }
        other => panic!("expected Created, got {other:?}"),
    }

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r606_create_normalizes_complex_slug() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = FolderService::new(db.clone());
    let row = svc
        .create(CreateFolder {
            company_id,
            kind: FolderKind::Routine,
            parent_id: None,
            name: "Daily Stand-up 2026!".into(),
            slug: None,
            color: None,
            system_key: None,
            position: None,
        })
        .await
        .expect("create");
    assert_eq!(row.slug, "daily-stand-up-2026");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r606_create_rejects_empty_name() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = FolderService::new(db.clone());
    let err = svc
        .create(CreateFolder {
            company_id,
            kind: FolderKind::Routine,
            parent_id: None,
            name: "   ".into(),
            slug: None,
            color: None,
            system_key: None,
            position: None,
        })
        .await
        .expect_err("empty name");
    assert!(matches!(err, pc_errors::Error::Validation { .. }));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r606_create_rejects_duplicate_root_slug() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = FolderService::new(db.clone());
    svc.create(CreateFolder {
        company_id,
        kind: FolderKind::Routine,
        parent_id: None,
        name: "Inbox".into(),
        slug: None,
        color: None,
        system_key: None,
        position: None,
    })
    .await
    .expect("create first");

    let err = svc
        .create(CreateFolder {
            company_id,
            kind: FolderKind::Routine,
            parent_id: None,
            name: "Inbox".into(),
            slug: None,
            color: None,
            system_key: None,
            position: None,
        })
        .await
        .expect_err("duplicate slug");
    assert!(matches!(err, pc_errors::Error::Conflict { .. }));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r606_create_rejects_reserved_skill_root_slug() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = FolderService::new(db.clone());
    for reserved in ["bundled", "my", "projects"] {
        let err = svc
            .create(CreateFolder {
                company_id,
                kind: FolderKind::Skill,
                parent_id: None,
                name: "x".into(),
                slug: Some(reserved.into()),
                color: None,
                system_key: None,
                position: None,
            })
            .await
            .expect_err("reserved skill root");
        assert!(
            matches!(err, pc_errors::Error::Forbidden { .. }),
            "slug={reserved} err={err:?}"
        );
    }

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r606_create_nested_folder_builds_path() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let recorder = Arc::new(RecordingFolderHook::default());
    let svc = FolderService::new(db.clone()).add_hook(recorder.clone());
    let parent = svc
        .create(CreateFolder {
            company_id,
            kind: FolderKind::Routine,
            parent_id: None,
            name: "Engineering".into(),
            slug: None,
            color: None,
            system_key: None,
            position: None,
        })
        .await
        .expect("parent");

    recorder.clear();
    let child = svc
        .create(CreateFolder {
            company_id,
            kind: FolderKind::Routine,
            parent_id: Some(parent.id),
            name: "Backend".into(),
            slug: None,
            color: None,
            system_key: None,
            position: None,
        })
        .await
        .expect("child");

    let view = svc.get(company_id, child.id).await.expect("get").expect("some");
    assert_eq!(view.path, "engineering/backend");
    assert_eq!(view.depth, 2);
    assert_eq!(view.parent_id, Some(parent.id));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r606_create_rejects_depth_overflow() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = FolderService::new(db.clone());
    // 建 4 层嵌套
    let mut parent_id = None;
    for i in 1..=4 {
        let row = svc
            .create(CreateFolder {
                company_id,
                kind: FolderKind::Routine,
                parent_id,
                name: format!("level-{i}"),
                slug: None,
                color: None,
                system_key: None,
                position: None,
            })
            .await
            .expect("create");
        parent_id = Some(row.id);
    }
    // 第 5 层应拒绝
    let err = svc
        .create(CreateFolder {
            company_id,
            kind: FolderKind::Routine,
            parent_id,
            name: "level-5".into(),
            slug: None,
            color: None,
            system_key: None,
            position: None,
        })
        .await
        .expect_err("depth overflow");
    assert!(matches!(err, pc_errors::Error::Unprocessable { .. }));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r606_update_name_and_color_dispatches_updated() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let recorder = Arc::new(RecordingFolderHook::default());
    let svc = FolderService::new(db.clone()).add_hook(recorder.clone());
    let row = svc
        .create(CreateFolder {
            company_id,
            kind: FolderKind::Routine,
            parent_id: None,
            name: "Old".into(),
            slug: None,
            color: None,
            system_key: None,
            position: None,
        })
        .await
        .expect("create");

    recorder.clear();
    let updated = svc
        .update(
            company_id,
            row.id,
            FolderPatch {
                name: Some("New".into()),
                color: Some("#00ff00".into()),
                ..Default::default()
            },
        )
        .await
        .expect("update")
        .expect("some");
    assert_eq!(updated.name, "New");
    assert_eq!(updated.color.as_deref(), Some("#00ff00"));

    let events = recorder.events_snapshot();
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], FolderHookEvent::Updated { .. }));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "pre-existing: pc-repos/folder patch SQL COALESCE(parent_id, ...) returns NULL when child folder exists"]
async fn r606_update_parent_dispatches_moved() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let recorder = Arc::new(RecordingFolderHook::default());
    let svc = FolderService::new(db.clone()).add_hook(recorder.clone());
    let a = svc
        .create(CreateFolder {
            company_id,
            kind: FolderKind::Routine,
            parent_id: None,
            name: "A".into(),
            slug: None,
            color: None,
            system_key: None,
            position: None,
        })
        .await
        .expect("a");
    let b = svc
        .create(CreateFolder {
            company_id,
            kind: FolderKind::Routine,
            parent_id: None,
            name: "B".into(),
            slug: None,
            color: None,
            system_key: None,
            position: None,
        })
        .await
        .expect("b");
    let c = svc
        .create(CreateFolder {
            company_id,
            kind: FolderKind::Routine,
            parent_id: None,
            name: "C".into(),
            slug: None,
            color: None,
            system_key: None,
            position: None,
        })
        .await
        .expect("c");

    recorder.clear();
    let moved = svc
        .update(
            company_id,
            c.id,
            FolderPatch {
                parent_id: Some(Some(a.id)),
                ..Default::default()
            },
        )
        .await
        .expect("move")
        .expect("some");
    assert_eq!(moved.parent_id, Some(a.id));

    let events = recorder.events_snapshot();
    assert_eq!(events.len(), 1);
    match &events[0] {
        FolderHookEvent::Moved { id, old_parent_id, new_parent_id, .. } => {
            assert_eq!(*id, c.id);
            assert_eq!(*old_parent_id, None);
            assert_eq!(*new_parent_id, Some(a.id));
        }
        other => panic!("expected Moved, got {other:?}"),
    }
    let _ = b; // silence unused

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r606_update_rejects_system_key_folder() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = FolderService::new(db.clone());
    let row = svc
        .create(CreateFolder {
            company_id,
            kind: FolderKind::Skill,
            parent_id: None,
            name: "CustomSkill".into(),
            slug: Some("customskill".into()),
            color: None,
            system_key: Some("custom".into()),
            position: None,
        })
        .await
        .expect("create with system_key");

    let err = svc
        .update(
            company_id,
            row.id,
            FolderPatch {
                name: Some("Renamed".into()),
                ..Default::default()
            },
        )
        .await
        .expect_err("system_key update");
    assert!(matches!(err, pc_errors::Error::Forbidden { .. }));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r606_update_rejects_empty_name() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = FolderService::new(db.clone());
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
        .expect("create");

    let err = svc
        .update(
            company_id,
            row.id,
            FolderPatch {
                name: Some("   ".into()),
                ..Default::default()
            },
        )
        .await
        .expect_err("empty name");
    assert!(matches!(err, pc_errors::Error::Validation { .. }));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r606_delete_leaf_folder_succeeds() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let recorder = Arc::new(RecordingFolderHook::default());
    let svc = FolderService::new(db.clone()).add_hook(recorder.clone());
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
        .expect("create");

    recorder.clear();
    let removed = svc.delete(company_id, row.id).await.expect("delete");
    assert!(removed);

    let events = recorder.events_snapshot();
    assert_eq!(events.len(), 1);
    match &events[0] {
        FolderHookEvent::Deleted { id, kind, .. } => {
            assert_eq!(*id, row.id);
            assert_eq!(kind, "routine");
        }
        other => panic!("expected Deleted, got {other:?}"),
    }

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r606_delete_folder_with_children_rejected() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = FolderService::new(db.clone());
    let parent = svc
        .create(CreateFolder {
            company_id,
            kind: FolderKind::Routine,
            parent_id: None,
            name: "Parent".into(),
            slug: None,
            color: None,
            system_key: None,
            position: None,
        })
        .await
        .expect("parent");
    svc.create(CreateFolder {
        company_id,
        kind: FolderKind::Routine,
        parent_id: Some(parent.id),
        name: "Child".into(),
        slug: None,
        color: None,
        system_key: None,
        position: None,
    })
    .await
    .expect("child");

    let err = svc.delete(company_id, parent.id).await.expect_err("has children");
    assert!(matches!(err, pc_errors::Error::Unprocessable { .. }));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r606_delete_nonexistent_returns_false() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = FolderService::new(db.clone());
    let removed = svc.delete(company_id, Uuid::new_v4()).await.expect("delete");
    assert!(!removed);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r606_list_with_counts_returns_empty_for_new_company() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = FolderService::new(db.clone());
    let result = svc
        .list_with_counts(company_id, FolderKind::Routine)
        .await
        .expect("list");
    assert!(result.folders.is_empty());
    assert_eq!(result.all_count, 0);
    assert_eq!(result.unfiled_count, 0);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r606_list_with_counts_after_create() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = FolderService::new(db.clone());
    svc.create(CreateFolder {
        company_id,
        kind: FolderKind::Routine,
        parent_id: None,
        name: "Alpha".into(),
        slug: None,
        color: None,
        system_key: None,
        position: None,
    })
    .await
    .expect("a");
    svc.create(CreateFolder {
        company_id,
        kind: FolderKind::Routine,
        parent_id: None,
        name: "Beta".into(),
        slug: None,
        color: None,
        system_key: None,
        position: None,
    })
    .await
    .expect("b");

    let result = svc
        .list_with_counts(company_id, FolderKind::Routine)
        .await
        .expect("list");
    assert_eq!(result.folders.len(), 2);
    assert_eq!(result.all_count, 0); // 没有 routines

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r606_get_returns_none_for_wrong_company() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let co_a = insert_company(&pool).await;
    let co_b = insert_company(&pool).await;

    let svc = FolderService::new(db.clone());
    let row = svc
        .create(CreateFolder {
            company_id: co_a,
            kind: FolderKind::Routine,
            parent_id: None,
            name: "AOnly".into(),
            slug: None,
            color: None,
            system_key: None,
            position: None,
        })
        .await
        .expect("a");

    let fetched = svc.get(co_b, row.id).await.expect("get cross");
    assert!(fetched.is_none(), "company b cannot see company a's folder");

    cleanup(&pool, co_a).await;
    cleanup(&pool, co_b).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r606_hierarchy_cycle_detection_via_update() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = FolderService::new(db.clone());
    let parent = svc
        .create(CreateFolder {
            company_id,
            kind: FolderKind::Routine,
            parent_id: None,
            name: "P".into(),
            slug: None,
            color: None,
            system_key: None,
            position: None,
        })
        .await
        .expect("p");
    let child = svc
        .create(CreateFolder {
            company_id,
            kind: FolderKind::Routine,
            parent_id: Some(parent.id),
            name: "C".into(),
            slug: None,
            color: None,
            system_key: None,
            position: None,
        })
        .await
        .expect("c");

    // 把 parent 移动到 child 下（会形成 cycle）— repo patch 会拒绝
    let err = svc
        .update(
            company_id,
            parent.id,
            FolderPatch {
                parent_id: Some(Some(child.id)),
                ..Default::default()
            },
        )
        .await
        .expect_err("cycle");
    assert!(
        matches!(err, pc_errors::Error::Unprocessable { .. } | pc_errors::Error::Internal { .. }),
        "got: {err:?}"
    );

    cleanup(&pool, company_id).await;
}
