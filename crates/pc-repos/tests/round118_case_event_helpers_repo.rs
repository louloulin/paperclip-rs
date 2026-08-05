//! Round 118 集成测试：验证 CaseRepo::record_case_event 通用辅助方法。
//!
//! 同时验证 get_case_company_id 作为 ensure_case_exists 的替代。

use pc_db::Db;
use pc_repos::case::CaseRepo;
use serde_json::json;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(id)
        .bind(format!("r118-{tag}-{id}"))
        .bind(format!("R118{}", &id.simple().to_string()[..4]))
        .execute(db.pool()).await.expect("insert company");
    id
}

async fn insert_case(db: &Db, company_id: Uuid, status: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO cases (id, company_id, case_number, identifier, case_type, key, title, status) \
         VALUES ($1, $2, 1, 'CASE-001', 'requirement', NULL, 'test', $3)",
    )
    .bind(id).bind(company_id).bind(status)
    .execute(db.pool()).await.expect("insert case");
    id
}

/// 1. status_changed 事件（review 路由场景）
#[tokio::test(flavor = "current_thread")]
async fn record_event_status_changed() {
    let db = db().await;
    let cid = insert_company(&db, "statchg").await;
    let case_id = insert_case(&db, cid, "in_review").await;
    let payload = json!({
        "decision": "approved",
        "note": Some("looks good"),
        "expectedVersion": 3_i32,
    });
    let event_id = CaseRepo::new(&db)
        .record_case_event(cid, case_id, "status_changed", "user", payload.clone())
        .await
        .expect("record status_changed");
    assert!(!event_id.is_nil(), "event id should be returned");
}

/// 2. fields_changed 事件 + system actor（suggest_transition 路由场景）
#[tokio::test(flavor = "current_thread")]
async fn record_event_fields_changed_system() {
    let db = db().await;
    let cid = insert_company(&db, "fc-system").await;
    let case_id = insert_case(&db, cid, "in_progress").await;
    let payload = json!({
        "toStageKey": "stage.done",
        "reason": "agent suggests completion",
        "confidence": 0.92_f64,
    });
    let event_id = CaseRepo::new(&db)
        .record_case_event(cid, case_id, "fields_changed", "system", payload)
        .await
        .expect("record fields_changed/system");
    assert!(!event_id.is_nil());
}

/// 3. fields_changed 事件 + user actor（resolve_suggestion / acknowledge_drift 路由场景）
#[tokio::test(flavor = "current_thread")]
async fn record_event_fields_changed_user() {
    let db = db().await;
    let cid = insert_company(&db, "fc-user").await;
    let case_id = insert_case(&db, cid, "in_progress").await;
    let payload = json!({ "event": "drift_acknowledged" });
    let event_id = CaseRepo::new(&db)
        .record_case_event(cid, case_id, "fields_changed", "user", payload)
        .await
        .expect("record fields_changed/user");
    assert!(!event_id.is_nil());
}

/// 4. document_revised 事件（delete_case_document 路由场景）
#[tokio::test(flavor = "current_thread")]
async fn record_event_document_revised() {
    let db = db().await;
    let cid = insert_company(&db, "docrev").await;
    let case_id = insert_case(&db, cid, "in_progress").await;
    let payload = json!({ "key": "spec.md", "deleted": true });
    let event_id = CaseRepo::new(&db)
        .record_case_event(cid, case_id, "document_revised", "user", payload)
        .await
        .expect("record document_revised");
    assert!(!event_id.is_nil());
}

/// 5. get_case_company_id — 替换 ensure_case_exists 的等价方法
#[tokio::test(flavor = "current_thread")]
async fn get_case_company_id_returns_some_for_existing_case() {
    let db = db().await;
    let cid = insert_company(&db, "gcid-some").await;
    let case_id = insert_case(&db, cid, "draft").await;
    let fetched = CaseRepo::new(&db)
        .get_case_company_id(case_id)
        .await
        .expect("get company id");
    assert_eq!(fetched, Some(cid));
}

/// 6. get_case_company_id — 不存在返回 None
#[tokio::test(flavor = "current_thread")]
async fn get_case_company_id_returns_none_for_missing_case() {
    let db = db().await;
    let missing = Uuid::new_v4();
    let fetched = CaseRepo::new(&db)
        .get_case_company_id(missing)
        .await
        .expect("get company id");
    assert_eq!(fetched, None);
}
