//! Round 106 集成测试：验证 `pc_repos::CaseRepo` 在 `case_events` 上的真实 schema 路径。
//!
//! 真实表 schema (0143_cases_foundation.sql)：
//!   case_events(
//!     id, company_id, case_id, kind, actor_type, actor_user_id, actor_agent_id,
//!     run_id, payload, created_at, updated_at
//!   )
//!
//! 这些测试直接验证 CaseRepo::list_events_by_case_id 的纯 id-based 查询路径
//! （不强制 company_id），用于 `GET /api/cases/:case_id/events` 端点。

use pc_db::Db;
use pc_repos::case::{CaseActor, CaseEventKind, CaseRepo};
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
        .bind(format!("r106-{tag}-{id}"))
        .bind(format!("R106{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("insert company");
    id
}

/// 在 case_events 写入最少必需字段（绕过 Case + FK CASCADE）
async fn seed_case_event(db: &Db, company_id: Uuid, case_id: Uuid, kind: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO case_events (company_id, case_id, kind, actor_type, payload) \
         VALUES ($1, $2, $3, 'system', '{}'::jsonb)",
    )
    .bind(company_id)
    .bind(case_id)
    .bind(kind)
    .execute(db.pool())
    .await
    .expect("insert event");
    id
}


/// 1. list_events_by_case_id：按 case_id 单查，按 created_at DESC 排序
#[tokio::test(flavor = "current_thread")]
async fn case_events_repo_list_by_case_id_orders_recent_first() {
    let db = db().await;
    let repo = CaseRepo::new(&db);
    let cid = insert_company(&db, "case1").await;
    let case_id = Uuid::new_v4();
    // seed 3 events (FK 不强制 case 存在，所以可以直接 insert case_events)
    for kind in ["created", "fields_changed", "updated"] {
        seed_case_event(&db, cid, case_id, kind).await;
        tokio::time::sleep(std::time::Duration::from_millis(15)).await;
    }
    let rows = repo
        .list_events_by_case_id(case_id, 100)
        .await
        .expect("list");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].kind, "updated"); // 最新
}

/// 2. 跨 case 查询隔离：case_b 不会出现在 case_a 的列表
#[tokio::test(flavor = "current_thread")]
async fn case_events_repo_list_filters_by_case_id() {
    let db = db().await;
    let repo = CaseRepo::new(&db);
    let cid = insert_company(&db, "case2").await;
    let case_a = Uuid::new_v4();
    let case_b = Uuid::new_v4();

    seed_case_event(&db, cid, case_a, "created").await;
    seed_case_event(&db, cid, case_b, "created").await;
    seed_case_event(&db, cid, case_b, "updated").await;

    let a_rows = repo.list_events_by_case_id(case_a, 100).await.expect("list a");
    let b_rows = repo.list_events_by_case_id(case_b, 100).await.expect("list b");

    assert_eq!(a_rows.len(), 1, "case_a events only");
    assert_eq!(b_rows.len(), 2, "case_b events only");
    assert_eq!(a_rows[0].case_id, case_a);
    assert!(b_rows.iter().all(|r| r.case_id == case_b));
}

/// 3. limit clamp：list_events_by_case_id 把 limit 强制 clamp 到 [1, 500]
#[tokio::test(flavor = "current_thread")]
async fn case_events_repo_list_clamps_limit() {
    let db = db().await;
    let repo = CaseRepo::new(&db);
    let cid = insert_company(&db, "limit").await;
    let case_id = Uuid::new_v4();
    seed_case_event(&db, cid, case_id, "created").await;

    // limit=0 应该被 clamp 到 1
    let rows = repo
        .list_events_by_case_id(case_id, 0)
        .await
        .expect("list");
    assert_eq!(rows.len(), 1);

    // limit=99999 应该被 clamp 到 500（因为只有一个 event,应该返回 1）
    let rows = repo
        .list_events_by_case_id(case_id, 99_999)
        .await
        .expect("list");
    assert_eq!(rows.len(), 1);
}

/// 4. create_event：通过 CaseRepo::create_event 插入，验证落库完整
#[tokio::test(flavor = "current_thread")]
async fn case_events_repo_create_event_uses_real_columns() {
    let db = db().await;
    let repo = CaseRepo::new(&db);
    let cid = insert_company(&db, "create").await;
    // 真实 schema 有 FK: case_events.case_id → cases.id，所以必须先 insert case
    let case_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO cases (company_id, case_number, identifier, case_type, key, title, status) \
         VALUES ($1, 1, 'CASE-001', 'requirement', NULL, 'test', 'draft')",
    )
    .bind(cid)
    .execute(db.pool())
    .await
    .expect("insert case");

    let actor = CaseActor {
        actor_type: pc_repos::case::CaseActorType::User,
        actor_user_id: Some("u-test".into()),
        actor_agent_id: None,
        run_id: None,
    };
    let row = repo
        .create_event(
            cid,
            case_id,
            CaseEventKind::StatusChanged,
            &actor,
            json!({"from": "draft", "to": "in_progress"}),
        )
        .await
        .expect("create event");

    assert_eq!(row.kind, "status_changed");
    assert_eq!(row.actor_type, "user");
    assert_eq!(row.actor_user_id.as_deref(), Some("u-test"));
    assert_eq!(row.payload["from"], "draft");
    assert_eq!(row.payload["to"], "in_progress");
}
