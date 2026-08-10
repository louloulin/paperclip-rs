use std::sync::Arc;
use pc_feedback_share::{
    build_feedback_share_object_key, encode_feedback_share_payload, FeedbackShareConfig,
    FeedbackShareHookEvent, FeedbackShareService, FeedbackTraceBundle, HttpFeedbackTraceShareClient,
    RecordingFeedbackShareHook,
};
use chrono::{TimeZone, Utc};

fn bundle(trace_id: &str, company_id: &str) -> FeedbackTraceBundle {
    FeedbackTraceBundle::minimal(trace_id, company_id)
}

#[tokio::test]
async fn build_object_key_and_encode_payload() {
    let s = FeedbackShareService::new(HttpFeedbackTraceShareClient::new(&FeedbackShareConfig::new(None, None)));
    let b = bundle("trace-1", "comp-1");
    let at = Utc.with_ymd_and_hms(2026, 8, 10, 12, 30, 45).unwrap();
    let key = s.build_object_key_async(&b, at).await.unwrap();
    let expected = build_feedback_share_object_key(&b, at);
    assert_eq!(key, expected);
    assert!(key.starts_with("feedback-traces/comp-1/2026/08/10/"));
    let (enc, payload) = s.encode_payload_async(&key, at, &b).await.unwrap();
    assert_eq!(enc, pc_feedback_share::FEEDBACK_SHARE_ENCODING);
    assert!(!payload.is_empty());
    let _ = encode_feedback_share_payload(&key, at, &b);
}

#[tokio::test]
async fn validation_paths() {
    let s = FeedbackShareService::new(HttpFeedbackTraceShareClient::new(&FeedbackShareConfig::new(None, None)));
    let mut b = bundle("trace-2", "comp-2");
    b.trace_id = "".into();
    let at = Utc.with_ymd_and_hms(2026, 8, 10, 0, 0, 0).unwrap();
    assert!(s.build_object_key_async(&b, at).await.is_err());
    assert!(s.upload(&b).await.is_err());
    b.trace_id = "trace-2".into();
    b.company_id = "".into();
    assert!(s.build_object_key_async(&b, at).await.is_err());
}

#[tokio::test]
async fn upload_emits_failed_event_when_endpoint_unreachable() {
    let cfg = FeedbackShareConfig::new(Some("http://127.0.0.1:1".into()), Some("tok".into()));
    let h = Arc::new(RecordingFeedbackShareHook::default());
    let s = FeedbackShareService::with_hooks(HttpFeedbackTraceShareClient::new(&cfg), vec![h.clone()]);
    let b = bundle("trace-3", "comp-3");
    let res = s.upload(&b).await;
    assert!(res.is_err());
    let snapshot = h.events_snapshot();
    assert!(snapshot.iter().any(|e| matches!(e, FeedbackShareHookEvent::UploadFailed { .. })));
}
