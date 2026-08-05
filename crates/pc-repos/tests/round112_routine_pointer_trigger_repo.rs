//! Round 112 集成测试：验证 RoutineRepo 3 个收尾方法
//! (update_revision_pointer / get_trigger_for_rotation / set_trigger_secret_ref)。

use pc_db::Db;
use pc_repos::routine::RoutineRepo;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(id)
        .bind(format!("r112-{tag}-{id}"))
        .bind(format!("R112{}", &id.simple().to_string()[..4]))
        .execute(db.pool()).await.expect("insert company");
    id
}

async fn insert_project(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO projects (id, company_id, name) VALUES ($1, $2, 'p112')")
        .bind(id).bind(company_id)
        .execute(db.pool()).await.expect("insert project");
    id
}

async fn insert_agent(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, status) VALUES ($1, $2, 'a112', 'active')",
    )
    .bind(id).bind(company_id)
    .execute(db.pool()).await.expect("insert agent");
    id
}

async fn insert_routine(db: &Db, company_id: Uuid, project_id: Uuid, agent_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO routines (id, company_id, project_id, title, assignee_agent_id, status) \
         VALUES ($1, $2, $3, 'r112', $4, 'active')",
    )
    .bind(id).bind(company_id).bind(project_id).bind(agent_id)
    .execute(db.pool()).await.expect("insert routine");
    id
}

async fn insert_trigger(
    db: &Db,
    company_id: Uuid,
    routine_id: Uuid,
    secret_ref: Option<&str>,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO routine_triggers (id, company_id, routine_id, kind, secret_ref) \
         VALUES ($1, $2, $3, 'webhook', $4)",
    )
    .bind(id).bind(company_id).bind(routine_id).bind(secret_ref)
    .execute(db.pool()).await.expect("insert trigger");
    id
}

async fn insert_revision(db: &Db, routine_id: Uuid, n: i32) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO routine_revisions (id, routine_id, revision_number, title, snapshot) \
         VALUES ($1, $2, $3, 'rev', '{}'::jsonb)",
    )
    .bind(id).bind(routine_id).bind(n)
    .execute(db.pool()).await.expect("insert revision");
    id
}

/// 1. update_revision_pointer: 写入 latest_revision_id/number + 同步 title/description
#[tokio::test(flavor = "current_thread")]
async fn update_revision_pointer_writes_fields() {
    let db = db().await;
    let cid = insert_company(&db, "ptr").await;
    let pid = insert_project(&db, cid).await;
    let aid = insert_agent(&db, cid).await;
    let rid = insert_routine(&db, cid, pid, aid).await;
    let rev_id = insert_revision(&db, rid, 1).await;

    let repo = RoutineRepo::new(&db);
    let n = repo
        .update_revision_pointer(rid, rev_id, 1, "new title", Some("new desc"))
        .await
        .expect("upd");
    assert_eq!(n, 1);

    let (latest_id, latest_num, title, description): (Uuid, i32, String, Option<String>) =
        sqlx::query_as(
            "SELECT latest_revision_id, latest_revision_number, title, description \
             FROM routines WHERE id = $1",
        )
        .bind(rid)
        .fetch_one(db.pool())
        .await
        .expect("query");
    assert_eq!(latest_id, rev_id);
    assert_eq!(latest_num, 1);
    assert_eq!(title, "new title");
    assert_eq!(description, Some("new desc".to_owned()));
}

/// 2. update_revision_pointer: description=None 设为 NULL
#[tokio::test(flavor = "current_thread")]
async fn update_revision_pointer_description_none() {
    let db = db().await;
    let cid = insert_company(&db, "ptr-no-desc").await;
    let pid = insert_project(&db, cid).await;
    let aid = insert_agent(&db, cid).await;
    let rid = insert_routine(&db, cid, pid, aid).await;
    let rev_id = insert_revision(&db, rid, 1).await;

    let repo = RoutineRepo::new(&db);
    let n = repo
        .update_revision_pointer(rid, rev_id, 1, "t", None)
        .await
        .expect("upd");
    assert_eq!(n, 1);
    let description: Option<String> = sqlx::query_scalar(
        "SELECT description FROM routines WHERE id = $1",
    )
    .bind(rid)
    .fetch_one(db.pool())
    .await
    .expect("query");
    assert!(description.is_none());
}

