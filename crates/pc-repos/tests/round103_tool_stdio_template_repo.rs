//! Round 103 集成测试：验证 `pc_repos::ToolRepo` 在 `tool_stdio_command_templates` 上的
//! 真实 schema 路径。
//!
//! 真实表 schema (0153_tool_stdio_command_templates.sql)：
//!   tool_stdio_command_templates(
//!     id, company_id, template_key, name, description, status, command,
//!     args, env_keys, tools,
//!     created_by_agent_id, created_by_user_id,
//!     disabled_at,
//!     created_at, updated_at
//!   )
//!
//! **不存在**的列：`template_id`(实为 template_key) / `env_schema`(实为 args/env_keys/tools) / `disabled_reason`

use pc_db::Db;
use pc_repos::tool::{NewToolStdioTemplate, ToolRepo};
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
        .bind(format!("r103-{tag}-{id}"))
        .bind(format!("R103{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("insert company");
    id
}

/// 1. list_stdio_templates_by_company：按 name ASC 排序，真实列投影
#[tokio::test(flavor = "current_thread")]
async fn tool_stdio_template_repo_list_orders_by_name_asc() {
    let db = db().await;
    let repo = ToolRepo::new(&db);
    let cid = insert_company(&db, "list").await;

    for (key, name) in [("k3", "Charlie"), ("k1", "Alpha"), ("k2", "Bravo")] {
        repo.create_stdio_template(&NewToolStdioTemplate {
            company_id: cid,
            template_key: key.into(),
            name: name.into(),
            description: None,
            command: "echo".into(),
            args: json!([]),
            env_keys: json!([]),
            tools: json!([]),
            created_by_agent_id: None,
            created_by_user_id: None,
        })
        .await
        .expect("create");
    }

    let rows = repo.list_stdio_templates_by_company(cid).await.expect("list");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].name, "Alpha");
    assert_eq!(rows[1].name, "Bravo");
    assert_eq!(rows[2].name, "Charlie");
    // 真实列投影
    assert_eq!(rows[0].template_key, "k1");
    assert_eq!(rows[0].status, "active");
    assert_eq!(rows[0].command, "echo");
    assert_eq!(rows[0].args, json!([]));
}

/// 2. create_stdio_template：args/env_keys/tools 真实写入 jsonb
#[tokio::test(flavor = "current_thread")]
async fn tool_stdio_template_repo_create_persists_jsonb_arrays() {
    let db = db().await;
    let repo = ToolRepo::new(&db);
    let cid = insert_company(&db, "create").await;

    let row = repo
        .create_stdio_template(&NewToolStdioTemplate {
            company_id: cid,
            template_key: "k".into(),
            name: "MyT".into(),
            description: Some("hello".into()),
            command: "bash".into(),
            args: json!(["-c", "echo hi"]),
            env_keys: json!(["PATH", "HOME"]),
            tools: json!([{"name": "echo", "risk": "read"}]),
            created_by_agent_id: None,
            created_by_user_id: Some("u-test".into()),
        })
        .await
        .expect("create");

    assert_eq!(row.command, "bash");
    assert_eq!(row.args[0], "-c");
    assert_eq!(row.args[1], "echo hi");
    assert_eq!(row.env_keys[1], "HOME");
    assert_eq!(row.tools[0]["name"], "echo");
    assert_eq!(row.created_by_user_id.as_deref(), Some("u-test"));
}

/// 3. find_stdio_template_id_by_name：name 冲突检测
#[tokio::test(flavor = "current_thread")]
async fn tool_stdio_template_repo_find_by_name_for_conflict() {
    let db = db().await;
    let repo = ToolRepo::new(&db);
    let cid = insert_company(&db, "find").await;
    let row = repo
        .create_stdio_template(&NewToolStdioTemplate {
            company_id: cid,
            template_key: "k".into(),
            name: "DupName".into(),
            description: None,
            command: "x".into(),
            args: json!([]),
            env_keys: json!([]),
            tools: json!([]),
            created_by_agent_id: None,
            created_by_user_id: None,
        })
        .await
        .expect("create");

    let found = repo
        .find_stdio_template_id_by_name(cid, "DupName")
        .await
        .expect("find")
        .expect("present");
    assert_eq!(found, row.id);

    let none = repo
        .find_stdio_template_id_by_name(cid, "OtherName")
        .await
        .expect("find other");
    assert!(none.is_none());
}

