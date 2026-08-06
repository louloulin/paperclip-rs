//! Round 120 集成测试：验证 CaseRepo 复合事务方法：
//! breakdown_case, replace_blockers, open_conversation,
//! list_context_events, list_context_issues, count_children, list_outputs.

use pc_db::Db;
use pc_repos::case::{CaseRepo, NewBreakdownChild};
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
        .bind(format!("r120-{tag}-{id}"))
        .bind(format!("R120{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("insert company");
    id
}

async fn insert_case(db: &Db, company_id: Uuid, parent: Option<Uuid>, status: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO cases (id, company_id, case_number, identifier, case_type, key, title, status, parent_case_id) \
         VALUES ($1, $2, 1, 'CASE-001', 'requirement', NULL, 'test', $3, $4)",
    )
    .bind(id).bind(company_id).bind(status).bind(parent)
    .execute(db.pool()).await.expect("insert case");
    id
}

async fn insert_issue(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, identifier, title, status, kind) \
         VALUES ($1, $2, $3, 'i', 'open', 'task')",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("ISS-{}", &id.simple().to_string()[..6]))
    .execute(db.pool())
    .await
    .expect("insert issue");
    id
}

async fn insert_event(
    db: &Db,
    company_id: Uuid,
    case_id: Uuid,
    kind: &str,
    payload: serde_json::Value,
) {
    sqlx::query(
        "INSERT INTO case_events (company_id, case_id, kind, actor_type, payload) \
         VALUES ($1, $2, $3, 'user', $4::jsonb)",
    )
    .bind(company_id)
    .bind(case_id)
    .bind(kind)
    .bind(payload)
    .execute(db.pool())
    .await
    .expect("insert event");
}

/// 1. breakdown_case — 复合事务：插入 N child + 事件 + commit
#[tokio::test(flavor = "current_thread")]
async fn breakdown_case_creates_children_and_events() {
    let db = db().await;
    let cid = insert_company(&db, "breakdown").await;
    let parent = insert_case(&db, cid, None, "draft").await;
    let children = vec![
        NewBreakdownChild {
            title: "child-a".to_owned(),
            case_type: None,
            summary: None,
            fields: None,
        },
        NewBreakdownChild {
            title: "child-b".to_owned(),
            case_type: Some("bug".to_owned()),
            summary: Some("second child".to_owned()),
            fields: None,
        },
    ];
    let ids = CaseRepo::new(&db)
        .breakdown_case(cid, parent, None, "requirement", children, Some("via test"))
        .await
        .expect("breakdown");
    assert_eq!(ids.len(), 2);
    for id in &ids {
        let row = CaseRepo::new(&db)
            .get(*id)
            .await
            .expect("get child")
            .unwrap();
        assert_eq!(row.parent_case_id, Some(parent));
        assert_eq!(row.status, "draft");
    }
}

/// 2. breakdown_case — 空 children 返回空数组（不调用 tx）
#[tokio::test(flavor = "current_thread")]
async fn breakdown_case_empty_returns_empty() {
    let db = db().await;
    let cid = insert_company(&db, "breakdown-empty").await;
    let parent = insert_case(&db, cid, None, "draft").await;
    let ids = CaseRepo::new(&db)
        .breakdown_case(cid, parent, None, "requirement", vec![], None)
        .await
        .expect("breakdown empty");
    assert!(ids.is_empty());
}

/// 3. replace_blockers — 清空 + 重插，事件单独写入
#[tokio::test(flavor = "current_thread")]
async fn replace_blockers_replaces_set() {
    let db = db().await;
    let cid = insert_company(&db, "blockers").await;
    let case = insert_case(&db, cid, None, "draft").await;
    let blocker_a = insert_case(&db, cid, None, "draft").await;
    let blocker_b = insert_case(&db, cid, None, "draft").await;
    CaseRepo::new(&db)
        .replace_blockers(cid, case, vec![blocker_a, blocker_b], json!({"test": true}))
        .await
        .expect("replace");
    // Verify
    let rows: Vec<(Uuid,)> =
        sqlx::query_as("SELECT case_id FROM pipeline_case_blockers WHERE case_id = $1")
            .bind(case)
            .fetch_all(db.pool())
            .await
            .expect("list blockers");
    assert_eq!(rows.len(), 2);
}

