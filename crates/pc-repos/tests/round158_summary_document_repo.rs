//! Round 158 集成测试：summary_slots 仓储化扩展 — SummaryRepo +5 方法 / DocumentRepo +6 方法 / DocumentRevisionRow 字段扩展。

use pc_db::Db;
use pc_repos::document::{DocumentRepo, DocumentRevisionRow, DocumentRow};
use pc_repos::summary::{SummaryRepo, SummarySlotRow};
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(format!("r158-{tag}-{id}"))
        .bind(format!("R158{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("company");
    id
}

// ===== DocumentRepo 新方法 =====

/// 1. get_in_company — 命中 / 不命中。
#[tokio::test(flavor = "current_thread")]
async fn get_in_company_basic() {
    let db = db().await;
    let cid = insert_company(&db, "gic1").await;
    let did = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO documents (id, company_id, title, format, latest_body) VALUES ($1, $2, 'a', 'markdown', 'b')",
    )
    .bind(did)
    .bind(cid)
    .execute(db.pool())
    .await
    .expect("doc");

    let repo = DocumentRepo::new(&db);
    let hit: Option<DocumentRow> = repo.get_in_company(cid, did).await.expect("hit");
    assert!(hit.is_some());

    let wrong_company: Option<DocumentRow> = repo.get_in_company(Uuid::new_v4(), did).await.expect("wrong");
    assert!(wrong_company.is_none());
}

/// 2. latest_revision_id_in_company — 取最新 revision id。
#[tokio::test(flavor = "current_thread")]
async fn latest_revision_id_in_company_basic() {
    let db = db().await;
    let cid = insert_company(&db, "lrc1").await;
    let did = Uuid::new_v4();
    let rid = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO documents (id, company_id, format, latest_body, latest_revision_id, latest_revision_number) VALUES ($1, $2, 'markdown', 'x', $3, 1)",
    )
    .bind(did)
    .bind(cid)
    .bind(rid)
    .execute(db.pool())
    .await
    .expect("doc");

    let repo = DocumentRepo::new(&db);
    let v = repo.latest_revision_id_in_company(cid, did).await.expect("get");
    assert_eq!(v, Some(rid));

    let none_v = repo.latest_revision_id_in_company(Uuid::new_v4(), did).await.expect("get wrong");
    assert!(none_v.is_none());
}

/// 3. write_body — 更新 document body + rev 递增。
#[tokio::test(flavor = "current_thread")]
async fn write_body_basic() {
    let db = db().await;
    let cid = insert_company(&db, "wb1").await;
    let did = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO documents (id, company_id, format, latest_body, latest_revision_number) VALUES ($1, $2, 'markdown', 'orig', 5)",
    )
    .bind(did)
    .bind(cid)
    .execute(db.pool())
    .await
    .expect("doc");

    let repo = DocumentRepo::new(&db);
    let now = chrono::Utc::now();
    let row = repo
        .write_body(cid, did, Some("new-title"), "new-body", now)
        .await
        .expect("write");
    assert_eq!(row.title, Some("new-title".to_string()));
    assert_eq!(row.latest_body, "new-body");
    assert_eq!(row.latest_revision_number, 6);
    // write_body 把 updated_by_agent_id 设为 NULL
    assert!(row.updated_by_agent_id.is_none());
}

/// 4. create_markdown — 创建新 markdown document。
#[tokio::test(flavor = "current_thread")]
async fn create_markdown_basic() {
    let db = db().await;
    let cid = insert_company(&db, "cm1").await;
    let repo = DocumentRepo::new(&db);
    let now = chrono::Utc::now();
    let row = repo
        .create_markdown(cid, Some("md-title"), "md-body", now)
        .await
        .expect("create");
    assert_eq!(row.title, Some("md-title".to_string()));
    assert_eq!(row.format, "markdown");
    assert_eq!(row.latest_body, "md-body");
    assert_eq!(row.company_id, cid);
}

