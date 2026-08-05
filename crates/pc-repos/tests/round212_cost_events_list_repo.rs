//! Round 212 集成测试：cost_events 列表仓储语义。
//!
//! 覆盖：
//! - `CostRepo::list_cost_events` 按 company_id 过滤 + occurred_at DESC 排序
//! - limit 参数 clamp 到 [1, 500]

use pc_db::Db;
use pc_repos::cost::{CostRepo, CreateCostEvent};
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
        .bind(format!("r212-{tag}-{id}"))
        .bind(format!("R212{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("company");
    id
}

async fn insert_agent(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, status, adapter_type) \
         VALUES ($1, $2, 'r212-agent', 'assistant', 'idle', 'codex_local')",
    )
    .bind(id)
    .bind(company_id)
    .execute(db.pool())
    .await
    .expect("agent");
    id
}

async fn make_cost_event(
    db: &Db,
    company_id: Uuid,
    agent_id: Uuid,
    cost_cents: i32,
) {
    let input = CreateCostEvent {
        agent_id,
        issue_id: None,
        project_id: None,
        goal_id: None,
        heartbeat_run_id: None,
        billing_code: Some("r212".into()),
        provider: "openai".into(),
        biller: "openai".into(),
        billing_type: "api".into(),
        model: "gpt-4o-mini".into(),
        input_tokens: 100,
        cached_input_tokens: 0,
        output_tokens: 50,
        cost_cents,
        occurred_at: chrono::Utc::now(),
    };
    CostRepo::new(db)
        .create_event(company_id, &input)
        .await
        .expect("create");
}

// ===== 1) list_cost_events: 返回空集 =====
#[tokio::test(flavor = "current_thread")]
async fn list_cost_events_empty() {
    let db = db().await;
    let cid = insert_company(&db, "le").await;
    let rows = CostRepo::new(&db)
        .list_cost_events(cid, 100)
        .await
        .expect("list");
    assert!(rows.is_empty());
}

// ===== 2) list_cost_events: 多条按 occurred_at DESC =====
#[tokio::test(flavor = "current_thread")]
async fn list_cost_events_desc_order() {
    let db = db().await;
    let cid = insert_company(&db, "lo").await;
    let aid = insert_agent(&db, cid).await;
    // 插入 3 条 cost events
    make_cost_event(&db, cid, aid, 100).await;
    make_cost_event(&db, cid, aid, 200).await;
    make_cost_event(&db, cid, aid, 300).await;
    let rows = CostRepo::new(&db)
        .list_cost_events(cid, 100)
        .await
        .expect("list");
    assert_eq!(rows.len(), 3);
    // 按 occurred_at DESC：最后插入的应在最前
    assert_eq!(rows[0].cost_cents, 300);
    assert_eq!(rows[2].cost_cents, 100);
}

// ===== 3) list_cost_events: limit clamp 1..=500 =====
#[tokio::test(flavor = "current_thread")]
async fn list_cost_events_limit_clamp() {
    let db = db().await;
    let cid = insert_company(&db, "lc").await;
    let aid = insert_agent(&db, cid).await;
    for i in 0..3 {
        make_cost_event(&db, cid, aid, 10 * (i + 1)).await;
    }
    // limit=0 应被 clamp 到 1
    let r0 = CostRepo::new(&db).list_cost_events(cid, 0).await.expect("c0");
    assert_eq!(r0.len(), 1, "limit=0 should clamp to 1");
    // limit=1000 应被 clamp 到 500（不会报错；只取 3 条）
    let r1 = CostRepo::new(&db).list_cost_events(cid, 1000).await.expect("c1");
    assert_eq!(r1.len(), 3);
}

// ===== 4) list_cost_events: 跨公司隔离 =====
#[tokio::test(flavor = "current_thread")]
async fn list_cost_events_company_isolation() {
    let db = db().await;
    let c1 = insert_company(&db, "iso1").await;
    let c2 = insert_company(&db, "iso2").await;
    let a1 = insert_agent(&db, c1).await;
    let a2 = insert_agent(&db, c2).await;
    make_cost_event(&db, c1, a1, 100).await;
    make_cost_event(&db, c1, a1, 200).await;
    make_cost_event(&db, c2, a2, 999).await;

    let r1 = CostRepo::new(&db).list_cost_events(c1, 100).await.expect("c1");
    let r2 = CostRepo::new(&db).list_cost_events(c2, 100).await.expect("c2");
    assert_eq!(r1.len(), 2);
    assert_eq!(r2.len(), 1);
    assert_eq!(r2[0].cost_cents, 999);
}
