//! Round 139 集成测试：IssueDiagnosticsRepo — issues.rs diagnostics 子模块仓储化。
//!
//! 覆盖：
//! - list_blockers / assignee_agent_id / list_wake_requests_for_agent / list_subtree

use pc_db::Db;
use pc_repos::issue_diagnostics::IssueDiagnosticsRepo;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(id)
        .bind(format!("r139-{tag}-{id}"))
        .bind(format!("R139{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("company");
    id
}

async fn insert_agent(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO agents (id, company_id, name, kind, status, owner_user_id) VALUES ($1,$2,'a','assistant','active','tester')")
        .bind(id).bind(company_id)
        .execute(db.pool()).await.expect("agent");
    id
}

async fn insert_issue(
    db: &Db,
    company_id: Uuid,
    parent_id: Option<Uuid>,
    status: &str,
    assignee: Option<Uuid>,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO issues (id, company_id, identifier, title, kind, status, priority, parent_id, assignee_agent_id) VALUES ($1,$2,$3,'i','task',$4,'normal',$5,$6)")
        .bind(id).bind(company_id).bind(format!("ISS-{}", &id.simple().to_string()[..6]))
        .bind(status).bind(parent_id).bind(assignee)
        .execute(db.pool()).await.expect("issue");
    id
}

async fn insert_wake_request(db: &Db, company_id: Uuid, agent_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO agent_wakeup_requests (id, company_id, agent_id, source, status) VALUES ($1, $2, $3, 'test', 'queued')")
        .bind(id).bind(company_id).bind(agent_id)
        .execute(db.pool()).await.expect("wake");
    id
}

// ===== IssueDiagnosticsRepo::list_blockers =====

/// 1. list_blockers — 无 blocker 时返回空集合。
#[tokio::test(flavor = "current_thread")]
async fn list_blockers_empty() {
    let db = db().await;
    let cid = insert_company(&db, "be").await;
    let iid = insert_issue(&db, cid, None, "todo", None).await;
    let list = IssueDiagnosticsRepo::new(&db)
        .list_blockers(iid, 100)
        .await
        .expect("list");
    assert!(list.is_empty());
}

/// 2. list_blockers — 包含 issue 自身（若 status='blocked'）。
#[tokio::test(flavor = "current_thread")]
async fn list_blockers_includes_self() {
    let db = db().await;
    let cid = insert_company(&db, "bs").await;
    let iid = insert_issue(&db, cid, None, "blocked", None).await;
    let list = IssueDiagnosticsRepo::new(&db)
        .list_blockers(iid, 100)
        .await
        .expect("list");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, iid);
}

/// 3. list_blockers — 包含 children（parent_id = root）。
#[tokio::test(flavor = "current_thread")]
async fn list_blockers_includes_children() {
    let db = db().await;
    let cid = insert_company(&db, "bc").await;
    let parent = insert_issue(&db, cid, None, "todo", None).await;
    let child = insert_issue(&db, cid, Some(parent), "blocked", None).await;
    let list = IssueDiagnosticsRepo::new(&db)
        .list_blockers(parent, 100)
        .await
        .expect("list");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, child);
}

/// 4. list_blockers — 排除 status='todo' 的子 issues。
#[tokio::test(flavor = "current_thread")]
async fn list_blockers_filters_status() {
    let db = db().await;
    let cid = insert_company(&db, "bf").await;
    let parent = insert_issue(&db, cid, None, "todo", None).await;
    insert_issue(&db, cid, Some(parent), "todo", None).await;
    insert_issue(&db, cid, Some(parent), "in_progress", None).await;
    let list = IssueDiagnosticsRepo::new(&db)
        .list_blockers(parent, 100)
        .await
        .expect("list");
    assert!(list.is_empty(), "todo + in_progress should not be blockers");
}

// ===== IssueDiagnosticsRepo::assignee_agent_id =====

/// 5. assignee_agent_id — 存在时返回 Some。
#[tokio::test(flavor = "current_thread")]
async fn assignee_agent_id_some() {
    let db = db().await;
    let cid = insert_company(&db, "aa").await;
    let aid = insert_agent(&db, cid).await;
    let iid = insert_issue(&db, cid, None, "todo", Some(aid)).await;
    let r = IssueDiagnosticsRepo::new(&db)
        .assignee_agent_id(iid)
        .await
        .expect("ok");
    assert_eq!(r, Some(aid));
}

