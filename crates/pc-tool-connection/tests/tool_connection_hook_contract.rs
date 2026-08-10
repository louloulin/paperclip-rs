use pc_tool_connection::{NoopToolConnectionHook, RecordingToolConnectionHook, ToolConnectionHook, ToolConnectionHookEvent};
use uuid::Uuid;
#[tokio::test]
async fn noop_ok() {
    let e = ToolConnectionHookEvent::Deleted { connection_id: Uuid::new_v4() };
    assert!(ToolConnectionHook::on_tool_connection_event(&NoopToolConnectionHook, e).await.is_ok());
}
#[tokio::test]
async fn recorder_captures_all_variants() {
    let h = RecordingToolConnectionHook::default();
    let events = vec![
        ToolConnectionHookEvent::Enabled { connection_id: Uuid::new_v4() },
        ToolConnectionHookEvent::ConfigReplaced { connection_id: Uuid::new_v4() },
        ToolConnectionHookEvent::HealthChecked { connection_id: Uuid::new_v4(), status: "healthy".into(), message: None },
    ];
    for e in events.iter() { ToolConnectionHook::on_tool_connection_event(&h, e.clone()).await.unwrap(); }
    assert_eq!(h.len(), 3);
    h.clear(); assert!(h.is_empty());
}
#[test]
fn tag_is_camel_case() {
    let v: serde_json::Value = serde_json::to_value(ToolConnectionHookEvent::Reconnecting { connection_id: Uuid::nil() }).unwrap();
    assert_eq!(v["type"], "reconnecting");
}
