//! R788: `pc-documents` 集成测试 (使用 55433 devdb, 验证 pure split 后的服务层).
//!
//! 验证：
//! - DocumentService create/update/delete 走真实 PostgreSQL
//! - RecordingDocumentHook 收到完整 lifecycle 事件
//! - 错误的输入通过 pure:: 验证函数拒绝 (不接触 DB)
//! - 锁状态正确 (锁定后拒绝更新)
//! - 修订历史正确累积
//!
//! 与 R608 e2e_document_service.rs 区别: 本测试侧重验证 pure 模块提取后的
//! integration 行为 (非 5432 端口, 使用 55433 devdb).

use pc_documents::{
    CreateDocument, DocumentPatch, DocumentService, NoopDocumentHook, RecordingDocumentHook,
};
use pc_repos::Db;
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

// R788: 使用 devdb 55433 (与 paperclip-rs 开发环境一致)
const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:55433/paperclip_repos";

static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn setup_db() -> (Db, PgPool) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(TEST_DATABASE_URL)
        .await
        .expect("connect 55433");
    let db = Db::connect(TEST_DATABASE_URL, 4, 1).await.expect("Db");
    (db, pool)
}

async fn insert_company(pool: &PgPool) -> Uuid {
    let id = Uuid::new_v4();
    let prefix = format!("R{}", Uuid::new_v4().simple().to_string().chars().take(5).collect::<String>());
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("R788-{id}"))
    .bind(prefix)
    .execute(pool)
    .await
    .expect("insert company");
    id
}

async fn cleanup(pool: &PgPool, company_id: Uuid, document_id: Uuid) {
    let _ = sqlx::query("DELETE FROM document_revisions WHERE company_id = $1")
        .bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM documents WHERE company_id = $1")
        .bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id).execute(pool).await;
    let _ = (document_id,);
}

#[tokio::test]
async fn r788_create_document_persists_to_db() {
    let _lock = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let recorder = Arc::new(RecordingDocumentHook::default());
    let service = DocumentService::with_hooks(
        db.clone(),
        vec![recorder.clone() as Arc<dyn pc_documents::DocumentHook>],
    );

    let document_id = {
        let input = CreateDocument {
            company_id,
            title: Some("R788 Test Doc".to_string()),
            format: Some("markdown".to_string()),
            body: "Hello, world!".to_string(),
            created_by_agent_id: None,
            created_by_user_id: None,
        };
        let row = service.create(input).await.expect("create");
        assert_eq!(row.title.as_deref(), Some("R788 Test Doc"));
        assert_eq!(row.format, "markdown");
        row.id
    };

    // Verify hook fired Created event
    let events = recorder.events_snapshot();
    assert_eq!(events.len(), 1);
    match &events[0] {
        pc_documents::DocumentHookEvent::Created { id, company_id: cid, title, format } => {
            assert_eq!(*id, document_id);
            assert_eq!(*cid, company_id);
            assert_eq!(title.as_deref(), Some("R788 Test Doc"));
            assert_eq!(format, "markdown");
        }
        _ => panic!("expected Created event"),
    }

    // Verify it persists across queries
    let got = service.get(document_id).await.expect("get").expect("found");
    assert_eq!(got.id, document_id);
    assert_eq!(got.latest_revision_number, 1);

    cleanup(&pool, company_id, document_id).await;
    recorder.clear();
}

#[tokio::test]
async fn r788_update_document_creates_revision_and_fires_updated() {
    let _lock = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let recorder = Arc::new(RecordingDocumentHook::default());
    let service = DocumentService::with_hooks(
        db.clone(),
        vec![recorder.clone() as Arc<dyn pc_documents::DocumentHook>],
    );

    let document_id = {
        let input = CreateDocument {
            company_id,
            title: Some("v1".to_string()),
            format: None,
            body: "v1 body".to_string(),
            created_by_agent_id: None,
            created_by_user_id: None,
        };
        service.create(input).await.expect("create").id
    };
    recorder.clear();

    {
        let patch = DocumentPatch {
            title: Some("v2".to_string()),
            format: None,
            body: Some("v2 body, longer".to_string()),
            updated_by_agent_id: None,
            updated_by_user_id: None,
        };
        let row = service.update(company_id, document_id, patch).await.expect("update").expect("row");
        assert_eq!(row.title.as_deref(), Some("v2"));
        assert_eq!(row.latest_revision_number, 2);
    }

    let events = recorder.events_snapshot();
    assert_eq!(events.len(), 1);
    match &events[0] {
        pc_documents::DocumentHookEvent::Updated { id, latest_revision_number, .. } => {
            assert_eq!(*id, document_id);
            assert_eq!(*latest_revision_number, 2);
        }
        _ => panic!("expected Updated event"),
    }

    // Verify revision history
    let revisions = service.list_revisions(document_id).await.expect("list_revisions");
    assert_eq!(revisions.len(), 2);

    cleanup(&pool, company_id, document_id).await;
    recorder.clear();
}

