//! BudgetService 业务层 e2e 测试（真实 Postgres）。

use std::sync::Arc;

use pc_budgets::{
    BudgetEnforcementHook, BudgetEnforcementScope, BudgetPolicyStatus, BudgetService,
    BudgetThresholdType, BudgetWindowKind, FullEvaluation, IncidentOutcome, NoopEnforcementHook,
};
use pc_repos::budget::{UpsertPolicyInput};
use sqlx::PgPool;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn setup_pool() -> PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(TEST_DATABASE_URL)
        .await
        .expect("connect to postgres")
}

async fn setup_db(pool: &PgPool) -> pc_repos::Db {
    pc_repos::Db::from_pool(pool.clone())
}

async fn insert_company(db: &pc_repos::Db) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("bg-svc-{id}"))
    .bind(format!("BG{}", &id.simple().to_string()[..4]))
    .execute(db.pool())
    .await
    .expect("insert company");
    id
}

async fn upsert_policy(
    db: &pc_repos::Db,
    company_id: Uuid,
    amount: i32,
    warn_percent: i32,
    hard_stop: bool,
    notify: bool,
) -> Uuid {
    let scope_id = Uuid::new_v4();
    let input = UpsertPolicyInput {
        scope_type: "agent".into(),
        scope_id,
        metric: "billed_cents".into(),
        window_kind: "calendar_month_utc".into(),
        amount,
        warn_percent,
        hard_stop_enabled: hard_stop,
        notify_enabled: notify,
        is_active: true,
        updated_by_user_id: Some("user-1".into()),
    };
    pc_repos::budget::BudgetRepo::new(db)
        .upsert_policy(company_id, &input)
        .await
        .expect("upsert policy")
        .id
}

// ---------------------------------------------------------------------------
// CountingHook for hook dispatch verification
// ---------------------------------------------------------------------------

#[derive(Default)]
struct CountingHook {
    hard_stops: std::sync::atomic::AtomicU32,
    warnings: std::sync::atomic::AtomicU32,
    resolves: std::sync::atomic::AtomicU32,
}

