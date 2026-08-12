//! R639.1: pc-pipeline-case-outputs service DB glue 集成测试（真实 PG）。
//!
//! 验证 \`list_case_outputs\` 端到端：
//! - 来源 issues JOIN pipeline_case_issue_links
//! - documents JOIN issue_documents + document_revisions
//! - sort + preview + source_trust fallback

use pc_pipeline_case_outputs::{list_case_outputs, summarize_pipeline_case_outputs_for_context};
use pc_repos::Db;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn connect() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn cleanup(db: &Db) {
    // 按依赖顺序删除
    let _ = sqlx::query("DELETE FROM document_revisions WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'r639pct-%')")
        .execute(db.pool()).await;
    let _ = sqlx::query("DELETE FROM documents WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'r639pct-%')")
        .execute(db.pool()).await;
    let _ = sqlx::query("DELETE FROM issue_documents WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'r639pct-%')")
        .execute(db.pool()).await;
    let _ = sqlx::query("DELETE FROM pipeline_case_issue_links WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'r639pct-%')")
        .execute(db.pool()).await;
    let _ = sqlx::query("DELETE FROM pipeline_cases WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'r639pct-%')")
        .execute(db.pool()).await;
    let _ = sqlx::query("DELETE FROM pipeline_stages WHERE pipeline_id IN (SELECT id FROM pipelines WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'r639pct-%'))")
        .execute(db.pool()).await;
    let _ = sqlx::query("DELETE FROM pipelines WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'r639pct-%')")
        .execute(db.pool()).await;
    let _ = sqlx::query("DELETE FROM issues WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'r639pct-%')")
        .execute(db.pool()).await;
    let _ = sqlx::query("DELETE FROM companies WHERE name LIKE 'r639pct-%'")
        .execute(db.pool()).await;
}

async fn fixture(db: &Db) -> (Uuid, Uuid, Uuid, Uuid) {
    let company_id = Uuid::new_v4();
    let pipeline_id = Uuid::new_v4();
    let stage_id = Uuid::new_v4();
    let case_id = Uuid::new_v4();

    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(company_id)
    .bind(format!("r639pct-{company_id}"))
    .bind(format!("R{}", Uuid::new_v4().simple().to_string().chars().take(5).collect::<String>()))
    .execute(db.pool())
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO pipelines (id, company_id, key, name, created_at, updated_at) \
         VALUES ($1, $2, 'p1', 'Test Pipeline', now(), now())",
    )
    .bind(pipeline_id)
    .bind(company_id)
    .execute(db.pool())
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO pipeline_stages (id, pipeline_id, key, name, kind, position, created_at, updated_at) \
         VALUES ($1, $2, 's1', 'Stage 1', 'working', 0, now(), now())",
    )
    .bind(stage_id)
    .bind(pipeline_id)
    .execute(db.pool())
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO pipeline_cases (id, company_id, pipeline_id, stage_id, case_key, title, fields, child_count, terminal_child_count, version, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, 'CASE-1', 'Test Case', '{}'::jsonb, 0, 0, 1, now(), now())",
    )
    .bind(case_id)
    .bind(company_id)
    .bind(pipeline_id)
    .bind(stage_id)
    .execute(db.pool())
    .await
    .unwrap();

    (company_id, pipeline_id, stage_id, case_id)
}

async fn insert_issue(db: &Db, company_id: Uuid, identifier: &str, title: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, identifier, title, status, priority, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, 'todo', 'normal', now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(identifier)
    .bind(title)
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn insert_document(db: &Db, company_id: Uuid, issue_id: Uuid, key: &str, title: &str, body: &str) -> Uuid {
    let doc_id = Uuid::new_v4();
    let rev_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO documents (id, company_id, title, format, latest_body, latest_revision_id, latest_revision_number, created_at, updated_at) \
         VALUES ($1, $2, $3, 'markdown', $4, $5, 1, now(), now())",
    )
    .bind(doc_id)
    .bind(company_id)
    .bind(title)
    .bind(body)
    .bind(rev_id)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO document_revisions (id, company_id, document_id, revision_number, title, body, created_at) \
         VALUES ($1, $2, $3, 1, $4, $5, now())",
    )
    .bind(rev_id)
    .bind(company_id)
    .bind(doc_id)
    .bind(title)
    .bind(body)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO issue_documents (id, company_id, issue_id, document_id, key, created_at, updated_at) \
         VALUES (gen_random_uuid(), $1, $2, $3, $4, now(), now())",
    )
    .bind(company_id)
    .bind(issue_id)
    .bind(doc_id)
    .bind(key)
    .execute(db.pool())
    .await
    .unwrap();
    doc_id
}

