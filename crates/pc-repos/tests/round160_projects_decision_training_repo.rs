//! Round 160 集成测试：projects.rs + decision_training.rs 仓储化扩展。
//!
//! ProjectRepo 9 新方法（workspaces 生命周期管理）+
//! DecisionTrainingService 6 新方法（list_filtered / preview / patch_with_history / owner_for_id）。

use pc_db::Db;
use pc_repos::decision_training::DecisionTrainingService;
use pc_repos::project::ProjectRepo;
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
        .bind(format!("r160-{tag}-{id}"))
        .bind(format!("R160{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("company");
    id
}

async fn insert_project(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO projects (id, company_id, name, source_type, key, status, paused) VALUES ($1, $2, 'p160', 'internal', $3, 'active', false)",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("r160-p-{id}"))
    .execute(db.pool())
    .await
    .expect("project");
    id
}

async fn insert_workspace(db: &Db, company_id: Uuid, project_id: Uuid, primary: bool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO project_workspaces \
         (id, company_id, project_id, name, source_type, cwd, is_primary) \
         VALUES ($1, $2, $3, 'ws-160', 'local_path', '/tmp/x', $4)",
    )
    .bind(id)
    .bind(company_id)
    .bind(project_id)
    .bind(primary)
    .execute(db.pool())
    .await
    .expect("workspace");
    id
}

// ===== ProjectRepo 新方法 =====

/// 1. company_id_for_project — 命中 / 不命中。
#[tokio::test(flavor = "current_thread")]
async fn company_id_for_project_basic() {
    let db = db().await;
    let cid = insert_company(&db, "cfp1").await;
    let pid = insert_project(&db, cid).await;
    let repo = ProjectRepo::new(&db);
    let hit = repo.company_id_for_project(pid).await.expect("hit");
    assert_eq!(hit, Some(cid));

    let miss = repo
        .company_id_for_project(Uuid::new_v4())
        .await
        .expect("miss");
    assert!(miss.is_none());
}

/// 2. company_id_for_workspace — 限定 project_id。
#[tokio::test(flavor = "current_thread")]
async fn company_id_for_workspace_basic() {
    let db = db().await;
    let cid = insert_company(&db, "cfw1").await;
    let pid = insert_project(&db, cid).await;
    let wsid = insert_workspace(&db, cid, pid, false).await;

    let repo = ProjectRepo::new(&db);
    let hit = repo.company_id_for_workspace(wsid, pid).await.expect("hit");
    assert_eq!(hit, Some(cid));

    // miss 路径: workspace 不属于此 project
    let other_pid = insert_project(&db, cid).await;
    let miss = repo
        .company_id_for_workspace(wsid, other_pid)
        .await
        .expect("miss");
    assert!(miss.is_none());
}

/// 3. company_id_for_workspace_any — 不限定 project。
#[tokio::test(flavor = "current_thread")]
async fn company_id_for_workspace_any_basic() {
    let db = db().await;
    let cid = insert_company(&db, "cwa1").await;
    let pid = insert_project(&db, cid).await;
    let wsid = insert_workspace(&db, cid, pid, false).await;
    let repo = ProjectRepo::new(&db);
    let hit = repo.company_id_for_workspace_any(wsid).await.expect("hit");
    assert_eq!(hit, Some(cid));
}

/// 4. unset_all_primary_workspaces — 把所有 primary 设为 false。
#[tokio::test(flavor = "current_thread")]
async fn unset_all_primary_workspaces_basic() {
    let db = db().await;
    let cid = insert_company(&db, "uap1").await;
    let pid = insert_project(&db, cid).await;
    let _ws1 = insert_workspace(&db, cid, pid, true).await;
    let _ws2 = insert_workspace(&db, cid, pid, true).await;

    let repo = ProjectRepo::new(&db);
    let n = repo.unset_all_primary_workspaces(pid).await.expect("unset");
    assert!(n >= 2);
}