/// 4. disable_stdio_template：按 UUID 禁用，写 disabled_at，不写 disabled_reason（schema 无此列）
#[tokio::test(flavor = "current_thread")]
async fn tool_stdio_template_repo_disable_by_uuid() {
    let db = db().await;
    let repo = ToolRepo::new(&db);
    let cid = insert_company(&db, "disable-uuid").await;
    let row = repo
        .create_stdio_template(&NewToolStdioTemplate {
            company_id: cid,
            template_key: "k".into(),
            name: "ToDisable".into(),
            description: None,
            command: "x".into(),
            args: json!([]),
            env_keys: json!([]),
            tools: json!([]),
            created_by_agent_id: None,
            created_by_user_id: None,
        })
        .await
        .expect("create");

    let n = repo
        .disable_stdio_template(cid, &row.id.to_string())
        .await
        .expect("disable");
    assert!(n);

    // 反查 disabled_at
    let (disabled_at,): (Option<pc_core::Timestamp>,) =
        sqlx::query_as("SELECT disabled_at FROM tool_stdio_command_templates WHERE id = $1")
            .bind(row.id)
            .fetch_one(db.pool())
            .await
            .expect("query");
    assert!(disabled_at.is_some());

    // 验证不存在 disabled_reason 列（schema 漂移防御）
    let bad_col: Vec<(String,)> = sqlx::query_as(
        "SELECT column_name FROM information_schema.columns              WHERE table_name = 'tool_stdio_command_templates' AND column_name = 'disabled_reason'",
    )
    .fetch_all(db.pool())
    .await
    .expect("query cols");
    assert!(bad_col.is_empty(), "schema leak: disabled_reason column found");
}

/// 5. disable_stdio_template：按 template_key 兜底
#[tokio::test(flavor = "current_thread")]
async fn tool_stdio_template_repo_disable_by_template_key() {
    let db = db().await;
    let repo = ToolRepo::new(&db);
    let cid = insert_company(&db, "disable-key").await;
    let row = repo
        .create_stdio_template(&NewToolStdioTemplate {
            company_id: cid,
            template_key: "my-key".into(),
            name: "ByKey".into(),
            description: None,
            command: "x".into(),
            args: json!([]),
            env_keys: json!([]),
            tools: json!([]),
            created_by_agent_id: None,
            created_by_user_id: None,
        })
        .await
        .expect("create");

    let n = repo.disable_stdio_template(cid, "my-key").await.expect("disable by key");
    assert!(n);

    let after = repo
        .list_stdio_templates_by_company(cid)
        .await
        .expect("list")
        .into_iter()
        .find(|r| r.id == row.id)
        .expect("still exists");
    assert!(after.disabled_at.is_some());
}

/// 6. validation：空 name / command / template_key 必须拒绝
#[tokio::test(flavor = "current_thread")]
async fn tool_stdio_template_repo_validation_rejects_empty_fields() {
    let db = db().await;
    let repo = ToolRepo::new(&db);
    let cid = insert_company(&db, "val").await;

    // 空 name
    let bad1 = repo
        .create_stdio_template(&NewToolStdioTemplate {
            company_id: cid,
            template_key: "k".into(),
            name: "".into(),
            description: None,
            command: "x".into(),
            args: json!([]),
            env_keys: json!([]),
            tools: json!([]),
            created_by_agent_id: None,
            created_by_user_id: None,
        })
        .await;
    assert!(bad1.is_err(), "empty name rejected");

    // 空 command
    let bad2 = repo
        .create_stdio_template(&NewToolStdioTemplate {
            company_id: cid,
            template_key: "k".into(),
            name: "n".into(),
            description: None,
            command: "".into(),
            args: json!([]),
            env_keys: json!([]),
            tools: json!([]),
            created_by_agent_id: None,
            created_by_user_id: None,
        })
        .await;
    assert!(bad2.is_err(), "empty command rejected");

    // 空 template_key
    let bad3 = repo
        .create_stdio_template(&NewToolStdioTemplate {
            company_id: cid,
            template_key: "".into(),
            name: "n".into(),
            description: None,
            command: "x".into(),
            args: json!([]),
            env_keys: json!([]),
            tools: json!([]),
            created_by_agent_id: None,
            created_by_user_id: None,
        })
        .await;
    assert!(bad3.is_err(), "empty template_key rejected");
}

/// 7. disable_stdio_template：重复禁用（disabled_at IS NOT NULL）返回 false
#[tokio::test(flavor = "current_thread")]
async fn tool_stdio_template_repo_disable_idempotent() {
    let db = db().await;
    let repo = ToolRepo::new(&db);
    let cid = insert_company(&db, "idem").await;
    let row = repo
        .create_stdio_template(&NewToolStdioTemplate {
            company_id: cid,
            template_key: "k".into(),
            name: "Idem".into(),
            description: None,
            command: "x".into(),
            args: json!([]),
            env_keys: json!([]),
            tools: json!([]),
            created_by_agent_id: None,
            created_by_user_id: None,
        })
        .await
        .expect("create");

    let first = repo
        .disable_stdio_template(cid, &row.id.to_string())
        .await
        .expect("1st");
    assert!(first);

    let second = repo
        .disable_stdio_template(cid, &row.id.to_string())
        .await
        .expect("2nd");
    assert!(!second, "second disable must return false (already disabled)");
}
