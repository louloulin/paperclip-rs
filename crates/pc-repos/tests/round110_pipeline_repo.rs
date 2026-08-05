//! Round 110 集成测试：验证 PipelineRepo::get_stage_config / set_stage_config /
//! get_pipeline_document_meta / list_pipeline_document_revisions /
//! touch_pipeline_document / company_id_for_pipeline / create_case_minimal
//! 全部走真实 schema 路径。

use pc_db::Db;
use pc_repos::pipeline::PipelineRepo;
use serde_json::json;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(id)
        .bind(format!("r110-{tag}-{id}"))
        .bind(format!("R110{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("insert company");
    id
}

async fn insert_pipeline(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO pipelines (id, company_id, key, name, enforce_transitions) \
         VALUES ($1, $2, $3, 'p110', false)",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("pl-{id}"))
    .execute(db.pool())
    .await
    .expect("insert pipeline");
    id
}

async fn insert_stage(db: &Db, pipeline_id: Uuid, key: &str, kind: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO pipeline_stages (id, pipeline_id, key, name, kind, position, config) \
         VALUES ($1, $2, $3, $3, $4, 0, '{}'::jsonb)",
    )
    .bind(id)
    .bind(pipeline_id)
    .bind(key)
    .bind(kind)
    .execute(db.pool())
    .await
    .expect("insert stage");
    id
}

async fn insert_pipeline_document(
    db: &Db,
    company_id: Uuid,
    pipeline_id: Uuid,
    key: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    let doc_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO pipeline_documents (id, company_id, pipeline_id, document_id, key) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(company_id)
    .bind(pipeline_id)
    .bind(doc_id)
    .bind(key)
    .execute(db.pool())
    .await
    .expect("insert pipeline_document");
    id
}

/// 1. get_stage_config：未存在 stage 返 None
#[tokio::test(flavor = "current_thread")]
async fn stage_config_missing_returns_none() {
    let db = db().await;
    let repo = PipelineRepo::new(&db);
    let none = repo
        .get_stage_config(Uuid::new_v4())
        .await
        .expect("get");
    assert!(none.is_none());
}

/// 2. set_stage_config + get_stage_config round trip
#[tokio::test(flavor = "current_thread")]
async fn stage_config_round_trip() {
    let db = db().await;
    let cid = insert_company(&db, "cfg").await;
    let pid = insert_pipeline(&db, cid).await;
    let sid = insert_stage(&db, pid, "open", "open").await;

    let repo = PipelineRepo::new(&db);
    let cfg = json!({"automation_env": {"KEY": "VAL"}, "x": 1});
    assert!(repo.set_stage_config(sid, &cfg).await.expect("set"));

    let back = repo
        .get_stage_config(sid)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(back, cfg);
}

/// 3. set_stage_config：未知 stage_id 返 Ok(false)
#[tokio::test(flavor = "current_thread")]
async fn stage_config_set_unknown_returns_false() {
    let db = db().await;
    let repo = PipelineRepo::new(&db);
    let ok = repo
        .set_stage_config(Uuid::new_v4(), &json!({}))
        .await
        .expect("set");
    assert!(!ok);
}

/// 4. get_pipeline_document_meta：未存在返 None
#[tokio::test(flavor = "current_thread")]
async fn pipeline_document_meta_missing_returns_none() {
    let db = db().await;
    let repo = PipelineRepo::new(&db);
    let none = repo
        .get_pipeline_document_meta(Uuid::new_v4(), "ghost")
        .await
        .expect("get");
    assert!(none.is_none());
}

/// 5. get_pipeline_document_meta：存在返 stub Value
#[tokio::test(flavor = "current_thread")]
async fn pipeline_document_meta_returns_stub_value() {
    let db = db().await;
    let cid = insert_company(&db, "meta").await;
    let pid = insert_pipeline(&db, cid).await;
    insert_pipeline_document(&db, cid, pid, "design").await;

    let repo = PipelineRepo::new(&db);
    let v = repo
        .get_pipeline_document_meta(pid, "design")
        .await
        .expect("get")
        .expect("present");
    assert_eq!(v["pipelineId"], json!(pid));
    assert_eq!(v["key"], json!("design"));
    assert_eq!(v["deprecated"], json!(true));
    assert!(v["id"].is_string());
    assert!(v["createdAt"].is_string());
    assert!(v["updatedAt"].is_string());
}

