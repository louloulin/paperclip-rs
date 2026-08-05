//! Round 194 集成测试：budget 域（`pc_repos::budget` 新模块）。
//!
//! 覆盖：
//! - `BudgetRepo::list_policies` — 公司范围查询
//! - `BudgetRepo::upsert_policy` — 新建 + 复合 key 冲突时更新
//! - `BudgetRepo::list_incidents` — 公司事件列表
//! - `BudgetRepo::get_incident` — 单点查询
//! - `BudgetRepo::resolve_incident` — 状态机：open → resolved
//! - 默认值语义：metric / warn_percent / hard_stop_enabled / notify_enabled / is_active

use pc_db::Db;
use pc_repos::budget::{BudgetRepo, ResolveIncidentInput, UpsertPolicyInput};
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
        .bind(format!("r194-{tag}-{id}"))
        .bind(format!("R194{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("company");
    id
}

async fn insert_policy(
    db: &Db,
    company_id: Uuid,
    scope_type: &str,
    scope_id: Uuid,
    metric: &str,
    window_kind: &str,
    amount: i32,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO budget_policies \
            (id, company_id, scope_type, scope_id, metric, window_kind, amount) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(id)
    .bind(company_id)
    .bind(scope_type)
    .bind(scope_id)
    .bind(metric)
    .bind(window_kind)
    .bind(amount)
    .execute(db.pool())
    .await
    .expect("policy");
    id
}

async fn insert_incident(
    db: &Db,
    company_id: Uuid,
    policy_id: Uuid,
    status: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    let scope_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO budget_incidents \
            (id, company_id, policy_id, scope_type, scope_id, metric, window_kind, \
             window_start, window_end, threshold_type, amount_limit, amount_observed, status) \
         VALUES ($1, $2, $3, 'company', $4, 'billed_cents', 'calendar_month_utc', \
                 now(), now() + interval '30 days', 'hard', 100000, 110000, $5)",
    )
    .bind(id)
    .bind(company_id)
    .bind(policy_id)
    .bind(scope_id)
    .bind(status)
    .execute(db.pool())
    .await
    .expect("incident");
    id
}

// ===== 1) list_policies: filter by company =====
#[tokio::test(flavor = "current_thread")]
async fn list_policies_filters_by_company() {
    let db = db().await;
    let c1 = insert_company(&db, "lp-c1").await;
    let c2 = insert_company(&db, "lp-c2").await;
    let sid = Uuid::new_v4();
    insert_policy(&db, c1, "company", sid, "billed_cents", "calendar_month_utc", 100_000).await;
    insert_policy(&db, c1, "company", sid, "billed_cents", "rolling_24h", 10_000).await;
    insert_policy(&db, c2, "company", sid, "billed_cents", "calendar_month_utc", 200_000).await;

    let repo = BudgetRepo::new(&db);
    let rows = repo.list_policies(c1).await.expect("list c1");
    assert_eq!(rows.len(), 2);
    let rows2 = repo.list_policies(c2).await.expect("list c2");
    assert_eq!(rows2.len(), 1);
    assert_eq!(rows2[0].amount, 200_000);
}

#[tokio::test(flavor = "current_thread")]
async fn list_policies_empty_company_returns_empty() {
    let db = db().await;
    let cid = insert_company(&db, "lp-empty").await;
    let repo = BudgetRepo::new(&db);
    let rows = repo.list_policies(cid).await.expect("list");
    assert!(rows.is_empty());
}

// ===== 2) upsert_policy: insert new =====
#[tokio::test(flavor = "current_thread")]
async fn upsert_policy_inserts_new_with_defaults() {
    let db = db().await;
    let cid = insert_company(&db, "up-new").await;
    let scope_id = Uuid::new_v4();
    let repo = BudgetRepo::new(&db);

    let row = repo
        .upsert_policy(
            cid,
            &UpsertPolicyInput {
                scope_type: "company".into(),
                scope_id,
                metric: "billed_cents".into(),
                window_kind: "calendar_month_utc".into(),
                amount: 500_000,
                warn_percent: 0, // not used; default applied
                hard_stop_enabled: false,
                notify_enabled: false,
                is_active: true,
                updated_by_user_id: Some("u-1".into()),
            },
        )
        .await
        .expect("upsert");
    assert_eq!(row.amount, 500_000);
    assert_eq!(row.warn_percent, 80, "default warn_percent");
    assert!(row.hard_stop_enabled);
    assert!(row.notify_enabled);
    assert!(row.is_active);
    assert_eq!(row.metric, "billed_cents");
    assert_eq!(row.updated_by_user_id.as_deref(), Some("u-1"));
}

