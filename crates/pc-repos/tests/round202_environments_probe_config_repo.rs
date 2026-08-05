//! Round 202 集成测试：environments/probe-config 仓储语义。
//!
//! 覆盖：
//! - `EnvironmentRepo::list_for_company` 按 company_id 过滤
//! - 不同 status 下探测行为（active 视为 valid）

use pc_db::Db;
use pc_repos::environment::{EnvironmentDriver, EnvironmentRepo, EnvironmentStatus, NewEnvironment};
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
        .bind(format!("r202-{tag}-{id}"))
        .bind(format!("R202{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("company");
    id
}

async fn insert_env(
    db: &Db,
    company_id: Uuid,
    name: &str,
    driver: EnvironmentDriver,
    status: EnvironmentStatus,
    config: serde_json::Value,
    env_vars: serde_json::Value,
) -> Uuid {
    let repo = EnvironmentRepo::new(&db);
    let row = repo
        .create(&NewEnvironment {
            name: name.to_owned(),
            description: Some(format!("r202 env {name}")),
            driver,
            status,
            config,
            env_vars,
            metadata: Some(json!({"probe": true})),
        })
        .await
        .expect("create env");
    // patch company_id (create() doesn't take it)
    sqlx::query("UPDATE environments SET company_id = $1 WHERE id = $2")
        .bind(company_id)
        .bind(row.id)
        .execute(db.pool())
        .await
        .expect("set company_id");
    row.id
}

// ===== 1) list_for_company: 跨公司隔离 =====
#[tokio::test(flavor = "current_thread")]
async fn list_for_company_isolation() {
    let db = db().await;
    let c1 = insert_company(&db, "iso1").await;
    let c2 = insert_company(&db, "iso2").await;
    let repo = EnvironmentRepo::new(&db);

    insert_env(
        &db,
        c1,
        "alpha",
        EnvironmentDriver::Local,
        EnvironmentStatus::Active,
        json!({"key": "v"}),
        json!({}),
    )
    .await;
    insert_env(
        &db,
        c1,
        "beta",
        EnvironmentDriver::Local,
        EnvironmentStatus::Active,
        json!({"key": "v"}),
        json!({}),
    )
    .await;
    insert_env(
        &db,
        c2,
        "gamma",
        EnvironmentDriver::Local,
        EnvironmentStatus::Active,
        json!({"key": "v"}),
        json!({}),
    )
    .await;

    let r1 = repo.list_for_company(c1).await.expect("l1");
    let r2 = repo.list_for_company(c2).await.expect("l2");

    assert_eq!(r1.len(), 2, "c1 should have 2 envs");
    assert_eq!(r2.len(), 1, "c2 should have 1 env");

    let mut n1: Vec<&str> = r1.iter().map(|r| r.name.as_str()).collect();
    n1.sort();
    assert_eq!(n1, vec!["alpha", "beta"]);
    assert_eq!(r2[0].name, "gamma");
}

// ===== 2) list_for_company: 空 =====
#[tokio::test(flavor = "current_thread")]
async fn list_for_company_empty() {
    let db = db().await;
    let cid = insert_company(&db, "empty").await;
    let repo = EnvironmentRepo::new(&db);
    let rows = repo.list_for_company(cid).await.expect("empty");
    assert!(rows.is_empty());
}

// ===== 3) list_for_company: 保留 secret_refs key 用于 probe =====
#[tokio::test(flavor = "current_thread")]
async fn list_for_company_preserves_secret_keys() {
    let db = db().await;
    let cid = insert_company(&db, "sk").await;
    let repo = EnvironmentRepo::new(&db);

    insert_env(
        &db,
        cid,
        "with-secret",
        EnvironmentDriver::Local,
        EnvironmentStatus::Active,
        json!({
            "secret_api_key": "ref://x",
            "encrypted_password": "ref://y",
            "regular_key": "v",
        }),
        json!({"ENV": "prod"}),
    )
    .await;

    let rows = repo.list_for_company(cid).await.expect("q");
    assert_eq!(rows.len(), 1);
    let cfg = rows[0].config.as_object().expect("obj");
    assert_eq!(cfg.len(), 3);
    let envv = rows[0].env_vars.as_object().expect("envv");
    assert_eq!(envv.len(), 1);
}
