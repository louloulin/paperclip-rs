use pc_feedback_share::{FeedbackShareHook, FeedbackShareHookEvent, NoopFeedbackShareHook, RecordingFeedbackShareHook};
#[tokio::test]
async fn noop_ok() {
    let e = FeedbackShareHookEvent::Uploaded { trace_id: "t".into(), object_key: "k".into() };
    assert!(FeedbackShareHook::on_feedback_share_event(&NoopFeedbackShareHook, e).await.is_ok());
}
#[tokio::test]
async fn recorder_captures() {
    let h = RecordingFeedbackShareHook::default();
    let ev = FeedbackShareHookEvent::ObjectKeyBuilt { trace_id: "t".into(), object_key: "k".into() };
    FeedbackShareHook::on_feedback_share_event(&h, ev.clone()).await.unwrap();
    assert_eq!(h.events_snapshot(), vec![ev]);
    h.clear(); assert!(h.is_empty());
}
#[test]
fn tag_is_camel_case() {
    let v: serde_json::Value = serde_json::to_value(FeedbackShareHookEvent::UploadFailed { trace_id: "t".into(), status: Some(500), message: "boom".into() }).unwrap();
    assert_eq!(v["type"], "uploadFailed");
}