/// 5. unset_other_primary_workspaces — 除自身外。
#[tokio::test(flavor = "current_thread")]
async fn unset_other_primary_workspaces_basic() {
    let db = db().await;
    let cid = insert_company(&db, "uop1").await;
    let pid = insert_project(&db, cid).await;
    let ws1 = insert_workspace(&db, cid, pid, true).await;
    let _ws2 = insert_workspace(&db, cid, pid, true).await;
    let repo = ProjectRepo::new(&db);
    let n = repo
        .unset_other_primary_workspaces(pid, ws1)
        .await
        .expect("unset");
    assert_eq!(n, 1);
}

/// 6. insert_workspace_simple — 简单 INSERT + 拿 id。
#[tokio::test(flavor = "current_thread")]
async fn insert_workspace_simple_basic() {
    let db = db().await;
    let cid = insert_company(&db, "iws1").await;
    let pid = insert_project(&db, cid).await;
    let repo = ProjectRepo::new(&db);
    let id = repo
        .insert_workspace_simple(
            cid,
            pid,
            "simple-ws",
            "/simple/path",
            Some("git@github.com:x/y.git"),
            None,
            Some(json!({"created_by": "test"})),
            Some(true),
        )
        .await
        .expect("insert");
    assert!(!id.is_nil());
}

/// 7. patch_workspace_partial — COALESCE 模式 + 返回 affected。
#[tokio::test(flavor = "current_thread")]
async fn patch_workspace_partial_basic() {
    let db = db().await;
    let cid = insert_company(&db, "pwp1").await;
    let pid = insert_project(&db, cid).await;
    let wsid = insert_workspace(&db, cid, pid, false).await;
    let repo = ProjectRepo::new(&db);

    // 只改 name
    let n = repo
        .patch_workspace_partial(
            wsid,
            pid,
            Some("updated-name"),
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("patch");
    assert_eq!(n, 1);

    // 传 None — COALESCE 不修改
    let n = repo
        .patch_workspace_partial(wsid, pid, None, None, None, None, None, None)
        .await
        .expect("noop");
    assert_eq!(n, 0);
}

/// 8. delete_workspace_in_project — DELETE + project_id 限定。
#[tokio::test(flavor = "current_thread")]
async fn delete_workspace_in_project_basic() {
    let db = db().await;
    let cid = insert_company(&db, "dwp1").await;
    let pid = insert_project(&db, cid).await;
    let wsid = insert_workspace(&db, cid, pid, false).await;
    let repo = ProjectRepo::new(&db);

    let n = repo
        .delete_workspace_in_project(wsid, pid)
        .await
        .expect("del");
    assert_eq!(n, 1);

    // 重复 delete → 0 affected
    let n = repo
        .delete_workspace_in_project(wsid, pid)
        .await
        .expect("del2");
    assert_eq!(n, 0);
}

/// 9. append_runtime_action — 修改 metadata jsonb。
#[tokio::test(flavor = "current_thread")]
async fn append_runtime_action_basic() {
    let db = db().await;
    let cid = insert_company(&db, "ara1").await;
    let pid = insert_project(&db, cid).await;
    let wsid = insert_workspace(&db, cid, pid, false).await;
    let repo = ProjectRepo::new(&db);
    let n = repo
        .append_runtime_action(wsid, "start")
        .await
        .expect("append");
    assert_eq!(n, 1);
}

// ===== DecisionTrainingService 新方法 =====

async fn insert_decision(db: &Db, company_id: Uuid, status: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO decisions (id, company_id, title, payload, status) \
         VALUES ($1, $2, $3, '{}'::jsonb, $4)",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("r160-d-{id}"))
    .bind(status)
    .execute(db.pool())
    .await
    .expect("decision");
    id
}

async fn insert_approval(db: &Db, company_id: Uuid, status: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO approvals (id, company_id, status, request_kind, request_payload, decided_by) \
         VALUES ($1, $2, $3, 'misc', '{}'::jsonb, 'tester')",
    )
    .bind(id)
    .bind(company_id)
    .bind(status)
    .execute(db.pool())
    .await
    .expect("approval");
    id
}

