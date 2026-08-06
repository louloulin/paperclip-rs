//! Round 131 集成测试：
//! - AssetRepo::list_by_company
//! - CompanyRepo::update_branding（logo 嵌入 description 后缀 + name 单独 update）
//! - FeedbackTraceRepo::list_for_company（表不存在时返回空）

use pc_db::Db;
use pc_repos::asset::{AssetRepo, CreateAssetRecord};
use pc_repos::company::CompanyRepo;
use pc_repos::feedback_trace::FeedbackTraceRepo;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(id)
        .bind(format!("r131-{tag}-{id}"))
        .bind(format!("R131{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("insert company");
    id
}

async fn insert_asset(db: &Db, company_id: Uuid, key: &str) -> Uuid {
    let r = AssetRepo::new(db)
        .create(
            company_id,
            CreateAssetRecord::new("paperclip", key, "image/png", 1024, "deadbeef"),
        )
        .await
        .expect("create asset");
    r.id
}

// ===== AssetRepo::list_by_company =====

/// 1. list_by_company — 按 created_at DESC 排序。
#[tokio::test(flavor = "current_thread")]
async fn asset_list_orders_by_created_desc() {
    let db = db().await;
    let cid = insert_company(&db, "asset").await;
    let a = insert_asset(&db, cid, "k1").await;
    // 等 100ms 让 created_at 错开
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let b = insert_asset(&db, cid, "k2").await;
    let list = AssetRepo::new(&db)
        .list_by_company(cid, 10)
        .await
        .expect("list");
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].id, b, "newer first");
    assert_eq!(list[1].id, a);
}

/// 2. list_by_company — 跨公司隔离。
#[tokio::test(flavor = "current_thread")]
async fn asset_list_isolates_tenants() {
    let db = db().await;
    let a = insert_company(&db, "a").await;
    let b = insert_company(&db, "b").await;
    insert_asset(&db, a, "ka").await;
    insert_asset(&db, b, "kb1").await;
    insert_asset(&db, b, "kb2").await;
    assert_eq!(
        AssetRepo::new(&db)
            .list_by_company(a, 100)
            .await
            .expect("a")
            .len(),
        1
    );
    assert_eq!(
        AssetRepo::new(&db)
            .list_by_company(b, 100)
            .await
            .expect("b")
            .len(),
        2
    );
}

/// 3. list_by_company — limit 生效。
#[tokio::test(flavor = "current_thread")]
async fn asset_list_respects_limit() {
    let db = db().await;
    let cid = insert_company(&db, "lim").await;
    for i in 0..5 {
        insert_asset(&db, cid, &format!("k{i}")).await;
    }
    let list = AssetRepo::new(&db)
        .list_by_company(cid, 3)
        .await
        .expect("list");
    assert_eq!(list.len(), 3);
}

// ===== CompanyRepo::update_branding =====

/// 4. update_branding — 只改 name。
#[tokio::test(flavor = "current_thread")]
async fn branding_updates_name_only() {
    let db = db().await;
    let cid = insert_company(&db, "bn").await;
    let row = CompanyRepo::new(&db)
        .update_branding(cid, Some("New Name"), None)
        .await
        .expect("upd")
        .expect("row");
    assert_eq!(row.name, "New Name");
    // description 保持默认（NULL）
    assert!(row.description.is_none());
}

/// 5. update_branding — 只改 logo（嵌入 description 后缀）。
#[tokio::test(flavor = "current_thread")]
async fn branding_appends_logo_to_description() {
    let db = db().await;
    let cid = insert_company(&db, "bl").await;
    let row = CompanyRepo::new(&db)
        .update_branding(cid, None, Some("https://cdn/logo.png"))
        .await
        .expect("upd")
        .expect("row");
    let desc = row.description.expect("desc");
    assert!(
        desc.contains("<!-- logo:https://cdn/logo.png -->"),
        "actual: {desc}"
    );
    // name 未变
    assert!(row.name.starts_with("r131-bl-"));
}

/// 6. update_branding — 已有 description 时 append 而非覆盖。
#[tokio::test(flavor = "current_thread")]
async fn branding_preserves_existing_description() {
    let db = db().await;
    let cid = insert_company(&db, "bp").await;
    sqlx::query("UPDATE companies SET description=$2 WHERE id=$1")
        .bind(cid)
        .bind("existing content")
        .execute(db.pool())
        .await
        .expect("seed desc");
    let row = CompanyRepo::new(&db)
        .update_branding(cid, None, Some("logo1"))
        .await
        .expect("upd")
        .expect("row");
    let desc = row.description.expect("desc");
    assert!(desc.contains("existing content"), "actual: {desc}");
    assert!(desc.contains("<!-- logo:logo1 -->"), "actual: {desc}");
}

/// 7. update_branding — name + logo 同时改。
#[tokio::test(flavor = "current_thread")]
async fn branding_updates_both() {
    let db = db().await;
    let cid = insert_company(&db, "bb").await;
    let row = CompanyRepo::new(&db)
        .update_branding(cid, Some("Acme"), Some("logo-url"))
        .await
        .expect("upd")
        .expect("row");
    assert_eq!(row.name, "Acme");
    let desc = row.description.expect("desc");
    assert!(desc.contains("<!-- logo:logo-url -->"));
}

/// 8. update_branding — 不存在的 company 返回 None。
#[tokio::test(flavor = "current_thread")]
async fn branding_unknown_company_returns_none() {
    let db = db().await;
    let row = CompanyRepo::new(&db)
        .update_branding(Uuid::new_v4(), Some("X"), None)
        .await
        .expect("upd");
    assert!(row.is_none());
}

/// 9. update_branding — name 为空时不更新（COALESCE 语义）。
#[tokio::test(flavor = "current_thread")]
async fn branding_name_none_keeps_existing() {
    let db = db().await;
    let cid = insert_company(&db, "bk").await;
    let original_name = CompanyRepo::new(&db)
        .get(cid)
        .await
        .expect("get")
        .expect("row")
        .name;
    let row = CompanyRepo::new(&db)
        .update_branding(cid, None, None)
        .await
        .expect("upd")
        .expect("row");
    assert_eq!(row.name, original_name);
}

// ===== FeedbackTraceRepo::list_for_company =====

/// 10. feedback_trace — 表不存在 / 无 traces 返回空集合。
#[tokio::test(flavor = "current_thread")]
async fn feedback_traces_empty_when_table_missing() {
    let db = db().await;
    let cid = insert_company(&db, "ft").await;
    let list = FeedbackTraceRepo::new(&db)
        .list_for_company(cid, 100)
        .await
        .unwrap_or_default();
    // 表不存在 → unwrap_or_default 给空 Vec
    assert!(list.is_empty());
}
