use pc_issue_approvals::{IssueApprovalHook, IssueApprovalHookEvent, NoopIssueApprovalHook, RecordingIssueApprovalHook};
use uuid::Uuid;
#[tokio::test]
async fn noop_ok() {
    let e = IssueApprovalHookEvent::Unlinked { company_id: Uuid::new_v4(), issue_id: Uuid::new_v4(), approval_id: Uuid::new_v4() };
    assert!(IssueApprovalHook::on_issue_approval_event(&NoopIssueApprovalHook, e).await.is_ok());
}
#[tokio::test]
async fn recorder_captures_all() {
    let h = RecordingIssueApprovalHook::default();
    let ev = IssueApprovalHookEvent::BulkLinked { company_id: Uuid::new_v4(), approval_id: Uuid::new_v4(), count: 3 };
    IssueApprovalHook::on_issue_approval_event(&h, ev.clone()).await.unwrap();
    assert_eq!(h.events_snapshot(), vec![ev]);
    h.clear(); assert!(h.is_empty());
}
#[test]
fn tag_is_camel_case() {
    let v: serde_json::Value = serde_json::to_value(IssueApprovalHookEvent::BulkLinked { company_id: Uuid::nil(), approval_id: Uuid::nil(), count: 1 }).unwrap();
    assert_eq!(v["type"], "bulkLinked");
}