async fn link_issue(db: &Db, company_id: Uuid, case_id: Uuid, issue_id: Uuid, role: &str) {
    sqlx::query(
        "INSERT INTO pipeline_case_issue_links (id, company_id, case_id, issue_id, role, created_at, updated_at) \
         VALUES (gen_random_uuid(), $1, $2, $3, $4, now(), now())",
    )
    .bind(company_id)
    .bind(case_id)
    .bind(issue_id)
    .bind(role)
    .execute(db.pool())
    .await
    .unwrap();
}

#[tokio::test]
async fn r639_list_case_outputs_returns_sources_and_documents() {
    let db = connect().await;
    cleanup(&db).await;
    let (company_id, _pipeline_id, _stage_id, case_id) = fixture(&db).await;
    let issue = insert_issue(&db, company_id, "PC-1", "Source Issue").await;
    link_issue(&db, company_id, case_id, issue, "work").await;
    let _doc = insert_document(&db, company_id, issue, "brief", "Project Brief", "Brief body content here.").await;

    let response = list_case_outputs(&db, company_id, case_id).await.expect("list_case_outputs");
    let response = response.expect("case exists");
    assert_eq!(response.company_id.as_deref(), Some(company_id.to_string().as_str()));
    assert_eq!(response.case_id.as_deref(), Some(case_id.to_string().as_str()));
    assert_eq!(response.items.len(), 1, "one document from one source issue");
    let item = &response.items[0];
    assert_eq!(item.kind as i32, pc_pipeline_case_outputs::PipelineCaseOutputItemKind::Document as i32);
    assert_eq!(item.title, "Project Brief");
    assert_eq!(item.document_key.as_deref(), Some("brief"));
    assert!(item.document_id.is_some());
    assert!(item.preview.as_ref().unwrap().contains("Brief body"));

    // 把 DB glue 输出接到 summarize 纯函数（端到端）
    let summary = summarize_pipeline_case_outputs_for_context(&response, Some(10));
    assert_eq!(summary.total_item_count, 1);
    assert_eq!(summary.item_count, 1);

    cleanup(&db).await;
}

#[tokio::test]
async fn r639_list_case_outputs_returns_none_for_unknown_case() {
    let db = connect().await;
    let unknown = Uuid::new_v4();
    let result = list_case_outputs(&db, Uuid::new_v4(), unknown).await.expect("ok");
    assert!(result.is_none());
}

#[tokio::test]
async fn r639_list_case_outputs_skips_retired_links() {
    let db = connect().await;
    cleanup(&db).await;
    let (company_id, _pipeline_id, _stage_id, case_id) = fixture(&db).await;
    let issue = insert_issue(&db, company_id, "PC-2", "Retired Link Source").await;
    link_issue(&db, company_id, case_id, issue, "work").await;
    // 标记 retired
    sqlx::query(
        "UPDATE pipeline_case_issue_links SET retired_at = now() WHERE company_id = $1 AND case_id = $2 AND issue_id = $3",
    )
    .bind(company_id)
    .bind(case_id)
    .bind(issue)
    .execute(db.pool())
    .await
    .unwrap();
    let _doc = insert_document(&db, company_id, issue, "brief", "Brief", "body").await;

    let response = list_case_outputs(&db, company_id, case_id).await.expect("ok");
    let response = response.expect("case exists");
    assert_eq!(response.items.len(), 0, "retired links should be filtered out");

    cleanup(&db).await;
}
