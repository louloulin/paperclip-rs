//! Round 137 集成测试：HeartbeatRepo 扩展方法（get_run_with_context / cancel_run_for_issue /
//! get_agent_and_context / insert_queued_run），用于 issues.rs relations 子模块剩余
//! 路由（get_issue_run / cancel_issue_run / restart_issue_run / start_issue_run）。

use pc_db::Db;
use pc_repos::heartbeat::HeartbeatRepo;
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
        .bind(format!("r137-{tag}-{id}"))
        .bind(format!("R137{}", &id.simple().to_string()[..4]))
        .execute(db.pool()).await.expect("company");
    id
}

async fn insert_agent(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO agents (id, company_id, name, kind, status, owner_user_id) VALUES ($1,$2,'a','assistant','active','tester')")
        .bind(id).bind(company_id)
        .execute(db.pool()).await.expect("agent");
    id
}

async fn insert_issue(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO issues (id, company_id, identifier, title, kind, status, priority) VALUES ($1,$2,$3,'i','task','todo','normal')")
        .bind(id).bind(company_id).bind(format!("ISS-{}", &id.simple().to_string()[..6]))
        .execute(db.pool()).await.expect("issue");
    id
}

async fn insert_run(db: &Db, company_id: Uuid, agent_id: Uuid, issue_id: Uuid, status: &str) -> Uuid {
    let id = Uuid::new_v4();
    let ctx = json!({"issueId": issue_id.to_string(), "source": "test"});
    sqlx::query("INSERT INTO heartbeat_runs (id, company_id, agent_id, invocation_source, status, context_snapshot) VALUES ($1, $2, $3, 'on_demand', $4, $5)")
        .bind(id).bind(company_id).bind(agent_id).bind(status).bind(&ctx)
        .execute(db.pool()).await.expect("run");
    id
}

// ===== HeartbeatRepo::get_run_with_context =====

/// 1. get_run_with_context — 返回完整 10 列元组。
#[tokio::test(flavor = "current_thread")]
async fn get_run_with_context_returns_full_tuple() {
    let db = db().await;
    let cid = insert_company(&db, "gw1").await;
    let aid = insert_agent(&db, cid).await;
    let iid = insert_issue(&db, cid).await;
    let run_id = insert_run(&db, cid, aid, iid, "queued").await;
    let row = HeartbeatRepo::new(&db).get_run_with_context(run_id).await.expect("get").expect("row");
    assert_eq!(row.0, run_id);
    assert_eq!(row.1, cid);
    assert_eq!(row.2, aid);
    assert_eq!(row.3, "queued");
    assert_eq!(row.4, "on_demand");
    assert!(row.9.get("issueId").is_some());
}

/// 2. get_run_with_context — 不存在返回 None。
#[tokio::test(flavor = "current_thread")]
async fn get_run_with_context_unknown_returns_none() {
    let db = db().await;
    let row = HeartbeatRepo::new(&db).get_run_with_context(Uuid::new_v4()).await.expect("ok");
    assert!(row.is_none());
}

// ===== HeartbeatRepo::cancel_run_for_issue =====

/// 3. cancel_run_for_issue — 取消 queued run。
#[tokio::test(flavor = "current_thread")]
async fn cancel_queued_run() {
    let db = db().await;
    let cid = insert_company(&db, "cq").await;
    let aid = insert_agent(&db, cid).await;
    let iid = insert_issue(&db, cid).await;
    let run_id = insert_run(&db, cid, aid, iid, "queued").await;
    assert!(HeartbeatRepo::new(&db).cancel_run_for_issue(run_id, iid).await.expect("cancel"));
    let row = HeartbeatRepo::new(&db).get_run_with_context(run_id).await.expect("get").expect("row");
    assert_eq!(row.3, "cancelled");
}

/// 4. cancel_run_for_issue — 取消 running run。
#[tokio::test(flavor = "current_thread")]
async fn cancel_running_run() {
    let db = db().await;
    let cid = insert_company(&db, "cr").await;
    let aid = insert_agent(&db, cid).await;
    let iid = insert_issue(&db, cid).await;
    let run_id = insert_run(&db, cid, aid, iid, "running").await;
    assert!(HeartbeatRepo::new(&db).cancel_run_for_issue(run_id, iid).await.expect("cancel"));
}