/// 5. set_latest_revision — 设置 latest_revision_id + number。
#[tokio::test(flavor = "current_thread")]
async fn set_latest_revision_basic() {
    let db = db().await;
    let cid = insert_company(&db, "slr1").await;
    let did = Uuid::new_v4();
    let rid = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO documents (id, company_id, format, latest_body) VALUES ($1, $2, 'markdown', 'x')",
    )
    .bind(did)
    .bind(cid)
    .execute(db.pool())
    .await
    .expect("doc");

    let repo = DocumentRepo::new(&db);
    let row = repo.set_latest_revision(did, rid, 7).await.expect("set");
    assert_eq!(row.latest_revision_id, Some(rid));
    assert_eq!(row.latest_revision_number, 7);
}

/// 6. insert_revision_full — 创建 revision（带 title/format/body）。
#[tokio::test(flavor = "current_thread")]
async fn insert_revision_full_basic() {
    let db = db().await;
    let cid = insert_company(&db, "irf1").await;
    let did = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO documents (id, company_id, format, latest_body) VALUES ($1, $2, 'markdown', 'x')",
    )
    .bind(did)
    .bind(cid)
    .execute(db.pool())
    .await
    .expect("doc");

    let repo = DocumentRepo::new(&db);
    let now = chrono::Utc::now();
    let rev: DocumentRevisionRow = repo
        .insert_revision_full(cid, did, 1, Some("v1-title"), "v1-body", Some("init"), now)
        .await
        .expect("insert");
    assert_eq!(rev.format.as_deref(), Some("markdown"));
    assert_eq!(rev.title.as_deref(), Some("v1-title"));
    assert_eq!(rev.body, "v1-body");
    assert_eq!(rev.document_id, did);
    assert_eq!(rev.company_id, cid);
    assert_eq!(rev.revision_number, 1);
}

/// 7. list_revisions_in_company — 按 company + document_id 列出，按 rev 倒序，limit。
#[tokio::test(flavor = "current_thread")]
async fn list_revisions_in_company_basic() {
    let db = db().await;
    let cid = insert_company(&db, "lri1").await;
    let did = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO documents (id, company_id, format, latest_body) VALUES ($1, $2, 'markdown', 'x')",
    )
    .bind(did)
    .bind(cid)
    .execute(db.pool())
    .await
    .expect("doc");

    let repo = DocumentRepo::new(&db);
    let now = chrono::Utc::now();
    // 插入 3 revisions
    for n in 1..=3 {
        repo.insert_revision_full(cid, did, n, None, &format!("b{n}"), None, now)
            .await
            .expect("insert rev");
    }

    let rows = repo.list_revisions_in_company(cid, did, 20).await.expect("list");
    assert_eq!(rows.len(), 3);
    // 倒序：3, 2, 1
    assert_eq!(rows[0].revision_number, 3);
    assert_eq!(rows[1].revision_number, 2);
    assert_eq!(rows[2].revision_number, 1);

    // limit=2 只取前两个
    let top2 = repo.list_revisions_in_company(cid, did, 2).await.expect("top2");
    assert_eq!(top2.len(), 2);
    assert_eq!(top2[0].revision_number, 3);
}

// ===== SummaryRepo 新方法 =====

/// 8. find_by_scope_str — 字符串 scope_kind + Option<Uuid> scope_id。
#[tokio::test(flavor = "current_thread")]
async fn find_by_scope_str_basic() {
    let db = db().await;
    let cid = insert_company(&db, "fbs1").await;
    let repo = SummaryRepo::new(&db);
    // 没有 scope_id
    let id: Uuid = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO summary_slots (id, company_id, scope_kind, scope_id, slot_key, status) VALUES ($1, $2, 'company', NULL, 'k1', 'idle')",
    )
    .bind(id)
    .bind(cid)
    .execute(db.pool())
    .await
    .expect("slot");

    let hit: Option<SummarySlotRow> = repo
        .find_by_scope_str(cid, "company", "k1", None)
        .await
        .expect("hit");
    assert!(hit.is_some());

    let miss: Option<SummarySlotRow> = repo
        .find_by_scope_str(cid, "company", "missing-key", None)
        .await
        .expect("miss");
    assert!(miss.is_none());
}

