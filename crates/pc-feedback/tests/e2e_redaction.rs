use pc_feedback::{RecordingRedactionHook, RedactionHookEvent, RedactionService};
use std::sync::Arc;

#[tokio::test]
async fn redact_bearer_and_jwt() {
    let s = RedactionService::new();
    let input = "Authorization: Bearer eyJhbGc.eyJzdWI.SflKxw";
    let (out, state) = s.redact_async(input).await;
    assert!(out.contains("[REDACTED_TOKEN]"));
    assert!(state.redacted_patterns.contains("bearer_token"));
}

#[tokio::test]
async fn redact_provider_api_key() {
    let s = RedactionService::new();
    let input = "sk-ant-abcdefghij1234567890 hello";
    let (out, state) = s.redact_async(input).await;
    assert!(out.contains("[REDACTED_API_KEY]"));
    assert!(state.redacted_patterns.contains("provider_api_key"));
}

#[tokio::test]
async fn truncate_validates_max_chars() {
    let s = RedactionService::new();
    assert!(s.truncate("hello", 0).is_err());
    let (out, was) = s.truncate("hello world", 5).unwrap();
    assert!(was);
    assert!(out.len() <= 5);
    let (out, was) = s.truncate("hi", 5).unwrap();
    assert!(!was);
    assert_eq!(out, "hi");
}

#[tokio::test]
async fn sanitize_value_runs_both() {
    let s = RedactionService::new();
    let input = serde_json::json!({ "body": "token=abcdef123456 and sk-abcdef1234567890", "empty": "short" });
    let (out, state) = s.sanitize_value_async(&input, 1024).await.unwrap();
    let body = out["body"].as_str().unwrap();
    assert!(body.contains("[REDACTED]") || body.contains("[REDACTED_API_KEY]"));
    assert!(!state.redacted_patterns.is_empty());
}

#[tokio::test]
async fn hook_records() {
    let h = Arc::new(RecordingRedactionHook::default());
    let s = RedactionService::with_hooks(vec![h.clone()]);
    s.redact_async("Authorization: Bearer abcdefghijklmnop")
        .await;
    let snapshot = h.events_snapshot();
    assert!(snapshot
        .iter()
        .any(|e| matches!(e, RedactionHookEvent::Redacted { .. })));
}
