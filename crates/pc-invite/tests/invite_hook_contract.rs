//! R615: pc-invite hook contract tests.

use pc_invite::{InviteHook, InviteHookEvent, InviteService, NoopInviteHook, RecordingInviteHook};
use pc_repos::Db;
use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

#[tokio::test(flavor = "current_thread")]
async fn noop_hook_accepts_events() {
    let hook = NoopInviteHook;
    let event = InviteHookEvent::Revoked {
        company_id: Uuid::new_v4(),
        invite_id: Uuid::new_v4(),
    };
    let res = InviteHook::on_invite_event(&hook, event).await;
    assert!(res.is_ok());
}

#[tokio::test(flavor = "current_thread")]
async fn recording_hook_stores_events_in_order() {
    let hook = RecordingInviteHook::default();
    assert!(hook.is_empty());
    let ev1 = InviteHookEvent::Created {
        company_id: Uuid::new_v4(),
        invite_id: Uuid::new_v4(),
        invited_by_user_id: Some("u".into()),
    };
    let ev2 = InviteHookEvent::Accepted {
        company_id: Uuid::new_v4(),
        invite_id: Uuid::new_v4(),
    };
    InviteHook::on_invite_event(&hook, ev1.clone())
        .await
        .unwrap();
    InviteHook::on_invite_event(&hook, ev2.clone())
        .await
        .unwrap();
    let snap = hook.events_snapshot();
    assert_eq!(snap, vec![ev1, ev2]);
    hook.clear();
    assert!(hook.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn hook_event_serializes_with_camel_case_type() {
    let ev = InviteHookEvent::Created {
        company_id: Uuid::new_v4(),
        invite_id: Uuid::new_v4(),
        invited_by_user_id: Some("u".into()),
    };
    let v: Value = serde_json::to_value(&ev).expect("serialize");
    assert_eq!(v["type"], "created");
    assert_eq!(v["invited_by_user_id"], "u");
}

#[tokio::test(flavor = "current_thread")]
async fn service_add_hook_appends() {
    let db = Db::connect(TEST_DATABASE_URL, 1, 0).await.expect("db");
    let svc = InviteService::new(db);
    assert_eq!(svc.hook_count(), 0);
    let svc = svc.add_hook(Arc::new(NoopInviteHook));
    let svc = svc.add_hook(Arc::new(RecordingInviteHook::default()));
    assert_eq!(svc.hook_count(), 2);
}
