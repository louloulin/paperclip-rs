//! R613: pc-project hook contract tests.

use pc_project::{
    MembershipState, NoopProjectHook, ProjectHook, ProjectHookEvent, ProjectService, ProjectStatus,
    RecordingProjectHook,
};
use pc_repos::Db;
use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

#[tokio::test(flavor = "current_thread")]
async fn noop_hook_accepts_events() {
    let hook = NoopProjectHook;
    let event = ProjectHookEvent::Deleted {
        company_id: Uuid::new_v4(),
        project_id: Uuid::new_v4(),
    };
    let res = ProjectHook::on_project_event(&hook, event).await;
    assert!(res.is_ok());
}

#[tokio::test(flavor = "current_thread")]
async fn recording_hook_stores_events_in_order() {
    let hook = RecordingProjectHook::default();
    assert!(hook.is_empty());
    let ev1 = ProjectHookEvent::Created {
        company_id: Uuid::new_v4(),
        project_id: Uuid::new_v4(),
        name: "a".into(),
    };
    let ev2 = ProjectHookEvent::StatusChanged {
        company_id: Uuid::new_v4(),
        project_id: Uuid::new_v4(),
        old_status: Some(ProjectStatus::Backlog),
        new_status: ProjectStatus::Active,
    };
    ProjectHook::on_project_event(&hook, ev1.clone())
        .await
        .unwrap();
    ProjectHook::on_project_event(&hook, ev2.clone())
        .await
        .unwrap();
    let snap = hook.events_snapshot();
    assert_eq!(snap, vec![ev1, ev2]);
    hook.clear();
    assert!(hook.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn hook_event_status_changed_serializes_with_camel_case_type() {
    let ev = ProjectHookEvent::StatusChanged {
        company_id: Uuid::new_v4(),
        project_id: Uuid::new_v4(),
        old_status: Some(ProjectStatus::Paused),
        new_status: ProjectStatus::Active,
    };
    let v: Value = serde_json::to_value(&ev).expect("serialize");
    assert_eq!(v["type"], "statusChanged");
    assert_eq!(v["old_status"], "paused");
    assert_eq!(v["new_status"], "active");
}

#[tokio::test(flavor = "current_thread")]
async fn service_add_hook_appends() {
    let db = Db::connect(TEST_DATABASE_URL, 1, 0).await.expect("db");
    let svc = ProjectService::new(db);
    assert_eq!(svc.hook_count(), 0);
    let svc = svc.add_hook(Arc::new(NoopProjectHook));
    let svc = svc.add_hook(Arc::new(RecordingProjectHook::default()));
    assert_eq!(svc.hook_count(), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn hook_event_membership_upserted_includes_state() {
    let ev = ProjectHookEvent::MembershipUpserted {
        company_id: Uuid::new_v4(),
        project_id: Uuid::new_v4(),
        user_id: "u1".into(),
        state: MembershipState::Joined,
    };
    let v: Value = serde_json::to_value(&ev).expect("serialize");
    assert_eq!(v["type"], "membershipUpserted");
    assert_eq!(v["user_id"], "u1");
    assert_eq!(v["state"], "joined");
}