/// 9. insert_idle — 创建 idle slot。
#[tokio::test(flavor = "current_thread")]
async fn insert_idle_basic() {
    let db = db().await;
    let cid = insert_company(&db, "ii1").await;
    let repo = SummaryRepo::new(&db);
    let row: SummarySlotRow = repo
        .insert_idle(cid, "company", None, "slot-x")
        .await
        .expect("insert");
    assert_eq!(row.status, "idle");
    assert_eq!(row.slot_key, "slot-x");
    assert_eq!(row.scope_kind, "company");
    assert_eq!(row.company_id, cid);
}

/// 10. mark_slot_written — UPDATE slot to idle with doc + model + RETURNING。
#[tokio::test(flavor = "current_thread")]
async fn mark_slot_written_basic() {
    let db = db().await;
    let cid = insert_company(&db, "msw1").await;
    let repo = SummaryRepo::new(&db);
    let slot = repo.insert_idle(cid, "company", None, "msw-slot").await.expect("idle");
    let did = Uuid::new_v4();
    let now = chrono::Utc::now();
    let back: SummarySlotRow = repo
        .mark_slot_written(slot.id, did, now, Some("gpt-5"))
        .await
        .expect("mark");
    assert_eq!(back.document_id, Some(did));
    assert_eq!(back.status, "idle");
    assert!(back.generating_issue_id.is_none());
    assert_eq!(back.last_model.as_deref(), Some("gpt-5"));
}

/// 11. update_to_generating — 标记为 generating。
#[tokio::test(flavor = "current_thread")]
async fn update_to_generating_basic() {
    let db = db().await;
    let cid = insert_company(&db, "utg1").await;
    let repo = SummaryRepo::new(&db);
    let slot = repo.insert_idle(cid, "company", None, "utg-slot").await.expect("idle");
    let issue_id: Uuid = Uuid::new_v4();
    let back: SummarySlotRow = repo
        .update_to_generating(slot.id, issue_id)
        .await
        .expect("update");
    assert_eq!(back.status, "generating");
    assert_eq!(back.generating_issue_id, Some(issue_id));
}

/// 12. insert_generating — INSERT + generating 状态。
#[tokio::test(flavor = "current_thread")]
async fn insert_generating_basic() {
    let db = db().await;
    let cid = insert_company(&db, "ig1").await;
    let repo = SummaryRepo::new(&db);
    let issue_id: Uuid = Uuid::new_v4();
    let row: SummarySlotRow = repo
        .insert_generating(cid, "issue", None, "gen-slot", issue_id)
        .await
        .expect("insert");
    assert_eq!(row.status, "generating");
    assert_eq!(row.generating_issue_id, Some(issue_id));
    assert_eq!(row.scope_kind, "issue");
}

// ===== DTO smoke (sync) =====

/// 13. DocumentRevisionRow 1:1 schema projection — 验证 FromRow + 字段集。
#[test]
fn document_revision_row_typecheck() {
    fn assert_from_row<T: for<'a> sqlx::FromRow<'a, sqlx::postgres::PgRow>>() {}
    assert_from_row::<DocumentRevisionRow>();
}

/// 14. DocumentRow 类型 smoke。
#[test]
fn document_row_typecheck() {
    fn assert_from_row<T: for<'a> sqlx::FromRow<'a, sqlx::postgres::PgRow>>() {}
    assert_from_row::<DocumentRow>();
}

/// 15. SummarySlotRow 类型 smoke。
#[test]
fn summary_slot_row_typecheck() {
    fn assert_from_row<T: for<'a> sqlx::FromRow<'a, sqlx::postgres::PgRow>>() {}
    assert_from_row::<SummarySlotRow>();
}

