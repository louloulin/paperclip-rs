use pc_core::Timestamp;
use pc_finance::{FinanceDateRange, FinanceService, NewFinanceEvent};
use pc_repos::Db;
use sqlx::PgPool;
use uuid::Uuid;

const URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";
static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn setup() -> (Db, PgPool) {
    (
        Db::connect(URL, 4, 1).await.unwrap(),
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(4)
            .connect(URL)
            .await
            .unwrap(),
    )
}

async fn company(p: &PgPool) -> Uuid {
    let id = Uuid::new_v4();
    let prefix = format!("FIN{}", &id.simple().to_string()[..6]);
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("fin-{id}"))
    .bind(prefix)
    .execute(p)
    .await
    .unwrap();
    id
}

async fn cleanup(p: &PgPool, cid: Uuid) {
    let _ = sqlx::query("DELETE FROM finance_events WHERE company_id = $1")
        .bind(cid)
        .execute(p)
        .await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(cid)
        .execute(p)
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn create_event_summary_by_biller_by_kind_list() {
    let _g = LOCK.lock().await;
    let (db, p) = setup().await;
    let cid = company(&p).await;
    let svc = FinanceService::new(db);

    for (amount, direction, estimated) in [
        (100i32, Some("debit"), Some(false)),
        (50i32, Some("credit"), None),
        (200i32, Some("debit"), Some(true)),
    ] {
        let new = NewFinanceEvent {
            agent_id: None,
            issue_id: None,
            project_id: None,
            goal_id: None,
            heartbeat_run_id: None,
            cost_event_id: None,
            billing_code: None,
            description: None,
            event_kind: "compute".into(),
            direction: direction.map(String::from),
            biller: "openai".into(),
            provider: Some("openai".into()),
            execution_adapter_type: None,
            pricing_tier: None,
            region: None,
            model: Some("gpt-4o".into()),
            quantity: Some(1),
            unit: Some("call".into()),
            amount_cents: amount,
            currency: None,
            estimated,
            external_invoice_id: None,
            metadata_json: None,
            occurred_at: None,
        };
        svc.create_event(cid, new).await.unwrap();
    }

    let summary = svc.summary(cid, None).await.unwrap();
    assert_eq!(summary.company_id, cid);
    assert_eq!(summary.debit_cents, 300);
    assert_eq!(summary.credit_cents, 50);
    assert_eq!(summary.net_cents, 250);
    assert_eq!(summary.estimated_debit_cents, 200);
    assert_eq!(summary.event_count, 3);

    let by_biller = svc.by_biller(cid, None).await.unwrap();
    assert!(!by_biller.is_empty());
    assert_eq!(by_biller[0].biller, "openai");

    let by_kind = svc.by_kind(cid, None).await.unwrap();
    assert!(!by_kind.is_empty());
    assert_eq!(by_kind[0].event_kind, "compute");

    let listed = svc.list(cid, None, 10).await.unwrap();
    assert_eq!(listed.len(), 3);

    cleanup(&p, cid).await;
}

#[tokio::test(flavor = "current_thread")]
async fn summary_with_date_range() {
    let _g = LOCK.lock().await;
    let (db, p) = setup().await;
    let cid = company(&p).await;
    let svc = FinanceService::new(db);

    for (days_ago, amount) in [(30i64, 1000), (1i64, 100)] {
        let new = NewFinanceEvent {
            agent_id: None,
            issue_id: None,
            project_id: None,
            goal_id: None,
            heartbeat_run_id: None,
            cost_event_id: None,
            billing_code: None,
            description: None,
            event_kind: "compute".into(),
            direction: Some("debit".into()),
            biller: "openai".into(),
            provider: None,
            execution_adapter_type: None,
            pricing_tier: None,
            region: None,
            model: None,
            quantity: None,
            unit: None,
            amount_cents: amount,
            currency: None,
            estimated: None,
            external_invoice_id: None,
            metadata_json: None,
            occurred_at: Some(Timestamp::from_dt(chrono::Utc::now() - chrono::Duration::days(days_ago))),
        };
        svc.create_event(cid, new).await.unwrap();
    }

    let now = chrono::Utc::now();
    let range = FinanceDateRange {
        from: Some(now - chrono::Duration::days(7)),
        to: Some(now),
    };
    let summary = svc.summary(cid, Some(range)).await.unwrap();
    assert_eq!(summary.debit_cents, 100);
    assert_eq!(summary.event_count, 1);

    cleanup(&p, cid).await;
}

#[tokio::test(flavor = "current_thread")]
async fn fk_mismatch_returns_error() {
    let _g = LOCK.lock().await;
    let (db, p) = setup().await;
    let cid = company(&p).await;
    let svc = FinanceService::new(db);

    let new = NewFinanceEvent {
        agent_id: Some(Uuid::new_v4()),
        issue_id: None,
        project_id: None,
        goal_id: None,
        heartbeat_run_id: None,
        cost_event_id: None,
        billing_code: None,
        description: None,
        event_kind: "compute".into(),
        direction: None,
        biller: "x".into(),
        provider: None,
        execution_adapter_type: None,
        pricing_tier: None,
        region: None,
        model: None,
        quantity: None,
        unit: None,
        amount_cents: 1,
        currency: None,
        estimated: None,
        external_invoice_id: None,
        metadata_json: None,
        occurred_at: None,
    };
    let res = svc.create_event(cid, new).await;
    assert!(res.is_err());

    cleanup(&p, cid).await;
}
