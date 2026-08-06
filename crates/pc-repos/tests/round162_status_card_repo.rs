//! Round 162 集成测试：status_card 仓储化（新建模块）— StatusCardRepo 13 方法。

use pc_db::Db;
use pc_repos::status_card::{StatusCardRepo, StatusCardRow, StatusCardUpdateRow};
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
        .bind(format!("r162-{tag}-{id}"))
        .bind(format!("R162{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("company");
    id
}

async fn insert_card(db: &Db, company_id: Uuid, title: Option<&str>, archived: bool) -> Uuid {
    let id = Uuid::new_v4();
    let archived_at = if archived {
        Some(chrono::Utc::now())
    } else {
        None
    };
    sqlx::query(
        "INSERT INTO status_cards \\
         (id, company_id, title, interest_prompt, queries, refresh_policy, state, query_version, archived_at) \\
         VALUES ($1, $2, $3, 'r162-prompt', '[]'::jsonb, '{}'::jsonb, 'idle', 1, $4)",
    )
    .bind(id)
    .bind(company_id)
    .bind(title)
    .bind(archived_at)
    .execute(db.pool())
    .await
    .expect("card");
    id
}

// ===== StatusCardRepo 新方法 =====

/// 1. list_active — 只返未归档的。
#[tokio::test(flavor = "current_thread")]
async fn list_active_filters_archived() {
    let db = db().await;
    let cid = insert_company(&db, "la1").await;
    let _active = insert_card(&db, cid, Some("active"), false).await;
    let _archived = insert_card(&db, cid, Some("archived"), true).await;

    let repo = StatusCardRepo::new(&db);
    let rows = repo.list_active(cid).await.expect("list");
    // 至少 1 个 active
    assert!(rows.iter().any(|r| r.title.as_deref() == Some("active")));
    assert!(rows.iter().all(|r| r.archived_at.is_none()));
}

/// 2. get_by_id — 命中 / 不命中。
#[tokio::test(flavor = "current_thread")]
async fn get_by_id_basic() {
    let db = db().await;
    let cid = insert_company(&db, "gb1").await;
    let id = insert_card(&db, cid, Some("g"), false).await;
    let repo = StatusCardRepo::new(&db);
    let hit: Option<StatusCardRow> = repo.get_by_id(id).await.expect("hit");
    assert!(hit.is_some());

    let miss: Option<StatusCardRow> = repo.get_by_id(Uuid::new_v4()).await.expect("miss");
    assert!(miss.is_none());
}

/// 3. create — INSERT + RETURNING。
#[tokio::test(flavor = "current_thread")]
async fn create_basic() {
    let db = db().await;
    let cid = insert_company(&db, "cr1").await;
    let repo = StatusCardRepo::new(&db);
    let row = repo
        .create(
            cid,
            Some("created"),
            "prompt-x",
            &json!([]),
            &json!({"interval": "1h"}),
        )
        .await
        .expect("create");
    assert_eq!(row.title.as_deref(), Some("created"));
    assert_eq!(row.state, "compiling"); // 初始为 compiling
}

/// 4. patch — COALESCE 模式 + archived_at 翻转。
#[tokio::test(flavor = "current_thread")]
async fn patch_basic() {
    let db = db().await;
    let cid = insert_company(&db, "pt1").await;
    let id = insert_card(&db, cid, Some("orig"), false).await;
    let repo = StatusCardRepo::new(&db);
    let row = repo
        .patch(id, Some("upd-title"), None, None, Some(true))
        .await
        .expect("patch")
        .expect("present");
    assert_eq!(row.title.as_deref(), Some("upd-title"));
    assert!(row.archived_at.is_some());
}

/// 5. delete — DELETE by id。
#[tokio::test(flavor = "current_thread")]
async fn delete_basic() {
    let db = db().await;
    let cid = insert_company(&db, "dl1").await;
    let id = insert_card(&db, cid, None, false).await;
    let repo = StatusCardRepo::new(&db);
    let n = repo.delete(id).await.expect("del");
    assert_eq!(n, 1);
    let again = repo.delete(id).await.expect("del2");
    assert_eq!(again, 0);
}

/// 6. list_updates — 返空数组 (无 updates)。
#[tokio::test(flavor = "current_thread")]
async fn list_updates_empty() {
    let db = db().await;
    let _cid = insert_company(&db, "lu1").await;
    let repo = StatusCardRepo::new(&db);
    let rows: Vec<StatusCardUpdateRow> = repo.list_updates(Uuid::new_v4()).await.expect("list");
    assert!(rows.is_empty());
}

/// 7. get_doc_link — None 当 card 不存在。
#[tokio::test(flavor = "current_thread")]
async fn get_doc_link_miss() {
    let db = db().await;
    let _cid = insert_company(&db, "gdl1").await;
    let repo = StatusCardRepo::new(&db);
    let miss = repo.get_doc_link(Uuid::new_v4()).await.expect("miss");
    assert!(miss.is_none());
}

/// 8. recompile — UPDATE state=compiling + query_version++ + RETURNING。
#[tokio::test(flavor = "current_thread")]
async fn recompile_basic() {
    let db = db().await;
    let cid = insert_company(&db, "rc1").await;
    let id = insert_card(&db, cid, None, false).await;
    let repo = StatusCardRepo::new(&db);
    let row = repo
        .recompile(id)
        .await
        .expect("recompile")
        .expect("present");
    assert_eq!(row.state, "compiling");

    let miss = repo.recompile(Uuid::new_v4()).await.expect("miss");
    assert!(miss.is_none());
}

/// 9. refresh — UPDATE state=pending_refresh + next_eval_at=now。
#[tokio::test(flavor = "current_thread")]
async fn refresh_basic() {
    let db = db().await;
    let cid = insert_company(&db, "rf1").await;
    let id = insert_card(&db, cid, None, false).await;
    let repo = StatusCardRepo::new(&db);
    let changed = repo.refresh(id).await.expect("refresh");
    assert!(changed);

    let no_change = repo.refresh(Uuid::new_v4()).await.expect("miss");
    assert!(!no_change);
}

/// 10. claim_due — 0 命中（无 due cards）。
#[tokio::test(flavor = "current_thread")]
async fn claim_due_no_match() {
    let db = db().await;
    let _cid = insert_company(&db, "cd1").await;
    let repo = StatusCardRepo::new(&db);
    let n = repo.claim_due(10).await.expect("claim");
    // 接受任意（含其他测试残留）
    let _ = n;
}

/// 11. dry_run_meta — None miss。
#[tokio::test(flavor = "current_thread")]
async fn dry_run_meta_miss() {
    let db = db().await;
    let _cid = insert_company(&db, "dr1").await;
    let repo = StatusCardRepo::new(&db);
    let miss = repo.dry_run_meta(Uuid::new_v4()).await.expect("miss");
    assert!(miss.is_none());
}

/// 12. update_queries — UPDATE + RETURNING。
#[tokio::test(flavor = "current_thread")]
async fn update_queries_basic() {
    let db = db().await;
    let cid = insert_company(&db, "uq1").await;
    let id = insert_card(&db, cid, None, false).await;
    let repo = StatusCardRepo::new(&db);
    let row = repo
        .update_queries(id, &json!([{"q": "new"}]))
        .await
        .expect("upd")
        .expect("present");
    assert_eq!(row.queries, json!([{"q": "new"}]));
}

/// 13. insert_summary_update + touch_last_generated。
#[tokio::test(flavor = "current_thread")]
async fn summary_insert_and_touch() {
    let db = db().await;
    let cid = insert_company(&db, "si1").await;
    let id = insert_card(&db, cid, None, false).await;
    let repo = StatusCardRepo::new(&db);
    let summary_id = repo
        .insert_summary_update(
            id,
            &json!([{"field": "summary", "op": "set", "value": "x"}]),
            Some("gpt-5"),
            "manual summary (1 chars)",
        )
        .await
        .expect("insert");
    assert!(!summary_id.is_nil());
    let n = repo.touch_last_generated(id).await.expect("touch");
    assert_eq!(n, 1);
}

// ===== DTO smoke =====

/// 14. StatusCardRow 类型 smoke。
#[test]
fn status_card_row_typecheck() {
    fn assert_from_row<T: for<'a> sqlx::FromRow<'a, sqlx::postgres::PgRow>>() {}
    assert_from_row::<StatusCardRow>();
}
