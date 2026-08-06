//! Round 200 集成测试：built-in agents routines / provision 仓储语义。
//!
//! 覆盖：
//! - `AgentRepo::install_built_in` 幂等插入
//! - `AgentRepo::find_built_in_agent_id` 按 metadata.builtInKey 查找
//! - routine_triggers 表 enabled flag 更新语义（仓储层：sqlx::query）

use pc_db::Db;
use pc_repos::agent::AgentRepo;
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
        .bind(format!("r200-{tag}-{id}"))
        .bind(format!("R200{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("company");
    id
}

// ===== 1) install_built_in: 首次插入 → 返回 Some(agent_id) =====
#[tokio::test(flavor = "current_thread")]
async fn install_built_in_first_returns_some() {
    let db = db().await;
    let cid = insert_company(&db, "inst1").await;
    let repo = AgentRepo::new(&db);

    let metadata = json!({"builtInKey": "code-reviewer", "source": "built-in"});
    let id = repo
        .install_built_in(cid, "Code Reviewer", "reviewer", &metadata)
        .await
        .expect("install");
    assert!(id.is_some(), "first install must return Some(agent_id)");

    // Verify row exists
    let row = repo
        .find_built_in_agent_id(cid, "code-reviewer")
        .await
        .expect("find");
    assert_eq!(row, id);
}

// ===== 2) install_built_in: 重复同 key → None（幂等） =====
#[tokio::test(flavor = "current_thread")]
async fn install_built_in_idempotent() {
    let db = db().await;
    let cid = insert_company(&db, "inst2").await;
    let repo = AgentRepo::new(&db);

    let metadata = json!({"builtInKey": "doc-writer", "source": "built-in"});
    let id1 = repo
        .install_built_in(cid, "Doc Writer", "writer", &metadata)
        .await
        .expect("install 1");
    let id2 = repo
        .install_built_in(cid, "Doc Writer", "writer", &metadata)
        .await
        .expect("install 2");
    assert_eq!(
        id1, id2,
        "second install must return Some(first_id) for idempotency"
    );
}

// ===== 3) install_built_in: 不同 key → 不同 agent =====
#[tokio::test(flavor = "current_thread")]
async fn install_built_in_distinct_keys() {
    let db = db().await;
    let cid = insert_company(&db, "inst3").await;
    let repo = AgentRepo::new(&db);

    let m1 = json!({"builtInKey": "code-reviewer"});
    let m2 = json!({"builtInKey": "issue-triager"});
    let id1 = repo
        .install_built_in(cid, "Reviewer", "reviewer", &m1)
        .await
        .expect("i1");
    let id2 = repo
        .install_built_in(cid, "Triager", "triager", &m2)
        .await
        .expect("i2");
    assert_ne!(id1, id2);
}

// ===== 4) find_built_in_agent_id: 不存在 → None =====
#[tokio::test(flavor = "current_thread")]
async fn find_built_in_agent_id_missing() {
    let db = db().await;
    let cid = insert_company(&db, "find-mis").await;
    let repo = AgentRepo::new(&db);
    let row = repo
        .find_built_in_agent_id(cid, "nope")
        .await
        .expect("find");
    assert!(row.is_none());
}

// ===== 5) install + find 跨 company 隔离 =====
#[tokio::test(flavor = "current_thread")]
async fn install_isolation_between_companies() {
    let db = db().await;
    let c1 = insert_company(&db, "iso1").await;
    let c2 = insert_company(&db, "iso2").await;
    let repo = AgentRepo::new(&db);
    let m = json!({"builtInKey": "code-reviewer"});

    let id1 = repo
        .install_built_in(c1, "Reviewer", "reviewer", &m)
        .await
        .expect("i1")
        .expect("must insert");
    let id2 = repo
        .install_built_in(c2, "Reviewer", "reviewer", &m)
        .await
        .expect("i2")
        .expect("must insert");
    assert_ne!(id1, id2);

    let f1 = repo
        .find_built_in_agent_id(c1, "code-reviewer")
        .await
        .expect("f1");
    let f2 = repo
        .find_built_in_agent_id(c2, "code-reviewer")
        .await
        .expect("f2");
    assert_eq!(f1, Some(id1));
    assert_eq!(f2, Some(id2));
}

// ===== 6) routine_triggers: enabled flag 切换 =====
#[tokio::test(flavor = "current_thread")]
async fn routine_trigger_enable_disable() {
    let db = db().await;
    let cid = insert_company(&db, "rt1").await;
    let repo = AgentRepo::new(&db);
    let m = json!({"builtInKey": "issue-triager"});
    let agent_id = repo
        .install_built_in(cid, "Triager", "triager", &m)
        .await
        .expect("install")
        .expect("present");

    // Create routine
    let routine_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO routines (id, company_id, project_id, assignee_agent_id, title, status) \
         VALUES ($1, $2, gen_random_uuid(), $3, 'daily-triage', 'active')",
    )
    .bind(routine_id)
    .bind(cid)
    .bind(agent_id)
    .execute(db.pool())
    .await
    .expect("routine");

    // Create trigger (default enabled=true)
    let trigger_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO routine_triggers (id, company_id, routine_id, kind, enabled) \
         VALUES ($1, $2, $3, 'cron', true)",
    )
    .bind(trigger_id)
    .bind(cid)
    .bind(routine_id)
    .execute(db.pool())
    .await
    .expect("trigger");

    // Verify default enabled
    let enabled: bool = sqlx::query_scalar("SELECT enabled FROM routine_triggers WHERE id = $1")
        .bind(trigger_id)
        .fetch_one(db.pool())
        .await
        .expect("query");
    assert!(enabled);

    // Disable
    let updated = sqlx::query(
        "UPDATE routine_triggers SET enabled = false, updated_at = now() WHERE id = $1",
    )
    .bind(trigger_id)
    .execute(db.pool())
    .await
    .expect("disable")
    .rows_affected();
    assert_eq!(updated, 1);

    let enabled: bool = sqlx::query_scalar("SELECT enabled FROM routine_triggers WHERE id = $1")
        .bind(trigger_id)
        .fetch_one(db.pool())
        .await
        .expect("query");
    assert!(!enabled);
}
