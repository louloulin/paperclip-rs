//! Round 201 集成测试：secrets/remote-import 仓储语义。
//!
//! 覆盖：
//! - `SecretRepo::find_existing_names` 按公司 + name 批量查重
//! - `SecretRepo::bulk_create_secrets_atomic` 事务性批量插入（含首批 version 同步）

use pc_db::Db;
use pc_repos::secret::{RemoteImportItem, SecretRepo};
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
        .bind(format!("r201-{tag}-{id}"))
        .bind(format!("R201{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("company");
    id
}

// ===== 1) find_existing_names: 空集合返回空 =====
#[tokio::test(flavor = "current_thread")]
async fn find_existing_names_empty() {
    let db = db().await;
    let cid = insert_company(&db, "fempty").await;
    let repo = SecretRepo::new(&db);
    let names: Vec<String> = vec![];
    let existing = repo
        .find_existing_names(cid, &names)
        .await
        .expect("query");
    assert!(existing.is_empty());
}

// ===== 2) find_existing_names: 部分命中 =====
#[tokio::test(flavor = "current_thread")]
async fn find_existing_names_partial() {
    let db = db().await;
    let cid = insert_company(&db, "fpart").await;
    let repo = SecretRepo::new(&db);

    let ext = format!("local:{}", Uuid::new_v4().simple());
    repo.create_company_secret(cid, "EXISTING", "local_encrypted", &ext, None)
        .await
        .expect("create");

    let names = vec!["EXISTING".to_owned(), "MISSING".to_owned()];
    let existing = repo
        .find_existing_names(cid, &names)
        .await
        .expect("query");
    assert!(existing.contains("EXISTING"));
    assert!(!existing.contains("MISSING"));
    assert_eq!(existing.len(), 1);
}

// ===== 3) bulk_create_secrets_atomic: 全部新建 =====
#[tokio::test(flavor = "current_thread")]
async fn bulk_create_all_new() {
    let db = db().await;
    let cid = insert_company(&db, "ball").await;
    let repo = SecretRepo::new(&db);

    let items = vec![
        RemoteImportItem {
            name: "ALPHA".into(),
            provider: "local_encrypted".into(),
            description: Some("first".into()),
            value: Some("alpha-secret".into()),
        },
        RemoteImportItem {
            name: "BETA".into(),
            provider: "local_encrypted".into(),
            description: None,
            value: None,
        },
    ];
    let created = repo
        .bulk_create_secrets_atomic(cid, &items)
        .await
        .expect("bulk");
    assert_eq!(created.len(), 2);
    let mut names: Vec<String> = created.iter().map(|(_, n)| n.clone()).collect();
    names.sort();
    assert_eq!(names, vec!["ALPHA".to_owned(), "BETA".to_owned()]);

    // 验证 BETA 没有 version，ALPHA 有 v1
    for (id, name) in &created {
        if name == "ALPHA" {
            let v: i32 = sqlx::query_scalar(
                "SELECT COALESCE(MAX(version), 0) FROM company_secret_versions WHERE secret_id = $1",
            )
            .bind(id)
            .fetch_one(db.pool())
            .await
            .expect("v");
            assert_eq!(v, 1);
        } else if name == "BETA" {
            let v: i32 = sqlx::query_scalar(
                "SELECT COALESCE(MAX(version), 0) FROM company_secret_versions WHERE secret_id = $1",
            )
            .bind(id)
            .fetch_one(db.pool())
            .await
            .expect("v");
            assert_eq!(v, 0);
        }
    }
}

// ===== 4) bulk_create_secrets_atomic: 冲突时整体回滚 =====
#[tokio::test(flavor = "current_thread")]
async fn bulk_create_conflict_rolls_back() {
    let db = db().await;
    let cid = insert_company(&db, "brb").await;
    let repo = SecretRepo::new(&db);

    // 预置冲突项
    let ext = format!("local:{}", Uuid::new_v4().simple());
    repo.create_company_secret(cid, "DUPLICATE", "local_encrypted", &ext, None)
        .await
        .expect("seed");

    // 一次性插 3 条（含一条冲突）
    let items = vec![
        RemoteImportItem {
            name: "FRESH_A".into(),
            provider: "local_encrypted".into(),
            description: None,
            value: None,
        },
        RemoteImportItem {
            name: "DUPLICATE".into(),
            provider: "local_encrypted".into(),
            description: None,
            value: None,
        },
        RemoteImportItem {
            name: "FRESH_B".into(),
            provider: "local_encrypted".into(),
            description: None,
            value: None,
        },
    ];
    let result = repo.bulk_create_secrets_atomic(cid, &items).await;
    assert!(result.is_err(), "conflict must abort tx");

    // 验证 FRESH_A 与 FRESH_B 没被插入（整体回滚）
    let fresh_a: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM company_secrets WHERE company_id = $1 AND name = 'FRESH_A'",
    )
    .bind(cid)
    .fetch_optional(db.pool())
    .await
    .expect("qa");
    let fresh_b: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM company_secrets WHERE company_id = $1 AND name = 'FRESH_B'",
    )
    .bind(cid)
    .fetch_optional(db.pool())
    .await
    .expect("qb");
    assert!(fresh_a.is_none(), "FRESH_A should be rolled back");
    assert!(fresh_b.is_none(), "FRESH_B should be rolled back");
}

// ===== 5) bulk_create_secrets_atomic: 跨 company 隔离 =====
#[tokio::test(flavor = "current_thread")]
async fn bulk_create_cross_company_isolation() {
    let db = db().await;
    let c1 = insert_company(&db, "iso1").await;
    let c2 = insert_company(&db, "iso2").await;
    let repo = SecretRepo::new(&db);

    let items = vec![RemoteImportItem {
        name: "SHARED_NAME".into(),
        provider: "local_encrypted".into(),
        description: None,
        value: None,
    }];
    let r1 = repo
        .bulk_create_secrets_atomic(c1, &items)
        .await
        .expect("c1");
    let r2 = repo
        .bulk_create_secrets_atomic(c2, &items)
        .await
        .expect("c2");
    assert_eq!(r1.len(), 1);
    assert_eq!(r2.len(), 1);
    assert_ne!(r1[0].0, r2[0].0);
}
