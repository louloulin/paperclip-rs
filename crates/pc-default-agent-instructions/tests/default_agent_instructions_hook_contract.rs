use pc_default_agent_instructions::{DefaultAgentInstructionsHook, DefaultAgentInstructionsHookEvent, NoopDefaultHook, RecordingDefaultHook};
#[tokio::test]
async fn noop_ok() {
    let e = DefaultAgentInstructionsHookEvent::Resolved { role: "ceo".into(), file_count: 4 };
    assert!(DefaultAgentInstructionsHook::on_default_agent_instructions_event(&NoopDefaultHook, e).await.is_ok());
}
#[tokio::test]
async fn recorder_captures() {
    let h = RecordingDefaultHook::default();
    let ev = DefaultAgentInstructionsHookEvent::Resolved { role: "default".into(), file_count: 1 };
    DefaultAgentInstructionsHook::on_default_agent_instructions_event(&h, ev.clone()).await.unwrap();
    assert_eq!(h.events_snapshot(), vec![ev]);
    h.clear(); assert!(h.is_empty());
}
#[test]
fn tag_is_camel_case() {
    let v: serde_json::Value = serde_json::to_value(DefaultAgentInstructionsHookEvent::Resolved { role: "x".into(), file_count: 1 }).unwrap();
    assert_eq!(v["type"], "resolved");
}
