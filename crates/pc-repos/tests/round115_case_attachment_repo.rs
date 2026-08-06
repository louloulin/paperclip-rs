//! Round 115 集成测试：验证 CaseRepo 2 个 case_attachments 新方法。
//! - upsert_case_attachment
//! - record_attachment_added_event

use pc_db::Db;
use pc_repos::case::CaseRepo;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(id)
        .bind(format!("r115-{tag}-{id}"))
        .bind(format!("R115{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("insert company");
    id
}

async fn insert_case(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO cases (id, company_id, case_number, identifier, case_type, key, title, status) \
         VALUES ($1, $2, 1, 'CASE-001', 'requirement', NULL, 'test', 'draft')",
    )
    .bind(id).bind(company_id)
    .execute(db.pool()).await.expect("insert case");
    id
}

/// 1. upsert_case_attachment 首次插入
#[tokio::test(flavor = "current_thread")]
async fn upsert_case_attachment_inserts_new() {
    let db = db().await;
    let cid = insert_company(&db, "new").await;
    let case_id = insert_case(&db, cid).await;
    let asset_id = Uuid::new_v4();

    let repo = CaseRepo::new(&db);
    let id = repo
        .upsert_case_attachment(cid, case_id, asset_id)
        .await
        .expect("upsert");
    assert!(!id.is_nil());

    let (back_company, back_case, back_asset): (Uuid, Uuid, Uuid) =
        sqlx::query_as("SELECT company_id, case_id, asset_id FROM case_attachments WHERE id = $1")
            .bind(id)
            .fetch_one(db.pool())
            .await
            .expect("query");
    assert_eq!(back_company, cid);
    assert_eq!(back_case, case_id);
    assert_eq!(back_asset, asset_id);
}

/// 2. upsert_case_attachment 重复 upsert 返相同 id
#[tokio::test(flavor = "current_thread")]
async fn upsert_case_attachment_idempotent() {
    let db = db().await;
    let cid = insert_company(&db, "idem").await;
    let case_id = insert_case(&db, cid).await;
    let asset_id = Uuid::new_v4();

    let repo = CaseRepo::new(&db);
    let id1 = repo
        .upsert_case_attachment(cid, case_id, asset_id)
        .await
        .expect("1");
    let id2 = repo
        .upsert_case_attachment(cid, case_id, asset_id)
        .await
        .expect("2");
    assert_eq!(id1, id2);
}

/// 3. upsert_case_attachment 跨 case 隔离（同一 asset 多次到不同 case 创建新行）
#[tokio::test(flavor = "current_thread")]
async fn upsert_case_attachment_cross_case() {
    let db = db().await;
    let cid = insert_company(&db, "cross").await;
    let case_a = insert_case(&db, cid).await;
    let case_b = insert_case(&db, cid).await;
    let asset_id = Uuid::new_v4();

    let repo = CaseRepo::new(&db);
    let id_a = repo
        .upsert_case_attachment(cid, case_a, asset_id)
        .await
        .expect("a");
    let id_b = repo
        .upsert_case_attachment(cid, case_b, asset_id)
        .await
        .expect("b");
    assert_ne!(id_a, id_b);
}

/// 4. record_attachment_added_event 写入
#[tokio::test(flavor = "current_thread")]
async fn record_attachment_added_event_writes() {
    let db = db().await;
    let cid = insert_company(&db, "ev").await;
    let case_id = insert_case(&db, cid).await;
    let asset_id = Uuid::new_v4();

    let repo = CaseRepo::new(&db);
    let event_id = repo
        .record_attachment_added_event(cid, case_id, asset_id)
        .await
        .expect("record");
    let (kind, payload): (String, serde_json::Value) =
        sqlx::query_as("SELECT kind, payload FROM case_events WHERE id = $1")
            .bind(event_id)
            .fetch_one(db.pool())
            .await
            .expect("query");
    assert_eq!(kind, "attachment_added");
    assert_eq!(payload["assetId"], serde_json::json!(asset_id.to_string()));
}

/// 5. upsert + event 组合流程
#[tokio::test(flavor = "current_thread")]
async fn upsert_then_record_event_end_to_end() {
    let db = db().await;
    let cid = insert_company(&db, "e2e").await;
    let case_id = insert_case(&db, cid).await;
    let asset_id = Uuid::new_v4();

    let repo = CaseRepo::new(&db);
    let attachment_id = repo
        .upsert_case_attachment(cid, case_id, asset_id)
        .await
        .expect("up");
    let event_id = repo
        .record_attachment_added_event(cid, case_id, asset_id)
        .await
        .expect("ev");

    // 验证 attachment + event 都存在
    let (a_count, e_count): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*)::bigint FROM case_attachments WHERE id = $1) AS ac,                 (SELECT count(*)::bigint FROM case_events WHERE id = $2 AND kind = 'attachment_added') AS ec",
    )
    .bind(attachment_id)
    .bind(event_id)
    .fetch_one(db.pool())
    .await
    .expect("query");
    assert_eq!(a_count, 1);
    assert_eq!(e_count, 1);
}
