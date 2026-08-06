//! Pause-hold 抑制闸门的真实 PostgreSQL 集成测试。
//! 验证 `is_automatic_recovery_suppressed_by_pause_hold` 在 DB 上检测 ancestor 上的 active pause hold。
use pc_heartbeat::recovery::is_automatic_recovery_suppressed_by_pause_hold;
use pc_repos::Db;
use serde_json::json;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn connect() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn fixture_with_company(db: &Db) -> Uuid {
    let company_id = Uuid::new_v4();
    let prefix = format!("R{}", &company_id.simple().to_string()[..8]);
    sqlx::query("INSERT INTO companies (id,name,issue_prefix) VALUES ($1,$2,$3)")
        .bind(company_id)
        .bind(format!("r297-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    company_id
}

async fn insert_issue(db: &Db, company_id: Uuid, parent_id: Option<Uuid>) -> Uuid {
    let issue_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id,company_id,parent_id,title,status,priority,origin_kind,origin_fingerprint) VALUES ($1,$2,$3,'r297-issue','in_progress','normal','system',$4)",
    )
    .bind(issue_id)
    .bind(company_id)
    .bind(parent_id)
    .bind(format!("r297-fp-{issue_id}"))
    .execute(db.pool())
    .await
    .unwrap();
    issue_id
}

async fn insert_pause_hold(
    db: &Db,
    company_id: Uuid,
    root_issue_id: Uuid,
    mode: &str,
    status: &str,
) -> Uuid {
    let hold_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issue_tree_holds (id, company_id, root_issue_id, mode, status, reason, release_policy) VALUES ($1, $2, $3, $4, $5, 'r297-fixture', $6)",
    )
    .bind(hold_id)
    .bind(company_id)
    .bind(root_issue_id)
    .bind(mode)
    .bind(status)
    .bind(json!({}))
    .execute(db.pool())
    .await
    .unwrap();
    hold_id
}

async fn cleanup(db: &Db, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM issue_tree_holds WHERE company_id=$1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM issues WHERE company_id=$1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM companies WHERE id=$1")
        .bind(company_id)
        .execute(db.pool())
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn no_pause_hold_returns_none() {
    let db = connect().await;
    let company_id = fixture_with_company(&db).await;
    let issue_id = insert_issue(&db, company_id, None).await;

    let hit = is_automatic_recovery_suppressed_by_pause_hold(&db, company_id, issue_id)
        .await
        .unwrap();
    assert!(hit.is_none(), "no holds → no suppression");

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn self_pause_hold_suppresses() {
    let db = connect().await;
    let company_id = fixture_with_company(&db).await;
    let issue_id = insert_issue(&db, company_id, None).await;
    let _hold_id = insert_pause_hold(&db, company_id, issue_id, "pause", "active").await;

    let hit = is_automatic_recovery_suppressed_by_pause_hold(&db, company_id, issue_id)
        .await
        .unwrap()
        .expect("must be suppressed");
    assert!(hit.is_root);
    assert_eq!(hit.root_issue_id, issue_id);
    assert_eq!(hit.reason.as_deref(), Some("r297-fixture"));

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn ancestor_pause_hold_suppresses_via_parent_chain() {
    let db = connect().await;
    let company_id = fixture_with_company(&db).await;
    let parent_id = insert_issue(&db, company_id, None).await;
    let child_id = insert_issue(&db, company_id, Some(parent_id)).await;
    let grandchild_id = insert_issue(&db, company_id, Some(child_id)).await;
    let _hold_id = insert_pause_hold(&db, company_id, parent_id, "pause", "active").await;

    let hit = is_automatic_recovery_suppressed_by_pause_hold(&db, company_id, grandchild_id)
        .await
        .unwrap()
        .expect("must be suppressed");
    assert_eq!(hit.root_issue_id, parent_id);
    assert!(!hit.is_root);
    assert_eq!(hit.issue_id, grandchild_id);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn released_hold_does_not_suppress() {
    let db = connect().await;
    let company_id = fixture_with_company(&db).await;
    let issue_id = insert_issue(&db, company_id, None).await;
    let _hold_id = insert_pause_hold(&db, company_id, issue_id, "pause", "released").await;

    let hit = is_automatic_recovery_suppressed_by_pause_hold(&db, company_id, issue_id)
        .await
        .unwrap();
    assert!(hit.is_none(), "released hold must not suppress");

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn non_pause_hold_modes_do_not_suppress() {
    let db = connect().await;
    let company_id = fixture_with_company(&db).await;
    let issue_id = insert_issue(&db, company_id, None).await;
    // "stop" mode is NOT a pause hold, must not suppress
    let _hold_id = insert_pause_hold(&db, company_id, issue_id, "stop", "active").await;
    let _hold_id2 = insert_pause_hold(&db, company_id, issue_id, "throttle", "active").await;
    let _hold_id3 = insert_pause_hold(&db, company_id, issue_id, "isolate", "active").await;

    let hit = is_automatic_recovery_suppressed_by_pause_hold(&db, company_id, issue_id)
        .await
        .unwrap();
    assert!(hit.is_none(), "only mode='pause' should suppress recovery");

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn missing_issue_returns_none_without_panic() {
    let db = connect().await;
    let company_id = fixture_with_company(&db).await;
    let phantom_issue = Uuid::new_v4();

    let hit = is_automatic_recovery_suppressed_by_pause_hold(&db, company_id, phantom_issue)
        .await
        .unwrap();
    assert!(hit.is_none(), "missing issue chain resolves to None");

    cleanup(&db, company_id).await;
}
