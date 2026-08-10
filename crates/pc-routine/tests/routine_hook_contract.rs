use pc_routine::{NoopRoutineHook, RecordingRoutineHook, RoutineHook, RoutineHookEvent};
use uuid::Uuid;
#[tokio::test]
async fn noop_ok() {
    let e = RoutineHookEvent::Deleted { routine_id: Uuid::new_v4() };
    assert!(RoutineHook::on_routine_event(&NoopRoutineHook, e).await.is_ok());
}
#[tokio::test]
async fn recorder_captures_all() {
    let h = RecordingRoutineHook::default();
    let events = vec![
        RoutineHookEvent::Created { company_id: Uuid::new_v4(), routine_id: Uuid::new_v4(), title: "t".into() },
        RoutineHookEvent::Patched { routine_id: Uuid::new_v4() },
        RoutineHookEvent::Triggered { routine_id: Uuid::new_v4() },
    ];
    for e in events.iter() { RoutineHook::on_routine_event(&h, e.clone()).await.unwrap(); }
    assert_eq!(h.len(), 3);
    h.clear(); assert!(h.is_empty());
}
#[test]
fn tag_is_camel_case() {
    let v: serde_json::Value = serde_json::to_value(RoutineHookEvent::Triggered { routine_id: Uuid::nil() }).unwrap();
    assert_eq!(v["type"], "triggered");
}
