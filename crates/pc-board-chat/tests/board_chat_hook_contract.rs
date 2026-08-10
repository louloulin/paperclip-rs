use pc_board_chat::{BoardChatHook, BoardChatHookEvent, NoopBoardChatHook, RecordingBoardChatHook};
use uuid::Uuid;
#[tokio::test]
async fn noop_ok() {
    let e = BoardChatHookEvent::MessagePosted { company_id: Uuid::new_v4(), thread_id: Uuid::new_v4(), message_id: Uuid::new_v4(), role: "user".into() };
    assert!(BoardChatHook::on_board_chat_event(&NoopBoardChatHook, e).await.is_ok());
}
#[tokio::test]
async fn recorder_captures() {
    let h = RecordingBoardChatHook::default();
    let ev = BoardChatHookEvent::ThreadOpened { company_id: Uuid::new_v4(), thread_id: Uuid::new_v4(), title: "t".into() };
    BoardChatHook::on_board_chat_event(&h, ev.clone()).await.unwrap();
    assert_eq!(h.events_snapshot(), vec![ev]);
    h.clear(); assert!(h.is_empty());
}
#[test]
fn tag_is_camel_case() {
    let v: serde_json::Value = serde_json::to_value(BoardChatHookEvent::BoardIssueEnsured { company_id: Uuid::nil(), issue_id: Uuid::nil() }).unwrap();
    assert_eq!(v["type"], "boardIssueEnsured");
}
