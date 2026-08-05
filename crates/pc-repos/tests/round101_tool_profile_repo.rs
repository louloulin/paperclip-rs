//! Round 101 集成测试：验证 `pc_repos::ToolRepo` 在 tool_profiles / tool_profile_entries 上的
//! 1:1 真实 schema 路径。
//!
//! 前置：DB 已运行 196 条 migrate，包含 0149_agent_access_phase2_contracts.sql。
//!
//! 关键点：
//! - 真实 `tool_profiles` 列：id, company_id, profile_key, name, description, status,
//!   default_action, metadata（**没有** kind/scope）
//! - 真实 `tool_profile_entries` 列：id, company_id, profile_id, selector_type, effect,
//!   application_id, connection_id, catalog_entry_id, tool_name, risk_level, conditions
//!
//! 这些测试直接调用 Repo API，验证仓储层独立可用（不被 HTTP 路由耦合）。

use pc_db::Db;
use pc_repos::tool::{NewToolProfile, NewToolProfileEntry, ToolRepo};
use serde_json::json;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect to test db")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(id)
        .bind(format!("r101-{tag}-{id}"))
        .bind(format!("R101{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("insert company");
    id
}

/// 1. list_profiles_by_company：用真实列投影，按 updated_at DESC 排序
#[tokio::test(flavor = "current_thread")]
async fn tool_profile_repo_list_orders_by_updated_at_desc() {
    let db = db().await;
    let repo = ToolRepo::new(&db);
    let cid = insert_company(&db, "list").await;

    repo.create_profile(&NewToolProfile {
        company_id: cid,
        profile_key: "p1".into(),
        name: "Profile 1".into(),
        description: Some("first".into()),
        status: "active".into(),
        default_action: "deny".into(),
        metadata: json!({"foo": 1}),
    })
    .await
    .expect("create p1");
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    repo.create_profile(&NewToolProfile {
        company_id: cid,
        profile_key: "p2".into(),
        name: "Profile 2".into(),
        description: None,
        status: "active".into(),
        default_action: "allow".into(),
        metadata: json!({}),
    })
    .await
    .expect("create p2");

    let rows = repo.list_profiles_by_company(cid).await.expect("list");
    assert_eq!(rows.len(), 2);
    // 最近创建的 p2 在前
    assert_eq!(rows[0].profile_key, "p2");
    assert_eq!(rows[1].profile_key, "p1");
    // 验证真实列投影
    assert_eq!(rows[0].default_action, "allow");
    assert_eq!(rows[1].description.as_deref(), Some("first"));
    assert_eq!(rows[1].metadata["foo"], 1);
}

/// 2. get_profile：精确 (company_id, id) 查找
#[tokio::test(flavor = "current_thread")]
async fn tool_profile_repo_get_by_company_and_id() {
    let db = db().await;
    let repo = ToolRepo::new(&db);
    let cid = insert_company(&db, "get").await;
    let row = repo
        .create_profile(&NewToolProfile {
            company_id: cid,
            profile_key: "k".into(),
            name: "n".into(),
            description: None,
            status: "active".into(),
            default_action: "deny".into(),
            metadata: json!({}),
        })
        .await
        .expect("create");

    let got = repo.get_profile(cid, row.id).await.expect("get").expect("present");
    assert_eq!(got.profile_key, "k");
    assert_eq!(got.name, "n");

    // 跨 company 查不到
    let other_cid = insert_company(&db, "other").await;
    let none = repo.get_profile(other_cid, row.id).await.expect("get-other");
    assert!(none.is_none());
}

/// 3. find_profile_id_by_key：冲突检测 helper
#[tokio::test(flavor = "current_thread")]
async fn tool_profile_repo_find_by_key_for_conflict_check() {
    let db = db().await;
    let repo = ToolRepo::new(&db);
    let cid = insert_company(&db, "key").await;
    let row = repo
        .create_profile(&NewToolProfile {
            company_id: cid,
            profile_key: "my-key".into(),
            name: "n".into(),
            description: None,
            status: "active".into(),
            default_action: "deny".into(),
            metadata: json!({}),
        })
        .await
        .expect("create");

    let found = repo
        .find_profile_id_by_key(cid, "my-key")
        .await
        .expect("find")
        .expect("must exist");
    assert_eq!(found, row.id);

    // 不同 company 不会冲突
    let other_cid = insert_company(&db, "othr").await;
    let none = repo
        .find_profile_id_by_key(other_cid, "my-key")
        .await
        .expect("find other");
    assert!(none.is_none());
}

/// 4. delete_profile：物理删除，并级联删除 entries（FK CASCADE）
#[tokio::test(flavor = "current_thread")]
async fn tool_profile_repo_delete_cascades_entries() {
    let db = db().await;
    let repo = ToolRepo::new(&db);
    let cid = insert_company(&db, "del").await;
    let profile = repo
        .create_profile(&NewToolProfile {
            company_id: cid,
            profile_key: "k".into(),
            name: "n".into(),
            description: None,
            status: "active".into(),
            default_action: "deny".into(),
            metadata: json!({}),
        })
        .await
        .expect("create profile");

    // 创建 2 个 entries
    for tool_name in ["read_file", "write_file"] {
        repo.create_profile_entry(&NewToolProfileEntry {
            company_id: cid,
            profile_id: profile.id,
            selector_type: "tool_name".into(),
            effect: "include".into(),
            application_id: None,
            connection_id: None,
            catalog_entry_id: None,
            tool_name: Some(tool_name.into()),
            risk_level: Some("read".into()),
            conditions: None,
        })
        .await
        .expect("entry");
    }

    let entries_before = repo
        .list_profile_entries(profile.id)
        .await
        .expect("list");
    assert_eq!(entries_before.len(), 2);

    // 删除 profile
    let n = repo.delete_profile(cid, profile.id).await.expect("delete");
    assert!(n);
    // entries 通过 FK CASCADE 也消失
    let entries_after = repo
        .list_profile_entries(profile.id)
        .await
        .expect("list after");
    assert_eq!(entries_after.len(), 0);
}

/// 5. create_profile_entry：写入真实列
#[tokio::test(flavor = "current_thread")]
async fn tool_profile_repo_create_entry_persists_real_columns() {
    let db = db().await;
    let repo = ToolRepo::new(&db);
    let cid = insert_company(&db, "entry").await;
    let profile = repo
        .create_profile(&NewToolProfile {
            company_id: cid,
            profile_key: "k".into(),
            name: "n".into(),
            description: None,
            status: "active".into(),
            default_action: "deny".into(),
            metadata: json!({}),
        })
        .await
        .expect("create profile");

    let entry = repo
        .create_profile_entry(&NewToolProfileEntry {
            company_id: cid,
            profile_id: profile.id,
            selector_type: "tool_name".into(),
            effect: "exclude".into(),
            application_id: None,
            connection_id: None,
            catalog_entry_id: None,
            tool_name: Some("delete_all".into()),
            risk_level: Some("destructive".into()),
            conditions: Some(json!({"max_arg_size": 1024})),
        })
        .await
        .expect("entry");

    assert_eq!(entry.effect, "exclude");
    assert_eq!(entry.tool_name.as_deref(), Some("delete_all"));
    assert_eq!(entry.conditions.as_ref().unwrap()["max_arg_size"], 1024);
}

/// 6. validation：name / profile_key / selector_type 空必须拒绝
#[tokio::test(flavor = "current_thread")]
async fn tool_profile_repo_validation_rejects_empty_fields() {
    let db = db().await;
    let repo = ToolRepo::new(&db);
    let cid = insert_company(&db, "val").await;

    // 空 name
    let bad1 = repo
        .create_profile(&NewToolProfile {
            company_id: cid,
            profile_key: "k".into(),
            name: "".into(),
            description: None,
            status: "active".into(),
            default_action: "deny".into(),
            metadata: json!({}),
        })
        .await;
    assert!(bad1.is_err());

    // 空 profile_key
    let bad2 = repo
        .create_profile(&NewToolProfile {
            company_id: cid,
            profile_key: "".into(),
            name: "n".into(),
            description: None,
            status: "active".into(),
            default_action: "deny".into(),
            metadata: json!({}),
        })
        .await;
    assert!(bad2.is_err());
}