/// 6. assignee_agent_id — 无 assignee 返回 None。
#[tokio::test(flavor = "current_thread")]
async fn assignee_agent_id_none() {
    let db = db().await;
    let cid = insert_company(&db, "an").await;
    let iid = insert_issue(&db, cid, None, "todo", None).await;
    let r = IssueDiagnosticsRepo::new(&db)
        .assignee_agent_id(iid)
        .await
        .expect("ok");
    assert!(r.is_none());
}

// ===== IssueDiagnosticsRepo::list_wake_requests_for_agent =====

/// 7. list_wake_requests_for_agent — 按 agent 过滤。
#[tokio::test(flavor = "current_thread")]
async fn list_wake_requests_filters_by_agent() {
    let db = db().await;
    let cid = insert_company(&db, "wf").await;
    let a1 = insert_agent(&db, cid).await;
    let a2 = insert_agent(&db, cid).await;
    let iid = insert_issue(&db, cid, None, "todo", Some(a1)).await;
    insert_wake_request(&db, cid, a1).await;
    insert_wake_request(&db, cid, a1).await;
    insert_wake_request(&db, cid, a2).await;
    let list = IssueDiagnosticsRepo::new(&db)
        .list_wake_requests_for_agent(iid, a1, 100)
        .await
        .expect("list");
    assert_eq!(list.len(), 2);
}

/// 8. list_wake_requests_for_agent — limit 生效。
#[tokio::test(flavor = "current_thread")]
async fn list_wake_requests_respects_limit() {
    let db = db().await;
    let cid = insert_company(&db, "wl").await;
    let aid = insert_agent(&db, cid).await;
    let iid = insert_issue(&db, cid, None, "todo", Some(aid)).await;
    for _ in 0..5 {
        insert_wake_request(&db, cid, aid).await;
    }
    let list = IssueDiagnosticsRepo::new(&db)
        .list_wake_requests_for_agent(iid, aid, 3)
        .await
        .expect("list");
    assert_eq!(list.len(), 3);
}

// ===== IssueDiagnosticsRepo::list_subtree =====

/// 9. list_subtree — 根节点返回自身（depth=0）。
#[tokio::test(flavor = "current_thread")]
async fn list_subtree_root_only() {
    let db = db().await;
    let cid = insert_company(&db, "sr").await;
    let iid = insert_issue(&db, cid, None, "todo", None).await;
    let list = IssueDiagnosticsRepo::new(&db)
        .list_subtree(iid, 8)
        .await
        .expect("list");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].depth, 0);
}

/// 10. list_subtree — 包含 children（递归）。
#[tokio::test(flavor = "current_thread")]
async fn list_subtree_recursive() {
    let db = db().await;
    let cid = insert_company(&db, "sc").await;
    let root = insert_issue(&db, cid, None, "todo", None).await;
    let child1 = insert_issue(&db, cid, Some(root), "todo", None).await;
    let child2 = insert_issue(&db, cid, Some(root), "todo", None).await;
    let grand = insert_issue(&db, cid, Some(child1), "todo", None).await;
    let list = IssueDiagnosticsRepo::new(&db)
        .list_subtree(root, 8)
        .await
        .expect("list");
    assert_eq!(list.len(), 4);
    let ids: Vec<_> = list.iter().map(|n| n.id).collect();
    assert!(ids.contains(&root));
    assert!(ids.contains(&child1));
    assert!(ids.contains(&child2));
    assert!(ids.contains(&grand));
}

/// 11. list_subtree — max_depth 限制递归层数。
#[tokio::test(flavor = "current_thread")]
async fn list_subtree_respects_max_depth() {
    let db = db().await;
    let cid = insert_company(&db, "sd").await;
    let root = insert_issue(&db, cid, None, "todo", None).await;
    let child = insert_issue(&db, cid, Some(root), "todo", None).await;
    insert_issue(&db, cid, Some(child), "todo", None).await;
    // max_depth=1 → root + child（不含 grandchild）
    let list = IssueDiagnosticsRepo::new(&db)
        .list_subtree(root, 1)
        .await
        .expect("list");
    assert_eq!(list.len(), 2);
}