/// 6. touch_pipeline_document：upsert 命中已存在
#[tokio::test(flavor = "current_thread")]
async fn touch_pipeline_document_updates_existing() {
    let db = db().await;
    let cid = insert_company(&db, "touch-up").await;
    let pid = insert_pipeline(&db, cid).await;
    insert_pipeline_document(&db, cid, pid, "design").await;

    let repo = PipelineRepo::new(&db);
    assert!(repo.touch_pipeline_document(pid, "design").await.expect("touch"));
}

/// 7. touch_pipeline_document：upsert 缺失时插入
#[tokio::test(flavor = "current_thread")]
async fn touch_pipeline_document_inserts_when_missing() {
    let db = db().await;
    let cid = insert_company(&db, "touch-in").await;
    let pid = insert_pipeline(&db, cid).await;

    let repo = PipelineRepo::new(&db);
    assert!(repo.touch_pipeline_document(pid, "newkey").await.expect("touch"));

    let row: Option<(String,)> = sqlx::query_as(
        "SELECT key FROM pipeline_documents WHERE pipeline_id=$1 AND key=$2",
    )
    .bind(pid)
    .bind("newkey")
    .fetch_optional(db.pool())
    .await
    .expect("query");
    assert_eq!(row.map(|(k,)| k), Some("newkey".to_string()));
}

/// 8. touch_pipeline_document：未知 pipeline 返 Ok(false)
#[tokio::test(flavor = "current_thread")]
async fn touch_pipeline_document_unknown_pipeline_returns_false() {
    let db = db().await;
    let repo = PipelineRepo::new(&db);
    let ok = repo
        .touch_pipeline_document(Uuid::new_v4(), "x")
        .await
        .expect("touch");
    assert!(!ok);
}

/// 9. list_pipeline_document_revisions：按 created_at ASC
#[tokio::test(flavor = "current_thread")]
async fn pipeline_document_revisions_orders_asc() {
    let db = db().await;
    let cid = insert_company(&db, "rev").await;
    let pid = insert_pipeline(&db, cid).await;
    insert_pipeline_document(&db, cid, pid, "design").await;
    insert_pipeline_document(&db, cid, pid, "design").await;

    let repo = PipelineRepo::new(&db);
    let revs = repo
        .list_pipeline_document_revisions(pid, "design")
        .await
        .expect("list");
    assert_eq!(revs.len(), 2);
    assert!(revs[0].as_datetime() <= revs[1].as_datetime());
}

/// 10. company_id_for_pipeline：正向 / 反向
#[tokio::test(flavor = "current_thread")]
async fn company_id_for_pipeline_round_trip() {
    let db = db().await;
    let cid = insert_company(&db, "lookup").await;
    let pid = insert_pipeline(&db, cid).await;

    let repo = PipelineRepo::new(&db);
    let back = repo
        .company_id_for_pipeline(pid)
        .await
        .expect("lookup")
        .expect("present");
    assert_eq!(back, cid);

    let none = repo
        .company_id_for_pipeline(Uuid::new_v4())
        .await
        .expect("lookup");
    assert!(none.is_none());
}

/// 11. create_case_minimal：要求有效 stage_id
#[tokio::test(flavor = "current_thread")]
async fn create_case_minimal_inserts_case() {
    let db = db().await;
    let cid = insert_company(&db, "case").await;
    let pid = insert_pipeline(&db, cid).await;
    let sid = insert_stage(&db, pid, "open", "open").await;

    let repo = PipelineRepo::new(&db);
    let case_id = repo
        .create_case_minimal(cid, pid, sid, 1, "c1", "Test Case", &json!({"foo": "bar"}))
        .await
        .expect("insert");

    let row: (Uuid, Uuid, Uuid, String, String) = sqlx::query_as(
        "SELECT id, pipeline_id, stage_id, case_key, title FROM pipeline_cases WHERE id=$1",
    )
    .bind(case_id)
    .fetch_one(db.pool())
    .await
    .expect("query");
    assert_eq!(row.0, case_id);
    assert_eq!(row.1, pid);
    assert_eq!(row.2, sid);
    assert_eq!(row.3, "c1");
    assert_eq!(row.4, "Test Case");
}
