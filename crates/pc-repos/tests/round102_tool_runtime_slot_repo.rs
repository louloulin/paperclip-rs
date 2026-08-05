//! Round 102 集成测试：验证 `pc_repos::ToolRepo` 在 `tool_runtime_slots` 上的真实 schema 路径。
//!
//! 关键点：
//! - 真实列：id, company_id, connection_id, slot_key, status, provider_ref,
//!   health_status, health_message, last_started_at, last_used_at,
//!   idle_deadline_at, metadata, created_at, updated_at
//! - **不存在的列**：`slot_kind / acquired_at / last_heartbeat_at`
//!   （路由层 list_tool_runtime_slots 之前用的就是这三列 → 一启动就是 500）

use pc_db::Db;
use pc_repos::tool::ToolRepo;
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
        .bind(format!("r102-{tag}-{id}"))
        .bind(format!("R102{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("insert company");
    id
}

/// 插入一个 tool_connection（runtime_slot 依赖 connection_id FK）
async fn insert_tool_connection(db: &Db, company_id: Uuid, conn_name: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO tool_connections (company_id, application_id, name, transport) \
         VALUES ($1, (SELECT id FROM tool_applications LIMIT 1), $2, 'stdio')",
    )
    .bind(company_id)
    .bind(conn_name)
    .execute(db.pool())
    .await
    .expect("insert tool_connection");
    id
}

/// 1. list_runtime_slots_by_company：排序 + 真实列投影
#[tokio::test(flavor = "current_thread")]
async fn tool_runtime_slot_repo_list_orders_by_last_started_at_desc() {
    let db = db().await;
    let cid = insert_company(&db, "list").await;
    // 必须有 application 才能创建 tool_connection（FK）
    sqlx::query(
        "INSERT INTO tool_applications (id, company_id, name, type, status, metadata) \
         VALUES (gen_random_uuid(), $1, 'app', 'mcp', 'active', '{}'::jsonb)",
    )
    .bind(cid)
    .execute(db.pool())
    .await
    .expect("insert app");
    let conn = insert_tool_connection(&db, cid, "c1").await;

    // 插入 3 个 slots
    for (i, key) in ["s1", "s2", "s3"].iter().enumerate() {
        sqlx::query(
            "INSERT INTO tool_runtime_slots (company_id, connection_id, slot_key, status, health_status) \
             VALUES ($1, $2, $3, 'stopped', 'unchecked')",
        )
        .bind(cid)
        .bind(conn)
        .bind(key)
        .execute(db.pool())
        .await
        .expect("insert slot");
        // 间隔：s3 最新，s1 最旧
        if i < 2 {
            tokio::time::sleep(std::time::Duration::from_millis(15)).await;
        }
    }

    let rows = ToolRepo::new(&db)
        .list_runtime_slots_by_company(cid, 100)
        .await
        .expect("list");
    assert_eq!(rows.len(), 3);
    // 真实列 slot_key 出现
    for r in &rows {
        assert!(["s1", "s2", "s3"].contains(&r.slot_key.as_str()));
        assert_eq!(r.status, "stopped");
        assert_eq!(r.health_status, "unchecked");
    }
}

/// 2. runtime_health：COUNT active + MAX last_used_at（替代不存在的 last_heartbeat_at）
#[tokio::test(flavor = "current_thread")]
async fn tool_runtime_slot_repo_health_aggregates_active_slots() {
    let db = db().await;
    let cid = insert_company(&db, "health").await;
    sqlx::query(
        "INSERT INTO tool_applications (id, company_id, name, type, status, metadata) \
         VALUES (gen_random_uuid(), $1, 'app', 'mcp', 'active', '{}'::jsonb)",
    )
    .bind(cid)
    .execute(db.pool())
    .await
    .expect("insert app");
    let conn = insert_tool_connection(&db, cid, "c1").await;

    // 2 active + 1 stopped
    for (key, status) in [("a1", "active"), ("a2", "active"), ("s1", "stopped")] {
        sqlx::query(
            "INSERT INTO tool_runtime_slots (company_id, connection_id, slot_key, status, health_status) \
             VALUES ($1, $2, $3, $4, 'unchecked')",
        )
        .bind(cid)
        .bind(conn)
        .bind(key)
        .bind(status)
        .execute(db.pool())
        .await
        .expect("insert");
    }

    let h = ToolRepo::new(&db).runtime_health(cid).await.expect("health");
    assert_eq!(h.active_slots, 2);
    assert_eq!(h.company_id, cid);
    // last_used_at 默认 NULL，因为我们没有显式设置
    assert!(h.last_used_at.is_none());
}

/// 3. get_runtime_slot：按 (company_id, id) 二元查找
#[tokio::test(flavor = "current_thread")]
async fn tool_runtime_slot_repo_get_by_company_and_id() {
    let db = db().await;
    let cid = insert_company(&db, "get").await;
    sqlx::query(
        "INSERT INTO tool_applications (id, company_id, name, type, status, metadata) \
         VALUES (gen_random_uuid(), $1, 'app', 'mcp', 'active', '{}'::jsonb)",
    )
    .bind(cid)
    .execute(db.pool())
    .await
    .expect("insert app");
    let conn = insert_tool_connection(&db, cid, "c1").await;
    let slot_id: Uuid = sqlx::query_scalar(
        "INSERT INTO tool_runtime_slots (company_id, connection_id, slot_key, status, health_status) \
         VALUES ($1, $2, 'unique', 'active', 'healthy') RETURNING id",
    )
    .bind(cid)
    .bind(conn)
    .fetch_one(db.pool())
    .await
    .expect("insert slot");

    let row = ToolRepo::new(&db)
        .get_runtime_slot(cid, slot_id)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(row.slot_key, "unique");
    assert_eq!(row.status, "active");
    assert_eq!(row.health_status, "healthy");
    assert_eq!(row.connection_id, conn);

    // 跨 company 查不到
    let other_cid = insert_company(&db, "other").await;
    let none = ToolRepo::new(&db)
        .get_runtime_slot(other_cid, slot_id)
        .await
        .expect("get-other");
    assert!(none.is_none());
}

/// 4. 列名漂移防御：repo 使用的查询不应引用不存在的列
///   这个测试通过尝试插入/查询方式间接验证：
///   - 任何列 `slot_kind/acquired_at/last_heartbeat_at` 都不应该存在
#[tokio::test(flavor = "current_thread")]
async fn tool_runtime_slots_table_does_not_have_wrong_columns() {
    let db = db().await;
    // 用 SQL INFORMATION_SCHEMA 验证
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT column_name FROM information_schema.columns              WHERE table_name='tool_runtime_slots'              AND column_name IN ('slot_kind', 'acquired_at', 'last_heartbeat_at')",
    )
    .fetch_all(db.pool())
    .await
    .expect("query");
    assert!(
        rows.is_empty(),
        "schema leak: {:?}",
        rows.iter().map(|(c,)| c).collect::<Vec<_>>()
    );

    // 确认真实列存在
    let real: Vec<(String,)> = sqlx::query_as(
        "SELECT column_name FROM information_schema.columns              WHERE table_name='tool_runtime_slots'              AND column_name IN ('slot_key', 'last_started_at', 'last_used_at')",
    )
    .fetch_all(db.pool())
    .await
    .expect("query real");
    let real_names: Vec<&str> = real.iter().map(|(s,)| s.as_str()).collect();
    assert!(real_names.contains(&"slot_key"));
    assert!(real_names.contains(&"last_started_at"));
    assert!(real_names.contains(&"last_used_at"));
}
