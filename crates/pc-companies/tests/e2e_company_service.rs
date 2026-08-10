//! R590: CompanyService 真实 DB 端到端测试。

use std::sync::Arc;

use pc_companies::{
    CompanyActor, CompanyLifecycleEvent, CompanyService, CreateCompanyInput, NoopCompanyHook,
    RecordingCompanyHook, UpdateCompanyPatch,
};
use pc_repos::Db;
use sqlx::PgPool;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn setup_pool() -> PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(TEST_DATABASE_URL)
        .await
        .expect("connect to postgres")
}

/// 每个测试用独立 DB 连接，但共享一个 `Db`。
async fn setup_db() -> (Db, PgPool) {
    let pool = setup_pool().await;
    let db = Db::connect(TEST_DATABASE_URL, 4, 1)
        .await
        .expect("connect Db");
    (db, pool)
}

/// 删掉测试遗留 company。
async fn cleanup_company(pool: &PgPool, id: Uuid) {
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn r590_company_service_create_emits_lifecycle_event() {
    let (db, pool) = setup_db().await;
    let hook = Arc::new(RecordingCompanyHook::default());
    let svc = CompanyService::with_hooks(&db, vec![hook.clone()]);

    let input = CreateCompanyInput {
        name: format!("R590-Create-{}", Uuid::new_v4()),
        description: Some("test".into()),
        owner_principal_id: "user-test-1".into(),
        budget_monthly_cents: None,
    };
    let row = svc.create(input.clone()).await.expect("create");

    assert_eq!(row.name, input.name);
    assert_eq!(row.status, "active");
    assert!(row.id != Uuid::nil());

    let events = hook.events.lock().expect("lock");
    assert_eq!(events.len(), 1);
    match &events[0] {
        CompanyLifecycleEvent::Created { id, owner_principal_id, .. } => {
            assert_eq!(*id, row.id);
            assert_eq!(owner_principal_id, "user-test-1");
        }
        other => panic!("expected Created event, got {other:?}"),
    }

    cleanup_company(&pool, row.id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r590_company_service_create_validates_name() {
    let (db, _) = setup_db().await;
    let svc = CompanyService::new(&db);

    let res = svc
        .create(CreateCompanyInput {
            name: "   ".into(),
            description: None,
            owner_principal_id: "u".into(),
            budget_monthly_cents: None,
        })
        .await;
    assert!(res.is_err(), "empty name must reject");
    match res.unwrap_err() {
        pc_companies::CompanyServiceError::InvalidInput(_) => {}
        other => panic!("expected InvalidInput, got {other:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn r590_company_service_get_returns_none_for_missing() {
    let (db, _) = setup_db().await;
    let svc = CompanyService::new(&db);

    let res = svc.get_by_id(Uuid::new_v4()).await.expect("get_by_id");
    assert!(res.is_none(), "missing id should return None");
}

#[tokio::test(flavor = "current_thread")]
async fn r590_company_service_update_partial_returns_row() {
    let (db, pool) = setup_db().await;
    let hook = Arc::new(RecordingCompanyHook::default());
    let svc = CompanyService::with_hooks(&db, vec![hook.clone()]);

    let created = svc
        .create(CreateCompanyInput {
            name: format!("R590-Update-{}", Uuid::new_v4()),
            description: Some("before".into()),
            owner_principal_id: "user-test-2".into(),
            budget_monthly_cents: None,
        })
        .await
        .expect("create");

    let updated = svc
        .update(
            created.id,
            UpdateCompanyPatch {
                description: Some("after".into()),
                ..Default::default()
            },
            &CompanyActor::system(),
        )
        .await
        .expect("update")
        .expect("row present");

    assert_eq!(updated.description.as_deref(), Some("after"));
    assert_eq!(updated.name, created.name, "name untouched");

    let events = hook.events.lock().expect("lock");
    assert!(events
        .iter()
        .any(|e| matches!(e, CompanyLifecycleEvent::Updated { id, .. } if *id == created.id)));

    cleanup_company(&pool, created.id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r590_company_service_update_rejects_invalid_status() {
    let (db, pool) = setup_db().await;
    let svc = CompanyService::new(&db);

    let created = svc
        .create(CreateCompanyInput {
            name: format!("R590-Status-{}", Uuid::new_v4()),
            description: None,
            owner_principal_id: "user-test-3".into(),
            budget_monthly_cents: None,
        })
        .await
        .expect("create");

    let res = svc
        .update(
            created.id,
            UpdateCompanyPatch {
                status: Some("bogus".into()),
                ..Default::default()
            },
            &CompanyActor::system(),
        )
        .await;
    assert!(matches!(
        res.unwrap_err(),
        pc_companies::CompanyServiceError::InvalidInput(_)
    ));

    cleanup_company(&pool, created.id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r590_company_service_update_missing_returns_none() {
    let (db, _) = setup_db().await;
    let svc = CompanyService::new(&db);

    let res = svc
        .update(
            Uuid::new_v4(),
            UpdateCompanyPatch {
                name: Some("anything".into()),
                ..Default::default()
            },
            &CompanyActor::system(),
        )
        .await
        .expect("update");
    assert!(res.is_none(), "missing id should return None");
}

#[tokio::test(flavor = "current_thread")]
async fn r590_company_service_archive_emits_event() {
    let (db, pool) = setup_db().await;
    let hook = Arc::new(RecordingCompanyHook::default());
    let svc = CompanyService::with_hooks(&db, vec![hook.clone()]);

    let created = svc
        .create(CreateCompanyInput {
            name: format!("R590-Archive-{}", Uuid::new_v4()),
            description: None,
            owner_principal_id: "user-test-4".into(),
            budget_monthly_cents: None,
        })
        .await
        .expect("create");

    let archived = svc
        .archive(created.id, &CompanyActor::system())
        .await
        .expect("archive")
        .expect("row present");
    assert_eq!(archived.status, "archived");

    let events = hook.events.lock().expect("lock");
    assert!(events
        .iter()
        .any(|e| matches!(e, CompanyLifecycleEvent::Archived { id, .. } if *id == created.id)));

    cleanup_company(&pool, created.id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r590_company_service_remove_emits_event() {
    let (db, pool) = setup_db().await;
    let hook = Arc::new(RecordingCompanyHook::default());
    let svc = CompanyService::with_hooks(&db, vec![hook.clone()]);

    let created = svc
        .create(CreateCompanyInput {
            name: format!("R590-Remove-{}", Uuid::new_v4()),
            description: None,
            owner_principal_id: "user-test-5".into(),
            budget_monthly_cents: None,
        })
        .await
        .expect("create");

    // FK constraint requires memberships cleanup before remove
    sqlx::query("DELETE FROM company_memberships WHERE company_id = $1")
        .bind(created.id)
        .execute(&pool)
        .await
        .expect("cleanup memberships");

    let ok = svc.remove(created.id).await.expect("remove");
    assert!(ok);

    let events = hook.events.lock().expect("lock");
    assert!(events
        .iter()
        .any(|e| matches!(e, CompanyLifecycleEvent::Removed { id, .. } if *id == created.id)));
}

#[tokio::test(flavor = "current_thread")]
async fn r590_company_service_list_includes_created() {
    let (db, pool) = setup_db().await;
    let svc = CompanyService::new(&db);

    let created = svc
        .create(CreateCompanyInput {
            name: format!("R590-List-{}", Uuid::new_v4()),
            description: None,
            owner_principal_id: "user-test-6".into(),
            budget_monthly_cents: None,
        })
        .await
        .expect("create");

    let rows = svc.list().await.expect("list");
    assert!(
        rows.iter().any(|r| r.id == created.id),
        "created company should appear in list"
    );

    cleanup_company(&pool, created.id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r590_company_service_with_noop_hook_does_not_panic() {
    let (db, pool) = setup_db().await;
    let svc = CompanyService::with_hooks(&db, vec![Arc::new(NoopCompanyHook)]);

    let created = svc
        .create(CreateCompanyInput {
            name: format!("R590-Noop-{}", Uuid::new_v4()),
            description: None,
            owner_principal_id: "user-test-7".into(),
            budget_monthly_cents: None,
        })
        .await
        .expect("create");

    cleanup_company(&pool, created.id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r590_company_service_stats_returns_row() {
    let (db, pool) = setup_db().await;
    let svc = CompanyService::new(&db);

    let created = svc
        .create(CreateCompanyInput {
            name: format!("R590-Stats-{}", Uuid::new_v4()),
            description: None,
            owner_principal_id: "user-test-8".into(),
            budget_monthly_cents: None,
        })
        .await
        .expect("create");

    let stats = svc.stats(created.id).await.expect("stats");
    assert_eq!(stats.company_id, created.id);

    cleanup_company(&pool, created.id).await;
}
