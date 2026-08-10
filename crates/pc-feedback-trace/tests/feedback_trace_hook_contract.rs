use pc_feedback_trace::{FeedbackTraceHook, FeedbackTraceHookEvent, NoopFeedbackTraceHook, RecordingFeedbackTraceHook};
use serde_json::Value;
use uuid::Uuid;
#[tokio::test]
async fn noop_accepts_delete(){let e=FeedbackTraceHookEvent::Deleted{trace_id:Uuid::new_v4(),issue_id:Uuid::new_v4()}; assert!(NoopFeedbackTraceHook.on_feedback_trace_event(e).await.is_ok());}
#[tokio::test]
async fn recorder_round_trips(){let h=RecordingFeedbackTraceHook::default(); let e=FeedbackTraceHookEvent::Deleted{trace_id:Uuid::new_v4(),issue_id:Uuid::new_v4()}; h.on_feedback_trace_event(e.clone()).await.unwrap(); assert_eq!(h.events_snapshot(),vec![e]);}
#[test]
fn event_is_tagged(){let e=FeedbackTraceHookEvent::Deleted{trace_id:Uuid::nil(),issue_id:Uuid::nil()}; let v:Value=serde_json::to_value(e).unwrap(); assert_eq!(v["type"],"deleted"); assert!(v["trace_id"].is_string());}
