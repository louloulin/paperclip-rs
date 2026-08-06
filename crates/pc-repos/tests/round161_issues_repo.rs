//! Round 161 集成测试：issues.rs 仓储化扩展 — IssueRepo +9 方法 / HeartbeatRepo +2 方法。

use pc_db::Db;
use pc_repos::heartbeat::HeartbeatRepo;
use pc_repos::issue::IssueRepo;
use serde_json::json;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(format!("r161-{tag}-{id}"))
        .bind(format!("R161{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("company");
    id
}

async fn insert_issue(db: &Db, company_id: Uuid, status: &str, priority: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, case_number, identifier, case_type, title, status, priority) \
         VALUES ($1, $2, 161, $3, 'task', 'r161-issue', $4, $5)",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("r161-i-{id}"))
    .bind(status)
    .bind(priority)
    .execute(db.pool())
    .await
    .expect("issue");
    id
}

async fn insert_project(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO projects (id, company_id, source_type, name) VALUES ($1, $2, 'internal', 'r161-p')")
        .bind(id)
        .bind(company_id)
        .execute(db.pool())
        .await
        .expect("project");
    id
}

// ===== IssueRepo 新方法 =====

/// 1. heartbeat_context_inputs — 6-tuple。
#[tokio::test(flavor = "current_thread")]
async fn heartbeat_context_inputs_basic() {
    let db = db().await;
    let cid = insert_company(&db, "hci1").await;
    let aid = Uuid::new_v4(); // agent
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, kind, status) VALUES ($1, $2, 'r161-agent', 'general', 'active')",
    )
    .bind(aid)
    .bind(cid)
    .execute(db.pool())
    .await
    .expect("agent");

    let pid = insert_project(&db, cid).await;
    let pwid = Uuid::new_v4();
    sqlx::query("INSERT INTO project_workspaces (id, company_id, project_id, name, source_type, cwd, is_primary) VALUES ($1, $2, $3, 'pw', 'local_path', '/x', false)")
        .bind(pwid)
        .bind(cid)
        .bind(pid)
        .execute(db.pool())
        .await
        .expect("pw");

    let iid = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, case_number, identifier, case_type, title, status, priority, project_id, project_workspace_id, assignee_agent_id, work_mode) \
         VALUES ($1, $2, 991, 'r161-i-c', 'task', 'ctx-test', 'in_progress', 'medium', $3, $4, $5, 'heartbeat')",
    )
    .bind(iid)
    .bind(cid)
    .bind(pid)
    .bind(pwid)
    .bind(aid)
    .execute(db.pool())
    .await
    .expect("issue");

    let repo = IssueRepo::new(&db);
    let row = repo.heartbeat_context_inputs(iid).await.expect("get");
    assert!(row.is_some());
    let (c, a, p, pw, st, wm) = row.unwrap();
    assert_eq!(c, cid);
    assert_eq!(a, Some(aid));
    assert_eq!(p, Some(pid));
    assert_eq!(pw, Some(pwid));
    assert_eq!(st, "in_progress");
    assert_eq!(wm, "heartbeat");
}

/// 2. list_company_basic — 基本 5-tuple + limit。
#[tokio::test(flavor = "current_thread")]
async fn list_company_basic_basic() {
    let db = db().await;
    let cid = insert_company(&db, "lcb1").await;
    let _ = insert_issue(&db, cid, "open", "medium").await;
    let _ = insert_issue(&db, cid, "open", "high").await;
    let repo = IssueRepo::new(&db);
    let rows = repo.list_company_basic(cid, 100).await.expect("list");
    assert!(rows.len() >= 2);
}

/// 3. start_run_inputs — 3-tuple。
#[tokio::test(flavor = "current_thread")]
async fn start_run_inputs_basic() {
    let db = db().await;
    let cid = insert_company(&db, "sri1").await;
    let aid = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, kind, status) VALUES ($1, $2, 'r161-sri', 'general', 'active')",
    )
    .bind(aid)
    .bind(cid)
    .execute(db.pool())
    .await
    .expect("agent");
    let pid = insert_project(&db, cid).await;

    let iid = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, case_number, identifier, case_type, title, status, priority, project_id, assignee_agent_id) \
         VALUES ($1, $2, 992, 'r161-sri-i', 'task', 'start-run', 'open', 'medium', $3, $4)",
    )
    .bind(iid)
    .bind(cid)
    .bind(pid)
    .bind(aid)
    .execute(db.pool())
    .await
    .expect("issue");

    let repo = IssueRepo::new(&db);
    let row = repo.start_run_inputs(iid).await.expect("get");
    assert!(row.is_some());
    let (c, p, a) = row.unwrap();
    assert_eq!(c, cid);
    assert_eq!(p, pid);
    assert_eq!(a, Some(aid));
}

