use pc_tool::{NoopToolHook, RecordingToolHook, ToolHook, ToolHookEvent};
use uuid::Uuid;
#[tokio::test]
async fn noop_ok() { let e = ToolHookEvent::Deleted { company_id: Uuid::new_v4(), application_id: Uuid::new_v4() }; assert!(ToolHook::on_tool_event(&NoopToolHook, e).await.is_ok()); }
#[tokio::test]
async fn recorder_captures() {
    let h = RecordingToolHook::default();
    let ev = ToolHookEvent::StatusChanged { company_id: Uuid::new_v4(), application_id: Uuid::new_v4(), status: "active".into() };
    ToolHook::on_tool_event(&h, ev.clone()).await.unwrap();
    assert_eq!(h.events_snapshot(), vec![ev]);
    h.clear(); assert!(h.is_empty());
}
#[test]
fn tag_is_camel_case() {
    let v: serde_json::Value = serde_json::to_value(ToolHookEvent::Patched { company_id: Uuid::nil(), application_id: Uuid::nil() }).unwrap();
    assert_eq!(v["type"], "patched");
}