// ===== 3) upsert_policy: same key → update =====
#[tokio::test(flavor = "current_thread")]
async fn upsert_policy_updates_on_conflict() {
    let db = db().await;
    let cid = insert_company(&db, "up-upd").await;
    let scope_id = Uuid::new_v4();
    let repo = BudgetRepo::new(&db);

    // initial
    let r1 = repo
        .upsert_policy(
            cid,
            &UpsertPolicyInput {
                scope_type: "company".into(),
                scope_id,
                metric: "billed_cents".into(),
                window_kind: "calendar_month_utc".into(),
                amount: 100_000,
                warn_percent: 70,
                hard_stop_enabled: true,
                notify_enabled: true,
                is_active: true,
                updated_by_user_id: Some("u-1".into()),
            },
        )
        .await
        .expect("insert");

    // update same key with new amount
    let r2 = repo
        .upsert_policy(
            cid,
            &UpsertPolicyInput {
                scope_type: "company".into(),
                scope_id,
                metric: "billed_cents".into(),
                window_kind: "calendar_month_utc".into(),
                amount: 200_000,
                warn_percent: 90,
                hard_stop_enabled: false,
                notify_enabled: false,
                is_active: false,
                updated_by_user_id: Some("u-2".into()),
            },
        )
        .await
        .expect("update");

    // Same row (id stable), updated fields
    assert_eq!(r1.id, r2.id);
    assert_eq!(r2.amount, 200_000);
    assert_eq!(r2.warn_percent, 90);
    assert!(!r2.hard_stop_enabled);
    assert_eq!(r2.updated_by_user_id.as_deref(), Some("u-2"));
}

// ===== 4) list_incidents =====
#[tokio::test(flavor = "current_thread")]
async fn list_incidents_filters_by_company() {
    let db = db().await;
    let c1 = insert_company(&db, "li-c1").await;
    let c2 = insert_company(&db, "li-c2").await;
    let p1 = insert_policy(&db, c1, "company", Uuid::new_v4(), "billed_cents", "calendar_month_utc", 100).await;
    let p2 = insert_policy(&db, c2, "company", Uuid::new_v4(), "billed_cents", "calendar_month_utc", 200).await;
    insert_incident(&db, c1, p1, "open").await;
    insert_incident(&db, c1, p1, "resolved").await;
    insert_incident(&db, c2, p2, "open").await;

    let repo = BudgetRepo::new(&db);
    let rows = repo.list_incidents(c1).await.expect("list c1");
    assert_eq!(rows.len(), 2);
    let rows2 = repo.list_incidents(c2).await.expect("list c2");
    assert_eq!(rows2.len(), 1);
}

// ===== 5) get_incident =====
#[tokio::test(flavor = "current_thread")]
async fn get_incident_returns_row() {
    let db = db().await;
    let cid = insert_company(&db, "gi").await;
    let pid = insert_policy(&db, cid, "company", Uuid::new_v4(), "billed_cents", "calendar_month_utc", 100).await;
    let iid = insert_incident(&db, cid, pid, "open").await;
    let repo = BudgetRepo::new(&db);

    let row = repo.get_incident(cid, iid).await.expect("get").expect("exists");
    assert_eq!(row.id, iid);
    assert_eq!(row.status, "open");
    assert!(row.resolved_at.is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn get_incident_missing_returns_none() {
    let db = db().await;
    let cid = insert_company(&db, "gi-m").await;
    let repo = BudgetRepo::new(&db);
    let row = repo.get_incident(cid, Uuid::new_v4()).await.expect("get");
    assert!(row.is_none());
}

// ===== 6) resolve_incident =====
#[tokio::test(flavor = "current_thread")]
async fn resolve_incident_sets_status_resolved() {
    let db = db().await;
    let cid = insert_company(&db, "ri").await;
    let pid = insert_policy(&db, cid, "company", Uuid::new_v4(), "billed_cents", "calendar_month_utc", 100).await;
    let iid = insert_incident(&db, cid, pid, "open").await;
    let repo = BudgetRepo::new(&db);

    let row = repo
        .resolve_incident(
            cid,
            iid,
            &ResolveIncidentInput {
                action: "acknowledge".into(),
                amount: None,
                decision_note: Some("Reviewed manually".into()),
            },
        )
        .await
        .expect("resolve")
        .expect("exists");
    assert_eq!(row.status, "resolved");
    assert!(row.resolved_at.is_some());
}

#[tokio::test(flavor = "current_thread")]
async fn resolve_incident_already_resolved_returns_none() {
    let db = db().await;
    let cid = insert_company(&db, "ri-done").await;
    let pid = insert_policy(&db, cid, "company", Uuid::new_v4(), "billed_cents", "calendar_month_utc", 100).await;
    let iid = insert_incident(&db, cid, pid, "resolved").await;
    let repo = BudgetRepo::new(&db);

    let row = repo
        .resolve_incident(
            cid,
            iid,
            &ResolveIncidentInput {
                action: "acknowledge".into(),
                amount: None,
                decision_note: None,
            },
        )
        .await
        .expect("resolve");
    assert!(row.is_none(), "already-resolved incidents must not re-resolve");
}


