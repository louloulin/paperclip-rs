//! R611: pc-inbox hook contract tests.

use pc_inbox::{
    InboxAgentPolicyMode, InboxHook, InboxHookEvent, NoopInboxHook, RecordingInboxHook,
};
use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;

#[tokio::test(flavor = "current_thread")]
async fn noop_hook_accepts_events() {
    let hook = NoopInboxHook;
    let event = InboxHookEvent::Restored {
        company_id: Uuid::new_v4(),
        user_id: "u1".into(),
        item_key: "k".into(),
    };
    let res = InboxHook::on_inbox_event(&hook, event).await;
    assert!(res.is_ok());
}

#[tokio::test(flavor = "current_thread")]
async fn recording_hook_stores_events_in_order() {
    let hook = RecordingInboxHook::default();
    assert!(hook.is_empty());

    let ev1 = InboxHookEvent::Dismissed {
        company_id: Uuid::new_v4(),
        user_id: "u".into(),
        item_key: "k".into(),
    };
    let ev2 = InboxHookEvent::AgentPolicyUpdated {
        company_id: Uuid::new_v4(),
        user_id: "u".into(),
        mode: InboxAgentPolicyMode::Open,
        allowed_count: 0,
    };
    InboxHook::on_inbox_event(&hook, ev1.clone()).await.unwrap();
    InboxHook::on_inbox_event(&hook, ev2.clone()).await.unwrap();
    let snap = hook.events_snapshot();
    assert_eq!(snap, vec![ev1, ev2]);
    assert_eq!(hook.len(), 2);
    hook.clear();
    assert!(hook.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn hook_event_serializes_with_type_tag() {
    let ev = InboxHookEvent::Dismissed {
        company_id: Uuid::new_v4(),
        user_id: "u1".into(),
        item_key: "issue-1".into(),
    };
    let v: Value = serde_json::to_value(&ev).expect("serialize");
    assert_eq!(v["type"], "dismissed");
    assert_eq!(v["user_id"], "u1");
    assert_eq!(v["item_key"], "issue-1");
}

#[tokio::test(flavor = "current_thread")]
async fn hook_event_snoozed_includes_until_field() {
    let ev = InboxHookEvent::Snoozed {
        company_id: Uuid::new_v4(),
        user_id: "u".into(),
        item_key: "k".into(),
        snoozed_until: pc_core::Timestamp::now(),
    };
    let v: Value = serde_json::to_value(&ev).expect("serialize");
    assert_eq!(v["type"], "snoozed");
    assert!(v["snoozed_until"].is_string());
}

#[tokio::test(flavor = "current_thread")]
async fn hook_event_agent_policy_updated_serializes_mode_lowercase() {
    let ev = InboxHookEvent::AgentPolicyUpdated {
        company_id: Uuid::new_v4(),
        user_id: "u".into(),
        mode: InboxAgentPolicyMode::Allowlist,
        allowed_count: 3,
    };
    let v: Value = serde_json::to_value(&ev).expect("serialize");
    assert_eq!(v["type"], "agentPolicyUpdated");
    assert_eq!(v["mode"], "allowlist");
    assert_eq!(v["allowed_count"], 3);
}

#[tokio::test(flavor = "current_thread")]
async fn arc_recorder_works_through_dyn_trait() {
    let hook: Arc<dyn InboxHook> = Arc::new(RecordingInboxHook::default());
    let event = InboxHookEvent::Restored {
        company_id: Uuid::new_v4(),
        user_id: "u".into(),
        item_key: "k".into(),
    };
    InboxHook::on_inbox_event(&*hook, event).await.unwrap();
}
