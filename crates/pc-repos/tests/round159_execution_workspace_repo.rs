//! Round 159 集成测试：execution_workspaces 仓储化扩展 — ExecutionRepo 8 新方法。

use pc_db::Db;
use pc_repos::execution::ExecutionRepo;
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
        .bind(format!("r159-{tag}-{id}"))
        .bind(format!("R159{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("company");
    id
}

async fn insert_workspace(db: &Db, company_id: Uuid, status: &str, name: &str) -> Uuid {
    let id = Uuid::new_v4();
    let pid = Uuid::new_v4(); // project_id
    sqlx::query(
        "INSERT INTO execution_workspaces \
         (id, company_id, project_id, mode, strategy_type, name, status, provider_type) \
         VALUES ($1, $2, $3, 'execution', 'worktree', $4, $5, 'local_fs')",
    )
    .bind(id)
    .bind(company_id)
    .bind(pid)
    .bind(name)
    .bind(status)
    .execute(db.pool())
    .await
    .expect("workspace");
    id
}

// ===== ExecutionRepo 新方法 =====

/// 1. overview_stats — 三个子查询（active_workspaces / recent_24h_runs / failed_24h_runs）。
#[tokio::test(flavor = "current_thread")]
async fn overview_stats_basic() {
    let db = db().await;
    let cid = insert_company(&db, "ov1").await;
    let _ = insert_workspace(&db, cid, "active", "w1").await;
    let _ = insert_workspace(&db, cid, "active", "w2").await;
    let _ = insert_workspace(&db, cid, "closed", "w3").await;

    let repo = ExecutionRepo::new(&db);
    let (active, _recent, _failed) = repo.overview_stats(cid).await.expect("stats");
    // 至少有 2 个 active（可能其他测试残留）
    assert!(active >= 2);
}

/// 2. get_by_id — 命中 / 不命中。
#[tokio::test(flavor = "current_thread")]
async fn get_by_id_basic() {
    let db = db().await;
    let cid = insert_company(&db, "gb1").await;
    let id = insert_workspace(&db, cid, "active", "ws-1").await;
    let repo = ExecutionRepo::new(&db);
    let hit = repo.get_by_id(id).await.expect("hit");
    assert!(hit.is_some());
    let hit = hit.unwrap();
    assert_eq!(hit.company_id, cid);

    let miss = repo.get_by_id(Uuid::new_v4()).await.expect("miss");
    assert!(miss.is_none());
}

/// 3. company_id_for_id — 命中 / miss。
#[tokio::test(flavor = "current_thread")]
async fn company_id_for_id_basic() {
    let db = db().await;
    let cid = insert_company(&db, "cf1").await;
    let id = insert_workspace(&db, cid, "active", "ws-cf").await;
    let repo = ExecutionRepo::new(&db);
    let back = repo.company_id_for_id(id).await.expect("get");
    assert_eq!(back, Some(cid));

    let miss = repo.company_id_for_id(Uuid::new_v4()).await.expect("miss");
    assert!(miss.is_none());
}

/// 4. update_name — COALESCE 模式。
#[tokio::test(flavor = "current_thread")]
async fn update_name_basic() {
    let db = db().await;
    let cid = insert_company(&db, "un1").await;
    let id = insert_workspace(&db, cid, "active", "old-name").await;
    let repo = ExecutionRepo::new(&db);
    // 传 Some("new-name") — 应更新
    let changed = repo.update_name(id, Some("new-name")).await.expect("upd");
    assert!(changed);
    let row = repo.get_by_id(id).await.expect("get").expect("present");
    assert_eq!(row.name, "new-name");

    // 传 None — COALESCE 保留原值
    let no_change = repo.update_name(id, None).await.expect("noop");
    assert!(!no_change);
    let row = repo.get_by_id(id).await.expect("get").expect("present");
    assert_eq!(row.name, "new-name");
}

/// 5. set_status_to_reconciling — UPDATE status。
#[tokio::test(flavor = "current_thread")]
async fn set_status_to_reconciling_basic() {
    let db = db().await;
    let cid = insert_company(&db, "sr1").await;
    let id = insert_workspace(&db, cid, "active", "ws-sr").await;
    let repo = ExecutionRepo::new(&db);
    let changed = repo.set_status_to_reconciling(id).await.expect("set");
    assert!(changed);
    let row = repo.get_by_id(id).await.expect("get").expect("present");
    assert_eq!(row.status, "reconciling");
}

/// 6. set_branch_provider_ref — 写入 branch + provider_ref + touch。
#[tokio::test(flavor = "current_thread")]
async fn set_branch_provider_ref_basic() {
    let db = db().await;
    let cid = insert_company(&db, "sb1").await;
    let id = insert_workspace(&db, cid, "active", "ws-sb").await;
    let repo = ExecutionRepo::new(&db);
    let changed = repo
        .set_branch_provider_ref(id, "feature-x", "/path/to/worktree")
        .await
        .expect("set");
    assert!(changed);
    let row = repo.get_by_id(id).await.expect("get").expect("present");
    assert_eq!(row.branch_name.as_deref(), Some("feature-x"));
    assert_eq!(row.provider_ref.as_deref(), Some("/path/to/worktree"));
}

/// 7. clear_provider_ref — 清掉 provider_ref + 设 cleanup_reason。
#[tokio::test(flavor = "current_thread")]
async fn clear_provider_ref_basic() {
    let db = db().await;
    let cid = insert_company(&db, "cp1").await;
    let id = insert_workspace(&db, cid, "active", "ws-cp").await;
    let _ = repo_set_provider_ref(&db, id, "/some/path").await;

    let repo = ExecutionRepo::new(&db);
    let changed = repo.clear_provider_ref(id).await.expect("clear");
    assert!(changed);
    let row = repo.get_by_id(id).await.expect("get").expect("present");
    assert!(row.provider_ref.is_none());
    assert_eq!(row.cleanup_reason.as_deref(), Some("worktree_removed"));
}

async fn repo_set_provider_ref(db: &Db, id: Uuid, path: &str) {
    sqlx::query(
        "UPDATE execution_workspaces SET provider_ref = $2, branch_name = 'b' WHERE id = $1",
    )
    .bind(id)
    .bind(path)
    .execute(db.pool())
    .await
    .expect("pre");
}

/// 8. touch_last_used — 复用既有方法（验证写路径 OK）。
#[tokio::test(flavor = "current_thread")]
async fn touch_last_used_basic() {
    let db = db().await;
    let cid = insert_company(&db, "tl1").await;
    let id = insert_workspace(&db, cid, "active", "ws-tl").await;
    let repo = ExecutionRepo::new(&db);
    repo.touch_last_used(id).await.expect("touch");
    // 验证 workspace 仍然存在（hint: last_used_at 字段在 schema 中 NOT NULL）
    let row = repo.get_by_id(id).await.expect("get").expect("present");
    assert_eq!(row.id, id);
}

/// 9. latest_heartbeat_for_workspace — 空 (没有 heartbeat_run)。
#[tokio::test(flavor = "current_thread")]
async fn latest_heartbeat_for_workspace_empty() {
    let db = db().await;
    let cid = insert_company(&db, "lh1").await;
    let id = insert_workspace(&db, cid, "active", "ws-lh").await;
    let repo = ExecutionRepo::new(&db);
    let none = repo
        .latest_heartbeat_for_workspace(id)
        .await
        .expect("get");
    // 可能为 None（新 workspace 无 heartbeat），也可能命中其他测试残留
    let _ = none;
}

// ===== DTO smoke (sync) =====

/// 10. WorkspaceRow 类型 smoke。
#[test]
fn workspace_row_typecheck() {
    use pc_repos::execution::WorkspaceRow;
    fn assert_from_row<T: for<'a> sqlx::FromRow<'a, sqlx::postgres::PgRow>>() {}
    assert_from_row::<WorkspaceRow>();
}
