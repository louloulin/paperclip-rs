//! Recovery action 的真实 PostgreSQL upsert 语义。
use pc_db::Db;
use pc_repos::issue::{IssueRepo, UpsertRecoveryAction};
use serde_json::json;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";
async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}
async fn fixture(db: &Db) -> (Uuid, Uuid) {
    let company_id = Uuid::new_v4();
    let issue_id = Uuid::new_v4();
    let prefix = format!("R{}", &company_id.simple().to_string()[..8]);
    sqlx::query("INSERT INTO companies (id,name,issue_prefix) VALUES ($1,$2,$3)")
        .bind(company_id)
        .bind(format!("r289-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO issues (id,company_id,title,status,priority,origin_kind,origin_fingerprint) VALUES ($1,$2,'recovery fixture','blocked','normal','system',$3)").bind(issue_id).bind(company_id).bind(format!("r289-fp-{issue_id}")).execute(db.pool()).await.unwrap();
    (company_id, issue_id)
}
fn input(company_id: Uuid, issue_id: Uuid, fingerprint: &str) -> UpsertRecoveryAction {
    UpsertRecoveryAction {
        company_id,
        source_issue_id: issue_id,
        recovery_issue_id: None,
        kind: "configuration_validation".into(),
        owner_type: Some("board".into()),
        owner_agent_id: None,
        owner_user_id: None,
        previous_owner_agent_id: None,
        return_owner_agent_id: None,
        cause: "configuration_incomplete".into(),
        fingerprint: fingerprint.into(),
        evidence: Some(json!({"test":true})),
        next_action: "repair configuration".into(),
        wake_policy: Some(json!({"type":"manual_repair_required"})),
        monitor_policy: None,
        max_attempts: None,
        timeout_at: None,
        last_attempt_at: None,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn upsert_creates_then_increments_active_action() {
    let db = db().await;
    let (company_id, issue_id) = fixture(&db).await;
    let first = IssueRepo::new(&db)
        .upsert_recovery_action(&input(company_id, issue_id, "fp-a"))
        .await
        .unwrap();
    assert_eq!(first.attempt_count, 1);
    assert_eq!(first.status, "active");
    let second = IssueRepo::new(&db)
        .upsert_recovery_action(&input(company_id, issue_id, "fp-a"))
        .await
        .unwrap();
    assert_eq!(second.id, first.id);
    assert_eq!(second.attempt_count, 2);
    assert_eq!(second.fingerprint, "fp-a");
    sqlx::query("DELETE FROM issues WHERE company_id=$1")
        .bind(company_id)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM companies WHERE id=$1")
        .bind(company_id)
        .execute(db.pool())
        .await
        .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn open_human_query_returns_active_board_action() {
    let db = db().await;
    let (company_id, issue_id) = fixture(&db).await;
    IssueRepo::new(&db)
        .upsert_recovery_action(&input(company_id, issue_id, "fp-b"))
        .await
        .unwrap();
    let rows = IssueRepo::new(&db)
        .list_open_human_recovery_actions(company_id)
        .await
        .unwrap();
    assert!(rows
        .iter()
        .any(|row| row.source_issue_id == issue_id && row.owner_type == "board"));
    sqlx::query("DELETE FROM issues WHERE company_id=$1")
        .bind(company_id)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM companies WHERE id=$1")
        .bind(company_id)
        .execute(db.pool())
        .await
        .unwrap();
}
