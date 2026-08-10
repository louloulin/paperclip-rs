use pc_feedback_redaction::{NoopRedactionHook, RedactionHook, RedactionHookEvent, RecordingRedactionHook};
#[tokio::test]
async fn noop_accepts() {
    let ev = RedactionHookEvent::Truncated { fields: vec!["x".into()] };
    assert!(RedactionHook::on_redaction_event(&NoopRedactionHook, ev).await.is_ok());
}
#[tokio::test]
async fn recorder_captures() {
    let h = RecordingRedactionHook::default();
    let ev1 = RedactionHookEvent::Redacted { patterns: vec!["pem_block".into()], total_redactions: 1 };
    let ev2 = RedactionHookEvent::Sanitized { patterns: vec!["jwt".into()], truncated_fields: vec!["$".into()] };
    RedactionHook::on_redaction_event(&h, ev1.clone()).await.unwrap();
    RedactionHook::on_redaction_event(&h, ev2.clone()).await.unwrap();
    assert_eq!(h.events_snapshot(), vec![ev1, ev2]);
    h.clear(); assert!(h.is_empty());
}
#[test]
fn tag_is_camel_case() {
    let v: serde_json::Value = serde_json::to_value(RedactionHookEvent::Truncated { fields: vec!["a".into()] }).unwrap();
    assert_eq!(v["type"], "truncated");
}
