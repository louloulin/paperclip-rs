use pc_label::{LabelHook, LabelHookEvent, NoopLabelHook, RecordingLabelHook};
use serde_json::Value;
use uuid::Uuid;
#[tokio::test]
async fn noop_accepts_events() {
    let e = LabelHookEvent::Deleted {
        label_id: Uuid::new_v4(),
        company_id: Uuid::new_v4(),
    };
    assert!(noop_label(&e).await)
}
async fn noop_label(e: &LabelHookEvent) -> bool {
    NoopLabelHook.on_label_event(e.clone()).await.is_ok()
}
#[tokio::test]
async fn recorder_stores() {
    let h = RecordingLabelHook::default();
    let e = LabelHookEvent::Created {
        label_id: Uuid::new_v4(),
        company_id: Uuid::new_v4(),
        name: "bug".into(),
    };
    h.on_label_event(e.clone()).await.unwrap();
    assert_eq!(h.events_snapshot(), vec![e]);
}
#[test]
fn event_tag() {
    let v: Value = serde_json::to_value(LabelHookEvent::Deleted {
        label_id: Uuid::nil(),
        company_id: Uuid::nil(),
    })
    .unwrap();
    assert_eq!(v["type"], "deleted");
}