/// 4. replace_blockers — 跳过 self-blocker
#[tokio::test(flavor = "current_thread")]
async fn replace_blockers_skips_self() {
    let db = db().await;
    let cid = insert_company(&db, "blockers-self").await;
    let case = insert_case(&db, cid, None, "draft").await;
    CaseRepo::new(&db)
        .replace_blockers(cid, case, vec![case], json!({}))
        .await
        .expect("replace");
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint FROM pipeline_case_blockers WHERE case_id = $1",
    )
    .bind(case)
    .fetch_one(db.pool())
    .await
    .expect("count");
    assert_eq!(count, 0);
}

/// 5. open_conversation — 创建新 issue + link + event
#[tokio::test(flavor = "current_thread")]
async fn open_conversation_creates_issue_and_link() {
    let db = db().await;
    let cid = insert_company(&db, "conv-new").await;
    let case = insert_case(&db, cid, None, "draft").await;
    let issue_id = CaseRepo::new(&db)
        .open_conversation(cid, case, "Test Case", None, Some("hello"))
        .await
        .expect("open conversation");
    assert!(!issue_id.is_nil());
    let links: i64 =
        sqlx::query_scalar("SELECT count(*)::bigint FROM case_issue_links WHERE case_id = $1")
            .bind(case)
            .fetch_one(db.pool())
            .await
            .expect("count links");
    assert_eq!(links, 1);
}

/// 6. open_conversation — 复用已存在的 issue
#[tokio::test(flavor = "current_thread")]
async fn open_conversation_reuses_existing_issue() {
    let db = db().await;
    let cid = insert_company(&db, "conv-exist").await;
    let case = insert_case(&db, cid, None, "draft").await;
    let existing = insert_issue(&db, cid).await;
    let returned = CaseRepo::new(&db)
        .open_conversation(cid, case, "Test", Some(existing), None)
        .await
        .expect("open");
    assert_eq!(returned, existing);
}

/// 7. count_children
#[tokio::test(flavor = "current_thread")]
async fn count_children_returns_count() {
    let db = db().await;
    let cid = insert_company(&db, "count").await;
    let parent = insert_case(&db, cid, None, "draft").await;
    insert_case(&db, cid, Some(parent), "draft").await;
    insert_case(&db, cid, Some(parent), "draft").await;
    insert_case(&db, cid, Some(parent), "draft").await;
    let count = CaseRepo::new(&db)
        .count_children(cid, parent)
        .await
        .expect("count");
    assert_eq!(count, 3);
}

/// 8. list_context_events + list_context_issues
#[tokio::test(flavor = "current_thread")]
async fn list_context_events_and_issues() {
    let db = db().await;
    let cid = insert_company(&db, "ctx").await;
    let case = insert_case(&db, cid, None, "draft").await;
    let issue = insert_issue(&db, cid).await;
    insert_event(&db, cid, case, "status_changed", json!({"to": "in_review"})).await;
    insert_event(&db, cid, case, "fields_changed", json!({"note": "test"})).await;
    sqlx::query("INSERT INTO case_issue_links (id, company_id, case_id, issue_id, role) VALUES ($1, $2, $3, $4, 'reference')")
        .bind(Uuid::new_v4()).bind(cid).bind(case).bind(issue)
        .execute(db.pool()).await.expect("link issue");
    let events = CaseRepo::new(&db)
        .list_context_events(cid, case)
        .await
        .expect("events");
    assert_eq!(events.len(), 2);
    let issues = CaseRepo::new(&db)
        .list_context_issues(cid, case)
        .await
        .expect("issues");
    assert_eq!(issues.len(), 1);
}

/// 9. list_outputs
#[tokio::test(flavor = "current_thread")]
async fn list_outputs_returns_outputs() {
    let db = db().await;
    let cid = insert_company(&db, "outputs").await;
    let case = insert_case(&db, cid, None, "draft").await;
    let issue = insert_issue(&db, cid).await;
    sqlx::query("INSERT INTO case_issue_links (id, company_id, case_id, issue_id, role) VALUES ($1, $2, $3, $4, 'reference')")
        .bind(Uuid::new_v4()).bind(cid).bind(case).bind(issue)
        .execute(db.pool()).await.expect("link");
    let rows = CaseRepo::new(&db)
        .list_outputs(cid, case)
        .await
        .expect("outputs");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].link_role, "reference");
}
