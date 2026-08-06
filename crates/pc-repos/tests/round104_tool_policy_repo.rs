//! Round 104 集成测试：验证 `pc_repos::ToolRepo` 在 `tool_policies` 上的真实 schema 路径。
//!
//! 真实表 schema (0149_agent_access_phase2_contracts.sql)：
//!   tool_policies(
//!     id, company_id, name, description, policy_type, priority, enabled,
//!     selectors, conditions, config, created_by_agent_id, created_by_user_id,
//!     created_at, updated_at
//!   )
//!
//! **不存在**的列：`decision / scope`

use pc_db::Db;
use pc_repos::tool::{NewToolPolicy, ToolRepo};
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
        .bind(format!("r104-{tag}-{id}"))
        .bind(format!("R104{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("insert company");
    id
}

async fn create_policy_row(
    repo: &ToolRepo<'_>,
    cid: Uuid,
    name: &str,
    priority: i32,
    enabled: bool,
) -> Uuid {
    repo.create_policy(&NewToolPolicy {
        company_id: cid,
        name: name.into(),
        description: None,
        policy_type: "scoped".into(),
        priority,
        enabled,
        selectors: json!({}),
        conditions: json!({}),
        config: json!({}),
        created_by_agent_id: None,
        created_by_user_id: None,
    })
    .await
    .expect("create")
    .id
}

/// 1. list_policies_by_company：按 name ASC 排序，真实列投影
#[tokio::test(flavor = "current_thread")]
async fn tool_policy_repo_list_orders_by_name_asc() {
    let db = db().await;
    let repo = ToolRepo::new(&db);
    let cid = insert_company(&db, "list").await;
    for (n, p) in [("c", 1), ("a", 2), ("b", 3)] {
        create_policy_row(&repo, cid, n, p, true).await;
    }
    let rows = repo.list_policies_by_company(cid).await.expect("list");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].name, "a");
    assert_eq!(rows[1].name, "b");
    assert_eq!(rows[2].name, "c");
    // 真实列
    assert_eq!(rows[0].policy_type, "scoped");
    assert_eq!(rows[0].priority, 2);
}

/// 2. list_enabled_policies_by_company：enabled=true 过滤 + priority 排序
#[tokio::test(flavor = "current_thread")]
async fn tool_policy_repo_list_enabled_only() {
    let db = db().await;
    let repo = ToolRepo::new(&db);
    let cid = insert_company(&db, "enabled").await;
    create_policy_row(&repo, cid, "A", 1, true).await;
    create_policy_row(&repo, cid, "B", 2, false).await;
    create_policy_row(&repo, cid, "C", 3, true).await;

    let rows = repo
        .list_enabled_policies_by_company(cid)
        .await
        .expect("list");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].name, "A"); // priority=1
    assert_eq!(rows[1].name, "C"); // priority=3
}

/// 3. create_policy：默认 priority=100, enabled=true, selectors={}
#[tokio::test(flavor = "current_thread")]
async fn tool_policy_repo_create_uses_defaults() {
    let db = db().await;
    let repo = ToolRepo::new(&db);
    let cid = insert_company(&db, "defaults").await;

    let row = repo
        .create_policy(&NewToolPolicy {
            company_id: cid,
            name: "D".into(),
            description: Some("default-test".into()),
            policy_type: "scoped".into(),
            priority: 100,
            enabled: true,
            selectors: json!({}),
            conditions: json!({}),
            config: json!({}),
            created_by_agent_id: None,
            created_by_user_id: Some("u-x".into()),
        })
        .await
        .expect("create");

    assert_eq!(row.priority, 100);
    assert!(row.enabled);
    assert_eq!(row.description.as_deref(), Some("default-test"));
    assert_eq!(row.created_by_user_id.as_deref(), Some("u-x"));
}

/// 4. find_policy_id_by_name：冲突检测
#[tokio::test(flavor = "current_thread")]
async fn tool_policy_repo_find_by_name_for_conflict() {
    let db = db().await;
    let repo = ToolRepo::new(&db);
    let cid = insert_company(&db, "find").await;
    let pid = create_policy_row(&repo, cid, "Dup", 1, true).await;

    let found = repo
        .find_policy_id_by_name(cid, "Dup")
        .await
        .expect("find")
        .expect("present");
    assert_eq!(found, pid);
    assert!(repo
        .find_policy_id_by_name(cid, "Other")
        .await
        .expect("find other")
        .is_none());
}

/// 5. delete_policy：物理删除
#[tokio::test(flavor = "current_thread")]
async fn tool_policy_repo_delete() {
    let db = db().await;
    let repo = ToolRepo::new(&db);
    let cid = insert_company(&db, "del").await;
    let pid = create_policy_row(&repo, cid, "ToDel", 1, true).await;
    let n = repo.delete_policy(cid, pid).await.expect("del");
    assert!(n);
    // 不存在
    assert!(repo.get_policy(cid, pid).await.expect("get").is_none());
}

/// 6. reorder_policies：同一事务内重排优先级
#[tokio::test(flavor = "current_thread")]
async fn tool_policy_repo_reorder_assigns_stepped_priorities() {
    let db = db().await;
    let repo = ToolRepo::new(&db);
    let cid = insert_company(&db, "reorder").await;
    let p1 = create_policy_row(&repo, cid, "P1", 999, true).await;
    let p2 = create_policy_row(&repo, cid, "P2", 999, true).await;
    let p3 = create_policy_row(&repo, cid, "P3", 999, true).await;

    let step = 50;
    let n = repo
        .reorder_policies(cid, &[p3, p1, p2], step)
        .await
        .expect("reorder");
    assert_eq!(n, 3, "should affect 3 rows");

    // 按 priority ASC 拉回：p3=0, p1=50, p2=100
    let rows = repo
        .list_enabled_policies_by_company(cid)
        .await
        .expect("list");
    let order: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(order, vec!["P3", "P1", "P2"]);

    // 同时验证 priority 数值
    let r1 = rows.iter().find(|r| r.name == "P3").unwrap();
    assert_eq!(r1.priority, 0);
    let r2 = rows.iter().find(|r| r.name == "P2").unwrap();
    assert_eq!(r2.priority, 100);
}

/// 7. 真实 schema 防漂移：decision / scope 不应存在
#[tokio::test(flavor = "current_thread")]
async fn tool_policies_table_real_column_audit() {
    let db = db().await;
    let bad: Vec<(String,)> = sqlx::query_as(
        "SELECT column_name FROM information_schema.columns              WHERE table_name='tool_policies'              AND column_name IN ('decision', 'scope')",
    )
    .fetch_all(db.pool())
    .await
    .expect("query bad cols");
    assert!(
        bad.is_empty(),
        "schema leak: {:?}",
        bad.iter().map(|(c,)| c.clone()).collect::<Vec<_>>()
    );
    // 真实列必须在
    let real: Vec<(String,)> = sqlx::query_as(
        "SELECT column_name FROM information_schema.columns              WHERE table_name='tool_policies'              AND column_name IN ('policy_type', 'priority', 'enabled', 'selectors', 'conditions', 'config')",
    )
    .fetch_all(db.pool())
    .await
    .expect("query real cols");
    let real_names: std::collections::HashSet<String> = real.into_iter().map(|(s,)| s).collect();
    for must in [
        "policy_type",
        "priority",
        "enabled",
        "selectors",
        "conditions",
        "config",
    ] {
        assert!(real_names.contains(must), "missing column: {must}");
    }
}