/// 5. cancel_run_for_issue — 已 cancelled 不再取消（幂等返回 false）。
#[tokio::test(flavor = "current_thread")]
async fn cancel_idempotent() {
    let db = db().await;
    let cid = insert_company(&db, "ci").await;
    let aid = insert_agent(&db, cid).await;
    let iid = insert_issue(&db, cid).await;
    let run_id = insert_run(&db, cid, aid, iid, "queued").await;
    let repo = HeartbeatRepo::new(&db);
    assert!(repo.cancel_run_for_issue(run_id, iid).await.expect("1st"));
    assert!(!repo.cancel_run_for_issue(run_id, iid).await.expect("2nd"));
}

/// 6. cancel_run_for_issue — issue id 不匹配时返回 false。
#[tokio::test(flavor = "current_thread")]
async fn cancel_rejects_wrong_issue() {
    let db = db().await;
    let cid = insert_company(&db, "cw").await;
    let aid = insert_agent(&db, cid).await;
    let i1 = insert_issue(&db, cid).await;
    let i2 = insert_issue(&db, cid).await;
    let run_id = insert_run(&db, cid, aid, i1, "queued").await;
    assert!(!HeartbeatRepo::new(&db).cancel_run_for_issue(run_id, i2).await.expect("cancel"));
}

// ===== HeartbeatRepo::get_agent_and_context =====

/// 7. get_agent_and_context — 返回 (agent_id, context_snapshot)。
#[tokio::test(flavor = "current_thread")]
async fn get_agent_and_context_returns_pair() {
    let db = db().await;
    let cid = insert_company(&db, "gac").await;
    let aid = insert_agent(&db, cid).await;
    let iid = insert_issue(&db, cid).await;
    let run_id = insert_run(&db, cid, aid, iid, "queued").await;
    let (agent_id, ctx) = HeartbeatRepo::new(&db)
        .get_agent_and_context(run_id)
        .await
        .expect("get")
        .expect("row");
    assert_eq!(agent_id, aid);
    assert_eq!(ctx.get("issueId").and_then(|v| v.as_str()), Some(iid.to_string().as_str()));
}

/// 8. get_agent_and_context — 不存在返回 None。
#[tokio::test(flavor = "current_thread")]
async fn get_agent_and_context_unknown_returns_none() {
    let db = db().await;
    let row = HeartbeatRepo::new(&db).get_agent_and_context(Uuid::new_v4()).await.expect("ok");
    assert!(row.is_none());
}

// ===== HeartbeatRepo::insert_queued_run =====

/// 9. insert_queued_run — 插入新 run 并 RETURNING。
#[tokio::test(flavor = "current_thread")]
async fn insert_queued_run_creates_new() {
    let db = db().await;
    let cid = insert_company(&db, "iq").await;
    let aid = insert_agent(&db, cid).await;
    let iid = insert_issue(&db, cid).await;
    let ctx = json!({"issueId": iid.to_string(), "source": "manual_start"});
    let new_id = Uuid::new_v4();
    HeartbeatRepo::new(&db).insert_queued_run(new_id, cid, aid, &ctx).await.expect("insert");
    let row = HeartbeatRepo::new(&db).get_run_with_context(new_id).await.expect("get").expect("row");
    assert_eq!(row.3, "queued");
    assert_eq!(row.4, "on_demand");
}

/// 10. insert_queued_run — context_snapshot 包含自定义字段。
#[tokio::test(flavor = "current_thread")]
async fn insert_queued_run_preserves_context() {
    let db = db().await;
    let cid = insert_company(&db, "ic").await;
    let aid = insert_agent(&db, cid).await;
    let iid = insert_issue(&db, cid).await;
    let ctx = json!({
        "issueId": iid.to_string(),
        "source": "manual_start",
        "retryOf": iid.to_string(),
        "wakeReason": "manual_restart",
    });
    let new_id = Uuid::new_v4();
    HeartbeatRepo::new(&db).insert_queued_run(new_id, cid, aid, &ctx).await.expect("insert");
    let (_, stored) = HeartbeatRepo::new(&db).get_agent_and_context(new_id).await.expect("get").expect("row");
    assert_eq!(stored.get("retryOf").and_then(|v| v.as_str()), Some(iid.to_string().as_str()));
    assert_eq!(stored.get("wakeReason").and_then(|v| v.as_str()), Some("manual_restart"));
}
