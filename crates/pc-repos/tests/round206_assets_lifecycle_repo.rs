//! Round 206 集成测试：assets 生命周期仓储语义。
//!
//! 覆盖：
//! - `AssetRepo::delete_by_id` 删除并返回受影响行数
//! - `AssetRepo::list_attachments_for_asset` 反查 issue_attachments
//! - `AssetRepo::list_by_company_with_provider` 按公司+provider 过滤

use pc_db::Db;
use pc_repos::asset::{AssetRepo, CreateAssetRecord};
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(format!("r206-{tag}-{id}"))
        .bind(format!("R206{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("company");
    id
}

async fn insert_issue(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, origin_kind, origin_fingerprint) \
         VALUES ($1, $2, 'r206-issue', 'todo', 'medium', 'system', $3)",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("r206-fp-{id}"))
    .execute(db.pool())
    .await
    .expect("issue");
    id
}

async fn make_asset(db: &Db, company_id: Uuid, provider: &str, key: &str) -> Uuid {
    let rec = CreateAssetRecord::new(
        provider.to_owned(),
        key.to_owned(),
        "image/png".to_owned(),
        1024,
        format!("sha-{}", Uuid::new_v4().simple()),
    );
    let row = AssetRepo::new(&db)
        .create(company_id, rec)
        .await
        .expect("create");
    row.id
}

// ===== 1) delete_by_id: 删除后再次删除返回 false =====
#[tokio::test(flavor = "current_thread")]
async fn delete_by_id_round_trip() {
    let db = db().await;
    let cid = insert_company(&db, "del").await;
    let aid = make_asset(&db, cid, "company-assets", "k1.png").await;
    let repo = AssetRepo::new(&db);

    let deleted = repo.delete_by_id(aid).await.expect("d1");
    assert!(deleted, "first delete must return true");

    let deleted2 = repo.delete_by_id(aid).await.expect("d2");
    assert!(!deleted2, "second delete must return false");

    let fetched = repo.get_by_id(aid).await.expect("get");
    assert!(fetched.is_none());
}

// ===== 2) list_attachments_for_asset: 空/多条 =====
#[tokio::test(flavor = "current_thread")]
async fn list_attachments_for_asset() {
    let db = db().await;
    let cid = insert_company(&db, "att").await;
    let aid = make_asset(&db, cid, "company-assets", "k2.png").await;
    let repo = AssetRepo::new(&db);

    // 初始无引用
    let att0 = repo.list_attachments_for_asset(aid).await.expect("a0");
    assert!(att0.is_empty());

    // 加 2 个引用（不同 issue）
    let i1 = insert_issue(&db, cid).await;
    let i2 = insert_issue(&db, cid).await;
    for issue_id in [i1, i2] {
        sqlx::query(
            "INSERT INTO issue_attachments (id, company_id, issue_id, asset_id) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(Uuid::new_v4())
        .bind(cid)
        .bind(issue_id)
        .bind(aid)
        .execute(db.pool())
        .await
        .expect("ia");
    }

    let att = repo.list_attachments_for_asset(aid).await.expect("a1");
    assert_eq!(att.len(), 2);
    let mut issue_ids: Vec<Uuid> = att.iter().map(|(_, i, _)| *i).collect();
    issue_ids.sort();
    assert_eq!(issue_ids, vec![i1.min(i2), i1.max(i2)]);
}

// ===== 3) list_by_company_with_provider: 过滤 + 排序 =====
#[tokio::test(flavor = "current_thread")]
async fn list_by_company_with_provider_filter() {
    let db = db().await;
    let cid = insert_company(&db, "lpf").await;
    let repo = AssetRepo::new(&db);

    let a1 = make_asset(&db, cid, "company-assets", "p1/k1").await;
    let a2 = make_asset(&db, cid, "company-assets", "p1/k2").await;
    let a3 = make_asset(&db, cid, "external-s3", "p2/k1").await;

    // 不带 provider → 全列
    let all = repo
        .list_by_company_with_provider(cid, None, 10)
        .await
        .expect("all");
    let all_ids: std::collections::HashSet<Uuid> = all.iter().map(|r| r.id).collect();
    assert!(all_ids.contains(&a1));
    assert!(all_ids.contains(&a2));
    assert!(all_ids.contains(&a3));
    assert_eq!(all.len(), 3);

    // 带 provider=company-assets → 2 条
    let p1 = repo
        .list_by_company_with_provider(cid, Some("company-assets"), 10)
        .await
        .expect("p1");
    assert_eq!(p1.len(), 2);
    let p1_ids: std::collections::HashSet<Uuid> = p1.iter().map(|r| r.id).collect();
    assert!(p1_ids.contains(&a1));
    assert!(p1_ids.contains(&a2));
    assert!(!p1_ids.contains(&a3));

    // 带 provider=external-s3 → 1 条
    let p2 = repo
        .list_by_company_with_provider(cid, Some("external-s3"), 10)
        .await
        .expect("p2");
    assert_eq!(p2.len(), 1);
    assert_eq!(p2[0].id, a3);
}