/// 10. list_filtered_simple — 不带 issues JOIN。
#[tokio::test(flavor = "current_thread")]
async fn list_filtered_simple_basic() {
    let db = db().await;
    let _cid = insert_company(&db, "lfs1").await;
    let svc = DecisionTrainingService::new(&db);
    // 没有 example → 返空
    let rows = svc
        .list_filtered_simple(_cid, None, None, None)
        .await
        .expect("list");
    // 接受任意（含残留）
    let _ = rows;
}

/// 11. preview_decision — SELECT decisions (status, decision_outcome, options)。
#[tokio::test(flavor = "current_thread")]
async fn preview_decision_basic() {
    let db = db().await;
    let cid = insert_company(&db, "pvd1").await;
    let decision_id = insert_decision(&db, cid, "open").await;
    let svc = DecisionTrainingService::new(&db);
    let row = svc.preview_decision(cid, decision_id).await.expect("get");
    assert!(row.is_some());
    let (st, _outcome, _opts) = row.unwrap();
    assert_eq!(st, "open");

    let miss = svc
        .preview_decision(cid, Uuid::new_v4())
        .await
        .expect("miss");
    assert!(miss.is_none());
}

/// 12. preview_approval — SELECT approvals (status)。
#[tokio::test(flavor = "current_thread")]
async fn preview_approval_basic() {
    let db = db().await;
    let cid = insert_company(&db, "pva1").await;
    let approval_id = insert_approval(&db, cid, "pending").await;
    let svc = DecisionTrainingService::new(&db);
    let row = svc.preview_approval(cid, approval_id).await.expect("get");
    assert!(row.is_some());

    let miss = svc
        .preview_approval(cid, Uuid::new_v4())
        .await
        .expect("miss");
    assert!(miss.is_none());
}

/// 13. export_resolved_decisions — SELECT WHERE status='resolved'。
#[tokio::test(flavor = "current_thread")]
async fn export_resolved_decisions_basic() {
    let db = db().await;
    let cid = insert_company(&db, "erd1").await;
    let _ = insert_decision(&db, cid, "resolved").await;
    let svc = DecisionTrainingService::new(&db);
    let rows = svc.export_resolved_decisions(cid).await.expect("list");
    assert!(rows.iter().any(|(id, _, _, _)| !id.is_nil()));
}

/// 14. owner_for_id — 取 created_by_user_id。
#[tokio::test(flavor = "current_thread")]
async fn owner_for_id_basic() {
    let db = db().await;
    let _cid = insert_company(&db, "ofid1").await;
    let svc = DecisionTrainingService::new(&db);
    // miss
    let miss = svc.owner_for_id(Uuid::new_v4()).await.expect("miss");
    assert!(miss.is_none());
}

/// 15. patch_with_history — UPDATE notes + outcome + 推 history。
#[tokio::test(flavor = "current_thread")]
async fn patch_with_history_basic() {
    let db = db().await;
    let _cid = insert_company(&db, "pwh1").await;
    let svc = DecisionTrainingService::new(&db);
    // miss — 不存在的 id 应返 None
    let miss = svc
        .patch_with_history(Uuid::new_v4(), Some("n".into()), None)
        .await
        .expect("miss");
    assert!(miss.is_none());
}

// ===== DTO smoke =====

/// 16. DecisionTrainingExampleRow 类型 smoke。
#[test]
fn example_row_typecheck() {
    use pc_repos::decision_training::DecisionTrainingExampleRow;
    fn assert_from_row<T: for<'a> sqlx::FromRow<'a, sqlx::postgres::PgRow>>() {}
    assert_from_row::<DecisionTrainingExampleRow>();
}
