//! R736: e2e for `pc-work-products` against real Postgres.

use pc_repos::Db;
use pc_work_products::import_write_types::ImportIssueWorkProductRow;
use pc_work_products::{
    import_row_to_create_input, CreateWorkProductInput, UpdateWorkProductPatch, WorkProductService,
};
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

async fn insert_company(pool: &PgPool, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    let suffix = Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .take(6)
        .collect::<String>();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("R736-{tag}-{id}"))
    .bind(format!("R736{tag}-{suffix}"))
    .execute(pool)
    .await
    .expect("insert company");
    id
}

async fn insert_agent(pool: &PgPool, company_id: Uuid, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents \
            (id, company_id, name, role, status, adapter_type, adapter_config, \
             runtime_config, permissions, budget_monthly_cents, spent_monthly_cents, created_at, updated_at) \
         VALUES ($1, $2, $3, 'engineer', 'active', 'codex_local', '{}'::jsonb, '{}'::jsonb, '{}'::jsonb, 0, 0, now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("R736 agent {tag}"))
    .execute(pool)
    .await
    .expect("insert agent");
    id
}

async fn insert_issue(pool: &PgPool, company_id: Uuid, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    let identifier = format!(
        "R736-{}-{}",
        tag,
        Uuid::new_v4()
            .simple()
            .to_string()
            .chars()
            .take(6)
            .collect::<String>()
    );
    sqlx::query(
        "INSERT INTO issues (id, company_id, identifier, title, status, priority, request_depth, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, 'backlog', 'medium', 0, now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(identifier)
    .bind(format!("R736 issue {tag}"))
    .execute(pool)
    .await
    .expect("insert issue");
    id
}

async fn cleanup(pool: &PgPool, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM issue_work_products WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM issues WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM agents WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn create_and_get() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "cg").await;
    let _agent = insert_agent(&pool, company_id, "cg").await;
    let issue_id = insert_issue(&pool, company_id, "cg").await;
    let svc = WorkProductService::new(&db);

    let created = svc
        .create_for_issue(
            issue_id,
            company_id,
            CreateWorkProductInput {
                kind: "pr".to_string(),
                provider: "github".to_string(),
                external_id: Some("ext-1".to_string()),
                title: "PR #1".to_string(),
                url: Some("https://github.com/foo/bar/pull/1".to_string()),
                status: "open".to_string(),
                review_state: Some("pending".to_string()),
                is_primary: true,
                health_status: Some("ok".to_string()),
                summary: Some("summary".to_string()),
                metadata: Some(json!({"foo": "bar"})),
                source_trust: Some(json!({"preset": "standard"})),
                ..Default::default()
            },
        )
        .await
        .expect("create")
        .expect("some");

    assert_eq!(created.kind, "pr");
    assert_eq!(created.provider, "github");
    assert!(created.is_primary);
    assert_eq!(created.review_state, "pending");

    let got = svc.get_by_id(created.id).await.expect("get").expect("some");
    assert_eq!(got.title, "PR #1");

    let listed = svc.list_for_issue(issue_id).await.expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, created.id);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn create_primary_clears_other_primary_same_type() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "cp").await;
    let _agent = insert_agent(&pool, company_id, "cp").await;
    let issue_id = insert_issue(&pool, company_id, "cp").await;
    let svc = WorkProductService::new(&db);

    let first = svc
        .create_for_issue(
            issue_id,
            company_id,
            CreateWorkProductInput {
                kind: "pr".to_string(),
                provider: "github".to_string(),
                title: "first".to_string(),
                status: "open".to_string(),
                is_primary: true,
                ..Default::default()
            },
        )
        .await
        .expect("create1")
        .expect("some");

    let second = svc
        .create_for_issue(
            issue_id,
            company_id,
            CreateWorkProductInput {
                kind: "pr".to_string(),
                provider: "github".to_string(),
                title: "second".to_string(),
                status: "open".to_string(),
                is_primary: true,
                ..Default::default()
            },
        )
        .await
        .expect("create2")
        .expect("some");

    // first 不再是 primary
    let first_reloaded = svc.get_by_id(first.id).await.expect("get").expect("some");
    assert!(!first_reloaded.is_primary);
    assert!(second.is_primary);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn primary_does_not_clear_other_types() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "dt").await;
    let _agent = insert_agent(&pool, company_id, "dt").await;
    let issue_id = insert_issue(&pool, company_id, "dt").await;
    let svc = WorkProductService::new(&db);

    let pr_primary = svc
        .create_for_issue(
            issue_id,
            company_id,
            CreateWorkProductInput {
                kind: "pr".to_string(),
                provider: "github".to_string(),
                title: "pr".to_string(),
                status: "open".to_string(),
                is_primary: true,
                ..Default::default()
            },
        )
        .await
        .expect("pr")
        .expect("some");

    let _doc_primary = svc
        .create_for_issue(
            issue_id,
            company_id,
            CreateWorkProductInput {
                kind: "doc".to_string(),
                provider: "gdrive".to_string(),
                title: "doc".to_string(),
                status: "active".to_string(),
                is_primary: true,
                ..Default::default()
            },
        )
        .await
        .expect("doc")
        .expect("some");

    // pr 不应该被 doc 的 primary 创建清除
    let pr = svc
        .get_by_id(pr_primary.id)
        .await
        .expect("get")
        .expect("some");
    assert!(pr.is_primary);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn update_partial_fields() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "up").await;
    let _agent = insert_agent(&pool, company_id, "up").await;
    let issue_id = insert_issue(&pool, company_id, "up").await;
    let svc = WorkProductService::new(&db);

    let created = svc
        .create_for_issue(
            issue_id,
            company_id,
            CreateWorkProductInput {
                kind: "pr".to_string(),
                provider: "github".to_string(),
                title: "title".to_string(),
                status: "open".to_string(),
                ..Default::default()
            },
        )
        .await
        .expect("create")
        .expect("some");

    let updated = svc
        .update(
            created.id,
            UpdateWorkProductPatch {
                status: Some("merged".to_string()),
                review_state: Some("approved".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("update")
        .expect("some");

    assert_eq!(updated.status, "merged");
    assert_eq!(updated.review_state, "approved");
    assert_eq!(updated.title, "title"); // 未改

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn update_setting_primary_clears_other_primary() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "up2").await;
    let _agent = insert_agent(&pool, company_id, "up2").await;
    let issue_id = insert_issue(&pool, company_id, "up2").await;
    let svc = WorkProductService::new(&db);

    let first = svc
        .create_for_issue(
            issue_id,
            company_id,
            CreateWorkProductInput {
                kind: "pr".to_string(),
                provider: "github".to_string(),
                title: "first".to_string(),
                status: "open".to_string(),
                is_primary: true,
                ..Default::default()
            },
        )
        .await
        .expect("create1")
        .expect("some");

    let second = svc
        .create_for_issue(
            issue_id,
            company_id,
            CreateWorkProductInput {
                kind: "pr".to_string(),
                provider: "github".to_string(),
                title: "second".to_string(),
                status: "open".to_string(),
                is_primary: false,
                ..Default::default()
            },
        )
        .await
        .expect("create2")
        .expect("some");

    // 通过 update 把 second 设为 primary → first 应被清掉
    let _ = svc
        .update(
            second.id,
            UpdateWorkProductPatch {
                is_primary: Some(true),
                ..Default::default()
            },
        )
        .await
        .expect("update")
        .expect("some");
    let first_reloaded = svc.get_by_id(first.id).await.expect("get").expect("some");
    assert!(!first_reloaded.is_primary);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn remove_returns_row_and_deletes() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "rm").await;
    let _agent = insert_agent(&pool, company_id, "rm").await;
    let issue_id = insert_issue(&pool, company_id, "rm").await;
    let svc = WorkProductService::new(&db);

    let created = svc
        .create_for_issue(
            issue_id,
            company_id,
            CreateWorkProductInput {
                kind: "pr".to_string(),
                provider: "github".to_string(),
                title: "x".to_string(),
                status: "open".to_string(),
                ..Default::default()
            },
        )
        .await
        .expect("create")
        .expect("some");

    let removed = svc.remove(created.id).await.expect("remove").expect("some");
    assert_eq!(removed.id, created.id);

    let gone = svc.get_by_id(created.id).await.expect("get");
    assert!(gone.is_none());

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn remove_nonexistent_returns_none() {
    let _guard = TEST_LOCK.lock().await;
    let (db, _pool) = setup_db().await;
    let svc = WorkProductService::new(&db);
    let result = svc.remove(Uuid::new_v4()).await.expect("remove");
    assert!(result.is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn create_many_for_import_sets_last_primary_per_group() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "imp").await;
    let _agent = insert_agent(&pool, company_id, "imp").await;
    let issue_id = insert_issue(&pool, company_id, "imp").await;
    let svc = WorkProductService::new(&db);

    let row1 = ImportIssueWorkProductRow {
        company_id,
        issue_id,
        project_id: None,
        kind: "pr".to_string(),
        provider: "github".to_string(),
        external_id: Some("ext-1".to_string()),
        title: "first".to_string(),
        url: None,
        status: "open".to_string(),
        review_state: "pending".to_string(),
        is_primary: true, // 第一个 primary
        health_status: "ok".to_string(),
        summary: None,
        metadata: None,
        execution_workspace_id: None,
        runtime_service_id: None,
        created_by_run_id: None,
        source_trust: None,
    };
    let row2 = ImportIssueWorkProductRow {
        is_primary: true, // 第二个 primary (last wins)
        title: "second".to_string(),
        ..row1.clone()
    };
    let row3 = ImportIssueWorkProductRow {
        kind: "doc".to_string(),
        provider: "gdrive".to_string(),
        is_primary: false,
        title: "doc".to_string(),
        status: "active".to_string(),
        ..row1.clone()
    };
    svc.create_many_for_import(vec![row1, row2, row3])
        .await
        .expect("import");

    let listed = svc.list_for_issue(issue_id).await.expect("list");
    assert_eq!(listed.len(), 3);
    // pr group: row2 是 primary, row1 不是
    let prs: Vec<_> = listed.iter().filter(|w| w.kind == "pr").collect();
    assert_eq!(prs.iter().filter(|w| w.is_primary).count(), 1);
    assert_eq!(prs.iter().find(|w| w.is_primary).unwrap().title, "second");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn create_many_for_import_empty_is_noop() {
    let _guard = TEST_LOCK.lock().await;
    let (db, _pool) = setup_db().await;
    let svc = WorkProductService::new(&db);
    svc.create_many_for_import(vec![])
        .await
        .expect("empty import");
}

#[test]
fn import_row_to_create_input_round_trip() {
    use chrono::Utc;
    let now = Utc::now();
    let row = ImportIssueWorkProductRow {
        company_id: Uuid::nil(),
        issue_id: Uuid::nil(),
        project_id: None,
        kind: "pr".to_string(),
        provider: "github".to_string(),
        external_id: Some("ext".to_string()),
        title: "t".to_string(),
        url: Some("https://x".to_string()),
        status: "open".to_string(),
        review_state: "pending".to_string(),
        is_primary: true,
        health_status: "ok".to_string(),
        summary: Some("s".to_string()),
        metadata: Some(json!({"k": "v"})),
        execution_workspace_id: None,
        runtime_service_id: None,
        created_by_run_id: None,
        source_trust: None,
    };
    let _ = now;
    let create = import_row_to_create_input(&row);
    assert_eq!(create.kind, "pr");
    assert_eq!(create.review_state.as_deref(), Some("pending"));
    assert!(create.is_primary);
    assert_eq!(create.health_status.as_deref(), Some("ok"));
}
