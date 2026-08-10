use std::sync::Arc;
use pc_issue_approvals::{IssueApprovalHookEvent, IssueApprovalLinkActor, IssueApprovalService, RecordingIssueApprovalHook};
use pc_repos::Db;
use sqlx::PgPool;
use uuid::Uuid;

const URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";
static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn setup() -> (Db, PgPool) {
    let p = sqlx::postgres::PgPoolOptions::new().max_connections(4).connect(URL).await.unwrap();
    (Db::connect(URL, 4, 1).await.unwrap(), p)
}
async fn company(p: &PgPool) -> Uuid {
    let id = Uuid::new_v4();
    let prefix = format!("IA{}", &id.simple().to_string()[..6]);
    sqlx::query("INSERT INTO companies (id,name,status,issue_prefix,created_at,updated_at) VALUES ($1,$2,'active',$3,now(),now())")
        .bind(id).bind(format!("ia-{id}")).bind(prefix).execute(p).await.unwrap();
    id
}
async fn seed_issue_and_approval(p: &PgPool, company_id: Uuid) -> (Uuid, Uuid) {
    let issue_id = Uuid::new_v4();
    sqlx::query("INSERT INTO issues (id, company_id, title, status, priority, created_at, updated_at) VALUES ($1,$2,$3,'todo','normal',now(),now())")
        .bind(issue_id).bind(company_id).bind(format!("pc-ia-{issue_id}")).execute(p).await.unwrap();
    let approval_id = Uuid::new_v4();
    sqlx::query("INSERT INTO approvals (id, company_id, type, status, payload, created_at, updated_at) VALUES ($1,$2,'hire_agent','pending','{}'::jsonb,now(),now())")
        .bind(approval_id).bind(company_id).execute(p).await.unwrap();
    (issue_id, approval_id)
}
async fn cleanup(p: &PgPool, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM issue_approvals WHERE company_id=$1").bind(company_id).execute(p).await;
    let _ = sqlx::query("DELETE FROM approvals WHERE company_id=$1").bind(company_id).execute(p).await;
    let _ = sqlx::query("DELETE FROM issues WHERE company_id=$1").bind(company_id).execute(p).await;
    let _ = sqlx::query("DELETE FROM companies WHERE id=$1").bind(company_id).execute(p).await;
}

#[tokio::test(flavor = "current_thread")]
async fn link_unlink_and_list_against_real_db() {
    let _g = LOCK.lock().await;
    let (db, p) = setup().await;
    let cid = company(&p).await;
    let (issue_id, approval_id) = seed_issue_and_approval(&p, cid).await;
    let h = Arc::new(RecordingIssueApprovalHook::default());
    let s = IssueApprovalService::with_hooks(db, vec![h.clone()]);
    s.link(issue_id, approval_id, Some(IssueApprovalLinkActor { agent_id: None, user_id: Some("u-tester".into()) })).await.unwrap();
    let aps = s.list_approvals_for_issue(issue_id).await.unwrap();
    assert!(aps.iter().any(|a| a.id == approval_id));
    let issues = s.list_issues_for_approval(approval_id).await.unwrap();
    assert!(issues.iter().any(|i| i.id == issue_id));
    s.unlink(issue_id, approval_id).await.unwrap();
    let aps2 = s.list_approvals_for_issue(issue_id).await.unwrap();
    assert!(!aps2.iter().any(|a| a.id == approval_id));
    let snapshot = h.events_snapshot();
    assert!(snapshot.iter().any(|e| matches!(e, IssueApprovalHookEvent::Linked { .. })));
    assert!(snapshot.iter().any(|e| matches!(e, IssueApprovalHookEvent::Unlinked { .. })));
    cleanup(&p, cid).await;
}

#[tokio::test(flavor = "current_thread")]
async fn link_many_rejects_empty() {
    let _g = LOCK.lock().await;
    let (db, _p) = setup().await;
    let s = IssueApprovalService::new(db);
    assert!(s.link_many(Uuid::new_v4(), vec![]).await.is_err());
    assert!(s.link_many(Uuid::nil(), vec![Uuid::new_v4()]).await.is_err());
    assert!(s.list_approvals_for_issue(Uuid::nil()).await.is_err());
}
