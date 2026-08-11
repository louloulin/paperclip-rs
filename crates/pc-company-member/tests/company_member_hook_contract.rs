//! R614: pc-company-member hook contract tests.

use pc_company_member::{
    CompanyMemberHook, CompanyMemberHookEvent, CompanyMemberService, MemberStatus,
    NoopCompanyMemberHook, RecordingCompanyMemberHook,
};
use pc_repos::Db;
use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

#[tokio::test(flavor = "current_thread")]
async fn noop_hook_accepts_events() {
    let hook = NoopCompanyMemberHook;
    let event = CompanyMemberHookEvent::Archived {
        company_id: Uuid::new_v4(),
        member_id: Uuid::new_v4(),
    };
    let res = CompanyMemberHook::on_company_member_event(&hook, event).await;
    assert!(res.is_ok());
}

#[tokio::test(flavor = "current_thread")]
async fn recording_hook_stores_events_in_order() {
    let hook = RecordingCompanyMemberHook::default();
    assert!(hook.is_empty());
    let ev1 = CompanyMemberHookEvent::Patched {
        company_id: Uuid::new_v4(),
        member_id: Uuid::new_v4(),
        old_role: Some("member".into()),
        new_role: Some("admin".into()),
        new_status: None,
    };
    let ev2 = CompanyMemberHookEvent::Archived {
        company_id: Uuid::new_v4(),
        member_id: Uuid::new_v4(),
    };
    CompanyMemberHook::on_company_member_event(&hook, ev1.clone())
        .await
        .unwrap();
    CompanyMemberHook::on_company_member_event(&hook, ev2.clone())
        .await
        .unwrap();
    let snap = hook.events_snapshot();
    assert_eq!(snap, vec![ev1, ev2]);
    hook.clear();
    assert!(hook.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn hook_event_serializes_with_camel_case_type() {
    let ev = CompanyMemberHookEvent::Patched {
        company_id: Uuid::new_v4(),
        member_id: Uuid::new_v4(),
        old_role: Some("member".into()),
        new_role: Some("admin".into()),
        new_status: Some(MemberStatus::Archived),
    };
    let v: Value = serde_json::to_value(&ev).expect("serialize");
    assert_eq!(v["type"], "patched");
    assert_eq!(v["old_role"], "member");
    assert_eq!(v["new_role"], "admin");
    assert_eq!(v["new_status"], "archived");
}

#[tokio::test(flavor = "current_thread")]
async fn service_add_hook_appends() {
    let db = Db::connect(TEST_DATABASE_URL, 1, 0).await.expect("db");
    let svc = CompanyMemberService::new(db);
    assert_eq!(svc.hook_count(), 0);
    let svc = svc.add_hook(Arc::new(NoopCompanyMemberHook));
    let svc = svc.add_hook(Arc::new(RecordingCompanyMemberHook::default()));
    assert_eq!(svc.hook_count(), 2);
}