/// 4. find_one_comment — 查 issue_comment（限定 issue + 未删除）。
#[tokio::test(flavor = "current_thread")]
async fn find_one_comment_basic() {
    let db = db().await;
    let cid = insert_company(&db, "foc1").await;
    let iid = insert_issue(&db, cid, "open", "medium").await;
    let cid_c = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issue_comments (id, issue_id, author_user_id, body) VALUES ($1, $2, 'tester', 'body-x')",
    )
    .bind(cid_c)
    .bind(iid)
    .execute(db.pool())
    .await
    .expect("comment");

    let repo = IssueRepo::new(&db);
    let hit = repo.find_one_comment(iid, cid_c).await.expect("hit");
    assert!(hit.is_some());

    // miss
    let miss = repo
        .find_one_comment(iid, Uuid::new_v4())
        .await
        .expect("miss");
    assert!(miss.is_none());
}

/// 5. issue_doc_exists — issue_document 是否存在。
#[tokio::test(flavor = "current_thread")]
async fn issue_doc_exists_basic() {
    let db = db().await;
    let cid = insert_company(&db, "ide1").await;
    let iid = insert_issue(&db, cid, "open", "medium").await;
    let repo = IssueRepo::new(&db);
    let miss = repo
        .issue_doc_exists(iid, "missing-key")
        .await
        .expect("miss");
    assert!(!miss);

    sqlx::query(
        "INSERT INTO issue_documents (issue_id, key, content, title) VALUES ($1, $2, '\"x\"'::jsonb, 't')",
    )
    .bind(iid)
    .bind("existing-key")
    .execute(db.pool())
    .await
    .expect("doc");

    let hit = repo
        .issue_doc_exists(iid, "existing-key")
        .await
        .expect("hit");
    assert!(hit);
}

/// 6. update_issue_doc_content + insert_issue_doc + soft_delete + set_current_revision。
#[tokio::test(flavor = "current_thread")]
async fn issue_doc_lifecycle() {
    let db = db().await;
    let cid = insert_company(&db, "idl1").await;
    let iid = insert_issue(&db, cid, "open", "medium").await;
    let repo = IssueRepo::new(&db);

    // insert
    let ok = repo
        .insert_issue_doc(iid, "k1", &json!("v1"), Some("title-1"))
        .await
        .expect("insert");
    assert!(ok);

    // update
    let upd = repo
        .update_issue_doc_content(iid, "k1", &json!("v2"))
        .await
        .expect("update");
    assert!(upd);

    // exists
    assert!(repo.issue_doc_exists(iid, "k1").await.expect("exists"));

    // set_revision
    let rid = Uuid::new_v4();
    let n = repo
        .set_issue_doc_current_revision(iid, "k1", rid)
        .await
        .expect("set_rev");
    assert!(n > 0);

    // soft delete
    let del = repo.soft_delete_issue_doc(iid, "k1").await.expect("del");
    assert!(del);
}

/// 7. attachment_content_meta — JOIN attachments + assets。
#[tokio::test(flavor = "current_thread")]
async fn attachment_content_meta_basic() {
    let db = db().await;
    let cid = insert_company(&db, "acm1").await;
    let iid = insert_issue(&db, cid, "open", "medium").await;
    let asset_id = Uuid::new_v4();
    let provider = "local_fs";
    sqlx::query(
        "INSERT INTO assets (id, company_id, provider, object_key, content_type, byte_size, original_filename) \
         VALUES ($1, $2, $3, 'obj/key/x', 'text/plain', 100, 'sample.txt')",
    )
    .bind(asset_id)
    .bind(cid)
    .bind(provider)
    .execute(db.pool())
    .await
    .expect("asset");

    let att_id = Uuid::new_v4();
    sqlx::query("INSERT INTO issue_attachments (id, issue_id, asset_id, company_id) VALUES ($1, $2, $3, $4)")
        .bind(att_id)
        .bind(iid)
        .bind(asset_id)
        .bind(cid)
        .execute(db.pool())
        .await
        .expect("attachment");

    let repo = IssueRepo::new(&db);
    let hit = repo.attachment_content_meta(att_id).await.expect("hit");
    assert!(hit.is_some());
    let row = hit.unwrap();
    assert_eq!(row.0, cid);
    assert_eq!(row.1, "local_fs");

    let miss = repo
        .attachment_content_meta(Uuid::new_v4())
        .await
        .expect("miss");
    assert!(miss.is_none());
}

// ===== HeartbeatRepo 新方法 =====

/// 8. recent_runs_for_issue — 空结果（无 heartbeat_run context_snapshot）。
#[tokio::test(flavor = "current_thread")]
async fn recent_runs_for_issue_basic() {
    let db = db().await;
    let _cid = insert_company(&db, "rrfi1").await;
    let repo = HeartbeatRepo::new(&db);
    let rows = repo
        .recent_runs_for_issue(Uuid::new_v4(), 5)
        .await
        .expect("get");
    let _ = rows;
}

/// 9. count_active_runs_for_issue — 0 个 active。
#[tokio::test(flavor = "current_thread")]
async fn count_active_runs_for_issue_basic() {
    let db = db().await;
    let _cid = insert_company(&db, "carfi1").await;
    let repo = HeartbeatRepo::new(&db);
    let n = repo
        .count_active_runs_for_issue(Uuid::new_v4())
        .await
        .expect("count");
    assert_eq!(n, 0);
}
