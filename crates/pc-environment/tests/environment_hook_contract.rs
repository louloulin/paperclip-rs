use pc_environment::{
    EnvironmentHook, EnvironmentHookEvent, NoopEnvironmentHook, RecordingEnvironmentHook,
};
use uuid::Uuid;
#[tokio::test]
async fn noop_accepts() {
    let hook = NoopEnvironmentHook;
    let ev = EnvironmentHookEvent::Deleted {
        environment_id: Uuid::new_v4(),
    };
    assert!(EnvironmentHook::on_environment_event(&hook, ev)
        .await
        .is_ok());
}
#[tokio::test]
async fn recorder_captures_all_variants() {
    let h = RecordingEnvironmentHook::default();
    let events = vec![
        EnvironmentHookEvent::Created {
            environment_id: Uuid::new_v4(),
            name: "n".into(),
            driver: "local".into(),
        },
        EnvironmentHookEvent::LeaseAcquired {
            lease_id: Uuid::new_v4(),
            environment_id: Uuid::new_v4(),
            company_id: Uuid::new_v4(),
            policy: "ephemeral".into(),
        },
        EnvironmentHookEvent::OverdueExpired { count: 3 },
    ];
    for e in events.iter() {
        EnvironmentHook::on_environment_event(&h, e.clone())
            .await
            .unwrap();
    }
    assert_eq!(h.len(), 3);
    h.clear();
    assert!(h.is_empty());
}
#[test]
fn tag_is_camel_case() {
    let v: serde_json::Value =
        serde_json::to_value(EnvironmentHookEvent::OverdueExpired { count: 2 }).unwrap();
    assert_eq!(v["type"], "overdueExpired");
    assert_eq!(v["count"], 2);
}