/// 3. update_revision_pointer: 未知 routine 返 0
#[tokio::test(flavor = "current_thread")]
async fn update_revision_pointer_missing_returns_zero() {
    let db = db().await;
    let repo = RoutineRepo::new(&db);
    let n = repo
        .update_revision_pointer(Uuid::new_v4(), Uuid::new_v4(), 1, "x", None)
        .await
        .expect("upd");
    assert_eq!(n, 0);
}

/// 4. get_trigger_for_rotation: 找到 / 找不到
#[tokio::test(flavor = "current_thread")]
async fn get_trigger_for_rotation_round_trip() {
    let db = db().await;
    let cid = insert_company(&db, "trig").await;
    let pid = insert_project(&db, cid).await;
    let aid = insert_agent(&db, cid).await;
    let rid = insert_routine(&db, cid, pid, aid).await;
    let tid = insert_trigger(&db, cid, rid, Some("sec_old")).await;

    let repo = RoutineRepo::new(&db);
    let info = repo
        .get_trigger_for_rotation(tid)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(info.company_id, cid);
    assert_eq!(info.routine_id, rid);
    assert_eq!(info.existing_secret_ref, Some("sec_old".to_owned()));

    let none = repo
        .get_trigger_for_rotation(Uuid::new_v4())
        .await
        .expect("get");
    assert!(none.is_none());
}

/// 5. get_trigger_for_rotation: secret_ref 为 None
#[tokio::test(flavor = "current_thread")]
async fn get_trigger_for_rotation_null_secret() {
    let db = db().await;
    let cid = insert_company(&db, "trig-null").await;
    let pid = insert_project(&db, cid).await;
    let aid = insert_agent(&db, cid).await;
    let rid = insert_routine(&db, cid, pid, aid).await;
    let tid = insert_trigger(&db, cid, rid, None).await;

    let repo = RoutineRepo::new(&db);
    let info = repo.get_trigger_for_rotation(tid).await.expect("get").expect("present");
    assert!(info.existing_secret_ref.is_none());
}

/// 6. set_trigger_secret_ref: 写入新 secret_ref + metadata 记录
#[tokio::test(flavor = "current_thread")]
async fn set_trigger_secret_ref_writes_and_metadata() {
    let db = db().await;
    let cid = insert_company(&db, "rot").await;
    let pid = insert_project(&db, cid).await;
    let aid = insert_agent(&db, cid).await;
    let rid = insert_routine(&db, cid, pid, aid).await;
    let tid = insert_trigger(&db, cid, rid, Some("old")).await;

    let repo = RoutineRepo::new(&db);
    let n = repo
        .set_trigger_secret_ref(tid, "new://ref", Some("manual rotation"))
        .await
        .expect("set");
    assert_eq!(n, 1);

    let (secret_ref_col, meta): (Option<String>, serde_json::Value) = sqlx::query_as(
        "SELECT secret_ref, metadata FROM routine_triggers WHERE id = $1",
    )
    .bind(tid)
    .fetch_one(db.pool())
    .await
    .expect("query");
    assert_eq!(secret_ref_col, Some("new://ref".to_owned()));
    assert_eq!(meta["rotateReason"], serde_json::json!("manual rotation"));
    assert!(meta["rotatedAt"].is_string());
}

/// 7. set_trigger_secret_ref: reason=None 不报错
#[tokio::test(flavor = "current_thread")]
async fn set_trigger_secret_ref_no_reason() {
    let db = db().await;
    let cid = insert_company(&db, "rot-nr").await;
    let pid = insert_project(&db, cid).await;
    let aid = insert_agent(&db, cid).await;
    let rid = insert_routine(&db, cid, pid, aid).await;
    let tid = insert_trigger(&db, cid, rid, None).await;

    let repo = RoutineRepo::new(&db);
    let n = repo
        .set_trigger_secret_ref(tid, "x://y", None)
        .await
        .expect("set");
    assert_eq!(n, 1);
}

/// 8. set_trigger_secret_ref: 未知 trigger 返 0
#[tokio::test(flavor = "current_thread")]
async fn set_trigger_secret_ref_missing_returns_zero() {
    let db = db().await;
    let repo = RoutineRepo::new(&db);
    let n = repo
        .set_trigger_secret_ref(Uuid::new_v4(), "x://y", None)
        .await
        .expect("set");
    assert_eq!(n, 0);
}