#[tokio::test]
async fn r788_lock_blocks_update() {
    let _lock = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let recorder = Arc::new(RecordingDocumentHook::default());
    let service = DocumentService::with_hooks(
        db.clone(),
        vec![recorder.clone() as Arc<dyn pc_documents::DocumentHook>],
    );

    let document_id = {
        let input = CreateDocument {
            company_id,
            title: Some("to lock".to_string()),
            format: None,
            body: "locked body".to_string(),
            created_by_agent_id: None,
            created_by_user_id: None,
        };
        service.create(input).await.expect("create").id
    };
    recorder.clear();

    // Lock without actor (avoids FK on agents table)
    service.lock_document(company_id, document_id, None, None::<&str>)
        .await.expect("lock").expect("row");

    // Verify Locked event
    let events = recorder.events_snapshot();
    assert!(events.iter().any(|e| matches!(e, pc_documents::DocumentHookEvent::Locked { .. })));
    recorder.clear();

    // Try to update -> should fail
    let patch = DocumentPatch {
        body: Some("hack".to_string()),
        ..Default::default()
    };
    let result = service.update(company_id, document_id, patch).await;
    assert!(result.is_err(), "locked doc should reject update");

    // Unlock first
    service.unlock_document(company_id, document_id).await.expect("unlock").expect("row");
    recorder.clear();

    // Now update succeeds
    let patch = DocumentPatch {
        body: Some("unlocked body".to_string()),
        ..Default::default()
    };
    let row = service.update(company_id, document_id, patch).await.expect("update after unlock").expect("row");
    assert_eq!(row.latest_revision_number, 2);

    cleanup(&pool, company_id, document_id).await;
}

#[tokio::test]
async fn r788_pure_validation_rejects_bad_input_before_db() {
    let _lock = TEST_LOCK.lock().await;
    let (db, _pool) = setup_db().await;
    let service = DocumentService::new(db);

    // Empty body -> pure validation rejects (no DB write)
    let input = CreateDocument {
        company_id: Uuid::new_v4(),
        title: None,
        format: None,
        body: "".to_string(),
        created_by_agent_id: None,
        created_by_user_id: None,
    };
    let err = service.create(input).await.unwrap_err();
    assert!(err.to_string().contains("body must not be empty"));

    // Bad format
    let input = CreateDocument {
        company_id: Uuid::new_v4(),
        title: None,
        format: Some("xml".to_string()),
        body: "ok".to_string(),
        created_by_agent_id: None,
        created_by_user_id: None,
    };
    let err = service.create(input).await.unwrap_err();
    assert!(err.to_string().contains("format must be one of"));

    // Nil company_id
    let input = CreateDocument {
        company_id: Uuid::nil(),
        title: None,
        format: None,
        body: "ok".to_string(),
        created_by_agent_id: None,
        created_by_user_id: None,
    };
    let err = service.create(input).await.unwrap_err();
    assert!(err.to_string().contains("companyId"));
}

#[tokio::test]
async fn r788_noop_hook_does_not_interfere() {
    let _lock = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let service = DocumentService::with_hooks(
        db.clone(),
        vec![Arc::new(NoopDocumentHook) as Arc<dyn pc_documents::DocumentHook>],
    );

    let input = CreateDocument {
        company_id,
        title: Some("noop".to_string()),
        format: None,
        body: "fine".to_string(),
        created_by_agent_id: None,
        created_by_user_id: None,
    };
    let row = service.create(input).await.expect("create");
    assert_eq!(row.title.as_deref(), Some("noop"));

    cleanup(&pool, company_id, row.id).await;
}

