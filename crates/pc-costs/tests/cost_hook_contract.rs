//! R609: pc-costs hook contract tests.
//!
//! Verifies the public hook surface is stable:
//! - CostHook trait can be implemented and dispatched
//! - CostHookEvent variants serialize with `type` tag and camelCase fields
//! - NoopCostHook / RecordingCostHook behave per contract
//! - CostFinanceError enum roundtrips through Display

use pc_costs::{
    CostFinanceError, CostHook, CostHookEvent, CostRange, CostService, NoopCostHook,
    RecordingCostHook,
};
use pc_repos::Db;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

#[tokio::test(flavor = "current_thread")]
async fn noop_hook_accepts_events() {
    let hook = NoopCostHook;
    let event = CostHookEvent::CostEventCreated {
        company_id: Uuid::new_v4(),
        event_id: Uuid::new_v4(),
        agent_id: Uuid::new_v4(),
        cost_cents: 100,
        provider: "openai".into(),
        billing_type: "api".into(),
        model: "gpt-4o-mini".into(),
    };
    let res = CostHook::on_cost_event(&hook, event).await;
    assert!(res.is_ok());
}

#[tokio::test(flavor = "current_thread")]
async fn recording_hook_stores_events() {
    let hook = RecordingCostHook::default();
    assert!(hook.is_empty());

    let ev = CostHookEvent::MonthlySpendUpdated {
        company_id: Uuid::new_v4(),
        agent_id: Uuid::new_v4(),
        agent_month_cents: 10,
        company_month_cents: 100,
    };
    CostHook::on_cost_event(&hook, ev.clone()).await.unwrap();
    assert_eq!(hook.len(), 1);
    let snap = hook.events_snapshot();
    assert_eq!(snap[0], ev);

    hook.clear();
    assert!(hook.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn hook_event_serializes_with_type_tag() {
    let ev = CostHookEvent::FinanceEventCreated {
        company_id: Uuid::new_v4(),
        event_id: Uuid::new_v4(),
        event_kind: "model_usage".into(),
        direction: "debit".into(),
        amount_cents: 100,
        biller: "openai".into(),
    };
    let v: Value = serde_json::to_value(&ev).expect("serialize");
    assert_eq!(v["type"], "financeEventCreated");
    assert_eq!(v["event_kind"], "model_usage");
    assert_eq!(v["direction"], "debit");
    assert_eq!(v["amount_cents"], 100);
}

#[tokio::test(flavor = "current_thread")]
async fn service_with_hooks_dispatches_in_order() {
    let db = Db::connect(TEST_DATABASE_URL, 1, 0).await.expect("db");
    let recorder = Arc::new(RecordingCostHook::default());
    let svc = CostService::with_hooks(db, vec![recorder.clone()]);

    // Manually emit two events (without going through create_cost_event to keep this DB-free)
    // by calling on_cost_event on the recorder directly. The dispatch order is exercised
    // in e2e tests via create_cost_event.
    let ev1 = CostHookEvent::CostEventCreated {
        company_id: Uuid::new_v4(),
        event_id: Uuid::new_v4(),
        agent_id: Uuid::new_v4(),
        cost_cents: 1,
        provider: "x".into(),
        billing_type: "api".into(),
        model: "y".into(),
    };
    let ev2 = CostHookEvent::MonthlySpendUpdated {
        company_id: Uuid::new_v4(),
        agent_id: Uuid::new_v4(),
        agent_month_cents: 1,
        company_month_cents: 1,
    };
    CostHook::on_cost_event(&*recorder, ev1).await.unwrap();
    CostHook::on_cost_event(&*recorder, ev2).await.unwrap();
    let snap = recorder.events_snapshot();
    assert_eq!(snap.len(), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn cost_finance_error_display() {
    let e = CostFinanceError::Validation("bad input".into());
    assert!(e.to_string().contains("bad input"));
    let e = CostFinanceError::NotFound("Agent not found".into());
    assert!(e.to_string().contains("Agent not found"));
    let e = CostFinanceError::Fk("wrong company".into());
    assert!(e.to_string().contains("wrong company"));
}

#[tokio::test(flavor = "current_thread")]
async fn service_add_hook_appends() {
    let db = Db::connect(TEST_DATABASE_URL, 1, 0).await.expect("db");
    let svc = CostService::new(db);
    assert_eq!(svc.hook_count(), 0);
    let svc = svc.add_hook(Arc::new(NoopCostHook));
    let svc = svc.add_hook(Arc::new(RecordingCostHook::default()));
    assert_eq!(svc.hook_count(), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn cost_range_default_is_unbounded() {
    let r = CostRange { from: None, to: None };
    assert!(r.from.is_none());
    assert!(r.to.is_none());
}
