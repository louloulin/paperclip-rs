//! Round 195 集成测试：approval request_revision。
//!
//! 覆盖：
//! - `ApprovalRepo::request_revision` — pending → revision_requested 状态机
//! - 仅允许 pending → revision_requested
//! - 已 terminal 状态（approved/rejected/cancelled/expired/revision_requested）拒绝再 revision

use pc_db::Db;
use pc_repos::approval::ApprovalRepo;
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
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(format!("r195-{tag}-{id}"))
        .bind(format!("R195{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("company");
    id
}

async fn insert_approval(
    db: &Db,
    company_id: Uuid,
    status: &str,
    requested_by_user_id: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO approvals (id, company_id, type, requested_by_user_id, payload, status) \
         VALUES ($1, $2, 'cost_budget', $3, $4, $5)",
    )
    .bind(id)
    .bind(company_id)
    .bind(requested_by_user_id)
    .bind(json!({"amount_cents": 100_000}))
    .bind(status)
    .execute(db.pool())
    .await
    .expect("approval");
    id
}

// ===== 1) request_revision: pending → revision_requested =====
#[tokio::test(flavor = "current_thread")]
async fn request_revision_from_pending_succeeds() {
    let db = db().await;
    let cid = insert_company(&db, "rv-ok").await;
    let aid = insert_approval(&db, cid, "pending", "alice").await;
    let repo = ApprovalRepo::new(&db);

    let row = repo
        .request_revision(aid, "board", Some("need more details"))
        .await
        .expect("rev")
        .expect("exists");
    assert_eq!(row.status, "revision_requested");
    assert_eq!(row.decision_note.as_deref(), Some("need more details"));
    assert_eq!(row.decided_by_user_id.as_deref(), Some("board"));
    assert!(row.decided_at.is_some(), "decided_at must be set");
}

// ===== 2) request_revision: terminal status → None =====
#[tokio::test(flavor = "current_thread")]
async fn request_revision_rejects_already_approved() {
    let db = db().await;
    let cid = insert_company(&db, "rv-appr").await;
    let aid = insert_approval(&db, cid, "approved", "alice").await;
    let repo = ApprovalRepo::new(&db);

    let row = repo
        .request_revision(aid, "board", None)
        .await
        .expect("rev");
    assert!(row.is_none(), "approved approvals must not be revisable");
}

#[tokio::test(flavor = "current_thread")]
async fn request_revision_rejects_already_rejected() {
    let db = db().await;
    let cid = insert_company(&db, "rv-rej").await;
    let aid = insert_approval(&db, cid, "rejected", "alice").await;
    let repo = ApprovalRepo::new(&db);

    let row = repo
        .request_revision(aid, "board", None)
        .await
        .expect("rev");
    assert!(row.is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn request_revision_rejects_cancelled() {
    let db = db().await;
    let cid = insert_company(&db, "rv-can").await;
    let aid = insert_approval(&db, cid, "cancelled", "alice").await;
    let repo = ApprovalRepo::new(&db);

    let row = repo
        .request_revision(aid, "board", None)
        .await
        .expect("rev");
    assert!(row.is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn request_revision_rejects_already_revision_requested() {
    let db = db().await;
    let cid = insert_company(&db, "rv-rev").await;
    let aid = insert_approval(&db, cid, "revision_requested", "alice").await;
    let repo = ApprovalRepo::new(&db);

    let row = repo
        .request_revision(aid, "board", None)
        .await
        .expect("rev");
    assert!(row.is_none(), "double revision must be rejected");
}

// ===== 3) request_revision: missing approval → None =====
#[tokio::test(flavor = "current_thread")]
async fn request_revision_missing_returns_none() {
    let db = db().await;
    let cid = insert_company(&db, "rv-mis").await;
    let repo = ApprovalRepo::new(&db);

    let row = repo
        .request_revision(Uuid::new_v4(), "board", None)
        .await
        .expect("rev");
    assert!(row.is_none());
}

// ===== 4) request_revision: optional note =====
#[tokio::test(flavor = "current_thread")]
async fn request_revision_with_null_note() {
    let db = db().await;
    let cid = insert_company(&db, "rv-null").await;
    let aid = insert_approval(&db, cid, "pending", "alice").await;
    let repo = ApprovalRepo::new(&db);

    let row = repo
        .request_revision(aid, "board", None)
        .await
        .expect("rev")
        .expect("exists");
    assert_eq!(row.status, "revision_requested");
    assert!(row.decision_note.is_none());
}
