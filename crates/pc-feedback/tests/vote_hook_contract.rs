//! R612: pc-feedback-vote hook contract tests.

use pc_feedback::vote::{
    FeedbackVoteHook, FeedbackVoteHookEvent, NoopFeedbackVoteHook, RecordingFeedbackVoteHook,
};
use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;

#[tokio::test(flavor = "current_thread")]
async fn noop_hook_accepts_events() {
    let hook = NoopFeedbackVoteHook;
    let event = FeedbackVoteHookEvent::Cast {
        company_id: Uuid::new_v4(),
        issue_id: Uuid::new_v4(),
        vote_id: Uuid::new_v4(),
        vote: "up".into(),
        author_user_id: "u1".into(),
    };
    let res = FeedbackVoteHook::on_feedback_vote_event(&hook, event).await;
    assert!(res.is_ok());
}

#[tokio::test(flavor = "current_thread")]
async fn recording_hook_stores_events() {
    let hook = RecordingFeedbackVoteHook::default();
    assert!(hook.is_empty());
    let ev = FeedbackVoteHookEvent::Cast {
        company_id: Uuid::new_v4(),
        issue_id: Uuid::new_v4(),
        vote_id: Uuid::new_v4(),
        vote: "down".into(),
        author_user_id: "u1".into(),
    };
    FeedbackVoteHook::on_feedback_vote_event(&hook, ev.clone())
        .await
        .unwrap();
    assert_eq!(hook.len(), 1);
    assert_eq!(hook.events_snapshot()[0], ev);
    hook.clear();
    assert!(hook.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn hook_event_serializes_with_type_tag() {
    let ev = FeedbackVoteHookEvent::Cast {
        company_id: Uuid::new_v4(),
        issue_id: Uuid::new_v4(),
        vote_id: Uuid::new_v4(),
        vote: "up".into(),
        author_user_id: "u1".into(),
    };
    let v: Value = serde_json::to_value(&ev).expect("serialize");
    assert_eq!(v["type"], "cast");
    assert_eq!(v["vote"], "up");
    assert_eq!(v["author_user_id"], "u1");
}

#[tokio::test(flavor = "current_thread")]
async fn arc_recorder_works_through_dyn_trait() {
    let hook: Arc<dyn FeedbackVoteHook> = Arc::new(RecordingFeedbackVoteHook::default());
    let ev = FeedbackVoteHookEvent::Cast {
        company_id: Uuid::new_v4(),
        issue_id: Uuid::new_v4(),
        vote_id: Uuid::new_v4(),
        vote: "up".into(),
        author_user_id: "u".into(),
    };
    FeedbackVoteHook::on_feedback_vote_event(&*hook, ev)
        .await
        .unwrap();
}