#[async_trait::async_trait]
impl BudgetEnforcementHook for CountingHook {
    async fn on_hard_stop(&self, _: &BudgetEnforcementScope) -> pc_budgets::BudgetResult<()> {
        self.hard_stops.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    async fn on_warning(&self, _: &BudgetEnforcementScope) -> pc_budgets::BudgetResult<()> {
        self.warnings.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    async fn on_resolve(&self, _: &BudgetEnforcementScope) -> pc_budgets::BudgetResult<()> {
        self.resolves.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

async fn fetch_policy(db: &pc_repos::Db, id: Uuid) -> pc_repos::budget::PolicyRow {
    sqlx::query_as::<_, pc_repos::budget::PolicyRow>(
        "SELECT id, company_id, scope_type, scope_id, metric, window_kind, amount, \
                warn_percent, hard_stop_enabled, notify_enabled, is_active, \
                created_by_user_id, updated_by_user_id, created_at, updated_at \
         FROM budget_policies WHERE id = $1",
    )
    .bind(id)
    .fetch_one(db.pool())
    .await
    .expect("fetch policy")
}

async fn count_incidents(db: &pc_repos::Db, policy_id: Uuid) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM budget_incidents WHERE policy_id = $1",
    )
    .bind(policy_id)
    .fetch_one(db.pool())
    .await
    .expect("count incidents")
}

// ---------------------------------------------------------------------------
// E2E Tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn r582_e2e_upsert_and_list_policy() {
    let pool = setup_pool().await;
    let db = setup_db(&pool).await;
    let company_id = insert_company(&db).await;
    let svc = BudgetService::new(&db);

    let id = upsert_policy(&db, company_id, 10000, 80, true, true).await;
    let policies = svc.list_policies(company_id).await.expect("list");
    let ids: Vec<Uuid> = policies.iter().map(|p| p.id).collect();
    assert!(ids.contains(&id));
    assert_eq!(policies.iter().find(|p| p.id == id).unwrap().amount, 10000);
}

#[tokio::test(flavor = "current_thread")]
async fn r582_e2e_upsert_idempotent_same_key_updates() {
    let pool = setup_pool().await;
    let db = setup_db(&pool).await;
    let company_id = insert_company(&db).await;
    let svc = BudgetService::new(&db);

    let scope_id = Uuid::new_v4();
    let mk_input = |amount: i32| UpsertPolicyInput {
        scope_type: "agent".into(),
        scope_id,
        metric: "billed_cents".into(),
        window_kind: "calendar_month_utc".into(),
        amount,
        warn_percent: 80,
        hard_stop_enabled: true,
        notify_enabled: true,
        is_active: true,
        updated_by_user_id: Some("user-1".into()),
    };
    let input1 = mk_input(1000);
    let row1 = svc.upsert_policy(company_id, input1).await.expect("upsert 1");
    let input2 = mk_input(5000);
    let row2 = svc.upsert_policy(company_id, input2).await.expect("upsert 2");
    assert_eq!(row1.id, row2.id, "same scope -> same policy id");
    assert_eq!(row2.amount, 5000);
}

#[tokio::test(flavor = "current_thread")]
async fn r582_e2e_upsert_rejects_invalid_window_kind() {
    let pool = setup_pool().await;
    let db = setup_db(&pool).await;
    let company_id = insert_company(&db).await;
    let svc = BudgetService::new(&db);

    let input = UpsertPolicyInput {
        scope_type: "agent".into(),
        scope_id: Uuid::new_v4(),
        metric: "billed_cents".into(),
        window_kind: "bogus_window".into(),
        amount: 1000,
        warn_percent: 80,
        hard_stop_enabled: true,
        notify_enabled: true,
        is_active: true,
        updated_by_user_id: Some("user-1".into()),
    };
    let err = svc.upsert_policy(company_id, input).await.expect_err("should reject");
    assert!(matches!(err, pc_budgets::BudgetError::InvalidWindowKind(_)));
}

#[tokio::test(flavor = "current_thread")]
async fn r582_e2e_evaluate_full_writes_incident_and_triggers_hook() {
    let pool = setup_pool().await;
    let db = setup_db(&pool).await;
    let company_id = insert_company(&db).await;
    let hook = Arc::new(CountingHook::default());
    let svc = BudgetService::with_hooks(&db, vec![hook.clone()]);

    // 1. 创建 policy
    let policy_id = upsert_policy(&db, company_id, 1000, 80, true, true).await;
    let policy = fetch_policy(&db, policy_id).await;

    // 2. 评估 observed=900（达到警告阈值） → Warning
    let eval = svc
        .evaluate_full(&policy, 900, chrono::Utc::now())
        .await
        .expect("evaluate full");
    assert_eq!(eval.status, BudgetPolicyStatus::Warning);
    assert!(eval.incident.is_some(), "should create soft incident");
    assert!(eval.hook_triggered, "should trigger warning hook");
    assert_eq!(hook.warnings.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(hook.hard_stops.load(std::sync::atomic::Ordering::SeqCst), 0);

    // 3. DB 中应有 1 个 soft incident
    assert_eq!(count_incidents(&db, policy_id).await, 1);
}

#[tokio::test(flavor = "current_thread")]
async fn r582_e2e_evaluate_full_hard_stop_triggers_hard_hook() {
    let pool = setup_pool().await;
    let db = setup_db(&pool).await;
    let company_id = insert_company(&db).await;
    let hook = Arc::new(CountingHook::default());
    let svc = BudgetService::with_hooks(&db, vec![hook.clone()]);

    let policy_id = upsert_policy(&db, company_id, 1000, 80, true, true).await;
    let policy = fetch_policy(&db, policy_id).await;

    let eval = svc
        .evaluate_full(&policy, 1500, chrono::Utc::now())
        .await
        .expect("evaluate full");
    assert_eq!(eval.status, BudgetPolicyStatus::HardStop);
    assert!(eval.hook_triggered);
    assert_eq!(hook.hard_stops.load(std::sync::atomic::Ordering::SeqCst), 1);
    // hard_stop 触发 1 个 hard incident（不会先创建 soft）
    assert_eq!(count_incidents(&db, policy_id).await, 1);
}

#[tokio::test(flavor = "current_thread")]
async fn r582_e2e_evaluate_full_below_writes_no_incident() {
    let pool = setup_pool().await;
    let db = setup_db(&pool).await;
    let company_id = insert_company(&db).await;
    let hook = Arc::new(CountingHook::default());
    let svc = BudgetService::with_hooks(&db, vec![hook.clone()]);

    let policy_id = upsert_policy(&db, company_id, 1000, 80, true, true).await;
    let policy = fetch_policy(&db, policy_id).await;

    let eval = svc
        .evaluate_full(&policy, 500, chrono::Utc::now())
        .await
        .expect("evaluate full");
    assert_eq!(eval.status, BudgetPolicyStatus::Ok);
    assert!(eval.incident.is_none());
    assert!(!eval.hook_triggered);
    assert_eq!(count_incidents(&db, policy_id).await, 0);
    assert_eq!(hook.warnings.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn r582_e2e_evaluate_full_idempotent_incident_creation() {
    let pool = setup_pool().await;
    let db = setup_db(&pool).await;
    let company_id = insert_company(&db).await;
    let hook = Arc::new(CountingHook::default());
    let svc = BudgetService::with_hooks(&db, vec![hook.clone()]);

    let policy_id = upsert_policy(&db, company_id, 1000, 80, true, false).await;
    let policy = fetch_policy(&db, policy_id).await;

    // 第一次
    let eval1 = svc
        .evaluate_full(&policy, 900, chrono::Utc::now())
        .await
        .expect("evaluate 1");
    assert!(matches!(eval1.incident.unwrap(), IncidentOutcome::Created(_)));

    // 第二次 — 应返回 AlreadyExists，不重复创建
    let eval2 = svc
        .evaluate_full(&policy, 950, chrono::Utc::now())
        .await
        .expect("evaluate 2");
    assert!(matches!(eval2.incident.unwrap(), IncidentOutcome::AlreadyExists(_)));

    assert_eq!(count_incidents(&db, policy_id).await, 1, "should still have 1 incident");
}

#[tokio::test(flavor = "current_thread")]
async fn r582_e2e_resolve_incident_triggers_resolve_hook() {
    let pool = setup_pool().await;
    let db = setup_db(&pool).await;
    let company_id = insert_company(&db).await;
    let hook = Arc::new(CountingHook::default());
    let svc = BudgetService::with_hooks(&db, vec![hook.clone()]);

    let policy_id = upsert_policy(&db, company_id, 1000, 80, true, true).await;
    let policy = fetch_policy(&db, policy_id).await;
    let eval = svc
        .evaluate_full(&policy, 1500, chrono::Utc::now())
        .await
        .expect("evaluate");
    let incident_id = match eval.incident.unwrap() {
        IncidentOutcome::Created(row) | IncidentOutcome::AlreadyExists(row) => row.id,
    };

    // 解决 incident
    let input = pc_repos::budget::ResolveIncidentInput {
        action: "acknowledge".into(),
        amount: None,
        decision_note: Some("false positive".into()),
    };
    let resolved = svc
        .resolve_incident(company_id, incident_id, input)
        .await
        .expect("resolve");
    assert!(resolved.is_some());
    assert_eq!(resolved.unwrap().status, "resolved");

    assert_eq!(hook.resolves.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn r582_e2e_list_open_attention_returns_unresolved() {
    let pool = setup_pool().await;
    let db = setup_db(&pool).await;
    let company_id = insert_company(&db).await;
    let svc = BudgetService::new(&db);

    let policy_id = upsert_policy(&db, company_id, 1000, 80, true, true).await;
    let policy = fetch_policy(&db, policy_id).await;
    svc.evaluate_full(&policy, 1500, chrono::Utc::now())
        .await
        .expect("evaluate");

    let open = svc.list_open_attention(company_id).await.expect("list open");
    let open_for_policy: Vec<_> = open.iter().filter(|i| i.policy_id == policy_id).collect();
    assert!(!open_for_policy.is_empty(), "should have open incidents for our policy");
}

#[tokio::test(flavor = "current_thread")]
async fn r582_e2e_threshold_type_mapping_in_evaluate_full() {
    // 验证 evaluate_full 内部把 status 映射到正确 threshold_type
    // 这是 pure logic 测试，但走 e2e 验证 DB 落库
    let pool = setup_pool().await;
    let db = setup_db(&pool).await;
    let company_id = insert_company(&db).await;
    let svc = BudgetService::new(&db);

    let policy_id = upsert_policy(&db, company_id, 1000, 80, true, true).await;
    let policy = fetch_policy(&db, policy_id).await;
    let eval = svc
        .evaluate_full(&policy, 900, chrono::Utc::now())
        .await
        .expect("evaluate");
    let incident = match eval.incident.unwrap() {
        IncidentOutcome::Created(row) => row,
        IncidentOutcome::AlreadyExists(row) => row,
    };
    assert_eq!(incident.threshold_type, "soft");

    // 再评估 hard stop
    let eval2 = svc
        .evaluate_full(&policy, 1500, chrono::Utc::now())
        .await
        .expect("evaluate 2");
    let incident2 = match eval2.incident.unwrap() {
        IncidentOutcome::Created(row) => row,
        IncidentOutcome::AlreadyExists(row) => row,
    };
    assert_eq!(incident2.threshold_type, "hard");
}

#[tokio::test(flavor = "current_thread")]
async fn r582_e2e_get_incident_returns_stored_row() {
    let pool = setup_pool().await;
    let db = setup_db(&pool).await;
    let company_id = insert_company(&db).await;
    let svc = BudgetService::new(&db);

    let policy_id = upsert_policy(&db, company_id, 1000, 80, true, true).await;
    let policy = fetch_policy(&db, policy_id).await;
    let eval = svc
        .evaluate_full(&policy, 1500, chrono::Utc::now())
        .await
        .expect("evaluate");
    let created_id = match eval.incident.unwrap() {
        IncidentOutcome::Created(row) => row.id,
        IncidentOutcome::AlreadyExists(row) => row.id,
    };

    let got = svc
        .get_incident(company_id, created_id)
        .await
        .expect("get incident");
    assert!(got.is_some());
    assert_eq!(got.unwrap().id, created_id);
}

#[tokio::test(flavor = "current_thread")]
async fn r582_e2e_multiple_hooks_all_triggered() {
    let pool = setup_pool().await;
    let db = setup_db(&pool).await;
    let company_id = insert_company(&db).await;
    let h1 = Arc::new(CountingHook::default());
    let h2 = Arc::new(CountingHook::default());
    let svc = BudgetService::with_hooks(&db, vec![h1.clone(), h2.clone()]);

    let policy_id = upsert_policy(&db, company_id, 1000, 80, true, true).await;
    let policy = fetch_policy(&db, policy_id).await;
    svc.evaluate_full(&policy, 1500, chrono::Utc::now())
        .await
        .expect("evaluate");

    assert_eq!(h1.hard_stops.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(h2.hard_stops.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn r582_e2e_noop_hook_works() {
    let pool = setup_pool().await;
    let db = setup_db(&pool).await;
    let company_id = insert_company(&db).await;
    let svc = BudgetService::with_hooks(&db, vec![Arc::new(NoopEnforcementHook)]);

    let policy_id = upsert_policy(&db, company_id, 1000, 80, true, true).await;
    let policy = fetch_policy(&db, policy_id).await;
    let eval = svc
        .evaluate_full(&policy, 1500, chrono::Utc::now())
        .await
        .expect("evaluate");
    assert_eq!(eval.status, BudgetPolicyStatus::HardStop);
    assert!(eval.hook_triggered);
}

#[tokio::test(flavor = "current_thread")]
async fn r582_e2e_add_hook_builder() {
    let pool = setup_pool().await;
    let db = setup_db(&pool).await;
    let svc = BudgetService::new(&db)
        .add_hook(Arc::new(NoopEnforcementHook))
        .add_hook(Arc::new(NoopEnforcementHook));
    assert_eq!(svc.hook_count(), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn r582_e2e_budget_window_kind_roundtrip_in_db() {
    // 验证 DB 存的 window_kind 字符串可以解析回 enum
    use std::str::FromStr;
    let kind = BudgetWindowKind::CalendarMonthUtc;
    let s = kind.as_str();
    assert_eq!(BudgetWindowKind::from_str(s).ok(), Some(kind));
    assert!(BudgetWindowKind::from_str("bogus").is_err());
}
