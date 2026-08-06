//! Round 105 集成测试：验证 `pc_repos::ToolRepo` 在 `tool_action_requests` 上的真实 schema 路径。
//!
//! 真实表 schema (0149_agent_access_phase2_contracts.sql)：
//!   tool_action_requests(
//!     id, company_id, invocation_id, issue_id, interaction_id, approval_id,
//!     status, canonical_arguments_hash, canonical_arguments_summary,
//!     signed_arguments, preview_markdown,
//!     requested_by_agent_id, requested_by_user_id,
//!     resolved_by_agent_id, resolved_by_user_id,
//!     decided_by_agent_id, decided_by_user_id,
//!     decided_at, expires_at, resolved_at,
//!     created_at, updated_at
//!   )
//!
//! **不存在**的列：`action_kind / requested_by / payload`

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
        .bind(format!("r105-{tag}-{id}"))
        .bind(format!("R105{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("insert company");
    id
}

async fn insert_action_request(db: &Db, company_id: Uuid, status: &str) -> Uuid {
    let id = Uuid::new_v4();
    let inv_id = Uuid::new_v4();
    let canonical_summary = json!({"action_name": "stripe.refund", "amount": 100});
    sqlx::query(
        "INSERT INTO tool_action_requests             (company_id, invocation_id, status, canonical_arguments_hash, canonical_arguments_summary) \
         VALUES ($1, $2, $3, 'aabbcc', $4)",
    )
    .bind(company_id)
    .bind(inv_id)
    .bind(status)
    .bind(&canonical_summary)
    .execute(db.pool())
    .await
    .expect("insert action request");
    id
}

/// 1. list_action_requests_by_company：按 created_at DESC + 真实列投影
#[tokio::test(flavor = "current_thread")]
async fn tool_action_request_repo_list_orders_by_created_at_desc() {
    let db = db().await;
    let repo = ToolRepo::new(&db);
    let cid = insert_company(&db, "list").await;
    for s in ["pending", "pending", "approved"] {
        insert_action_request(&db, cid, s).await;
    }
    let rows = repo
        .list_action_requests_by_company(cid, 100)
        .await
        .expect("list");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].status, "approved"); // 最近
                                            // 真实列投影
    for r in &rows {
        assert!(!r.canonical_arguments_hash.is_empty());
        assert_eq!(
            r.canonical_arguments_summary["action_name"],
            "stripe.refund"
        );
    }
}

/// 2. get_action_request：精确 (company_id, id) 二元查找
#[tokio::test(flavor = "current_thread")]
async fn tool_action_request_repo_get_by_company_and_id() {
    let db = db().await;
    let cid = insert_company(&db, "get").await;
    let aid = insert_action_request(&db, cid, "pending").await;

    let row = repo_get(&ToolRepo::new(&db), cid, aid)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(row.status, "pending");
    assert!(!row.canonical_arguments_hash.is_empty());

    // 跨 company 查不到
    let other = insert_company(&db, "other").await;
    let none = ToolRepo::new(&db)
        .get_action_request(other, aid)
        .await
        .expect("get other");
    assert!(none.is_none());
}

async fn repo_get<'a>(
    repo: &'a ToolRepo<'a>,
    cid: Uuid,
    id: Uuid,
) -> Result<Option<pc_repos::tool::ToolActionRequestRow>, pc_repos::RepoError> {
    repo.get_action_request(cid, id).await
}

/// 3. list_action_requests_by_invocation
#[tokio::test(flavor = "current_thread")]
async fn tool_action_request_repo_list_by_invocation() {
    let db = db().await;
    let cid = insert_company(&db, "inv").await;
    let inv_id = Uuid::new_v4();

    // 同一 invocation 下 2 个请求 + 1 个不同 invocation
    for _ in 0..2 {
        sqlx::query(
            "INSERT INTO tool_action_requests                 (company_id, invocation_id, status, canonical_arguments_hash, canonical_arguments_summary) \
             VALUES ($1, $2, 'pending', 'h1', '{}'::jsonb)",
        )
        .bind(cid)
        .bind(inv_id)
        .execute(db.pool())
        .await
        .expect("insert");
    }
    // 不同 invocation
    sqlx::query(
        "INSERT INTO tool_action_requests             (company_id, invocation_id, status, canonical_arguments_hash, canonical_arguments_summary) \
         VALUES ($1, gen_random_uuid(), 'pending', 'h2', '{}'::jsonb)",
    )
    .bind(cid)
    .execute(db.pool())
    .await
    .expect("insert");

    let rows = ToolRepo::new(&db)
        .list_action_requests_by_invocation(inv_id)
        .await
        .expect("list");
    assert_eq!(rows.len(), 2);
    for r in &rows {
        assert_eq!(r.invocation_id, inv_id);
    }
}

/// 4. 真实 schema 防漂移：action_kind / requested_by / payload 不存在
#[tokio::test(flavor = "current_thread")]
async fn tool_action_requests_table_real_column_audit() {
    let db = db().await;
    let bad: Vec<(String,)> = sqlx::query_as(
        "SELECT column_name FROM information_schema.columns              WHERE table_name='tool_action_requests'              AND column_name IN ('action_kind', 'requested_by', 'payload')",
    )
    .fetch_all(db.pool())
    .await
    .expect("query");
    assert!(
        bad.is_empty(),
        "schema leak: {:?}",
        bad.iter().map(|(c,)| c.clone()).collect::<Vec<_>>()
    );
    // 真实列
    let real: Vec<(String,)> = sqlx::query_as(
        "SELECT column_name FROM information_schema.columns              WHERE table_name='tool_action_requests'              AND column_name IN ('invocation_id', 'canonical_arguments_hash', 'canonical_arguments_summary')",
    )
    .fetch_all(db.pool())
    .await
    .expect("query real");
    let names: std::collections::HashSet<String> = real.into_iter().map(|(s,)| s).collect();
    for must in [
        "invocation_id",
        "canonical_arguments_hash",
        "canonical_arguments_summary",
    ] {
        assert!(names.contains(must), "missing: {must}");
    }
}
